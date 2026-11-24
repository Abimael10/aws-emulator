// SPDX-License-Identifier: CC0-1.0

//! How an operation that addresses ONE agent runtime endpoint words a resource it cannot
//! find, end to end over HTTP.
//!
//! One rule, read off the Smithy model rather than off any one recorded request: an
//! operation whose route addresses a single agent runtime endpoint binds BOTH identifiers
//! the endpoint is named by (`agentRuntimeId` and `endpointName`) as `@required`
//! `@httpLabel` members, so the lookup it performs is the endpoint's and its
//! `ResourceNotFoundException` (HTTP 404) is worded at endpoint granularity — "Agent
//! endpoint with name <endpointName> not found for agent <agentRuntimeId>" — with the two
//! variable parts taken from the request path. The sentence does not depend on which of
//! the two the store is missing, and it does not depend on the asking operation, so a read
//! and a change of the same unknown endpoint answer byte for byte alike. An operation whose
//! route addresses the RUNTIME instead (the create, which names the new endpoint in its
//! body) keeps the agent's own sentence, so the granularity follows the route rather than
//! the resource family.
//!
//! Sources: `smithy_query(query="operation",operation="GetAgentRuntimeEndpoint,\
//! UpdateAgentRuntimeEndpoint,DeleteAgentRuntimeEndpoint,CreateAgentRuntimeEndpoint,\
//! CreateAgentRuntime")` for the
//! routes, the `@httpLabel` members, the success statuses and the modelled errors with
//! their HTTP statuses, and the `AgentRuntimeId` / `EndpointName` patterns quoted below for
//! the identifier forms the rows span.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentRole";

/// An `agentRuntimeId` inside `AgentRuntimeId`'s pattern
/// `^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$` that no row ever creates.
const ABSENT_RUNTIME_ID: &str = "noSuchRuntime-abcdefghij";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the error code `restJson1` puts in `__type`, the `message` the
/// body carries and the parsed body itself.
struct Answer {
    status: u16,
    code: String,
    message: String,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
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
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status,
        code: parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        message: parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body: parsed,
    }
}

fn text(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A container artifact whose URI satisfies `RuntimeContainerUri`'s ECR pattern.
fn container_artifact(tag: &str) -> Value {
    json!({"containerConfiguration": {
        "containerUri": format!("public.ecr.aws/agentcore/demo:{tag}")
    }})
}

/// Create one agent runtime and return the `agentRuntimeId` the service minted for it.
async fn create_runtime(router: &MultiServiceRouter, name: &str) -> String {
    let answer = send(
        router,
        "PUT",
        "/runtimes/",
        Some(&json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": container_artifact("v1"),
            "roleArn": ROLE_ARN,
        })),
    )
    .await;
    assert_eq!(
        answer.status, 202,
        "setup: creating the runtime {name:?} must succeed for this suite to say anything, got {} \
         with {}",
        answer.status, answer.body
    );
    text(&answer.body, "/agentRuntimeId")
}

/// `PUT /runtimes/{agentRuntimeId}/runtime-endpoints/` — the route the model binds
/// `CreateAgentRuntimeEndpoint` at, which addresses the RUNTIME and names the endpoint in
/// the body.
async fn create_endpoint(router: &MultiServiceRouter, runtime_id: &str, name: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
        Some(&json!({"name": name})),
    )
    .await
}

/// The sentence an operation addressing one endpoint owes for an endpoint it cannot find.
fn endpoint_not_found(endpoint_name: &str, runtime_id: &str) -> String {
    format!("Agent endpoint with name {endpoint_name} not found for agent {runtime_id}")
}

/// The sentence the operations that address a RUNTIME owe for a runtime they cannot find.
fn agent_not_found(runtime_id: &str) -> String {
    format!("Agent with agentId: {runtime_id} not found")
}

/// The operations whose route addresses ONE endpoint, with the HTTP method each is bound to
/// and the body it carries: every one of them must answer the identical sentence, so neither
/// the verb nor the body can appear in it. A read, a change and a removal of the same unknown
/// endpoint are all driven here.
const ENDPOINT_OPERATIONS: [(&str, &str, bool); 3] = [
    ("GetAgentRuntimeEndpoint", "GET", false),
    ("UpdateAgentRuntimeEndpoint", "PUT", true),
    ("DeleteAgentRuntimeEndpoint", "DELETE", false),
];

/// Drive every operation that addresses one endpoint at `runtime_id`/`endpoint_name` and
/// assert each answers the modelled 404 with the endpoint-granularity sentence.
async fn assert_endpoint_sentence(
    router: &MultiServiceRouter,
    case: &str,
    runtime_id: &str,
    endpoint_name: &str,
) {
    let expected = endpoint_not_found(endpoint_name, runtime_id);
    let empty = json!({});
    let mut sentences: Vec<(&str, String)> = Vec::new();
    for (operation, method, with_body) in ENDPOINT_OPERATIONS {
        let answer = send(
            router,
            method,
            &format!("/runtimes/{runtime_id}/runtime-endpoints/{endpoint_name}/"),
            with_body.then_some(&empty),
        )
        .await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({operation}): an endpoint that is not there is the modelled 404 \
             ResourceNotFoundException, got {} {:?} with {}",
            answer.status,
            answer.code,
            answer.body
        );
        assert_eq!(
            answer.message, expected,
            "case {case:?} ({operation}): expected {expected:?}, got {:?}",
            answer.message
        );
        sentences.push((operation, answer.message));
    }
    let (first_operation, first) = &sentences[0];
    for (operation, sentence) in &sentences[1..] {
        assert_eq!(
            sentence, first,
            "case {case:?}: {operation} must answer exactly what {first_operation} answers, so \
             the verb cannot appear in the sentence, got {sentence:?} against {first:?}"
        );
    }
}

