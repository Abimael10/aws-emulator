// SPDX-License-Identifier: CC0-1.0

//! `SynchronizeGatewayTargets` end to end over HTTP, every row read back through
//! `GetGatewayTarget` and `ListGatewayTargets`.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared
//! engine — so what the synchronization writes is what the read reports.
//!
//! The rules under test, read off the Smithy model rather than off any one recorded
//! request (`smithy_query(query="operation",operation="SynchronizeGatewayTargets")`,
//! `smithy_query(query="shape",shape="McpServerTargetConfiguration,TargetConfiguration")`):
//!
//! 1. `SynchronizeGatewayTargets` (`PUT /gateways/{gatewayIdentifier}/synchronizeTargets`)
//!    fetches tool definitions FROM the target's endpoint, so it applies only to the target
//!    shape whose tools live at the endpoint rather than in its own configuration: an MCP
//!    SERVER target. An MCP server pinned to a static `mcpToolSchema` is the exception
//!    ("Dynamic tool discovery/synchronization will be disabled when a target is configured
//!    with mcpToolSchema"), and every other target type — one carrying its tools in its
//!    configuration (an OpenAPI schema, a Smithy model, a Lambda tool schema) and one
//!    serving no tool listing at all (an HTTP or an inference target) — is refused 400
//!    `ValidationException` naming what is not supported, with nothing about the target
//!    changed.
//! 2. An accepted synchronization records the instant it attempted the fetch in
//!    `lastSynchronizedAt`: absent until the first attempt, reported by the
//!    synchronization's own answer, by a later read and by the listing row alike, and
//!    advanced by each later attempt. `updatedAt` is untouched — a synchronization
//!    refreshes what the target serves rather than reconfiguring it, which is why the shape
//!    carries the two instants separately.
//! 3. The fetch's outcome is owed by the clock, not by the call: the call answers 202
//!    reporting `SYNCHRONIZING`, a caller acting straight after still sees that status, and
//!    the target then settles into what the fetch achieved. An endpoint nothing in this
//!    emulator serves answers nothing, so the attempt settles the way a fetch that could
//!    not be completed does — `FAILED`, reporting why in `statusReasons` — rather than
//!    reporting tool definitions nothing read. The same fetch is attempted by the CREATE
//!    itself, which is why such a target is already `FAILED` before the first explicit
//!    synchronization (the suite `gateway_target_tool_discovery` is about that rule).
//! 4. A target is addressed only through its gateway: an identifier naming no stored
//!    gateway, a target id naming no target of that gateway and a target whose deletion was
//!    already accepted are each the modelled `ResourceNotFoundException` (404). The gateway
//!    itself has to be in the terminal status it settles into: while the clock still owes it
//!    a transition (`CREATING`), the synchronization is refused 400 `ValidationException`
//!    naming the operation and the status it found.

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

/// A `gatewayIdentifier` inside `GatewayIdentifier`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that no row ever creates.
const ABSENT_GATEWAY_ID: &str = "no-such-gateway-0123456789";

/// A `targetId` inside `TargetId`'s pattern `^[0-9a-zA-Z]{10}$` that no row ever creates.
const ABSENT_TARGET_ID: &str = "ZZZZ999999";

/// The refusal a target pinned to a static tool list gets, in the words the model
/// documents for exactly that case
/// (`smithy_query(query="doc",shape="SynchronizeGatewayTargets")`).
const STATIC_SCHEMA_REFUSAL: &str =
    "You cannot synchronize a target that has a static tool schema (mcpToolSchema) \
     configured. Remove the static schema through an UpdateGatewayTarget call to enable \
     dynamic tool synchronization.";

/// The reason an attempted fetch leaves the target `FAILED` with when NOTHING in this
/// emulator answers the endpoint the target names: the fixed sentence, with that endpoint
/// filled in from the target's own configuration.
fn fetch_failure_reason(endpoint: &str) -> String {
    format!(
        "The MCP server at {endpoint} did not respond to the tool discovery request. Verify \
         that the endpoint is reachable from the gateway and serves the Model Context \
         Protocol, then synchronize the target again."
    )
}

/// The one reason the settled status reports, asserted in full against the endpoint the
/// target itself names, read off the same body.
fn assert_fetch_failure(case: &str, body: &Value) {
    let endpoint = body
        .pointer("/targetConfiguration/mcp/mcpServer/endpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !endpoint.is_empty(),
        "case {case:?}: this row's target names an MCP server endpoint, got {body}"
    );
    assert_eq!(
        member(body, "statusReasons"),
        json!([fetch_failure_reason(&endpoint)]),
        "case {case:?}: expected the reason the failed fetch of {endpoint:?} is reported in, \
         got {body}"
    );
}

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

