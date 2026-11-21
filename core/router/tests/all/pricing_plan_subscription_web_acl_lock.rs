// SPDX-License-Identifier: CC0-1.0

//! A distribution a flat-rate pricing plan is bought over keeps its web ACL.
//!
//! RULE: a flat-rate CloudFront pricing plan is sold over "exactly one Amazon CloudFront
//! distribution and exactly one WAF web ACL"
//! (`smithy_query(query="doc",shape="CreateSubscriptionInput",member="resourceArns")`), so
//! for as long as a subscription covers a distribution the web ACL in front of it has to
//! stay there: every CloudFront write that would CLEAR the stored `WebACLId` or point it at
//! another web ACL is refused with the modelled `InvalidArgument` (HTTP 400) stating "You
//! can't remove or replace the web ACL for your distribution. Distributions with a pricing
//! plan subscription must have a web ACL resource.", and the stored configuration is left
//! exactly as it was. The gate is state-dependent, not a shape check: the identical write is
//! served once no subscription covers the distribution, and a write that leaves the
//! association as it is was never gated at all.
//!
//! This suite needs Pricing Plan Manager, CloudFront and WAFv2 on one engine — the gate is a
//! CloudFront write weighed against the selling service's own record, and only WAFv2 mints a
//! web ACL ARN — which is why it lives here rather than in either service's crate.
//!
//! The rows differ along what the rule generalises over: WHICH write would take the web ACL
//! off (the whole-configuration update, the dedicated disassociation, an association naming
//! another web ACL), whether the distribution is covered by a subscription at all, and
//! whether the write changes the association or leaves it in place.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, Answer, API_VERSION, CLOUDFRONT_HOST, REGION,
};

/// The plan family whose subscriptions name a distribution and a web ACL.
const CLOUDFRONT_FAMILY: &str = "CloudFront";

/// The tier the recorded call asks for; `FREE` is "always activated immediately".
const FREE_TIER: &str = "FREE";

/// The scope a web ACL that fronts a CloudFront distribution is created in.
const SCOPE_CLOUDFRONT: &str = "CLOUDFRONT";

/// The modelled refusal a CloudFront write that would unprotect a subscribed distribution
/// earns, and the whole sentence it states — both halves fixed text, naming no value from
/// the request and none from either store.
const INVALID_ARGUMENT: &str = "InvalidArgument";
const WEB_ACL_HELD: &str =
    "You can't remove or replace the web ACL for your distribution. Distributions with a \
     pricing plan subscription must have a web ACL resource.";

/// A sentence as it travels inside a restXml error document, where an apostrophe is written
/// as its XML entity.
fn as_xml_text(sentence: &str) -> String {
    sentence.replace('\'', "&apos;")
}

/// The three services this suite exercises, mounted on one engine exactly as the router
/// mounts them, with request authentication off: each call carries a credential scope
/// (which is what routes it and names the Region) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_pricingplanmanager_awspricingplanmanager::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
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

/// The `DistributionConfig` this suite submits: the same document every time but for the
/// `WebACLId` it names, so what one write differs from another in is the association alone.
///
/// `PriceClass` is stated because an update submits the configuration WHOLE — a member the
/// stored record carries and the document leaves out is a modification CloudFront refuses.
fn config_xml(caller_reference: &str, web_acl_id: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>a.example.com</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment><Enabled>false</Enabled>\
         <PriceClass>PriceClass_All</PriceClass>\
         <WebACLId>{web_acl_id}</WebACLId>\
         </DistributionConfig>"
    )
}

/// `cloudfront:CreateDistribution` over a custom origin, fronted by `web_acl_arn`, answering
/// `(Id, ARN)` once CloudFront reports it deployed.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    web_acl_arn: &str,
) -> (String, String) {
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        Bytes::from(config_xml(caller_reference, web_acl_arn)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, arn) = (created.element("Id"), created.element("ARN"));
    assert!(
        !id.is_empty() && !arn.is_empty(),
        "case {case:?}: the created distribution reports no Id/ARN: {:?}",
        created.text()
    );
    awaited_deployment(router, case, &id).await;
    (id, arn)
}

