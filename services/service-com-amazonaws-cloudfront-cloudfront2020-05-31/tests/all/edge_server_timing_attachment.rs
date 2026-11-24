// SPDX-License-Identifier: CC0-1.0

//! WHICH response headers policy decides a viewer answer's `Server-Timing` header: the one the
//! CURRENTLY stored configuration attaches to the behaviour the request matched.
//!
//! The rule: `ServerTimingHeadersConfig$Enabled` "determines whether CloudFront adds the
//! `Server-Timing` header to HTTP responses that it sends in response to requests that match a
//! cache behavior that's associated with this response headers policy", and a distribution
//! serves the configuration it has deployed — so the header travels on a behaviour's answers
//! exactly while that behaviour names a policy enabling the group, and a configuration write
//! that attaches, swaps or removes `ResponseHeadersPolicyId` decides every answer served once
//! the distribution reports `Deployed` again.
//!
//! The rows differ along what the rule generalises over: how the configuration reached the
//! store (the create that takes tags as much as the plain one, and `UpdateDistribution`), what
//! the attachment was replaced with (nothing at all, a policy whose group is not enabled, a
//! policy stating no group, a policy enabling it), the direction of the change (attached,
//! detached, re-attached) and WHICH behaviour of the distribution carries the attachment (the
//! default one or an ordered one, each serving its own path).
//!
//! The harness is the `Server-Timing` suite's and the lifecycle suites', reused rather than
//! copied — the second create path included
//! ([`crate::lambda_edge_association::create_distribution_with_tags`]).

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, update_distribution, Answer, API_VERSION,
    HOST,
};
use crate::edge_server_timing_header::{
    metric_names, metric_parameter, timing_policy, view, CACHING_DISABLED, SERVED_PATH,
};
use crate::lambda_edge_association::create_distribution_with_tags;
use crate::response_headers_policy_lifecycle::ServerTiming;

/// A second path the served origin answers, so an ordered behaviour can be given a pattern
/// that selects requests of its own.
const ORDERED_PATH: &str = "/2020-05-31/origin-access-control";

/// The ordered behaviour's `PathPattern`, written as a wildcard over [`ORDERED_PATH`].
const ORDERED_PATTERN: &str = "/2020-05-31/origin-access-contro*";

/// How many answers each row reads: "every response that matches the behaviour" is the rule,
/// so one answer carrying the header (or lacking it) is not enough to decide a row.
const ANSWERS_PER_ROW: usize = 3;

/// The metrics that stand on every value of the header, whatever work the request caused.
const ALWAYS_PRESENT: [&str; 2] = ["cdn-pop", "cdn-rid"];

/// One `DistributionConfig` over the served origin: `default_attachment` is the
/// `ResponseHeadersPolicyId` of the DEFAULT behaviour (none when `None`), and
/// `ordered_attachment` that of one ordered behaviour on [`ORDERED_PATTERN`], which is left
/// out of the document entirely when `ordered` is false.
fn config_xml(
    reference: &str,
    default_attachment: Option<&str>,
    ordered: bool,
    ordered_attachment: Option<&str>,
) -> String {
    let attachment = |id: Option<&str>| match id {
        Some(id) => format!("<ResponseHeadersPolicyId>{id}</ResponseHeadersPolicyId>"),
        None => String::new(),
    };
    let behaviors = if ordered {
        format!(
            "<CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
             <PathPattern>{ORDERED_PATTERN}</PathPattern>\
             <TargetOriginId>only</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             {}</CacheBehavior></Items></CacheBehaviors>",
            attachment(ordered_attachment)
        )
    } else {
        "<CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>".to_string()
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>only</Id><DomainName>{HOST}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>only</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         {}</DefaultCacheBehavior>\
         {behaviors}\
         <Comment>the server timing attachment suite</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        attachment(default_attachment)
    )
}

/// The `(id, domain, etag)` of one created distribution, read out of the create's own answer.
struct Minted {
    id: String,
    domain: String,
    etag: String,
}

