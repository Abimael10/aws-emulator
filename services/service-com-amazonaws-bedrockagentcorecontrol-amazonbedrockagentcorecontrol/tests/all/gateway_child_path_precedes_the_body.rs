// SPDX-License-Identifier: CC0-1.0

//! Where a gateway child operation decides the resource its PATH names, relative to the body
//! members this service judges for itself.
//!
//! RULE under test: every operation addressed by `{gatewayIdentifier}` resolves that gateway
//! BEFORE it judges a body member the model leaves optional, so a `gatewayIdentifier` naming
//! no gateway is the modelled `ResourceNotFoundException` (HTTP 404) whatever the body
//! carries. The members refused ahead of the lookup are the ones the MODEL constrains —
//! `@required`, a length, a pattern, a range — which the protocol boundary enforces; a member
//! the model leaves optional is this service's own rule about the body and is judged once the
//! path's resources are known to be there, as the modelled `ValidationException` (HTTP 400).
//!
//! `smithy_query(query="operation",operation="CreateGatewayTarget,UpdateGatewayTarget,CreateGatewayRule,ListGatewayTargets,GetGatewayTarget")`:
//! `gatewayIdentifier` is `@required` and `@httpLabel` on every one of them, `name` is
//! optional on both gateway-target writes, and `credentialProviderConfigurations` is optional
//! on `UpdateGatewayTarget` — each `@required` only on the RESPONSE.
//!
//! The rows differ along what the rule generalises over: the OPERATION (two writes of a
//! target, a write of a rule, a read of one target, a listing of them), the METHOD (POST, PUT,
//! GET), and — for the two writes — whether the body carries the optional member this service
//! judges or leaves it out. The control at the end drives the same two bodies at a gateway
//! that IS there, so the 404 rows cannot pass by refusing everything.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// A `gatewayIdentifier` inside `GatewayIdentifier`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that nothing here ever creates.
const ABSENT_GATEWAY_ID: &str = "no-such-gateway-9876543210";

/// A `targetId` inside `TargetId`'s pattern `^[0-9a-zA-Z]{10}$` that nothing here ever
/// creates.
const ABSENT_TARGET_ID: &str = "YYYY888888";

/// The terminal status the clock owes a freshly created gateway
/// (`smithy_query(query="shape",shape="GatewayStatus")`).
const READY: &str = "READY";

/// How long a poll loop keeps looking for the clock-owed transition before giving up.
/// Generous on purpose: the loop waits for the CONDITION, and the span this service gives the
/// gateway family is far shorter than this on any host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The sentence this service answers a `gatewayIdentifier` naming no gateway with.
const GATEWAY_ABSENT: &str = "Failed to retrieve gateway because it doesn't exist. Retry the \
                              request with a different resource identifier.";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The error code `restJson1` puts in the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A target configuration union: an MCP target serving an inline OpenAPI schema.
fn openapi_configuration() -> Value {
    json!({"mcp": {"openApiSchema": {"inlinePayload": "{\"openapi\":\"3.1.0\"}"}}})
}

/// The one credential provider every body below that carries the member names.
fn providers() -> Value {
    json!([{"credentialProviderType": "CALLER_IAM_CREDENTIALS"}])
}

