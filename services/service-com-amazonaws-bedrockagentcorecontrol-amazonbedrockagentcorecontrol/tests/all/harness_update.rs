// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE of a harness, and of one of its endpoints, changes — driven end to end
//! over the wire path (a `restJson1` request through the multi-service router into the
//! mounted service), so what is asserted is what a client sees.
//!
//! THE RULES UNDER TEST, read off the model rather than off any one recorded request
//! (`smithy_query(query="shape",shape="UpdateHarnessRequest,UpdateHarnessEndpointRequest,\
//! UpdatedHarnessEnvironmentArtifact,UpdatedAuthorizerConfiguration,\
//! UpdatedHarnessMemoryConfiguration,Harness,HarnessEndpoint",model_path="services/\
//! service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/\
//! com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json")`):
//!
//! 1. PATCH SEMANTICS. A member the update names replaces the stored one; a member it omits
//!    is "retained when not specified"; and the three `Updated*` wrappers decide a member of
//!    their own — the wrapper carrying `optionalValue` SETS the member, the wrapper carrying
//!    none CLEARS it, and the wrapper absent leaves it unchanged.
//! 2. THE VERSION AND THE CLOCK. A served update publishes the next `harnessVersion` and the
//!    harness reports `UPDATING` until the transition the clock owes it is due, then `READY`.
//! 3. THE ENDPOINT FOLLOWS. `UpdateHarnessEndpoint` points the endpoint at the harness
//!    version it names and reports `UPDATING`; while it is in flight it serves the version it
//!    served before (`liveVersion` lags `targetVersion`), and a SECOND change asked of it in
//!    that window is the modelled `ConflictException` (409) naming the endpoint and the
//!    status, with nothing written.
//! 4. WHAT IS NOT THERE. A harness this account does not hold, and an endpoint name a held
//!    harness does not carry, are each the modelled `ResourceNotFoundException` (404).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// Role ARNs inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`: the
/// one a harness is created with and the one an update moves it to.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/HarnessRole";
const OTHER_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OtherHarnessRole";

/// How long a row waits for a clock-owed transition before giving up, and how long it waits
/// between two looks. Generous: a row waits for the CONDITION, so it finishes as soon as the
/// transition is due.
const READY_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `HarnessStatus` / `HarnessEndpointStatus` members this emulator drives.
const CREATING: &str = "CREATING";
const UPDATING: &str = "UPDATING";
const READY: &str = "READY";

/// A `harnessId` inside `HarnessId`'s pattern that no row ever creates.
const ABSENT_HARNESS_ID: &str = "noSuchHarness-abcdefghij";

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

/// Create one harness carrying every member the rows later change, and answer its id.
async fn create(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = json!({
        "harnessName": name,
        "executionRoleArn": ROLE_ARN,
        "model": {"bedrockModelConfig": {"modelId": "amazon.nova-pro-v1:0"}},
        "systemPrompt": [{"text": "be brief"}],
        "allowedTools": ["@builtin"],
        "maxIterations": 4,
        "environmentArtifact": {"containerConfiguration": {
            "containerUri": "public.ecr.aws/agentcore/harness:v1"
        }},
    });
    let answer = send(router, "POST", "/harnesses", Some(&body)).await;
    assert_eq!(
        answer.status, 201,
        "case {case:?}: setup CreateHarness must succeed, got {} with {}",
        answer.status, answer.body
    );
    at(&answer.body, "/harness/harnessId")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create reports a harnessId, got {}", answer.body))
        .to_string()
}

/// `PATCH /harnesses/{harnessId}` — the route the model binds `UpdateHarness` at.
async fn update(router: &MultiServiceRouter, id: &str, body: &Value) -> Answer {
    send(router, "PATCH", &format!("/harnesses/{id}"), Some(body)).await
}

/// `GET /harnesses/{harnessId}`.
async fn read(router: &MultiServiceRouter, id: &str) -> Answer {
    send(router, "GET", &format!("/harnesses/{id}"), None).await
}

/// `POST /harnesses/{harnessId}/endpoints` — the route `CreateHarnessEndpoint` is bound at.
async fn create_endpoint(router: &MultiServiceRouter, id: &str, name: &str) -> Answer {
    send(
        router,
        "POST",
        &format!("/harnesses/{id}/endpoints"),
        Some(&json!({"endpointName": name})),
    )
    .await
}

