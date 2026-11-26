// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a failing asynchronous invocation is handed to the function
//! again: the wait the asynchronous event queue puts between two delivery attempts.
//!
//! RULE: an accepted asynchronous event whose handler errored is returned to the queue with
//! its next delivery attempt due the wait AWS publishes for that attempt — one minute before
//! the first retry, two minutes before the second, growing with the attempts spent — so the
//! handler is not entered again before that instant, and the age the retried attempt meters
//! (`AsyncEventAge`, "the time between the Lambda service receiving the event and the
//! invocation") is that wait rather than no wait at all. That wait is part of the time the
//! event spends queued, so a retry that would fall due past the window the event is retained
//! for ("the maximum age of a request that Lambda sends to a function for processing",
//! `FunctionEventInvokeConfig$MaximumEventAgeInSeconds`) is not made at all. A handler that
//! RETURNED ends the event, so nothing is scheduled for it.
//!
//! The rows differ along what the rule generalises over: how many retries the qualifier's
//! asynchronous-invocation configuration allows — each end of `MaximumRetryAttempts`' own
//! `@range(min: 0, max: 2)` — the window that configuration retains the event for, and what the
//! handler did with the event, which is what decides whether there is a next attempt at all.
//! Each row reads the instant the service itself put on the queue entry, so what is asserted is
//! the SCHEDULE the emulator made and never a span of this host's wall clock; the waits are then
//! brought FORWARD ([`make_next_attempt_due`]) rather than slept through, which is also how the
//! suites that drive a whole ladder of attempts reach the last of them.
//!
//! How MANY attempts an event gets is `async_retry_budget_attempts.rs`'s subject and what the
//! record the last of them earns carries is `async_destination_record_shape.rs`'s; this suite
//! is about the wait between two of them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::FUNCTION_ERROR_UNHANDLED;
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, AsyncInvocationRecord, ComAmazonawsLambdaAwsgirapiserviceState,
    FIRST_ASYNC_RETRY_DELAY, SECOND_ASYNC_RETRY_DELAY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{make_activation_due, router_on, send, ROLE};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionEventInvokeConfig` is bound.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The request header `Invoke$InvocationType` is bound to, and the value that queues the
/// event instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// How long a wait gives work the CLOCK owes: one queueing delay, one run of the handler, and
/// the entry being written back.
///
/// A ceiling on PATIENCE and never a span anything here counts on — every wait below looks
/// again until what it waits for is there or this deadline passes, so a loaded host is waited
/// for and only something that never arrives fails a row. The waits the retry policy puts
/// between two attempts are not part of it: a row reads the instant the service scheduled and
/// then brings it forward ([`make_next_attempt_due`]).
const BUDGET: Duration = Duration::from_secs(180);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How much LONGER than the published wait a scheduled attempt may be due: the instant is
/// written as the wait from the moment the attempt ended, and a row reads it afterwards, so
/// what a row sees can only be SHORTER than the wait. The room above it is for a host whose
/// clock the two reads do not share exactly.
const SCHEDULING_SLACK: Duration = Duration::from_secs(5);

/// The `Timeout` the function is created with.
///
/// Generous on purpose, because no row here is about the timeout: a handler stopped at this
/// deadline is reported as a run that errored for a reason the row did not ask for. Well
/// inside the range the model allows the member (`smithy_query(query="shape",
/// shape="Timeout")`: 1 to 5400).
const GENEROUS_TIMEOUT_SECONDS: u32 = 300;

/// The handler every function below runs: it raises when its event asks it to and returns
/// otherwise, so which attempt an entry records is evidence the guest ran on that event.
const HANDLER_SOURCE: &str = "def lambda_handler(event, context):\n\
                              \x20   if event.get('fail'):\n\
                              \x20       raise RuntimeError('the event asked this handler to fail')\n\
                              \x20   return {'received': event}\n";

/// Bring the next delivery attempt of every accepted asynchronous event forward to now, so
/// the attempt the retry policy scheduled is made at once instead of this suite sleeping
/// through the minute-scale waits AWS publishes between two of them.
///
/// The same move [`make_activation_due`] makes for the `Pending` -> `Active` transition: the
/// instant an attempt falls due lives on the queue entry, so moving it is what waiting would
/// do, without waiting. Answers whether any entry was still waiting for its attempt.
///
/// Shared with every suite that drives a ladder of attempts end to end.
pub(crate) fn make_next_attempt_due(engine: &Arc<engine::InMemoryEngine>) -> bool {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let now = SystemTime::now();
    let past = now - Duration::from_secs(1);
    let mut moved = false;
    for function in store.functions.values_mut() {
        for entry in &mut function.async_invocations {
            if entry.attempt_due > now {
                entry.attempt_due = past;
                moved = true;
            }
        }
    }
    moved
}

/// The queue entries Lambda holds for `name`, in queue order, read out of the service's own
/// state because no modelled operation reports an accepted asynchronous invocation back out.
fn held(engine: &Arc<engine::InMemoryEngine>, name: &str) -> Vec<AsyncInvocationRecord> {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    store
        .functions
        .get(name)
        .map(|function| function.async_invocations.iter().cloned().collect())
        .unwrap_or_default()
}

/// Create the function `name` running [`HANDLER_SOURCE`] and bring it to `Active`, stating the
/// retries and the retention window the row states.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    retries: Option<i64>,
    max_event_age: Option<i64>,
) {
    let created = send(
        router,
        "POST",
        FUNCTIONS_URI,
        "application/json",
        serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": GENEROUS_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(engine);
    let mut configuration = serde_json::Map::new();
    if let Some(retries) = retries {
        configuration.insert("MaximumRetryAttempts".to_string(), json!(retries));
    }
    if let Some(seconds) = max_event_age {
        configuration.insert("MaximumEventAgeInSeconds".to_string(), json!(seconds));
    }
    if !configuration.is_empty() {
        let configured = send(
            router,
            "PUT",
            &format!("{EVENT_INVOKE_CONFIG_URI}/{name}/event-invoke-config"),
            "application/json",
            serde_json::to_vec(&Value::Object(configuration))
                .expect("the configuration body serializes"),
            &[],
        )
        .await;
        assert_eq!(
            configured.status, 200,
            "case {case:?}: PutFunctionEventInvokeConfig answered {} {}",
            configured.status,
            String::from_utf8_lossy(&configured.body)
        );
    }
}

/// Queue one event for `function` through an asynchronous `Invoke`, which the accepting call
/// answers `202` for.
async fn invoked_asynchronously(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &str,
) {
    let accepted = send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        event.as_bytes().to_vec(),
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE.to_string())],
    )
    .await;
    assert_eq!(
        accepted.status, 202,
        "case {case:?}: an asynchronous Invoke answered {} {}",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
}

