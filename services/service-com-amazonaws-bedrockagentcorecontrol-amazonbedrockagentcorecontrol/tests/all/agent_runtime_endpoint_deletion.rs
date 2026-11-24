// SPDX-License-Identifier: CC0-1.0

//! Removing ONE agent runtime endpoint, end to end over HTTP.
//!
//! One rule, read off the Smithy model rather than off any one recorded request: a served
//! `DeleteAgentRuntimeEndpoint` removes the endpoint its two `@httpLabel` identifiers name
//! from the runtime that holds it — answering the modelled 202 with those identifiers and
//! the `AgentRuntimeEndpointStatus` a removal puts the endpoint in — so
//!
//! * the endpoint is gone from every later call that addresses it: the read is the
//!   modelled `404 ResourceNotFoundException` at endpoint granularity, and a second delete
//!   answers that same sentence;
//! * a listing of the runtime no longer reports it, while every OTHER endpoint on that
//!   runtime, and the runtime itself, are left exactly as they were;
//! * the name it held is free again, so a create that was the modelled `409
//!   ConflictException` while the endpoint existed is served afterwards.
//!
//! Sources: `smithy_query(query="operation",operation="DeleteAgentRuntimeEndpoint,\
//! CreateAgentRuntimeEndpoint,GetAgentRuntimeEndpoint,ListAgentRuntimeEndpoints,\
//! CreateAgentRuntime")` for the routes, the methods, the success statuses, the members
//! (`clientToken` is this delete's `@httpQuery` `@idempotencyToken`) and the modelled
//! errors, and `smithy_query(query="shape",shape="AgentRuntimeEndpointStatus,EndpointName,\
//! ClientToken,AgentRuntimeVersion")` for the status values and the name, token and version
//! grammars the rows span.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentRole";

/// The `AgentRuntimeVersion` a created runtime carries: the grammar `^([1-9][0-9]{0,4})$`
/// starts at 1, and only an update of the runtime publishes another.
const FIRST_VERSION: &str = "1";

/// The name of the endpoint this service manages for every agent runtime: it is there
/// without any caller create, so every listing below expects it beside the row's own.
const MANAGED_ENDPOINT: &str = "DEFAULT";

/// Every `AgentRuntimeEndpointStatus` value
/// (`smithy_query(query="shape",shape="AgentRuntimeEndpointStatus")`).
const ENDPOINT_STATUS_VALUES: [&str; 7] = [
    "CREATING",
    "CREATE_FAILED",
    "UPDATING",
    "UPDATE_FAILED",
    "READY",
    "DELETING",
    "DELETE_FAILED",
];

/// How this service words the clash a second endpoint under one name on one runtime is.
const NAME_TAKEN_MESSAGE: &str =
    "An endpoint with the specified name already exists. Check the request and try again.";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and what
/// the handlers read the ARN region from.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the error code `restJson1` puts in `__type` and the parsed body.
struct Answer {
    status: u16,
    code: String,
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
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status: response.status().as_u16(),
        code: parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        body: parsed,
    }
}

