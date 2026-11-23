// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the only way the API exposes a durable execution's OUTCOME: a client
//! that starts one and then POLLS `GetDurableExecution` for it.
//!
//! The rule under test: **an accepted durable execution advances on its own — the client makes
//! no call but the read — and leaves `RUNNING` for a terminal `ExecutionStatus` (`SUCCEEDED`
//! when the handler returned, `FAILED` when a step's attempts were spent), carrying the
//! `EndTimestamp` of when it ended, the `Result` of what the handler returned, or the `Error`
//! object with `ErrorType` `CallableRuntimeError` and the `ErrorMessage` the step's own
//! exception carried; its history carries one `StepFailed` per attempt the step's retry
//! strategy allowed, numbered in `RetryDetails$CurrentAttempt`, no `StepSucceeded` for a step
//! whose attempts were spent and no checkpoint at all for a step the execution never reached;
//! and every step's WORK really ran, exactly as many times as those checkpoints say.**
//!
//! Sources: `smithy_query(query="shape", shape="ExecutionStatus")` for the statuses an
//! execution takes, `smithy_query(query="operation", operation="GetDurableExecution")` for the
//! members a read reports (`Status`, `EndTimestamp`, `Result`, `Error`), and
//! `smithy_query(query="shape", shape="EventType,StepFailedDetails,RetryDetails")` for the
//! step events and the attempt each carries.
//!
//! What this suite adds to [`crate::durable_execution_step_retry`], whose rows read a
//! SYNCHRONOUS invocation's execution once its own call has answered: here the execution is
//! started by an invocation that is merely ACCEPTED, so the run happens off every response
//! path and the terminal status is reached without the client calling anything else — the
//! shape a client polling for an outcome sees. And each step's work is not a Python-local
//! counter but an `UpdateItem` against an Amazon DynamoDB table on the same engine, keyed on
//! the step, so how many times a step's work REALLY ran is read out of the service that owns
//! that state: a step that already succeeded shows one run however many attempts a later step
//! took, and a step the execution never reached shows no item at all.
//!
//! The rows differ along what the rule generalises over: how many attempts the step's strategy
//! allows (one, two, three), which attempt its work starts returning on (the first, the
//! second, the third, never), how many steps the execution runs, and the terminal status it
//! therefore reaches.
//!
//! Nothing here waits for a span of wall clock: the terminal status is waited for as the
//! CONDITION it is, looked at again until it holds or a generous ceiling passes, and the
//! `Timeout` and `ExecutionTimeout` every row runs under are far above what its handler takes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_step_retry::{
    create_durable_function, get_execution, get_history, step_checkpoints, ARN_HEADER,
    CALLABLE_RUNTIME_ERROR, FAILED, FUNCTIONS_URI, NAME_HEADER, STEP_FAILED, STEP_STARTED,
    STEP_SUCCEEDED, SUCCEEDED,
};
use crate::function_lifecycle::send;
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The DynamoDB wire of this test target: the one call helper and the one table fixture every
// suite that reads a handler's own DynamoDB write back uses, with the key attribute the sink
// table of this target is keyed by.
use crate::kinesis_event_source_handler_writes_dynamodb::{dynamodb, given_table, KEY_ATTRIBUTE};

/// The non-terminal `ExecutionStatus` an execution is polled out of
/// (`smithy_query(query="shape", shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";

/// The header an asynchronous invocation is asked for on (`Invoke$InvocationType`).
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// The counter table every step's work updates, and the attribute each update adds one to. The
/// table is keyed by [`KEY_ATTRIBUTE`], which every table of this target is keyed by.
const TABLE: &str = "durable-step-runs";
const RUNS_ATTRIBUTE: &str = "Runs";

/// The one function every row's execution runs.
const FUNCTION: &str = "durable-polled";

