// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the STEPS of a durable execution: their retries, their checkpoints,
//! and what a step whose attempts are spent does to the execution.
//!
//! The rule under test: a step of a durable execution is run under its own RETRY STRATEGY —
//! the work is attempted again for as long as the strategy allows, a step that has already
//! succeeded is never run a second time, and a step whose attempts are spent FAILS the
//! execution with `Error.ErrorType` `CallableRuntimeError` carrying the message the work's own
//! exception carried. Every attempt is checkpointed on the execution's history as the model's
//! own event for it (`StepStarted`, `StepSucceeded`, `StepFailed`,
//! `smithy_query(query="shape", shape="EventType")`), carrying the attempt it is of in
//! `RetryDetails$CurrentAttempt`, and a step the execution never reached is never
//! checkpointed at all.
//!
//! Every row drives real HTTP requests through the router against a function whose deployment
//! package RUNS a handler written against the durable-execution programming model, and reads
//! the outcome back through `GetDurableExecution` and `GetDurableExecutionHistory`. The rows
//! differ along what the rule generalises over: how many attempts the strategy allows (one,
//! two, three, and none configured at all), which attempt the work starts returning on
//! (the first, the second, the third, never), and how many steps the execution reached.
//!
//! The fixtures below — the durable function, the two reads and the checkpoint reading — are
//! `pub(crate)`, so a sibling suite driving the same operations
//! ([`crate::durable_execution_polled_to_terminal`]) reads them off the same wire rather than
//! rebuilding a harness that could disagree with this one.
//!
//! No row waits for a span of wall clock: a synchronous invocation of a durable function is
//! answered once its run has finished, so the execution is read straight afterwards. The
//! `Timeout` and the `ExecutionTimeout` every function here runs under are far above what its
//! handler takes, so how fast the host runs decides nothing asserted here.

use std::io::Write as _;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::durable_steps::MAX_STEP_PAYLOAD_BYTES;
use service_com_amazonaws_lambda_awsgirapiservice::state::AsyncInvocationOutcome;
use smithy_server_runtime::MultiServiceRouter;

use crate::asynchronous_invoke_run::settled_newest;
use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, router_on, send, zip_file, Answer, ROLE,
};