/// `wafv2:CreateWebACL` in the `CLOUDFRONT` scope, answering the web ACL's ARN.
async fn create_web_acl(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = json!({
        "Name": name,
        "Scope": SCOPE_CLOUDFRONT,
        "DefaultAction": { "Allow": {} },
        "VisibilityConfig": {
            "SampledRequestsEnabled": true,
            "CloudWatchMetricsEnabled": true,
            "MetricName": name,
        },
    });
    let created = call(
        router,
        "wafv2",
        &format!("wafv2.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "AWSWAF_20190729.CreateWebACL"),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateWebACL must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let parsed: Value = serde_json::from_str(&created.text()).unwrap_or(Value::Null);
    let arn = parsed["Summary"]["ARN"].as_str().unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "case {case:?}: the created web ACL reports no Summary.ARN: {parsed}"
    );
    arn.to_string()
}

/// `cloudfront:GetDistributionConfig`: `(the WebACLId it reports, the current ETag, the
/// whole document)`.
async fn read_config(router: &MultiServiceRouter, case: &str, id: &str) -> (String, String, String) {
    let read = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetDistributionConfig must succeed, got {} with {:?}",
        read.status,
        read.text()
    );
    (read.element("WebACLId"), read.header("etag"), read.text())
}

/// One Pricing Plan Manager call, optionally under `If-Match` (the header the mutating
/// operations bind their `ifMatch` member to): `(status, parsed body, the error code the
/// answer named)`.
async fn pricing(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    if_match: Option<&str>,
) -> (u16, Value, String) {
    let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    if let Some(tag) = if_match {
        headers.push(("if-match", tag));
    }
    let answered = call(
        router,
        "pricingplanmanager",
        &format!("pricingplanmanager.{REGION}.amazonaws.com"),
        "POST",
        &format!("/v1/{operation}"),
        &headers,
        Bytes::from(body.to_string()),
    )
    .await;
    let code = answered.header("x-amzn-errortype");
    let code = code
        .split('/')
        .next()
        .unwrap_or(&code)
        .split('#')
        .next_back()
        .unwrap_or(&code)
        .to_string();
    let text = answered.text();
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (answered.status, parsed, code)
}

/// `pricingplanmanager:CreateSubscription` over the distribution and the web ACL in front of
/// it, answering `(the subscription's ARN, its ETag)`.
async fn subscribe(
    router: &MultiServiceRouter,
    case: &str,
    distribution_arn: &str,
    web_acl_arn: &str,
) -> (String, String) {
    let body = json!({
        "planFamily": CLOUDFRONT_FAMILY,
        "planTier": FREE_TIER,
        "resourceArns": [distribution_arn, web_acl_arn],
        "approvalMode": "IMMEDIATE",
    });
    let answered = call(
        router,
        "pricingplanmanager",
        &format!("pricingplanmanager.{REGION}.amazonaws.com"),
        "POST",
        "/v1/CreateSubscription",
        &[("content-type", "application/json")],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: CreateSubscription must succeed, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let parsed: Value = serde_json::from_str(&answered.text()).unwrap_or(Value::Null);
    let arn = parsed["arn"].as_str().unwrap_or_default().to_string();
    assert!(
        !arn.is_empty(),
        "case {case:?}: the created subscription reports no arn: {parsed}"
    );
    assert_eq!(
        parsed["status"].as_str(),
        Some("ACTIVE"),
        "case {case:?}: the free-tier subscription must be active at once: {parsed}"
    );
    (arn, answered.header("etag"))
}

/// Which CloudFront write a row makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// `UpdateDistribution` submitting the whole configuration, with this `WebACLId`.
    UpdateConfig(WebAcl),
    /// `DisassociateDistributionWebACL`.
    Disassociate,
    /// `AssociateDistributionWebACL` naming this web ACL.
    Associate(WebAcl),
}

/// Which web ACL a row's write names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebAcl {
    /// None at all — the empty value that names no association.
    None,
    /// The one the distribution already runs under.
    Current,
    /// Another web ACL the account holds.
    Other,
}

