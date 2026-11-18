// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what becomes of an accepted ASYNCHRONOUS event when the function's
//! own concurrency reservation admits no invocation of it at all.
//!
//! RULE: an asynchronous event is handed to the function only while the function's reserved
//! concurrency pool has room for it — "The number of simultaneous executions to reserve for
//! the function", which "appl[ies] to the function as a whole" (`PutFunctionConcurrency`) —
//! so a reservation of ZERO is a pool nothing can ever release a slot of: the handler is
//! never run for the event, the event is not kept on the queue waiting for capacity, and the
//! invocation is completed as a FAILED one — exactly one invocation record to the destination
//! the qualifier's `DestinationConfig$OnFailure` names, reporting the reservation as the
//! `requestContext.condition` and carrying no response at all, and the event itself to the
//! function's `DeadLetterConfig$TargetArn`. A function that reserves capacity, and one that
//! reserves none, both RUN the event instead and are completed as the successes they are.
//!
//! The rows differ along what the rule generalises over: the reservation in force (zero, one
//! execution, and no reservation at all) and how many retries the qualifier's
//! `MaximumRetryAttempts` allows (none and the published maximum of two), so an emulator that
//! ran the handler anyway, that kept retrying a reservation nothing can satisfy, or that
//! routed the completion to the wrong destination slot fails here. Each row carries its own
//! three queues, so a delivery made for another row, or made twice, fails too.
//!
//! What a record CARRIES in general is `async_destination_record_shape.rs`'s subject, and the
//! ladder of attempts an ERRORING handler earns is `async_retry_exhaustion_destination.rs`'s;
//! this suite is about the one ending that reaches no handler.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, ComAmazonawsLambdaAwsgirapiserviceState,
};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction`, `Invoke` and `UpdateFunctionConfiguration` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionEventInvokeConfig` is bound.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The Region every call below is made in, and so the Region of every ARN it mints.
const REGION: &str = "us-east-1";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

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

/// The `requestContext.condition` of a record of an event whose handler RETURNED.
const CONDITION_SUCCESS: &str = "Success";

/// The `requestContext.condition` of a record of an event the function reserves no
/// concurrency to run: the ending AWS words for an invocation no reservation admitted, beside
/// `Success`, `RetriesExhausted` and `EventAgeExceeded`.
const CONDITION_ZERO_RESERVED_CONCURRENCY: &str = "ZeroReservedConcurrency";

/// How long a row waits for work the CLOCK owes: the queueing delay of an accepted event, the
/// run of the handler where one happens, and the completion that follows it.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host is waited for
/// and only something that never arrives fails a row.
const BUDGET: Duration = Duration::from_secs(180);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every function below runs: it RETURNS, so an event that reached it is a
/// success and the record of it would be delivered to the success destination with the
/// document below as its response payload. A row whose record arrives on the FAILURE
/// destination with no response at all is therefore a row whose handler never ran.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"ran": True}
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

