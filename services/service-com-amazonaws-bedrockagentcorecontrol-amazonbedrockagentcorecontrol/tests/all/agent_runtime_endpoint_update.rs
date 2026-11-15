// SPDX-License-Identifier: CC0-1.0

//! `UpdateAgentRuntimeEndpoint` over HTTP, end to end.
//!
//! One rule, read off the Smithy model rather than off any one recorded request: an
//! update changes a STORED endpoint in place — only the members the request names — and
//! starts a clock-owed `UPDATING` -> `READY` shift onto the runtime version it points
//! at, during which the endpoint keeps serving the version it was serving. So
//!
//! * the request's `agentRuntimeVersion` becomes the endpoint's `targetVersion`, and a
//!   request naming none leaves the pin where it was; `description` behaves the same way;
//! * while the shift is in flight the endpoint reports `UPDATING` with the PREVIOUS
//!   version as its `liveVersion`, and once the transition is due it reports `READY`
//!   serving the version it now points at;
//! * one endpoint serves one version at a time, so a change asked for while a shift — or
//!   the create — is still in flight is the modelled `409 ConflictException`, and a retry
//!   carrying the `clientToken` that started the shift is answered with that shift rather
//!   than refused;
//! * the endpoint is addressed only THROUGH its runtime: a runtime this account does not
//!   hold, an endpoint name that runtime does not hold and an `agentRuntimeVersion` the
//!   runtime never published are each the modelled `404 ResourceNotFoundException`.
//!
//! Sources: `smithy_query(query="operation",operation="UpdateAgentRuntimeEndpoint,\
//! CreateAgentRuntimeEndpoint,GetAgentRuntimeEndpoint,CreateAgentRuntime,\
//! UpdateAgentRuntime")` for the routes, the success statuses, the members and the
//! errors, and `smithy_query(query="shape",shape="AgentRuntimeEndpointStatus,\
//! AgentRuntimeVersion,AgentEndpointDescription,ClientToken")` for the status values and
//! the version, description and token grammars the rows below are built from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentRole";

/// The `AgentRuntimeVersion` a created runtime carries: the grammar `^([1-9][0-9]{0,4})$`
/// starts at 1, and only an update of the runtime publishes another.
const FIRST_VERSION: &str = "1";

/// Every `AgentRuntimeEndpointStatus` value
/// (`smithy_query shape=AgentRuntimeEndpointStatus`).
const ENDPOINT_STATUS_VALUES: [&str; 7] = [
    "CREATING",
    "CREATE_FAILED",
    "UPDATING",
    "UPDATE_FAILED",
    "READY",
    "DELETING",
    "DELETE_FAILED",
];

/// How long a row waits for a clock-owed transition before giving up, and how often it
/// looks again. The transition is due a short span after the call that starts it; this
/// deadline is far longer, so a loaded or slow host cannot fail a row — the row waits for
/// the CONDITION, never for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// An `agentRuntimeId` inside `AgentRuntimeId`'s pattern
/// `^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$` that no row ever creates.
const ABSENT_RUNTIME_ID: &str = "noSuchRuntime-abcdefghij";

/// The highest `agentRuntimeVersion` the wire can carry: `AgentRuntimeVersion` is
/// `^([1-9][0-9]{0,4})$`, so five digits — and no row publishes anywhere near it.
const UNPUBLISHED_HIGHEST_VERSION: &str = "99999";

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

/// One answer: the status and the parsed body.
struct Answer {
    status: u16,
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
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The member at `pointer`, or `Value::Null` when the answer carries none.
fn at(body: &Value, pointer: &str) -> Value {
    body.pointer(pointer).cloned().unwrap_or(Value::Null)
}

fn text(body: &Value, pointer: &str) -> String {
    at(body, pointer).as_str().unwrap_or_default().to_string()
}

/// A timestamp member, as `restJson1` renders `DateTimestamp` on the wire.
fn instant(body: &Value, pointer: &str) -> f64 {
    at(body, pointer).as_f64().unwrap_or(f64::NAN)
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A container artifact whose URI satisfies `RuntimeContainerUri`'s ECR pattern.
fn container_artifact(tag: &str) -> Value {
    json!({"containerConfiguration": {
        "containerUri": format!("public.ecr.aws/agentcore/demo:{tag}")
    }})
}

/// Create one agent runtime and return its `agentRuntimeId`.
async fn create_runtime(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "agentRuntimeName": name,
        "agentRuntimeArtifact": container_artifact("v1"),
        "roleArn": ROLE_ARN,
    });
    let answer = send(router, "PUT", "/runtimes/", Some(&body)).await;
    assert_eq!(
        answer.status, 202,
        "creating the runtime {name:?} must succeed for this suite to say anything, got {} with {}",
        answer.status, answer.body
    );
    text(&answer.body, "/agentRuntimeId")
}

