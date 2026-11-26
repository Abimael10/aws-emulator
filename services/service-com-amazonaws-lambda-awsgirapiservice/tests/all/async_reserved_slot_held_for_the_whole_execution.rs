// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CONCURRENCY an asynchronous invocation holds while it executes,
//! and for what becomes of a second event that cannot be admitted inside its own age window.
//!
//! RULE: a delivery attempt of an accepted asynchronous event — which no caller is waiting
//! for — holds one slot of the function's reserved concurrency for the whole span of its
//! execution: "The number of simultaneous executions to reserve for the function", which
//! "appl[ies] to the function as a whole" (`PutFunctionConcurrency`), so no further invocation
//! of that function is admitted while its pool is full, however long the running handler
//! occupies it. An accepted event that finds no slot is held on the queue and attempted again;
//! one still unadmitted when "The maximum age of a request that Lambda sends to a function for
//! processing" (`FunctionEventInvokeConfig$MaximumEventAgeInSeconds`) is spent is discarded and
//! reported to the `DestinationConfig$OnFailure` destination as the age-out it is, while one
//! that finds a slot inside that window runs and is reported to `OnSuccess`.
//!
//! The rows differ along what the rule generalises over: how long the execution occupies the
//! slot (past the whole window, and well inside it), how many slots the function reserves (one
//! and two), and the age window in force — at both bounds of the member's own published range
//! (`smithy_query(query="shape", shape="MaximumEventAgeInSeconds")`: `@range(min: 60, max:
//! 21600)`). Every row makes the SAME two back-to-back `Invoke` calls with
//! `InvocationType=Event` and allows the same retries, so the only thing deciding how many
//! handler executions happen and where each event's record lands is the rule above. Each row
//! carries its own three queues, so a delivery made for another row, or made twice, fails too.
//!
//! The reservation that admits NOTHING at any instant is
//! `async_zero_reserved_concurrency_destination.rs`'s subject, and what a record CARRIES in
//! general is `async_destination_record_shape.rs`'s; this suite is about the slot a running
//! execution holds and the window a throttled event is held inside.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every call below is made in, and so the Region of every ARN it mints.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
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

/// The status the accepting call of an asynchronous invocation answers: "A success response
/// (202 Accepted) indicates that the request is queued for invocation".
const ACCEPTED_STATUS: u16 = 202;

/// The `requestContext.condition` of a record of an event whose handler RETURNED.
const CONDITION_SUCCESS: &str = "Success";

/// The `requestContext.condition` of a record of an event discarded because its maximum age
/// was spent before the function could be run for it.
const CONDITION_EVENT_AGE_EXCEEDED: &str = "EventAgeExceeded";

/// The smallest and largest `MaximumEventAgeInSeconds` the model admits
/// (`smithy_query(query="shape", shape="MaximumEventAgeInSeconds")`: `@range(min: 60, max:
/// 21600)`), which are the two windows the rows below are driven at.
const SMALLEST_EVENT_AGE_SECONDS: i64 = 60;
/// See [`SMALLEST_EVENT_AGE_SECONDS`].
const LARGEST_EVENT_AGE_SECONDS: i64 = 21_600;

/// The retries every row allows, the published maximum
/// (`smithy_query(query="shape", shape="MaximumRetryAttempts")`: `@range(min: 0, max: 2)`): it
/// is the same for every row, so no difference between them can be a difference in retries.
const RETRIES: i64 = 2;

/// The `Timeout` every function below is created with: longer than any span one of these
/// handlers occupies its slot for, so what ends a run is the handler's own return and never
/// the function's own span.
const TIMEOUT_SECONDS: i64 = 100;

/// How long an execution that outlasts the smallest age window holds its slot, in the seconds
/// its handler asks to wait for.
const OCCUPY_PAST_THE_WINDOW: f64 = 70.0;

/// How long one that gives the slot back well inside that window holds it.
const OCCUPY_INSIDE_THE_WINDOW: f64 = 2.0;

/// How long one held for longer than this host waits a handler's wait out, but inside no
/// window: a throttled event of such a function is admitted once that execution is over.
const OCCUPY_PAST_THE_HOSTS_WAIT: f64 = 25.0;

/// How long a row waits for work the CLOCK owes: the queueing delay of an accepted event, the
/// executions that happen, the throttled attempts of the event that waits for a slot, the
/// window it is held inside and the completion record each event earns.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host is waited for
/// and only something that never arrives fails a row. Generous beside the one span a row does
/// pin, which is the age window it configures through the API.
const BUDGET: Duration = Duration::from_secs(240);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The environment variable the handler reads the side-channel queue's url from.
const RAN_QUEUE_VARIABLE: &str = "RAN_QUEUE_URL";

