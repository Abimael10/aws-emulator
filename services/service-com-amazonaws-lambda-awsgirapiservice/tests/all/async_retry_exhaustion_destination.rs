// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the invocation RECORD an asynchronous event earns once the attempts
//! its configuration allows are spent, whatever QUEUED the event.
//!
//! RULE: an accepted asynchronous event whose handler errored on the last attempt the
//! qualifier's `MaximumRetryAttempts` allows ("the maximum number of times to retry when the
//! function returns an error") is a FAILED invocation, so exactly one invocation record is
//! delivered to the destination that qualifier's `DestinationConfig$OnFailure` names ("the
//! destination configuration for failed invocations"), carrying
//! `requestContext.condition` `RetriesExhausted`, the attempts the event really had as
//! `requestContext.approximateInvokeCount`, and the event itself as `requestPayload` — and
//! this holds for every way an event reaches that queue, because a push source that invokes
//! the function asynchronously queues the SAME event on the SAME queue as a direct
//! `Invoke` with `InvocationType=Event` does.
//!
//! The rows differ along what the rule generalises over: WHO queued the event (a direct
//! asynchronous `Invoke`, and a publish to a topic the function is subscribed to over the
//! `lambda` protocol — "lambda – delivery of JSON-encoded message to an Lambda function",
//! `SubscribeInput$Protocol`) and how many retries the configuration allows (none, and one),
//! so a record that only ever arrives for one source or only when nothing is retried fails
//! here. Each row carries its own destination queue, so a record delivered for another row,
//! or twice, fails too.
//!
//! What the attempts THEMSELVES are is `async_invocation_delivery.rs`'s subject and the
//! `state::async_delivery` table's; what a record CARRIES is
//! `async_destination_record_shape.rs`'s. This suite is about the record being delivered at
//! all once the ladder of attempts is spent, from either kind of source.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, ComAmazonawsLambdaAwsgirapiserviceState, FIRST_ASYNC_RETRY_DELAY,
};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// Where `CreateFunction`, `Invoke` and `AddPermission` are bound.
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

/// The service principal Amazon SNS delivers to a subscribed function as, and the action
/// that delivery performs — the grant without which no notification reaches the function.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";
/// See [`SNS_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `requestContext.condition` of a record of an event whose attempts are spent, and the
/// `responseContext.functionError` of an attempt whose handler raised.
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";
/// See [`CONDITION_RETRIES_EXHAUSTED`].
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// How long a row waits for work the CLOCK owes: the ladder of attempts an event has — the
/// wait before the first retry included ([`FIRST_ASYNC_RETRY_DELAY`]) — and the
/// record that follows the last of them.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host — where an
/// attempt that finds no capacity to run the function is made again rather than counted — is
/// waited for, and only something that never arrives fails a row.
const BUDGET: Duration = FIRST_ASYNC_RETRY_DELAY.saturating_add(Duration::from_secs(180));

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a row keeps looking at its destination AFTER the record it expects has arrived,
/// before it accepts that nothing else was delivered there.
///
/// Generous on purpose: it is far longer than the delivery of the record that WAS owed takes
/// on any host — that record has already arrived by the time this window opens — so a second
/// delivery of one event would be seen inside it.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The handler every function below runs: it always raises, so every attempt of every event
/// errors and the ladder of attempts is always spent.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    raise RuntimeError("the handler refused this event")
"#;

/// The error the handler above raises, as the error document a failed run answers with reports
/// it: the exception's own class and message, which is what the record's `responsePayload`
/// carries.
const HANDLER_ERROR_TYPE: &str = "RuntimeError";
/// See [`HANDLER_ERROR_TYPE`].
const HANDLER_ERROR_MESSAGE: &str = "the handler refused this event";

/// Lambda beside the two services a push-sourced completion is observed through: the topic a
/// notification is published to, and the queue the record is delivered to — all three on ONE
/// engine, so every delivery is made where a client reads it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
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