/// Publish one more version of `runtime_id` and return the version it published.
async fn publish_version(router: &MultiServiceRouter, runtime_id: &str, tag: &str) -> String {
    let body = json!({
        "agentRuntimeArtifact": container_artifact(tag),
        "roleArn": ROLE_ARN,
    });
    let answer = send(router, "PUT", &format!("/runtimes/{runtime_id}/"), Some(&body)).await;
    assert_eq!(
        answer.status, 202,
        "publishing another runtime version must succeed for this suite to say anything, got {} \
         with {}",
        answer.status, answer.body
    );
    text(&answer.body, "/agentRuntimeVersion")
}

/// `PUT /runtimes/{agentRuntimeId}/runtime-endpoints/` — the route the model binds
/// `CreateAgentRuntimeEndpoint` at.
async fn create_endpoint(router: &MultiServiceRouter, runtime_id: &str, body: &Value) -> Answer {
    send(
        router,
        "PUT",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
        Some(body),
    )
    .await
}

/// `PUT /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route the
/// model binds `UpdateAgentRuntimeEndpoint` at.
async fn update_endpoint(
    router: &MultiServiceRouter,
    runtime_id: &str,
    name: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PUT",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
        Some(body),
    )
    .await
}

/// `GET /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route the
/// model binds `GetAgentRuntimeEndpoint` at.
async fn get_endpoint(router: &MultiServiceRouter, runtime_id: &str, name: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
        None,
    )
    .await
}