/// How long a row waits for its execution to leave `RUNNING`, and how often it looks again.
/// The ceiling is generous on purpose — it bounds nothing this suite asserts, it only states
/// that the emulator went quiet about an accepted execution.
const SETTLE_BUDGET: Duration = Duration::from_secs(90);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler every row runs: one step per entry the event names, each counting its own RUN in
/// DynamoDB and raising while that count is inside the attempts the event told it to fail.
///
/// The count is the TABLE's, not the process's, so what it reports is how many times the work
/// really ran — a step the runtime re-ran, or one it skipped, shows up in the item.
const STEP_HANDLER: &str = r#"
import boto3

from aws_durable_execution_sdk_python import durable_execution, durable_step
from aws_durable_execution_sdk_python.config import StepConfig
from aws_durable_execution_sdk_python.retries import (
    RetryStrategyConfig,
    create_retry_strategy,
)

COUNTERS = boto3.client("dynamodb")


@durable_step
def work(context, table, key, runs_attribute, step_id, fail_until):
    """Count this run of the step, then raise while the count is inside `fail_until`."""
    counted = COUNTERS.update_item(
        TableName=table,
        Key={key: {"S": step_id}},
        UpdateExpression="ADD %s :one" % (runs_attribute,),
        ExpressionAttributeValues={":one": {"N": "1"}},
        ReturnValues="UPDATED_NEW",
    )
    runs = int(counted["Attributes"][runs_attribute]["N"])
    if runs <= fail_until:
        raise RuntimeError("%s failed on attempt %d" % (step_id, runs))
    return {"step": step_id, "runs": runs}


@durable_execution
def lambda_handler(event, context):
    completed = []
    for step in event["steps"]:
        completed.append(
            context.step(
                work(
                    event["table"],
                    event["key"],
                    event["runs_attribute"],
                    step["step_id"],
                    step["fail_until"],
                ),
                name=step["name"],
                config=StepConfig(
                    retry_strategy=create_retry_strategy(
                        RetryStrategyConfig(max_attempts=step["max_attempts"])
                    )
                ),
            )
        )
    return {"completed": completed}
"#;

/// Lambda and Amazon DynamoDB over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the step's own SDK call is dispatched back into this router, so the counter a row reads is
/// the counter its step wrote.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(engine)),
    );
    router
}