/// A member as the body carries it, `Value::Null` when it is absent.
fn member(body: &Value, name: &str) -> Value {
    body.get(name).cloned().unwrap_or(Value::Null)
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

async fn get_target(router: &MultiServiceRouter, gateway: &str, target: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway}/targets/{target}/"),
        None,
    )
    .await
}

/// The `TargetStatus` values a target settles into, and the poll that waits for one, are
/// the crate's one reading of a target's clock-owed transition
/// (`tests/all/gateway_target_settling.rs`).
use crate::gateway_target_settling::{settled_target as settled_target_answer, POLL_DEADLINE, POLL_PAUSE};

/// Read the target until the clock has made its transition out of the transient status
/// due, as this suite's own answer type.
async fn settled_target(router: &MultiServiceRouter, gateway: &str, target: &str) -> Answer {
    let (status, body) = settled_target_answer(router, gateway, target).await;
    Answer { status, body }
}

async fn list_targets(router: &MultiServiceRouter, gateway: &str) -> Answer {
    send(router, "GET", &format!("/gateways/{gateway}/targets/"), None).await
}

async fn synchronize(router: &MultiServiceRouter, gateway: &str, target_ids: &[&str]) -> Answer {
    send(
        router,
        "PUT",
        &format!("/gateways/{gateway}/synchronizeTargets"),
        Some(&json!({"targetIdList": target_ids})),
    )
    .await
}

/// Create a gateway and hand back its `gatewayId`, waited out to `READY`.
///
/// A gateway serves a target operation only once the clock-owed transition out of
/// `CREATING` has come due, so every row that is about the synchronization starts from a
/// settled gateway.
async fn settled_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let gateway_id = creating_gateway(router, name).await;
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
        "case {name:?}: the gateway must settle to READY before its targets can be \
         synchronized, got {} {} ({})",
        settled.status,
        text(&settled.body, "status"),
        settled.body
    );
    gateway_id
}

/// Create a gateway and hand back its `gatewayId` WITHOUT waiting the clock out, so it is
/// still in the transient status the clock owes a transition for.
async fn creating_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "authorizerType": "AWS_IAM",
    });
    let created = send(router, "POST", "/gateways/", Some(&body)).await;
    assert_eq!(
        created.status, 202,
        "case {name:?}: expected HTTP 202 from CreateGateway, got {} ({})",
        created.status, created.body
    );
    text(&created.body, "gatewayId")
}

/// A `CreateGatewayTarget` body for `configuration`. Every target carries the gateway's own
/// IAM role as its credential provider (`CredentialProviderType`'s `GATEWAY_IAM_ROLE`),
/// which is the credential the synchronization itself does not vary along.
fn create_body(name: &str, configuration: &Value) -> Value {
    json!({
        "name": name,
        "targetConfiguration": configuration,
        "credentialProviderConfigurations": [{"credentialProviderType": "GATEWAY_IAM_ROLE"}],
    })
}

/// True when `later` is strictly after `earlier`, whichever form the protocol rendered the
/// timestamp in: epoch seconds compare numerically, and an RFC 3339 instant compares
/// lexically (that encoding sorts in chronological order).
fn timestamp_after(later: &Value, earlier: &Value) -> bool {
    match (later, earlier) {
        (Value::Number(later), Value::Number(earlier)) => {
            later.as_f64().unwrap_or_default() > earlier.as_f64().unwrap_or_default()
        }
        (Value::String(later), Value::String(earlier)) => later > earlier,
        _ => false,
    }
}

/// One row of the configuration table: the target configuration a target is created with,
/// and the refusal its synchronization must answer with — `None` when the target's tool
/// definitions live at its endpoint and the synchronization is accepted.
struct Row {
    case: &'static str,
    /// The `name` the row's target is created under; inside `TargetName`'s pattern
    /// `^([0-9a-zA-Z][-]?){1,100}$`.
    target_name: &'static str,
    configuration: Value,
    /// The `ValidationException` message the synchronization is refused with.
    refusal: Option<String>,
}

