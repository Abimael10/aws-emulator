// SPDX-License-Identifier: CC0-1.0

//! `SendAutomationSignal` end to end: which STATE of an Automation execution admits a signal.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol") — and reads what the signal did
//! back through `GetAutomationExecution` and `DescribeAutomationStepExecutions`.
//!
//! The rule under test: a signal that carries a parked walk on (`Resume`, `StartStep`) is
//! applied only while the execution is WAITING at the step the payload names; in every other
//! state — before the service has picked the execution up, while another step is running, and
//! once the execution has ended — it is refused as the modelled
//! `InvalidAutomationSignalException` and the execution is left exactly as it was, with no
//! step moved and no status changed. The step the payload names still has to be one the
//! runbook declares, which is a fact of the document and not of the state
//! (`AutomationStepNotFoundException`); that refusal is covered beside this one in
//! `automation_step_actions`, which also covers the refusal of a signal to an execution that
//! has ENDED — the state axis this suite spans (not yet picked up, an earlier step running,
//! ended) is what it does not.
//!
//! The suites are TABLES: one row per state the execution can be in when the signal arrives,
//! crossed with the signal types that name a step and with the action the runbook parks at
//! (`aws:pause` and `aws:approve`), rather than the single runbook and single state a
//! recorded unit happened to use.
//!
//! An automation is `Pending` until the service picks it up and a waiting step holds it for
//! its own span, so the cases move the clock to those instants through the service's own
//! state ([`pick_up`], [`finish_waits`]) rather than sleeping for them, and then read the
//! execution until it has reached the state the row is about — nothing here depends on how
//! fast the host runs.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{automation, ComAmazonawsSsmAmazonssmState};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The modelled refusal of a signal the execution's own state does not admit
/// (`smithy_query(query="operation",operation="SendAutomationSignal")`).
const INVALID_SIGNAL: &str = "InvalidAutomationSignalException";

/// A wait no case lets the clock reach on its own, so the execution stands at the step for as
/// long as the row needs ([`finish_waits`] is what moves the clock to the end of it).
const A_LONG_WAIT: &str = "PT5M";

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case moves the clock with.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// Whether `status` is an `AutomationExecutionStatus` an execution never leaves again, as the
/// Automation slice itself decides it (`automation::is_terminal`).
fn terminal(status: &str) -> bool {
    automation::is_terminal(status)
}

/// Create an Automation runbook of `name` with `content`, asserting it succeeded.
async fn runbook(router: &MultiServiceRouter, name: &str, content: &Value) {
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook {name} must be created first, got {status} with {created:?}"
    );
}

/// Start an automation, asserting it succeeded, and answering the id it minted.
async fn started(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, response) = call(router, "StartAutomationExecution", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {response:?}"
    );
    text(&response, "AutomationExecutionId")
}

/// Read one execution back, asserting the read succeeded.
async fn read(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
    let (status, response) = call(
        router,
        "GetAutomationExecution",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAutomationExecution, got {status} with \
         {response:?}"
    );
    response["AutomationExecution"].clone()
}

/// The step executions `DescribeAutomationStepExecutions` reports, as `(name, status)` pairs
/// in the order the report lists them — the report a client reads a signal's effect from.
async fn reported_steps(
    router: &MultiServiceRouter,
    case: &str,
    execution_id: &str,
) -> Vec<(String, String)> {
    let (status, response) = call(
        router,
        "DescribeAutomationStepExecutions",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAutomationStepExecutions, got {status} \
         with {response:?}"
    );
    response["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|step| (text(step, "StepName"), text(step, "StepStatus")))
        .collect()
}

/// How many reads an execution is given to reach the state a row is about.
///
/// Each read performs what the clock already owes the execution, so one read is enough for
/// every runbook here; this is generous headroom for a loaded host, and every loop waits for
/// the CONDITION rather than for a span.
const READS_UNTIL_SETTLED: usize = 200;

/// An instant well in the past, which is what the clock is moved to below.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for
/// (`automation::PICKUP_DELAY`).
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// Let the clock reach the end of every wait a step of [`REGION`]'s executions is in.
fn finish_waits(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held != REGION {
            continue;
        }
        for step in &mut execution.steps {
            if step.due_at.is_some() {
                step.due_at = Some(long_ago());
            }
        }
    }
}

/// The step a runbook of this suite parks at, and the two steps around it: one that holds the
/// execution `InProgress` before it, and one that only a released park ever reaches.
const RUNNING_STEP: &str = "running";
const PARKED_STEP: &str = "parked";
const LATER_STEP: &str = "later";

/// A runbook that runs a long wait, then parks at `action`, then runs one more step: the three
/// states a signal can arrive in are all reachable on one document.
fn parking_runbook(action: &str) -> Value {
    json!({
        "schemaVersion": "0.3",
        "description": "Waits, parks for a signal, then runs one more step.",
        "mainSteps": [
            {"name": RUNNING_STEP, "action": "aws:sleep", "inputs": {"Duration": A_LONG_WAIT}},
            {"name": PARKED_STEP, "action": action, "inputs": {}},
            {"name": LATER_STEP, "action": "aws:sleep", "inputs": {"Duration": "PT0S"}},
        ],
    })
}

