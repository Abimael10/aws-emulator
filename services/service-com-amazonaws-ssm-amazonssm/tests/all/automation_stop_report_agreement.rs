// SPDX-License-Identifier: CC0-1.0

//! What a `StopAutomationExecution` leaves behind, as EVERY reader of an Automation
//! execution reports it.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The rule: a stop applied to a running execution ends it where it stands, and that one
//! stored outcome is what every read of it reports — `GetAutomationExecution`,
//! `DescribeAutomationExecutions` and `DescribeAutomationStepExecutions` agree on the
//! terminal status, on the end instant, on there being no step currently running, and the
//! execution history selects it by the status the stop left it in rather than by the status
//! it held while it was running.
//!
//! An automation is `Pending` until the service picks it up, so a case that looks past that
//! instant moves the clock through the service's own state ([`pick_up`]) rather than
//! sleeping for it: what it asserts is the stop and what the readers make of it, not how
//! long a span is.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// A wait no case of this suite lets the clock reach, so the execution stands at its first
/// step for as long as the case needs.
const A_LONG_WAIT: &str = "PT5M";

/// The statuses an execution that has NOT ended holds, from
/// `smithy_query(query="shape",shape="AutomationExecutionStatus")` — the set a case checks
/// the execution it is about to stop is in.
const RUNNING_STATUSES: [&str; 3] = ["Pending", "InProgress", "Waiting"];

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case reads the store through and moves the clock with.
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
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// A string member of a body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// Create the Automation runbook `name` with `content`, asserting it succeeded.
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

/// A runbook whose FIRST step is `action` with `inputs` and whose second step would run
/// after it, so a stop at the first step is visible as the second step never being entered.
fn two_step_runbook(action: &str, inputs: Value) -> Value {
    json!({
        "schemaVersion": "0.3",
        "description": "Stands at one step.",
        "mainSteps": [
            {"name": "stand", "action": action, "inputs": inputs},
            {"name": "after", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}},
        ],
    })
}

