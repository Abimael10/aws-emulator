// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ONE answer this service gives for a resource it does not
//! hold, driven over the wire through the router the production emulator uses.
//!
//! The rule under test is about wording, not about one recorded request: every
//! not-found refusal of this service carries the modelled `ResourceNotFoundException`
//! with HTTP 404 and ONE fixed message — it names neither the kind of resource asked
//! for nor any id, ARN or other value the request carried.
//!
//! The table spans what the rule generalises over: every operation of this service
//! that can answer not-found (gateway reads, deletes, the link surface, the
//! routing-rule surface and the certificate surface), each addressed with ids that
//! satisfy the modelled patterns so the existence check is what refuses, and with ids
//! of several forms — minted-looking, shortest the pattern admits, and one carrying
//! every character class — so a message built from the request would show up whatever
//! the request said.

use std::sync::Arc;

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::RESOURCE_NOT_FOUND_MESSAGE;
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// An ACM certificate ARN inside the modelled `AcmCertificateArn` pattern and its
/// published length bounds (75..256).
const CERTIFICATE_ARN: &str =
    "arn:aws:acm:us-west-2:123456789012:certificate/0123abcd-4567-89ef-0123-456789abcdef";

/// Ids no create ever minted, each satisfying its member's modelled pattern so the
/// request reaches the existence check: a minted-looking triple, the shortest the
/// patterns admit, and one carrying every character class they allow.
const UNKNOWN_IDS: &[(&str, &str, &str, &str)] = &[
    (
        "ids of the minted shape",
        "rtb-gw-0123456789abcdef01",
        "link-0123456789abcdef01",
        "rule-0123456789abcdef01",
    ),
    ("the shortest ids the patterns admit", "rtb-gw-a", "link-a", "rule-a"),
    (
        "every character class the patterns admit",
        "rtb-gw-a9-z",
        "link-a9-z",
        "rule-a9-z",
    ),
];

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return `(status, wire error code, body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String, Value) {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, code, body)
    }
}

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

/// Every call of this service that can answer not-found, for one (gateway, link,
/// rule) triple nothing minted: the operation's name, its method, its path and its
/// body.
fn calls(gateway_id: &str, link_id: &str, rule_id: &str) -> Vec<(String, &'static str, String, Option<Value>)> {
    let certificate = format!(
        "/responder-gateway/{gateway_id}/certificate?acmCertificateArn={}",
        utf8_percent_encode(CERTIFICATE_ARN, NON_ALPHANUMERIC)
    );
    vec![
        (
            "GetRequesterGateway".to_string(),
            "GET",
            format!("/requester-gateway/{gateway_id}"),
            None,
        ),
        (
            "DeleteRequesterGateway".to_string(),
            "DELETE",
            format!("/requester-gateway/{gateway_id}"),
            None,
        ),
        (
            "GetResponderGateway".to_string(),
            "GET",
            format!("/responder-gateway/{gateway_id}"),
            None,
        ),
        (
            "DeleteResponderGateway".to_string(),
            "DELETE",
            format!("/responder-gateway/{gateway_id}"),
            None,
        ),
        (
            "GetLink".to_string(),
            "GET",
            format!("/gateway/{gateway_id}/link/{link_id}"),
            None,
        ),
        (
            "DeleteLink".to_string(),
            "DELETE",
            format!("/gateway/{gateway_id}/link/{link_id}"),
            None,
        ),
        (
            "RejectLink".to_string(),
            "POST",
            format!("/gateway/{gateway_id}/link/{link_id}/reject"),
            None,
        ),
        (
            "AcceptLink".to_string(),
            "POST",
            format!("/gateway/{gateway_id}/link/{link_id}/accept"),
            Some(json!({ "logSettings": log_settings() })),
        ),
        (
            "CreateLink".to_string(),
            "POST",
            format!("/gateway/{gateway_id}/create-link"),
            Some(json!({ "peerGatewayId": gateway_id, "logSettings": log_settings() })),
        ),
        (
            "GetLinkRoutingRule".to_string(),
            "GET",
            format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        ),
        (
            "CreateLinkRoutingRule".to_string(),
            "POST",
            format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule"),
            Some(json!({
                "clientToken": "token-rule",
                "priority": 1,
                "conditions": { "pathPrefix": "/bid" },
            })),
        ),
        (
            "AssociateCertificate".to_string(),
            "POST",
            format!("/responder-gateway/{gateway_id}/certificate"),
            Some(json!({ "acmCertificateArn": CERTIFICATE_ARN, "clientToken": "token-certificate" })),
        ),
        (
            "GetCertificateAssociation".to_string(),
            "GET",
            certificate.clone(),
            None,
        ),
        ("DisassociateCertificate".to_string(), "DELETE", certificate, None),
    ]
}

#[tokio::test]
async fn every_not_found_refusal_of_this_service_states_the_same_fixed_message() {
    for (case, gateway_id, link_id, rule_id) in UNKNOWN_IDS {
        // A fresh store per row, so nothing an earlier call created can resolve one
        // of these ids.
        let fabric = Fabric::new();

        for (operation, method, path, body) in calls(gateway_id, link_id, rule_id) {
            let case = format!("{case}: {operation}");
            let (status, code, answered) = fabric.send(method, &path, body).await;

            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404, got {status} {answered:?}"
            );
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {code:?}"
            );
            let message = text(&answered, "message");
            assert_eq!(
                message, RESOURCE_NOT_FOUND_MESSAGE,
                "case {case:?}: expected {RESOURCE_NOT_FOUND_MESSAGE:?}, got {message:?}"
            );
            // Nothing the request carried is in the refusal: not an id, not an ARN.
            for value in [*gateway_id, *link_id, *rule_id, CERTIFICATE_ARN] {
                assert!(
                    !message.contains(value),
                    "case {case:?}: expected the refusal to name nothing from the request, got {message:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_resource_that_exists_is_not_answered_with_the_not_found_message() {
    // The counterpart of the rule: the fixed message is the answer to a resource the
    // store does not hold, so a stored one is still served.
    let fabric = Fabric::new();
    let (status, _, created) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(json!({
                "vpcId": "vpc-0123456789abcdef0",
                "subnetIds": ["subnet-0123456789abcdef0"],
                "securityGroupIds": ["sg-0123456789abcdef0"],
                "clientToken": "token-requester",
            })),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected CreateRequesterGateway to answer 200, got {status} {created:?}"
    );
    let gateway_id = text(&created, "gatewayId");

    let (status, _, read) = fabric
        .send("GET", &format!("/requester-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected the stored gateway to be readable, got {status} {read:?}"
    );
    assert_eq!(
        text(&read, "gatewayId"),
        gateway_id,
        "expected the stored gateway's id {gateway_id:?}, got {:?}",
        read["gatewayId"]
    );
}