/// The environment variable it reads how long to occupy its slot for from.
const OCCUPY_VARIABLE: &str = "OCCUPY_SECONDS";

/// The handler every function below runs: it reports the nonce of the event it was handed to
/// its own side-channel queue, occupies its execution environment for the span its
/// configuration names and then RETURNS that nonce — so the side channel is the count of
/// invocations that really reached the code, and a returning handler makes every event that
/// reached it a success.
const HANDLER_SOURCE: &str = r#"
import json
import os
import time

import boto3

QUEUES = boto3.client("sqs")
RAN_QUEUE_URL = os.environ["RAN_QUEUE_URL"]
OCCUPY_SECONDS = float(os.environ["OCCUPY_SECONDS"])


def lambda_handler(event, context):
    QUEUES.send_message(QueueUrl=RAN_QUEUE_URL, MessageBody=json.dumps(event))
    time.sleep(OCCUPY_SECONDS)
    return {"nonce": event["nonce"]}
"#;

/// Lambda beside the queues every delivery of this suite is made to — all on ONE engine, so
/// every record is written where a client reads it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. Nothing
/// in it is verified: the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda control-plane or data-plane call, the way a client makes it.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Answer {
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
    Answer {
        status: response.status().as_u16(),
        body: response.body().clone(),
    }
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
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
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

/// A function whose package RUNS [`HANDLER_SOURCE`], occupying its slot for `occupy` seconds
/// and reporting to the side-channel queue `ran_queue_url`, brought to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    ran_queue_url: &str,
    occupy: f64,
) {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": TIMEOUT_SECONDS,
            "Environment": {
                "Variables": {
                    RAN_QUEUE_VARIABLE: ran_queue_url,
                    OCCUPY_VARIABLE: occupy.to_string(),
                },
            },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        created.text()
    );
    awaited_active(router, case, name).await;
}

/// Wait until `function` reports `State: Active`, which is the state the model says an
/// invocation needs — looked for again until it is there or the patience above runs out, so
/// nothing here waits a span of its own choosing.
async fn awaited_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &Value::Null,
            &[],
        )
        .await;
        let configuration: Value = serde_json::from_slice(&read.body).unwrap_or(Value::Null);
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {function:?} to become Active within {BUDGET:?}, got \
             {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `PutFunctionConcurrency` (`PUT /2017-10-31/functions/{FunctionName}/concurrency`): the
/// executions the function reserves, confirmed by `GetFunctionConcurrency`.
async fn given_reservation(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    reserved: i64,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("/2017-10-31/functions/{function}/concurrency"),
        &json!({ "ReservedConcurrentExecutions": reserved }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: PutFunctionConcurrency answered {} {}",
        answer.status,
        answer.text()
    );
    let read = lambda(
        router,
        "GET",
        &format!("/2019-09-30/functions/{function}/concurrency"),
        &Value::Null,
        &[],
    )
    .await;
    let reported: Value = serde_json::from_slice(&read.body).unwrap_or(Value::Null);
    assert_eq!(
        reported["ReservedConcurrentExecutions"].as_i64(),
        Some(reserved),
        "case {case:?}: expected the function to reserve {reserved}, got {reported}"
    );
}

/// `PutFunctionEventInvokeConfig` on the function's unpublished version: the retries it
/// allows, the age window its events are held inside, and the queues its successful and
/// failed invocations' records go to.
async fn given_event_invoke_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_success: &str,
    on_failure: &str,
    window_seconds: i64,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("/2019-09-25/functions/{function}/event-invoke-config"),
        &json!({
            "MaximumRetryAttempts": RETRIES,
            "MaximumEventAgeInSeconds": window_seconds,
            "DestinationConfig": {
                "OnSuccess": { "Destination": on_success },
                "OnFailure": { "Destination": on_failure },
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: PutFunctionEventInvokeConfig answered {} {}",
        answer.status,
        answer.text()
    );
}

/// Queue one event for `function` through an asynchronous `Invoke`, which the accepting call
/// answers [`ACCEPTED_STATUS`] for.
async fn invoked_asynchronously(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    nonce: &str,
) {
    let answer = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        &json!({ "nonce": nonce }),
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
    )
    .await;
    assert_eq!(
        answer.status,
        ACCEPTED_STATUS,
        "case {case:?}: an asynchronous Invoke answered {} {}",
        answer.status,
        answer.text()
    );
}

/// Every message waiting on the queue `url` addresses, taken off it so a later look sees only
/// what arrived since.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
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
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
    }
    bodies
}