/// Read the endpoint until it reports `READY`, or fail once the generous deadline passes.
///
/// The transition is the clock's, so the row LOOKS AGAIN rather than sleeping for a span:
/// nothing here depends on how fast the host runs.
async fn read_until_ready(
    router: &MultiServiceRouter,
    runtime_id: &str,
    name: &str,
    case: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let answer = get_endpoint(router, runtime_id, name).await;
        let status = text(&answer.body, "/status");
        if status == "READY" {
            return answer.body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the endpoint never reached READY, last status {status:?} with {}",
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One update row: what the runtime publishes, what the endpoint is pinned to, what the
/// update names, and what the endpoint points at and reads back afterwards.
struct Shift {
    case: &'static str,
    /// How many versions the runtime publishes past its first.
    extra_versions: u32,
    /// The `agentRuntimeVersion` the CREATE pins the endpoint to, if it names one.
    created_version: Option<&'static str>,
    /// The description the CREATE persists, if it carries one.
    created_description: Option<String>,
    /// What the UPDATE's body carries.
    update: Value,
    /// The `targetVersion` the endpoint points at once the update is served.
    expected_version: &'static str,
    /// The description the endpoint reports once the update is served.
    expected_description: Option<String>,
}

/// An update points the endpoint at the version it names, keeps serving the previous one
/// until the clock-owed shift is due, and leaves every member the request does not name
/// where it was.
///
/// The rows differ along how many versions the runtime has published, which of them the
/// endpoint was pinned to, whether the update names a version at all (and whether it
/// names a later or an earlier one), and whether it names a description (the shortest and
/// the longest `AgentEndpointDescription`'s 1..256 admits).
#[tokio::test]
async fn an_update_points_the_endpoint_at_the_version_it_names() {
    let rows: Vec<Shift> = vec![
        Shift {
            case: "shifting onto the version just published",
            extra_versions: 1,
            created_version: Some(FIRST_VERSION),
            created_description: None,
            update: json!({"agentRuntimeVersion": "2"}),
            expected_version: "2",
            expected_description: None,
        },
        Shift {
            case: "naming no version, so the pin stays and only the description changes",
            extra_versions: 1,
            created_version: None,
            created_description: Some("the endpoint as created".to_string()),
            update: json!({"description": "x"}),
            expected_version: "2",
            expected_description: Some("x".to_string()),
        },
        Shift {
            case: "shifting two versions ahead with a description at the 256 bound",
            extra_versions: 2,
            created_version: Some(FIRST_VERSION),
            created_description: Some("short".to_string()),
            update: json!({"agentRuntimeVersion": "3", "description": "d".repeat(256)}),
            expected_version: "3",
            expected_description: Some("d".repeat(256)),
        },
        Shift {
            case: "shifting BACK onto an earlier version, naming no description",
            extra_versions: 1,
            created_version: Some("2"),
            created_description: Some("kept across the shift".to_string()),
            update: json!({"agentRuntimeVersion": FIRST_VERSION}),
            expected_version: FIRST_VERSION,
            expected_description: Some("kept across the shift".to_string()),
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let runtime_id = create_runtime(&router, "shifting_endpoint_runtime").await;
        for revision in 0..row.extra_versions {
            publish_version(&router, &runtime_id, &format!("v{}", revision + 2)).await;
        }

        let mut created_body = json!({"name": "live"});
        if let Some(version) = row.created_version {
            created_body["agentRuntimeVersion"] = json!(version);
        }
        if let Some(description) = &row.created_description {
            created_body["description"] = json!(description);
        }
        let created = create_endpoint(&router, &runtime_id, &created_body).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the create must succeed for this row to say anything, got {} with {}",
            created.status, created.body
        );
        let pinned_at_create = text(&created.body, "/targetVersion");
        let settled = read_until_ready(&router, &runtime_id, "live", case).await;
        assert_eq!(
            text(&settled, "/liveVersion"),
            pinned_at_create,
            "case {case:?}: the endpoint must serve the version it was created against before \
             the shift, got {settled}"
        );

        let shifted = update_endpoint(&router, &runtime_id, "live", &row.update).await;
        assert_eq!(
            shifted.status, 202,
            "case {case:?}: updating a settled endpoint is HTTP 202, got {} with {}",
            shifted.status, shifted.body
        );
        let answer = shifted.body;
        assert_eq!(
            text(&answer, "/status"),
            "UPDATING",
            "case {case:?}: the shift onto the version is the clock's, so the update reports \
             UPDATING, got {answer}"
        );
        assert!(
            ENDPOINT_STATUS_VALUES.contains(&text(&answer, "/status").as_str()),
            "case {case:?}: the status is an AgentRuntimeEndpointStatus value, got {answer}"
        );
        assert_eq!(
            text(&answer, "/targetVersion"),
            row.expected_version,
            "case {case:?}: the endpoint points at the version the update named, got {answer}"
        );
        assert_eq!(
            text(&answer, "/liveVersion"),
            pinned_at_create,
            "case {case:?}: the endpoint keeps serving the previous version while the shift is \
             in flight, got {answer}"
        );
        let runtime_arn = format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:runtime/{runtime_id}");
        assert_eq!(
            (
                text(&answer, "/agentRuntimeArn"),
                text(&answer, "/agentRuntimeEndpointArn")
            ),
            (
                runtime_arn.clone(),
                format!("{runtime_arn}/runtime-endpoint/live")
            ),
            "case {case:?}: an update leaves the endpoint's identity where it was, got {answer}"
        );
        assert_eq!(
            instant(&answer, "/createdAt"),
            instant(&settled, "/createdAt"),
            "case {case:?}: an update does not re-create the endpoint, got {answer}"
        );
        assert!(
            instant(&answer, "/lastUpdatedAt") >= instant(&answer, "/createdAt"),
            "case {case:?}: the update is reported no earlier than the create, got {answer}"
        );

        // The read reports the record the update wrote: the new pin whatever the status,
        // and the version the endpoint serves at the status it is in.
        let read = get_endpoint(&router, &runtime_id, "live").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the endpoint is readable after the update, got {} with {}",
            read.status, read.body
        );
        let during = read.body;
        assert_eq!(
            text(&during, "/targetVersion"),
            row.expected_version,
            "case {case:?}: the read reports the pin the update persisted, got {during}"
        );
        let status_during = text(&during, "/status");
        let serving_during = if status_during == "UPDATING" {
            pinned_at_create.clone()
        } else {
            row.expected_version.to_string()
        };
        assert_eq!(
            text(&during, "/liveVersion"),
            serving_during,
            "case {case:?}: an endpoint in {status_during:?} serves {serving_during:?}, got \
             {during}"
        );

        let shifted = read_until_ready(&router, &runtime_id, "live", case).await;
        assert_eq!(
            text(&shifted, "/liveVersion"),
            row.expected_version,
            "case {case:?}: once the shift is due the endpoint serves the version it points at, \
             got {shifted}"
        );
        assert_eq!(
            text(&shifted, "/targetVersion"),
            text(&shifted, "/liveVersion"),
            "case {case:?}: a settled endpoint serves the version it points to, got {shifted}"
        );
        assert_eq!(
            at(&shifted, "/description"),
            row.expected_description
                .as_ref()
                .map_or(Value::Null, |description| json!(description)),
            "case {case:?}: an update changes only the members it names, got {shifted}"
        );
        assert_eq!(
            at(&shifted, "/failureReason"),
            Value::Null,
            "case {case:?}: a shift the clock drives to READY has no failure, got {shifted}"
        );
    }
}

/// The conflict message `UpdateAgentRuntimeEndpoint` is refused with while a change is
/// still in flight, for the status the request found.
fn in_flight_message(status: &str) -> String {
    format!(
        "The endpoint is currently being modified by another operation. Current status: \
         {status}. Wait and try again."
    )
}

/// One endpoint serves one version at a time: a change asked for while another is still
/// in flight is refused, and the record the refused request named is left alone.
///
/// The rows differ along which change is in flight — the create, or an earlier shift —
/// and so along the status the refusal reports.
#[tokio::test]
async fn a_change_still_in_flight_refuses_another() {
    // (case, whether the row waits for the create to settle before shifting, the status
    // the second update finds, the version the endpoint still points at afterwards)
    let rows: [(&str, bool, &str, &str); 2] = [
        ("while the create is still in flight", false, "CREATING", FIRST_VERSION),
        ("while an earlier shift is still in flight", true, "UPDATING", "2"),
    ];

    for (case, settle_first, blocking_status, pinned) in rows {
        let router = router();
        let runtime_id = create_runtime(&router, "busy_endpoint_runtime").await;
        publish_version(&router, &runtime_id, "v2").await;
        let created = create_endpoint(
            &router,
            &runtime_id,
            &json!({"name": "live", "agentRuntimeVersion": FIRST_VERSION}),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the create must succeed for this row to say anything, got {} with {}",
            created.status, created.body
        );

        if settle_first {
            read_until_ready(&router, &runtime_id, "live", case).await;
            let first = update_endpoint(&router, &runtime_id, "live", &json!({"agentRuntimeVersion": "2"})).await;
            assert_eq!(
                first.status, 202,
                "case {case:?}: the first shift must be served for this row to say anything, got \
                 {} with {}",
                first.status, first.body
            );
        }

        let refused = update_endpoint(
            &router,
            &runtime_id,
            "live",
            &json!({"agentRuntimeVersion": FIRST_VERSION, "description": "a second change"}),
        )
        .await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (409, "ConflictException".to_string()),
            "case {case:?}: a change asked for while another is in flight is the modelled 409, \
             got {} with {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            text(&refused.body, "/message"),
            in_flight_message(blocking_status),
            "case {case:?}: the refusal names the status the request found, got {}",
            refused.body
        );

        let read = get_endpoint(&router, &runtime_id, "live").await;
        assert_eq!(
            text(&read.body, "/targetVersion"),
            pinned,
            "case {case:?}: the refused change left the endpoint's pin where it was, got {}",
            read.body
        );
        assert_eq!(
            at(&read.body, "/description"),
            Value::Null,
            "case {case:?}: the refused change persisted nothing, got {}",
            read.body
        );
    }
}