/// A function whose package RUNS [`HANDLER_SOURCE`], brought to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
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
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        created.text()
    );
    make_active(engine);
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` transition the clock owes rather than this suite
/// sleeping through it.
fn make_active(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = std::time::SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// `PutFunctionConcurrency` (`PUT /2017-10-31/functions/{FunctionName}/concurrency`): the
/// executions the function reserves.
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
}

/// `PutFunctionEventInvokeConfig` on the function's unpublished version: the retries it
/// allows, and the queues its successful and failed invocations' records go to.
async fn given_event_invoke_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_success: &str,
    on_failure: &str,
    retries: i64,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
        &json!({
            "MaximumRetryAttempts": retries,
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

/// `UpdateFunctionConfiguration` (`PUT /2015-03-31/functions/{FunctionName}/configuration`):
/// the dead-letter queue the function's failed asynchronous invocations are written to.
async fn given_dead_letter_queue(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    target: &str,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        &json!({ "DeadLetterConfig": { "TargetArn": target } }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: UpdateFunctionConfiguration answered {} {}",
        answer.status,
        answer.text()
    );
}

/// The unqualified ARN of the function `name` names.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// Queue one event for `function` through an asynchronous `Invoke`, which the accepting call
/// answers `202` for.
async fn invoked_asynchronously(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) {
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

/// Wait until every event accepted for `function` has SETTLED — no entry of its queue is
/// still queued or being delivered — and answer what each one came to.
async fn settled(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    function: &str,
) -> Vec<AsyncInvocationOutcome> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let entries: Vec<AsyncInvocationOutcome> = {
            let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
            let store = state.lock();
            store
                .functions
                .get(function)
                .map(|held| {
                    held.async_invocations
                        .iter()
                        .map(|entry| entry.outcome.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        let waiting = entries.iter().any(|outcome| {
            matches!(
                outcome,
                AsyncInvocationOutcome::Queued | AsyncInvocationOutcome::Delivering
            )
        });
        if !entries.is_empty() && !waiting {
            return entries;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every event accepted for {function:?} must settle within \
             {BUDGET:?}, got {entries:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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

/// The messages on the queue `url` addresses once there is at least one, looked for again
/// until they are there or the patience above runs out.
async fn awaited(router: &MultiServiceRouter, case: &str, what: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        held.extend(drained(router, case, url).await);
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what} on {url} within {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What the function's reservation is for one row.
#[derive(Clone, Copy, Debug)]
enum Reservation {
    /// `PutFunctionConcurrency` reserved this many executions.
    Reserved(i64),
    /// Nobody called `PutFunctionConcurrency` at all, so the function reserves nothing and is
    /// bounded by the account's capacity instead.
    Unreserved,
}

/// One row: the reservation in force, the retries the configuration allows, and whether the
/// handler therefore RUNS for the event.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own three queues.
    function: &'static str,
    reservation: Reservation,
    /// The `MaximumRetryAttempts` the configuration states, inside the member's own
    /// `@range(min: 0, max: 2)`.
    retries: i64,
    /// Whether the event is handed to the handler at all.
    runs: bool,
}

/// RULE: an accepted asynchronous event of a function that reserves NO concurrency is never
/// run and is completed as a failure — one record on the `OnFailure` destination reporting
/// the reservation and carrying no response, and the event itself on the dead-letter queue —
/// while a function with capacity runs the event and is completed as the success it is.
#[tokio::test]
async fn an_event_of_a_function_that_reserves_no_concurrency_fails_without_running() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[Row] = &[
        Row {
            case: "a reservation of zero with no retries allowed",
            function: "zero-reserved-none",
            reservation: Reservation::Reserved(0),
            retries: 0,
            runs: false,
        },
        Row {
            case: "a reservation of zero with every retry allowed",
            function: "zero-reserved-two",
            reservation: Reservation::Reserved(0),
            retries: 2,
            runs: false,
        },
        Row {
            case: "a reservation of one execution",
            function: "one-reserved",
            reservation: Reservation::Reserved(1),
            retries: 0,
            runs: true,
        },
        Row {
            case: "no reservation at all",
            function: "unreserved",
            reservation: Reservation::Unreserved,
            retries: 2,
            runs: true,
        },
    ];

    // Every row is wired and its event queued first, so the deliveries happen alongside one
    // another rather than one suite-length wait per row.
    let mut queues: Vec<(String, String, String)> = Vec::new();
    for row in rows {
        let (success_url, success_arn) =
            given_queue(&router, row.case, &format!("{}-success", row.function)).await;
        let (failure_url, failure_arn) =
            given_queue(&router, row.case, &format!("{}-failure", row.function)).await;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&router, row.case, &format!("{}-dead-letters", row.function)).await;
        given_function(&router, &engine, row.case, row.function).await;
        given_dead_letter_queue(&router, row.case, row.function, &dead_letter_arn).await;
        given_event_invoke_config(
            &router,
            row.case,
            row.function,
            &success_arn,
            &failure_arn,
            row.retries,
        )
        .await;
        if let Reservation::Reserved(reserved) = row.reservation {
            given_reservation(&router, row.case, row.function, reserved).await;
        }
        queues.push((success_url, failure_url, dead_letter_url));
        invoked_asynchronously(
            &router,
            row.case,
            row.function,
            &json!({ "probe": row.function }),
        )
        .await;
    }

    for (row, (success_url, failure_url, dead_letter_url)) in rows.iter().zip(&queues) {
        let case = row.case;
        let event = json!({ "probe": row.function });
        let outcomes = settled(&engine, case, row.function).await;
        let ran = matches!(
            outcomes.as_slice(),
            [AsyncInvocationOutcome::Delivered { .. }]
        );
        assert_eq!(
            ran, row.runs,
            "case {case:?}: expected the event {} have been handed to the handler, got \
             {outcomes:?}",
            if row.runs { "to" } else { "not to" }
        );

        // Where the record goes is the slot the completion chose, and both slots are written
        // by the same step of the pipeline: the queue that was NOT chosen is read once the
        // record has arrived on the one that was, so a record delivered to the wrong slot is
        // caught rather than waited for.
        let (chosen, withheld) = if row.runs {
            (success_url, failure_url)
        } else {
            (failure_url, success_url)
        };
        let held = awaited(&router, case, "one invocation record", chosen).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one invocation record on {chosen}, got {held:?}"
        );
        let record: Value = serde_json::from_str(&held[0])
            .unwrap_or_else(|error| panic!("case {case:?}: the record is no document: {error}"));
        let other = drained(&router, case, withheld).await;
        assert!(
            other.is_empty(),
            "case {case:?}: expected nothing on the destination slot the completion did not \
             choose ({withheld}), got {other:?}"
        );

        // (the member the record carries, the value it must carry)
        let stated: &[(&str, Value)] = &[
            (
                "/requestContext/functionArn",
                json!(format!("{}:$LATEST", function_arn(row.function))),
            ),
            (
                "/requestContext/condition",
                json!(if row.runs {
                    CONDITION_SUCCESS
                } else {
                    CONDITION_ZERO_RESERVED_CONCURRENCY
                }),
            ),
            ("/requestPayload", event.clone()),
        ];
        for (pointer, expected) in stated {
            let read = record.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {pointer} to be {expected}, got {read} in {record}"
            );
        }
        assert!(
            record
                .pointer("/requestContext/requestId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty()),
            "case {case:?}: the record must name the invocation's request id, got {record}"
        );

        // A record of an invocation that RAN reports what the handler answered; a record of
        // one nothing ran reports no response at all, because there was no run to report a
        // status or a payload for.
        let response = record.pointer("/responsePayload").cloned();
        let invokes = record
            .pointer("/requestContext/approximateInvokeCount")
            .and_then(Value::as_u64);
        if row.runs {
            assert_eq!(
                response,
                Some(json!({ "ran": true })),
                "case {case:?}: expected the record to carry what the handler answered, got \
                 {record}"
            );
            assert_eq!(
                invokes,
                Some(1),
                "case {case:?}: expected the record to report the one invocation the event \
                 had, got {record}"
            );
        } else {
            assert_eq!(
                response, None,
                "case {case:?}: expected a record with no response payload for an event \
                 nothing ran, got {record}"
            );
            assert_eq!(
                record.pointer("/responseContext").cloned(),
                None,
                "case {case:?}: expected a record with no response context for an event \
                 nothing ran, got {record}"
            );
            assert_eq!(
                invokes,
                Some(0),
                "case {case:?}: expected the record to report that the function was never \
                 invoked, got {record}"
            );
        }

        // The dead-letter queue carries the EVENT, exactly as the caller supplied it, for the
        // failed invocation and nothing at all for the successful one.
        if row.runs {
            let dead_letters = drained(&router, case, dead_letter_url).await;
            assert!(
                dead_letters.is_empty(),
                "case {case:?}: expected nothing on the dead-letter queue of a successful \
                 invocation, got {dead_letters:?}"
            );
        } else {
            let dead_letters =
                awaited(&router, case, "the dead-lettered event", dead_letter_url).await;
            let bodies: Vec<Value> = dead_letters
                .iter()
                .map(|body| serde_json::from_str(body).unwrap_or(Value::Null))
                .collect();
            assert_eq!(
                bodies,
                vec![event.clone()],
                "case {case:?}: expected the event itself on the dead-letter queue, got \
                 {dead_letters:?}"
            );
        }
    }
}
