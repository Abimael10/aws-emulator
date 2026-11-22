// SPDX-License-Identifier: CC0-1.0

//! Which members `CreateHarness` REQUIRES, driven end to end over the wire path (a
//! `restJson1` request through the multi-service router into the mounted service), so what
//! is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: the members an operation requires are the ones its input shape marks
//! `@required`, and no others. `CreateHarnessRequest` marks `harnessName` and
//! `executionRoleArn` and nothing else (`smithy_query(query="shape",
//! shape="CreateHarnessRequest", model_path="services/\
//! service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/\
//! com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json")`), so a create
//! carrying just that pair — with any subset of the optional members beside it — is accepted
//! (201) and the harness converges to `READY`, while a create that omits one of the two IS
//! refused with the modelled `ValidationException` (400) naming it.
//!
//! A member the request omits is reported as what the stored harness holds for it: the empty
//! list where the response makes a collection `@required`, and nothing at all for `model`,
//! which AWS publishes no default for — never a value this emulator invented.
//!
//! The rows span what the rule generalises over: which optional members the create carries
//! (none at all, the tags of the recorded shape, the model, the prompt and tool lists, the
//! numeric bounds, the environment variables) and, for the refusals, which of the two
//! `@required` members is missing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/HarnessRole";

/// How long a row waits for the clock-owed `CREATING` -> `READY` transition before giving
/// up, and how long it waits between two looks. Generous: a row waits for the CONDITION, so
/// it finishes as soon as the transition is due.
const READY_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `HarnessStatus` members a harness this emulator drives reports
/// (`smithy_query(query="shape",shape="HarnessStatus")`).
const CREATING: &str = "CREATING";
const READY: &str = "READY";

/// One answer: the status, the error code and the parsed body.
struct Answer {
    status: u16,
    code: String,
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
        code: response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The member at `pointer`, or `Value::Null` when the body carries none.
fn at(body: &Value, pointer: &str) -> Value {
    body.pointer(pointer).cloned().unwrap_or(Value::Null)
}

/// A create body: the two `@required` members, plus whatever the row configures.
fn create_body(name: &str, extra: Value) -> Value {
    let mut body = json!({"harnessName": name, "executionRoleArn": ROLE_ARN});
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            body[key] = value;
        }
    }
    body
}