/// THE RULE: every operation that addresses one endpoint reports an endpoint it cannot find
/// at endpoint granularity, whatever is missing and whichever operation asked.
///
/// The rows differ along which of the two identifiers the store does not hold (the runtime,
/// the endpoint, both), along where the named endpoint does exist (nowhere, on another
/// runtime), and along the identifiers' form: the shortest `EndpointName` the pattern
/// admits, one carrying digits and underscores, the longest the pattern admits, and the
/// longest `AgentRuntimeId` the pattern admits.
#[tokio::test]
async fn an_operation_addressing_one_endpoint_reports_the_endpoint() {
    let router = router();
    let owner = create_runtime(&router, "owner_runtime").await;
    let other = create_runtime(&router, "other_runtime").await;
    let created = create_endpoint(&router, &owner, "onlyHere").await;
    assert_eq!(
        created.status, 202,
        "setup: the endpoint create must succeed for this suite to say anything, got {} with {}",
        created.status, created.body
    );

    // `EndpointName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`: one leading letter plus up to 47
    // more characters, so the longest name is 48 characters.
    let longest_endpoint_name = format!("e{}", "x".repeat(47));
    // `AgentRuntimeId` is `^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$`: the longest id is
    // 100 characters, a hyphen and 10 more.
    let longest_runtime_id = format!("r{}-0123456789", "a".repeat(99));

    // (case, the agentRuntimeId the path carries, the endpointName the path carries)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "neither the runtime nor the endpoint is stored",
            ABSENT_RUNTIME_ID.to_string(),
            "missingendpoint".to_string(),
        ),
        (
            "the runtime is stored, the endpoint it is asked for is not",
            owner.clone(),
            "neverCreated".to_string(),
        ),
        (
            "the runtime is stored and the endpoint exists on ANOTHER runtime",
            other.clone(),
            "onlyHere".to_string(),
        ),
        (
            "the endpoint name is stored elsewhere but this runtime is not",
            ABSENT_RUNTIME_ID.to_string(),
            "onlyHere".to_string(),
        ),
        (
            "a name carrying digits and underscores",
            owner.clone(),
            "canary_v2_0".to_string(),
        ),
        (
            "the shortest name the pattern admits",
            owner.clone(),
            "e".to_string(),
        ),
        (
            "the longest name the pattern admits",
            owner.clone(),
            longest_endpoint_name.clone(),
        ),
        (
            "the longest runtime id the pattern admits, which nothing created",
            longest_runtime_id.clone(),
            longest_endpoint_name,
        ),
    ];

    for (case, runtime_id, endpoint_name) in rows {
        assert_endpoint_sentence(&router, case, &runtime_id, &endpoint_name).await;
    }

    // None of the refusals above touched the store: the endpoint its own runtime holds still
    // reads back under its own name.
    let held = send(
        &router,
        "GET",
        &format!("/runtimes/{owner}/runtime-endpoints/onlyHere/"),
        None,
    )
    .await;
    assert_eq!(
        (held.status, text(&held.body, "/name").as_str()),
        (200, "onlyHere"),
        "case \"the endpoint its runtime holds\": the refusals above changed nothing, got {} with \
         {}",
        held.status,
        held.body
    );
}

/// The granularity follows the ROUTE, not the resource family: `CreateAgentRuntimeEndpoint`
/// addresses the runtime (the endpoint it names does not exist yet), so a runtime it cannot
/// find is reported in the agent's own sentence — while the operations that address an
/// endpoint on that same unknown runtime report the endpoint, naming both identifiers.
#[tokio::test]
async fn a_create_addresses_the_runtime_and_reports_the_agent() {
    let router = router();

    for (case, runtime_id, endpoint_name) in [
        ("a runtime id nothing created", ABSENT_RUNTIME_ID, "live"),
        (
            "another runtime id nothing created",
            "r_probe2-0123456789",
            "canary",
        ),
    ] {
        let refused = create_endpoint(&router, runtime_id, endpoint_name).await;
        let agent_sentence = agent_not_found(runtime_id);
        assert_eq!(
            (refused.status, refused.code.as_str(), refused.message.as_str()),
            (404, "ResourceNotFoundException", agent_sentence.as_str()),
            "case {case:?}: a create addresses the runtime, so expected 404 \
             ResourceNotFoundException {agent_sentence:?}, got {} {:?} {:?}",
            refused.status,
            refused.code,
            refused.message
        );

        assert_endpoint_sentence(&router, case, runtime_id, endpoint_name).await;
        let endpoint_sentence = endpoint_not_found(endpoint_name, runtime_id);
        assert_ne!(
            endpoint_sentence, agent_sentence,
            "case {case:?}: the endpoint-granularity sentence must not be the agent's own, got \
             {endpoint_sentence:?}"
        );
    }
}
