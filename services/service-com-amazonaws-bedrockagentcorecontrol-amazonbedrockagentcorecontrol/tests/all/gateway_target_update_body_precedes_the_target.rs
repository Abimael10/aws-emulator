// SPDX-License-Identifier: CC0-1.0

//! Where `UpdateGatewayTarget` decides the body it was given, relative to the child the URI
//! path names under the gateway.
//!
//! RULE under test: the GATEWAY the path label names is resolved first, so a
//! `gatewayIdentifier` naming no gateway is the modelled `ResourceNotFoundException` (HTTP
//! 404) whatever the body carries; a refusal this operation decides from the request body
//! ALONE — a member its own response requires that the body leaves undefined — is then
//! reported before the target the path names under that gateway is resolved, as the modelled
//! `ValidationException` (HTTP 400) with the member's own sentence; and the target's own 404,
//! like the gateway's status conflict, is reached only by a body this operation could
//! otherwise render into its response.
//!
//! The member in question, read off the model
//! (`smithy_query(query="operation",operation="UpdateGatewayTarget")`):
//! `credentialProviderConfigurations` is optional on `UpdateGatewayTargetRequest` and
//! `@required`, with `@length` exactly one entry, on `UpdateGatewayTargetResponse`, and the
//! update replaces the target's whole configuration — so a request that defines none leaves
//! the modelled response nothing to report. The operation models both
//! `ValidationException` (HTTP 400) and `ResourceNotFoundException` (HTTP 404).
//!
//! The rows differ along what the rule generalises over: whether the target the path names is
//! there, is not there at all, or has had its deletion accepted; the gateway's own status
//! (settled, or still owed a clock transition); the `targetConfiguration` variant the body
//! carries (an inline OpenAPI schema, an HTTP passthrough endpoint, a Lambda naming a function
//! no mounted service resolves); and whether the body defines the member. The served control
//! at the end drives the same paths with a body that DOES define it, so the refusal rows
//! cannot pass by refusing everything.

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

/// The terminal status the clock owes a freshly created gateway or target
/// (`smithy_query(query="shape",shape="GatewayStatus,TargetStatus")`).
const READY: &str = "READY";

/// How long a poll loop keeps looking for a clock-owed transition before giving up. Generous
/// on purpose: the loop waits for the CONDITION, and the span this service gives the gateway
/// family is far shorter than this on any host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// A `gatewayIdentifier` inside `GatewayIdentifier`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that nothing here ever creates.
const ABSENT_GATEWAY_ID: &str = "no-such-gateway-1357924680";

/// A `targetId` inside `TargetId`'s pattern `^[0-9a-zA-Z]{10}$` that nothing here ever
/// creates.
const ABSENT_TARGET_ID: &str = "XxYy123456";

/// A second one, so a row can name an absent target that is not the one another row named.
const OTHER_ABSENT_TARGET_ID: &str = "Qq0011Zz99";

/// The sentence this service answers a body that defined no credential provider
/// configurations with: entirely fixed text, naming the member in prose and quoting nothing of
/// the request.
const NOT_DEFINED: &str = "Credential provider configurations is not defined";

/// The sentence a `gatewayIdentifier` naming no gateway earns.
const GATEWAY_ABSENT: &str = "Failed to retrieve gateway because it doesn't exist. Retry the \
                              request with a different resource identifier.";

/// The sentence a `targetId` naming no target of a gateway that IS there earns.
const TARGET_ABSENT: &str = "Failed to retrieve target because it doesn't exist. Retry the \
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

/// A target configuration union: an HTTP passthrough endpoint.
fn passthrough_configuration() -> Value {
    json!({"http": {"passthrough": {"endpoint": "https://example.com/mcp", "protocolType": "MCP"}}})
}

/// A target configuration union: a Lambda MCP target naming a function no mounted service
/// resolves, so a request that reaches the cross-service check is refused there instead.
fn absent_lambda_configuration() -> Value {
    json!({"mcp": {"lambda": {
        "lambdaArn": "arn:aws:lambda:us-east-1:000000000000:function:nothing-here",
        "toolSchema": {"inlinePayload": [{
            "name": "echo",
            "description": "echo a string back",
            "inputSchema": {"type": "object"},
        }]},
    }}})
}

/// The one credential provider a body that DOES define the member names.
fn providers() -> Value {
    json!([{"credentialProviderType": "CALLER_IAM_CREDENTIALS"}])
}

