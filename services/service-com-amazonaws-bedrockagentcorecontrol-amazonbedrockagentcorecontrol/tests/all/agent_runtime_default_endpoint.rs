// SPDX-License-Identifier: CC0-1.0

//! The endpoint an agent runtime is CREATED with, over HTTP, end to end.
//!
//! Three rules, each asserted over a table of inputs rather than over one recorded
//! request:
//!
//! * a runtime is created WITH a service-managed endpoint under the reserved name
//!   [`MANAGED_ENDPOINT`], which exists from the create's answer onward and TRACKS the
//!   runtime's newest published version — its `targetVersion` and `liveVersion` move onto
//!   every version the runtime publishes, with no endpoint call at all — while an endpoint
//!   a caller created stays pinned to the version it named;
//! * removing the RUNTIME is refused with the modelled `409 ConflictException` while an
//!   endpoint a caller created is still attached, and the runtime is left exactly as it
//!   was; the managed endpoint is this service's own, so it never blocks the removal, and
//!   a runtime whose caller-created endpoints have been deleted is removed;
//! * a change asked for against an endpoint resolves what the request REFERENCES first: an
//!   `agentRuntimeVersion` the runtime never published is the modelled `404
//!   ResourceNotFoundException` whatever status the endpoint is in, and the endpoint keeps
//!   the version it pointed at; the in-flight conflict is reserved for a request whose
//!   references all resolve.
//!
//! Sources: `smithy_query(query="operation",operation="CreateAgentRuntime,\
//! UpdateAgentRuntime,DeleteAgentRuntime,CreateAgentRuntimeEndpoint,\
//! GetAgentRuntimeEndpoint,UpdateAgentRuntimeEndpoint,DeleteAgentRuntimeEndpoint,\
//! ListAgentRuntimeEndpoints")` for the routes, the success statuses, the members and the
//! modelled errors, and `smithy_query(query="shape",shape="AgentRuntimeVersion,\
//! EndpointName,AgentRuntimeEndpointStatus,AgentRuntimeEndpointArn,\
//! AgentRuntimeEndpointId")` for the version and name grammars, the status values and the
//! ARN and id forms asserted below.

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

/// The name of the endpoint the service manages for every agent runtime. It is inside
/// `EndpointName`'s pattern `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`, so it is addressable by the
/// same routes a caller-created endpoint is.
const MANAGED_ENDPOINT: &str = "DEFAULT";

/// How this service words the refusal of a runtime removal while an endpoint is attached.
const HAS_ENDPOINTS_MESSAGE: &str =
    "This agent has endpoints that must be deleted before the agent can be removed.";

/// How long a row waits for a clock-owed transition before giving up, and how often it
/// looks again. The transition is due a short span after the call that starts it; this
/// deadline is far longer, so a loaded or slow host cannot fail a row — every row waits
/// for the CONDITION, never for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

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
        "networkConfiguration": {"networkMode": "PUBLIC"},
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

/// `DELETE /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/` — the route the
/// model binds `DeleteAgentRuntimeEndpoint` at.
async fn delete_endpoint(router: &MultiServiceRouter, runtime_id: &str, name: &str) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/{name}/"),
        None,
    )
    .await
}

/// `DELETE /runtimes/{agentRuntimeId}/` — the route the model binds `DeleteAgentRuntime`
/// at, with the modelled `version` query when `version` names one.
async fn delete_runtime(
    router: &MultiServiceRouter,
    runtime_id: &str,
    version: Option<&str>,
) -> Answer {
    let uri = match version {
        None => format!("/runtimes/{runtime_id}/"),
        Some(version) => format!("/runtimes/{runtime_id}/?version={version}"),
    };
    send(router, "DELETE", &uri, None).await
}

/// `POST /runtimes/{agentRuntimeId}/runtime-endpoints/` — the route the model binds
/// `ListAgentRuntimeEndpoints` at. Returns the names it reports.
async fn listed_endpoint_names(router: &MultiServiceRouter, runtime_id: &str) -> Vec<String> {
    let answer = send(
        router,
        "POST",
        &format!("/runtimes/{runtime_id}/runtime-endpoints/"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "listing the endpoints of a stored runtime is HTTP 200, got {} with {}",
        answer.status, answer.body
    );
    at(&answer.body, "/runtimeEndpoints")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| text(entry, "/name"))
        .collect()
}