/// The table spans `TargetConfiguration`'s union members — the MCP shapes, the HTTP ones
/// and the inference ones — and, inside the one shape that can pin a static tool list, both
/// forms of that list, which is what the rule generalises over
/// (`smithy_query(query="shape",shape="TargetConfiguration,McpTargetConfiguration,
/// HttpTargetConfiguration,InferenceTargetConfiguration,McpToolSchemaConfiguration")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an MCP server endpoint with no static tool schema",
            target_name: "mcpservertarget",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://tools.example.com/mcp"
            }}}),
            refusal: None,
        },
        Row {
            case: "an MCP server endpoint listing its tools dynamically",
            target_name: "dynamicmcpserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://dynamic.example.com/mcp",
                "listingMode": "DYNAMIC",
                "resourcePriority": 7,
            }}}),
            refusal: None,
        },
        Row {
            case: "an MCP server endpoint pinned to an inline static tool schema",
            target_name: "inlineschemaserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://pinned.example.com/mcp",
                "mcpToolSchema": {"inlinePayload": "{\"tools\":[]}"},
            }}}),
            refusal: Some(STATIC_SCHEMA_REFUSAL.to_string()),
        },
        Row {
            case: "an MCP server endpoint pinned to a static tool schema in S3",
            target_name: "s3schemaserver",
            configuration: json!({"mcp": {"mcpServer": {
                "endpoint": "https://pinned-s3.example.com/mcp",
                "mcpToolSchema": {"s3": {"uri": "s3://schemas-bucket/tools-static.json"}},
            }}}),
            refusal: Some(STATIC_SCHEMA_REFUSAL.to_string()),
        },
        Row {
            case: "an MCP target serving an inline OpenAPI schema",
            target_name: "openapitarget",
            configuration: json!({"mcp": {"openApiSchema": {
                "inlinePayload": "{\"openapi\":\"3.0.0\"}"
            }}}),
            refusal: Some(unsupported_target_type("OPEN_API_SCHEMA")),
        },
        Row {
            case: "an MCP target serving a Smithy model from S3",
            target_name: "smithymodeltarget",
            configuration: json!({"mcp": {"smithyModel": {
                "s3": {"uri": "s3://schemas-bucket/tools.json"}
            }}}),
            refusal: Some(unsupported_target_type("SMITHY_MODEL")),
        },
        Row {
            case: "an MCP target whose tools a Lambda function handles",
            target_name: "lambdatarget",
            configuration: json!({"mcp": {"lambda": {
                "lambdaArn": "arn:aws:lambda:us-east-1:000000000000:function:tools",
                "toolSchema": {"inlinePayload": [{
                    "name": "echo",
                    "description": "echo the input back",
                    "inputSchema": {"type": "object"},
                }]},
            }}}),
            refusal: Some(unsupported_target_type("LAMBDA")),
        },
        Row {
            case: "an HTTP passthrough endpoint",
            target_name: "passthroughtarget",
            configuration: json!({"http": {"passthrough": {
                "endpoint": "https://api.example.com/v1",
                "protocolType": "MCP",
            }}}),
            refusal: Some(unsupported_target_type("PASSTHROUGH")),
        },
        Row {
            case: "an HTTP target routing to an AgentCore runtime",
            target_name: "runtimetarget",
            configuration: json!({"http": {"agentcoreRuntime": {
                "arn": "arn:aws:bedrock-agentcore:us-east-1:000000000000:runtime/\
                        myruntime-0123456789",
            }}}),
            refusal: Some(unsupported_target_type("AGENTCORE_RUNTIME")),
        },
        Row {
            case: "an inference target routing to an LLM provider",
            target_name: "inferencetarget",
            configuration: json!({"inference": {"provider": {
                "endpoint": "https://llm.example.com/v1",
            }}}),
            refusal: Some(unsupported_target_type("PROVIDER")),
        },
    ]
}

/// The refusal a target whose type synchronization does not apply to gets, for the
/// `TargetType` it is (`smithy_query(query="shape",shape="TargetType")`).
fn unsupported_target_type(target_type: &str) -> String {
    format!("Target type {target_type} is not supported for synchronization")
}

