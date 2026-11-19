// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateGatewayTarget` body that defines no `credentialProviderConfigurations`
//! leaves the target with, and which bodies have to define one at all.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handler are all exercised against the one shared engine.
//!
//! RULE under test: `credentialProviderConfigurations` is OPTIONAL on
//! `UpdateGatewayTargetRequest` and `@required`, with `@length` exactly one entry, on
//! `UpdateGatewayTargetResponse`
//! (`smithy_query(query="operation",operation="UpdateGatewayTarget")`), so what a body that
//! defines none leaves the target with is decided by the `targetConfiguration` that body
//! carries:
//!
//! * an `mcp` configuration is one the GATEWAY ITSELF calls — it invokes the API the
//!   configuration describes (an OpenAPI schema, a Smithy model, a Lambda function, an MCP
//!   server, an API Gateway stage, a connector) — so it has to be told what to authenticate
//!   to that API as, and a body carrying one and defining no credential provider is the
//!   modelled `ValidationException` (HTTP 400) with the member's own sentence, decided from
//!   the request alone and so ahead of the target the path names;
//! * an `http` configuration "forwards requests directly to an external HTTP endpoint" or
//!   routes them to an AgentCore Runtime of this account, and an `inference` configuration
//!   "routes requests to a large language model (LLM) provider"
//!   (`smithy_query(query="shape",shape="HttpTargetConfiguration,InferenceTargetConfiguration")`),
//!   so neither has a credential provider of its own to be told about: the body is served
//!   (HTTP 202) and the target keeps the credential provider configuration it already
//!   carries, which is what the response's `@required` member reports.
//!
//! Because such a body is one this operation can render into its response, it goes on to
//! resolve the target the path addresses — so a `targetId` that names no target of the
//! gateway, or one whose deletion this service has already carried out, is the modelled
//! `ResourceNotFoundException` (HTTP 404) rather than an input refusal.
//!
//! The rows differ along what the rule generalises over: the union member the
//! `targetConfiguration` carries (four `mcp` shapes, two `http` shapes and an `inference`
//! one), whether the body defines the member, and whether the target the path names is
//! there, is not there at all, or has been deleted.

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

/// How long a poll loop keeps looking for a clock-owed transition before giving up.
/// Generous on purpose: the loop waits for the CONDITION, and the span this service gives
/// the gateway family is far shorter than this on any host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// A `targetId` inside `TargetId`'s pattern `^[0-9a-zA-Z]{10}$` that nothing here creates.
const ABSENT_TARGET_ID: &str = "Kk7788Mm55";

/// The sentence a body that had to define the credential provider configurations and did
/// not earns: entirely fixed text, naming the member in prose and quoting nothing of the
/// request.
const NOT_DEFINED: &str = "Credential provider configurations is not defined";

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

/// The credential provider configuration every target here is CREATED with, so a carried
/// forward one is distinguishable from the gateway-role default a create with none gets.
fn created_providers() -> Value {
    json!([{"credentialProviderType": "CALLER_IAM_CREDENTIALS"}])
}

/// A different one, so an update that DEFINES the member is distinguishable from an update
/// that carried the created one forward.
fn replacement_providers() -> Value {
    json!([{"credentialProviderType": "GATEWAY_IAM_ROLE"}])
}

/// The configuration every target here is created with: an MCP target serving an inline
/// OpenAPI schema.
fn created_configuration() -> Value {
    json!({"mcp": {"openApiSchema": {"inlinePayload": "{\"openapi\":\"3.0.0\"}"}}})
}

/// Read a resource again until it reports `READY`: the transition is the clock's, so the
/// loop looks for the CONDITION rather than waiting a fixed time, and a deadline that
/// passes without it is the setup failing rather than a later assertion going unexplained.
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
            "setup {uri:?}: expected the clock-owed transition to {READY}, last read \
             {status:?} ({read})"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Create a gateway and wait for the clock-owed `CREATING` -> `READY` transition, then
/// answer its `gatewayId`.
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

/// Create a target carrying [`created_providers`] on `gateway`, wait for it to settle, and
/// answer its `targetId`.
async fn settled_target(router: &MultiServiceRouter, gateway: &str, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        &format!("/gateways/{gateway}/targets/"),
        Some(&json!({
            "name": name,
            "description": "the target before any update",
            "targetConfiguration": created_configuration(),
            "credentialProviderConfigurations": created_providers(),
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {name:?}: CreateGatewayTarget must succeed, got {status} ({created})"
    );
    let target_id = text(&created, "targetId");
    settled(router, &format!("/gateways/{gateway}/targets/{target_id}/")).await;
    target_id
}

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

async fn read_target(router: &MultiServiceRouter, gateway: &str, target: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway}/targets/{target}/"),
        None,
    )
    .await
}

