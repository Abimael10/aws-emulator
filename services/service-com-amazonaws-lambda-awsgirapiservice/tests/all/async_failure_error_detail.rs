// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ERROR DETAIL the completion of an asynchronous invocation
//! reports: how much of the handler's own message the dead letter of a dropped event carries,
//! and what the invocation record of a run the function's own `Timeout` ended reports as the
//! response.
//!
//! Two rules are under test, each over a set of inputs rather than one case:
//!
//! * a dead letter records the failure's message truncated to the FIRST KILOBYTE of it — the
//!   `ErrorMessage` message attribute of a dead-lettered invocation carries the first 1 KB of
//!   the error message — while a shorter message travels whole, and the cut keeps the value
//!   the text it is, so a message of multi-byte characters is cut on a character boundary.
//!   Its `ErrorCode` is the status of the invocation that failed and is a `Number` attribute,
//!   and the request id beside it is a `String` one (`MessageAttributeValue$DataType`);
//! * a run the SANDBOX ended at the function's own `Timeout` — "the amount of time (in
//!   seconds) that Lambda allows a function to run before stopping it"
//!   (`smithy_query(query="shape", shape="Timeout")`) — is reported in the record's
//!   `responsePayload` as the platform's own error document and nothing besides: the class of
//!   the failure and the sentence about it, two members only, the sentence framing the
//!   invocation and the span the run was given (`RequestId: <id> Error: Task timed out after
//!   <timeout> seconds`, the span rendered to two decimals). No runtime serialized that
//!   document, so it carries neither a stack nor a second copy of the request id.
//!
//! Every row drives real HTTP requests through one router carrying Lambda and Amazon SQS on
//! ONE engine, so what is asserted is what a client reads off the queue the configuration
//! names. The function's package RUNS, so the failure reported is the handler's own and the
//! timeout is a run really ended by its span.
//!
//! Nothing here waits a span of its own: the activation window a create owes is brought
//! forward, and every wait looks again until what it waits for is there or a generous
//! deadline passes. The only waiting is the RUN's, inside the emulator, bounded by the
//! `Timeout` the row names.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::make_activation_due;
use crate::invoke_request_id::{is_host_bound, is_request_id, one_invocation_stream_at_a_time};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionEventInvokeConfig` is bound.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The Region every call below is made in.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about it and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The request header `Invoke$InvocationType` is bound to, and the value that queues the event
/// instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The `MaximumRetryAttempts` every configuration below states: none, the floor of the
/// member's own `@range(min: 0, max: 2)`. So each event has exactly ONE delivery attempt, and
/// what the dead letter or the record reports is that attempt's.
const NO_RETRIES: i64 = 0;

/// The message attributes a dead letter carries beside the event, and the `DataType` of each.
const REQUEST_ID_ATTRIBUTE: &str = "RequestID";
/// See [`REQUEST_ID_ATTRIBUTE`].
const ERROR_CODE_ATTRIBUTE: &str = "ErrorCode";
/// See [`REQUEST_ID_ATTRIBUTE`].
const ERROR_MESSAGE_ATTRIBUTE: &str = "ErrorMessage";
/// See [`REQUEST_ID_ATTRIBUTE`].
const STRING_TYPE: &str = "String";
/// See [`REQUEST_ID_ATTRIBUTE`].
const NUMBER_TYPE: &str = "Number";

/// The status an invocation that executed answers, which is what a dead letter's `ErrorCode`
/// records.
const INVOKED_STATUS: &str = "200";

/// The signal an invocation whose run FAILED carries, whatever ended it.
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// The class the platform reports a run it ended at the function's own `Timeout` under.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";

/// How long a row waits for work the CLOCK owes — the delivery attempt of an accepted event,
/// and the record or dead letter that follows it.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the dead-letter rows run: it RAISES with the message its event carries, so the
/// failure a dead letter reports is the handler's own and its length is the row's.
const RAISES_ITS_EVENTS_MESSAGE: &str = r#"
def lambda_handler(event, context):
    raise RuntimeError(event["message"])
"#;

/// The handler the timeout rows run: it asks to wait for far longer than its row's `Timeout`
/// allows, so the run is one the sandbox really ended at that span.
const WAITS_PAST_ITS_TIMEOUT: &str = r#"
import time


def lambda_handler(event, context):
    time.sleep(float(event["sleep"]))
    return {"returned": True}
"#;

/// How long the handler above asks to wait: well past every `Timeout` a row below names, so
/// what ends the run is that span and not the span asked for.
const ASKS_TO_WAIT_SECONDS: i64 = 30;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Lambda and Amazon SQS on ONE engine: a dead letter and a record are delivered through the
/// seam the invocation carries, so the queue they reach is this emulator's own.
fn router_with_queues(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
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
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
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
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/sqs/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A queue of this account: its url and its ARN, both as the queue itself reports them.
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

/// A function whose package RUNS `source` with `timeout` seconds to run in, carrying the
/// dead-letter target `dead_letter` names when the row asks for one, brought to `Active`.
///
/// The activation window is brought FORWARD rather than waited out ([`make_activation_due`]):
/// the transition is one the clock owes, so no row waits a span of its own for it.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    source: &str,
    timeout: i64,
    dead_letter: Option<&str>,
) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Timeout": timeout,
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
        },
    });
    if let Some(target) = dead_letter {
        body["DeadLetterConfig"] = json!({ "TargetArn": target });
    }
    let created = lambda(router, "POST", FUNCTIONS_URI, &body, &[]).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        created.text()
    );
    make_activation_due(engine);
    await_active(router, case, name).await;
}

/// Wait until `name` reports `State` `Active`, which is the move the clock owes after a create
/// and what every invocation of it waits for.
async fn await_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
            &[],
        )
        .await;
        if read.json()["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name:?} must become Active within {BUDGET:?}, got {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `PutFunctionEventInvokeConfig` on the unpublished version, allowing the event ONE delivery
/// attempt and naming the failure destination `on_failure` names when the row wants a record.
async fn given_single_attempt(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_failure: Option<&str>,
) {
    let mut body = json!({ "MaximumRetryAttempts": NO_RETRIES });
    if let Some(destination) = on_failure {
        body["DestinationConfig"] = json!({ "OnFailure": { "Destination": destination } });
    }
    let answer = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
        &body,
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

/// One asynchronous invocation of `function`, which the accepting call answers `202` for.
async fn invoke_async(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) {
    let answer = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        event,
        &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "case {case:?}: an asynchronous Invoke answered {} {}",
        answer.status,
        answer.text()
    );
}

/// One message of the queue `url` addresses, with its own message attributes, once one has
/// arrived — or a failure saying nothing did.
async fn awaited_message(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    url: &str,
) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, received) = sqs(
            router,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "MessageAttributeNames": ["All"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ReceiveMessage answered {status} {received}"
        );
        if let Some(message) = received["Messages"].as_array().and_then(|held| held.first()) {
            return message.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the {what} on {url} within {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A message of `length` ASCII characters, cycling one marked alphabet, so a value cut
/// anywhere but at the bound is told apart from one cut at it.
fn ascii_message(length: usize) -> String {
    "0123456789ABCDEF".chars().cycle().take(length).collect()
}

/// One row of the dead-letter table: the message the handler raises with, and how many bytes
/// of it the `ErrorMessage` attribute has to carry.
struct LetterRow {
    case: &'static str,
    /// The function and the queues the row drives, which are its own.
    name: &'static str,
    /// The message the handler raises with, which is the row's whole input.
    message: String,
    /// How many of that message's bytes the attribute carries.
    kept: usize,
}

/// The catalog: message lengths on both sides of the kilobyte the attribute is bound to, at
/// it, and one of multi-byte characters whose kilobyte ends mid-character.
fn letter_rows() -> Vec<LetterRow> {
    vec![
        LetterRow {
            case: "a message well inside the bound travels whole",
            name: "detail-short",
            message: ascii_message(64),
            kept: 64,
        },
        LetterRow {
            case: "a message of exactly the bound travels whole",
            name: "detail-exact",
            message: ascii_message(1024),
            kept: 1024,
        },
        LetterRow {
            case: "a message one character past the bound is cut at it",
            name: "detail-past",
            message: ascii_message(1025),
            kept: 1024,
        },
        LetterRow {
            case: "a message of several kilobytes keeps only the first",
            name: "detail-long",
            message: ascii_message(3000),
            kept: 1024,
        },
        LetterRow {
            // 1200 bytes of three-byte characters: the kilobyte ends inside the 342nd of
            // them, so the value kept is the 341 whole characters before it — 1023 bytes, the
            // most of this message that fits the bound while staying the text it is.
            case: "a message of multi-byte characters is cut on a character boundary",
            name: "detail-wide",
            message: "\u{20ac}".repeat(400),
            kept: 1023,
        },
    ]
}

/// RULE: the dead letter of a dropped asynchronous event records the failure's message
/// truncated to the first kilobyte of it — a shorter message whole — with the status of the
/// failed invocation as a `Number` attribute and the request id as a `String` one.
#[tokio::test]
async fn a_dead_letter_keeps_only_the_first_kilobyte_of_the_failures_message() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_queues(&engine);
    for row in letter_rows() {
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&router, case, &format!("{}-dlq", row.name)).await;
        given_function(
            &router,
            &engine,
            case,
            row.name,
            RAISES_ITS_EVENTS_MESSAGE,
            3,
            Some(&dead_letter_arn),
        )
        .await;
        given_single_attempt(&router, case, row.name, None).await;

        let event = json!({ "message": row.message });
        invoke_async(&router, case, row.name, &event).await;

        let letter = awaited_message(&router, case, "dead-lettered event", &dead_letter_url).await;
        // The body is the event payload EXACTLY as the caller supplied it, which is what makes
        // the dropped event replayable however long the failure's message was.
        let carried = letter["Body"].as_str().unwrap_or_default();
        assert_eq!(
            carried,
            event.to_string(),
            "case {case:?}: expected the dead letter to carry the event itself, got {carried:?}"
        );

        let attributes = &letter["MessageAttributes"];
        let expected = &row.message[..row.kept];
        let answered = attributes[ERROR_MESSAGE_ATTRIBUTE]["StringValue"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            answered.len(),
            row.kept,
            "case {case:?}: expected {} of the message's {} bytes, got {} in {attributes}",
            row.kept,
            row.message.len(),
            answered.len()
        );
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the first {} bytes of the handler's own message, got \
             {answered:?}",
            row.kept
        );
        let answered = attributes[ERROR_MESSAGE_ATTRIBUTE]["DataType"].as_str();
        assert_eq!(
            answered,
            Some(STRING_TYPE),
            "case {case:?}: the message is text, got {answered:?} in {attributes}"
        );
        let answered = attributes[ERROR_CODE_ATTRIBUTE]["DataType"].as_str();
        assert_eq!(
            answered,
            Some(NUMBER_TYPE),
            "case {case:?}: the status of the failed invocation is a number, got {answered:?} in \
             {attributes}"
        );
        let answered = attributes[ERROR_CODE_ATTRIBUTE]["StringValue"].as_str();
        assert_eq!(
            answered,
            Some(INVOKED_STATUS),
            "case {case:?}: expected the status an invocation that executed answers, got \
             {answered:?} in {attributes}"
        );
        let answered = attributes[REQUEST_ID_ATTRIBUTE]["DataType"].as_str();
        assert_eq!(
            answered,
            Some(STRING_TYPE),
            "case {case:?}: the request id is text, got {answered:?} in {attributes}"
        );
        let answered = attributes[REQUEST_ID_ATTRIBUTE]["StringValue"]
            .as_str()
            .unwrap_or_default();
        assert!(
            is_request_id(answered),
            "case {case:?}: expected the invocation's own request id, got {answered:?} in \
             {attributes}"
        );
    }
}

/// RULE: the failure record of an asynchronous invocation the SANDBOX ended at the function's
/// own `Timeout` reports the platform's error document as its `responsePayload` — the class of
/// the failure and the sentence framing the invocation and the span the run was given, and
/// nothing besides.
#[tokio::test]
async fn a_timed_out_async_invocation_records_the_platforms_timeout_document() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_queues(&engine);
    // (case, the resources the row drives, the `Timeout` the run is given) — the smallest span
    // the model allows (`@range(min: 1)`) and one above it, so the span the message states is
    // the row's own rather than one constant.
    let rows: &[(&str, &str, i64)] = &[
        ("the smallest Timeout the model allows", "timeout-one", 1),
        ("a Timeout of more than one second", "timeout-two", 2),
    ];
    // How many rows a RUN really answered: a row this process's own host bounds ended says
    // nothing about the rule ([`is_host_bound`]), so it is skipped rather than read.
    let mut read_rows = 0_usize;
    for (case, name, timeout) in rows {
        let (record_url, record_arn) = given_queue(&router, case, &format!("{name}-records")).await;
        given_function(
            &router,
            &engine,
            case,
            name,
            WAITS_PAST_ITS_TIMEOUT,
            *timeout,
            None,
        )
        .await;
        given_single_attempt(&router, case, name, Some(&record_arn)).await;

        invoke_async(
            &router,
            case,
            name,
            &json!({ "sleep": ASKS_TO_WAIT_SECONDS }),
        )
        .await;

        let message = awaited_message(&router, case, "invocation record", &record_url).await;
        let body = message["Body"].as_str().unwrap_or_default();
        let record: Value = serde_json::from_str(body)
            .unwrap_or_else(|_| panic!("case {case:?}: the record is no JSON document: {body}"));
        let payload = record["responsePayload"].clone();
        if is_host_bound(200, &payload.to_string()) {
            continue;
        }
        read_rows += 1;
        let request_id = record["requestContext"]["requestId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            is_request_id(&request_id),
            "case {case:?}: the record must name the request the event was accepted as, got \
             {record}"
        );
        let answered = record["responseContext"]["functionError"].as_str();
        assert_eq!(
            answered,
            Some(FUNCTION_ERROR_UNHANDLED),
            "case {case:?}: a run the sandbox ended is a failed invocation, got {answered:?} in \
             {record}"
        );
        let expected = json!({
            "errorType": SANDBOX_TIMEDOUT,
            "errorMessage": format!(
                "RequestId: {request_id} Error: Task timed out after {:.2} seconds",
                *timeout as f64
            ),
        });
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the platform's own timeout document {expected}, got \
             {payload} in {record}"
        );
    }
    assert!(
        read_rows > 0,
        "the rule has to be observed by a run the sandbox ended: every row was ended by this \
         process's own host bounds instead"
    );
}
