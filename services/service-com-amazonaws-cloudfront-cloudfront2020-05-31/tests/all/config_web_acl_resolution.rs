// SPDX-License-Identifier: CC0-1.0

//! The WAF web ACL a distribution's CONFIGURATION names, resolved against WAF, end to end
//! over HTTP with both services mounted on one router.
//!
//! The rule under test: every call that admits a submitted `DistributionConfig` resolves
//! `WebACLId` against WAF — the service that holds web ACLs — before it stores the
//! configuration, and a value naming a WAFv2 resource that is no web ACL the account holds
//! in the namespace CloudFront is protected by is refused `InvalidWebACLId` (HTTP 400) with
//! CloudFront's fixed sentence, storing nothing. The member reports the protection in front
//! of the distribution — "a unique identifier that specifies the WAF web ACL, if any, to
//! associate with this distribution" — so a configuration naming one that resolves to
//! nothing would tell a client auditing its posture that it is protected when no web ACL is.
//!
//! The rows differ along what the rule generalises over:
//!
//! * WHETHER the value resolves: three web ACLs WAF really holds, differing in their rules,
//!   their default action and the length of their name, each of which is admitted and read
//!   back;
//! * the WAYS a value that names a WAFv2 resource can fail to resolve — a well-formed global
//!   web ACL ARN of this very account WAF never minted, a web ACL WAF HELD and that was then
//!   deleted there (so the admission follows WAF's state rather than CloudFront's memory of
//!   it), a web ACL WAF really holds in the REGIONAL namespace (which WAF would never enforce
//!   in front of a distribution), and an ARN naming another WAFv2 resource kind — an IP set —
//!   that WAF really holds;
//! * the values that name no WAFv2 resource to resolve at all, and so are stored as
//!   submitted: the member left out, the empty member, a WAF Classic web ACL ARN and the bare
//!   WAF Classic ACL id (the two forms `DistributionConfig$WebACLId`'s own documentation names
//!   for that generation, which this emulator holds no web ACL of);
//! * WHICH call submits the configuration: `CreateDistribution`,
//!   `CreateDistributionWithTags` and `UpdateDistribution`.
//!
//! The harness (the router, the request helpers and the configuration builder) is
//! [`crate::distribution_lifecycle`]'s, and every web ACL is created through WAFv2's own
//! control plane ([`crate::waf_control_plane`]), so no WAFv2 identifier is built here: the
//! ARNs are the ones that service minted.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, config, create_distribution, emulator, get_distribution, list_distributions,
    update_distribution, Answer, Config, API_VERSION,
};
use crate::waf_control_plane::{
    create_web_acl, delete_web_acl, json_string, waf_call, COUNTING_RULE, SCOPE_CLOUDFRONT,
    SCOPE_REGIONAL,
};

/// The sentence CloudFront refuses a configuration whose `WebACLId` it could not resolve
/// with. The whole text is fixed — it says the web ACL named is not one this requester can
/// reach, and names neither the identifier submitted nor the reason it did not resolve — so
/// nothing of the request appears in it. Spelled out here rather than imported from the
/// implementation, so a constant edited to some other wording fails these rows.
const INACCESSIBLE_WEB_ACL: &str = "Web ACL is not accessible by the requester.";

/// A well-formed `wafv2` web ACL ARN of the global namespace in this emulator's own account
/// that WAF never minted: the layout
/// `arn:<partition>:wafv2:<region>:<account>:global/webacl/<name>/<id>` that WAF's own
/// `CreateWebACL` answers with, so what refuses it is that WAF holds nothing under it rather
/// than the shape of the value. Every row that uses it first asks WAF for it
/// ([`assert_waf_holds_nothing`]).
const NEVER_MINTED_WEB_ACL: &str = "arn:aws:wafv2:us-east-1:000000000000:global/webacl/\
     never-minted-web-acl/a1b2c3d4-5678-90ab-cdef-EXAMPLE11111";

/// `POST /2020-05-31/distribution?WithTags`, the second create path: the same
/// `DistributionConfig` wrapped in a `DistributionConfigWithTags`.
async fn create_distribution_with_tags(router: &MultiServiceRouter, config: &str) -> Answer {
    let body = format!(
        "<DistributionConfigWithTags \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{config}<Tags/>\
         </DistributionConfigWithTags>"
    );
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&body),
        None,
    )
    .await
}