/// One refusal row: the update that is refused and the answer it must get.
struct Refusal {
    case: &'static str,
    runtime_id: String,
    endpoint_name: String,
    body: Value,
    code: &'static str,
    status: u16,
    message: String,
}

/// An endpoint is updated only THROUGH the runtime that holds it, and only onto a version
/// that runtime published.
///
/// The rows differ along what the request names that nothing stored answers to: a runtime
/// id nothing created, an endpoint name its runtime does not hold, another runtime's
/// endpoint name, the version just past the runtime's latest and the highest version the
/// grammar admits.
#[tokio::test]
async fn an_update_is_addressed_only_through_a_stored_runtime_and_version() {
    let router = router();
    let owner = create_runtime(&router, "owner_runtime").await;
    let other = create_runtime(&router, "other_runtime").await;
    let created = create_endpoint(&router, &owner, &json!({"name": "onlyHere"})).await;
    assert_eq!(
        created.status, 202,
        "the create must succeed for this suite to say anything, got {} with {}",
        created.status, created.body
    );
    read_until_ready(&router, &owner, "onlyHere", "the endpoint every refusal row shares").await;

    let rows: Vec<Refusal> = vec![
        Refusal {
            case: "a runtime nothing created",
            runtime_id: ABSENT_RUNTIME_ID.to_string(),
            endpoint_name: "onlyHere".to_string(),
            body: json!({}),
            code: "ResourceNotFoundException",
            status: 404,
            message: format!("Agent with agentId: {ABSENT_RUNTIME_ID} not found"),
        },
        Refusal {
            case: "an endpoint name its runtime does not hold",
            runtime_id: owner.clone(),
            endpoint_name: "neverCreated".to_string(),
            body: json!({}),
            code: "ResourceNotFoundException",
            status: 404,
            message: "Agent endpoint with name neverCreated not found.".to_string(),
        },
        Refusal {
            case: "another runtime's endpoint name",
            runtime_id: other.clone(),
            endpoint_name: "onlyHere".to_string(),
            body: json!({}),
            code: "ResourceNotFoundException",
            status: 404,
            message: "Agent endpoint with name onlyHere not found.".to_string(),
        },
        Refusal {
            case: "the version just past the runtime's latest",
            runtime_id: owner.clone(),
            endpoint_name: "onlyHere".to_string(),
            body: json!({"agentRuntimeVersion": "2"}),
            code: "ResourceNotFoundException",
            status: 404,
            message: format!("Agent with agentId: {owner} not found"),
        },
        Refusal {
            case: "the highest version the grammar admits",
            runtime_id: owner.clone(),
            endpoint_name: "onlyHere".to_string(),
            body: json!({"agentRuntimeVersion": UNPUBLISHED_HIGHEST_VERSION}),
            code: "ResourceNotFoundException",
            status: 404,
            message: format!("Agent with agentId: {owner} not found"),
        },
    ];

    for row in rows {
        let case = row.case;
        let refused =
            update_endpoint(&router, &row.runtime_id, &row.endpoint_name, &row.body).await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (row.status, row.code.to_string()),
            "case {case:?}: expected {} {}, got {} with {}",
            row.status,
            row.code,
            refused.status,
            refused.body
        );
        assert_eq!(
            text(&refused.body, "/message"),
            row.message,
            "case {case:?}: expected the refusal {:?}, got {}",
            row.message,
            refused.body
        );
    }

    // Nothing above changed the endpoint: it still points at the version it was created
    // against, and it is still readable.
    let read = get_endpoint(&router, &owner, "onlyHere").await;
    assert_eq!(
        (read.status, text(&read.body, "/targetVersion")),
        (200, FIRST_VERSION.to_string()),
        "a refused update persists nothing, got {} with {}",
        read.status,
        read.body
    );
}