/// Read a resource again until it reports `READY`: the transition is the clock's, so the loop
/// looks for the CONDITION rather than waiting a fixed time, and a deadline that passes
/// without it is the setup failing rather than a later assertion going unexplained.
async fn settled(router: &MultiServiceRouter, uri: &str) {
    let started = Instant::now();
    loop {
        let (_, read) = send(router, "GET", uri, None).await;
        let status = text(&read, "status");
        if status == READY {
            return;
        }
        assert!(
            started.elapsed() <= POLL_DEADLINE,
            "setup {uri:?}: expected the clock-owed transition to {READY}, last read {status:?} \
             ({read})"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Create a gateway and wait for the clock-owed `CREATING` -> `READY` transition, then answer
/// its `gatewayId`.
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
    settled(router, &format!("/gateways/{gateway_id}/")).await;
    gateway_id
}

/// Create a target on `gateway`, wait for it to settle, and answer its `targetId`.
async fn settled_target(router: &MultiServiceRouter, gateway: &str, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        &format!("/gateways/{gateway}/targets/"),
        Some(&json!({
            "name": name,
            "description": "the target before any update",
            "targetConfiguration": openapi_configuration(),
            "credentialProviderConfigurations": providers(),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {name:?}: CreateGatewayTarget must succeed, got {status} ({created})"
    );
    let target_id = text(&created, "targetId");
    settled(
        router,
        &format!("/gateways/{gateway}/targets/{target_id}/"),
    )
    .await;
    target_id
}

/// `UpdateGatewayTarget` for one row.
async fn update_target(
    router: &MultiServiceRouter,
    gateway: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    send(
        router,
        "PUT",
        &format!("/gateways/{gateway}/targets/{target}/"),
        Some(body),
    )
    .await
}

/// One row of the table: what it is a case of, the path it addresses, the body it sends, and
/// the refusal it earns.
struct Row {
    case: &'static str,
    gateway: String,
    target: String,
    body: Value,
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// RULE: a body that defines no `credentialProviderConfigurations` is the modelled 400 with
/// the member's own sentence before the target the path names is resolved, while the gateway
/// the path label names is resolved ahead of the body.
#[tokio::test]
async fn the_undefined_member_is_reported_before_the_target_is_resolved() {
    let router = router();
    let gateway_id = settled_gateway(&router, "bodyfirstgate").await;
    let target_id = settled_target(&router, &gateway_id, "bodyfirsttarget").await;

    // A target whose deletion this service has already accepted: it is on its way out, so a
    // body-valid update of it is the same 404 an id naming nothing earns.
    let deleting_target = settled_target(&router, &gateway_id, "deletingtarget").await;
    let (status, deleted) = send(
        &router,
        "DELETE",
        &format!("/gateways/{gateway_id}/targets/{deleting_target}/"),
        None,
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: DeleteGatewayTarget must be accepted, got {status} ({deleted})"
    );

    // A second gateway left in the transient status the clock owes a transition for, so a row
    // can prove the body's refusal precedes the gateway's own status conflict too.
    let unsettled_gateway = settled_gateway(&router, "bodyfirstunsettled").await;
    let unsettled_target = settled_target(&router, &unsettled_gateway, "unsettledtarget").await;
    let (status, moved) = send(
        &router,
        "PUT",
        &format!("/gateways/{unsettled_gateway}/"),
        Some(&json!({
            "name": "bodyfirstunsettled",
            "roleArn": ROLE_ARN,
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        (status, text(&moved, "status").as_str()),
        (202, "UPDATING"),
        "setup: the second gateway must be left in UPDATING, got {status} ({moved})"
    );

    let rows = [
        Row {
            case: "a targetId naming no target, and a body that defines no providers",
            gateway: gateway_id.clone(),
            target: ABSENT_TARGET_ID.to_string(),
            body: json!({"targetConfiguration": openapi_configuration()}),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        Row {
            case: "another absent targetId, an HTTP passthrough body, and no providers",
            gateway: gateway_id.clone(),
            target: OTHER_ABSENT_TARGET_ID.to_string(),
            body: json!({
                "name": "renamedtarget",
                "description": "a description the refusal must not store",
                "targetConfiguration": passthrough_configuration(),
            }),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        Row {
            case: "an absent targetId, a Lambda no service resolves, and no providers",
            gateway: gateway_id.clone(),
            target: ABSENT_TARGET_ID.to_string(),
            body: json!({"targetConfiguration": absent_lambda_configuration()}),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        Row {
            case: "a target whose deletion was accepted, and a body with no providers",
            gateway: gateway_id.clone(),
            target: deleting_target.clone(),
            body: json!({"targetConfiguration": openapi_configuration()}),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        Row {
            case: "a target that is there, and a body with no providers",
            gateway: gateway_id.clone(),
            target: target_id.clone(),
            body: json!({"targetConfiguration": openapi_configuration()}),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        Row {
            case: "a gateway the clock still owes a transition, and a body with no providers",
            gateway: unsettled_gateway.clone(),
            target: unsettled_target.clone(),
            body: json!({"targetConfiguration": openapi_configuration()}),
            status: 400,
            code: "ValidationException",
            message: NOT_DEFINED,
        },
        // The gateway the path label names is resolved AHEAD of the body, so these two are
        // the gateway's own 404 whatever the body left out.
        Row {
            case: "a gatewayIdentifier naming no gateway, and a body with no providers",
            gateway: ABSENT_GATEWAY_ID.to_string(),
            target: target_id.clone(),
            body: json!({"targetConfiguration": openapi_configuration()}),
            status: 404,
            code: "ResourceNotFoundException",
            message: GATEWAY_ABSENT,
        },
        Row {
            case: "neither the gateway nor the target is there, and no providers either",
            gateway: ABSENT_GATEWAY_ID.to_string(),
            target: ABSENT_TARGET_ID.to_string(),
            body: json!({"targetConfiguration": passthrough_configuration()}),
            status: 404,
            code: "ResourceNotFoundException",
            message: GATEWAY_ABSENT,
        },
        // A body that DOES define the member passes the body check, so these reach the
        // target's own resolution and the refusals that follow it.
        Row {
            case: "an absent targetId, and a body that defines the providers",
            gateway: gateway_id.clone(),
            target: ABSENT_TARGET_ID.to_string(),
            body: json!({
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: TARGET_ABSENT,
        },
        Row {
            case: "a target whose deletion was accepted, and a body that defines them",
            gateway: gateway_id.clone(),
            target: deleting_target.clone(),
            body: json!({
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: TARGET_ABSENT,
        },
        Row {
            case: "a gatewayIdentifier naming no gateway, and a body that defines them",
            gateway: ABSENT_GATEWAY_ID.to_string(),
            target: target_id.clone(),
            body: json!({
                "targetConfiguration": openapi_configuration(),
                "credentialProviderConfigurations": providers(),
            }),
            status: 404,
            code: "ResourceNotFoundException",
            message: GATEWAY_ABSENT,
        },
    ];

    for row in &rows {
        let (status, refused) = update_target(&router, &row.gateway, &row.target, &row.body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (row.status, row.code),
            "case {:?}: expected HTTP {} {}, got {status} {} ({refused})",
            row.case,
            row.status,
            row.code,
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            row.message,
            "case {:?}: expected the sentence {:?}, got {}",
            row.case,
            row.message,
            text(&refused, "message")
        );
    }

    // Nothing a refused row asked for reached the stored target.
    let (status, fetched) = send(
        &router,
        "GET",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the target must still read back, got {status} ({fetched})"
    );
    assert_eq!(
        text(&fetched, "name"),
        "bodyfirsttarget",
        "a refused update must not rename the target, got {fetched}"
    );
    assert_eq!(
        text(&fetched, "description"),
        "the target before any update",
        "a refused update must not change the description, got {fetched}"
    );
    assert_eq!(
        fetched["targetConfiguration"],
        openapi_configuration(),
        "a refused update must leave the stored configuration alone, got {}",
        fetched["targetConfiguration"]
    );

    // The served control: the same path, with a body that defines the member, is accepted and
    // the change reads back — so the rows above are not passing because everything is refused.
    let (status, updated) = update_target(
        &router,
        &gateway_id,
        &target_id,
        &json!({
            "targetConfiguration": passthrough_configuration(),
            "credentialProviderConfigurations": providers(),
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "the control update must be accepted, got {status} ({updated})"
    );
    let (status, fetched) = send(
        &router,
        "GET",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        None,
    )
    .await;
    assert_eq!(
        (status, &fetched["targetConfiguration"]),
        (200, &passthrough_configuration()),
        "the accepted update must read back, got {status} ({fetched})"
    );
    assert_eq!(
        fetched["credentialProviderConfigurations"],
        providers(),
        "the accepted update must store the providers it defined, got {}",
        fetched["credentialProviderConfigurations"]
    );
}