/// The member at `pointer` as text, or the empty string when the answer carries none.
fn text(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence every operation addressing ONE endpoint owes for an endpoint it cannot
/// find, at endpoint granularity.
fn endpoint_not_found(endpoint_name: &str, runtime_id: &str) -> String {
    format!("Agent endpoint with name {endpoint_name} not found for agent {runtime_id}")
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
/// `CreateAgentRuntimeEndpoint` at, which addresses the runtime and names the endpoint in
/// its body.
async fn create_endpoint(router: &MultiServiceRouter, runtime_id: &str, body: &Value) -> Answer {
    send(
        router,
        "PUT",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
        Some(body),
    )
    .await
}

/// `GET /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route the model
/// binds `GetAgentRuntimeEndpoint` at.
async fn get_endpoint(router: &MultiServiceRouter, runtime_id: &str, name: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
        None,
    )
    .await
}

/// `DELETE /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route the
/// model binds `DeleteAgentRuntimeEndpoint` at, with the `clientToken` it binds to the
/// query string when the row carries one.
async fn delete_endpoint(
    router: &MultiServiceRouter,
    runtime_id: &str,
    name: &str,
    client_token: Option<&str>,
) -> Answer {
    let uri = match client_token {
        None => format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
        Some(token) => {
            format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/?clientToken={token}")
        }
    };
    send(router, "DELETE", &uri, None).await
}

/// `POST /runtimes/{agentRuntimeId}/runtime-endpoints/` — the route the model binds
/// `ListAgentRuntimeEndpoints` at. Returns the `name` of every entry it reports.
async fn listed_endpoint_names(
    router: &MultiServiceRouter,
    runtime_id: &str,
    case: &str,
) -> Vec<String> {
    let answer = send(
        router,
        "POST",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: listing the endpoints of a stored runtime is HTTP 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["runtimeEndpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| text(entry, "/name"))
        .collect()
}

/// One deletion row: the endpoints created on a runtime of this row's own, which of them
/// the row deletes, and the `clientToken` the delete carries.
struct Removed {
    case: &'static str,
    runtime: &'static str,
    /// The create bodies, in the order the row creates them.
    endpoints: Vec<Value>,
    /// The index into `endpoints` the row deletes.
    target: usize,
    /// Inside `ClientToken`'s len 33..256 and pattern
    /// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$` when a row carries one.
    client_token: Option<&'static str>,
}

/// THE RULE: a served delete removes the ONE endpoint its path names, and nothing else.
///
/// The rows differ along how many endpoints the runtime holds (one, three), which of them
/// the delete names (the first, the middle, the last created), the shape of the names (the
/// shortest `EndpointName` admits, digits and underscores, the 48-character bound), what
/// the removed endpoint carried (nothing, a description, tags) and whether the delete
/// carries the modelled `clientToken`.
#[tokio::test]
async fn a_served_delete_removes_one_endpoint_and_leaves_the_rest() {
    let router = router();

    // `EndpointName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`: a leading letter plus up to 47
    // more characters, so the longest name is 48 characters.
    let longest_name = format!("z{}", "x".repeat(47));
    let rows: Vec<Removed> = vec![
        Removed {
            case: "the only endpoint, under the shortest name the pattern admits",
            runtime: "deletion_runtime_single",
            endpoints: vec![json!({"name": "e"})],
            target: 0,
            client_token: None,
        },
        Removed {
            case: "the first of three, deleted with an idempotency token",
            runtime: "deletion_runtime_first",
            endpoints: vec![
                json!({"name": "alpha", "description": "the first endpoint"}),
                json!({"name": "beta"}),
                json!({"name": "gamma", "tags": {"team": "agents"}}),
            ],
            target: 0,
            client_token: Some("deletionTokenAAAAAAAAAAAAAAAAAAAA"),
        },
        Removed {
            case: "the middle of three, names carrying digits and underscores",
            runtime: "deletion_runtime_middle",
            endpoints: vec![
                json!({"name": "canary_1"}),
                json!({"name": "live_v2_0", "agentRuntimeVersion": FIRST_VERSION}),
                json!({"name": "prod_99", "description": "d".repeat(256)}),
            ],
            target: 1,
            client_token: None,
        },
        Removed {
            case: "the last of three, at the 48-character name bound",
            runtime: "deletion_runtime_last",
            endpoints: vec![
                json!({"name": "A"}),
                json!({"name": "m_name", "tags": {"env": "prod", "owner": "platform"}}),
                json!({"name": longest_name.clone()}),
            ],
            target: 2,
            client_token: Some("deletionTokenBBBBBBBBBBBBBBBBBBBB"),
        },
    ];

    for row in rows {
        let case = row.case;
        let runtime_id = create_runtime(&router, row.runtime).await;
        let names: Vec<String> = row
            .endpoints
            .iter()
            .map(|body| text(body, "/name"))
            .collect();
        for body in &row.endpoints {
            let created = create_endpoint(&router, &runtime_id, body).await;
            assert_eq!(
                created.status, 202,
                "case {case:?}: creating {body} must succeed for this row to say anything, got {} \
                 with {}",
                created.status, created.body
            );
        }
        let target = names[row.target].clone();
        let survivors: Vec<String> = names
            .iter()
            .filter(|name| **name != target)
            .cloned()
            .collect();

        let before = listed_endpoint_names(&router, &runtime_id, case).await;
        assert!(
            before.contains(&target) && before.len() == names.len() + 1,
            "case {case:?}: the runtime holds the row's endpoints and the managed one before the \
             delete, got {before:?}"
        );

        let removed = delete_endpoint(&router, &runtime_id, &target, row.client_token).await;
        assert_eq!(
            removed.status, 202,
            "case {case:?}: deleting the endpoint {target:?} is the modelled HTTP 202, got {} with \
             {}",
            removed.status, removed.body
        );
        let reported_status = text(&removed.body, "/status");
        assert_eq!(
            reported_status, "DELETING",
            "case {case:?}: the answer reports the endpoint in the status a removal puts it in, \
             got {}",
            removed.body
        );
        assert!(
            ENDPOINT_STATUS_VALUES.contains(&reported_status.as_str()),
            "case {case:?}: the status is an AgentRuntimeEndpointStatus value, got \
             {reported_status:?}"
        );
        assert_eq!(
            (
                text(&removed.body, "/agentRuntimeId"),
                text(&removed.body, "/endpointName")
            ),
            (runtime_id.clone(), target.clone()),
            "case {case:?}: the answer reports the endpoint it removed, got {}",
            removed.body
        );

        // The record is gone, so the read that addresses it is the endpoint-granularity 404.
        let gone = get_endpoint(&router, &runtime_id, &target).await;
        let sentence = endpoint_not_found(&target, &runtime_id);
        assert_eq!(
            (gone.status, gone.code.as_str(), text(&gone.body, "/message")),
            (404, "ResourceNotFoundException", sentence.clone()),
            "case {case:?}: a removed endpoint is not found, expected {sentence:?}, got {} {:?} \
             with {}",
            gone.status,
            gone.code,
            gone.body
        );

        // A second delete addresses an endpoint the store no longer holds, so it answers
        // exactly what the read does.
        let again = delete_endpoint(&router, &runtime_id, &target, row.client_token).await;
        assert_eq!(
            (
                again.status,
                again.code.as_str(),
                text(&again.body, "/message")
            ),
            (404, "ResourceNotFoundException", sentence.clone()),
            "case {case:?}: deleting it again reports the endpoint missing in the same sentence, \
             expected {sentence:?}, got {} {:?} with {}",
            again.status,
            again.code,
            again.body
        );

        // Nothing but that one endpoint moved: the listing drops it, every other endpoint
        // still reads back, and the runtime is untouched.
        let after = listed_endpoint_names(&router, &runtime_id, case).await;
        assert!(
            !after.contains(&target),
            "case {case:?}: the listing no longer reports {target:?}, got {after:?}"
        );
        assert!(
            after.contains(&MANAGED_ENDPOINT.to_string())
                && survivors.iter().all(|name| after.contains(name))
                && after.len() == survivors.len() + 1,
            "case {case:?}: the listing still reports every endpoint the delete did not name, \
             expected {survivors:?} beside {MANAGED_ENDPOINT:?}, got {after:?}"
        );
        for name in &survivors {
            let kept = get_endpoint(&router, &runtime_id, name).await;
            assert_eq!(
                (kept.status, text(&kept.body, "/name")),
                (200, name.clone()),
                "case {case:?}: the endpoint {name:?} the delete did not name reads back \
                 unchanged, got {} with {}",
                kept.status,
                kept.body
            );
        }
        let runtime = send(&router, "GET", &format!("/runtimes/{runtime_id}/"), None).await;
        assert_eq!(
            (runtime.status, text(&runtime.body, "/agentRuntimeVersion")),
            (200, FIRST_VERSION.to_string()),
            "case {case:?}: removing an endpoint leaves the runtime itself as it was, got {} with \
             {}",
            runtime.status,
            runtime.body
        );
        assert_ne!(
            text(&runtime.body, "/status"),
            "DELETING",
            "case {case:?}: removing an endpoint does not put its runtime on its way out, got {}",
            runtime.body
        );
    }
}

/// THE RULE, from the name's side: the name is what an endpoint's route and its ARN address
/// it by, so one runtime holds one endpoint per name — and a served delete frees that name,
/// turning the create the clash refused into a served one.
///
/// The rows differ along the shape of the name (the shortest the pattern admits, digits and
/// underscores, the 48-character bound) and along what the second create carries beyond the
/// name.
#[tokio::test]
async fn the_name_a_served_delete_frees_can_be_created_again() {
    let router = router();

    // `EndpointName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`.
    let longest_name = format!("n{}", "y".repeat(47));
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "the shortest name the pattern admits, re-created bare",
            "reuse_runtime_short",
            "e".to_string(),
            json!({}),
        ),
        (
            "a name carrying digits and underscores, re-created with a description",
            "reuse_runtime_underscores",
            "canary_v2_0".to_string(),
            json!({"description": "the second endpoint under this name"}),
        ),
        (
            "the 48-character bound, re-created with tags and an explicit version",
            "reuse_runtime_longest",
            longest_name,
            json!({"tags": {"env": "test"}, "agentRuntimeVersion": FIRST_VERSION}),
        ),
    ];

    for (case, runtime_name, name, extra) in rows {
        let runtime_id = create_runtime(&router, runtime_name).await;
        let mut first = json!({"name": name});
        first["description"] = json!("the first endpoint under this name");
        let created = create_endpoint(&router, &runtime_id, &first).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the first create must succeed for this row to say anything, got {} \
             with {}",
            created.status, created.body
        );

        // While the endpoint exists the name is taken, so a second create under it is the
        // modelled conflict rather than a second endpoint.
        let clash = create_endpoint(&router, &runtime_id, &first).await;
        assert_eq!(
            (
                clash.status,
                clash.code.as_str(),
                text(&clash.body, "/message")
            ),
            (409, "ConflictException", NAME_TAKEN_MESSAGE.to_string()),
            "case {case:?}: a repeated name on one runtime is the modelled 409, got {} {:?} with \
             {}",
            clash.status,
            clash.code,
            clash.body
        );

        let removed = delete_endpoint(&router, &runtime_id, &name, None).await;
        assert_eq!(
            removed.status, 202,
            "case {case:?}: the delete must be served for this row to say anything, got {} with {}",
            removed.status, removed.body
        );

        // The name the delete freed is available again, and what reads back under it is the
        // record the SECOND create persisted.
        let mut second = extra.clone();
        second["name"] = json!(name);
        let again = create_endpoint(&router, &runtime_id, &second).await;
        assert_eq!(
            again.status, 202,
            "case {case:?}: the name a served delete freed takes a new endpoint, got {} with {}",
            again.status, again.body
        );
        let read = get_endpoint(&router, &runtime_id, &name).await;
        assert_eq!(
            (read.status, text(&read.body, "/name")),
            (200, name.clone()),
            "case {case:?}: the re-created endpoint reads back under that name, got {} with {}",
            read.status,
            read.body
        );
        assert_eq!(
            text(&read.body, "/description"),
            text(&second, "/description"),
            "case {case:?}: the record read back is the second create's, got {}",
            read.body
        );
        assert_eq!(
            text(&read.body, "/targetVersion"),
            FIRST_VERSION,
            "case {case:?}: the re-created endpoint pins the version its runtime holds, got {}",
            read.body
        );
    }
}