/// Read the three members of a create answer every row below goes on to use.
fn minted(case: &str, created: &Answer) -> Minted {
    assert_eq!(
        created.status, 201,
        "case {case}: expected HTTP 201 creating the distribution, got {} with {}",
        created.status, created.raw
    );
    Minted {
        id: created
            .root()
            .find_text("Id")
            .unwrap_or_else(|| panic!("case {case}: the created distribution reports no id"))
            .to_string(),
        domain: created
            .root()
            .find_text("DomainName")
            .unwrap_or_else(|| {
                panic!("case {case}: the created distribution reports no domain name")
            })
            .to_string(),
        etag: created.header("etag").to_string(),
    }
}

/// Ask `path` of `domain` [`ANSWERS_PER_ROW`] times and assert every answer carries the header
/// when `expected` says it must, and none of them does when it says it must not.
///
/// A present header is read further: its metrics are the published ones that stand on every
/// value, and the id it reports is the id the edge stamped on THAT answer, so a header copied
/// from an earlier exchange fails the row.
async fn assert_header(
    router: &MultiServiceRouter,
    case: &str,
    domain: &str,
    path: &str,
    expected: bool,
) {
    for attempt in 0..ANSWERS_PER_ROW {
        let answer = view(router, domain, "GET", path).await;
        assert_eq!(
            answer.status, 200,
            "case {case} (request {attempt} of {path}): expected the request to be served, got \
             {} ({:?})",
            answer.status, answer.x_cache
        );
        assert_eq!(
            !answer.timing.is_empty(),
            expected,
            "case {case} (request {attempt} of {path}): expected the Server-Timing header to be \
             {}, got {:?}",
            if expected { "written" } else { "absent" },
            answer.timing
        );
        if !expected {
            continue;
        }
        let names = metric_names(&answer.timing);
        for always in ALWAYS_PRESENT {
            assert!(
                names.contains(&always.to_string()),
                "case {case} (request {attempt} of {path}): expected the {always} metric, got \
                 {:?}",
                answer.timing
            );
        }
        assert_eq!(
            metric_parameter(&answer.timing, "cdn-rid", "desc"),
            Some(answer.cf_id.clone()),
            "case {case} (request {attempt} of {path}): expected the request id the edge \
             stamped ({:?}), got {:?}",
            answer.cf_id,
            answer.timing
        );
    }
}

/// One step of the sequence one distribution is driven through: the attachment its default
/// behaviour carries after the step, and whether its answers must then carry the header.
struct Step {
    case: &'static str,
    /// Which of the policies created up front the behaviour names, by position, or `None` for
    /// a behaviour naming no response headers policy at all.
    attach: Option<usize>,
    header: bool,
}

