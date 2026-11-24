// SPDX-License-Identifier: CC0-1.0

//! A WAF web ACL cannot be deleted while a CloudFront distribution still names it.
//!
//! RULE: `wafv2:DeleteWebACL` refuses a web ACL that is still in front of a protected
//! resource — "before deleting any web ACL, first disassociate it from all resources" — with
//! the modelled `WAFAssociatedItemException`, and a CloudFront distribution is in front of
//! one exactly when the distribution's own `DistributionConfig$WebACLId` names it: that
//! association lives in the distribution's configuration rather than in WAF's bindings ("for
//! Amazon CloudFront, don't use this call. Instead, use your CloudFront distribution
//! configuration"), which is why `DeleteWebACL`'s own documentation sends an operator to
//! `ListDistributionsByWebACLId` for it. The refusal deletes nothing: WAF still reports the
//! web ACL and its lock token still lands a later delete. Once NO distribution names it any
//! more, however it stopped naming it, the same delete is served — and a second delete then
//! answers `WAFNonexistentItemException`, because the web ACL really is gone.
//!
//! This suite needs CloudFront and WAFv2 on one engine — the association is CloudFront's
//! record and the delete is WAF's decision — which is why it lives here rather than in either
//! service's crate.
//!
//! The rows differ along what the rule generalises over: HOW the distribution came to name
//! the web ACL (stated in the configuration the create submitted, or set by a later
//! `AssociateDistributionWebACL`), HOW MANY distributions name it at once (so clearing one of
//! two still leaves the delete refused), and HOW the naming ends (the whole-configuration
//! update stating an empty `WebACLId`, the dedicated `DisassociateDistributionWebACL`, or
//! deleting the distribution outright).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, Answer, API_VERSION, CLOUDFRONT_HOST, REGION,
};

/// The WAFv2 `Scope` a web ACL that protects a CloudFront distribution lives in: "for a
/// global resource type, such as a Amazon CloudFront distribution".
const SCOPE_CLOUDFRONT: &str = "CLOUDFRONT";

/// The `X-Amz-Target` prefix WAFv2's `awsJson1_1` operations are named under, from that
/// model's `targetPrefix`.
const WAF_TARGET: &str = "AWSWAF_20190729";

/// The modelled error WAF answers for a web ACL that is still in front of something, and the
/// whole sentence it states — fixed text naming nothing of the request (AWS writes both
/// apostrophes as U+2019).
const ASSOCIATED_ITEM: &str = "WAFAssociatedItemException";
const ASSOCIATED_ITEM_MESSAGE: &str = "AWS WAF couldn’t perform the operation because your \
     resource is being used by another resource or it’s associated with another resource.";

/// The modelled error WAF answers for a web ACL it holds nothing of.
const NONEXISTENT_ITEM: &str = "WAFNonexistentItemException";

/// CloudFront and WAFv2 on one engine, with request authentication off: each call carries a
/// credential scope (which is what routes it and names the Region) without a computed
/// signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(engine),
    );
    router
}

/// One WAFv2 call: `(the answer, its parsed body, the error type it named)`.
async fn waf(router: &MultiServiceRouter, operation: &str, body: &Value) -> (Answer, Value, String) {
    let answered = call(
        router,
        "wafv2",
        &format!("wafv2.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("{WAF_TARGET}.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    let text = answered.text();
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    // `awsJson1_1` names the error shape in `__type`, and the framework also stamps the
    // header every AWS JSON error carries.
    let named = parsed["__type"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| answered.header("x-amzn-errortype"));
    let named = named
        .split('/')
        .next()
        .unwrap_or_default()
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string();
    (answered, parsed, named)
}

/// One web ACL WAFv2 minted, as its own answer reports it: nothing here is built by hand.
#[derive(Debug, Clone)]
struct WebAcl {
    name: String,
    id: String,
    arn: String,
    lock_token: String,
}

/// `wafv2:CreateWebACL` in the `CLOUDFRONT` scope.
async fn create_web_acl(router: &MultiServiceRouter, case: &str, name: &str) -> WebAcl {
    let body = json!({
        "Name": name,
        "Scope": SCOPE_CLOUDFRONT,
        "DefaultAction": { "Allow": {} },
        "VisibilityConfig": {
            "SampledRequestsEnabled": false,
            "CloudWatchMetricsEnabled": false,
            "MetricName": name,
        },
    });
    let (answered, parsed, _) = waf(router, "CreateWebACL", &body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: CreateWebACL must succeed, got {} with {parsed}",
        answered.status
    );
    let field = |member: &str| {
        parsed["Summary"][member]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateWebACL answered no Summary.{member}: {parsed}")
            })
            .to_string()
    };
    WebAcl {
        name: field("Name"),
        id: field("Id"),
        arn: field("ARN"),
        lock_token: field("LockToken"),
    }
}

