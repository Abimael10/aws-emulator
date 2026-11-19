// SPDX-License-Identifier: CC0-1.0

//! The window an ACCEPTED `DeleteAgentRuntime` is under way in, driven end to end over the
//! wire path (a `restJson1` request through the multi-service router into the mounted
//! service), so what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: `DeleteAgentRuntime` answers 202 carrying `status`
//! (`AgentRuntimeStatus`), so it ACCEPTS the deletion rather than carrying it out — the
//! runtime stays readable and reports `DELETING` until the removal the clock owes it comes
//! due. In that window a change asked of the runtime is refused with `UpdateAgentRuntime`'s
//! modelled `ConflictException` (HTTP 409) naming the runtime, because a client has to tell
//! "busy, retry later" from "gone, stop"; once the removal is due every read of the id is
//! the modelled `ResourceNotFoundException` (404), the same change is that 404, and the name
//! the runtime reserved is this account's to use again.
//!
//! A version the runtime KEEPS is not part of that window: `DELETE …?version=` on a runtime
//! holding other versions removes that version at once and leaves the runtime itself alone.
//!
//! The rows span what the rule generalises over: whether the delete addresses the runtime as
//! a whole or names its only version, how many versions the runtime published, and which
//! operation probes the window (a read, a listing, an update).
//!
//! Sources: `smithy_query(query="operation",operation="DeleteAgentRuntime,UpdateAgentRuntime,\
//! GetAgentRuntime,ListAgentRuntimes",model_path="services/\
//! service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/\
//! com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json")` for the
//! routes, the success statuses and the modelled errors.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// A `roleArn` inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DeletionWindowRole";

/// How long a poll loop waits for the clock-owed removal before giving up, and how long it
/// waits between two looks.
///
/// The deadline is generous: a row waits for the CONDITION it is about — the runtime being
/// gone — so it finishes as soon as the removal is due, and the deadline only catches a
/// removal that never comes at all.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The statuses and codes the rows assert, as the model spells them
/// (`AgentRuntimeStatus`, and the `__type` / `x-amzn-errortype` the protocol carries).
const DELETING: &str = "DELETING";
const CONFLICT: &str = "ConflictException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// One answer: the status, the error code, the `message` and the whole body.
struct Answer {
    status: u16,
    code: String,
    message: String,
    body: Value,
}

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and what the
/// handlers read the ARN Region from.
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
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status,
        code,
        message: parsed
            .pointer("/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body: parsed,
    }
}

/// A `CreateAgentRuntime` / `UpdateAgentRuntime` body carrying the artifact `tag` names.
fn runtime_body(name: Option<&str>, tag: &str) -> Value {
    let mut body = json!({
        "agentRuntimeArtifact": {"containerConfiguration": {
            "containerUri": format!("public.ecr.aws/agentcore/demo:{tag}")
        }},
        "roleArn": ROLE_ARN,
        "networkConfiguration": {"networkMode": "PUBLIC"},
    });
    if let Some(name) = name {
        body["agentRuntimeName"] = json!(name);
    }
    body
}

/// Create one runtime named `name` and answer (the id, the ARN) the service minted.
async fn create(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let answer = send(router, "PUT", "/runtimes/", Some(&runtime_body(Some(name), "v1"))).await;
    assert_eq!(
        answer.status, 202,
        "case {case:?}: setup CreateAgentRuntime must succeed, got {} with {}",
        answer.status, answer.body
    );
    (
        answer.body["agentRuntimeId"]
            .as_str()
            .expect("a create answers the id it minted")
            .to_string(),
        answer.body["agentRuntimeArn"]
            .as_str()
            .expect("a create answers the ARN it minted")
            .to_string(),
    )
}

/// Publish one more version of the runtime.
async fn publish(router: &MultiServiceRouter, case: &str, id: &str, tag: &str) -> Answer {
    let answer = send(
        router,
        "PUT",
        &format!("/runtimes/{id}/"),
        Some(&runtime_body(None, tag)),
    )
    .await;
    assert_eq!(
        answer.status, 202,
        "case {case:?}: setup UpdateAgentRuntime must succeed, got {} with {}",
        answer.status, answer.body
    );
    answer
}