/// Create a gateway and read it again until the clock-owed `CREATING` -> `READY` transition
/// has happened, then answer its `gatewayId`.
async fn settled_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        "/gateways/",
        Some(&json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {name:?}: CreateGateway must succeed, got {status} ({created})"
    );
    let gateway_id = text(&created, "gatewayId");
    let started = Instant::now();
    loop {
        let (_, read) = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&read, "status") == READY || started.elapsed() > POLL_DEADLINE {
            return gateway_id;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One row: what it is a case of, the request it sends, and the body it carries.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// RULE: a `gatewayIdentifier` naming no gateway is the modelled 404 on every operation
/// addressed by one, whether or not the body carries the optional member this service judges.
#[tokio::test]
async fn an_absent_gateway_is_reported_absent_whatever_the_body_carries() {
    let router = router();

    let rows = [
        Row {
            case: "CreateGatewayTarget, with the name the response needs",
            method: "POST",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/"),
            body: Some(json!({
                "name": "pathfirsttarget",
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            })),
        },
        Row {
            case: "CreateGatewayTarget, with no name at all",
            method: "POST",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/"),
            body: Some(json!({
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            })),
        },
        Row {
            case: "UpdateGatewayTarget, with the credential providers the response needs",
            method: "PUT",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/{ABSENT_TARGET_ID}/"),
            body: Some(json!({
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            })),
        },
        Row {
            case: "UpdateGatewayTarget, with no credential providers at all",
            method: "PUT",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/{ABSENT_TARGET_ID}/"),
            body: Some(json!({"targetConfiguration": openapi_configuration()})),
        },
        Row {
            case: "CreateGatewayRule, another child write of the same gateway",
            method: "POST",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/rules"),
            body: Some(json!({
                "priority": 10,
                "actions": [{"routeToTarget": {"staticRoute": {"targetName": "orders"}}}],
            })),
        },
        Row {
            case: "GetGatewayTarget, a read of one child",
            method: "GET",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/{ABSENT_TARGET_ID}/"),
            body: None,
        },
        Row {
            case: "ListGatewayTargets, a read of all of them",
            method: "GET",
            uri: format!("/gateways/{ABSENT_GATEWAY_ID}/targets/"),
            body: None,
        },
    ];

    for row in &rows {
        let (status, answered) = send(&router, row.method, &row.uri, row.body.as_ref()).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (404, "ResourceNotFoundException"),
            "case {:?}: expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({answered})",
            row.case,
            error_code(&answered)
        );
        assert_eq!(
            text(&answered, "message"),
            GATEWAY_ABSENT,
            "case {:?}: expected the absent-gateway sentence, got {}",
            row.case,
            text(&answered, "message")
        );
    }
}

/// The other half of the rule, so the rows above cannot pass by refusing everything: on a
/// gateway that IS there, the very bodies that left the optional member out are refused 400
/// with this service's own sentence for the member, and the body that carries it is served.
#[tokio::test]
async fn the_body_is_judged_once_the_gateway_is_there() {
    let router = router();
    let gateway_id = settled_gateway(&router, "pathfirstgate").await;

    // A create with no name: the gateway resolves, so now the body's own refusal stands.
    let (status, refused) = send(
        &router,
        "POST",
        &format!("/gateways/{gateway_id}/targets/"),
        Some(&json!({
            "targetConfiguration": openapi_configuration(),
            "credentialProviderConfigurations": providers(),
        })),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str(), text(&refused, "message").as_str()),
        (400, "ValidationException", "name: is required"),
        "case a create with no name on a gateway that is there: expected the modelled 400, \
         got {status} ({refused})"
    );

    // The same create WITH a name is served, and the created target reads back.
    let (status, created) = send(
        &router,
        "POST",
        &format!("/gateways/{gateway_id}/targets/"),
        Some(&json!({
            "name": "pathfirsttarget",
            "targetConfiguration": openapi_configuration(),
            "credentialProviderConfigurations": providers(),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case a create carrying the name: expected HTTP 202, got {status} ({created})"
    );
    let target_id = text(&created, "targetId");
    let (status, fetched) = send(
        &router,
        "GET",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        None,
    )
    .await;
    assert_eq!(
        (status, text(&fetched, "name").as_str()),
        (200, "pathfirsttarget"),
        "case the created target reads back: got {status} ({fetched})"
    );

    // An update of that target with no credential providers: the path resolves, so the
    // body's own refusal stands here too.
    let (status, refused) = send(
        &router,
        "PUT",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        Some(&json!({"targetConfiguration": openapi_configuration()})),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ValidationException"),
        "case an update with no credential providers on a target that is there: expected the \
         modelled 400, got {status} ({refused})"
    );
    assert_eq!(
        text(&refused, "message"),
        "Credential provider configurations is not defined",
        "case an update with no credential providers: expected the member's own sentence, got {}",
        text(&refused, "message")
    );
}
