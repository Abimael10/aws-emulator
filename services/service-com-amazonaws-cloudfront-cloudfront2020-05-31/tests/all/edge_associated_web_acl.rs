// SPDX-License-Identifier: CC0-1.0

//! What `AssociateDistributionWebACL` DOES: the web ACL it puts in front of a distribution is
//! enforced on that distribution's viewer traffic, and `DisassociateDistributionWebACL` takes
//! the enforcement off again — end to end over HTTP, with CloudFront and WAFv2 mounted on one
//! router over one engine.
//!
//! The rule under test: a web ACL is in front of a distribution exactly while the stored
//! configuration names it, however it came to name it, so a request the web ACL does not let
//! through is refused AT THE EDGE — "CloudFront responds to requests either with the requested
//! content or with an HTTP 403 status code (Forbidden)" — with no origin request made and none
//! of the origin's bytes in the answer, while a request it allows is served the origin's own
//! answer. Taking the association off restores the origin's answer on the very path that was
//! refused.
//!
//! The sibling suite [`crate::edge_web_acl`] states the same enforcement for a web ACL the
//! CREATE's own `DistributionConfig` named, and [`crate::distribution_web_acl_association`]
//! and [`crate::web_acl_association`] state what the two association operations write. This
//! suite is the join of the two halves: that the CONTROL-plane association is what the EDGE
//! then decides by. Only that join can fail while both halves pass, which is why it is its own
//! suite.
//!
//! The rows span what the rule generalises over: a web ACL that refuses one path by a rule
//! statement while allowing the rest by its default action, and one whose default action
//! refuses everything — asserted on the path the rule matches, on a path it does not, before
//! the association, while it stands and after it is removed.
//!
//! The origin is this service's own control-plane endpoint, which really serves both paths
//! under test (`GET /2020-05-31/distribution` answers a `DistributionList` and
//! `GET /2020-05-31/public-key` a `PublicKeyList`), so an allowed request demonstrably carries
//! the origin's document and a refused one demonstrably carries none of it. The managed
//! `CachingDisabled` cache policy is on the default behaviour, so every viewer request reaches
//! the origin and no answer below comes out of the cache.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, create_distribution, API_VERSION, HOST};
use crate::distribution_web_acl_association::disassociate_web_acl;
use crate::edge_web_acl::{emulator_with_waf, view};
use crate::waf_control_plane::{create_web_acl, SCOPE_CLOUDFRONT};
use crate::web_acl_association::associate_web_acl;

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each answer below is the origin's own rather
/// than a cached copy of an earlier one.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// A rule that BLOCKS a viewer request whose URI path contains `distribution` —
/// `ByteMatchStatement$SearchString` is a blob, so the JSON carries the search string base64
/// encoded (`ZGlzdHJpYnV0aW9u` is `distribution`).
const BLOCKS_THE_DISTRIBUTION_PATH: &str = "{\"Name\":\"block-the-origin-path\",\"Priority\":0,\
     \"Statement\":{\"ByteMatchStatement\":{\"SearchString\":\"ZGlzdHJpYnV0aW9u\",\
     \"FieldToMatch\":{\"UriPath\":{}},\
     \"TextTransformations\":[{\"Priority\":0,\"Type\":\"NONE\"}],\
     \"PositionalConstraint\":\"CONTAINS\"}},\
     \"Action\":{\"Block\":{}},\
     \"VisibilityConfig\":{\"SampledRequestsEnabled\":false,\
     \"CloudWatchMetricsEnabled\":false,\"MetricName\":\"blocktheoriginpath\"}}";

/// One path the origin serves, and the element only that path's answer carries.
struct Served {
    path: &'static str,
    /// An element of the origin's own document, so an answer carrying it carries origin bytes.
    document: &'static str,
}

/// The path the blocking rule matches: the origin serves it, so a refusal of it is visibly a
/// refusal rather than an origin that had nothing to give.
const MATCHED: Served = Served {
    path: "/2020-05-31/distribution",
    document: "DistributionList",
};

/// A path the blocking rule does NOT match, served by the same origin.
const UNMATCHED: Served = Served {
    path: "/2020-05-31/public-key",
    document: "PublicKeyList",
};

/// One row: the web ACL a distribution is put behind, and which of the two paths it refuses
/// while it stands.
struct Row {
    case: &'static str,
    /// The web ACL's name, which is WAFv2's own identity for it.
    web_acl: &'static str,
    /// `WebACL$DefaultAction`, the action for a request no rule matched.
    default_action: &'static str,
    /// `WebACL$Rules`, empty for a web ACL with no rules at all.
    rules: &'static str,
    /// Whether the path the rule matches is refused while the association stands.
    refuses_matched: bool,
    /// Whether the path the rule does not match is refused while the association stands.
    refuses_unmatched: bool,
}

/// The table: a web ACL that refuses by a RULE while its default action allows, and one that
/// refuses everything by its DEFAULT ACTION.
const ROWS: &[Row] = &[
    Row {
        case: "a rule that blocks one path, with a default action that allows",
        web_acl: "associated-edge-block-path",
        default_action: "{\"Allow\":{}}",
        rules: BLOCKS_THE_DISTRIBUTION_PATH,
        refuses_matched: true,
        refuses_unmatched: false,
    },
    Row {
        case: "no rules at all, with a default action that blocks",
        web_acl: "associated-edge-block-all",
        default_action: "{\"Block\":{}}",
        rules: "",
        refuses_matched: true,
        refuses_unmatched: true,
    },
];

