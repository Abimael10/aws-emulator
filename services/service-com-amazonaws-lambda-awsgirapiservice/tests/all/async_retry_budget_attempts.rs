// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW MANY times a failing asynchronous event reaches the handler, and
//! the record the last of those attempts earns.
//!
//! RULE: an accepted asynchronous event whose handler errors is handed to the function once
//! more for each retry the qualifier's asynchronous-invocation configuration allows — "the
//! maximum number of times to retry when the function returns an error"
//! (`FunctionEventInvokeConfig$MaximumRetryAttempts`), inside that member's own `@range(min:
//! 0, max: 2)` — so the handler ENTERS exactly `1 + MaximumRetryAttempts` times with the same
//! event, two of those retries where the configuration states none ("if you invoke a function
//! asynchronously and it returns an error, Lambda executes the function up to two more
//! times"), and then exactly one invocation record reaches the destination
//! `DestinationConfig$OnFailure` names, reporting those entries as
//! `requestContext.approximateInvokeCount` and `RetriesExhausted` as
//! `requestContext.condition`.
//!
//! The rows differ along what the rule generalises over: the retries the configuration allows
//! — each bound of the published range and the value between them — and a configuration that
//! states no `MaximumRetryAttempts` at all, which is the documented default. Every row is
//! observed through the function's OWN side effect: the handler writes one counting message
//! carrying the event's marker and its invocation's request id before raising, so an attempt
//! that never ran the handler leaves nothing and an attempt the retry budget did not allow
//! shows up as a message too many.
//!
//! What a record CARRIES is `async_destination_record_shape.rs`'s subject, and that a record
//! is delivered at all whatever queued the event is
//! `async_retry_exhaustion_destination.rs`'s; this suite is about the LENGTH of the ladder of
//! attempts, which is what a client relying on Lambda to retry a transient failure reads the
//! retry policy through. `async_retry_budget_from_stored_config.rs` drives that same ladder
//! for a budget `UpdateFunctionEventInvokeConfig` stated, through the wiring and the one
//! reader of what a ladder leaves behind this file shares with it ([`assert_each_ladder`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    ComAmazonawsLambdaAwsgirapiserviceState, ASYNC_DELIVERY_DELAY, FIRST_ASYNC_RETRY_DELAY,
    SECOND_ASYNC_RETRY_DELAY,
};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionEventInvokeConfig` is bound, and the other two operations of that
/// resource with it.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`]; see [`assert_each_ladder`].
pub(crate) const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The Region every call below is made in, and so the Region of every ARN it mints.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about the role and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The request header `Invoke$InvocationType` is bound to, and the value that queues the
/// event instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The `requestContext.condition` of a record for an event whose attempts are spent.
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// How long a wait gives work the CLOCK owes: the whole ladder of attempts an event has, the
/// waits between them included, and the record that follows the last of them.
///
/// A ceiling on PATIENCE and never a span anything here counts on: every wait below looks
/// again until what it waits for is there or this deadline passes, so a loaded host — where
/// an attempt that finds no capacity to run the function is made again rather than counted —
/// is waited for, and only something that never arrives fails a row.
const BUDGET: Duration = ASYNC_DELIVERY_DELAY
    .saturating_add(FIRST_ASYNC_RETRY_DELAY)
    .saturating_add(SECOND_ASYNC_RETRY_DELAY)
    .saturating_add(Duration::from_secs(240));

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long this suite keeps watching every queue AFTER every row has the attempts its
/// configuration allows and its record.
///
/// Twice the LONGEST wait the service puts between two attempts
/// ([`SECOND_ASYNC_RETRY_DELAY`]), so an attempt past the budget — or a second record for one
/// event — would be delivered inside this window rather than after it.
const QUIET_WINDOW: Duration = SECOND_ASYNC_RETRY_DELAY.saturating_mul(2);

/// The handler every function below runs: it writes ONE counting message — the event's own
/// marker, and the request id of the invocation it is running under — and then raises, so
/// every attempt errors and the ladder of attempts is always spent.
const HANDLER_SOURCE: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    boto3.client("sqs").send_message(
        QueueUrl=event["attempts"],
        MessageBody=json.dumps(
            {"marker": event["marker"], "requestId": context.aws_request_id}
        ),
    )
    raise RuntimeError("the handler refused this event")
"#;

/// Lambda beside the queue service the counting messages and the invocation records are read
/// on — all on ONE engine, so every write the handler and the destination delivery make is
/// where a client reads it.
pub(crate) fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. Nothing
/// in it is verified: the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda control-plane or data-plane call, as its status and its body.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`]; see [`assert_each_ladder`].
pub(crate) async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(
            builder
                .body(bytes::Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body().as_ref()).to_string(),
    )
}

