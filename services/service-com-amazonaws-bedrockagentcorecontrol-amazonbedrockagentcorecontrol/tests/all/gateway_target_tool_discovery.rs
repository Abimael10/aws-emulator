// SPDX-License-Identifier: CC0-1.0

//! The tool-definition fetch a gateway target's status is decided by, end to end over
//! HTTP, every row read back through `GetGatewayTarget` and `ListGatewayTargets`.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="CreateGatewayTarget,UpdateGatewayTarget,\
//! SynchronizeGatewayTargets")`,
//! `smithy_query(query="shape",shape="McpServerTargetConfiguration,TargetStatus,\
//! StatusReasons")`):
//!
//! A gateway target whose tool definitions are DISCOVERED from the endpoint it names — an
//! MCP server target with no static `mcpToolSchema`, the one `TargetConfiguration` variant
//! that carries no schema of its own — has that fetch attempted against the endpoint by
//! every call that writes what the target serves (the create, the update and the
//! synchronization), and settles into what the fetch achieved: `READY` with no
//! `statusReasons` when the endpoint completes the MCP `initialize` handshake, and `FAILED`
//! carrying a `statusReasons` entry naming the failed dial when it does not. A target whose
//! tools are DECLARED in its own configuration — a static tool schema, an OpenAPI schema, a
//! Smithy model, a Lambda tool schema — has nothing to fetch and settles `READY`.
//!
//! The endpoints the rows span are real ones: this emulator serves the Model Context
//! Protocol on the `gatewayUrl` a `READY` gateway publishes, so a target pointed at such an
//! endpoint is one whose handshake completes, and an address nothing here serves is one that
//! answers nothing.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// A function ARN inside the Lambda ARN grammar. No Lambda is mounted here, so nothing
/// resolves it — which is not what this suite is about: a Lambda target declares its tools
/// in its own configuration and has no endpoint to fetch from.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-east-1:000000000000:function:tools";

/// The settled status of the gateway this suite's targets are created on is waited for by
/// the crate's one poll ([`crate::gateway_target_settling`]), which is also what every
/// row's target is read with.
use crate::gateway_target_settling::{POLL_DEADLINE, POLL_PAUSE};

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

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
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
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A member as the body carries it, `Value::Null` when it is absent.
fn member(body: &Value, name: &str) -> Value {
    body.get(name).cloned().unwrap_or(Value::Null)
}