/// Read the endpoint until it reports `READY`, or fail once the generous deadline passes.
///
/// The transition is the clock's, so the row LOOKS AGAIN rather than waiting a fixed span:
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
        if answer.status == 200 && text(&answer.body, "/status") == "READY" {
            return answer.body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the endpoint {name:?} never reached READY, last answer {} with {}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the tracking table: how many versions the runtime publishes past its first,
/// and the endpoint a caller pins to the first version beside the managed one.
struct Tracked {
    case: &'static str,
    runtime: &'static str,
    /// How many further versions the runtime publishes, one call each.
    publishes: usize,
    /// The endpoint the caller creates, pinned to the runtime's first version.
    pinned_endpoint: &'static str,
}

/// The managed endpoint exists from the create and tracks every version the runtime
/// publishes; the endpoint a caller created stays on the version it pinned.
///
/// The rows differ along the number of versions published after the create (none, one,
/// several), which is what the rule generalises over, and along the name the caller's own
/// endpoint carries.
#[tokio::test]
async fn the_managed_endpoint_tracks_every_published_version() {
    let router = router();

    let rows = [
        Tracked {
            case: "no version published past the create",
            runtime: "tracking_runtime_none",
            publishes: 0,
            pinned_endpoint: "p",
        },
        Tracked {
            case: "one further version published",
            runtime: "tracking_runtime_one",
            publishes: 1,
            pinned_endpoint: "pinned_endpoint_1",
        },
        Tracked {
            case: "three further versions published",
            runtime: "tracking_runtime_many",
            publishes: 3,
            pinned_endpoint: "Pinned_To_The_First_Version",
        },
    ];

    for row in rows {
        let Tracked {
            case,
            runtime,
            publishes,
            pinned_endpoint,
        } = row;
        let runtime_id = create_runtime(&router, runtime).await;

        // The managed endpoint is readable from the create's own answer onward, under the
        // reserved name, with no endpoint call at all.
        let fresh = get_endpoint(&router, &runtime_id, MANAGED_ENDPOINT).await;
        assert_eq!(
            fresh.status, 200,
            "case {case:?}: the endpoint the create establishes is readable at once, got {} with {}",
            fresh.status, fresh.body
        );
        assert_eq!(
            text(&fresh.body, "/targetVersion"),
            FIRST_VERSION,
            "case {case:?}: the managed endpoint tracks the version the create published, got {}",
            fresh.body
        );
        assert_eq!(
            text(&fresh.body, "/agentRuntimeEndpointArn"),
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:runtime/{runtime_id}/\
                 runtime-endpoint/{MANAGED_ENDPOINT}"
            ),
            "case {case:?}: the managed endpoint carries the ARN its name gives it under its \
             runtime, got {}",
            fresh.body
        );
        assert!(
            text(&fresh.body, "/id").starts_with(&format!("{MANAGED_ENDPOINT}-")),
            "case {case:?}: the managed endpoint carries an AgentRuntimeEndpointId of its own, \
             got {}",
            fresh.body
        );

        // The caller's own endpoint pins the version it named.
        let created = create_endpoint(
            &router,
            &runtime_id,
            &json!({"name": pinned_endpoint, "agentRuntimeVersion": FIRST_VERSION}),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: creating the caller's endpoint must succeed for this row to say \
             anything, got {} with {}",
            created.status, created.body
        );

        let settled = read_until_ready(&router, &runtime_id, MANAGED_ENDPOINT, case).await;
        assert_eq!(
            (
                text(&settled, "/targetVersion"),
                text(&settled, "/liveVersion")
            ),
            (FIRST_VERSION.to_string(), FIRST_VERSION.to_string()),
            "case {case:?}: a settled managed endpoint serves the version it tracks, got {settled}"
        );

        // Every publish moves the managed endpoint onto the version it published, and
        // leaves the caller's pin where it was.
        for publish in 0..publishes {
            let published = publish_version(&router, &runtime_id, &format!("v{}", publish + 2)).await;
            let tracked = read_until_ready(&router, &runtime_id, MANAGED_ENDPOINT, case).await;
            assert_eq!(
                (
                    text(&tracked, "/targetVersion"),
                    text(&tracked, "/liveVersion")
                ),
                (published.clone(), published.clone()),
                "case {case:?}: the managed endpoint moves onto the version {published:?} the \
                 publish produced, got {tracked}"
            );
            let pinned = get_endpoint(&router, &runtime_id, pinned_endpoint).await;
            assert_eq!(
                text(&pinned.body, "/targetVersion"),
                FIRST_VERSION,
                "case {case:?}: the endpoint the caller pinned stays on the version it named \
                 while {published:?} is published, got {}",
                pinned.body
            );
        }

        // Both endpoints are the runtime's, so the listing reports them together.
        let mut expected = vec![MANAGED_ENDPOINT.to_string(), pinned_endpoint.to_string()];
        expected.sort();
        let listed = listed_endpoint_names(&router, &runtime_id).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: the listing reports the managed endpoint beside the caller's, got \
             {listed:?}"
        );
    }
}

