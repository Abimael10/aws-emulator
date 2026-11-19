// SPDX-License-Identifier: CC0-1.0

//! Reaching WAFv2's OWN control plane from a CloudFront suite.
//!
//! A web ACL is a WAF resource, so a suite that needs one the account really holds creates it
//! the way a client does — through WAFv2's own `CreateWebACL`, on the same engine this
//! service is mounted on — and names the ARN that service minted. Nothing here builds a WAF
//! identifier by hand: the ARN, the id and the lock token all come out of WAF's own answer,
//! which is what makes an association that resolves one of them an association of a web ACL
//! that exists.
//!
//! The helpers are `pub(crate)` and live here ONCE, so the suites that put a web ACL in front
//! of a distribution (the association surface) and the suites that drive one at the edge reach
//! WAFv2 the same way rather than each carrying its own copy of the wire details.

use smithy_server_runtime::MultiServiceRouter;

/// The WAFv2 control plane's own endpoint, which is what routes a request to that service.
pub(crate) const WAF_HOST: &str = "wafv2.amazonaws.com";

/// The `X-Amz-Target` prefix WAFv2's `awsJson1_1` operations are named under, from that
/// model's `targetPrefix`.
pub(crate) const WAF_TARGET: &str = "AWSWAF_20190729";

/// A SigV4 credential scope naming WAFv2's signing name, so the router resolves that
/// service for the setup calls.
pub(crate) const WAF_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/wafv2/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The WAFv2 `Scope` a web ACL that protects a CloudFront distribution lives in: "for a
/// global resource type, such as a Amazon CloudFront distribution".
pub(crate) const SCOPE_CLOUDFRONT: &str = "CLOUDFRONT";

/// The WAFv2 `Scope` of the REGIONAL namespace, whose web ACLs protect regional resources and
/// so never stand in front of a distribution.
pub(crate) const SCOPE_REGIONAL: &str = "REGIONAL";

/// A rule that COUNTS a request whose URI path contains `probe`, so a web ACL carrying it
/// differs from one with no rules at all without changing what it lets through.
///
/// `ByteMatchStatement$SearchString` is a blob, so the JSON carries the base64 of `probe`.
pub(crate) const COUNTING_RULE: &str = "{\"Name\":\"count-the-probe\",\"Priority\":0,\
     \"Statement\":{\"ByteMatchStatement\":{\"SearchString\":\"cHJvYmU=\",\
     \"FieldToMatch\":{\"UriPath\":{}},\
     \"TextTransformations\":[{\"Priority\":0,\"Type\":\"NONE\"}],\
     \"PositionalConstraint\":\"CONTAINS\"}},\
     \"Action\":{\"Count\":{}},\
     \"VisibilityConfig\":{\"SampledRequestsEnabled\":false,\
     \"CloudWatchMetricsEnabled\":false,\"MetricName\":\"counttheprobe\"}}";

/// Call one WAFv2 operation and answer `(status, body)`.
pub(crate) async fn waf_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &str,
) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", WAF_HOST)
        .header("authorization", WAF_AUTHORIZATION)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{WAF_TARGET}.{operation}"))
        .body(::bytes::Bytes::from(body.to_string()))
        .expect("the setup request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}

/// The value of the string member `name` in the JSON document `body`, or `None` when the
/// document carries none.
///
/// Enough of a reader for the setup answers these suites read: every member taken from them
/// is a flat string the service names once.
pub(crate) fn json_string(body: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\":\"");
    let start = body.find(&marker)? + marker.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// One web ACL WAFv2 minted, as its own answer reports it.
#[derive(Debug, Clone)]
pub(crate) struct CreatedWebAcl {
    /// `WebACLSummary$Name`, which a delete addresses the web ACL by.
    pub(crate) name: String,
    /// `WebACLSummary$Id`.
    pub(crate) id: String,
    /// `WebACLSummary$ARN` — the identifier a CloudFront resource names its web ACL by.
    pub(crate) arn: String,
    /// `WebACLSummary$LockToken`, the version a delete has to state.
    pub(crate) lock_token: String,
    /// The `Scope` the web ACL was created in, which a delete has to state too.
    pub(crate) scope: &'static str,
}

/// Create one web ACL in `scope` whose default action is `default_action` and whose rules are
/// `rules` (an empty string for a web ACL with no rules), and answer what WAFv2 minted.
pub(crate) async fn create_web_acl(
    router: &MultiServiceRouter,
    name: &str,
    scope: &'static str,
    default_action: &str,
    rules: &str,
) -> CreatedWebAcl {
    let body = format!(
        "{{\"Name\":\"{name}\",\"Scope\":\"{scope}\",\
         \"DefaultAction\":{default_action},\
         \"VisibilityConfig\":{{\"SampledRequestsEnabled\":false,\
         \"CloudWatchMetricsEnabled\":false,\"MetricName\":\"{name}\"}},\
         \"Rules\":[{rules}]}}"
    );
    let (status, answered) = waf_call(router, "CreateWebACL", &body).await;
    assert_eq!(
        status, 200,
        "case {name}: expected HTTP 200 from CreateWebACL, got {status} with {answered}"
    );
    let field = |member: &str| {
        json_string(&answered, member).unwrap_or_else(|| {
            panic!("case {name}: CreateWebACL answered no {member}: {answered}")
        })
    };
    CreatedWebAcl {
        name: field("Name"),
        id: field("Id"),
        arn: field("ARN"),
        lock_token: field("LockToken"),
        scope,
    }
}

/// Delete one web ACL WAFv2 holds, so a suite can state what an association does with an
/// identifier the account no longer has.
pub(crate) async fn delete_web_acl(router: &MultiServiceRouter, web_acl: &CreatedWebAcl) {
    let body = format!(
        "{{\"Name\":\"{}\",\"Scope\":\"{}\",\"Id\":\"{}\",\"LockToken\":\"{}\"}}",
        web_acl.name, web_acl.scope, web_acl.id, web_acl.lock_token
    );
    let (status, answered) = waf_call(router, "DeleteWebACL", &body).await;
    assert_eq!(
        status, 200,
        "case {}: expected HTTP 200 from DeleteWebACL, got {status} with {answered}",
        web_acl.name
    );
}