/// `wafv2:DeleteWebACL` for `web_acl` under `lock_token`.
async fn delete_web_acl(
    router: &MultiServiceRouter,
    web_acl: &WebAcl,
    lock_token: &str,
) -> (Answer, Value, String) {
    let body = json!({
        "Name": web_acl.name,
        "Scope": SCOPE_CLOUDFRONT,
        "Id": web_acl.id,
        "LockToken": lock_token,
    });
    waf(router, "DeleteWebACL", &body).await
}

/// The lock token WAF reports for `web_acl` now, or `None` when WAF holds it no longer.
async fn held_lock_token(router: &MultiServiceRouter, web_acl: &WebAcl) -> Option<String> {
    let body = json!({ "ARN": web_acl.arn });
    let (answered, parsed, _) = waf(router, "GetWebACL", &body).await;
    if answered.status != 200 {
        return None;
    }
    parsed["LockToken"].as_str().map(str::to_string)
}

/// The `DistributionConfig` a row submits: a disabled distribution over one custom origin,
/// naming `web_acl_id` when the row states one.
///
/// `PriceClass` is stated because an update submits the configuration WHOLE, and `Enabled` is
/// `false` so a row that ends the naming by deleting the distribution can: "the specified
/// CloudFront distribution is not disabled" is what a delete of an enabled one earns.
fn config_xml(caller_reference: &str, web_acl_id: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment><Enabled>false</Enabled>\
         <PriceClass>PriceClass_All</PriceClass>\
         <WebACLId>{web_acl_id}</WebACLId>\
         </DistributionConfig>"
    )
}

/// One CloudFront call over the restXml control plane.
async fn cloudfront(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: Bytes,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/xml")];
    if let Some(version) = if_match {
        headers.push(("if-match", version));
    }
    call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        method,
        path,
        &headers,
        body,
    )
    .await
}

/// `cloudfront:CreateDistribution` naming `web_acl_id`, answered as `(Id, ETag)` once the
/// clock has deployed it.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    web_acl_id: &str,
) -> (String, String) {
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        None,
        Bytes::from(config_xml(caller_reference, web_acl_id)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "case {case:?}: the created distribution reports no Id: {:?}",
        created.text()
    );
    awaited_deployment(router, case, &id).await;
    let etag = current_etag(router, case, &id).await;
    (id, etag)
}

/// The version `GetDistribution` reports for `id` now, and the `WebACLId` it reports.
async fn read_back(router: &MultiServiceRouter, case: &str, id: &str) -> (String, String) {
    let read = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}"),
        None,
        Bytes::new(),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetDistribution must succeed, got {} with {:?}",
        read.status,
        read.text()
    );
    (read.header("etag"), read.element("WebACLId"))
}

/// The current version of `id`, which every conditional write below states.
async fn current_etag(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    read_back(router, case, id).await.0
}

/// How a distribution came to name the web ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// Stated in the `DistributionConfig` the create submitted.
    ByCreate,
    /// Set by a later `AssociateDistributionWebACL`.
    ByAssociate,
}

/// How a distribution stops naming the web ACL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cleared {
    /// `UpdateDistribution` submitting the whole configuration with an empty `WebACLId`.
    UpdateConfig,
    /// `DisassociateDistributionWebACL`.
    Disassociate,
    /// `DeleteDistribution`: the distribution itself goes, so nothing names the web ACL.
    DeleteDistribution,
}

/// One row: how many distributions name the web ACL, how each came to, and how each stops.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    distributions: usize,
    reached: Reached,
    cleared: Cleared,
}