/// One Amazon SNS call, in the `awsQuery` form that service's protocol binds.
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", "2010-03-31");
    for (name, value) in parameters {
        form.append_pair(name, value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(bytes::Bytes::from(form.finish()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The text one XML element of an `awsQuery` answer carries.
fn element(document: &str, name: &str) -> Option<String> {
    let opening = format!("<{name}>");
    let closing = format!("</{name}>");
    let from = document.find(&opening)? + opening.len();
    let to = document[from..].find(&closing)? + from;
    Some(document[from..to].to_string())
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

/// `PutFunctionEventInvokeConfig` on the function's unpublished version: the retries it
/// allows, and the queue its failed invocations' records go to.
async fn given_event_invoke_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_failure: &str,
    retries: i64,
) {
    let answer = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
        &json!({
            "MaximumRetryAttempts": retries,
            "DestinationConfig": { "OnFailure": { "Destination": on_failure } },
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

/// The unqualified ARN of the function `name` names, which is the endpoint a `lambda`
/// subscription carries.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// Queue one event for `function` through a direct asynchronous `Invoke`, which the accepting
/// call answers `202` for.
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

/// Queue one event for `function` the way a PUSH SOURCE does: a topic the function is
/// subscribed to over the `lambda` protocol, with the grant Amazon SNS needs on the function,
/// published to with `message`.
async fn published_to_a_subscribed_function(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    message: &str,
) {
    let (status, created) = sns(router, "CreateTopic", &[("Name", &format!("{function}-topic"))]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic answered {status} {created}"
    );
    let topic = element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TopicArn in {created}"));

    // A notification reaches a subscribed function only where the FUNCTION's own
    // resource-based policy grants Amazon SNS the invocation.
    let granted = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        &json!({
            "StatementId": "sns-invoke",
            "Action": INVOKE_ACTION,
            "Principal": SNS_PRINCIPAL,
            "SourceArn": topic,
        }),
        &[],
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "case {case:?}: AddPermission answered {} {}",
        granted.status,
        granted.text()
    );

    let (status, subscribed) = sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "lambda"),
            ("Endpoint", &function_arn(function)),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Subscribe answered {status} {subscribed}"
    );

    let (status, published) =
        sns(router, "Publish", &[("TopicArn", &topic), ("Message", message)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Publish answered {status} {published}"
    );
    assert!(
        element(&published, "MessageId").is_some_and(|id| !id.is_empty()),
        "case {case:?}: Publish must answer a MessageId, got {published}"
    );
}

/// Wait until every event accepted for `function` has SETTLED — no entry of its queue is
/// still waiting for an attempt or being delivered — and answer what each one came to.
///
/// What the entries record is read rather than how many attempts were MADE for them: an
/// attempt that found no capacity to run the function ran nothing and is made again
/// (`state::async_delivery::next_throttled_attempt_in`), so on a loaded host one event can be
/// attempted more often than its retry allowance without the allowance being spent. How many
/// times the function really RAN is what the record reports, and that is where each row
/// asserts it.
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
async fn awaited(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        held.extend(drained(router, case, url).await);
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one invocation record on {url} within {BUDGET:?}, got none"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Keep looking at the queue `url` addresses until [`QUIET_WINDOW`] has passed, failing the
/// moment anything more is delivered there — which is how a record delivered TWICE for one
/// event is caught.
async fn stayed_quiet(router: &MultiServiceRouter, case: &str, url: &str) {
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let more = drained(router, case, url).await;
        assert!(
            more.is_empty(),
            "case {case:?}: expected exactly one invocation record on {url}, got another {more:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What queued the event a row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// A direct `Invoke` with `InvocationType=Event`.
    Invoke,
    /// A `Publish` to a topic the function is subscribed to over the `lambda` protocol.
    Topic,
}

/// One row: what queued the event, how many retries the configuration allows, and how many
/// times the function therefore RAN for it.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own destination queue.
    function: &'static str,
    source: Source,
    /// The `MaximumRetryAttempts` the configuration states, inside the member's own
    /// `@range(min: 0, max: 2)`.
    retries: i64,
    /// How many times the handler ran for the event: the first invocation plus the retries
    /// above, which is what the record reports as `approximateInvokeCount`.
    invocations: i64,
}

/// RULE: an asynchronous event whose attempts are spent on an erroring handler delivers
/// exactly one invocation record — `RetriesExhausted`, the attempts it really had, and the
/// event itself — to the destination its qualifier's `DestinationConfig$OnFailure` names,
/// whichever source queued the event.
#[tokio::test]
async fn an_event_whose_attempts_are_spent_delivers_one_record_to_its_failure_destination() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[Row] = &[
        Row {
            case: "a direct asynchronous Invoke with one retry",
            function: "exhausted-invoke-one",
            source: Source::Invoke,
            retries: 1,
            invocations: 2,
        },
        Row {
            case: "a publish to a subscribed function with no retries",
            function: "exhausted-topic-none",
            source: Source::Topic,
            retries: 0,
            invocations: 1,
        },
        Row {
            case: "a publish to a subscribed function with one retry",
            function: "exhausted-topic-one",
            source: Source::Topic,
            retries: 1,
            invocations: 2,
        },
    ];

    // Every row is wired and its event queued first, so the ladders of attempts run
    // alongside one another rather than one suite-length wait per row.
    let mut queues: Vec<String> = Vec::new();
    for row in rows {
        let (url, arn) =
            given_queue(&router, row.case, &format!("{}-failure", row.function)).await;
        given_function(&router, &engine, row.case, row.function).await;
        given_event_invoke_config(&router, row.case, row.function, &arn, row.retries).await;
        queues.push(url);
        let marker = format!("{}-probe", row.function);
        match row.source {
            Source::Invoke => {
                invoked_asynchronously(&router, row.case, row.function, &json!({ "probe": marker }))
                    .await;
            }
            Source::Topic => {
                published_to_a_subscribed_function(&router, row.case, row.function, &marker).await;
            }
        }
    }

    for (row, url) in rows.iter().zip(&queues) {
        let case = row.case;
        let marker = format!("{}-probe", row.function);
        let settled_outcomes = settled(&engine, case, row.function).await;
        assert_eq!(
            settled_outcomes,
            vec![AsyncInvocationOutcome::Delivered {
                function_error: Some(FUNCTION_ERROR_UNHANDLED.to_string()),
            }],
            "case {case:?}: expected the one accepted event to have been delivered to the \
             function and its handler to have errored, got {settled_outcomes:?}"
        );
        let held = awaited(&router, case, url).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one invocation record, got {held:?}"
        );
        stayed_quiet(&router, case, url).await;
        let record: Value = serde_json::from_str(&held[0])
            .unwrap_or_else(|error| panic!("case {case:?}: the record is no document: {error}"));
        // (the member the record carries, the value it must carry)
        let stated: &[(&str, Value)] = &[
            (
                "/requestContext/functionArn",
                json!(format!("{}:$LATEST", function_arn(row.function))),
            ),
            ("/requestContext/condition", json!(CONDITION_RETRIES_EXHAUSTED)),
            (
                "/requestContext/approximateInvokeCount",
                json!(row.invocations),
            ),
            (
                "/responseContext/functionError",
                json!(FUNCTION_ERROR_UNHANDLED),
            ),
            // The response payload is the error document the handler's own raise produced,
            // which is what makes the record a failure a consumer can act on.
            ("/responsePayload/errorType", json!(HANDLER_ERROR_TYPE)),
            ("/responsePayload/errorMessage", json!(HANDLER_ERROR_MESSAGE)),
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
        // The `requestPayload` is the event the function was handed, which is the event the
        // row's own source queued: the document a direct invoke carried, and the `Records`
        // envelope a notification is delivered inside.
        let payload = record
            .pointer("/requestPayload")
            .cloned()
            .unwrap_or(Value::Null);
        match row.source {
            Source::Invoke => assert_eq!(
                payload,
                json!({ "probe": marker }),
                "case {case:?}: expected the record to carry the invoked event, got {payload}"
            ),
            Source::Topic => assert_eq!(
                payload
                    .pointer("/Records/0/Sns/Message")
                    .and_then(Value::as_str),
                Some(marker.as_str()),
                "case {case:?}: expected the record to carry the published notification, got \
                 {payload}"
            ),
        }
    }
}