/// An instant well in the past, which is what the clock is moved to below.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for, without
/// touching the waits its steps are in.
///
/// The service owes a started automation ONE transition, a span after the start
/// (`automation::PICKUP_DELAY`); a case that stops a RUNNING execution moves the clock to it
/// through the store rather than sleeping for it.
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// Read one execution back through `GetAutomationExecution`, asserting the read succeeded.
async fn get(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
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

/// Report this Region's executions with whatever `filters` carries, asserting the report
/// succeeded, and answering the entries it carried.
async fn history(router: &MultiServiceRouter, case: &str, filters: Value) -> Vec<Value> {
    let mut body = json!({});
    if !filters.is_null() {
        body["Filters"] = filters;
    }
    let (status, response) = call(router, "DescribeAutomationExecutions", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAutomationExecutions, got {status} with \
         {response:?}"
    );
    response["AutomationExecutionMetadataList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The history entry for `execution_id`, asserting the history carries exactly one.
async fn history_entry(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
    let entries = history(router, case, Value::Null).await;
    let mine: Vec<Value> = entries
        .iter()
        .filter(|entry| text(entry, "AutomationExecutionId") == execution_id)
        .cloned()
        .collect();
    assert_eq!(
        mine.len(),
        1,
        "case {case:?}: expected the history to carry the execution {execution_id} exactly \
         once, got {entries:?}"
    );
    mine.into_iter().next().unwrap_or(Value::Null)
}

/// Whether the history narrowed to `status` carries `execution_id`.
async fn selected_by_status(
    router: &MultiServiceRouter,
    case: &str,
    execution_id: &str,
    status: &str,
) -> bool {
    history(
        router,
        case,
        json!([{"Key": "ExecutionStatus", "Values": [status]}]),
    )
    .await
    .iter()
    .any(|entry| text(entry, "AutomationExecutionId") == execution_id)
}

/// The step executions `DescribeAutomationStepExecutions` reports for `execution_id`, as
/// `(step name, step status)` pairs.
async fn step_report(
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

/// Stop one execution, answering `(status, body)`. `stop_type` is the `Type` the request
/// names, or `None` for a request that names none.
async fn stop(
    router: &MultiServiceRouter,
    execution_id: &str,
    stop_type: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({"AutomationExecutionId": execution_id});
    if let Some(stop_type) = stop_type {
        body["Type"] = json!(stop_type);
    }
    call(router, "StopAutomationExecution", &body).await
}

/// One row of the table below: the case, the `Type` the stop request names, the `Mode` the
/// start names, the action the automation stands at, that action's inputs, and the status
/// the stop must leave the execution in.
type Row = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    Value,
    &'static str,
);

/// RULE: a stop applied to a running automation ends it where it stands, and that one stored
/// outcome is what every reader reports — the single read, the execution history and the
/// step report agree on the terminal status, on the end instant and on there being no step
/// currently running, and the history selects the execution by the status the stop left it
/// in and no longer by the status it held while it ran.
///
/// The rows differ along the `StopType` the request names (none, each of the two the model
/// declares), along the `ExecutionMode` the automation runs in, along the step it stands at
/// (one the clock would have ended by itself and one only a caller's signal would ever have
/// carried on) and so along the terminal status the stop leaves it in. Each row runs on its
/// own emulator, so the history it reads holds only its own execution.
#[tokio::test]
async fn every_reader_agrees_on_what_a_stop_left_behind() {
    // (case, the `Type` the request names, the `Mode` the start names, the action the
    // automation stands at, its inputs, the status the stop must leave it in)
    let rows: Vec<Row> = vec![
        (
            "a stop that names no type",
            None,
            None,
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            "Cancelled",
        ),
        (
            "a cancel at a step waiting for a signal",
            Some("Cancel"),
            None,
            "aws:pause",
            json!({}),
            "Cancelled",
        ),
        (
            "a cancel of an interactive automation",
            Some("Cancel"),
            Some("Interactive"),
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            "Cancelled",
        ),
        (
            "a completion of an interactive automation",
            Some("Complete"),
            Some("Interactive"),
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            "Success",
        ),
    ];

    for (case, stop_type, mode, action, inputs, expected_status) in rows {
        let (router, engine) = service();
        // A document `Name` admits `[a-zA-Z0-9_\-.]` only
        // (`smithy_query(query="operation",operation="CreateDocument")`), so the runbook is
        // named through that set.
        let name = format!("Stop-Reported-{}", action.replace(':', "-"));
        runbook(&router, &name, &two_step_runbook(action, inputs)).await;

        let mut start = json!({"DocumentName": name});
        if let Some(mode) = mode {
            start["Mode"] = json!(mode);
        }
        let (status, started) = call(&router, "StartAutomationExecution", &start).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StartAutomationExecution, got {status} with \
             {started:?}"
        );
        let id = text(&started, "AutomationExecutionId");

        // The execution is RUNNING before the stop: the pickup the clock owes is reached
        // through the store, and both readers report the same non-terminal status for it.
        pick_up(&engine);
        let before = get(&router, case, &id).await;
        let running_status = text(&before, "AutomationExecutionStatus");
        assert!(
            RUNNING_STATUSES.contains(&running_status.as_str()),
            "case {case:?}: expected the execution to be running before the stop, got \
             {running_status:?} in {before}"
        );
        let entry_before = history_entry(&router, case, &id).await;
        assert_eq!(
            text(&entry_before, "AutomationExecutionStatus"),
            running_status,
            "case {case:?}: expected the history to report the running status the single read \
             reports, got {entry_before}"
        );

        let (status, answered) = stop(&router, &id, stop_type).await;
        assert_eq!(
            (status, &answered),
            (200, &json!({})),
            "case {case:?}: expected an empty 200 for a stop of a running automation, got \
             {status} with {answered}"
        );

        // The single read: the terminal status the stop leaves, with its own end instant and
        // nothing left running.
        let after = get(&router, case, &id).await;
        assert_eq!(
            text(&after, "AutomationExecutionStatus"),
            expected_status,
            "case {case:?}: expected the single read to report the stopped execution as \
             {expected_status:?}, got {after}"
        );
        assert!(
            !after["ExecutionEndTime"].is_null(),
            "case {case:?}: expected an end instant on the stopped execution, got {after}"
        );

        // The history: the same status, the same end instant, the same mode, and no step
        // reported as currently running, since nothing is running one.
        let entry = history_entry(&router, case, &id).await;
        assert_eq!(
            (
                &entry["AutomationExecutionStatus"],
                &entry["ExecutionEndTime"],
                &entry["Mode"],
            ),
            (
                &after["AutomationExecutionStatus"],
                &after["ExecutionEndTime"],
                &after["Mode"],
            ),
            "case {case:?}: expected the history and the single read to agree about the \
             stopped execution, got {entry} against {after}"
        );
        assert!(
            entry["CurrentStepName"].is_null()
                && entry["CurrentAction"].is_null()
                && after["CurrentStepName"].is_null()
                && after["CurrentAction"].is_null(),
            "case {case:?}: expected no step reported as running once the execution has been \
             stopped, got {entry} against {after}"
        );

        // The step report: the step the automation stood at is ended with it, and the step
        // after it was never entered.
        let steps = step_report(&router, case, &id).await;
        assert_eq!(
            steps,
            vec![
                ("stand".to_string(), "Cancelled".to_string()),
                ("after".to_string(), "Pending".to_string()),
            ],
            "case {case:?}: expected the step it stood at ended and nothing after it entered, \
             got {steps:?}"
        );

        // And the history selects it by what the stop left it in, not by what it held while
        // it was running.
        assert!(
            selected_by_status(&router, case, &id, expected_status).await,
            "case {case:?}: expected the history narrowed to {expected_status:?} to carry the \
             stopped execution {id}"
        );
        assert!(
            !selected_by_status(&router, case, &id, &running_status).await,
            "case {case:?}: expected the history narrowed to {running_status:?} to carry the \
             stopped execution {id} no longer"
        );
    }
}