/// Rule 1, 2 and 3 over the configuration table: which targets have tool definitions to
/// fetch, what the fetch records, and what a target that has none answers.
#[tokio::test]
async fn synchronization_applies_to_targets_that_discover_their_tool_definitions() {
    let router = router();
    let gateway = settled_gateway(&router, "syncrowsgateway").await;

    // Every row's target is created first and the clock waited out ONCE, so each row
    // starts from a settled target.
    let mut created: Vec<(Row, String, Value, Value)> = Vec::new();
    for row in rows() {
        let answer =
            create_target(&router, &gateway, &create_body(row.target_name, &row.configuration))
                .await;
        assert_eq!(
            answer.status, 202,
            "case {:?}: expected HTTP 202 from CreateGatewayTarget, got {} ({})",
            row.case, answer.status, answer.body
        );
        assert_eq!(
            member(&answer.body, "lastSynchronizedAt"),
            Value::Null,
            "case {:?}: a freshly created target has been synchronized by nothing, so \
             lastSynchronizedAt must be absent, got {}",
            row.case,
            answer.body
        );
        let target_id = text(&answer.body, "targetId");
        let created_at = member(&answer.body, "createdAt");
        let updated_at = member(&answer.body, "updatedAt");
        created.push((row, target_id, created_at, updated_at));
    }

    let mut synchronized: Vec<(&'static str, String, Value)> = Vec::new();
    for (row, target_id, created_at, updated_at) in &created {
        // The create attempts the same fetch, so a target whose tools are DISCOVERED from
        // an endpoint nothing serves has already settled `FAILED` on that attempt, and one
        // whose tools its own configuration declares has settled `READY` with nothing
        // fetched. The transition is the clock's, so the row reads the target again until
        // it reports a status it settles into rather than waiting a fixed time for one.
        let settled_before = if row.refusal.is_none() { "FAILED" } else { "READY" };
        let before = settled_target(&router, &gateway, target_id).await;
        assert_eq!(
            (before.status, text(&before.body, "status").as_str()),
            (200, settled_before),
            "case {:?}: the target must settle to {settled_before} before it is \
             synchronized, got {} {} ({})",
            row.case,
            before.status,
            text(&before.body, "status"),
            before.body
        );

        let answer = synchronize(&router, &gateway, &[target_id.as_str()]).await;
        let Some(refusal) = row.refusal.as_deref() else {
            assert_eq!(
                answer.status, 202,
                "case {:?}: expected HTTP 202 from SynchronizeGatewayTargets, got {} ({})",
                row.case, answer.status, answer.body
            );
            let targets = answer
                .body
                .get("targets")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                targets.len(),
                1,
                "case {:?}: the answer reports the one target the request named, got {}",
                row.case,
                answer.body
            );
            let reported = &targets[0];
            assert_eq!(
                text(reported, "targetId"),
                *target_id,
                "case {:?}: expected the synchronized target {target_id}, got {reported}",
                row.case
            );
            assert_eq!(
                text(reported, "status"),
                "SYNCHRONIZING",
                "case {:?}: the fetch's outcome is the clock's, so the answer reports the \
                 fetch under way, got {reported}",
                row.case
            );
            assert_eq!(
                member(reported, "statusReasons"),
                Value::Null,
                "case {:?}: the fetch has not finished, so there is no reason to report \
                 yet, got {reported}",
                row.case
            );
            let stamped = member(reported, "lastSynchronizedAt");
            assert!(
                stamped.is_string(),
                "case {:?}: lastSynchronizedAt is a date-time string on this wire, got \
                 {stamped}",
                row.case
            );
            assert_eq!(
                member(reported, "createdAt"),
                *created_at,
                "case {:?}: a synchronization does not touch the target's creation \
                 instant, got {reported}",
                row.case
            );
            assert_eq!(
                member(reported, "updatedAt"),
                *updated_at,
                "case {:?}: a synchronization refreshes what the target serves rather \
                 than reconfiguring it, so updatedAt is untouched, got {reported}",
                row.case
            );

            let read = get_target(&router, &gateway, target_id).await;
            assert_eq!(
                (read.status, text(&read.body, "status").as_str()),
                (200, "SYNCHRONIZING"),
                "case {:?}: a caller acting straight after the synchronization still sees \
                 SYNCHRONIZING, got {} {} ({})",
                row.case,
                read.status,
                text(&read.body, "status"),
                read.body
            );
            assert_eq!(
                member(&read.body, "lastSynchronizedAt"),
                stamped,
                "case {:?}: the read reports exactly the instant the synchronization \
                 recorded, got {}",
                row.case,
                read.body
            );
            synchronized.push((row.case, target_id.clone(), stamped));
            continue;
        };

        assert_eq!(
            (answer.status, error_code(&answer.body).as_str()),
            (400, "ValidationException"),
            "case {:?}: a target with no tool definitions to fetch is refused, got {} {} \
             ({})",
            row.case,
            answer.status,
            error_code(&answer.body),
            answer.body
        );
        assert_eq!(
            text(&answer.body, "message"),
            refusal,
            "case {:?}: expected the refusal {refusal:?}, got {}",
            row.case,
            answer.body
        );
        let read = get_target(&router, &gateway, target_id).await;
        assert_eq!(
            (
                read.status,
                text(&read.body, "status"),
                member(&read.body, "lastSynchronizedAt")
            ),
            (200, "READY".to_string(), Value::Null),
            "case {:?}: a refused synchronization writes nothing, got {} ({})",
            row.case,
            read.status,
            read.body
        );
    }

    assert!(
        !synchronized.is_empty(),
        "the table must carry at least one configuration whose synchronization is accepted"
    );

    // The clock owes every attempted fetch its outcome: an endpoint nothing in this
    // emulator serves answers nothing, so each attempt settles FAILED with its reason, and
    // the instant the attempt recorded stays put. Each row reads the target again until it
    // reports a status it settles into.
    for (case, target_id, stamped) in &synchronized {
        let settled = settled_target(&router, &gateway, target_id).await;
        assert_eq!(
            (settled.status, text(&settled.body, "status").as_str()),
            (200, "FAILED"),
            "case {case:?}: a fetch of an endpoint this emulator cannot read settles \
             FAILED, got {} {} ({})",
            settled.status,
            text(&settled.body, "status"),
            settled.body
        );
        assert_fetch_failure(case, &settled.body);
        assert_eq!(
            member(&settled.body, "lastSynchronizedAt"),
            *stamped,
            "case {case:?}: the recorded attempt instant does not move on its own, got {}",
            settled.body
        );
    }
}