/// Create and start a [`parking_runbook`] of `action`, answering the execution id. NOTHING has
/// picked the execution up yet, so it is still `Pending`.
async fn started_parking_at(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    suffix: &str,
) -> String {
    // A document `Name` admits `[a-zA-Z0-9_\-.]` only
    // (`smithy_query(query="operation",operation="CreateDocument")`).
    let name = format!("Signal-{}-{suffix}", action.replace(':', "-"));
    runbook(router, &name, &parking_runbook(action)).await;
    started(router, case, &json!({"DocumentName": name})).await
}

/// Read `execution_id` until it reports `status`, and answer the execution it reported.
async fn until_status(
    router: &MultiServiceRouter,
    case: &str,
    execution_id: &str,
    status: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..READS_UNTIL_SETTLED {
        last = read(router, case, execution_id).await;
        if text(&last, "AutomationExecutionStatus") == status {
            return last;
        }
        ::tokio::task::yield_now().await;
    }
    panic!("case {case:?}: the execution never reported {status}, last read {last}")
}

/// Read `execution_id` until it reports `status`, letting the clock reach the end of every
/// wait a step is in between reads, and answer the execution it reported.
///
/// The wait a step is in only exists once the step has STARTED, which is what the read before
/// each nudge does, so the two alternate until the execution has reached the state the row is
/// about.
async fn until_status_nudged(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    execution_id: &str,
    status: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..READS_UNTIL_SETTLED {
        last = read(router, case, execution_id).await;
        if text(&last, "AutomationExecutionStatus") == status {
            return last;
        }
        finish_waits(engine);
        ::tokio::task::yield_now().await;
    }
    panic!("case {case:?}: the execution never reported {status}, last read {last}")
}

/// Send one signal, answering `(status, body)`.
async fn signal(
    router: &MultiServiceRouter,
    execution_id: &str,
    signal_type: &str,
    payload: Value,
) -> (u16, Value) {
    let mut body = json!({
        "AutomationExecutionId": execution_id,
        "SignalType": signal_type,
    });
    if !payload.is_null() {
        body["Payload"] = payload;
    }
    call(router, "SendAutomationSignal", &body).await
}

/// RULE: a signal that carries a parked walk on is applied only while the execution is WAITING
/// at the step the payload names; every other state refuses it as the modelled invalid signal
/// and leaves the execution exactly as it was.
///
/// One row per state the execution can be in when the signal arrives — not yet picked up,
/// running an earlier step, and ended — crossed with the two signal types that name a step and
/// with both actions a runbook parks at.
#[tokio::test]
async fn a_signal_is_refused_in_every_state_that_does_not_admit_it() {
    let (router, engine) = service();
    for action in ["aws:pause", "aws:approve"] {
        // The state each row puts the execution in before the signal arrives, and the status
        // the execution reports there.
        let rows: Vec<(&str, &str, bool)> = vec![
            ("before the service picked the execution up", "Pending", false),
            ("while an earlier step is running", "InProgress", true),
            ("after the execution has ended", "Cancelled", true),
        ];
        for (state, expected_status, picked_up) in rows {
            for signal_type in ["Resume", "StartStep"] {
                let case = format!("{state}, parked at {action}, signalled {signal_type}");
                let suffix = format!(
                    "{}-{signal_type}",
                    state
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .collect::<String>()
                );
                let id = started_parking_at(&router, &case, action, &suffix).await;
                if picked_up {
                    pick_up(&engine);
                }
                if expected_status == "Cancelled" {
                    let (stopped, body) = call(
                        &router,
                        "StopAutomationExecution",
                        &json!({"AutomationExecutionId": &id}),
                    )
                    .await;
                    assert_eq!(
                        stopped, 200,
                        "case {case:?}: the execution must be stoppable first, got {stopped} \
                         with {body:?}"
                    );
                }
                let before = until_status(&router, &case, &id, expected_status).await;
                let steps_before = reported_steps(&router, &case, &id).await;

                let (status, refused) = signal(
                    &router,
                    &id,
                    signal_type,
                    json!({"StepName": [PARKED_STEP]}),
                )
                .await;
                assert_eq!(
                    (status, error_code(&refused).as_str()),
                    (400, INVALID_SIGNAL),
                    "case {case:?}: expected the modelled invalid-signal refusal, got {status} \
                     with {refused}"
                );
                assert!(
                    !error_message(&refused).is_empty()
                        && error_message(&refused) != "<no message>",
                    "case {case:?}: the refusal must carry a message, got {refused}"
                );

                // The refusal applied NOTHING: the execution reports the state it was in and
                // every step reports the status it had — which is also what says no step after
                // the parked one was entered, since each of those was `Pending` before.
                let after = read(&router, &case, &id).await;
                assert_eq!(
                    text(&after, "AutomationExecutionStatus"),
                    text(&before, "AutomationExecutionStatus"),
                    "case {case:?}: the refused signal must leave the status alone, got {after}"
                );
                let steps_after = reported_steps(&router, &case, &id).await;
                assert_eq!(
                    steps_after, steps_before,
                    "case {case:?}: the refused signal must leave every step alone, got \
                     {steps_after:?}"
                );
                assert!(
                    steps_after
                        .iter()
                        .all(|(name, step_status)| name != LATER_STEP
                            || step_status != "Success"),
                    "case {case:?}: no step after the parked one may run for a refused signal, \
                     got {steps_after:?}"
                );
            }
        }
    }
}