/// One row of the table: a `targetConfiguration` union member, and whether a body carrying
/// it has to define the credential provider configurations.
struct Row {
    case: &'static str,
    configuration: Value,
    must_define: bool,
}

/// Every union member of `TargetConfiguration` this rule is stated over, read off the model
/// (`smithy_query(query="shape",shape="TargetConfiguration,McpTargetConfiguration,HttpTargetConfiguration,InferenceTargetConfiguration")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an MCP target serving an inline OpenAPI schema",
            configuration: json!({"mcp": {"openApiSchema": {
                "inlinePayload": "{\"openapi\":\"3.1.0\"}",
            }}}),
            must_define: true,
        },
        Row {
            case: "an MCP target serving an inline Smithy model",
            configuration: json!({"mcp": {"smithyModel": {
                "inlinePayload": "{\"smithy\":\"2.0\"}",
            }}}),
            must_define: true,
        },
        Row {
            case: "an MCP target whose tools are a Lambda function",
            configuration: json!({"mcp": {"lambda": {
                "lambdaArn": "arn:aws:lambda:us-east-1:000000000000:function:carried",
                "toolSchema": {"inlinePayload": [{
                    "name": "echo",
                    "description": "echo a string back",
                    "inputSchema": {"type": "object"},
                }]},
            }}}),
            must_define: true,
        },
        Row {
            case: "an MCP server target",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://example.com/mcp-server",
            }}}),
            must_define: true,
        },
        Row {
            case: "an HTTP passthrough target",
            configuration: json!({"http": {"passthrough": {
                "endpoint": "https://example.com/mcp-v2",
                "protocolType": "MCP",
            }}}),
            must_define: false,
        },
        Row {
            case: "an HTTP target routing to an AgentCore Runtime",
            configuration: json!({"http": {"agentcoreRuntime": {
                "arn": "arn:aws:bedrock-agentcore:us-east-1:000000000000:runtime/\
                        carriedruntime-abc1234567",
            }}}),
            must_define: false,
        },
        Row {
            case: "an inference target routing to an LLM provider",
            configuration: json!({"inference": {"provider": {
                "endpoint": "https://example.com/v1/chat",
            }}}),
            must_define: false,
        },
    ]
}

/// RULE: a body that defines no `credentialProviderConfigurations` is refused with the
/// member's own sentence when its configuration is one the gateway itself has to
/// authenticate to, and is served — carrying the stored credential provider configuration
/// forward — when it is not.
#[tokio::test]
async fn an_omitted_member_is_carried_forward_or_refused_by_the_configuration() {
    let router = router();
    let gateway_id = settled_gateway(&router, "carriedcredsgate").await;

    for (index, row) in rows().into_iter().enumerate() {
        let target_id =
            settled_target(&router, &gateway_id, &format!("carriedtarget{index}")).await;
        let body = json!({"targetConfiguration": row.configuration.clone()});
        let (status, answered) = update_target(&router, &gateway_id, &target_id, &body).await;

        if row.must_define {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ValidationException"),
                "case {:?}: expected HTTP 400 ValidationException, got {status} {} ({answered})",
                row.case,
                error_code(&answered)
            );
            assert_eq!(
                text(&answered, "message"),
                NOT_DEFINED,
                "case {:?}: expected the fixed sentence {NOT_DEFINED:?}, got {}",
                row.case,
                text(&answered, "message")
            );
            // Nothing the refused body carried reached the stored target.
            let (status, fetched) = read_target(&router, &gateway_id, &target_id).await;
            assert_eq!(
                (status, &fetched["targetConfiguration"]),
                (200, &created_configuration()),
                "case {:?}: a refused update must leave the stored configuration alone, got \
                 {status} ({fetched})",
                row.case
            );
            continue;
        }

        assert_eq!(
            status, 202,
            "case {:?}: expected HTTP 202 from UpdateGatewayTarget, got {status} ({answered})",
            row.case
        );
        assert_eq!(
            answered["credentialProviderConfigurations"],
            created_providers(),
            "case {:?}: the update must report the credential provider the target already \
             carried, got {}",
            row.case,
            answered["credentialProviderConfigurations"]
        );
        // The accepted update is proved by a read, not by its own echo.
        let (status, fetched) = read_target(&router, &gateway_id, &target_id).await;
        assert_eq!(
            (status, &fetched["targetConfiguration"]),
            (200, &row.configuration),
            "case {:?}: the read must report the configuration the update stored, got {status} \
             ({fetched})",
            row.case
        );
        assert_eq!(
            fetched["credentialProviderConfigurations"],
            created_providers(),
            "case {:?}: the read must report the carried-forward credential provider, got {}",
            row.case,
            fetched["credentialProviderConfigurations"]
        );
    }
}