/// Read the harness until it reports `READY`, or fail once the generous deadline passes.
async fn read_until_ready(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let answer = send(router, "GET", &format!("/harnesses/{id}"), None).await;
        let harness = at(&answer.body, "/harness");
        if answer.status == 200 && harness["status"] == json!(READY) {
            return harness;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the harness never reached {READY}, last answer {} with {}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One accepted-create row: the name, the optional members beside the required pair, and
/// whether `model` is among them.
struct Row {
    case: &'static str,
    name: &'static str,
    extra: fn() -> Value,
    carries_model: bool,
}

/// RULE: a create carrying only the two `@required` members — with any subset of the
/// optional ones beside them — is ACCEPTED (201), persists what it carried, and converges to
/// `READY`; `model` is reported only when the request configured one.
#[tokio::test]
async fn a_create_is_accepted_without_the_members_the_model_leaves_optional() {
    let rows = [
        Row {
            case: "only the two required members",
            name: "barestHarness",
            extra: || json!({}),
            carries_model: false,
        },
        Row {
            case: "the required members and tags",
            name: "taggedHarness",
            extra: || json!({"tags": {"team": "agents", "env": "test"}}),
            carries_model: false,
        },
        Row {
            case: "the required members and a model",
            name: "modelledHarness",
            extra: || json!({"model": {"bedrockModelConfig": {"modelId": "amazon.nova-pro-v1:0"}}}),
            carries_model: true,
        },
        Row {
            case: "the required members, a system prompt and allowed tools",
            name: "promptedHarness",
            extra: || {
                json!({
                    "systemPrompt": [{"text": "be brief"}],
                    "allowedTools": ["@builtin"],
                })
            },
            carries_model: false,
        },
        Row {
            case: "the required members and the numeric bounds",
            name: "boundedHarness",
            extra: || json!({"maxIterations": 4, "maxTokens": 256, "timeoutSeconds": 30}),
            carries_model: false,
        },
        Row {
            case: "the required members and environment variables",
            name: "environedHarness",
            extra: || json!({"environmentVariables": {"LOG_LEVEL": "debug"}}),
            carries_model: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let extra = (row.extra)();
        let created = send(
            &router,
            "POST",
            "/harnesses",
            Some(&create_body(row.name, extra.clone())),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: a create carrying every @required member is accepted, got {} {:?} \
             with {}",
            created.status, created.code, created.body
        );
        let harness = at(&created.body, "/harness");
        assert_eq!(
            harness["harnessName"],
            json!(row.name),
            "case {case:?}: the create reports the name it persisted, got {harness}"
        );
        assert_eq!(
            harness["executionRoleArn"],
            json!(ROLE_ARN),
            "case {case:?}: the create reports the role it persisted, got {harness}"
        );
        assert_eq!(
            harness["status"],
            json!(CREATING),
            "case {case:?}: a new harness is created, not ready, got {harness}"
        );
        assert_eq!(
            harness.get("model").is_some(),
            row.carries_model,
            "case {case:?}: model is reported only when the request configured one, got {harness}"
        );
        if row.carries_model {
            assert_eq!(
                harness["model"], extra["model"],
                "case {case:?}: the model reported is the one the request carried, got {harness}"
            );
        }
        // Every member the response makes `@required` while the request leaves it optional is
        // reported as the empty collection the caller asked for.
        for member in ["systemPrompt", "tools", "skills", "allowedTools"] {
            assert!(
                harness[member].is_array(),
                "case {case:?}: {member} is reported as a list, got {harness}"
            );
        }

        // The round trip: the read reports what the create persisted, and the harness
        // converges to READY on the clock.
        let id = harness["harnessId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create reports a harnessId, got {harness}"))
            .to_string();
        let settled = read_until_ready(&router, case, &id).await;
        assert_eq!(
            settled["harnessId"], harness["harnessId"],
            "case {case:?}: the read reports the harness the create minted, got {settled}"
        );
        assert_eq!(
            settled.get("model").is_some(),
            row.carries_model,
            "case {case:?}: the read reports model exactly as the create did, got {settled}"
        );
        for (member, value) in extra.as_object().into_iter().flatten() {
            if member == "tags" {
                // `tags` is a request-only member: no response shape of this family reports
                // the harness's tags back.
                continue;
            }
            assert_eq!(
                settled[member], *value,
                "case {case:?}: the read reports the {member} the create persisted, got {settled}"
            );
        }
    }
}

/// RULE: the members the operation refuses a create for are exactly the `@required` ones, so
/// a create omitting `harnessName` or `executionRoleArn` is the modelled 400 naming it, and
/// no harness is created.
#[tokio::test]
async fn a_create_omitting_a_required_member_is_refused_and_creates_nothing() {
    let router = router();

    // (case, the body, the member the refusal must name)
    let rows: &[(&str, Value, &str)] = &[
        (
            "no harnessName",
            json!({"executionRoleArn": ROLE_ARN}),
            "harnessName",
        ),
        (
            "no executionRoleArn",
            json!({"harnessName": "incompleteHarness"}),
            "executionRoleArn",
        ),
        ("neither of them", json!({}), "harnessName"),
    ];

    for (case, body, member) in rows {
        let answer = send(&router, "POST", "/harnesses", Some(body)).await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a create missing a @required member is the modelled 400, got {} {:?} \
             with {}",
            answer.status,
            answer.code,
            answer.body
        );
        assert!(
            answer.body["message"]
                .as_str()
                .unwrap_or_default()
                .contains(member),
            "case {case:?}: the refusal names {member}, got {}",
            answer.body
        );
    }

    // Nothing a refused create left behind: the name one of them carried is still this
    // account's to use, which it would not be if the refused create had stored a harness.
    let created = send(
        &router,
        "POST",
        "/harnesses",
        Some(&create_body("incompleteHarness", json!({}))),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "a refused create leaves no harness behind, so its name is free, got {} with {}",
        created.status, created.body
    );
}