/// Rule 2 as an invariant: each attempted synchronization of one target reports an instant
/// strictly after the one before it, and every reader of the target reports the latest one
/// — a target the previous attempt left FAILED included.
#[tokio::test]
async fn each_synchronization_advances_the_recorded_instant() {
    let router = router();
    let gateway = settled_gateway(&router, "syncadvancegateway").await;
    let configuration = json!({"mcp": {"mcpServer": {
        "endpoint": "https://tools.example.com/mcp"
    }}});
    let created = create_target(
        &router,
        &gateway,
        &create_body("advancingtarget", &configuration),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "expected HTTP 202 from CreateGatewayTarget, got {} ({})",
        created.status, created.body
    );
    let target_id = text(&created.body, "targetId");

    // The create's own fetch is the clock's too: the target is read again until it reports
    // a status it settles into.
    let before = settled_target(&router, &gateway, &target_id).await;
    assert_eq!(
        member(&before.body, "lastSynchronizedAt"),
        Value::Null,
        "case \"before any synchronization\": lastSynchronizedAt must be absent, got {}",
        before.body
    );

    let mut previous = Value::Null;
    for round in 1..=3u32 {
        let case = format!("synchronization round {round}");
        let answer = synchronize(&router, &gateway, &[target_id.as_str()]).await;
        assert_eq!(
            answer.status, 202,
            "case {case:?}: expected HTTP 202, got {} ({})",
            answer.status, answer.body
        );
        let reported = answer
            .body
            .get("targets")
            .and_then(Value::as_array)
            .and_then(|targets| targets.first())
            .cloned()
            .unwrap_or(Value::Null);
        let stamped = member(&reported, "lastSynchronizedAt");
        assert!(
            stamped.is_string(),
            "case {case:?}: the answer reports the instant it fetched at, got {reported}"
        );
        if !previous.is_null() {
            assert!(
                timestamp_after(&stamped, &previous),
                "case {case:?}: expected an instant strictly after {previous}, got {stamped}"
            );
        }

        let read = get_target(&router, &gateway, &target_id).await;
        assert_eq!(
            member(&read.body, "lastSynchronizedAt"),
            stamped,
            "case {case:?}: the read reports the latest recorded instant, got {}",
            read.body
        );
        let listing = list_targets(&router, &gateway).await;
        let row = listing
            .body
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| text(item, "targetId") == target_id)
                    .cloned()
            })
            .unwrap_or(Value::Null);
        assert_eq!(
            member(&row, "lastSynchronizedAt"),
            stamped,
            "case {case:?}: the listing row cannot drift from the read, got {}",
            listing.body
        );

        previous = stamped;
        // The clock owes each attempt its outcome before the next round starts: the target
        // is read again until it reports a status it settles into, which is also what makes
        // the next attempt's instant a later one.
        let settled = settled_target(&router, &gateway, &target_id).await;
        assert_eq!(
            text(&settled.body, "status"),
            "FAILED",
            "case {case:?}: an attempt against an endpoint nothing in this emulator serves \
             settles FAILED, got {}",
            settled.body
        );
        assert_fetch_failure(&case, &settled.body);
    }
}

