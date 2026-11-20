// SPDX-License-Identifier: CC0-1.0

//! The WAFv2 WEB ACL a distribution's configuration puts in front of it, decided at the
//! edge — end to end over HTTP, with both services mounted on one router.
//!
//! The rule under test: `DistributionConfig$WebACLId` names "the WAF web ACL, if any, to
//! associate with this distribution … to specify a web ACL created using the latest version
//! of WAF, use the ACL ARN", and from then on the distribution's viewer traffic is decided
//! by that web ACL — "CloudFront responds to requests either with the requested content or
//! with an HTTP 403 status code (Forbidden)". A request the web ACL does not let through is
//! answered at the EDGE: no origin request is made, so none of the origin's bytes appear in
//! the answer, and the answer carries the status WAF serves that action under.
//!
//! The rows span what the rule generalises over: a web ACL whose rule BLOCKS the path, the
//! same web ACL on a path its rule does not match (which its `DefaultAction` then allows), a
//! web ACL whose default action blocks everything, and the three ways a configuration names
//! no WAFv2 web ACL at all — the member left out, a WAF Classic ACL id (which is no ARN, and
//! whose rules this emulator holds none of), and an ARN this emulator does not hold. Every
//! row is driven over the SAME distribution configuration and the same origin, so what
//! decides each answer is the web ACL and nothing else.
//!
//! The origin is this service's own control-plane endpoint, whose
//! `GET /2020-05-31/distribution` answers a `DistributionList`: an origin this emulator
//! really serves, so an allowed request carries the origin's own document and a refused one
//! demonstrably carries none of it.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{past_propagation, API_VERSION, HOST};
use crate::waf_control_plane::SCOPE_CLOUDFRONT;

/// The viewer path the blocking rule matches. The `SearchString` that matches it is the
/// base64 of `blockme` in [`BLOCKING_RULE`] — `ByteMatchStatement$SearchString` is a blob,
/// so the JSON carries it encoded.
pub(crate) const BLOCKED_PATH: &str = "/blockme.html";

/// One router carrying BOTH services over one shared engine: the distribution's edge asks
/// the WAFv2 service about every viewer request, so the two have to be mounted together for
/// that question to have an answer.
pub(crate) fn emulator_with_waf() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(engine),
    );
    router
}

/// Create one web ACL whose rules are `rules` and whose default action is `default_action`,
/// and answer the ARN WAFv2 minted for it.
///
/// The ARN is read out of the create's own answer, never built here: it is the identifier that
/// service assigned, and it is what a distribution's configuration names. The call itself is
/// [`crate::waf_control_plane`]'s, so this suite and the association suites reach WAFv2 the
/// same way.
async fn create_web_acl(
    router: &MultiServiceRouter,
    name: &str,
    default_action: &str,
    rules: &str,
) -> String {
    crate::waf_control_plane::create_web_acl(router, name, SCOPE_CLOUDFRONT, default_action, rules)
        .await
        .arn
}

/// A rule that BLOCKS a viewer request whose URI path contains `blockme`.
pub(crate) const BLOCKING_RULE: &str = "{\"Name\":\"block-the-path\",\"Priority\":0,\
     \"Statement\":{\"ByteMatchStatement\":{\"SearchString\":\"YmxvY2ttZQ==\",\
     \"FieldToMatch\":{\"UriPath\":{}},\
     \"TextTransformations\":[{\"Priority\":0,\"Type\":\"NONE\"}],\
     \"PositionalConstraint\":\"CONTAINS\"}},\
     \"Action\":{\"Block\":{}},\
     \"VisibilityConfig\":{\"SampledRequestsEnabled\":false,\
     \"CloudWatchMetricsEnabled\":false,\"MetricName\":\"blockthepath\"}}";

/// One `DistributionConfig` over the served origin, naming `web_acl_id` when it names one.
fn config_xml(reference: &str, web_acl_id: Option<&str>) -> String {
    let member = web_acl_id
        .map(|id| format!("<WebACLId>{id}</WebACLId>"))
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>{member}\
         </DistributionConfig>"
    )
}