/// One Amazon SQS call (`POST /` with `X-Amz-Target: AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A queue of this account, as the queue itself reports it: its url and its ARN.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`]; see [`assert_each_ladder`].
pub(crate) async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue {name:?} answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A function whose package RUNS [`HANDLER_SOURCE`], brought to `Active`.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`]; see [`assert_each_ladder`].
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 10,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction {name:?} answered {status} {created}"
    );
    make_active(engine);
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` transition the clock owes rather than this suite
/// waiting out that span.
fn make_active(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = std::time::SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// `PutFunctionEventInvokeConfig` on the function's unpublished version: the retries it
/// allows, when the row states any, and the queue its failed invocations' records go to.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`], whose rows PUT their
/// destination this way and then state the budget in force through an update; see
/// [`assert_each_ladder`].
pub(crate) async fn given_event_invoke_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_failure: &str,
    retries: Option<i64>,
) {
    let mut body = json!({
        "DestinationConfig": { "OnFailure": { "Destination": on_failure } },
    });
    if let Some(retries) = retries {
        body["MaximumRetryAttempts"] = json!(retries);
    }
    let (status, answered) = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
        &body,
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionEventInvokeConfig answered {status} {answered}"
    );
}

/// Queue one event for `function` through an asynchronous `Invoke`, which the accepting call
/// answers `202` for.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`]; see [`assert_each_ladder`].
pub(crate) async fn invoked_asynchronously(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        event,
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: an asynchronous Invoke answered {status} {answered}"
    );
}

/// Every message waiting on the queue `url` addresses, as documents, taken off it so a later
/// look sees only what arrived since.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    let mut bodies = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage answered {status} {deleted}"
            );
        }
        let body = message["Body"].as_str().unwrap_or_default();
        bodies.push(serde_json::from_str(body).unwrap_or(Value::Null));
    }
    bodies
}

/// One row: how many retries the configuration allows, and how many times the handler
/// therefore ENTERS for one event.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own two queues.
    function: &'static str,
    /// The `MaximumRetryAttempts` the configuration states, or `None` for one that states
    /// none at all.
    retries: Option<i64>,
    /// The entries the handler makes for the event: the first invocation plus the retries
    /// above, which is what the record reports as `approximateInvokeCount`.
    attempts: usize,
}

/// What one row is observed through, once it is wired and its event queued.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`], whose rows state the budget
/// in force through an update; see [`assert_each_ladder`].
pub(crate) struct Ladder {
    /// The row this wiring belongs to, which every assertion below names.
    pub(crate) case: &'static str,
    /// The queue the handler writes its counting message to on every entry.
    pub(crate) attempts_url: String,
    /// The queue the qualifier's `DestinationConfig$OnFailure` names.
    pub(crate) failure_url: String,
    /// The marker this row's event carries, which every attempt of it repeats.
    pub(crate) marker: String,
    /// The entries into the handler the budget in force allows this row's event: the first
    /// invocation plus the retries the configuration holds.
    pub(crate) attempts: usize,
}

/// RULE: a failing asynchronous event enters the handler `1 + MaximumRetryAttempts` times —
/// the documented two retries where the configuration states no value — with the same event
/// each time, and then earns exactly one `RetriesExhausted` record reporting those entries.
#[tokio::test]
async fn a_failing_event_enters_the_handler_once_plus_the_retries_it_is_allowed() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    // The retries each row allows: both ends of the published range, the value between them,
    // and a configuration that states none at all.
    let rows: &[Row] = &[
        Row {
            case: "a configuration stating no MaximumRetryAttempts, which retries twice",
            function: "retry-budget-unstated",
            retries: None,
            attempts: 3,
        },
        Row {
            case: "the published minimum, no retries",
            function: "retry-budget-none",
            retries: Some(0),
            attempts: 1,
        },
        Row {
            case: "one retry",
            function: "retry-budget-one",
            retries: Some(1),
            attempts: 2,
        },
        Row {
            case: "the published maximum, two retries",
            function: "retry-budget-two",
            retries: Some(2),
            attempts: 3,
        },
    ];

    // Every row is wired and its event queued first, so the ladders of attempts run alongside
    // one another rather than one suite-length wait per row.
    let mut wired: Vec<Ladder> = Vec::new();
    for row in rows {
        let (attempts_url, _) =
            given_queue(&router, row.case, &format!("{}-attempts", row.function)).await;
        let (failure_url, failure_arn) =
            given_queue(&router, row.case, &format!("{}-failure", row.function)).await;
        given_function(&router, &engine, row.case, row.function).await;
        given_event_invoke_config(&router, row.case, row.function, &failure_arn, row.retries)
            .await;
        let marker = format!("{}-marker", row.function);
        invoked_asynchronously(
            &router,
            row.case,
            row.function,
            &json!({ "marker": marker, "attempts": attempts_url }),
        )
        .await;
        wired.push(Ladder {
            case: row.case,
            attempts_url,
            failure_url,
            marker,
            attempts: row.attempts,
        });
    }

    assert_each_ladder(&router, &wired).await;
}