/// What a row names as its configuration's `WebACLId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// A web ACL WAF holds in the `CLOUDFRONT` scope, created under this name with this
    /// default action and these rules.
    Held {
        name: &'static str,
        default_action: &'static str,
        rules: &'static str,
    },
    /// A web ACL WAF held in the `CLOUDFRONT` scope and that was then deleted there.
    Deleted,
    /// A web ACL WAF holds in the `REGIONAL` namespace, whose web ACLs protect regional
    /// resources and so never stand in front of a distribution.
    Regional,
    /// An IP set of the global namespace: a WAFv2 resource this account really holds, of a
    /// kind that is no web ACL.
    IpSet,
    /// A well-formed global web ACL ARN WAF never minted ([`NEVER_MINTED_WEB_ACL`]).
    NeverMinted,
    /// The value spelled out, for the identifiers that name no WAFv2 resource at all.
    Literal(&'static str),
    /// The member left out of the configuration entirely.
    Absent,
}

/// One row: what the configuration names, and whether the write admits it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    named: Named,
    admitted: bool,
}

/// The table every write below is driven over.
///
/// The WAF Classic forms are the two `DistributionConfig$WebACLId`'s own documentation names
/// ("to specify a web ACL created using the latest version of WAF, use the ACL ARN … To
/// specify a web ACL created using WAF Classic, use the ACL ID").
const ROWS: &[Row] = &[
    Row {
        case: "a web ACL WAF holds, with no rules",
        named: Named::Held {
            name: "config-web-acl-plain",
            default_action: "{\"Allow\":{}}",
            rules: "",
        },
        admitted: true,
    },
    Row {
        case: "a web ACL WAF holds, with one rule",
        named: Named::Held {
            name: "config-web-acl-one-rule",
            default_action: "{\"Allow\":{}}",
            rules: COUNTING_RULE,
        },
        admitted: true,
    },
    Row {
        case: "a web ACL WAF holds, with a blocking default action and a considerably longer name",
        named: Named::Held {
            name: "config-web-acl-with-a-considerably-longer-name",
            default_action: "{\"Block\":{}}",
            rules: COUNTING_RULE,
        },
        admitted: true,
    },
    Row {
        case: "a global web ACL ARN WAF never minted",
        named: Named::NeverMinted,
        admitted: false,
    },
    Row {
        case: "a web ACL WAF held and then deleted",
        named: Named::Deleted,
        admitted: false,
    },
    Row {
        case: "a web ACL WAF holds in the regional namespace",
        named: Named::Regional,
        admitted: false,
    },
    Row {
        case: "an IP set of the global namespace WAF holds",
        named: Named::IpSet,
        admitted: false,
    },
    Row {
        case: "no WebACLId member at all",
        named: Named::Absent,
        admitted: true,
    },
    Row {
        case: "an empty WebACLId",
        named: Named::Literal(""),
        admitted: true,
    },
    Row {
        case: "a WAF Classic web ACL ARN",
        named: Named::Literal(
            "arn:aws:waf::123456789012:webacl/473e64fd-f30b-4765-81a0-62ad96dd167a",
        ),
        admitted: true,
    },
    Row {
        case: "a bare WAF Classic ACL id",
        named: Named::Literal("473e64fd-f30b-4765-81a0-62ad96dd167a"),
        admitted: true,
    },
];

