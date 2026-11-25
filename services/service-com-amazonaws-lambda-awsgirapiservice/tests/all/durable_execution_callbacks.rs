// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CALLBACKS of a durable execution: a handler's
//! `context.wait_for_callback`, the `CallbackStarted` history event its id travels in, and the
//! three operations a client signals that id through (`POST
//! /2025-12-01/durable-execution-callbacks/{CallbackId}/succeed`, `…/fail`, `…/heartbeat`).
//!
//! The rule under test: a durable function can wait for something OUTSIDE itself. The durable
//! context mints a callback id, the execution takes a `CallbackStarted` event carrying that id
//! and the timeouts the wait was configured with, and the waiting handler then receives what a
//! client sends to that id — the result of a success, the error of a failure — while the
//! callback is OPEN. Past its window the callback is CLOSED: it takes its own `CallbackTimedOut`
//! event at the instant the window closed, the waiting handler is told the window closed, and
//! every signal to that id is refused `CallbackTimeoutException`. An id no callback was minted
//! for is `ResourceNotFoundException`.
//!
//! The rows differ along what the rule generalises over: WHICH signal ends the wait (a success
//! carrying a result, a failure carrying an error, nothing at all until the window closes),
//! whether a heartbeat was sent first, and which signal is made after the callback has closed.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, router_on, send, zip_file, Answer, ROLE,
};

/// Where `Invoke` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where the three callback operations are bound.
const CALLBACKS_URI: &str = "/2025-12-01/durable-execution-callbacks";

/// The request header `Invoke$DurableExecutionName` is bound to, the one
/// `Invoke$InvocationType` is bound to, and the response header `Invoke$DurableExecutionArn`
/// is bound to.
const NAME_HEADER: &str = "x-amz-durable-execution-name";
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// The module every package below holds its handler in.
const HANDLER_MODULE: &str = "lambda_function.py";
/// See [`HANDLER_MODULE`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `DurableConfig$ExecutionTimeout` every function here runs under, in seconds: far longer
/// than any row's own wait, so no execution times out as a whole.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// The function `Timeout`, in seconds: longer than any row's wait, so no run is cut off.
const FUNCTION_TIMEOUT: u64 = 300;

/// The callback timeout a row that is SIGNALLED runs under, in seconds: far longer than the
/// signalling takes, so nothing about those rows turns on how fast the host runs.
const OPEN_TIMEOUT: u64 = 600;

/// The callback timeout the row that is NOT signalled runs under, in seconds. This is the one
/// limit that row is about, so it is the one span here that is short; every other budget of
/// this suite is generous.
const CLOSING_TIMEOUT: u64 = 2;

/// How long this suite keeps looking for a condition the emulator owes before it reports it
/// never held. Generous: it bounds a loop that polls, so a loaded host takes longer to answer
/// rather than failing.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long between two looks of such a loop.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `ExecutionStatus` values a settled execution carries
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";

/// The `EventType` values a callback's own history events carry
/// (`smithy_query(query="shape",shape="EventType")`).
const CALLBACK_STARTED: &str = "CallbackStarted";
const CALLBACK_SUCCEEDED: &str = "CallbackSucceeded";
const CALLBACK_FAILED: &str = "CallbackFailed";
const CALLBACK_TIMED_OUT: &str = "CallbackTimedOut";

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

/// A durable function running [`CALLBACK_HANDLER`], brought to the state an invocation needs.
async fn create_durable_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
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
            "Code": { "ZipFile": zip_file(&package(CALLBACK_HANDLER)) },
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

/// Start one durable execution ASYNCHRONOUSLY — the run happens off the response path, which is
/// what lets this suite signal the callback while the handler is still waiting on it — and
/// answer the ARN it was registered under.
async fn start_execution(
    router: &MultiServiceRouter,
    function: &str,
    execution: &str,
    event: &Value,
) -> String {
    let invoked = send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        serde_json::to_vec(event).expect("the event serializes"),
        &[
            (NAME_HEADER, execution.to_string()),
            (INVOCATION_TYPE_HEADER, "Event".to_string()),
        ],
    )
    .await;
    assert_eq!(
        invoked.status,
        202,
        "an asynchronous invocation of {function:?} must be accepted, got {} {}",
        invoked.status,
        String::from_utf8_lossy(&invoked.body)
    );
    invoked.header(ARN_HEADER).unwrap_or_else(|| {
        panic!("the invocation of a durable function must answer an execution ARN")
    })
}