/// One row of the removal table: the endpoints a caller creates on the runtime, and the
/// `version` query the delete carries.
struct Removal {
    case: &'static str,
    runtime: &'static str,
    /// The endpoints the caller creates, by name.
    caller_endpoints: &'static [&'static str],
    /// The modelled `version` query the delete names, if any.
    version: Option<&'static str>,
}

/// Removing a runtime is refused while an endpoint a caller created is attached, the
/// runtime is left exactly as it was, and the removal is served once those endpoints are
/// deleted — the managed endpoint never standing in the way.
///
/// The rows differ along how many endpoints the caller created and the shape of their
/// names, and along whether the delete addresses the runtime as a whole or names its only
/// version in the modelled `version` query.
#[tokio::test]
async fn a_caller_created_endpoint_blocks_the_runtime_removal_and_the_managed_one_does_not() {
    let router = router();

    let rows = [
        Removal {
            case: "one endpoint, delete addressing the runtime",
            runtime: "removal_runtime_one",
            caller_endpoints: &["prod"],
            version: None,
        },
        Removal {
            case: "three endpoints, delete addressing the runtime",
            runtime: "removal_runtime_three",
            caller_endpoints: &["beta", "canary", "live_v2_endpoint_42"],
            version: None,
        },
        Removal {
            case: "one endpoint, delete naming the runtime's only version",
            runtime: "removal_runtime_versioned",
            caller_endpoints: &["e"],
            version: Some(FIRST_VERSION),
        },
    ];

    for row in rows {
        let Removal {
            case,
            runtime,
            caller_endpoints,
            version,
        } = row;
        let runtime_id = create_runtime(&router, runtime).await;
        for name in caller_endpoints {
            let created = create_endpoint(&router, &runtime_id, &json!({"name": name})).await;
            assert_eq!(
                created.status, 202,
                "case {case:?}: creating {name:?} must succeed for this row to say anything, got \
                 {} with {}",
                created.status, created.body
            );
        }

        let refused = delete_runtime(&router, &runtime_id, version).await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (409, "ConflictException".to_string()),
            "case {case:?}: a runtime still holding an endpoint a caller created is not removed, \
             got {} with {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            text(&refused.body, "/message"),
            HAS_ENDPOINTS_MESSAGE,
            "case {case:?}: the refusal carries this service's sentence for it, got {}",
            refused.body
        );

        // The refusal wrote nothing: the runtime is still readable at its version, and so
        // is every endpoint on it.
        let read = send(&router, "GET", &format!("/runtimes/{runtime_id}/"), None).await;
        assert_eq!(
            (read.status, text(&read.body, "/agentRuntimeVersion")),
            (200, FIRST_VERSION.to_string()),
            "case {case:?}: the refused delete leaves the runtime exactly as it was, got {} with \
             {}",
            read.status,
            read.body
        );
        assert_ne!(
            text(&read.body, "/status"),
            "DELETING",
            "case {case:?}: a refused delete does not put the runtime on its way out, got {}",
            read.body
        );
        for name in caller_endpoints {
            let endpoint = get_endpoint(&router, &runtime_id, name).await;
            assert_eq!(
                endpoint.status, 200,
                "case {case:?}: the endpoint {name:?} the refusal named is still there, got {} \
                 with {}",
                endpoint.status, endpoint.body
            );
        }

        // Deleting the caller's endpoints is what clears the way; the managed endpoint is
        // still attached and the removal is served anyway.
        for name in caller_endpoints {
            let removed = delete_endpoint(&router, &runtime_id, name).await;
            assert_eq!(
                (removed.status, text(&removed.body, "/status")),
                (202, "DELETING".to_string()),
                "case {case:?}: deleting the endpoint {name:?} is accepted, got {} with {}",
                removed.status,
                removed.body
            );
            let gone = get_endpoint(&router, &runtime_id, name).await;
            assert_eq!(
                (gone.status, error_code(&gone.body)),
                (404, "ResourceNotFoundException".to_string()),
                "case {case:?}: the endpoint {name:?} is gone once its delete is served, got {} \
                 with {}",
                gone.status,
                gone.body
            );
        }
        assert_eq!(
            listed_endpoint_names(&router, &runtime_id).await,
            vec![MANAGED_ENDPOINT.to_string()],
            "case {case:?}: the managed endpoint is what the runtime still holds",
        );

        let removed = delete_runtime(&router, &runtime_id, version).await;
        assert_eq!(
            (removed.status, text(&removed.body, "/status")),
            (202, "DELETING".to_string()),
            "case {case:?}: a runtime holding only the managed endpoint is removed, got {} with {}",
            removed.status,
            removed.body
        );
        let read = send(&router, "GET", &format!("/runtimes/{runtime_id}/"), None).await;
        assert_eq!(
            (read.status, error_code(&read.body)),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: the removed runtime is not found, got {} with {}",
            read.status,
            read.body
        );
    }
}