/// The `WebACLId` this row's configuration states, staged through WAFv2's own control plane
/// where the row names a resource that service holds — so every ARN under test is one WAF
/// minted, and every ARN the rule refuses is refused for the reason the row names rather than
/// for its shape.
///
/// `None` is the member left out entirely.
async fn staged(router: &MultiServiceRouter, row: &Row) -> Option<String> {
    match row.named {
        Named::Held {
            name,
            default_action,
            rules,
        } => Some(
            create_web_acl(router, name, SCOPE_CLOUDFRONT, default_action, rules)
                .await
                .arn,
        ),
        Named::Deleted => {
            let created = create_web_acl(
                router,
                "config-web-acl-to-be-deleted",
                SCOPE_CLOUDFRONT,
                "{\"Allow\":{}}",
                "",
            )
            .await;
            let arn = created.arn.clone();
            delete_web_acl(router, &created).await;
            assert_waf_holds_nothing(router, row.case, &arn).await;
            Some(arn)
        }
        Named::Regional => Some(
            create_web_acl(
                router,
                "config-web-acl-regional",
                SCOPE_REGIONAL,
                "{\"Allow\":{}}",
                "",
            )
            .await
            .arn,
        ),
        Named::IpSet => {
            let (status, answered) = waf_call(
                router,
                "CreateIPSet",
                "{\"Name\":\"config-web-acl-ip-set\",\"Scope\":\"CLOUDFRONT\",\
                 \"IPAddressVersion\":\"IPV4\",\"Addresses\":[\"203.0.113.0/24\"]}",
            )
            .await;
            assert_eq!(
                status, 200,
                "case {}: expected HTTP 200 from CreateIPSet, got {status} with {answered}",
                row.case
            );
            Some(json_string(&answered, "ARN").unwrap_or_else(|| {
                panic!("case {}: CreateIPSet answered no ARN: {answered}", row.case)
            }))
        }
        Named::NeverMinted => {
            assert_waf_holds_nothing(router, row.case, NEVER_MINTED_WEB_ACL).await;
            Some(NEVER_MINTED_WEB_ACL.to_string())
        }
        Named::Literal(value) => Some(value.to_string()),
        Named::Absent => None,
    }
}

/// Establish through WAF's OWN reader that it holds no web ACL under `arn`, the way a client
/// establishes it: `GetWebACL` by ARN answers `WAFNonexistentItemException` for one it holds
/// nothing of.
async fn assert_waf_holds_nothing(router: &MultiServiceRouter, case: &str, arn: &str) {
    let body = format!("{{\"ARN\":\"{arn}\"}}");
    let (status, answered) = waf_call(router, "GetWebACL", &body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected WAF to hold nothing under {arn:?}, got HTTP {status} with \
         {answered}"
    );
    assert!(
        answered.contains("WAFNonexistentItemException"),
        "case {case}: expected WAFNonexistentItemException for {arn:?}, got {answered}"
    );
}

/// A web ACL WAF holds, for the positive control every refused row is checked against: the
/// SAME configuration naming it is admitted, so what was refused was the identifier.
async fn control_web_acl(router: &MultiServiceRouter) -> String {
    create_web_acl(
        router,
        "config-web-acl-control",
        SCOPE_CLOUDFRONT,
        "{\"Allow\":{}}",
        "",
    )
    .await
    .arn
}

/// Assert `answer` is the refusal the rule states: HTTP 400, `InvalidWebACLId`, CloudFront's
/// fixed sentence, and nothing of the request inside it.
fn assert_refused(answer: &Answer, case: &str, operation: &str, named: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({operation}): expected HTTP 400 for a WebACLId naming no web ACL the \
         account holds, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidWebACLId",
        "case {case} ({operation}): expected InvalidWebACLId, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, INACCESSIBLE_WEB_ACL,
        "case {case} ({operation}): expected the message {INACCESSIBLE_WEB_ACL:?}, got \
         {message:?}"
    );
    assert!(
        !message.contains(named),
        "case {case} ({operation}): expected nothing of the request in the message, got \
         {message:?}"
    );
}

/// What `GetDistribution` reports under `DistributionConfig/WebACLId`: a configuration that
/// named no web ACL reads back as the empty value, which is what "no web ACL" is.
async fn stored_web_acl(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let fetched = get_distribution(router, id).await;
    assert_eq!(
        fetched.status, 200,
        "case {case}: expected HTTP 200 from GetDistribution, got {} with {}",
        fetched.status, fetched.raw
    );
    fetched
        .root()
        .text_at("DistributionConfig/WebACLId")
        .unwrap_or_default()
        .to_string()
}