/// The stored attachment of the moment decides the header: the group travels while the
/// behaviour names a policy enabling it, and each `UpdateDistribution` that changes the
/// attachment changes what the edge writes once the distribution is deployed again.
#[tokio::test]
async fn the_stored_attachment_of_the_moment_decides_whether_the_header_is_written() {
    let router = emulator();

    // The policies the steps attach, in the positions `Step::attach` names: the group enabled
    // at the whole rate, enabled with no rate stated, enabled at no rate at all, not enabled,
    // and a policy stating no such group.
    let policies = [
        (
            "enabled-whole-rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(100.0),
            }),
        ),
        (
            "enabled-no-rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: None,
            }),
        ),
        (
            "enabled-zero-rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(0.0),
            }),
        ),
        (
            "not-enabled",
            Some(ServerTiming {
                enabled: false,
                sampling_rate: Some(100.0),
            }),
        ),
        ("no-group", None),
    ];
    let mut ids: Vec<String> = Vec::new();
    for (name, timing) in policies {
        ids.push(timing_policy(&router, name, timing).await);
    }

    let reference = "timing-attachment";
    let created =
        create_distribution(&router, &config_xml(reference, Some(&ids[0]), false, None)).await;
    let distribution = minted("created attaching an enabled policy", &created);
    awaited_deployment(&router, "created attaching an enabled policy", &distribution.id).await;
    assert_header(
        &router,
        "created attaching an enabled policy",
        &distribution.domain,
        SERVED_PATH,
        true,
    )
    .await;

    // The steps differ along what the attachment is replaced with and along the DIRECTION of
    // the change, and each is decided over the configuration the step before it left stored:
    // the sequence returns to an enabled policy twice, so a decision taken once and kept fails
    // it.
    let steps = [
        Step {
            case: "the attachment removed from the behaviour",
            attach: None,
            header: false,
        },
        Step {
            case: "an enabled policy attached again",
            attach: Some(0),
            header: true,
        },
        Step {
            case: "swapped for a policy whose group is not enabled",
            attach: Some(3),
            header: false,
        },
        Step {
            case: "swapped for a policy stating no such group",
            attach: Some(4),
            header: false,
        },
        Step {
            case: "swapped for a policy enabling it at no stated rate",
            attach: Some(1),
            header: true,
        },
        Step {
            case: "swapped for a policy enabling it at the zero rate",
            attach: Some(2),
            header: false,
        },
        Step {
            case: "the attachment removed once more",
            attach: None,
            header: false,
        },
    ];

    let mut etag = distribution.etag;
    for step in steps {
        let attachment = step.attach.map(|at| ids[at].as_str());
        let updated = update_distribution(
            &router,
            &distribution.id,
            &config_xml(reference, attachment, false, None),
            Some(&etag),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {}: expected HTTP 200 from UpdateDistribution, got {} with {}",
            step.case, updated.status, updated.raw
        );
        etag = updated.header("etag").to_string();
        // A configuration that has not reached the edge locations serves no viewer, and the
        // CLOCK owes that propagation: the condition is polled for, never a span waited out.
        awaited_deployment(&router, step.case, &distribution.id).await;
        assert_header(
            &router,
            step.case,
            &distribution.domain,
            SERVED_PATH,
            step.header,
        )
        .await;
    }
}

/// The attachment is the BEHAVIOUR's: an ordered behaviour that names an enabling policy
/// writes the header on the answers its own pattern selects, and a behaviour naming none
/// writes it on nothing — whichever of them is the default one. The create that takes tags
/// stores the same configuration, so it serves the same answers.
#[tokio::test]
async fn the_header_follows_the_behaviour_the_request_matched() {
    let router = emulator();
    let policy = timing_policy(
        &router,
        "timing-per-behaviour",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;

    let reference = "timing-behaviours";
    // Minted through the create that takes tags: the same configuration, stored by the other
    // of the two create paths.
    let created = create_distribution_with_tags(
        &router,
        &config_xml(reference, None, true, Some(&policy)),
    )
    .await;
    let case = "the ordered behaviour attaches it";
    let distribution = minted(case, &created);
    awaited_deployment(&router, case, &distribution.id).await;
    // The ordered behaviour's pattern selects this path, so its attachment decides the answer;
    // the default behaviour, which names no policy, serves the other path.
    assert_header(&router, case, &distribution.domain, ORDERED_PATH, true).await;
    assert_header(
        &router,
        "the default behaviour attaches none",
        &distribution.domain,
        SERVED_PATH,
        false,
    )
    .await;

    // The mirror of that configuration: the attachment moved to the DEFAULT behaviour, which
    // moves the header to the other path and off this one.
    let moved = update_distribution(
        &router,
        &distribution.id,
        &config_xml(reference, Some(&policy), true, None),
        Some(&distribution.etag),
    )
    .await;
    assert_eq!(
        moved.status, 200,
        "expected HTTP 200 moving the attachment to the default behaviour, got {} with {}",
        moved.status, moved.raw
    );
    let case = "the attachment moved to the default behaviour";
    awaited_deployment(&router, case, &distribution.id).await;
    assert_header(&router, case, &distribution.domain, SERVED_PATH, true).await;
    assert_header(
        &router,
        "the ordered behaviour attaches none",
        &distribution.domain,
        ORDERED_PATH,
        false,
    )
    .await;
}