/// `GetDurableExecution` on `arn`. The label is not greedy, so the ARN's slashes are
/// percent-encoded in the path exactly as an SDK encodes them.
async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2025-12-01/durable-executions/{}", arn.replace('/', "%2F")),
        &Value::Null,
    )
    .await
}

/// `GetDurableExecutionHistory` on `arn`.
async fn get_history(router: &MultiServiceRouter, arn: &str) -> Answer {
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

/// The first event of `arn`'s history whose `EventType` is `event_type`, looked for again until
/// it appears or [`PATIENCE`] runs out.
///
/// The event is owed by the emulator — by the run that mints a callback, by the client that
/// signals it, or by the clock that closes its window — so this waits for the CONDITION rather
/// than for a span: it looks again until the event is there.
async fn event_of(
    router: &MultiServiceRouter,
    arn: &str,
    event_type: &str,
    case: &str,
) -> Option<Value> {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = get_history(router, arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: GetDurableExecutionHistory must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        let found = last["Events"]
            .as_array()
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event["EventType"].as_str() == Some(event_type))
            })
            .cloned();
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case}: expected a {event_type} event in the history, got {last}");
}

/// The execution `arn` once it has SETTLED, looked for again until it has or [`PATIENCE`] runs
/// out.
async fn settled(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = get_execution(router, arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: GetDurableExecution must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        if last["Status"].as_str() != Some("RUNNING") {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case}: expected the execution to settle, got {last}");
}

/// `SendDurableExecutionCallbackSuccess` on one callback, carrying `result` as the payload.
async fn send_success(router: &MultiServiceRouter, callback_id: &str, result: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("{CALLBACKS_URI}/{callback_id}/succeed"),
        "application/octet-stream",
        serde_json::to_vec(result).expect("the result serializes"),
        &[],
    )
    .await
}

/// `SendDurableExecutionCallbackFailure` on one callback, carrying the error object.
async fn send_failure(router: &MultiServiceRouter, callback_id: &str, error: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("{CALLBACKS_URI}/{callback_id}/fail"),
        "application/json",
        serde_json::to_vec(error).expect("the error serializes"),
        &[],
    )
    .await
}

/// `SendDurableExecutionCallbackHeartbeat` on one callback.
async fn send_heartbeat(router: &MultiServiceRouter, callback_id: &str) -> Answer {
    send(
        router,
        "POST",
        &format!("{CALLBACKS_URI}/{callback_id}/heartbeat"),
        "application/json",
        Vec::new(),
        &[],
    )
    .await
}

/// The error the failure row sends, as `ErrorObject` declares one.
fn sent_error() -> Value {
    json!({
        "ErrorType": "PaymentDeclined",
        "ErrorMessage": "the card issuer declined the charge",
    })
}

/// The result the success row sends.
fn sent_result() -> Value {
    json!({ "charged": true, "reference": "ch-42" })
}

