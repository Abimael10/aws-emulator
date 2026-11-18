// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CHECKPOINTS of a durable execution: the history events every
//! operation of one writes, and the token `CheckpointDurableExecution` applies one against.
//!
//! The rules under test:
//!
//! * EVERY operation a durable handler runs is checkpointed onto the execution's history as
//!   the model's event for that operation's type and the action taken on it, carrying that
//!   event type's own details member — so a `context.wait(…)` appears as a `WaitStarted`
//!   event followed by a `WaitSucceeded` one, between the execution's first and last events
//!   and in the same `EventId`/`EventTimestamp` order as the step events beside them.
//!   Before this held, a handler's wait was performed but never recorded, so a client
//!   reconstructing the execution saw one that apparently never waited.
//! * A checkpoint is applied only against the execution's CURRENT checkpoint token: each
//!   applied checkpoint consumes the token it carried and answers the one the next must
//!   carry, and a checkpoint carrying any other token is refused with HTTP 400
//!   `InvalidParameterValueException` / "Invalid checkpoint token", leaving the history
//!   exactly as it was.
//!
//! Every assertion is over real HTTP requests through the router. The wait rows differ along
//! what the first rule generalises over: the unit the span is named in (seconds,
//! milliseconds), how many waits one handler takes, whether the handler names the wait, and
//! where the wait falls among the steps — plus a handler that takes none, which must record
//! none. The checkpoint rows differ along which operation type and action the checkpoint
//! carries, how many updates one carries, and which token it carries.
//!
//! The waits here are the shortest spans that still prove the handler's own wait was taken
//! and reported, and nothing asserts on how long anything took, so no row's outcome turns on
//! how fast the host runs.

use std::io::Write as _;
use std::sync::Arc;
use std::time::SystemTime;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::durable_steps::{
    MAX_CHECKPOINT_UPDATES, MAX_STEP_EVENTS as MAX_HISTORY_EVENTS,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    ComAmazonawsLambdaAwsgirapiserviceState, DurableExecutionStart,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, package_bytes, router_on, send, zip_file,
    Answer, ROLE,
};

/// Where `Invoke` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$DurableExecutionName` is bound to, and the response header
/// `Invoke$DurableExecutionArn` is bound to.
const NAME_HEADER: &str = "x-amz-durable-execution-name";
const ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// The unpublished version label, as the model's `Version` pattern spells it.
const VERSION_LATEST: &str = "$LATEST";

/// The module every package below holds its handler in.
const HANDLER_MODULE: &str = "lambda_function.py";
/// See [`HANDLER_MODULE`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `DurableConfig$ExecutionTimeout` every function here runs under, in seconds: far
/// longer than any row's wait, so nothing times out.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// The function `Timeout`, in seconds: longer than any row's wait, so no run is cut off.
const FUNCTION_TIMEOUT: u64 = 30;

/// The model's `EventType` values these suites assert on, from `smithy_query(query="shape",
/// shape="EventType")`.
const EVENT_EXECUTION_STARTED: &str = "ExecutionStarted";
const EVENT_EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";
const EVENT_STEP_STARTED: &str = "StepStarted";
const EVENT_STEP_SUCCEEDED: &str = "StepSucceeded";
const EVENT_STEP_FAILED: &str = "StepFailed";
const EVENT_WAIT_STARTED: &str = "WaitStarted";
const EVENT_WAIT_SUCCEEDED: &str = "WaitSucceeded";
const EVENT_WAIT_CANCELLED: &str = "WaitCancelled";
const EVENT_CONTEXT_STARTED: &str = "ContextStarted";
const EVENT_CONTEXT_SUCCEEDED: &str = "ContextSucceeded";