/// The `statusReasons` a body reports, as the strings they are.
fn status_reasons(body: &Value) -> Vec<String> {
    body.get("statusReasons")
        .and_then(Value::as_array)
        .map(|reasons| {
            reasons
                .iter()
                .map(|reason| reason.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The `TargetStatus` values a target settles into, and the poll that waits for one, are
/// the crate's one reading of a target's clock-owed transition
/// (`tests/all/gateway_target_settling.rs`).
use crate::gateway_target_settling::settled_target as settled_target_answer;

/// Read the target until the clock has made its transition out of the transient status
/// due, as this suite's own answer type.
async fn settled_target(router: &MultiServiceRouter, gateway: &str, target: &str) -> Answer {
    let (status, body) = settled_target_answer(router, gateway, target).await;
    Answer { status, body }
}

async fn create_target(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("/gateways/{gateway}/targets/"),
        Some(body),
    )
    .await
}

/// Create a gateway with `authorizer_type` and hand back its `gatewayId` and the
/// `gatewayUrl` it publishes, waited out to `READY` — the status at which the gateway
/// serves both its target operations and its own MCP endpoint.
async fn settled_gateway(
    router: &MultiServiceRouter,
    name: &str,
    authorizer_type: &str,
) -> (String, String) {
    let body = json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "authorizerType": authorizer_type,
    });
    let created = send(router, "POST", "/gateways/", Some(&body)).await;
    assert_eq!(
        created.status, 202,
        "case {name:?}: expected HTTP 202 from CreateGateway, got {} ({})",
        created.status, created.body
    );
    let gateway_id = text(&created.body, "gatewayId");
    // The transition is the clock's, so the wait looks for the CONDITION rather than for a
    // fixed time.
    let started = Instant::now();
    let settled = loop {
        let read = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&read.body, "status") == "READY" || started.elapsed() > POLL_DEADLINE {
            break read;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        (settled.status, text(&settled.body, "status").as_str()),
        (200, "READY"),
        "case {name:?}: the gateway must settle to READY before it serves anything, got {} \
         {} ({})",
        settled.status,
        text(&settled.body, "status"),
        settled.body
    );
    let url = text(&settled.body, "gatewayUrl");
    assert!(
        url.starts_with("https://"),
        "case {name:?}: the gateway publishes an https endpoint, got {url:?}"
    );
    (gateway_id, url)
}

/// A `CreateGatewayTarget` body for `configuration`. Every target carries the gateway's own
/// IAM role as its credential provider (`CredentialProviderType`'s `GATEWAY_IAM_ROLE`),
/// which the fetch does not vary along.
fn create_body(name: &str, configuration: &Value) -> Value {
    json!({
        "name": name,
        "targetConfiguration": configuration,
        "credentialProviderConfigurations": [{"credentialProviderType": "GATEWAY_IAM_ROLE"}],
    })
}

/// The reason a target reports when NOTHING in this emulator answered the dial of its
/// endpoint, and the one it reports when something answered without completing the MCP
/// session: the fixed sentences, with the endpoint the target names and the status that
/// answer carried filled in from the request and from what the dial observed.
fn no_response_reason(endpoint: &str) -> String {
    format!(
        "The MCP server at {endpoint} did not respond to the tool discovery request. Verify \
         that the endpoint is reachable from the gateway and serves the Model Context \
         Protocol, then synchronize the target again."
    )
}

fn unfinished_handshake_reason(endpoint: &str, status: u16) -> String {
    format!(
        "The MCP server at {endpoint} did not complete the MCP initialize handshake: the tool \
         discovery request was answered with HTTP {status}. Verify that the endpoint serves \
         the Model Context Protocol, then synchronize the target again."
    )
}

/// What a row expects the target to settle into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settles {
    /// The target's tools were read, or it declares them itself: `READY`, no reason.
    Ready,
    /// The fetch reached nothing at that address.
    Unanswered,
    /// Something answered the dial with this HTTP status without completing the MCP
    /// session.
    Unfinished(u16),
}

impl Settles {
    fn status(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::Unanswered | Self::Unfinished(_) => "FAILED",
        }
    }

    /// The `statusReasons` the settled status reports for `endpoint` — empty for a status
    /// that has no reason.
    fn reasons(self, endpoint: &str) -> Vec<String> {
        match self {
            Self::Ready => Vec::new(),
            Self::Unanswered => vec![no_response_reason(endpoint)],
            Self::Unfinished(status) => vec![unfinished_handshake_reason(endpoint, status)],
        }
    }
}

/// One row of the table: a target configuration, and what its fetch settles the target
/// into.
struct Row {
    case: &'static str,
    /// Inside `TargetName`'s pattern `^([0-9a-zA-Z][-]?){1,100}$`.
    target_name: &'static str,
    configuration: Value,
    settles: Settles,
}