/// Where `Invoke` is bound.
pub(crate) const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$DurableExecutionName` is bound to, and the response header
/// `Invoke$DurableExecutionArn` is bound to.
pub(crate) const NAME_HEADER: &str = "x-amz-durable-execution-name";
pub(crate) const ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// The header a run that FAILED is signalled on (`Invoke$FunctionError`).
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The `ExecutionStatus` values a settled execution carries (`smithy_query(query="shape",
/// shape="ExecutionStatus")`).
pub(crate) const SUCCEEDED: &str = "SUCCEEDED";
pub(crate) const FAILED: &str = "FAILED";

/// The model's `EventType` values for what happens to a STEP (`smithy_query(query="shape",
/// shape="EventType")`).
pub(crate) const STEP_STARTED: &str = "StepStarted";
pub(crate) const STEP_SUCCEEDED: &str = "StepSucceeded";
pub(crate) const STEP_FAILED: &str = "StepFailed";

/// The `ErrorType` the durable-execution programming model fails an execution with once a
/// step's attempts are spent.
pub(crate) const CALLABLE_RUNTIME_ERROR: &str = "CallableRuntimeError";

/// The module every package below holds its handler in.
const HANDLER_MODULE: &str = "lambda_function.py";
/// See [`HANDLER_MODULE`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `DurableConfig$ExecutionTimeout` every function here runs under, in seconds: far
/// longer than any row's run, so nothing times out.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// The function `Timeout`, in seconds: generous on purpose, because no row here is about the
/// timeout.
const FUNCTION_TIMEOUT: u64 = 300;

/// The handler every row runs: a step whose work fails its first `fail_until` attempts and
/// returns afterwards, driven step by step by the event.
///
/// The attempt counter is the handler's OWN state, so what it reports is how many times the
/// work really ran — a step the runtime re-ran, or one it skipped, shows up here.
const STEP_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import durable_execution, durable_step
from aws_durable_execution_sdk_python.config import StepConfig
from aws_durable_execution_sdk_python.retries import (
    RetryStrategyConfig,
    create_retry_strategy,
)

ATTEMPTS = {}


@durable_step
def work(context, label, fail_until):
    ATTEMPTS[label] = ATTEMPTS.get(label, 0) + 1
    if ATTEMPTS[label] <= fail_until:
        raise RuntimeError("%s failed on attempt %d" % (label, ATTEMPTS[label]))
    return {"step": label, "attempt": ATTEMPTS[label]}


@durable_execution
def lambda_handler(event, context):
    completed = []
    for step in event["steps"]:
        config = None
        if step.get("max_attempts") is not None:
            config = StepConfig(
                retry_strategy=create_retry_strategy(
                    RetryStrategyConfig(max_attempts=step["max_attempts"])
                )
            )
        completed.append(
            context.step(
                work(step["name"], step["fail_until"]),
                name=step["name"],
                config=config,
            )
        )
    return {"completed": completed, "attempts": ATTEMPTS}
"#;

/// A .zip deployment package holding `source` as the handler's module.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file(HANDLER_MODULE, options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// A durable function running `source`, brought to the state an invocation needs.
pub(crate) async fn create_durable_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    source: &str,
) {
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "DurableConfig": { "ExecutionTimeout": EXECUTION_TIMEOUT },
            "Code": { "ZipFile": zip_file(&package(source)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must answer 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(engine);
}

/// `GetDurableExecution` on `arn`.
pub(crate) async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// `GetDurableExecutionHistory` on `arn`.
pub(crate) async fn get_history(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// One checkpoint the history reported for a STEP: its event type, the step's name and the
/// attempt it is of (`RetryDetails$CurrentAttempt`, which a scheduled step carries none of).
pub(crate) fn step_checkpoints(history: &Value) -> Vec<(String, String, i64)> {
    history
        .get("Events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    let event_type = event.get("EventType").and_then(Value::as_str)?;
                    if ![STEP_STARTED, STEP_SUCCEEDED, STEP_FAILED].contains(&event_type) {
                        return None;
                    }
                    let name = event
                        .get("Name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let attempt = ["StepSucceededDetails", "StepFailedDetails"]
                        .iter()
                        .find_map(|details| {
                            event.pointer(&format!("/{details}/RetryDetails/CurrentAttempt"))
                        })
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    Some((event_type.to_string(), name, attempt))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row: the steps the execution runs, and what that execution must therefore report.
struct Case {
    case: &'static str,
    /// The `steps` the event names: each a step's name, how many of its attempts fail, and
    /// how many attempts its retry strategy allows (`null` for a step configured with none).
    steps: fn() -> Value,
    /// The `Status` the execution must end in.
    status: &'static str,
    /// The value the handler must return, for a row whose handler returns.
    result: Option<fn() -> Value>,
    /// The `Error.ErrorMessage` the execution must report, for a row whose step ran out of
    /// attempts.
    error_message: Option<&'static str>,
    /// Every step checkpoint the history must carry, in order.
    checkpoints: &'static [(&'static str, &'static str, i64)],
}

/// RULE: a step is attempted again for as long as its retry strategy allows, each attempt is
/// checkpointed with the attempt number it is of, a step that succeeded is not run again, and
/// a step whose attempts are spent fails the execution with `CallableRuntimeError`.
#[tokio::test]
async fn a_step_is_retried_as_its_strategy_allows_and_checkpointed_per_attempt() {
    let rows = [
        Case {
            case: "a step that returns on its first attempt",
            steps: || json!([{ "name": "alpha", "fail_until": 0, "max_attempts": 1 }]),
            status: SUCCEEDED,
            result: Some(|| {
                json!({
                    "completed": [{ "step": "alpha", "attempt": 1 }],
                    "attempts": { "alpha": 1 },
                })
            }),
            error_message: None,
            checkpoints: &[
                (STEP_STARTED, "alpha", 0),
                (STEP_SUCCEEDED, "alpha", 1),
            ],
        },
        Case {
            case: "a step that returns on the second attempt its strategy allows",
            steps: || {
                json!([
                    { "name": "alpha", "fail_until": 0, "max_attempts": 2 },
                    { "name": "beta", "fail_until": 1, "max_attempts": 2 },
                ])
            },
            status: SUCCEEDED,
            result: Some(|| {
                json!({
                    "completed": [
                        { "step": "alpha", "attempt": 1 },
                        { "step": "beta", "attempt": 2 },
                    ],
                    // The step that had already succeeded was NOT run again for the retry of
                    // the one after it.
                    "attempts": { "alpha": 1, "beta": 2 },
                })
            }),
            error_message: None,
            checkpoints: &[
                (STEP_STARTED, "alpha", 0),
                (STEP_SUCCEEDED, "alpha", 1),
                (STEP_STARTED, "beta", 0),
                (STEP_FAILED, "beta", 1),
                (STEP_SUCCEEDED, "beta", 2),
            ],
        },
        Case {
            case: "a step that returns on the third attempt its strategy allows",
            steps: || json!([{ "name": "beta", "fail_until": 2, "max_attempts": 3 }]),
            status: SUCCEEDED,
            result: Some(|| {
                json!({
                    "completed": [{ "step": "beta", "attempt": 3 }],
                    "attempts": { "beta": 3 },
                })
            }),
            error_message: None,
            checkpoints: &[
                (STEP_STARTED, "beta", 0),
                (STEP_FAILED, "beta", 1),
                (STEP_FAILED, "beta", 2),
                (STEP_SUCCEEDED, "beta", 3),
            ],
        },
        Case {
            case: "a step whose attempts are all spent",
            steps: || {
                json!([
                    { "name": "alpha", "fail_until": 0, "max_attempts": 2 },
                    { "name": "beta", "fail_until": 9, "max_attempts": 2 },
                    // The step AFTER the failing one, which the execution never reaches.
                    { "name": "gamma", "fail_until": 0, "max_attempts": 2 },
                ])
            },
            status: FAILED,
            result: None,
            error_message: Some("beta failed on attempt 2"),
            checkpoints: &[
                (STEP_STARTED, "alpha", 0),
                (STEP_SUCCEEDED, "alpha", 1),
                (STEP_STARTED, "beta", 0),
                (STEP_FAILED, "beta", 1),
                (STEP_FAILED, "beta", 2),
            ],
        },
        Case {
            case: "a step configured with no retry strategy at all is attempted once",
            steps: || json!([{ "name": "beta", "fail_until": 1, "max_attempts": null }]),
            status: FAILED,
            result: None,
            error_message: Some("beta failed on attempt 1"),
            checkpoints: &[(STEP_STARTED, "beta", 0), (STEP_FAILED, "beta", 1)],
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, row) in rows.iter().enumerate() {
        let function = format!("durable-step-{index}");
        create_durable_function(&router, &engine, &function, STEP_HANDLER).await;
        let event = json!({ "steps": (row.steps)() });
        let invoked = send(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            "application/json",
            serde_json::to_vec(&event).expect("the event serializes"),
            &[(NAME_HEADER, format!("exec-{index}"))],
        )
        .await;
        assert_eq!(
            invoked.status,
            200,
            "case {:?}: expected 200 from Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let faulted = invoked.header(FUNCTION_ERROR_HEADER);
        match row.result {
            Some(expected) => {
                assert_eq!(
                    faulted, None,
                    "case {:?}: the handler must not fault, got {}",
                    row.case,
                    String::from_utf8_lossy(&invoked.body)
                );
                assert_eq!(
                    invoked.json(),
                    expected(),
                    "case {:?}: expected the handler's own value as the answer, got {}",
                    row.case,
                    String::from_utf8_lossy(&invoked.body)
                );
            }
            None => assert!(
                faulted.is_some(),
                "case {:?}: a step whose attempts are spent fails the invocation, got {}",
                row.case,
                String::from_utf8_lossy(&invoked.body)
            ),
        }

        let arn = invoked.header(ARN_HEADER).unwrap_or_else(|| {
            panic!(
                "case {:?}: the invocation of a durable function must answer an execution ARN",
                row.case
            )
        });
        let read = get_execution(&router, &arn).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: GetDurableExecution must answer 200, got {} {}",
            row.case,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let reported = read.json();
        assert_eq!(
            reported["Status"].as_str(),
            Some(row.status),
            "case {:?}: expected the execution to have reached {}, got {reported}",
            row.case,
            row.status
        );
        match row.error_message {
            Some(message) => {
                assert_eq!(
                    reported.pointer("/Error/ErrorType").and_then(Value::as_str),
                    Some(CALLABLE_RUNTIME_ERROR),
                    "case {:?}: expected the error type {CALLABLE_RUNTIME_ERROR}, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported
                        .pointer("/Error/ErrorMessage")
                        .and_then(Value::as_str),
                    Some(message),
                    "case {:?}: expected the step's own message {message:?}, got {reported}",
                    row.case
                );
            }
            None => assert!(
                reported.get("Error").is_none(),
                "case {:?}: a successful execution reports no error, got {reported}",
                row.case
            ),
        }

        let history = get_history(&router, &arn).await;
        assert_eq!(
            history.status,
            200,
            "case {:?}: GetDurableExecutionHistory must answer 200, got {} {}",
            row.case,
            history.status,
            String::from_utf8_lossy(&history.body)
        );
        let checkpointed = step_checkpoints(&history.json());
        let expected: Vec<(String, String, i64)> = row
            .checkpoints
            .iter()
            .map(|(event_type, name, attempt)| {
                ((*event_type).to_string(), (*name).to_string(), *attempt)
            })
            .collect();
        assert_eq!(
            checkpointed, expected,
            "case {:?}: expected the step checkpoints {expected:?}, got {checkpointed:?} from {}",
            row.case,
            String::from_utf8_lossy(&history.body)
        );
    }
}

/// RULE: the invocation a durable execution is the run of is delivered ONCE — the retry a
/// failing durable function gets is its steps' own retry strategy, not a second run of the
/// handler — so an ASYNCHRONOUS invocation whose handler failed settles the execution on that
/// one run.
#[tokio::test]
async fn an_asynchronous_durable_execution_is_settled_by_its_first_run() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-step-async";
    create_durable_function(&router, &engine, function, STEP_HANDLER).await;

    let event = json!({
        "steps": [{ "name": "beta", "fail_until": 9, "max_attempts": 2 }],
    });
    let accepted = send(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        serde_json::to_vec(&event).expect("the event serializes"),
        &[
            ("x-amz-invocation-type", "Event".to_string()),
            (NAME_HEADER, "exec-async".to_string()),
        ],
    )
    .await;
    assert_eq!(
        accepted.status,
        202,
        "an asynchronous invocation must be accepted, got {} {}",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
    let arn = accepted
        .header(ARN_HEADER)
        .expect("the invocation of a durable function must answer an execution ARN");

    // The delivery is waited for as the CONDITION it is, however long this host takes to get
    // to it: the entry carries what the attempt came to once it has settled.
    let case = "an asynchronous durable execution whose step ran out of attempts";
    let delivered = settled_newest(&engine, case, function).await;
    assert!(
        matches!(
            delivered.outcome,
            AsyncInvocationOutcome::Delivered {
                function_error: Some(_)
            }
        ),
        "case {case:?}: the handler must have run and failed, got {:?}",
        delivered.outcome
    );
    assert_eq!(
        delivered.attempts, 1,
        "case {case:?}: the run of a durable execution is made once, so the event must not be \
         delivered again, got {} attempt(s)",
        delivered.attempts
    );

    let reported = get_execution(&router, &arn).await.json();
    assert_eq!(
        reported["Status"].as_str(),
        Some(FAILED),
        "case {case:?}: expected the execution to have reached {FAILED}, got {reported}"
    );
    assert_eq!(
        reported.pointer("/Error/ErrorType").and_then(Value::as_str),
        Some(CALLABLE_RUNTIME_ERROR),
        "case {case:?}: expected the error type {CALLABLE_RUNTIME_ERROR}, got {reported}"
    );

    // The history carries the attempts of that ONE run and no others: the step was attempted
    // twice, which is what its strategy allowed, and never a third time.
    let history = get_history(&router, &arn).await.json();
    let checkpointed = step_checkpoints(&history);
    let expected: Vec<(String, String, i64)> = vec![
        (STEP_STARTED.to_string(), "beta".to_string(), 0),
        (STEP_FAILED.to_string(), "beta".to_string(), 1),
        (STEP_FAILED.to_string(), "beta".to_string(), 2),
    ];
    assert_eq!(
        checkpointed, expected,
        "case {case:?}: expected the step checkpoints {expected:?}, got {checkpointed:?} from \
         {history}"
    );
}

/// A handler whose step returns a result of the size the event asks for, in bytes.
const WIDE_RESULT_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import durable_execution, durable_step


@durable_step
def wide(context, size):
    return "r" * size


@durable_execution
def lambda_handler(event, context):
    return {"length": len(context.step(wide(event["size"])))}
"#;

/// RULE: what one step checkpoint carries of a step's result is bounded — the ceiling AWS
/// publishes for a `STEP` operation's payload — and a result that ceiling cut is reported as
/// `Truncated`, while the handler's own answer is whatever it returned.
///
/// The rows differ along the size of the result: inside the ceiling, exactly at it, and past
/// it.
#[tokio::test]
async fn a_step_checkpoint_carries_at_most_the_payload_a_step_may() {
    // The ceiling the service publishes for a STEP operation's payload, which this emulator
    // reads a checkpoint inside (`state::durable_steps::MAX_STEP_PAYLOAD_BYTES`).
    let ceiling = MAX_STEP_PAYLOAD_BYTES;
    // (case, the result's size in bytes, the payload length the history must report, whether
    // the event must report it truncated). The payload is the JSON text of the string, so it
    // carries the two quotes around it.
    let rows: [(&str, usize, usize, bool); 3] = [
        ("a result well inside the ceiling", 1_024, 1_026, false),
        ("a result at the ceiling", ceiling - 2, ceiling, false),
        ("a result past the ceiling", ceiling + 1_024, ceiling, true),
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, (case, size, expected_length, truncated)) in rows.iter().enumerate() {
        let function = format!("durable-step-wide-{index}");
        create_durable_function(&router, &engine, &function, WIDE_RESULT_HANDLER).await;
        let invoked = send(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            "application/json",
            serde_json::to_vec(&json!({ "size": size })).expect("the event serializes"),
            &[(NAME_HEADER, format!("exec-wide-{index}"))],
        )
        .await;
        assert_eq!(
            invoked.status,
            200,
            "case {case:?}: expected 200 from Invoke, got {} {}",
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        assert_eq!(
            invoked.json(),
            json!({ "length": size }),
            "case {case:?}: the handler's own answer is the whole result its step returned, \
             got {}",
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(ARN_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the invocation must answer an ARN"));
        let history = get_history(&router, &arn).await.json();
        let succeeded = history
            .get("Events")
            .and_then(Value::as_array)
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event.get("EventType").and_then(Value::as_str) == Some(STEP_SUCCEEDED))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: the step's own checkpoint, got {history}"));
        let payload = succeeded
            .pointer("/StepSucceededDetails/Result/Payload")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            payload.len(),
            *expected_length,
            "case {case:?}: expected {expected_length} bytes of the result to be checkpointed, \
             got {}",
            payload.len()
        );
        assert_eq!(
            succeeded
                .pointer("/StepSucceededDetails/Result/Truncated")
                .and_then(Value::as_bool),
            Some(*truncated),
            "case {case:?}: expected the checkpoint to report truncated={truncated}, got \
             {succeeded}"
        );
    }
}