/// The wire error code a refusal of a non-current checkpoint token carries, and the sentence
/// it carries with it.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";
const INVALID_CHECKPOINT_TOKEN: &str = "Invalid checkpoint token";

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
async fn create_durable_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    source: Option<&str>,
) {
    let code = match source {
        Some(source) => zip_file(&package(source)),
        // A function nothing here invokes needs no runnable handler: its executions are
        // started through the state the `Invoke` handler registers one in.
        None => zip_file(&package_bytes(64)),
    };
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "DurableConfig": { "ExecutionTimeout": EXECUTION_TIMEOUT },
            "Code": { "ZipFile": code },
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

/// `GetDurableExecutionHistory` on `arn`, with `query` appended. The label is not greedy, so
/// the ARN's slashes are percent-encoded in the path exactly as an SDK encodes them.
async fn history_with(router: &MultiServiceRouter, arn: &str, query: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history{query}",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await
}

/// `GetDurableExecutionHistory` on `arn`, one page of the documented default size.
async fn history(router: &MultiServiceRouter, arn: &str) -> Answer {
    history_with(router, arn, "").await
}

/// `GetDurableExecution` on `arn`.
async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
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

/// `CheckpointDurableExecution` on `arn`, carrying `body`.
async fn checkpoint(router: &MultiServiceRouter, arn: &str, body: &Value) -> Answer {
    json_call(
        router,
        "POST",
        &format!(
            "/2025-12-01/durable-executions/{}/checkpoint",
            arn.replace('/', "%2F")
        ),
        body,
    )
    .await
}

/// The events an answer reported, in the order it reported them.
fn events(answer: &Answer) -> Vec<Value> {
    answer
        .json()
        .get("Events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `EventType` of every event an answer reported, in order.
fn event_types(answer: &Answer) -> Vec<String> {
    events(answer)
        .iter()
        .map(|event| {
            event
                .get("EventType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The refusal sentence an answer carries, under either spelling the model gives the member.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// A member of one reported event, reached by a dotted path.
fn at<'a>(event: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = event;
    for step in path.split('.') {
        current = current.get(step)?;
    }
    Some(current)
}

/// A handler that runs a step, waits the span its event names in SECONDS, then runs a second
/// step — so the wait falls between two steps.
const STEP_WAIT_STEP_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import Duration, durable_execution, durable_step


@durable_step
def one(context, value):
    return {"step": 1, "value": value}


@durable_step
def two(context, value):
    return {"step": 2, "value": value}


@durable_execution
def lambda_handler(event, context):
    first = context.step(one, event["n"])
    context.wait(Duration.from_seconds(event["wait_seconds"]))
    second = context.step(two, event["n"])
    return {"first": first, "second": second}
"#;

/// A handler whose only operation is a wait named in MILLISECONDS.
const WAIT_IN_MILLIS_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import Duration, durable_execution


@durable_execution
def lambda_handler(event, context):
    context.wait(Duration.from_millis(event["wait_millis"]))
    return {"waited_millis": event["wait_millis"]}
"#;

/// A handler that takes TWO waits, the first of them NAMED.
const TWO_WAITS_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import Duration, durable_execution


@durable_execution
def lambda_handler(event, context):
    context.wait(Duration.from_seconds(event["wait_seconds"]), name="settle")
    context.wait(Duration.from_millis(event["wait_millis"]))
    return {"waits": 2}
"#;

/// A handler that takes NO wait at all, which must record none.
const NO_WAIT_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import durable_execution, durable_step


@durable_step
def one(context, value):
    return {"step": 1, "value": value}


@durable_execution
def lambda_handler(event, context):
    return {"first": context.step(one, event["n"])}
"#;

/// What one WAIT of a handler must be recorded as: the span it asked for in whole seconds,
/// and the name the handler gave it.
struct WaitExpectation {
    /// `WaitStartedDetails$Duration` / `WaitSucceededDetails$Duration`, "the duration to
    /// wait, in seconds".
    seconds: i64,
    /// `Event$Name`, "the customer-provided name for this operation", and `None` for a wait
    /// the handler did not name.
    name: Option<&'static str>,
}

/// One row of the recorded-wait table: the handler, the event it is invoked with, the whole
/// history it must therefore record, and what each of its waits must be recorded as.
struct WaitCase {
    case: &'static str,
    source: &'static str,
    event: Value,
    types: Vec<&'static str>,
    waits: Vec<WaitExpectation>,
}

/// The spans the rows ask for: the shortest that still prove the handler's own wait was
/// taken and reported. A span named finer than a whole second is reported as the nearest
/// second, which is the `DurationSeconds` floor of 0 for a quarter of one.
const WAIT_SECONDS: u64 = 1;
const WAIT_MILLIS: u64 = 250;

/// RULE: every operation a durable handler runs is checkpointed onto the execution's history
/// as the model's event for that operation's type and action — so a handler's wait is a
/// `WaitStarted` event followed by a `WaitSucceeded` one, between the first and last events.
#[tokio::test]
async fn a_durable_handlers_wait_is_recorded_in_its_history() {
    let rows = [
        WaitCase {
            case: "a wait named in seconds, between two steps",
            source: STEP_WAIT_STEP_HANDLER,
            event: json!({ "n": 3, "wait_seconds": WAIT_SECONDS }),
            types: vec![
                EVENT_EXECUTION_STARTED,
                EVENT_STEP_STARTED,
                EVENT_STEP_SUCCEEDED,
                EVENT_WAIT_STARTED,
                EVENT_WAIT_SUCCEEDED,
                EVENT_STEP_STARTED,
                EVENT_STEP_SUCCEEDED,
                EVENT_EXECUTION_SUCCEEDED,
            ],
            waits: vec![WaitExpectation {
                seconds: 1,
                name: None,
            }],
        },
        WaitCase {
            case: "a wait named in milliseconds, the handler's only operation",
            source: WAIT_IN_MILLIS_HANDLER,
            event: json!({ "wait_millis": WAIT_MILLIS }),
            types: vec![
                EVENT_EXECUTION_STARTED,
                EVENT_WAIT_STARTED,
                EVENT_WAIT_SUCCEEDED,
                EVENT_EXECUTION_SUCCEEDED,
            ],
            waits: vec![WaitExpectation {
                seconds: 0,
                name: None,
            }],
        },
        WaitCase {
            case: "two waits, the first of them named",
            source: TWO_WAITS_HANDLER,
            event: json!({ "wait_seconds": WAIT_SECONDS, "wait_millis": WAIT_MILLIS }),
            types: vec![
                EVENT_EXECUTION_STARTED,
                EVENT_WAIT_STARTED,
                EVENT_WAIT_SUCCEEDED,
                EVENT_WAIT_STARTED,
                EVENT_WAIT_SUCCEEDED,
                EVENT_EXECUTION_SUCCEEDED,
            ],
            waits: vec![
                WaitExpectation {
                    seconds: 1,
                    name: Some("settle"),
                },
                WaitExpectation {
                    seconds: 0,
                    name: None,
                },
            ],
        },
        WaitCase {
            case: "a handler that takes no wait at all",
            source: NO_WAIT_HANDLER,
            event: json!({ "n": 5 }),
            types: vec![
                EVENT_EXECUTION_STARTED,
                EVENT_STEP_STARTED,
                EVENT_STEP_SUCCEEDED,
                EVENT_EXECUTION_SUCCEEDED,
            ],
            waits: Vec::new(),
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, row) in rows.iter().enumerate() {
        let function = format!("durable-wait-{index}");
        create_durable_function(&router, &engine, &function, Some(row.source)).await;
        let invoked = send(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            "application/json",
            serde_json::to_vec(&row.event).expect("the event serializes"),
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
        assert_eq!(
            invoked.header("x-amz-function-error"),
            None,
            "case {:?}: the handler must not fault, got {}",
            row.case,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked.header(ARN_HEADER).unwrap_or_else(|| {
            panic!(
                "case {:?}: the invocation of a durable function must answer an execution ARN",
                row.case
            )
        });

        let read = history(&router, &arn).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: GetDurableExecutionHistory must answer 200, got {} {}",
            row.case,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let reported = event_types(&read);
        assert_eq!(
            reported, row.types,
            "case {:?}: expected the history {:?}, got {reported:?}",
            row.case, row.types
        );

        // "Event IDs increment sequentially": the ids run from 1 over every event, the wait
        // pair included, so the pair is part of the one ordered log rather than hidden
        // beside it, and the timestamps never go backwards.
        let recorded = events(&read);
        let ids: Vec<i64> = recorded
            .iter()
            .map(|event| event.get("EventId").and_then(Value::as_i64).unwrap_or(-1))
            .collect();
        let expected_ids: Vec<i64> = (1..=i64::try_from(row.types.len()).unwrap_or(0)).collect();
        assert_eq!(
            ids, expected_ids,
            "case {:?}: expected the event ids {expected_ids:?}, got {ids:?}",
            row.case
        );
        let stamps: Vec<f64> = recorded
            .iter()
            .map(|event| {
                event
                    .get("EventTimestamp")
                    .and_then(Value::as_f64)
                    .unwrap_or(-1.0)
            })
            .collect();
        assert!(
            stamps.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {:?}: expected the events ordered by EventTimestamp, got {stamps:?}",
            row.case
        );

        // Each wait the handler took, as the pair of events it is: both strictly between the
        // first and the last event, the started one before the succeeded one, each carrying
        // its own details member and the span the handler asked for.
        let started: Vec<(usize, &Value)> = recorded
            .iter()
            .enumerate()
            .filter(|(_, event)| at(event, "WaitStartedDetails").is_some())
            .collect();
        let succeeded: Vec<(usize, &Value)> = recorded
            .iter()
            .enumerate()
            .filter(|(_, event)| at(event, "WaitSucceededDetails").is_some())
            .collect();
        assert_eq!(
            (started.len(), succeeded.len()),
            (row.waits.len(), row.waits.len()),
            "case {:?}: expected {} wait pair(s) in the history, got {} started and {} \
             succeeded in {recorded:?}",
            row.case,
            row.waits.len(),
            started.len(),
            succeeded.len()
        );
        for (wait, ((start_index, start), (end_index, end))) in
            row.waits.iter().zip(started.iter().zip(succeeded.iter()))
        {
            assert!(
                *start_index > 0 && *end_index > *start_index && *end_index < recorded.len() - 1,
                "case {:?}: expected the wait pair strictly between the first and last \
                 events and in that order, got positions {start_index} and {end_index} of \
                 {} events",
                row.case,
                recorded.len()
            );
            assert_eq!(
                start.get("EventType").and_then(Value::as_str),
                Some(EVENT_WAIT_STARTED),
                "case {:?}: the event carrying WaitStartedDetails must be a \
                 {EVENT_WAIT_STARTED}, got {start}",
                row.case
            );
            assert_eq!(
                end.get("EventType").and_then(Value::as_str),
                Some(EVENT_WAIT_SUCCEEDED),
                "case {:?}: the event carrying WaitSucceededDetails must be a \
                 {EVENT_WAIT_SUCCEEDED}, got {end}",
                row.case
            );
            assert_eq!(
                at(start, "WaitStartedDetails.Duration").and_then(Value::as_i64),
                Some(wait.seconds),
                "case {:?}: expected the started wait to report {} second(s), got {start}",
                row.case,
                wait.seconds
            );
            assert_eq!(
                at(end, "WaitSucceededDetails.Duration").and_then(Value::as_i64),
                Some(wait.seconds),
                "case {:?}: expected the succeeded wait to report {} second(s), got {end}",
                row.case,
                wait.seconds
            );
            // "The date and time when the wait operation is scheduled to complete": the
            // span the wait asked for after the instant it began.
            let began = start
                .get("EventTimestamp")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0);
            let scheduled = at(start, "WaitStartedDetails.ScheduledEndTimestamp")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0);
            let span = wait.seconds as f64;
            assert!(
                (scheduled - began - span).abs() < 0.001,
                "case {:?}: expected the wait scheduled to end {span} second(s) after it \
                 began ({began}), got {scheduled}",
                row.case
            );
            assert_eq!(
                start.get("Name").and_then(Value::as_str),
                wait.name,
                "case {:?}: expected the wait's name to be {:?}, got {start}",
                row.case,
                wait.name
            );
            // The pair is ONE operation, so both events name the same one.
            assert_eq!(
                start.get("Id").and_then(Value::as_str),
                end.get("Id").and_then(Value::as_str),
                "case {:?}: expected both events of one wait to name the same operation, \
                 got {start} and {end}",
                row.case
            );
        }
    }
}

/// Which token one checkpoint of the table below carries.
enum CarriedToken {
    /// A well-formed token no checkpoint of this execution ever minted.
    Invented(&'static str),
    /// The token currently minted for ANOTHER execution of the same function.
    OfAnotherExecution,
    /// The token this execution's own earlier checkpoint already consumed.
    AlreadyConsumed,
}

/// One row of the refused-token table.
struct RefusedCase {
    case: &'static str,
    token: CarriedToken,
    /// The updates the refused checkpoint carries, which must change nothing either.
    updates: Value,
}

/// Start one durable execution of `function` through the state the `Invoke` handler
/// registers one in, answering the ARN it is addressed by.
fn start_execution(
    engine: &Arc<engine::InMemoryEngine>,
    function: &str,
    name: &str,
    payload: &[u8],
) -> String {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let record = store
        .functions
        .get_mut(function)
        .unwrap_or_else(|| panic!("the store must hold the function {function}"));
    match record.start_durable_execution(name, VERSION_LATEST, payload, SystemTime::now()) {
        DurableExecutionStart::Started(arn) => arn,
        other => panic!("starting the execution {name} must answer its ARN, got {other:?}"),
    }
}

/// The checkpoint token the stored execution `name` of `function` currently holds — what the
/// Lambda runtime driving that execution is handed, and the only token a checkpoint of it is
/// applied against.
fn current_token(
    engine: &Arc<engine::InMemoryEngine>,
    function: &str,
    name: &str,
) -> Option<String> {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    store
        .functions
        .get(function)
        .and_then(|record| record.durable_executions.get(name))
        .and_then(|execution| execution.current_checkpoint_token().map(str::to_string))
}

/// A well-formed `CheckpointToken` (`^[A-Za-z0-9+/]+={0,2}$`) carrying `bytes`, which is how
/// a caller sends a token it was never handed.
fn invented_token(bytes: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes())
}

/// RULE: a checkpoint is applied only against the execution's CURRENT token — any other
/// token is refused with `InvalidParameterValueException` / "Invalid checkpoint token" and
/// changes nothing.
#[tokio::test]
async fn a_checkpoint_carrying_a_non_current_token_is_refused_and_changes_nothing() {
    let rows = [
        RefusedCase {
            case: "a well-formed token no checkpoint of this execution minted",
            token: CarriedToken::Invented("not-the-real-token"),
            updates: json!([]),
        },
        RefusedCase {
            case: "a token that is current for another execution of the same function",
            token: CarriedToken::OfAnotherExecution,
            updates: json!([{ "Id": "step-1", "Type": "STEP", "Action": "START" }]),
        },
        RefusedCase {
            case: "the token this execution's own earlier checkpoint consumed",
            token: CarriedToken::AlreadyConsumed,
            updates: json!([
                { "Id": "wait-1", "Type": "WAIT", "Action": "START",
                  "WaitOptions": { "WaitSeconds": 30 } },
            ]),
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-tokens";
    create_durable_function(&router, &engine, function, None).await;
    // The execution whose own token is never the one under test, so one row can carry a
    // token that is current somewhere else.
    let other = "other-execution";
    start_execution(&engine, function, other, b"{}");

    for (index, row) in rows.iter().enumerate() {
        let name = format!("refused-{index}");
        let arn = start_execution(&engine, function, &name, b"{}");
        let carried = match row.token {
            CarriedToken::Invented(bytes) => invented_token(bytes),
            CarriedToken::OfAnotherExecution => current_token(&engine, function, other)
                .unwrap_or_else(|| panic!("case {:?}: the other execution holds a token", row.case)),
            CarriedToken::AlreadyConsumed => {
                let current = current_token(&engine, function, &name).unwrap_or_else(|| {
                    panic!("case {:?}: a running execution holds a token", row.case)
                });
                let applied = checkpoint(
                    &router,
                    &arn,
                    &json!({ "CheckpointToken": current, "Updates": [] }),
                )
                .await;
                assert_eq!(
                    applied.status,
                    200,
                    "case {:?}: the checkpoint carrying the current token must be applied, \
                     got {} {}",
                    row.case,
                    applied.status,
                    String::from_utf8_lossy(&applied.body)
                );
                current
            }
        };

        let before = event_types(&history(&router, &arn).await);
        let refused = checkpoint(
            &router,
            &arn,
            &json!({ "CheckpointToken": carried, "Updates": row.updates }),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {:?}: expected HTTP 400, got {} {}",
            row.case,
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_PARAMETER_VALUE,
            "case {:?}: expected {INVALID_PARAMETER_VALUE}, got {}",
            row.case,
            refused.error_code()
        );
        assert_eq!(
            message(&refused),
            INVALID_CHECKPOINT_TOKEN,
            "case {:?}: expected the sentence {INVALID_CHECKPOINT_TOKEN:?}, got {:?}",
            row.case,
            message(&refused)
        );
        let after = event_types(&history(&router, &arn).await);
        assert_eq!(
            after, before,
            "case {:?}: the refusal must leave the history as it was ({before:?}), got \
             {after:?}",
            row.case
        );
    }
}

/// One row of the applied-checkpoint table: the updates one checkpoint carries, and the
/// events they must append to the execution's history.
struct AppliedCase {
    case: &'static str,
    updates: Value,
    appended: Vec<&'static str>,
}

/// RULE: an applied checkpoint appends the history event each of its updates is — the
/// model's event for that update's operation type and action — consumes the token it carried
/// and answers the one the next checkpoint must carry.
#[tokio::test]
async fn an_applied_checkpoint_records_its_updates_and_rotates_the_token() {
    let rows = [
        AppliedCase {
            case: "a step that started",
            updates: json!([
                { "Id": "step-1", "Type": "STEP", "Action": "START", "Name": "charge" },
            ]),
            appended: vec![EVENT_STEP_STARTED],
        },
        AppliedCase {
            case: "a step that succeeded, carrying its result",
            updates: json!([
                { "Id": "step-1", "Type": "STEP", "Action": "SUCCEED",
                  "Payload": "{\"charged\":true}" },
            ]),
            appended: vec![EVENT_STEP_SUCCEEDED],
        },
        AppliedCase {
            case: "a step that failed, carrying its error",
            updates: json!([
                { "Id": "step-1", "Type": "STEP", "Action": "FAIL",
                  "Error": { "ErrorType": "ValueError", "ErrorMessage": "no funds" } },
            ]),
            appended: vec![EVENT_STEP_FAILED],
        },
        AppliedCase {
            case: "a whole wait: the one that began and the one that elapsed",
            updates: json!([
                { "Id": "wait-1", "Type": "WAIT", "Action": "START",
                  "WaitOptions": { "WaitSeconds": 30 } },
                { "Id": "wait-1", "Type": "WAIT", "Action": "SUCCEED",
                  "WaitOptions": { "WaitSeconds": 30 } },
            ]),
            appended: vec![EVENT_WAIT_STARTED, EVENT_WAIT_SUCCEEDED],
        },
        AppliedCase {
            case: "a wait that was cancelled",
            updates: json!([
                { "Id": "wait-1", "Type": "WAIT", "Action": "CANCEL",
                  "Error": { "ErrorType": "Cancelled", "ErrorMessage": "the wait was cut off" } },
            ]),
            appended: vec![EVENT_WAIT_CANCELLED],
        },
        AppliedCase {
            case: "a child context that started and succeeded",
            updates: json!([
                { "Id": "ctx-1", "Type": "CONTEXT", "Action": "START" },
                { "Id": "ctx-1", "Type": "CONTEXT", "Action": "SUCCEED", "Payload": "{}" },
            ]),
            appended: vec![EVENT_CONTEXT_STARTED, EVENT_CONTEXT_SUCCEEDED],
        },
        AppliedCase {
            case: "a checkpoint carrying no update at all",
            updates: json!([]),
            appended: Vec::new(),
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-updates";
    create_durable_function(&router, &engine, function, None).await;

    for (index, row) in rows.iter().enumerate() {
        let name = format!("applied-{index}");
        let arn = start_execution(&engine, function, &name, b"{}");
        let carried = current_token(&engine, function, &name)
            .unwrap_or_else(|| panic!("case {:?}: a running execution holds a token", row.case));
        let before = event_types(&history(&router, &arn).await);

        let applied = checkpoint(
            &router,
            &arn,
            &json!({ "CheckpointToken": carried, "Updates": row.updates }),
        )
        .await;
        assert_eq!(
            applied.status,
            200,
            "case {:?}: expected HTTP 200, got {} {}",
            row.case,
            applied.status,
            String::from_utf8_lossy(&applied.body)
        );

        let expected: Vec<String> = before
            .iter()
            .cloned()
            .chain(row.appended.iter().map(|event| (*event).to_string()))
            .collect();
        let after = event_types(&history(&router, &arn).await);
        assert_eq!(
            after, expected,
            "case {:?}: expected the history {expected:?}, got {after:?}",
            row.case
        );

        // "Each checkpoint operation consumes this token and returns a new one": the answer
        // carries the token the NEXT checkpoint must carry, which is the one the execution
        // now holds and not the one this request sent.
        let answered = applied.string("CheckpointToken").unwrap_or_else(|| {
            panic!(
                "case {:?}: an applied checkpoint must answer the next token, got {}",
                row.case,
                String::from_utf8_lossy(&applied.body)
            )
        });
        assert_ne!(
            answered, carried,
            "case {:?}: the answered token must not be the one this checkpoint consumed",
            row.case
        );
        assert_eq!(
            current_token(&engine, function, &name).as_deref(),
            Some(answered.as_str()),
            "case {:?}: the execution must now hold the answered token, holding {:?}",
            row.case,
            current_token(&engine, function, &name)
        );
        // "Updated execution state information that includes any changes that occurred since
        // the last checkpoint, such as completed callbacks or expired timers": nothing of
        // this execution completed between its checkpoints without a checkpoint of its own.
        assert_eq!(
            applied.json().get("NewExecutionState"),
            Some(&json!({ "Operations": [] })),
            "case {:?}: expected the updated state to report no operation, got {}",
            row.case,
            String::from_utf8_lossy(&applied.body)
        );

        // The token this checkpoint consumed is no longer current, so a repeat of the very
        // same request is refused and appends nothing a second time.
        let repeated = checkpoint(
            &router,
            &arn,
            &json!({ "CheckpointToken": carried, "Updates": row.updates }),
        )
        .await;
        assert_eq!(
            (repeated.status, message(&repeated)),
            (400, INVALID_CHECKPOINT_TOKEN.to_string()),
            "case {:?}: expected the consumed token to be refused, got {} {}",
            row.case,
            repeated.status,
            String::from_utf8_lossy(&repeated.body)
        );
        let repeated_history = event_types(&history(&router, &arn).await);
        assert_eq!(
            repeated_history, expected,
            "case {:?}: the refused repeat must leave the history at {expected:?}, got \
             {repeated_history:?}",
            row.case
        );
    }
}

/// RULE: the same checkpoint sent twice under one `ClientToken` is one checkpoint — the
/// duplicate is answered what the first was and applies nothing a second time.
#[tokio::test]
async fn a_duplicate_checkpoint_under_one_client_token_is_answered_once() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-idempotency";
    create_durable_function(&router, &engine, function, None).await;
    let name = "idempotent";
    let arn = start_execution(&engine, function, name, b"{}");
    let carried = current_token(&engine, function, name).expect("a running execution holds a token");
    let request = json!({
        "CheckpointToken": carried,
        "ClientToken": "checkpoint-request-1",
        "Updates": [{ "Id": "step-1", "Type": "STEP", "Action": "START", "Name": "charge" }],
    });

    let first = checkpoint(&router, &arn, &request).await;
    assert_eq!(
        first.status,
        200,
        "the first checkpoint must be applied, got {} {}",
        first.status,
        String::from_utf8_lossy(&first.body)
    );
    let after_first = event_types(&history(&router, &arn).await);
    assert_eq!(
        after_first,
        vec![EVENT_EXECUTION_STARTED, EVENT_STEP_STARTED],
        "the first checkpoint must record its update, got {after_first:?}"
    );

    let duplicate = checkpoint(&router, &arn, &request).await;
    assert_eq!(
        duplicate.status,
        200,
        "the duplicate must be answered as the first was, got {} {}",
        duplicate.status,
        String::from_utf8_lossy(&duplicate.body)
    );
    assert_eq!(
        duplicate.string("CheckpointToken"),
        first.string("CheckpointToken"),
        "the duplicate must answer the token the first was answered, got {}",
        String::from_utf8_lossy(&duplicate.body)
    );
    let after_duplicate = event_types(&history(&router, &arn).await);
    assert_eq!(
        after_duplicate, after_first,
        "the duplicate must apply nothing a second time, leaving {after_first:?}, got \
         {after_duplicate:?}"
    );
}

/// RULE: one checkpoint carries at most as many updates as the emulator's own ceiling on a
/// checkpoint's batch (`state::durable_steps::MAX_CHECKPOINT_UPDATES`, the events one
/// execution's history may be grown by, since `OperationUpdates` carries no modelled
/// length), and its events are recorded only while the history has ROOM for them: a batch
/// that fits is applied whole, and one that does not — past the batch ceiling, or past the
/// room the history has left — is refused with nothing applied and no token consumed.
#[tokio::test]
async fn a_checkpoint_carries_at_most_the_updates_one_may_carry() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-batch";
    create_durable_function(&router, &engine, function, None).await;

    // One STEP start per update, named apart so each is its own operation.
    let batch = |count: usize| -> Value {
        Value::Array(
            (0..count)
                .map(|index| json!({ "Id": format!("step-{index}"), "Type": "STEP",
                                     "Action": "START" }))
                .collect(),
        )
    };
    // Every page of the history at once, so the events a batch really appended are counted.
    let whole = format!("?MaxItems={MAX_HISTORY_EVENTS}");

    // One past the batch ceiling: refused, with the history and the token left as they were.
    let over = "one update past the batch ceiling";
    let arn = start_execution(&engine, function, "over-the-ceiling", b"{}");
    let carried = current_token(&engine, function, "over-the-ceiling")
        .expect("a running execution holds a token");
    let refused = checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": carried, "Updates": batch(MAX_CHECKPOINT_UPDATES + 1) }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, INVALID_PARAMETER_VALUE.to_string()),
        "case {over:?}: expected 400 {INVALID_PARAMETER_VALUE}, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    let after = event_types(&history(&router, &arn).await);
    assert_eq!(
        after,
        vec![EVENT_EXECUTION_STARTED],
        "case {over:?}: a refused batch must apply none of its updates, got {after:?}"
    );
    assert_eq!(
        current_token(&engine, function, "over-the-ceiling").as_deref(),
        Some(carried.as_str()),
        "case {over:?}: a refused checkpoint must consume no token"
    );

    // Exactly the room the history has left (it already holds the execution's start): applied
    // whole, so the history then holds every event one may record.
    let at = "the room the history has left";
    let arn = start_execution(&engine, function, "at-the-ceiling", b"{}");
    let carried = current_token(&engine, function, "at-the-ceiling")
        .expect("a running execution holds a token");
    let room = MAX_HISTORY_EVENTS - 1;
    let applied = checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": carried, "Updates": batch(room) }),
    )
    .await;
    assert_eq!(
        applied.status,
        200,
        "case {at:?}: expected HTTP 200, got {} {}",
        applied.status,
        String::from_utf8_lossy(&applied.body)
    );
    let recorded = events(&history_with(&router, &arn, &whole).await);
    assert_eq!(
        recorded.len(),
        MAX_HISTORY_EVENTS,
        "case {at:?}: expected every one of the {room} update(s) recorded beside the start, \
         got {} event(s)",
        recorded.len()
    );
    assert_ne!(
        current_token(&engine, function, "at-the-ceiling").as_deref(),
        Some(carried.as_str()),
        "case {at:?}: an applied checkpoint must consume the token it carried"
    );

    // One event past that room: refused whole, because a checkpoint whose events the history
    // cannot all take must not be answered as one it stored.
    let full = "one event past the room the history has left";
    let arn = start_execution(&engine, function, "past-the-room", b"{}");
    let carried = current_token(&engine, function, "past-the-room")
        .expect("a running execution holds a token");
    let refused = checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": carried, "Updates": batch(room + 1) }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, INVALID_PARAMETER_VALUE.to_string()),
        "case {full:?}: expected 400 {INVALID_PARAMETER_VALUE}, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    let after = event_types(&history(&router, &arn).await);
    assert_eq!(
        after,
        vec![EVENT_EXECUTION_STARTED],
        "case {full:?}: a refused batch must apply none of its updates, got {after:?}"
    );
}

/// RULE: the updates of one checkpoint are applied atomically, so a checkpoint carrying an
/// update this emulator cannot apply applies none of them — and the token it carried is still
/// the current one afterwards.
///
/// The rows differ along which update cannot be applied: one of an operation this emulator
/// keeps none of (a callback), and one whose action on the EXECUTION itself is not a move
/// this emulator makes (nothing retries or cancels an execution through a checkpoint).
#[tokio::test]
async fn a_checkpoint_this_emulator_cannot_apply_applies_nothing() {
    let rows = [
        (
            "a callback, an operation this emulator keeps none of",
            json!([
                { "Id": "step-1", "Type": "STEP", "Action": "START" },
                { "Id": "cb-1", "Type": "CALLBACK", "Action": "START",
                  "CallbackOptions": { "TimeoutSeconds": 60, "HeartbeatTimeoutSeconds": 10 } },
            ]),
        ),
        (
            "an action on the execution itself that no checkpoint makes",
            json!([
                { "Id": "step-1", "Type": "STEP", "Action": "START" },
                { "Id": "execution", "Type": "EXECUTION", "Action": "CANCEL" },
            ]),
        ),
        (
            "an update after the one reporting the execution's own outcome, which closes \
             the history",
            json!([
                { "Id": "execution", "Type": "EXECUTION", "Action": "SUCCEED",
                  "Payload": "{}" },
                { "Id": "step-1", "Type": "STEP", "Action": "START" },
            ]),
        ),
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-atomicity";
    create_durable_function(&router, &engine, function, None).await;

    for (index, (case, updates)) in rows.iter().enumerate() {
        let name = format!("atomic-{index}");
        let arn = start_execution(&engine, function, &name, b"{}");
        let carried = current_token(&engine, function, &name)
            .unwrap_or_else(|| panic!("case {case:?}: a running execution holds a token"));

        let refused = checkpoint(
            &router,
            &arn,
            &json!({ "CheckpointToken": carried, "Updates": updates }),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected HTTP 400 for an update this emulator cannot apply, got \
             {} {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {}",
            refused.error_code()
        );
        let after = event_types(&history(&router, &arn).await);
        assert_eq!(
            after,
            vec![EVENT_EXECUTION_STARTED],
            "case {case:?}: no update of a refused checkpoint may be applied, got {after:?}"
        );
        assert_eq!(
            current_token(&engine, function, &name).as_deref(),
            Some(carried.as_str()),
            "case {case:?}: a refused checkpoint must consume no token, leaving {:?}",
            current_token(&engine, function, &name)
        );
    }
}

/// RULE: an execution whose history is CLOSED has no current checkpoint token, so no
/// checkpoint of it is applied — and the checkpoint that closed it is answered no next
/// token.
#[tokio::test]
async fn a_checkpoint_that_settles_the_execution_leaves_no_next_token() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-settles";
    create_durable_function(&router, &engine, function, None).await;
    let name = "settled";
    let arn = start_execution(&engine, function, name, b"{}");
    let carried = current_token(&engine, function, name).expect("a running execution holds a token");
    let result = "{\"total\":9}";

    let applied = checkpoint(
        &router,
        &arn,
        &json!({
            "CheckpointToken": carried,
            "Updates": [
                { "Id": "step-1", "Type": "STEP", "Action": "START", "Name": "charge" },
                { "Id": "execution", "Type": "EXECUTION", "Action": "SUCCEED",
                  "Payload": result },
            ],
        }),
    )
    .await;
    assert_eq!(
        applied.status,
        200,
        "the checkpoint carrying the current token must be applied, got {} {}",
        applied.status,
        String::from_utf8_lossy(&applied.body)
    );
    assert_eq!(
        applied.string("CheckpointToken"),
        None,
        "an execution whose history is closed takes no further checkpoint, so none is \
         answered, got {}",
        String::from_utf8_lossy(&applied.body)
    );

    // The execution is settled on what the checkpoint reported, and its history carries the
    // event that settling is.
    let read = get_execution(&router, &arn).await;
    let reported = read.json();
    assert_eq!(
        (
            reported["Status"].as_str(),
            reported["Result"].as_str()
        ),
        (Some("SUCCEEDED"), Some(result)),
        "expected the execution settled on what the checkpoint reported, got {reported}"
    );
    let after = event_types(&history(&router, &arn).await);
    assert_eq!(
        after,
        vec![
            EVENT_EXECUTION_STARTED,
            EVENT_STEP_STARTED,
            EVENT_EXECUTION_SUCCEEDED
        ],
        "expected every update of the batch recorded, the settling one last, got {after:?}"
    );

    // Nothing can be checkpointed onto a closed history, whichever token is carried.
    let refused = checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": invented_token("any-token"), "Updates": [] }),
    )
    .await;
    assert_eq!(
        (refused.status, message(&refused)),
        (400, INVALID_CHECKPOINT_TOKEN.to_string()),
        "expected a settled execution to refuse every checkpoint, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
}

/// RULE: this operation models no `ResourceNotFoundException`, so an ARN that addresses no
/// execution of this emulator is refused as the parameter value it is.
#[tokio::test]
async fn a_checkpoint_of_an_execution_nothing_started_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-checkpoint-missing";
    create_durable_function(&router, &engine, function, None).await;
    let held = start_execution(&engine, function, "held", b"{}");
    // The same ARN with another execution id, which names an execution nothing started.
    let unheld = format!("{held}-elsewhere");

    let refused = checkpoint(
        &router,
        &unheld,
        &json!({ "CheckpointToken": invented_token("any-token"), "Updates": [] }),
    )
    .await;
    assert_eq!(
        refused.status,
        400,
        "expected HTTP 400 for an ARN naming no execution, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(
        refused.error_code(),
        INVALID_PARAMETER_VALUE,
        "expected {INVALID_PARAMETER_VALUE}, got {}",
        refused.error_code()
    );
    assert!(
        message(&refused).contains(&unheld),
        "expected the refusal to name the execution asked about, got {:?}",
        message(&refused)
    );
}