/// A retry carrying the `clientToken` that started a shift is answered with THAT shift,
/// rather than refused as a second change or served as another one.
///
/// The rows differ along the length of the token: the shortest `ClientToken`'s 33..256
/// admits and a longer one.
#[tokio::test]
async fn a_repeated_client_token_replays_the_shift_it_started() {
    // `ClientToken` is len 33..256, pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let rows: [(&str, String); 2] = [
        ("the shortest token the grammar admits", format!("t{}", "0".repeat(32))),
        ("a longer token with dashes", format!("shift-{}a", "a-".repeat(29))),
    ];

    for (case, token) in rows {
        let router = router();
        let runtime_id = create_runtime(&router, "replayed_endpoint_runtime").await;
        publish_version(&router, &runtime_id, "v2").await;
        let created = create_endpoint(
            &router,
            &runtime_id,
            &json!({"name": "live", "agentRuntimeVersion": FIRST_VERSION}),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the create must succeed for this row to say anything, got {} with {}",
            created.status, created.body
        );
        read_until_ready(&router, &runtime_id, "live", case).await;

        let body = json!({"agentRuntimeVersion": "2", "clientToken": token});
        let first = update_endpoint(&router, &runtime_id, "live", &body).await;
        assert_eq!(
            first.status, 202,
            "case {case:?}: the first shift is HTTP 202, got {} with {}",
            first.status, first.body
        );
        let replay = update_endpoint(&router, &runtime_id, "live", &body).await;
        assert_eq!(
            replay.status, 202,
            "case {case:?}: a retry carrying the token that started the shift is served, not \
             refused, got {} with {}",
            replay.status, replay.body
        );
        assert_eq!(
            (
                text(&replay.body, "/targetVersion"),
                text(&replay.body, "/liveVersion"),
                text(&replay.body, "/agentRuntimeEndpointArn"),
                instant(&replay.body, "/lastUpdatedAt"),
            ),
            (
                text(&first.body, "/targetVersion"),
                text(&first.body, "/liveVersion"),
                text(&first.body, "/agentRuntimeEndpointArn"),
                instant(&first.body, "/lastUpdatedAt"),
            ),
            "case {case:?}: the retry is answered with the shift the token started, got {}",
            replay.body
        );

        let settled = read_until_ready(&router, &runtime_id, "live", case).await;
        assert_eq!(
            (
                text(&settled, "/targetVersion"),
                text(&settled, "/liveVersion")
            ),
            ("2".to_string(), "2".to_string()),
            "case {case:?}: the two requests started ONE shift, got {settled}"
        );
    }
}