/// One row of the reference table: the `agentRuntimeVersion` an update names, and whether
/// the endpoint it addresses has settled when the update arrives.
struct Referenced {
    case: &'static str,
    runtime: &'static str,
    /// The version the update names. Every value is inside `AgentRuntimeVersion`'s own
    /// grammar `^([1-9][0-9]{0,4})$` (len 1..5), so the handler is what refuses it.
    version: &'static str,
    /// Whether the row waits for the endpoint's clock-owed `READY` before updating.
    settled: bool,
}

/// An `agentRuntimeVersion` the runtime never published is the modelled 404 whatever
/// status the endpoint is in: what a request REFERENCES is resolved before the endpoint's
/// own in-flight state is gated on, and the endpoint keeps the version it pointed at.
///
/// The rows differ along the shape of the unpublished version (the next number up, a
/// five-digit value, the highest the grammar admits) and along whether the endpoint has
/// settled, which is what decides the conflict the gate would otherwise answer.
#[tokio::test]
async fn an_unpublished_version_is_not_found_whatever_the_endpoint_status() {
    let router = router();

    let rows = [
        Referenced {
            case: "the next version number up, endpoint still settling",
            runtime: "reference_runtime_next_creating",
            version: "2",
            settled: false,
        },
        Referenced {
            case: "a five-digit version, endpoint still settling",
            runtime: "reference_runtime_wide_creating",
            version: "88888",
            settled: false,
        },
        Referenced {
            case: "the highest version the grammar admits, endpoint settled",
            runtime: "reference_runtime_highest_ready",
            version: "99999",
            settled: true,
        },
        Referenced {
            case: "the next version number up, endpoint settled",
            runtime: "reference_runtime_next_ready",
            version: "2",
            settled: true,
        },
    ];

    for row in rows {
        let Referenced {
            case,
            runtime,
            version,
            settled,
        } = row;
        let runtime_id = create_runtime(&router, runtime).await;
        let name = "shifting_endpoint";
        let created = create_endpoint(
            &router,
            &runtime_id,
            &json!({"name": name, "agentRuntimeVersion": FIRST_VERSION}),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: creating the endpoint must succeed for this row to say anything, got \
             {} with {}",
            created.status, created.body
        );
        if settled {
            read_until_ready(&router, &runtime_id, name, case).await;
        }

        let refused = update_endpoint(
            &router,
            &runtime_id,
            name,
            &json!({"agentRuntimeVersion": version}),
        )
        .await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: a version the runtime never published is not found, got {} with {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            text(&refused.body, "/message"),
            format!("Agent with agentId: {runtime_id} not found"),
            "case {case:?}: the refusal names the agent the version was looked for on, got {}",
            refused.body
        );

        // The refusal wrote nothing: the endpoint still points where it did.
        let read = get_endpoint(&router, &runtime_id, name).await;
        assert_eq!(
            (read.status, text(&read.body, "/targetVersion")),
            (200, FIRST_VERSION.to_string()),
            "case {case:?}: the refused update leaves the endpoint on the version it pinned, got \
             {} with {}",
            read.status,
            read.body
        );
    }
}

/// The in-flight conflict is reserved for a request whose references all resolve: a shift
/// onto a version the runtime DID publish, asked for while the endpoint's create is still
/// in flight, is the modelled 409 — and the same request is served once the endpoint has
/// settled.
#[tokio::test]
async fn a_published_version_asked_for_in_flight_is_the_modelled_conflict() {
    let router = router();
    let case = "a published version while the create is in flight";
    let runtime_id = create_runtime(&router, "reference_runtime_conflict").await;
    let published = publish_version(&router, &runtime_id, "v2").await;
    let name = "contended_endpoint";
    let created = create_endpoint(
        &router,
        &runtime_id,
        &json!({"name": name, "agentRuntimeVersion": FIRST_VERSION}),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: creating the endpoint must succeed for this test to say anything, got {} \
         with {}",
        created.status, created.body
    );

    let refused = update_endpoint(
        &router,
        &runtime_id,
        name,
        &json!({"agentRuntimeVersion": published}),
    )
    .await;
    assert_eq!(
        (refused.status, error_code(&refused.body)),
        (409, "ConflictException".to_string()),
        "case {case:?}: a second shift asked for while one is in flight is the modelled conflict, \
         got {} with {}",
        refused.status,
        refused.body
    );

    read_until_ready(&router, &runtime_id, name, case).await;
    let served = update_endpoint(
        &router,
        &runtime_id,
        name,
        &json!({"agentRuntimeVersion": published}),
    )
    .await;
    assert_eq!(
        (served.status, text(&served.body, "/targetVersion")),
        (202, published.clone()),
        "case {case:?}: the same shift is served once the endpoint has settled, got {} with {}",
        served.status,
        served.body
    );
}