/// Both creates resolve the web ACL the configuration names: one WAF holds is admitted and
/// read back, one that resolves to nothing is refused with `InvalidWebACLId` and creates
/// nothing — and the SAME configuration under the SAME caller reference naming a web ACL WAF
/// holds is then admitted, so the refusal spent nothing.
#[tokio::test]
async fn a_create_resolves_the_web_acl_its_configuration_names() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let named = staged(&router, row).await;
        let reference = format!("ref-config-web-acl-{index}");
        let template = Config {
            web_acl_id: named.clone(),
            ..config("config-web-acl", &reference)
        };

        let created = create_distribution(&router, &template.xml()).await;
        if row.admitted {
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201 from CreateDistribution, got {} with {}",
                created.status, created.raw
            );
            let id = created
                .root()
                .text_at("Id")
                .unwrap_or_default()
                .to_string();
            let expected = named.clone().unwrap_or_default();
            let stored = stored_web_acl(&router, case, &id).await;
            assert_eq!(
                stored, expected,
                "case {case}: expected GetDistribution to report WebACLId {expected:?}, got \
                 {stored:?}"
            );
            continue;
        }

        let dead = named.clone().unwrap_or_default();
        assert_refused(&created, case, "CreateDistribution", &dead);

        // The tagging create runs the same admission, so it answers the same refusal.
        let with_tags = create_distribution_with_tags(&router, &template.xml()).await;
        assert_refused(&with_tags, case, "CreateDistributionWithTags", &dead);

        // And the refusals created nothing at all.
        let listed = list_distributions(&router, "").await;
        assert_eq!(
            listed.root().text_at("Quantity"),
            Some("0"),
            "case {case}: expected the refused creates to leave the account with no \
             distributions, got {}",
            listed.raw
        );
        assert!(
            !listed.raw.contains(&dead),
            "case {case}: expected the refused identifier to be in no listing, got {}",
            listed.raw
        );

        // The positive control: the same configuration, under the same caller reference,
        // naming a web ACL WAF holds. It lands — so what was refused was the identifier, and
        // the refusals spent neither the caller reference nor anything else.
        let control = control_web_acl(&router).await;
        let admitted = Config {
            web_acl_id: Some(control.clone()),
            ..template.clone()
        };
        let created = create_distribution(&router, &admitted.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 for the same configuration naming a web ACL WAF \
             holds, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string();
        let stored = stored_web_acl(&router, case, &id).await;
        assert_eq!(
            stored, control,
            "case {case}: expected GetDistribution to report WebACLId {control:?}, got \
             {stored:?}"
        );
    }
}

/// `UpdateDistribution` resolves the member the same way: an update that would leave the
/// distribution naming a web ACL the account does not hold is refused, the stored
/// configuration and its version stand, and the same update naming a web ACL WAF holds lands.
#[tokio::test]
async fn an_update_resolves_the_web_acl_its_configuration_names() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let control = control_web_acl(&router).await;
        let reference = format!("ref-config-web-acl-update-{index}");
        let stored_config = Config {
            web_acl_id: Some(control.clone()),
            ..config("config-web-acl-update", &reference)
        };
        let created = create_distribution(&router, &stored_config.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from CreateDistribution, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string();
        let etag = created.header("etag").to_string();

        let named = staged(&router, row).await;
        let submitted = Config {
            web_acl_id: named.clone(),
            ..stored_config.clone()
        };
        let answer =
            update_distribution(&router, &id, &submitted.update_xml(), Some(&etag)).await;
        if row.admitted {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200 from UpdateDistribution, got {} with {}",
                answer.status, answer.raw
            );
            let expected = named.clone().unwrap_or_default();
            let stored = stored_web_acl(&router, case, &id).await;
            assert_eq!(
                stored, expected,
                "case {case}: expected the update to leave WebACLId {expected:?}, got \
                 {stored:?}"
            );
            continue;
        }

        let dead = named.clone().unwrap_or_default();
        assert_refused(&answer, case, "UpdateDistribution", &dead);

        // The refused update wrote nothing a later read can see.
        let stored = stored_web_acl(&router, case, &id).await;
        assert_eq!(
            stored, control,
            "case {case}: expected the refused update to leave WebACLId {control:?}, got \
             {stored:?}"
        );
        let fetched = get_distribution(&router, &id).await;
        assert_eq!(
            fetched.header("etag"),
            etag,
            "case {case}: expected the refused update to leave the version at {etag:?}, got \
             {:?}",
            fetched.header("etag")
        );

        // And the same update naming the web ACL the distribution already runs under lands,
        // so the refusal left the version it states usable.
        let again =
            update_distribution(&router, &id, &stored_config.update_xml(), Some(&etag)).await;
        assert_eq!(
            again.status, 200,
            "case {case}: expected HTTP 200 once the update names a web ACL WAF holds, got \
             {} with {}",
            again.status, again.raw
        );
    }
}