const ROWS: &[Row] = &[
    Row {
        case: "one distribution, named by its create, cleared by the configuration update",
        distributions: 1,
        reached: Reached::ByCreate,
        cleared: Cleared::UpdateConfig,
    },
    Row {
        case: "one distribution, named by an association, cleared by the disassociation",
        distributions: 1,
        reached: Reached::ByAssociate,
        cleared: Cleared::Disassociate,
    },
    Row {
        case: "one distribution, named by its create, cleared by deleting the distribution",
        distributions: 1,
        reached: Reached::ByCreate,
        cleared: Cleared::DeleteDistribution,
    },
    Row {
        case: "two distributions, named by their creates, cleared one at a time",
        distributions: 2,
        reached: Reached::ByCreate,
        cleared: Cleared::UpdateConfig,
    },
    Row {
        case: "two distributions, named by associations, cleared one at a time",
        distributions: 2,
        reached: Reached::ByAssociate,
        cleared: Cleared::Disassociate,
    },
];

/// Put the web ACL in front of one distribution the way `reached` says, answering its id.
async fn protected_distribution(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    web_acl: &WebAcl,
    reached: Reached,
) -> String {
    match reached {
        Reached::ByCreate => create_distribution(router, case, reference, &web_acl.arn).await.0,
        Reached::ByAssociate => {
            let (id, etag) = create_distribution(router, case, reference, "").await;
            let body = format!(
                "<AssociateDistributionWebACLRequest \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <WebACLArn>{}</WebACLArn></AssociateDistributionWebACLRequest>",
                web_acl.arn
            );
            let associated = cloudfront(
                router,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/associate-web-acl"),
                Some(&etag),
                Bytes::from(body),
            )
            .await;
            assert_eq!(
                associated.status, 200,
                "case {case:?}: AssociateDistributionWebACL must succeed, got {} with {:?}",
                associated.status,
                associated.text()
            );
            awaited_deployment(router, case, &id).await;
            id
        }
    }
}

/// Take the web ACL off one distribution the way `cleared` says.
async fn clear(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    id: &str,
    cleared: Cleared,
) {
    let etag = current_etag(router, case, id).await;
    let answered = match cleared {
        Cleared::UpdateConfig => {
            cloudfront(
                router,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/config"),
                Some(&etag),
                Bytes::from(config_xml(reference, "")),
            )
            .await
        }
        Cleared::Disassociate => {
            cloudfront(
                router,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/disassociate-web-acl"),
                Some(&etag),
                Bytes::new(),
            )
            .await
        }
        Cleared::DeleteDistribution => {
            cloudfront(
                router,
                "DELETE",
                &format!("/{API_VERSION}/distribution/{id}"),
                Some(&etag),
                Bytes::new(),
            )
            .await
        }
    };
    let expected = if cleared == Cleared::DeleteDistribution {
        204
    } else {
        200
    };
    assert_eq!(
        answered.status, expected,
        "case {case:?}: the write that ends the naming ({cleared:?}) must answer HTTP \
         {expected}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    if cleared != Cleared::DeleteDistribution {
        let (_, named) = read_back(router, case, id).await;
        assert!(
            named.is_empty(),
            "case {case:?}: expected the distribution to name no web ACL after {cleared:?}, \
             got {named:?}"
        );
    }
}

/// Assert `answered` is WAF's refusal for a web ACL that is still in front of something.
fn assert_refused(answered: &Answer, parsed: &Value, named: &str, case: &str, context: &str) {
    assert_eq!(
        answered.status, 400,
        "case {case:?} ({context}): expected HTTP 400 while a distribution names the web \
         ACL, got {} with {parsed}",
        answered.status
    );
    assert_eq!(
        named, ASSOCIATED_ITEM,
        "case {case:?} ({context}): expected {ASSOCIATED_ITEM}, got {named:?} with {parsed}"
    );
    let message = parsed["message"]
        .as_str()
        .or_else(|| parsed["Message"].as_str())
        .unwrap_or_default();
    assert_eq!(
        message, ASSOCIATED_ITEM_MESSAGE,
        "case {case:?} ({context}): expected the message {ASSOCIATED_ITEM_MESSAGE:?}, got \
         {message:?}"
    );
}