/// RULE: a durable handler's `wait_for_callback` mints a callback the execution's history
/// carries, and what a client sends to that id IN the window is what the waiting handler
/// receives; past the window the callback is closed and every signal to it is refused.
///
/// One test, three rows, each on its own function and execution: the ENDING is what the rows
/// differ along, and every row asserts both halves — what the execution's history took, and
/// what the waiting handler came to.
#[tokio::test]
async fn a_callback_carries_what_a_client_sends_to_the_waiting_handler() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // (the case, how the wait ends, the callback timeout the row runs under)
    let rows: &[(&str, &str, u64)] = &[
        ("a client that completes the callback", "succeeded", OPEN_TIMEOUT),
        ("a client that fails the callback", "failed", OPEN_TIMEOUT),
        ("a window that closes with nothing sent", "timed_out", CLOSING_TIMEOUT),
    ];

    for (index, (case, ending, timeout_seconds)) in rows.iter().enumerate() {
        let function = format!("durable-callback-{index}");
        create_durable_function(&router, &engine, &function).await;
        let execution = format!("exec-{index}");
        let arn = start_execution(
            &router,
            &function,
            &execution,
            &json!({ "timeout_seconds": timeout_seconds, "name": "await-the-charge" }),
        )
        .await;

        // The handler's wait MINTS the callback, and the history carries it: the id a client
        // signals and the timeouts the wait was configured with.
        let started = event_of(&router, &arn, CALLBACK_STARTED, case)
            .await
            .unwrap_or_default();
        let details = started["CallbackStartedDetails"].clone();
        let callback_id = details["CallbackId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !callback_id.is_empty(),
            "case {case}: the CallbackStarted event carries the callback's own id, got {started}"
        );
        assert_eq!(
            details["Timeout"].as_u64(),
            Some(*timeout_seconds),
            "case {case}: the event reports the timeout the wait was configured with, got \
             {started}"
        );
        assert_eq!(
            started["Name"].as_str(),
            Some("await-the-charge"),
            "case {case}: the event reports the name the wait was given, got {started}"
        );

        match *ending {
            "succeeded" => {
                // A HEARTBEAT first, which says the external work is still running and leaves
                // the callback open.
                let beat = send_heartbeat(&router, &callback_id).await;
                assert_eq!(
                    beat.status,
                    200,
                    "case {case}: a heartbeat inside the window is accepted, got {} {}",
                    beat.status,
                    String::from_utf8_lossy(&beat.body)
                );
                let sent = send_success(&router, &callback_id, &sent_result()).await;
                assert_eq!(
                    sent.status,
                    200,
                    "case {case}: a success inside the window is accepted, got {} {}",
                    sent.status,
                    String::from_utf8_lossy(&sent.body)
                );
                // The history takes the callback that SUCCEEDED, carrying what was sent.
                let event = event_of(&router, &arn, CALLBACK_SUCCEEDED, case)
                    .await
                    .unwrap_or_default();
                let payload: Value = event["CallbackSucceededDetails"]["Result"]["Payload"]
                    .as_str()
                    .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                assert_eq!(
                    payload,
                    sent_result(),
                    "case {case}: the event carries the result the client sent, got {event}"
                );
                // ... and the waiting handler received it.
                let reported = settled(&router, &arn, case).await;
                assert_eq!(
                    reported["Status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case}: expected the execution to succeed, got {reported}"
                );
                let result: Value = reported["Result"]
                    .as_str()
                    .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                assert_eq!(
                    result,
                    json!({
                        "ended": "succeeded",
                        "answered": sent_result(),
                        "handed": 1,
                    }),
                    "case {case}: expected the handler to receive what the client sent, got \
                     {reported}"
                );
            }
            "failed" => {
                let sent = send_failure(&router, &callback_id, &sent_error()).await;
                assert_eq!(
                    sent.status,
                    200,
                    "case {case}: a failure inside the window is accepted, got {} {}",
                    sent.status,
                    String::from_utf8_lossy(&sent.body)
                );
                let event = event_of(&router, &arn, CALLBACK_FAILED, case)
                    .await
                    .unwrap_or_default();
                assert_eq!(
                    event["CallbackFailedDetails"]["Error"]["Payload"]["ErrorMessage"].as_str(),
                    Some("the card issuer declined the charge"),
                    "case {case}: the event carries the error the client sent, got {event}"
                );
                let reported = settled(&router, &arn, case).await;
                let result: Value = reported["Result"]
                    .as_str()
                    .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                assert_eq!(
                    result,
                    json!({
                        "ended": "failed",
                        "message": "the card issuer declined the charge",
                        "handed": 1,
                    }),
                    "case {case}: expected the handler to be told what the client failed it \
                     with, got {reported}"
                );
            }
            _ => {
                // NOTHING is sent: the window closes on its own, which is a transition the
                // CLOCK owes, so the history takes the callback that timed out.
                let event = event_of(&router, &arn, CALLBACK_TIMED_OUT, case)
                    .await
                    .unwrap_or_default();
                assert_eq!(
                    event["CallbackTimedOutDetails"]["Error"]["Payload"]["ErrorType"].as_str(),
                    Some("CallbackTimeout"),
                    "case {case}: the event carries the timeout as the error it is, got {event}"
                );
                // Every signal to a callback that has closed is refused, whichever one it is.
                for (signal, answer) in [
                    ("a success", send_success(&router, &callback_id, &sent_result()).await),
                    ("a failure", send_failure(&router, &callback_id, &sent_error()).await),
                    ("a heartbeat", send_heartbeat(&router, &callback_id).await),
                ] {
                    assert_eq!(
                        answer.status,
                        400,
                        "case {case} {signal}: expected 400 past the window, got {} {}",
                        answer.status,
                        String::from_utf8_lossy(&answer.body)
                    );
                    assert!(
                        answer.error_code().contains("CallbackTimeoutException"),
                        "case {case} {signal}: expected CallbackTimeoutException, got {}",
                        answer.error_code()
                    );
                    assert_eq!(
                        answer.string("Message").as_deref(),
                        Some("The callback is either timed out or already completed"),
                        "case {case} {signal}: expected the sentence AWS words that refusal \
                         with, got {}",
                        String::from_utf8_lossy(&answer.body)
                    );
                }
                // ... and the waiting handler was told the window closed.
                let reported = settled(&router, &arn, case).await;
                let result: Value = reported["Result"]
                    .as_str()
                    .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                assert_eq!(
                    result["ended"].as_str(),
                    Some("timed_out"),
                    "case {case}: expected the handler to be told the window closed, got \
                     {reported}"
                );
            }
        }
    }
}