/// Rule 4: what a synchronization naming a resource that is not there, or a gateway the
/// clock still owes a transition, answers — and that it writes nothing.
#[tokio::test]
async fn synchronization_is_refused_for_resources_it_cannot_address() {
    let router = router();
    let gateway = settled_gateway(&router, "syncrefusalgateway").await;
    let configuration = json!({"mcp": {"mcpServer": {
        "endpoint": "https://tools.example.com/mcp"
    }}});
    let created = create_target(
        &router,
        &gateway,
        &create_body("refusaltarget", &configuration),
    )
    .await;
    let target_id = text(&created.body, "targetId");
    // The target's own transition is the clock's too: read it again until it reports the
    // status this row starts from, rather than waiting a fixed time for it.
    let started = Instant::now();
    while text(&get_target(&router, &gateway, &target_id).await.body, "status") != "READY"
        && started.elapsed() <= POLL_DEADLINE
    {
        tokio::time::sleep(POLL_PAUSE).await;
    }

    // A gateway identifier naming no stored gateway, and a target id naming no target of a
    // stored gateway, are each the modelled 404 — the target is addressed only through its
    // gateway.
    for (case, gateway_id, requested, expected_message) in [
        (
            "a gatewayIdentifier naming no stored gateway",
            ABSENT_GATEWAY_ID.to_string(),
            target_id.clone(),
            "Failed to retrieve gateway because it doesn't exist. Retry the request with a \
             different resource identifier.",
        ),
        (
            "a targetId naming no target of this gateway",
            gateway.clone(),
            ABSENT_TARGET_ID.to_string(),
            "Failed to retrieve target because it doesn't exist. Retry the request with a \
             different resource identifier.",
        ),
    ] {
        let answer = synchronize(&router, &gateway_id, &[requested.as_str()]).await;
        assert_eq!(
            (answer.status, error_code(&answer.body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected the modelled 404, got {} {} ({})",
            answer.status,
            error_code(&answer.body),
            answer.body
        );
        assert_eq!(
            text(&answer.body, "message"),
            expected_message,
            "case {case:?}: expected the refusal {expected_message:?}, got {}",
            answer.body
        );
    }

    // The target the rows above never reached is untouched.
    let read = get_target(&router, &gateway, &target_id).await;
    assert_eq!(
        member(&read.body, "lastSynchronizedAt"),
        Value::Null,
        "case \"after the refused synchronizations\": nothing was written, got {}",
        read.body
    );

    // A target whose deletion was already accepted is on its way out: it is no longer
    // there to synchronize.
    let deleted = send(
        &router,
        "DELETE",
        &format!("/gateways/{gateway}/targets/{target_id}/"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 202,
        "case \"a deleted target\": expected HTTP 202 from DeleteGatewayTarget, got {} ({})",
        deleted.status, deleted.body
    );
    let answer = synchronize(&router, &gateway, &[target_id.as_str()]).await;
    assert_eq!(
        (answer.status, error_code(&answer.body).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"a deleted target\": expected the modelled 404, got {} {} ({})",
        answer.status,
        error_code(&answer.body),
        answer.body
    );

    // A gateway the clock still owes a transition serves no target operation, and says so
    // before it resolves anything the body names.
    let creating = creating_gateway(&router, "syncpendinggateway").await;
    let answer = synchronize(&router, &creating, &[ABSENT_TARGET_ID]).await;
    assert_eq!(
        (answer.status, error_code(&answer.body).as_str()),
        (400, "ValidationException"),
        "case \"a gateway still CREATING\": expected the modelled 400, got {} {} ({})",
        answer.status,
        error_code(&answer.body),
        answer.body
    );
    assert_eq!(
        text(&answer.body, "message"),
        "Cannot perform operation SynchronizeGatewayTargets when gateway is in CREATING status",
        "case \"a gateway still CREATING\": expected the shared status refusal, got {}",
        answer.body
    );
}