/// The one entry held for `function`, waited for until it reports `attempts` attempts made
/// and a next attempt SCHEDULED — the instant that attempt falls due still ahead — and then
/// answered as how long is left until that instant.
///
/// Looks again until it is there or [`BUDGET`] passes, so a loaded host is waited for and
/// only a schedule that never appears fails a row.
async fn scheduled_wait(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    function: &str,
    attempts: u32,
) -> Duration {
    let deadline = Instant::now() + BUDGET;
    loop {
        let entries = held(engine, function);
        let standing: Vec<String> = entries
            .iter()
            .map(|entry| format!("{} attempt(s), {:?}", entry.attempts, entry.outcome))
            .collect();
        if let Some(left) = entries
            .iter()
            .find(|entry| entry.attempts == attempts)
            .and_then(|entry| entry.attempt_due.duration_since(SystemTime::now()).ok())
            .filter(|left| !left.is_zero())
        {
            return left;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the entry to report {attempts} attempt(s) and a next \
             attempt still to come within {BUDGET:?}, got {standing:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The one entry held for `function`, waited for until its delivery is over: the handler ran
/// and no further attempt is coming.
///
/// Every look brings a scheduled attempt forward ([`make_next_attempt_due`]), so the ladder
/// runs out inside this wait instead of this suite sleeping through the published waits.
async fn settled(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    function: &str,
) -> AsyncInvocationRecord {
    let deadline = Instant::now() + BUDGET;
    loop {
        let entries = held(engine, function);
        if let Some(entry) = entries.iter().find(|entry| {
            !matches!(
                entry.outcome,
                AsyncInvocationOutcome::Queued | AsyncInvocationOutcome::Delivering
            )
        }) {
            return entry.clone();
        }
        let standing: Vec<String> = entries
            .iter()
            .map(|entry| format!("{} attempt(s), {:?}", entry.attempts, entry.outcome))
            .collect();
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the event's delivery to be over within {BUDGET:?}, got \
             {standing:?}"
        );
        make_next_attempt_due(engine);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: what the handler does with the event, how many retries the configuration allows,
/// and the wait that is due before each of the attempts that follow the first.
struct Row {
    case: &'static str,
    /// The function the row drives, so each row has a queue of its own.
    function: &'static str,
    /// The event the accepting call carries, which decides what the handler does with it.
    event: &'static str,
    /// The `MaximumRetryAttempts` the configuration states, or `None` for a function nobody
    /// configured.
    retries: Option<i64>,
    /// The `MaximumEventAgeInSeconds` the configuration states, where the row states one.
    max_event_age: Option<i64>,
    /// The published wait before each attempt after the first, in order.
    waits: &'static [Duration],
    /// The `FunctionError` the entry must carry once the delivery is over.
    function_error: Option<&'static str>,
}

/// RULE: a failing asynchronous event's next delivery attempt is due the published wait after
/// the attempt that errored — one minute, then two — a retry that would fall due past the
/// window the event is retained for is not made at all, and a handler that returned leaves no
/// attempt to come.
#[tokio::test]
async fn a_retried_attempt_is_due_the_published_wait_after_the_one_that_errored() {
    let rows: &[Row] = &[
        Row {
            case: "one retry, which waits the published minute",
            function: "backoff-one-retry",
            event: r#"{"id":1,"fail":true}"#,
            retries: Some(1),
            max_event_age: None,
            waits: &[FIRST_ASYNC_RETRY_DELAY],
            function_error: Some(FUNCTION_ERROR_UNHANDLED),
        },
        Row {
            case: "the published maximum of two retries, whose waits grow",
            function: "backoff-two-retries",
            event: r#"{"id":2,"fail":true}"#,
            retries: Some(2),
            max_event_age: None,
            waits: &[FIRST_ASYNC_RETRY_DELAY, SECOND_ASYNC_RETRY_DELAY],
            function_error: Some(FUNCTION_ERROR_UNHANDLED),
        },
        Row {
            case: "a configuration stating no retries, which has no attempt to come",
            function: "backoff-no-retries",
            event: r#"{"id":3,"fail":true}"#,
            retries: Some(0),
            max_event_age: None,
            waits: &[],
            function_error: Some(FUNCTION_ERROR_UNHANDLED),
        },
        Row {
            // The smallest window `MaximumEventAgeInSeconds` admits (`@range(min: 60, max:
            // 21600)`) is shorter than the wait before the first retry, so the retries this
            // row allows are never made: the event is not retained long enough to reach them.
            case: "a window shorter than the wait, which allows no retry at all",
            function: "backoff-window-spent",
            event: r#"{"id":4,"fail":true}"#,
            retries: Some(2),
            max_event_age: Some(60),
            waits: &[],
            function_error: Some(FUNCTION_ERROR_UNHANDLED),
        },
        Row {
            case: "a handler that returned, which ends the event",
            function: "backoff-handler-returns",
            event: r#"{"id":5}"#,
            retries: Some(2),
            max_event_age: None,
            waits: &[],
            function_error: None,
        },
    ];

    for row in rows {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        given_function(
            &router,
            &engine,
            row.case,
            row.function,
            row.retries,
            row.max_event_age,
        )
        .await;
        invoked_asynchronously(&router, row.case, row.function, row.event).await;

        // Each attempt after the first: the entry reports the attempts made so far and the
        // instant the next one falls due, which is the published wait for it. The wait is
        // then brought forward, so the attempt is made at once and the row reads the next
        // schedule.
        let mut made: u32 = 1;
        for (index, published) in row.waits.iter().enumerate() {
            let case = row.case;
            let left = scheduled_wait(&engine, case, row.function, made).await;
            assert!(
                left > *published / 2 && left <= published.saturating_add(SCHEDULING_SLACK),
                "case {case:?}: expected attempt {} to be due about {published:?} after the \
                 one that errored, got {left:?} left",
                made + 1
            );
            // The waits GROW with the attempts spent, which is what a client watching the age
            // of its event sees.
            if let Some(earlier) = index.checked_sub(1).and_then(|before| row.waits.get(before)) {
                assert!(
                    left > *earlier,
                    "case {case:?}: expected attempt {} to wait longer than the {earlier:?} \
                     before the one that errored, got {left:?} left",
                    made + 1
                );
            }
            // No further attempt has been made while the event waits: the handler is entered
            // again only once the wait is over.
            let entries = held(&engine, row.function);
            let attempts: Vec<u32> = entries.iter().map(|entry| entry.attempts).collect();
            assert_eq!(
                attempts,
                vec![made],
                "case {case:?}: expected the handler not to be entered again before the \
                 attempt fell due, got {attempts:?} attempt(s)"
            );
            assert!(
                make_next_attempt_due(&engine),
                "case {case:?}: expected an attempt still waiting to be brought forward"
            );
            made += 1;
        }

        let entry = settled(&engine, row.case, row.function).await;
        let case = row.case;
        let AsyncInvocationOutcome::Delivered { function_error } = &entry.outcome else {
            panic!(
                "case {case:?}: expected the handler to have run with this event, got {:?}",
                entry.outcome
            )
        };
        assert_eq!(
            function_error.as_deref(),
            row.function_error,
            "case {case:?}: expected the last attempt to report {:?}, got {function_error:?}",
            row.function_error
        );
        assert_eq!(
            entry.attempts,
            made,
            "case {case:?}: expected {made} delivery attempt(s), got {}",
            entry.attempts
        );
    }
}