/// Create one distribution under this router and answer the domain name it published.
async fn distribution(router: &MultiServiceRouter, reference: &str, web_acl_id: Option<&str>) -> String {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("/{API_VERSION}/distribution"))
        .header("host", HOST)
        .header(
            "authorization",
            crate::distribution_lifecycle::AUTHORIZATION,
        )
        .header("content-type", "application/xml")
        .body(::bytes::Bytes::from(config_xml(reference, web_acl_id)))
        .expect("the create request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, 201,
        "case {reference}: expected HTTP 201 creating the distribution, got {status} with {body}"
    );
    let marker = "<DomainName>";
    let start = body
        .find(marker)
        .unwrap_or_else(|| panic!("case {reference}: the create carries no DomainName: {body}"))
        + marker.len();
    let rest = &body[start..];
    let end = rest
        .find('<')
        .unwrap_or_else(|| panic!("case {reference}: the DomainName is unterminated: {body}"));
    rest[..end].to_string()
}

/// One VIEWER request, addressed HOST-STYLE at the name the distribution published and
/// carrying no credential — which is what a viewer is.
pub(crate) async fn view(router: &MultiServiceRouter, domain: &str, path: &str) -> Viewed {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Viewed {
        status,
        x_cache,
        body,
    }
}

/// What the edge answered one viewer request with.
pub(crate) struct Viewed {
    pub(crate) status: u16,
    pub(crate) x_cache: String,
    pub(crate) body: String,
}

/// What a row expects of the web ACL in front of its distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The request is refused at the edge, with no origin bytes in the answer.
    Refused,
    /// The request reaches the origin and is served its answer.
    Served,
}

/// A web ACL that BLOCKS the path its rule names refuses exactly that path at the edge, and
/// lets every other path through to the origin; a web ACL whose DEFAULT action blocks
/// refuses every path; and a configuration that names no WAFv2 web ACL — the member left
/// out, a WAF Classic id, or an ARN this emulator does not hold — is served as it would be
/// with no WAF in front of it at all.
#[tokio::test]
async fn the_web_acl_a_distribution_names_decides_its_viewer_traffic() {
    let router = emulator_with_waf();
    let blocking = create_web_acl(&router, "edge-block-path", "{\"Allow\":{}}", BLOCKING_RULE).await;
    let default_block = create_web_acl(&router, "edge-block-all", "{\"Block\":{}}", "").await;
    let served_path = format!("/{API_VERSION}/distribution");

    // (case, the `WebACLId` the configuration names, the viewer path, what the edge owes)
    let rows: Vec<(&str, Option<String>, String, Expected)> = vec![
        (
            "a blocking rule, on the path it matches",
            Some(blocking.clone()),
            BLOCKED_PATH.to_string(),
            Expected::Refused,
        ),
        (
            "a blocking rule, on a path it does not match",
            Some(blocking.clone()),
            served_path.clone(),
            Expected::Served,
        ),
        (
            "a web ACL whose default action blocks",
            Some(default_block),
            served_path.clone(),
            Expected::Refused,
        ),
        (
            "no WebACLId at all",
            None,
            served_path.clone(),
            Expected::Served,
        ),
        (
            "a WAF Classic ACL id, which is no ARN",
            Some("a1b2c3d4-5678-90ab-cdef-EXAMPLE11111".to_string()),
            served_path.clone(),
            Expected::Served,
        ),
        (
            "an ARN this emulator does not hold",
            Some(
                "arn:aws:wafv2:us-east-1:000000000000:global/webacl/absent/\
                 a1b2c3d4-5678-90ab-cdef-EXAMPLE11111"
                    .to_string(),
            ),
            served_path.clone(),
            Expected::Served,
        ),
    ];

    for (index, (case, web_acl_id, path, expected)) in rows.into_iter().enumerate() {
        let domain = distribution(
            &router,
            &format!("ref-web-acl-{index}"),
            web_acl_id.as_deref(),
        )
        .await;
        // The distribution has to have reached the edge locations before a viewer is served.
        ::tokio::time::sleep(past_propagation()).await;
        let answer = view(&router, &domain, &path).await;
        match expected {
            Expected::Refused => {
                assert_eq!(
                    answer.status, 403,
                    "case {case}: expected the edge to refuse the request 403, got {} with {:?}",
                    answer.status, answer.body
                );
                assert!(
                    !answer.body.contains("DistributionList"),
                    "case {case}: a refusal the edge produced carries none of the origin's \
                     bytes, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.x_cache, "Error from cloudfront",
                    "case {case}: expected the answer to be reported as CloudFront's own, got \
                     {:?}",
                    answer.x_cache
                );
            }
            Expected::Served => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected the origin's answer, got {} with {:?}",
                    answer.status, answer.body
                );
                assert!(
                    answer.body.contains("DistributionList"),
                    "case {case}: expected the origin's own document, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.x_cache, "Miss from cloudfront",
                    "case {case}: expected a cache miss, got {:?}",
                    answer.x_cache
                );
            }
        }
    }
}