/// RULE: an id no callback of this emulator was minted for addresses no resource, so every one
/// of the three operations answers the modelled 404 for it.
///
/// The rows differ along WHICH text the id is: one that is not an id this emulator mints at
/// all, and one shaped like one but naming an execution that was never started.
#[tokio::test]
async fn an_id_no_callback_was_minted_for_is_not_found() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    // Each id satisfies the model's own `CallbackId` pattern (`^[A-Za-z0-9+/]+={0,2}$`), so
    // what refuses them is the lookup rather than the constraint.
    let rows: &[(&str, &str)] = &[
        ("an id this emulator never mints", "Zm9ydGhpc2lzbm90YW5pZA"),
        (
            // The hexadecimal of `absent\u{1f}exec\u{1f}id\u{1f}callback-1`, which is the SHAPE
            // a minted id has, naming a function this emulator holds nothing for.
            "an id shaped like one, for an execution never started",
            "6162736\
             56e741f657865631f69641f63616c6c6261636b2d31",
        ),
    ];
    for (case, callback_id) in rows {
        let callback_id = callback_id.replace(char::is_whitespace, "");
        for (signal, answer) in [
            (
                "a success",
                send_success(&router, &callback_id, &sent_result()).await,
            ),
            (
                "a failure",
                send_failure(&router, &callback_id, &sent_error()).await,
            ),
            ("a heartbeat", send_heartbeat(&router, &callback_id).await),
        ] {
            assert_eq!(
                answer.status,
                404,
                "case {case} {signal}: expected 404, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert!(
                answer.error_code().contains("ResourceNotFoundException"),
                "case {case} {signal}: expected ResourceNotFoundException, got {}",
                answer.error_code()
            );
            assert_eq!(
                answer.string("Message"),
                Some(format!("Callback not found: {callback_id}")),
                "case {case} {signal}: expected the refusal to name the resource the request \
                 asked about, got {}",
                String::from_utf8_lossy(&answer.body)
            );
        }
    }
}