/// The table spans what the rule generalises over: whether the configuration's tools are
/// discovered or declared, and — for the one shape that discovers them — what the endpoint
/// it names IS. `served` is the `gatewayUrl` of a gateway this emulator serves the MCP
/// protocol on and authorizes nothing for, and `guarded` the one of a gateway whose
/// authorizer refuses a dial that carries no credential.
fn rows(served: &str, guarded: &str) -> Vec<Row> {
    vec![
        Row {
            case: "an MCP server endpoint this emulator serves the protocol on",
            target_name: "servedmcpserver",
            configuration: json!({"mcp": {"mcpServer": {"endpoint": served}}}),
            settles: Settles::Ready,
        },
        Row {
            case: "the same endpoint with the transport path joined on again, listing \
                   its tools dynamically",
            target_name: "joinedmcpserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": format!("{served}/mcp"),
                "listingMode": "DYNAMIC",
                "resourcePriority": 7,
            }}}),
            settles: Settles::Ready,
        },
        Row {
            case: "an MCP server endpoint nothing in this emulator serves",
            target_name: "unservedmcpserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://tools.example.com/mcp"
            }}}),
            settles: Settles::Unanswered,
        },
        Row {
            case: "an endpoint on a host this emulator serves, at a path it publishes \
                   nothing at",
            target_name: "wrongpathserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": format!("{}/tools", host_url(served))
            }}}),
            // The gateway endpoint publishes its MCP surface and nothing else, so this
            // path is answered with the service's own 404.
            settles: Settles::Unfinished(404),
        },
        Row {
            case: "an endpoint whose server refuses a dial that carries no credential",
            target_name: "guardedmcpserver",
            configuration: json!({"mcp": {"mcpServer": {"endpoint": guarded}}}),
            // That gateway authorizes inbound requests with AWS_IAM, and the fetch of a
            // caller-named endpoint carries no credential, so the dial is refused 403.
            settles: Settles::Unfinished(403),
        },
        Row {
            case: "an MCP server pinned to an inline static tool schema, at an endpoint \
                   nothing serves",
            target_name: "inlineschemaserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://pinned.example.com/mcp",
                "mcpToolSchema": {"inlinePayload": "{\"tools\":[]}"},
            }}}),
            settles: Settles::Ready,
        },
        Row {
            case: "an MCP server pinned to a static tool schema in S3",
            target_name: "s3schemaserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://pinned-s3.example.com/mcp",
                "mcpToolSchema": {"s3": {"uri": "s3://schemas-bucket/tools.json"}},
            }}}),
            settles: Settles::Ready,
        },
        Row {
            case: "an MCP target serving an inline OpenAPI schema",
            target_name: "openapitarget",
            configuration: json!({"mcp": {"openApiSchema": {
                "inlinePayload": "{\"openapi\":\"3.0.0\"}"
            }}}),
            settles: Settles::Ready,
        },
        Row {
            case: "an MCP target whose tools a Lambda function handles",
            target_name: "lambdatarget",
            configuration: json!({"mcp": {"lambda": {
                "lambdaArn": LAMBDA_ARN,
                "toolSchema": {"inlinePayload": [{
                    "name": "echo",
                    "description": "echo the input back",
                    "inputSchema": {"type": "object"},
                }]},
            }}}),
            settles: Settles::Ready,
        },
        Row {
            case: "an HTTP passthrough endpoint nothing in this emulator serves",
            target_name: "passthroughtarget",
            configuration: json!({"http": {"passthrough": {
                "endpoint": "https://api.example.com/v1",
                "protocolType": "MCP",
            }}}),
            settles: Settles::Ready,
        },
    ]
}

/// The `scheme://host` half of a URL, with its path dropped.
fn host_url(url: &str) -> String {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let host = rest.split('/').next().unwrap_or(rest);
    format!("https://{host}")
}

/// Assert the settled answer is the status and the reason `settles` calls for, for the
/// endpoint the target itself names — read off the same body, so the reason is checked
/// against the target's own configuration rather than against a value written here.
fn assert_settled(case: &str, settles: Settles, settled: &Answer) {
    assert_eq!(
        (settled.status, text(&settled.body, "status").as_str()),
        (200, settles.status()),
        "case {case:?}: expected the target to settle {}, got {} {} ({})",
        settles.status(),
        settled.status,
        text(&settled.body, "status"),
        settled.body
    );
    let endpoint = settled
        .body
        .pointer("/targetConfiguration/mcp/mcpServer/endpoint")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let actual = status_reasons(&settled.body);
    let expected = settles.reasons(endpoint);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected statusReasons {expected:?}, got {actual:?}"
    );
    for reason in &actual {
        // `StatusReason` is at most 2048 characters
        // (`smithy_query(query="shape",shape="StatusReason")`).
        assert!(
            reason.chars().count() <= 2048,
            "case {case:?}: a StatusReason is at most 2048 characters, got {}",
            reason.chars().count()
        );
    }
}