/// How many times the work of the step `step_id` really ran, read out of the table it counted
/// itself in. A step that never ran has no item there, which is no runs.
async fn runs_counted(router: &MultiServiceRouter, case: &str, step_id: &str) -> i64 {
    let read = dynamodb(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { KEY_ATTRIBUTE: { "S": step_id } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read.pointer(&format!("/Item/{RUNS_ATTRIBUTE}/N"))
        .and_then(Value::as_str)
        .and_then(|counted| counted.parse::<i64>().ok())
        .unwrap_or_default()
}

/// The execution `arn` addresses, read again until it reports a status that is not `RUNNING` —
/// which is how a client learns a durable execution's outcome, there being no other way.
async fn polled_to_terminal(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = get_execution(router, arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDurableExecution must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let reported = read.json();
        if reported.get("Status").and_then(Value::as_str) != Some(RUNNING) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never left {RUNNING}, so a client polling for its \
             outcome would wait for ever: {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One step the handler runs.
struct Step {
    /// The step's own name, which its checkpoints carry.
    name: &'static str,
    /// How many of the step's attempts its work fails before returning.
    fail_until: i64,
    /// How many attempts the step's retry strategy allows in all, the first one included.
    max_attempts: i64,
}

/// One row: the steps an execution runs, and what that execution must therefore report.
struct Case {
    case: &'static str,
    steps: &'static [Step],
    /// The `Status` the execution must have reached.
    status: &'static str,
    /// The step whose attempts were spent, for a row that fails, and the attempt its work
    /// raised on last — which is what the `Error.ErrorMessage` names.
    failed_on: Option<(&'static str, i64)>,
    /// Every step checkpoint the history must carry, in order: the event, the step's name and
    /// the attempt it is of (`RetryDetails$CurrentAttempt`, which a scheduled step carries
    /// none of).
    checkpoints: &'static [(&'static str, &'static str, i64)],
    /// How many times each step's work must have run, read out of the counter table.
    runs: &'static [(&'static str, i64)],
}

/// The rows: how many attempts the strategy allows, which attempt the work returns on, and how
/// many steps the execution runs.
const ROWS: &[Case] = &[
    Case {
        case: "a step whose work raises on every one of the two attempts it is allowed",
        steps: &[
            Step { name: "alpha", fail_until: 0, max_attempts: 2 },
            Step { name: "beta", fail_until: 9, max_attempts: 2 },
            // The step AFTER the failing one, which the execution never reaches.
            Step { name: "gamma", fail_until: 0, max_attempts: 2 },
        ],
        status: FAILED,
        failed_on: Some(("beta", 2)),
        checkpoints: &[
            (STEP_STARTED, "alpha", 0),
            (STEP_SUCCEEDED, "alpha", 1),
            (STEP_STARTED, "beta", 0),
            (STEP_FAILED, "beta", 1),
            (STEP_FAILED, "beta", 2),
        ],
        runs: &[("alpha", 1), ("beta", 2), ("gamma", 0)],
    },
    Case {
        case: "a step whose work returns on the second attempt it is allowed",
        steps: &[
            Step { name: "alpha", fail_until: 0, max_attempts: 2 },
            Step { name: "beta", fail_until: 1, max_attempts: 2 },
            Step { name: "gamma", fail_until: 0, max_attempts: 2 },
        ],
        status: SUCCEEDED,
        failed_on: None,
        checkpoints: &[
            (STEP_STARTED, "alpha", 0),
            (STEP_SUCCEEDED, "alpha", 1),
            (STEP_STARTED, "beta", 0),
            (STEP_FAILED, "beta", 1),
            (STEP_SUCCEEDED, "beta", 2),
            (STEP_STARTED, "gamma", 0),
            (STEP_SUCCEEDED, "gamma", 1),
        ],
        // The step that had already succeeded was not run again for the retry of the one after
        // it, and the step after the retried one ran once.
        runs: &[("alpha", 1), ("beta", 2), ("gamma", 1)],
    },
    Case {
        case: "a step whose work returns on the third attempt it is allowed",
        steps: &[Step { name: "delta", fail_until: 2, max_attempts: 3 }],
        status: SUCCEEDED,
        failed_on: None,
        checkpoints: &[
            (STEP_STARTED, "delta", 0),
            (STEP_FAILED, "delta", 1),
            (STEP_FAILED, "delta", 2),
            (STEP_SUCCEEDED, "delta", 3),
        ],
        runs: &[("delta", 3)],
    },
    Case {
        case: "a step allowed a single attempt, whose work raises on it",
        steps: &[Step { name: "epsilon", fail_until: 9, max_attempts: 1 }],
        status: FAILED,
        failed_on: Some(("epsilon", 1)),
        checkpoints: &[(STEP_STARTED, "epsilon", 0), (STEP_FAILED, "epsilon", 1)],
        runs: &[("epsilon", 1)],
    },
];

/// The id the step `name` of the execution `execution` counts itself under: one row's steps are
/// its own, so the counters of two rows never meet.
fn step_id(execution: &str, name: &str) -> String {
    format!("{execution}-{name}")
}

/// RULE: an execution an invocation merely ACCEPTED advances on its own to a terminal status,
/// which a client reads by polling `GetDurableExecution` — `SUCCEEDED` with the handler's
/// `Result` for a run that returned, `FAILED` with `Error.ErrorType` `CallableRuntimeError` and
/// the step's own message for one whose step ran out of attempts — with the step checkpoints of
/// every attempt made and no work run beyond them.
#[tokio::test]
async fn an_accepted_durable_execution_is_polled_to_its_terminal_status() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_table(
        &router,
        "the counter table every step's work updates",
        TABLE,
    )
    .await;
    create_durable_function(&router, &engine, FUNCTION, STEP_HANDLER).await;

    for (index, row) in ROWS.iter().enumerate() {
        let execution = format!("exec-{index}");
        let event = json!({
            "table": TABLE,
            "key": KEY_ATTRIBUTE,
            "runs_attribute": RUNS_ATTRIBUTE,
            "steps": row
                .steps
                .iter()
                .map(|step| json!({
                    "name": step.name,
                    "step_id": step_id(&execution, step.name),
                    "fail_until": step.fail_until,
                    "max_attempts": step.max_attempts,
                }))
                .collect::<Vec<Value>>(),
        });
        let accepted = send(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            "application/json",
            serde_json::to_vec(&event).expect("the event serializes"),
            &[
                (INVOCATION_TYPE_HEADER, "Event".to_string()),
                (NAME_HEADER, execution.clone()),
            ],
        )
        .await;
        assert_eq!(
            accepted.status,
            202,
            "case {:?}: an asynchronous invocation must be accepted, got {} {}",
            row.case,
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        let arn = accepted.header(ARN_HEADER).unwrap_or_else(|| {
            panic!(
                "case {:?}: the accepted invocation of a durable function must answer an \
                 execution ARN",
                row.case
            )
        });

        // The outcome, read the only way the API exposes one.
        let reported = polled_to_terminal(&router, row.case, &arn).await;
        assert_eq!(
            reported.get("Status").and_then(Value::as_str),
            Some(row.status),
            "case {:?}: expected the execution to have reached {}, got {reported}",
            row.case,
            row.status
        );
        // An execution that has ended reports WHEN it ended, at or after it started.
        let (Some(started), Some(ended)) = (
            reported.get("StartTimestamp").and_then(Value::as_f64),
            reported.get("EndTimestamp").and_then(Value::as_f64),
        ) else {
            panic!(
                "case {:?}: expected a StartTimestamp and an EndTimestamp on an execution that \
                 ended, got {reported}",
                row.case
            )
        };
        assert!(
            ended >= started,
            "case {:?}: expected the EndTimestamp ({ended}) at or after the StartTimestamp \
             ({started}), got {reported}",
            row.case
        );

        match row.failed_on {
            Some((step, attempt)) => {
                let expected = format!(
                    "{} failed on attempt {attempt}",
                    step_id(&execution, step)
                );
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
                    Some(expected.as_str()),
                    "case {:?}: expected the step's own message {expected:?}, got {reported}",
                    row.case
                );
                assert!(
                    reported.get("Result").is_none(),
                    "case {:?}: an execution that failed carries no Result, got {reported}",
                    row.case
                );
            }
            None => {
                // The `Result` is the JSON text of what the handler returned: every step it
                // ran, in order, each carrying the run its work returned on.
                let expected = json!({
                    "completed": row
                        .steps
                        .iter()
                        .map(|step| json!({
                            "step": step_id(&execution, step.name),
                            "runs": row
                                .runs
                                .iter()
                                .find(|(name, _)| *name == step.name)
                                .map(|(_, runs)| *runs)
                                .unwrap_or_default(),
                        }))
                        .collect::<Vec<Value>>(),
                });
                let result = reported
                    .get("Result")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: expected the handler's own value as the Result, got \
                             {reported}",
                            row.case
                        )
                    });
                assert_eq!(
                    result, expected,
                    "case {:?}: expected the Result {expected}, got {result}",
                    row.case
                );
                assert!(
                    reported.get("Error").is_none(),
                    "case {:?}: an execution that succeeded carries no Error, got {reported}",
                    row.case
                );
            }
        }

        // Reading an execution reports it, so the next read of a settled one says the same.
        let again = polled_to_terminal(&router, row.case, &arn).await;
        assert_eq!(
            (again.get("Status"), again.get("EndTimestamp")),
            (reported.get("Status"), reported.get("EndTimestamp")),
            "case {:?}: a second read of a settled execution must report the same outcome, got \
             {again} after {reported}",
            row.case
        );

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

        // And the work behind those checkpoints really ran, as many times as they say: the
        // counter each step kept in the table it called is the service's own state.
        for (name, expected_runs) in row.runs {
            let id = step_id(&execution, name);
            let counted = runs_counted(&router, row.case, &id).await;
            assert_eq!(
                counted, *expected_runs,
                "case {:?}: expected the work of {id:?} to have run {expected_runs} time(s), \
                 got {counted}",
                row.case
            );
        }
    }
}