/// Make the write `write` makes against `id`, under the current `ETag`.
async fn write_distribution(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    caller_reference: &str,
    write: Write,
    current: &str,
    other: &str,
) -> Answer {
    let named = |which: WebAcl| match which {
        WebAcl::None => String::new(),
        WebAcl::Current => current.to_string(),
        WebAcl::Other => other.to_string(),
    };
    let (_web_acl, etag, _document) = read_config(router, case, id).await;
    match write {
        Write::UpdateConfig(which) => {
            call(
                router,
                "cloudfront",
                CLOUDFRONT_HOST,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/config"),
                &[("content-type", "application/xml"), ("if-match", &etag)],
                Bytes::from(config_xml(caller_reference, &named(which))),
            )
            .await
        }
        Write::Disassociate => {
            call(
                router,
                "cloudfront",
                CLOUDFRONT_HOST,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/disassociate-web-acl"),
                &[("content-type", "application/xml"), ("if-match", &etag)],
                Bytes::new(),
            )
            .await
        }
        Write::Associate(which) => {
            let body = format!(
                "<AssociateDistributionWebACLRequest \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <WebACLArn>{}</WebACLArn></AssociateDistributionWebACLRequest>",
                named(which)
            );
            call(
                router,
                "cloudfront",
                CLOUDFRONT_HOST,
                "PUT",
                &format!("/{API_VERSION}/distribution/{id}/associate-web-acl"),
                &[("content-type", "application/xml"), ("if-match", &etag)],
                Bytes::from(body),
            )
            .await
        }
    }
}

/// One row: the write it makes, whether a subscription covers the distribution, and whether
/// the write must be refused.
struct Row {
    case: &'static str,
    write: Write,
    subscribed: bool,
    refused: bool,
}

/// Every CloudFront write that would take the web ACL off a subscribed distribution is
/// refused and changes nothing; the same writes are served for a distribution no
/// subscription covers, and a write that leaves the association in place is always served.
#[tokio::test]
async fn a_subscribed_distributions_web_acl_cannot_be_removed_or_replaced() {
    let rows = [
        Row {
            case: "the whole configuration, with the web ACL cleared, while subscribed",
            write: Write::UpdateConfig(WebAcl::None),
            subscribed: true,
            refused: true,
        },
        Row {
            case: "the whole configuration, naming another web ACL, while subscribed",
            write: Write::UpdateConfig(WebAcl::Other),
            subscribed: true,
            refused: true,
        },
        Row {
            case: "the dedicated disassociation, while subscribed",
            write: Write::Disassociate,
            subscribed: true,
            refused: true,
        },
        Row {
            case: "an association naming another web ACL, while subscribed",
            write: Write::Associate(WebAcl::Other),
            subscribed: true,
            refused: true,
        },
        Row {
            case: "the whole configuration, keeping the web ACL, while subscribed",
            write: Write::UpdateConfig(WebAcl::Current),
            subscribed: true,
            refused: false,
        },
        Row {
            case: "an association naming the web ACL already in place, while subscribed",
            write: Write::Associate(WebAcl::Current),
            subscribed: true,
            refused: false,
        },
        Row {
            case: "the whole configuration, with the web ACL cleared, unsubscribed",
            write: Write::UpdateConfig(WebAcl::None),
            subscribed: false,
            refused: false,
        },
        Row {
            case: "the dedicated disassociation, unsubscribed",
            write: Write::Disassociate,
            subscribed: false,
            refused: false,
        },
        Row {
            case: "an association naming another web ACL, unsubscribed",
            write: Write::Associate(WebAcl::Other),
            subscribed: false,
            refused: false,
        },
    ];

    for row in &rows {
        let Row {
            case,
            write,
            subscribed,
            refused,
        } = row;
        // An account of its own per row: a resource is covered by at most one subscription,
        // and each row decides what its own distribution's configuration ends up as.
        let router = emulator();
        let protecting = create_web_acl(&router, case, "held-acl").await;
        let other = create_web_acl(&router, case, "other-acl").await;
        let caller_reference = "web-acl-lock";
        let (id, distribution_arn) =
            create_distribution(&router, case, caller_reference, &protecting).await;
        if *subscribed {
            subscribe(&router, case, &distribution_arn, &protecting).await;
        }

        let answered = write_distribution(
            &router,
            case,
            &id,
            caller_reference,
            *write,
            &protecting,
            &other,
        )
        .await;

        if *refused {
            assert_eq!(
                answered.status, 400,
                "case {case:?}: expected the write to be refused 400, got {} with {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.element("Code"),
                INVALID_ARGUMENT,
                "case {case:?}: expected {INVALID_ARGUMENT}, got {:?}",
                answered.text()
            );
            assert_eq!(
                answered.element("Message"),
                as_xml_text(WEB_ACL_HELD),
                "case {case:?}: expected the message {WEB_ACL_HELD:?}, got {:?}",
                answered.text()
            );
            // Inert: the stored configuration still reports the web ACL it ran under.
            let (web_acl, _etag, document) = read_config(&router, case, &id).await;
            assert_eq!(
                web_acl, protecting,
                "case {case:?}: a refused write must leave the association alone, got \
                 {document:?}"
            );
        } else {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the write to be served, got {} with {:?}",
                answered.status,
                answered.text()
            );
            let expected = match write {
                Write::Disassociate | Write::UpdateConfig(WebAcl::None) => String::new(),
                Write::UpdateConfig(WebAcl::Other) | Write::Associate(WebAcl::Other) => {
                    other.clone()
                }
                Write::UpdateConfig(WebAcl::Current) | Write::Associate(WebAcl::Current) => {
                    protecting.clone()
                }
                // No row associates nothing: an empty `WebACLArn` names no web ACL to
                // associate, which that operation refuses for its own reason.
                Write::Associate(WebAcl::None) => unreachable!("no row makes this write"),
            };
            let (web_acl, _etag, document) = read_config(&router, case, &id).await;
            assert_eq!(
                web_acl, expected,
                "case {case:?}: expected the stored WebACLId {expected:?}, got {document:?}"
            );
        }
    }
}