/// The rule over the whole table, for the call that FIRES the first fetch: `CreateGatewayTarget`.
#[tokio::test]
async fn a_created_target_settles_into_what_its_tool_definition_fetch_achieved() {
    let router = router();
    let (gateway, _) = settled_gateway(&router, "fetchrowsgateway", "AWS_IAM").await;
    let (_, served) = settled_gateway(&router, "servedmcpgateway", "NONE").await;
    let (_, guarded) = settled_gateway(&router, "guardedmcpgateway", "AWS_IAM").await;

    let mut created = Vec::new();
    for row in rows(&served, &guarded) {
        let answer =
            create_target(&router, &gateway, &create_body(row.target_name, &row.configuration))
                .await;
        assert_eq!(
            answer.status, 202,
            "case {:?}: expected HTTP 202 from CreateGatewayTarget, got {} ({})",
            row.case, answer.status, answer.body
        );
        // The fetch's outcome is owed by the clock, not by the call: a caller acting
        // straight after the create still sees the target being created, with no reason
        // to report yet.
        assert_eq!(
            (
                text(&answer.body, "status"),
                member(&answer.body, "statusReasons")
            ),
            ("CREATING".to_string(), Value::Null),
            "case {:?}: the create reports the target still being created, got {}",
            row.case,
            answer.body
        );
        created.push((row, text(&answer.body, "targetId")));
    }

    for (row, target_id) in &created {
        let settled = settled_target(&router, &gateway, target_id).await;
        assert_settled(row.case, row.settles, &settled);
        // A listing cannot drift from a read.
        let listing = send(&router, "GET", &format!("/gateways/{gateway}/targets/"), None).await;
        let listed = listing
            .body
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| text(item, "targetId") == *target_id)
                    .cloned()
            })
            .unwrap_or(Value::Null);
        assert_eq!(
            text(&listed, "status"),
            row.settles.status(),
            "case {:?}: the listing row reports the status the read reports, got {listed}",
            row.case
        );
    }
}