/// The delete is refused while any distribution names the web ACL, writes nothing while it
/// is, is served once none does, and answers `WAFNonexistentItemException` the second time.
#[tokio::test]
async fn deleting_a_web_acl_a_distribution_names_is_refused_until_nothing_names_it() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let web_acl = create_web_acl(&router, case, &format!("delete-guard-{index}")).await;

        // Each distribution reaches the web ACL the row's way, and is fully deployed before
        // the delete is tried — the condition the rule is stated about.
        let mut ids = Vec::new();
        for position in 0..row.distributions {
            let reference = format!("ref-delete-guard-{index}-{position}");
            ids.push((
                reference.clone(),
                protected_distribution(&router, case, &reference, &web_acl, row.reached).await,
            ));
        }
        for (_, id) in &ids {
            let (_, named) = read_back(&router, case, id).await;
            assert_eq!(
                named, web_acl.arn,
                "case {case:?}: expected the distribution to name {:?} before the delete, got \
                 {named:?}",
                web_acl.arn
            );
        }

        // Clearing the distributions ONE at a time: every delete tried while any of them
        // still names the web ACL is refused, and leaves WAF holding it at its own version.
        for cleared_so_far in 0..ids.len() {
            let lock_token = held_lock_token(&router, &web_acl).await.unwrap_or_else(|| {
                panic!("case {case:?}: WAF no longer holds the web ACL it was asked to delete")
            });
            let (answered, parsed, named) = delete_web_acl(&router, &web_acl, &lock_token).await;
            assert_refused(
                &answered,
                &parsed,
                &named,
                case,
                &format!("{} of {} cleared", cleared_so_far, ids.len()),
            );
            assert_eq!(
                held_lock_token(&router, &web_acl).await.as_deref(),
                Some(lock_token.as_str()),
                "case {case:?}: expected the refused delete to leave WAF holding the web ACL \
                 at {lock_token:?}"
            );

            let (reference, id) = &ids[cleared_so_far];
            clear(&router, case, reference, id, row.cleared).await;
        }

        // Nothing names it any more, so the very same delete lands.
        let lock_token = held_lock_token(&router, &web_acl).await.unwrap_or_else(|| {
            panic!("case {case:?}: WAF no longer holds the web ACL before the served delete")
        });
        let (answered, parsed, _) = delete_web_acl(&router, &web_acl, &lock_token).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the delete to be served once no distribution names the \
             web ACL, got {} with {parsed}",
            answered.status
        );
        assert_eq!(
            held_lock_token(&router, &web_acl).await,
            None,
            "case {case:?}: expected WAF to hold the web ACL no longer"
        );

        // And the second delete is about a web ACL that is really gone.
        let (answered, parsed, named) = delete_web_acl(&router, &web_acl, &lock_token).await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: expected HTTP 400 from the second delete, got {} with {parsed}",
            answered.status
        );
        assert_eq!(
            named, NONEXISTENT_ITEM,
            "case {case:?}: expected {NONEXISTENT_ITEM} from the second delete, got {named:?} \
             with {parsed}"
        );
    }
}

/// The guard reads CloudFront's state and nothing else: a web ACL no distribution ever named
/// is deleted at once, and one a distribution names only after the delete would have been
/// served is unaffected by that other distribution.
#[tokio::test]
async fn a_web_acl_no_distribution_names_is_deleted_at_once() {
    let case = "named-by-nothing";
    let router = emulator();
    let unnamed = create_web_acl(&router, case, "delete-guard-unnamed").await;
    let named = create_web_acl(&router, case, "delete-guard-named").await;

    // One distribution, in front of the OTHER web ACL: the account holds a protected
    // distribution, so what decides each delete is which web ACL that distribution names.
    create_distribution(&router, case, "ref-delete-guard-unnamed", &named.arn).await;

    let (answered, parsed, _) = delete_web_acl(&router, &unnamed, &unnamed.lock_token).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the web ACL no distribution names to be deleted, got {} with \
         {parsed}",
        answered.status
    );
    assert_eq!(
        held_lock_token(&router, &unnamed).await,
        None,
        "case {case:?}: expected WAF to hold the deleted web ACL no longer"
    );

    // While the one the distribution DOES name is refused.
    let lock_token = held_lock_token(&router, &named).await.unwrap_or_else(|| {
        panic!("case {case:?}: WAF no longer holds the web ACL the distribution names")
    });
    let (answered, parsed, error) = delete_web_acl(&router, &named, &lock_token).await;
    assert_refused(&answered, &parsed, &error, case, "the named web ACL");
}