/// `PATCH /harnesses/{harnessId}/endpoints/{endpointName}`.
async fn update_endpoint(
    router: &MultiServiceRouter,
    id: &str,
    name: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/harnesses/{id}/endpoints/{name}"),
        Some(body),
    )
    .await
}

/// `GET /harnesses/{harnessId}/endpoints/{endpointName}`.
async fn read_endpoint(router: &MultiServiceRouter, id: &str, name: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/harnesses/{id}/endpoints/{name}"),
        None,
    )
    .await
}

/// Read the harness until it reports `READY`, or fail once the generous deadline passes.
async fn harness_until_ready(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let answer = read(router, id).await;
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

/// Read the endpoint until it reports `READY`, or fail once the generous deadline passes.
async fn endpoint_until_ready(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    name: &str,
) -> Value {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let answer = read_endpoint(router, id, name).await;
        let endpoint = at(&answer.body, "/endpoint");
        if answer.status == 200 && endpoint["status"] == json!(READY) {
            return endpoint;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint never reached {READY}, last answer {} with {}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One PATCH row: the body the update sends, the member it is about, and what the stored
/// harness must report for that member afterwards.
struct Patch {
    case: &'static str,
    body: fn() -> Value,
    member: &'static str,
    /// `None` asserts the member is reported by nothing: the update CLEARED it.
    expected: fn() -> Option<Value>,
}

/// RULES 1 and 2: a member the update names replaces the stored one, a member it omits is
/// retained, a PATCH wrapper carrying no value clears the member it is about — and every
/// served update publishes the next version and leaves the harness `UPDATING` until the
/// clock owes it `READY`.
#[tokio::test]
async fn an_update_replaces_what_it_names_and_retains_what_it_omits() {
    let rows = [
        Patch {
            case: "a member the update names: the execution role",
            body: || json!({"executionRoleArn": OTHER_ROLE_ARN}),
            member: "executionRoleArn",
            expected: || Some(json!(OTHER_ROLE_ARN)),
        },
        Patch {
            case: "a member the update names: the model",
            body: || json!({"model": {"bedrockModelConfig": {"modelId": "amazon.nova-lite-v1:0"}}}),
            member: "model",
            expected: || Some(json!({"bedrockModelConfig": {"modelId": "amazon.nova-lite-v1:0"}})),
        },
        Patch {
            case: "a list the update replaces whole",
            body: || json!({"allowedTools": ["@builtin", "@serverName/toolName"]}),
            member: "allowedTools",
            expected: || Some(json!(["@builtin", "@serverName/toolName"])),
        },
        Patch {
            case: "a number the update replaces",
            body: || json!({"maxIterations": 9}),
            member: "maxIterations",
            expected: || Some(json!(9)),
        },
        Patch {
            case: "a member the update omits: the role stays as created",
            body: || json!({"maxTokens": 128}),
            member: "executionRoleArn",
            expected: || Some(json!(ROLE_ARN)),
        },
        Patch {
            case: "a PATCH wrapper carrying a value SETS the member",
            body: || {
                json!({"environmentArtifact": {"optionalValue": {"containerConfiguration": {
                    "containerUri": "public.ecr.aws/agentcore/harness:v2"
                }}}})
            },
            member: "environmentArtifact",
            expected: || {
                Some(json!({"containerConfiguration": {
                    "containerUri": "public.ecr.aws/agentcore/harness:v2"
                }}))
            },
        },
        Patch {
            case: "a PATCH wrapper carrying no value CLEARS the member",
            body: || json!({"environmentArtifact": {}}),
            member: "environmentArtifact",
            expected: || None,
        },
        Patch {
            case: "a wrapper the update omits leaves the member as created",
            body: || json!({"maxTokens": 64}),
            member: "environmentArtifact",
            expected: || {
                Some(json!({"containerConfiguration": {
                    "containerUri": "public.ecr.aws/agentcore/harness:v1"
                }}))
            },
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let id = create(&router, case, "patchedHarness").await;
        let created = harness_until_ready(&router, case, &id).await;
        assert_eq!(
            created["harnessVersion"],
            json!("1"),
            "case {case:?}: a created harness is at version 1, got {created}"
        );

        let answer = update(&router, &id, &(row.body)()).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the update is served, got {} {:?} with {}",
            answer.status, answer.code, answer.body
        );
        let updated = at(&answer.body, "/harness");
        assert_eq!(
            updated["harnessVersion"],
            json!("2"),
            "case {case:?}: a served update publishes the next version, got {updated}"
        );
        assert_eq!(
            updated["status"],
            json!(UPDATING),
            "case {case:?}: the harness is UPDATING until the clock owes it READY, got {updated}"
        );
        match (row.expected)() {
            Some(expected) => assert_eq!(
                updated[row.member], expected,
                "case {case:?}: the update reports the {} it stored, got {updated}",
                row.member
            ),
            None => assert!(
                updated.get(row.member).is_none(),
                "case {case:?}: the cleared {} is reported by nothing, got {updated}",
                row.member
            ),
        }

        // The round trip: the read reports exactly what the update persisted, and the
        // harness converges to READY on the clock.
        let settled = harness_until_ready(&router, case, &id).await;
        assert_eq!(
            settled["harnessVersion"],
            json!("2"),
            "case {case:?}: the read reports the version the update published, got {settled}"
        );
        match (row.expected)() {
            Some(expected) => assert_eq!(
                settled[row.member], expected,
                "case {case:?}: the read reports the {} the update persisted, got {settled}",
                row.member
            ),
            None => assert!(
                settled.get(row.member).is_none(),
                "case {case:?}: the read reports no {}, got {settled}",
                row.member
            ),
        }
    }
}

/// RULE 3: the endpoint follows the harness — a served endpoint update points it at the
/// harness version and reports `UPDATING`, it serves the version it served before until the
/// clock owes it `READY`, and a second change in that window is the modelled 409 naming the
/// endpoint and the status, with nothing written.
#[tokio::test]
async fn an_endpoint_update_is_in_flight_until_the_clock_owes_it_ready() {
    let case = "an endpoint repointed after the harness published a version";
    let router = router();
    let id = create(&router, case, "endpointedHarness").await;
    harness_until_ready(&router, case, &id).await;

    let created = create_endpoint(&router, &id, "live").await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: setup CreateHarnessEndpoint must succeed, got {} with {}",
        created.status, created.body
    );
    assert_eq!(
        at(&created.body, "/endpoint/status"),
        json!(CREATING),
        "case {case:?}: a new endpoint is created, not ready, got {}",
        created.body
    );
    let ready = endpoint_until_ready(&router, case, &id, "live").await;
    assert_eq!(
        (ready["targetVersion"].clone(), ready["liveVersion"].clone()),
        (json!("1"), json!("1")),
        "case {case:?}: a settled endpoint serves the version it points at, got {ready}"
    );

    // The harness publishes version 2, and the endpoint is pointed at it.
    let published = update(&router, &id, &json!({"maxTokens": 32})).await;
    assert_eq!(
        published.status, 200,
        "case {case:?}: the harness update is served, got {} with {}",
        published.status, published.body
    );
    harness_until_ready(&router, case, &id).await;

    // A member the update OMITS is retained: a description-only change, while the harness
    // has already moved on, leaves the endpoint pointing where it pointed and starts no
    // shift — it is still READY, still serving the version it served.
    let retained = update_endpoint(&router, &id, "live", &json!({"description": "renamed"})).await;
    assert_eq!(
        retained.status, 200,
        "case {case:?}: a description-only change is served, got {} {:?} with {}",
        retained.status, retained.code, retained.body
    );
    let kept = at(&retained.body, "/endpoint");
    assert_eq!(
        (
            kept["targetVersion"].clone(),
            kept["liveVersion"].clone(),
            kept["status"].clone(),
            kept["description"].clone(),
        ),
        (json!("1"), json!("1"), json!(READY), json!("renamed")),
        "case {case:?}: an omitted targetVersion is retained and starts no shift, got {kept}"
    );

    let repointed = update_endpoint(
        &router,
        &id,
        "live",
        &json!({"targetVersion": "2", "description": "the newest version"}),
    )
    .await;
    assert_eq!(
        repointed.status, 200,
        "case {case:?}: the endpoint update is served, got {} {:?} with {}",
        repointed.status, repointed.code, repointed.body
    );
    let endpoint = at(&repointed.body, "/endpoint");
    assert_eq!(
        (
            endpoint["status"].clone(),
            endpoint["targetVersion"].clone(),
            endpoint["description"].clone(),
        ),
        (json!(UPDATING), json!("2"), json!("the newest version")),
        "case {case:?}: the endpoint reports the shift it accepted, got {endpoint}"
    );
    assert_eq!(
        endpoint["liveVersion"], json!("1"),
        "case {case:?}: an endpoint in flight still serves the version it served, got {endpoint}"
    );

    // A SECOND change asked for in that window is the modelled conflict, and it writes
    // nothing.
    let conflict = update_endpoint(&router, &id, "live", &json!({"description": "again"})).await;
    assert_eq!(
        (conflict.status, conflict.code.as_str()),
        (409, "ConflictException"),
        "case {case:?}: a change asked of an endpoint in flight is the modelled 409, got {} {:?} \
         with {}",
        conflict.status,
        conflict.code,
        conflict.body
    );
    let expected = format!("Cannot update endpoint live while it is {UPDATING}. Wait and try again.");
    assert_eq!(
        conflict.body["message"], json!(expected),
        "case {case:?}: expected {expected:?}, got {}",
        conflict.body
    );
    let during = read_endpoint(&router, &id, "live").await;
    assert_eq!(
        at(&during.body, "/endpoint/description"),
        json!("the newest version"),
        "case {case:?}: the refused change wrote nothing, got {}",
        during.body
    );

    // Once the clock owes it READY the endpoint serves the version it was pointed at, and
    // another change is served again.
    let settled = endpoint_until_ready(&router, case, &id, "live").await;
    assert_eq!(
        (settled["targetVersion"].clone(), settled["liveVersion"].clone()),
        (json!("2"), json!("2")),
        "case {case:?}: the settled endpoint serves the version it was pointed at, got {settled}"
    );
    let again = update_endpoint(&router, &id, "live", &json!({"description": "and again"})).await;
    assert_eq!(
        again.status, 200,
        "case {case:?}: a settled endpoint takes another change, got {} with {}",
        again.status, again.body
    );
}

/// RULE 4: a harness this account does not hold, a version it never published, and an
/// endpoint name a held harness does not carry are each the modelled 404 — and nothing is
/// written.
#[tokio::test]
async fn an_update_of_something_that_is_not_there_is_the_modelled_404() {
    let case = "updates addressing what is not there";
    let router = router();
    let id = create(&router, case, "presentHarness").await;
    harness_until_ready(&router, case, &id).await;
    let created = create_endpoint(&router, &id, "live").await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: setup CreateHarnessEndpoint must succeed, got {} with {}",
        created.status, created.body
    );
    endpoint_until_ready(&router, case, &id, "live").await;

    // (case, the answer the row drove)
    let rows = vec![
        (
            "UpdateHarness on a harness this account does not hold",
            update(&router, ABSENT_HARNESS_ID, &json!({"maxTokens": 16})).await,
        ),
        (
            "UpdateHarnessEndpoint on a harness this account does not hold",
            update_endpoint(&router, ABSENT_HARNESS_ID, "live", &json!({"description": "x"})).await,
        ),
        (
            "UpdateHarnessEndpoint on an endpoint the harness does not carry",
            update_endpoint(&router, &id, "absent", &json!({"description": "x"})).await,
        ),
        (
            "UpdateHarnessEndpoint naming a version the harness never published",
            update_endpoint(&router, &id, "live", &json!({"targetVersion": "99999"})).await,
        ),
    ];

    for (row, answer) in rows {
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {row:?}: expected the modelled 404, got {} {:?} with {}",
            answer.status,
            answer.code,
            answer.body
        );
    }

    // The stored harness and its endpoint are untouched by every refusal above.
    let harness = read(&router, &id).await;
    assert_eq!(
        at(&harness.body, "/harness/harnessVersion"),
        json!("1"),
        "case {case:?}: no refused update published a version, got {}",
        harness.body
    );
    let endpoint = read_endpoint(&router, &id, "live").await;
    assert_eq!(
        at(&endpoint.body, "/endpoint/targetVersion"),
        json!("1"),
        "case {case:?}: no refused update moved the endpoint, got {}",
        endpoint.body
    );
}