/// Assert the RULE over every wired row whose event has been queued: the handler is entered
/// exactly [`Ladder::attempts`] times, every entry carrying the queued event's marker and one
/// invocation's request id; no further entry and no second record arrive inside the quiet
/// window; and exactly one record reports `RetriesExhausted` with those entries as
/// `requestContext.approximateInvokeCount`.
///
/// Shared with [`crate::async_retry_budget_from_stored_config`], so the two suites that drive
/// a ladder of attempts have ONE reader of what it leaves behind. Every wait looks again until
/// what it waits for is there or [`BUDGET`] passes, so a loaded host — where an attempt that
/// finds no capacity to run the function is made again rather than counted — is waited for,
/// and only something that never arrives fails a row.
pub(crate) async fn assert_each_ladder(router: &MultiServiceRouter, ladders: &[Ladder]) {
    // What has been taken off each row's two queues so far, one entry per ladder.
    let mut counted: Vec<Vec<Value>> = ladders.iter().map(|_| Vec::new()).collect();
    let mut records: Vec<Vec<Value>> = ladders.iter().map(|_| Vec::new()).collect();

    // Look again until every row has the attempts its configuration allows AND its record, or
    // the patience above runs out: what each wait reports is what the emulator did rather than
    // how fast this host is.
    let deadline = Instant::now() + BUDGET;
    loop {
        for (index, ladder) in ladders.iter().enumerate() {
            let more = drained(router, ladder.case, &ladder.attempts_url).await;
            counted[index].extend(more);
            let again = drained(router, ladder.case, &ladder.failure_url).await;
            records[index].extend(again);
        }
        let complete = ladders.iter().enumerate().all(|(index, ladder)| {
            counted[index].len() >= ladder.attempts && !records[index].is_empty()
        });
        if complete {
            break;
        }
        let progress: Vec<String> = ladders
            .iter()
            .enumerate()
            .map(|(index, ladder)| {
                format!(
                    "{:?}: {} of {} attempts, {} record(s)",
                    ladder.case,
                    counted[index].len(),
                    ladder.attempts,
                    records[index].len()
                )
            })
            .collect();
        assert!(
            Instant::now() < deadline,
            "expected every row's attempts and its invocation record within {BUDGET:?}, got \
             {progress:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // Keep watching: an attempt the budget did not allow, or a second record for one event,
    // arrives inside this window rather than after it.
    let quiet_until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < quiet_until {
        for ladder in ladders {
            let case = ladder.case;
            let more = drained(router, case, &ladder.attempts_url).await;
            assert!(
                more.is_empty(),
                "case {case:?}: expected exactly {} entries into the handler, got another \
                 {more:?}",
                ladder.attempts
            );
            let again = drained(router, case, &ladder.failure_url).await;
            assert!(
                again.is_empty(),
                "case {case:?}: expected exactly one invocation record, got another {again:?}"
            );
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    for (index, ladder) in ladders.iter().enumerate() {
        let case = ladder.case;
        let entries = &counted[index];
        assert_eq!(
            entries.len(),
            ladder.attempts,
            "case {case:?}: expected the handler to be entered {} times, got {entries:?}",
            ladder.attempts
        );
        // Every retry is of the SAME event under the SAME invocation, so every entry carries
        // the marker the accepting call queued and that invocation's own request id.
        let request_ids: Vec<&str> = entries
            .iter()
            .map(|message| message["requestId"].as_str().unwrap_or_default())
            .collect();
        for message in entries {
            assert_eq!(
                message["marker"].as_str(),
                Some(ladder.marker.as_str()),
                "case {case:?}: expected every entry to carry the queued event's marker, got \
                 {message}"
            );
        }
        assert!(
            request_ids
                .first()
                .is_some_and(|first| !first.is_empty() && request_ids.iter().all(|id| id == first)),
            "case {case:?}: expected every entry to run under one invocation's request id, got \
             {request_ids:?}"
        );
        let invocation = request_ids.first().copied().unwrap_or_default();

        let held_records = &records[index];
        assert_eq!(
            held_records.len(),
            1,
            "case {case:?}: expected exactly one invocation record, got {held_records:?}"
        );
        let record = &held_records[0];
        // (the member the record carries, the value it must carry)
        let stated: &[(&str, Value)] = &[
            (
                "/requestContext/condition",
                json!(CONDITION_RETRIES_EXHAUSTED),
            ),
            (
                "/requestContext/approximateInvokeCount",
                json!(ladder.attempts),
            ),
            ("/requestContext/requestId", json!(invocation)),
            ("/requestPayload/marker", json!(ladder.marker)),
        ];
        for (pointer, expected) in stated {
            let read = record.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {pointer} to be {expected}, got {read} in {record}"
            );
        }
    }
}