/// The `DistributionConfig` every row submits: one origin — this service's own control plane —
/// a default behaviour under the managed `CachingDisabled` policy, and NO web ACL, so the only
/// web ACL a row ever has is the one it associates.
fn config_xml(caller_reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Assert the edge served `served` the origin's own answer.
async fn assert_served(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    domain: &str,
    served: &Served,
) {
    let answer = view(router, domain, served.path).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?} ({stage}, {}): expected the origin's answer, got {} with {:?}",
        served.path, answer.status, answer.body
    );
    assert!(
        answer.body.contains(served.document),
        "case {case:?} ({stage}, {}): expected the origin's own {} document, got {:?}",
        served.path,
        served.document,
        answer.body
    );
    assert_eq!(
        answer.x_cache, "Miss from cloudfront",
        "case {case:?} ({stage}, {}): every request under CachingDisabled reaches the origin, \
         got {:?}",
        served.path, answer.x_cache
    );
}

/// Assert the edge refused `served` itself: HTTP 403, none of the origin's bytes, and the
/// answer reported as CloudFront's own rather than an origin's.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    domain: &str,
    served: &Served,
) {
    let answer = view(router, domain, served.path).await;
    assert_eq!(
        answer.status, 403,
        "case {case:?} ({stage}, {}): expected the edge to refuse the request 403, got {} with \
         {:?}",
        served.path, answer.status, answer.body
    );
    assert!(
        !answer.body.contains(served.document),
        "case {case:?} ({stage}, {}): a refusal the edge produced carries none of the origin's \
         bytes, got {:?}",
        served.path,
        answer.body
    );
    assert_eq!(
        answer.x_cache, "Error from cloudfront",
        "case {case:?} ({stage}, {}): expected the answer to be reported as CloudFront's own, \
         got {:?}",
        served.path, answer.x_cache
    );
}

/// Assert both paths under test are served the origin's answer — the state a distribution with
/// no web ACL in front of it is in.
async fn assert_both_served(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    domain: &str,
) {
    for served in [&MATCHED, &UNMATCHED] {
        assert_served(router, case, stage, domain, served).await;
    }
}

/// RULE: the web ACL an association puts in front of a distribution decides that
/// distribution's viewer traffic — a request it does not let through is refused at the edge
/// with HTTP 403 and none of the origin's bytes, a request it allows is served the origin's own
/// answer — and a disassociation restores the origin's answer on the refused path.
#[tokio::test]
async fn an_associated_web_acl_decides_the_edge_until_it_is_disassociated() {
    let router = emulator_with_waf();

    for row in ROWS {
        let case = row.case;
        let web_acl = create_web_acl(
            &router,
            row.web_acl,
            SCOPE_CLOUDFRONT,
            row.default_action,
            row.rules,
        )
        .await;

        let created = create_distribution(&router, &config_xml(&format!("ref-{}", row.web_acl)))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201 from CreateDistribution, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let domain = created
            .root()
            .text_at("DomainName")
            .unwrap_or_default()
            .to_string();
        assert!(
            !id.is_empty() && !domain.is_empty(),
            "case {case:?}: the create must publish an Id and a DomainName: {}",
            created.raw
        );
        let etag = created.header("etag").to_string();

        // The configuration has to have reached the edge locations before a viewer is served,
        // and the wait is on the status CloudFront reports rather than on a span of the clock.
        awaited_deployment(&router, case, &id).await;

        // THE POSITIVE CONTROL: with no web ACL in front of it, the distribution serves both
        // paths — including the one the rule matches, so a refusal below is the web ACL's doing
        // and not an origin that never answered.
        assert_both_served(&router, case, "before the association", &domain).await;

        // The association, stated with the version the create minted, exactly as a client
        // makes it.
        let associated = associate_web_acl(&router, &id, &web_acl.arn, Some(&etag)).await;
        assert_eq!(
            associated.status, 200,
            "case {case:?}: expected HTTP 200 from AssociateDistributionWebACL, got {} with {}",
            associated.status, associated.raw
        );
        let associated_etag = associated.header("etag").to_string();
        assert!(
            !associated_etag.is_empty(),
            "case {case:?}: the association must report the version it minted: {}",
            associated.raw
        );
        awaited_deployment(&router, case, &id).await;

        for (served, refused) in [
            (&MATCHED, row.refuses_matched),
            (&UNMATCHED, row.refuses_unmatched),
        ] {
            if refused {
                assert_refused(&router, case, "with the web ACL associated", &domain, served)
                    .await;
            } else {
                assert_served(&router, case, "with the web ACL associated", &domain, served).await;
            }
        }

        // Taking the association off leaves nothing in front of the distribution, so the path
        // that was refused serves the origin's own answer again.
        let disassociated =
            disassociate_web_acl(&router, &id, Some(&associated_etag)).await;
        assert_eq!(
            disassociated.status, 200,
            "case {case:?}: expected HTTP 200 from DisassociateDistributionWebACL, got {} with {}",
            disassociated.status, disassociated.raw
        );
        awaited_deployment(&router, case, &id).await;

        assert_both_served(&router, case, "after the disassociation", &domain).await;
    }
}