/// The other half of the same rule: a body that DOES define the member replaces the stored
/// credential provider configuration, whichever configuration it carries — so the rows
/// above are not passing because the member is ignored.
#[tokio::test]
async fn a_defined_member_replaces_the_stored_credential_providers() {
    let router = router();
    let gateway_id = settled_gateway(&router, "definedcredsgate").await;

    for (index, row) in rows().into_iter().enumerate() {
        // The Lambda shape is the one configuration this operation resolves against another
        // service, so a body that passes the member check is then refused for the function
        // it names; its member check is covered by the table above.
        if row.case.contains("Lambda") {
            continue;
        }
        let target_id =
            settled_target(&router, &gateway_id, &format!("definedtarget{index}")).await;
        let (status, answered) = update_target(
            &router,
            &gateway_id,
            &target_id,
            &json!({
                "targetConfiguration": row.configuration.clone(),
                "credentialProviderConfigurations": replacement_providers(),
            }),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {:?}: expected HTTP 202 from UpdateGatewayTarget, got {status} ({answered})",
            row.case
        );
        let (status, fetched) = read_target(&router, &gateway_id, &target_id).await;
        assert_eq!(
            (status, &fetched["credentialProviderConfigurations"]),
            (200, &replacement_providers()),
            "case {:?}: the read must report the credential provider the update defined, got \
             {status} ({fetched})",
            row.case
        );
    }
}

/// RULE: a body this operation could render into its response goes on to resolve the target
/// the path addresses, so a target that is gone — deleted, or never there — is the modelled
/// 404 and not an input refusal; a body that had to define the member is still refused 400
/// ahead of that resolution.
#[tokio::test]
async fn an_update_of_a_target_that_is_gone_is_not_found() {
    let router = router();
    let gateway_id = settled_gateway(&router, "gonetargetgate").await;
    let target_id = settled_target(&router, &gateway_id, "gonetarget").await;

    let (status, deleted) = send(
        &router,
        "DELETE",
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
        None,
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: DeleteGatewayTarget must be accepted, got {status} ({deleted})"
    );
    // The removal is the clock's, so the loop looks for the CONDITION: the target no longer
    // reads back at all.
    let started = Instant::now();
    loop {
        let (status, read) = read_target(&router, &gateway_id, &target_id).await;
        if status == 404 {
            break;
        }
        assert!(
            started.elapsed() <= POLL_DEADLINE,
            "setup: expected the accepted deletion to remove the target, last read {status} \
             ({read})"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }

    let passthrough = json!({"http": {"passthrough": {
        "endpoint": "https://example.com/mcp-v2",
        "protocolType": "MCP",
    }}});
    // (case, the targetId the path names, the body it sends, expected status, code, message)
    let probes: [(&str, &str, Value, u16, &str, &str); 4] = [
        (
            "the deleted target, and a body that needs no providers",
            target_id.as_str(),
            json!({"targetConfiguration": passthrough.clone()}),
            404,
            "ResourceNotFoundException",
            TARGET_ABSENT,
        ),
        (
            "the deleted target, and a body that defines them",
            target_id.as_str(),
            json!({
                "targetConfiguration": passthrough.clone(),
                "credentialProviderConfigurations": replacement_providers(),
            }),
            404,
            "ResourceNotFoundException",
            TARGET_ABSENT,
        ),
        (
            "a targetId that was never there, and a body that needs no providers",
            ABSENT_TARGET_ID,
            json!({"targetConfiguration": passthrough.clone()}),
            404,
            "ResourceNotFoundException",
            TARGET_ABSENT,
        ),
        (
            "the deleted target, and a body that had to define them",
            target_id.as_str(),
            json!({"targetConfiguration": created_configuration()}),
            400,
            "ValidationException",
            NOT_DEFINED,
        ),
    ];

    for (case, target, body, expected_status, expected_code, expected_message) in probes {
        let (status, refused) = update_target(&router, &gateway_id, target, &body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected HTTP {expected_status} {expected_code}, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            expected_message,
            "case {case:?}: expected the sentence {expected_message:?}, got {}",
            text(&refused, "message")
        );
    }

    // Nothing a refused update minted: the deleted target is still gone.
    let (status, fetched) = read_target(&router, &gateway_id, &target_id).await;
    assert_eq!(
        status, 404,
        "a refused update must not re-create the deleted target, got {status} ({fetched})"
    );
}