/// `GET /runtimes/{agentRuntimeId}/`.
async fn read(router: &MultiServiceRouter, id: &str) -> Answer {
    send(router, "GET", &format!("/runtimes/{id}/"), None).await
}

/// The sentence a change asked of a runtime whose deletion is under way is refused with: the
/// fixed text, with the runtime's own ARN as its only variable part.
fn update_conflict(agent_runtime_arn: &str) -> String {
    format!(
        "Cannot update agent {agent_runtime_arn} while it's {DELETING}. Please try again when \
         the agent is ready."
    )
}

/// Read the runtime until it is GONE — the modelled 404 — or fail once the generous deadline
/// passes.
async fn read_until_gone(router: &MultiServiceRouter, case: &str, id: &str) -> Answer {
    let deadline = Instant::now() + REMOVAL_DEADLINE;
    loop {
        let answer = read(router, id).await;
        if answer.status == 404 {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted runtime never went away, last answer {} with {}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: how the delete addresses the runtime, and how many versions it published first.
struct Row {
    case: &'static str,
    name: &'static str,
    /// Extra versions published past the create's version 1.
    extra_versions: usize,
    /// Whether the delete names the runtime's latest version instead of the runtime.
    by_version: bool,
}

/// RULE: an accepted delete leaves the runtime readable in `DELETING`, a change asked of it
/// in that window is the modelled 409 naming the runtime, and only once the clock has made
/// the removal due is every read — and that same change — the modelled 404.
#[tokio::test]
async fn an_accepted_delete_leaves_the_runtime_deleting_before_it_is_gone() {
    let rows = [
        Row {
            case: "the delete addresses the runtime, one version published",
            name: "window_one_version",
            extra_versions: 0,
            by_version: false,
        },
        Row {
            case: "the delete addresses the runtime, three versions published",
            name: "window_three_versions",
            extra_versions: 2,
            by_version: false,
        },
        Row {
            case: "the delete names the runtime's only version",
            name: "window_only_version",
            extra_versions: 0,
            by_version: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let (id, arn) = create(&router, case, row.name).await;
        for extra in 0..row.extra_versions {
            publish(&router, case, &id, &format!("v{}", extra + 2)).await;
        }
        let latest = read(&router, &id).await;
        let latest_version = latest.body["agentRuntimeVersion"]
            .as_str()
            .expect("a read reports the version it resolved")
            .to_string();

        let uri = if row.by_version {
            format!("/runtimes/{id}/?version={latest_version}")
        } else {
            format!("/runtimes/{id}/")
        };
        let deleted = send(&router, "DELETE", &uri, None).await;
        assert_eq!(
            (deleted.status, deleted.body["status"].clone()),
            (202, json!(DELETING)),
            "case {case:?}: the delete is accepted and reports DELETING, got {} with {}",
            deleted.status,
            deleted.body
        );

        // The runtime is still THERE, and it reports the status of a resource going away —
        // on the read and in the listing both.
        let during = read(&router, &id).await;
        assert_eq!(
            (during.status, during.body["status"].clone()),
            (200, json!(DELETING)),
            "case {case:?}: the runtime stays readable as DELETING, got {} with {}",
            during.status,
            during.body
        );
        let listed = send(&router, "POST", "/runtimes/", None).await;
        assert_eq!(
            listed.body["agentRuntimes"][0]["status"],
            json!(DELETING),
            "case {case:?}: the listing reports the runtime going away, got {}",
            listed.body
        );

        // A change asked of it in that window is the modelled CONFLICT, naming the runtime,
        // and it publishes nothing.
        let conflict = send(
            &router,
            "PUT",
            &format!("/runtimes/{id}/"),
            Some(&runtime_body(None, "conflict")),
        )
        .await;
        assert_eq!(
            (conflict.status, conflict.code.as_str()),
            (409, CONFLICT),
            "case {case:?}: a change asked of a runtime being deleted is the modelled 409, got \
             {} {:?} with {}",
            conflict.status,
            conflict.code,
            conflict.body
        );
        let expected = update_conflict(&arn);
        assert_eq!(
            conflict.message, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            conflict.message
        );
        let after_conflict = read(&router, &id).await;
        assert_eq!(
            after_conflict.body["agentRuntimeVersion"],
            latest.body["agentRuntimeVersion"],
            "case {case:?}: the refused change published no version, got {}",
            after_conflict.body
        );

        // Once the clock has made the removal due the runtime is gone: the read, and the
        // same change, are the modelled 404.
        let gone = read_until_gone(&router, case, &id).await;
        assert_eq!(
            gone.code, NOT_FOUND,
            "case {case:?}: a removed runtime is the modelled 404, got {} {:?} with {}",
            gone.status, gone.code, gone.body
        );
        let after = send(
            &router,
            "PUT",
            &format!("/runtimes/{id}/"),
            Some(&runtime_body(None, "after")),
        )
        .await;
        assert_eq!(
            (after.status, after.code.as_str()),
            (404, NOT_FOUND),
            "case {case:?}: once the runtime is gone the change is the modelled 404, got {} \
             {:?} with {}",
            after.status,
            after.code,
            after.body
        );
        let listed = send(&router, "POST", "/runtimes/", None).await;
        assert_eq!(
            listed.body["agentRuntimes"],
            json!([]),
            "case {case:?}: the removed runtime is in no listing, got {}",
            listed.body
        );

        // And the name it reserved is this account's to use again.
        let recreated = send(
            &router,
            "PUT",
            "/runtimes/",
            Some(&runtime_body(Some(row.name), "again")),
        )
        .await;
        assert_eq!(
            recreated.status, 202,
            "case {case:?}: the name is free once the runtime is gone, got {} with {}",
            recreated.status, recreated.body
        );
        assert_ne!(
            recreated.body["agentRuntimeId"], json!(id),
            "case {case:?}: the re-create mints a NEW runtime, got {}",
            recreated.body
        );
    }
}

/// RULE: only the RUNTIME's own removal is owed to the clock — a version the runtime keeps is
/// gone the moment its delete is served, and the runtime itself is untouched: it stays
/// readable at the version that remains and still takes a change.
#[tokio::test]
async fn deleting_a_version_the_runtime_keeps_leaves_the_runtime_alone() {
    let case = "two versions published, the older one deleted";
    let router = router();
    let (id, _) = create(&router, case, "window_kept_versions").await;
    publish(&router, case, &id, "v2").await;

    let deleted = send(&router, "DELETE", &format!("/runtimes/{id}/?version=1"), None).await;
    assert_eq!(
        (deleted.status, deleted.body["agentRuntimeVersion"].clone()),
        (202, json!("1")),
        "case {case:?}: the delete reports the version it removed, got {} with {}",
        deleted.status,
        deleted.body
    );

    let version = send(&router, "GET", &format!("/runtimes/{id}/?version=1"), None).await;
    assert_eq!(
        (version.status, version.code.as_str()),
        (404, NOT_FOUND),
        "case {case:?}: the deleted version is gone at once, got {} {:?} with {}",
        version.status,
        version.code,
        version.body
    );

    let kept = read(&router, &id).await;
    assert_eq!(
        (kept.status, kept.body["agentRuntimeVersion"].clone()),
        (200, json!("2")),
        "case {case:?}: the runtime keeps the version that was not deleted, got {} with {}",
        kept.status,
        kept.body
    );
    assert_ne!(
        kept.body["status"], json!(DELETING),
        "case {case:?}: the runtime itself is not going away, got {}",
        kept.body
    );

    let published = send(
        &router,
        "PUT",
        &format!("/runtimes/{id}/"),
        Some(&runtime_body(None, "v3")),
    )
    .await;
    assert_eq!(
        published.status, 202,
        "case {case:?}: the runtime still takes a change, got {} with {}",
        published.status, published.body
    );
}