/// RULE (the other side of the same rule): the state that DOES admit the signal applies it —
/// the parked step ends, the walk carries on to the step after it, and the execution reaches a
/// terminal status; and once it has, the same signal is refused again.
///
/// One row per action a runbook parks at, with the signal type that releases it.
#[tokio::test]
async fn the_state_that_admits_a_signal_carries_the_walk_on() {
    let (router, engine) = service();
    // (the action the runbook parks at, the signal that releases it, whether that signal
    // NAMES the step it is about through `Payload.StepName` — "for StartStep and Resume
    // signal types, you must send the name of the Automation step", while "for Approve and
    // Reject signal types, the payload is an optional comment"
    // (`SendAutomationSignalRequest$Payload`) — the status the parked step is left in, and
    // the status the execution reaches)
    let rows = [
        ("aws:pause", "Resume", true, "Success", "Success"),
        ("aws:approve", "Approve", false, "Success", "Success"),
        ("aws:approve", "Reject", false, "Failed", "Failed"),
    ];
    for (action, signal_type, names_a_step, parked_status, execution_status) in rows {
        let case = format!("parked at {action}, signalled {signal_type}");
        let payload = if names_a_step {
            json!({"StepName": [PARKED_STEP]})
        } else {
            json!({"Comment": ["the comment this signal may carry"]})
        };
        let id = started_parking_at(&router, &case, action, signal_type).await;
        pick_up(&engine);
        // The wait before the parked step is what keeps the execution out of the state that
        // admits the signal; letting the clock reach the end of it parks the walk.
        until_status_nudged(&router, &engine, &case, &id, "Waiting").await;

        // While it waits at the parked step, a signal that NAMES a step and names another
        // declared one is still refused: the state admits such a signal only for the step the
        // execution is parked at.
        if names_a_step {
            let (status, refused) =
                signal(&router, &id, signal_type, json!({"StepName": [LATER_STEP]})).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, INVALID_SIGNAL),
                "case {case:?}: a signal naming a step the execution is not parked at must be \
                 refused, got {status} with {refused}"
            );
        }

        let (status, applied) = signal(&router, &id, signal_type, payload.clone()).await;
        assert_eq!(
            (status, applied.clone()),
            (200, json!({})),
            "case {case:?}: expected an empty 200 for the signal the state admits, got \
             {status} with {applied}"
        );

        // Read the effect back: the parked step ended, the walk left it, and the execution
        // came to rest.
        let mut settled = Value::Null;
        for _ in 0..READS_UNTIL_SETTLED {
            settled = read(&router, &case, &id).await;
            if terminal(&text(&settled, "AutomationExecutionStatus")) {
                break;
            }
            finish_waits(&engine);
            ::tokio::task::yield_now().await;
        }
        assert_eq!(
            text(&settled, "AutomationExecutionStatus"),
            execution_status,
            "case {case:?}: expected the signalled execution to reach {execution_status}, got \
             {settled}"
        );
        let reported = reported_steps(&router, &case, &id).await;
        assert!(
            reported
                .iter()
                .any(|(name, step_status)| name == PARKED_STEP && step_status == parked_status),
            "case {case:?}: expected the parked step to end {parked_status}, got {reported:?}"
        );
        // A released `Success` carries the walk on to the step after the parked one; a
        // rejection ends the automation where it stood, so that step is never entered.
        let later = reported
            .iter()
            .find(|(name, _)| name == LATER_STEP)
            .map(|(_, step_status)| step_status.clone());
        let expected_later = if parked_status == "Success" {
            Some("Success".to_string())
        } else {
            Some("Pending".to_string())
        };
        assert_eq!(
            later, expected_later,
            "case {case:?}: expected the step after the parked one to be {expected_later:?}, \
             got {reported:?}"
        );

        // And the state no longer admits the signal, because the execution has ended.
        let (status, refused) = signal(&router, &id, signal_type, payload).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INVALID_SIGNAL),
            "case {case:?}: the same signal must be refused once the execution has ended, got \
             {status} with {refused}"
        );
    }
}