/// The gate is STATE-DEPENDENT: the very write that was refused is served once the
/// subscription covering the distribution is gone, and the web ACL comes off then.
#[tokio::test]
async fn the_same_removal_is_served_once_the_subscription_is_cancelled() {
    let router = emulator();
    let case = "cancel the subscription, then remove the web ACL";
    let caller_reference = "web-acl-lock-release";

    let protecting = create_web_acl(&router, case, "released-acl").await;
    let (id, distribution_arn) =
        create_distribution(&router, case, caller_reference, &protecting).await;
    let (subscription, subscription_etag) =
        subscribe(&router, case, &distribution_arn, &protecting).await;

    let refused = write_distribution(
        &router,
        case,
        &id,
        caller_reference,
        Write::UpdateConfig(WebAcl::None),
        &protecting,
        &protecting,
    )
    .await;
    assert_eq!(
        (refused.status, refused.element("Code")),
        (400, INVALID_ARGUMENT.to_string()),
        "case {case:?}: while the subscription covers it the removal must be refused, got \
         {} with {:?}",
        refused.status,
        refused.text()
    );

    let (cancel_status, cancelled, cancel_code) = pricing(
        &router,
        "CancelSubscription",
        &json!({"arn": subscription.clone()}),
        Some(&subscription_etag),
    )
    .await;
    assert_eq!(
        cancel_status, 200,
        "case {case:?}: the cancellation must be accepted, got {cancel_status} \
         {cancel_code} {cancelled}"
    );

    // Wait for the CONDITION — the cancellation having taken effect, which the selling
    // service reports by no longer holding the subscription — rather than for a time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let (status, body, code) = pricing(
            &router,
            "GetSubscription",
            &json!({"arn": subscription.clone()}),
            None,
        )
        .await;
        if status == 404 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the scheduled cancellation must take effect on the clock; \
             GetSubscription still answers {status} {code} {body}"
        );
        ::tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let served = write_distribution(
        &router,
        case,
        &id,
        caller_reference,
        Write::UpdateConfig(WebAcl::None),
        &protecting,
        &protecting,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: once nothing covers the distribution the same write must be \
         served, got {} with {:?}",
        served.status,
        served.text()
    );
    let (web_acl, _etag, document) = read_config(&router, case, &id).await;
    assert!(
        web_acl.is_empty() && !document.contains(&protecting),
        "case {case:?}: the web ACL must be off the distribution, got {document:?}"
    );
}