/// RULE: `NewExecutionState` reports "operations that have been updated since the last
/// checkpoint, such as completed asynchronous work like timers or callbacks", so a callback a
/// client closed between two checkpoints is reported to the next one — once, carrying what it
/// came to — and a callback still open is reported to none.
///
/// The callback is minted through the seam the RUN mints one through
/// (`state::durable_callbacks::answered_for_run`, which is what the durable context calls) and
/// closed over the wire, so what this reads is the same state a handler's own wait produces.
#[tokio::test]
async fn a_callback_that_closed_is_reported_to_the_next_checkpoint() {
    use service_com_amazonaws_lambda_awsgirapiservice::state::{
        durable_callbacks, ComAmazonawsLambdaAwsgirapiserviceState,
    };

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "durable-callback-reported";
    create_durable_function(&router, &engine, function).await;
    let execution = "reported";
    let arn = crate::durable_execution_checkpoints::start_execution(
        &engine,
        function,
        execution,
        b"{}",
    );

    // The callback the run's own durable context mints, which is what a client then signals.
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let minted: Value = serde_json::from_str(&durable_callbacks::answered_for_run(
        &state,
        function,
        execution,
        &json!({
            "action": "START",
            "id": "callback-1",
            "name": "await-the-charge",
            "timeoutSeconds": OPEN_TIMEOUT,
        })
        .to_string(),
    ))
    .expect("the seam answers a document");
    let callback_id = minted["callbackId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !callback_id.is_empty(),
        "minting a callback must answer its id, got {minted}"
    );

    // While it is still OPEN, a checkpoint is told of no updated operation.
    let token = crate::durable_execution_checkpoints::current_token(&engine, function, execution)
        .expect("a running execution holds a checkpoint token");
    let open = crate::durable_execution_checkpoints::checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": token, "Updates": [] }),
    )
    .await;
    assert_eq!(
        open.status,
        200,
        "a checkpoint of a running execution must answer 200, got {} {}",
        open.status,
        String::from_utf8_lossy(&open.body)
    );
    assert_eq!(
        open.json()["NewExecutionState"]["Operations"],
        json!([]),
        "case a callback still open: expected no operation reported as updated, got {}",
        String::from_utf8_lossy(&open.body)
    );

    // Closed by a client, and reported to the checkpoint after it.
    let sent = send_success(&router, &callback_id, &sent_result()).await;
    assert_eq!(
        sent.status,
        200,
        "a success inside the window is accepted, got {} {}",
        sent.status,
        String::from_utf8_lossy(&sent.body)
    );
    let token = crate::durable_execution_checkpoints::current_token(&engine, function, execution)
        .expect("a running execution holds a checkpoint token");
    let closed = crate::durable_execution_checkpoints::checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": token, "Updates": [] }),
    )
    .await;
    assert_eq!(
        closed.status,
        200,
        "a checkpoint of a running execution must answer 200, got {} {}",
        closed.status,
        String::from_utf8_lossy(&closed.body)
    );
    let reported = closed.json()["NewExecutionState"]["Operations"].clone();
    let first = reported
        .as_array()
        .and_then(|operations| operations.first())
        .cloned()
        .unwrap_or(Value::Null);
    for (member, expected) in [
        ("Id", json!("callback-1")),
        ("Name", json!("await-the-charge")),
        ("Type", json!("CALLBACK")),
        ("Status", json!("SUCCEEDED")),
    ] {
        assert_eq!(
            first[member], expected,
            "case a callback a client closed {member}: expected {expected}, got {reported}"
        );
    }
    assert_eq!(
        first["CallbackDetails"]["CallbackId"].as_str(),
        Some(callback_id.as_str()),
        "case a callback a client closed: expected the id the client signalled, got {reported}"
    );
    let result: Value = first["CallbackDetails"]["Result"]
        .as_str()
        .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    assert_eq!(
        result,
        sent_result(),
        "case a callback a client closed: expected what the client sent, got {reported}"
    );

    // ... and once: the checkpoint after that is told nothing again.
    let token = crate::durable_execution_checkpoints::current_token(&engine, function, execution)
        .expect("a running execution holds a checkpoint token");
    let again = crate::durable_execution_checkpoints::checkpoint(
        &router,
        &arn,
        &json!({ "CheckpointToken": token, "Updates": [] }),
    )
    .await;
    assert_eq!(
        again.json()["NewExecutionState"]["Operations"],
        json!([]),
        "case a completion already reported: expected it reported once, got {}",
        String::from_utf8_lossy(&again.body)
    );
}

/// The handler every row runs: it waits for a callback, hands the id the context minted to its
/// own callable — which is how a handler sends that id wherever the external work is — and
/// reports what the wait came to.
const CALLBACK_HANDLER: &str = r#"
from aws_durable_execution_sdk_python import (
    CallbackFailureError,
    CallbackTimeoutError,
    Duration,
    durable_execution,
)
from aws_durable_execution_sdk_python.config import WaitForCallbackConfig

# Every callback id this handler was handed, which is what proves the context handed it one.
_HANDED = []


@durable_execution
def lambda_handler(event, context):
    def hand(callback_id):
        _HANDED.append(callback_id)

    config = WaitForCallbackConfig(
        timeout=Duration.from_seconds(event["timeout_seconds"]),
        name=event.get("name"),
    )
    try:
        answered = context.wait_for_callback(hand, config=config)
    except CallbackTimeoutError as error:
        return {"ended": "timed_out", "message": str(error), "handed": len(_HANDED)}
    except CallbackFailureError as error:
        return {"ended": "failed", "message": str(error), "handed": len(_HANDED)}
    return {"ended": "succeeded", "answered": answered, "handed": len(_HANDED)}
"#;