/// The messages on the queue `url` addresses once `wanted` of them have arrived, looked for
/// again until they are there or the patience above runs out.
async fn awaited(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    url: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        held.extend(drained(router, case, url).await);
        if held.len() >= wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} {what} on {url} within {BUDGET:?}, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `nonce` each of `records` reports as its `requestPayload`, as a set: which EVENTS the
/// records are of, which is what the rule decides — the nonces themselves are this run's own.
fn nonces_of(case: &str, records: &[String]) -> BTreeSet<String> {
    records
        .iter()
        .map(|body| {
            let record: Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("case {case:?}: {body:?} is no document: {error}"));
            record
                .pointer("/requestPayload/nonce")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no requestPayload nonce in {record}"))
                .to_string()
        })
        .collect()
}

/// The `nonce` each message of the handler's own side channel carries.
fn ran_nonces(case: &str, bodies: &[String]) -> BTreeSet<String> {
    bodies
        .iter()
        .map(|body| {
            let event: Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("case {case:?}: {body:?} is no document: {error}"));
            event["nonce"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no nonce in {event}"))
                .to_string()
        })
        .collect()
}

/// The `requestContext.condition` every one of `records` reports.
fn conditions_of(case: &str, records: &[String]) -> Vec<String> {
    records
        .iter()
        .map(|body| {
            let record: Value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("case {case:?}: {body:?} is no document: {error}"));
            record
                .pointer("/requestContext/condition")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One row: what the function reserves, how long each of its executions occupies a slot, the
/// age window its events are held inside, and what the two back-to-back events therefore come
/// to.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own three queues.
    function: &'static str,
    /// The `ReservedConcurrentExecutions` the row puts on it.
    reserved: i64,
    /// The seconds its handler occupies its slot for.
    occupy: f64,
    /// The `MaximumEventAgeInSeconds` its events are held inside.
    window_seconds: i64,
    /// How many of the two events reach the handler code.
    runs: usize,
    /// How many are completed as successes.
    successes: usize,
    /// How many are discarded because their age window was spent.
    age_outs: usize,
}

/// The queues one row's deliveries are made to.
struct Queues {
    /// The handler's own side channel: one message per invocation that reached the code.
    ran_url: String,
    /// Where the records of its successful invocations go.
    success_url: String,
    /// Where the records of its failed ones go.
    failure_url: String,
}

/// RULE: an asynchronous event's delivery attempt holds one slot of the function's reserved
/// concurrency for the whole span of its execution, so a second event is admitted only once a
/// slot is free — and an event still unadmitted when its `MaximumEventAgeInSeconds` is spent
/// is discarded to the `OnFailure` destination as an age-out instead of being run later.
#[tokio::test]
async fn an_execution_holds_its_reserved_slot_until_the_event_ages_out() {
    // The crate-wide invocation gate, taken for as long as this suite is running handlers:
    // the executions below occupy their environments for tens of seconds, and the windows the
    // rows configure are REAL seconds — a row whose event is waiting for another suite's share
    // of the run envelope would be throttled for a reason that is not the rule under test.
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[Row] = &[
        Row {
            case: "one reserved slot an execution holds past the whole age window",
            function: "slot-held-past-the-window",
            reserved: 1,
            occupy: OCCUPY_PAST_THE_WINDOW,
            window_seconds: SMALLEST_EVENT_AGE_SECONDS,
            runs: 1,
            successes: 1,
            age_outs: 1,
        },
        Row {
            case: "one reserved slot an execution gives back well inside the window",
            function: "slot-free-inside-the-window",
            reserved: 1,
            occupy: OCCUPY_INSIDE_THE_WINDOW,
            window_seconds: SMALLEST_EVENT_AGE_SECONDS,
            runs: 2,
            successes: 2,
            age_outs: 0,
        },
        Row {
            case: "two reserved slots for the two events",
            function: "two-slots-for-two-events",
            reserved: 2,
            occupy: OCCUPY_PAST_THE_WINDOW,
            window_seconds: SMALLEST_EVENT_AGE_SECONDS,
            runs: 2,
            successes: 2,
            age_outs: 0,
        },
        Row {
            case: "one reserved slot inside the largest age window the model admits",
            function: "slot-held-inside-the-largest-window",
            reserved: 1,
            occupy: OCCUPY_PAST_THE_HOSTS_WAIT,
            window_seconds: LARGEST_EVENT_AGE_SECONDS,
            runs: 2,
            successes: 2,
            age_outs: 0,
        },
    ];

    // Every row is wired and its two events queued first, so the deliveries happen alongside
    // one another rather than one suite-length wait per row.
    let mut wired: Vec<(Queues, [String; 2])> = Vec::new();
    for row in rows {
        let case = row.case;
        let (ran_url, _ran_arn) =
            given_queue(&router, case, &format!("{}-ran", row.function)).await;
        let (success_url, success_arn) =
            given_queue(&router, case, &format!("{}-success", row.function)).await;
        let (failure_url, failure_arn) =
            given_queue(&router, case, &format!("{}-failure", row.function)).await;
        given_function(&router, case, row.function, &ran_url, row.occupy).await;
        given_event_invoke_config(
            &router,
            case,
            row.function,
            &success_arn,
            &failure_arn,
            row.window_seconds,
        )
        .await;
        given_reservation(&router, case, row.function, row.reserved).await;
        let nonces = [
            format!("first-{}", row.function),
            format!("second-{}", row.function),
        ];
        for nonce in &nonces {
            invoked_asynchronously(&router, case, row.function, nonce).await;
        }
        wired.push((
            Queues {
                ran_url,
                success_url,
                failure_url,
            },
            nonces,
        ));
    }

    for (row, (queues, nonces)) in rows.iter().zip(&wired) {
        let case = row.case;
        let queued: BTreeSet<String> = nonces.iter().cloned().collect();

        // The completion records are what say the events are FINISHED, so they are waited for
        // first: once every event has earned its record, no further execution can happen and
        // the side channel below is the whole count of invocations that reached the code.
        let successes = awaited(
            &router,
            case,
            "invocation record(s) of a successful invocation",
            &queues.success_url,
            row.successes,
        )
        .await;
        let failures = if row.age_outs > 0 {
            awaited(
                &router,
                case,
                "invocation record(s) of a discarded event",
                &queues.failure_url,
                row.age_outs,
            )
            .await
        } else {
            Vec::new()
        };
        // Both events are accounted for now, so anything still on either destination is a
        // record too many — including one on the slot the completions did not choose.
        let mut successes = successes;
        successes.extend(drained(&router, case, &queues.success_url).await);
        let mut failures = failures;
        failures.extend(drained(&router, case, &queues.failure_url).await);
        assert_eq!(
            successes.len(),
            row.successes,
            "case {case:?}: expected {} record(s) on the success destination, got {successes:?}",
            row.successes
        );
        assert_eq!(
            failures.len(),
            row.age_outs,
            "case {case:?}: expected {} record(s) on the failure destination, got {failures:?}",
            row.age_outs
        );
        assert_eq!(
            conditions_of(case, &successes),
            vec![CONDITION_SUCCESS.to_string(); row.successes],
            "case {case:?}: expected every success record to report the success condition, \
             got {successes:?}"
        );
        assert_eq!(
            conditions_of(case, &failures),
            vec![CONDITION_EVENT_AGE_EXCEEDED.to_string(); row.age_outs],
            "case {case:?}: expected every failure record to report the age-out condition, \
             got {failures:?}"
        );

        // HOW MANY invocations reached the handler code, which is what the reserved slot
        // decides: the side channel holds one message per execution, and a look for one more
        // after the records have arrived is what would catch an execution too many.
        let mut side_channel = awaited(
            &router,
            case,
            "side-channel message(s) of an invocation that ran",
            &queues.ran_url,
            row.runs,
        )
        .await;
        side_channel.extend(drained(&router, case, &queues.ran_url).await);
        let ran = ran_nonces(case, &side_channel);
        assert_eq!(
            ran.len(),
            row.runs,
            "case {case:?}: expected {} invocation(s) to reach the handler, got {ran:?}",
            row.runs
        );

        // Each event is completed exactly once, and the event an age-out discarded is the one
        // that never ran: the records account for both events between them.
        let succeeded = nonces_of(case, &successes);
        let aged_out = nonces_of(case, &failures);
        assert_eq!(
            succeeded, ran,
            "case {case:?}: expected the success records to name the events that ran, got \
             {succeeded:?} against {ran:?}"
        );
        assert!(
            aged_out.is_disjoint(&ran),
            "case {case:?}: expected no discarded event to have reached the handler, got \
             {aged_out:?} against {ran:?}"
        );
        let accounted: BTreeSet<String> = succeeded.union(&aged_out).cloned().collect();
        assert_eq!(
            accounted, queued,
            "case {case:?}: expected the records to account for both queued events, got \
             {accounted:?} against {queued:?}"
        );

        // A record of an event nothing ran reports no response at all, because there was no
        // run to report a status or a payload for.
        for body in &failures {
            let record: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            assert_eq!(
                record.pointer("/responsePayload").cloned(),
                None,
                "case {case:?}: expected no response payload for a discarded event, got \
                 {record}"
            );
        }
    }
}