/// The same rule for the two other calls that write what a target serves: the fetch is
/// attempted again, so a target moves between the two outcomes as its endpoint changes —
/// which is what makes the status mean "this target's tools were read" rather than "a call
/// was accepted".
#[tokio::test]
async fn every_call_that_writes_what_a_target_serves_fetches_again() {
    let router = router();
    let (gateway, _) = settled_gateway(&router, "refetchgateway", "AWS_IAM").await;
    let (_, served) = settled_gateway(&router, "refetchmcpgateway", "NONE").await;

    // Created against an address nothing serves: the first fetch reaches nothing.
    let unserved = json!({"mcp": {"mcpServer": {"endpoint": "https://tools.example.com/mcp"}}});
    let created = create_target(
        &router,
        &gateway,
        &create_body("refetchtarget", &unserved),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "expected HTTP 202 from CreateGatewayTarget, got {} ({})",
        created.status, created.body
    );
    let target_id = text(&created.body, "targetId");
    assert_settled(
        "created against an address nothing serves",
        Settles::Unanswered,
        &settled_target(&router, &gateway, &target_id).await,
    );

    // Reconfigured onto an endpoint this emulator serves the protocol on: the update
    // fetches again, and the target settles into what THAT fetch achieved.
    let reconfigured = json!({"mcp": {"mcpServer": {"endpoint": served}}});
    let updated = send(
        &router,
        "PUT",
        &format!("/gateways/{gateway}/targets/{target_id}/"),
        Some(&json!({
            "name": "refetchtarget",
            "targetConfiguration": reconfigured,
            "credentialProviderConfigurations": [
                {"credentialProviderType": "GATEWAY_IAM_ROLE"}
            ],
        })),
    )
    .await;
    assert_eq!(
        updated.status, 202,
        "expected HTTP 202 from UpdateGatewayTarget, got {} ({})",
        updated.status, updated.body
    );
    assert_eq!(
        (
            text(&updated.body, "status"),
            member(&updated.body, "statusReasons")
        ),
        ("UPDATING".to_string(), Value::Null),
        "the update reports the target being updated, with the previous status's reason \
         dropped, got {}",
        updated.body
    );
    assert_settled(
        "reconfigured onto an endpoint this emulator serves",
        Settles::Ready,
        &settled_target(&router, &gateway, &target_id).await,
    );

    // Synchronized: the fetch is made once more, against the endpoint the target now
    // names, and the target settles into what it achieved.
    let synchronized = send(
        &router,
        "PUT",
        &format!("/gateways/{gateway}/synchronizeTargets"),
        Some(&json!({"targetIdList": [target_id]})),
    )
    .await;
    assert_eq!(
        synchronized.status, 202,
        "expected HTTP 202 from SynchronizeGatewayTargets, got {} ({})",
        synchronized.status, synchronized.body
    );
    let reported = synchronized
        .body
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.first())
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        text(&reported, "status"),
        "SYNCHRONIZING",
        "the synchronization reports the fetch under way, got {reported}"
    );
    assert_settled(
        "synchronized against an endpoint this emulator serves",
        Settles::Ready,
        &settled_target(&router, &gateway, &target_id).await,
    );

    // Reconfigured back onto an address nothing serves: the target reports the failed
    // fetch again, so neither outcome is a one-way door.
    let back = send(
        &router,
        "PUT",
        &format!("/gateways/{gateway}/targets/{target_id}/"),
        Some(&json!({
            "name": "refetchtarget",
            "targetConfiguration": unserved,
            "credentialProviderConfigurations": [
                {"credentialProviderType": "GATEWAY_IAM_ROLE"}
            ],
        })),
    )
    .await;
    assert_eq!(
        back.status, 202,
        "expected HTTP 202 from UpdateGatewayTarget, got {} ({})",
        back.status, back.body
    );
    assert_settled(
        "reconfigured back onto an address nothing serves",
        Settles::Unanswered,
        &settled_target(&router, &gateway, &target_id).await,
    );
}

/// A target whose fetch did not complete serves no tools: the gateway's own MCP surface
/// lists nothing for it, which is what makes the status a report about the target rather
/// than a label on it.
#[tokio::test]
async fn a_target_whose_fetch_failed_serves_no_tools() {
    let router = router();
    let (gateway, gateway_url) = settled_gateway(&router, "toolserviceg", "NONE").await;
    let configuration = json!({"mcp": {"lambda": {
        "lambdaArn": LAMBDA_ARN,
        "toolSchema": {"inlinePayload": [{
            "name": "echo",
            "description": "echo the input back",
            "inputSchema": {"type": "object"},
        }]},
    }}});
    let created = create_target(
        &router,
        &gateway,
        &create_body("declaredtools", &configuration),
    )
    .await;
    let declared_id = text(&created.body, "targetId");
    let unserved = create_target(
        &router,
        &gateway,
        &create_body(
            "unreadtools",
            &json!({"mcp": {"mcpServer": {"endpoint": "https://tools.example.com/mcp"}}}),
        ),
    )
    .await;
    let unserved_id = text(&unserved.body, "targetId");
    assert_settled(
        "a target declaring its tools",
        Settles::Ready,
        &settled_target(&router, &gateway, &declared_id).await,
    );
    assert_settled(
        "a target whose tools were never read",
        Settles::Unanswered,
        &settled_target(&router, &gateway, &unserved_id).await,
    );

    // The gateway's own MCP surface, driven the way a client holding the published URL
    // drives it.
    let (host, path) = {
        let rest = gateway_url.strip_prefix("https://").expect("an https URL");
        match rest.find('/') {
            Some(start) => (rest[..start].to_string(), rest[start..].to_string()),
            None => (rest.to_string(), "/".to_string()),
        }
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .body(Bytes::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let listed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    let names: Vec<String> = listed
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| text(tool, "name"))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec!["declaredtools___echo".to_string()],
        "case \"the gateway's tool listing\": only the target whose tools were read is \
         served, got {listed}"
    );
}
