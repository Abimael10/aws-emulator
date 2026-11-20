// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call naming a queue that is GONE from a live SNS → SQS pipeline —
//! through the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles for the
//! binary.
//!
//! The RULE under test: **a request naming a queue this account does not hold is refused HTTP
//! 400 in the `QueueDoesNotExist` class — reported beside its legacy query code
//! `AWS.SimpleQueueService.NonExistentQueue` — carrying Amazon SQS's own fixed sentence for
//! that absence, "The specified queue does not exist.", whatever operation was called, however
//! the queue was addressed (its `QueueUrl`, or the `QueueName` the one name-addressed operation
//! resolves), whichever dialect the client speaks, and whatever another service's wiring still
//! names it.** The sentence is a CONSTANT: the error code is what identifies the condition, so
//! nothing of the request — no queue name, url, account or topic — is interpolated into it, and
//! a subscription still pointing at the gone queue adds no clause of its own.
//!
//! Two services have to agree for that to hold: SNS keeps a subscription whose endpoint is the
//! queue's ARN, and Amazon SQS decides every call on the queue itself. The Amazon SQS crate's
//! own `queue_does_not_exist` suite drives the table of operations and absence forms against
//! that service mounted alone; what only this suite can reach is the same refusal taken on the
//! PRODUCTION service set, with the queue reached as the destination of a pipeline that was
//! delivering until the queue was deleted — the path a client watching a fan-out takes, and the
//! one on which a stale destination is detected at all. It also pins the other half of the
//! rule's independence: the pipeline beside it goes on delivering, so the refusal is about the
//! absent queue and not about the fan-out.
//!
//! The rows differ along what the rule generalises over: the OPERATION called (every one of
//! this service that resolves a queue from a url, plus the name-addressed one), the FORM of the
//! absence (a url whose queue was deleted out from under a live subscription, a url of a queue
//! the account never created, a name that was deleted, a name that never existed), and the
//! DIALECT the client speaks (the native JSON one, and the query-compatible one an SDK opts into
//! with the query-mode header).

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{element, form_encode};

/// The Region the whole wiring of this suite lives in, and the one every call is signed for.
const REGION: &str = "us-east-1";

/// Amazon SQS's own sentence for a queue the account does not hold, written out here rather
/// than read off the service, so what this suite asserts is AWS's text and not the emulator's
/// idea of it.
///
/// RECORDED: a `SendMessage` on the url of a deleted queue, and a `GetQueueUrl` for a name the
/// account holds no queue under, are both refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` with exactly this text. The model
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no message text, so the served sentence is the requirement; it names no queue, so it
/// has no variable part.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The class the refusal names, as the `awsJson1_0` discriminator carries it, and the status
/// the model binds to it (`httpError: 400`).
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";
const REFUSAL_STATUS: u16 = 400;

/// The legacy query code and fault side reported beside that class, from the model's
/// `awsQueryError` trait — the pair an `awsQueryCompatible` service reports on every error, and
/// the code a client reads as `Error.Code`.
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";

/// The response header that report arrives in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The request header a query-compatible SDK opts into the legacy error report with, and the
/// value it sends.
const QUERY_MODE_HEADER: &str = "x-amzn-query-mode";
const QUERY_MODE_ON: &str = "true";

/// The dialect one call is made in: a native JSON client sends no opt-in, a query-compatible
/// one sends the header above.
#[derive(Debug, Clone, Copy)]
enum Dialect {
    Native,
    QueryCompatible,
}

impl Dialect {
    fn opt_in(self) -> Option<&'static str> {
        match self {
            Self::Native => None,
            Self::QueryCompatible => Some(QUERY_MODE_ON),
        }
    }
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), as its HTTP status, its response
/// headers and its parsed body — the seam a refusal is read through, so nothing here asserts a
/// status of its own.
async fn sqs_answer(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    dialect: Dialect,
) -> (u16, http::HeaderMap, Value) {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"));
    if let Some(value) = dialect.opt_in() {
        builder = builder.header(QUERY_MODE_HEADER, value);
    }
    let request = builder
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// The same call, which must be served.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, _, parsed) = sqs_answer(router, operation, body, Dialect::Native).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One SNS call over the `awsQuery` form wire that service serves, which must answer 200.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sns", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {text}"
    );
    text
}

/// The class a refusal names: the `__type` discriminator's own local name.
fn class_of(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The sentence a refusal carries, under either spelling of the member.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The value of one response header, or `None` when the answer carries none.
fn header_of(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

/// The url the same endpoint and account would address the queue called `name` at, built off a
/// url this emulator itself minted so no row writes the address form by hand.
fn url_of_name(minted: &str, name: &str) -> String {
    match minted.rfind('/') {
        Some(cut) => format!("{}/{name}", &minted[..cut]),
        None => format!("{minted}/{name}"),
    }
}

/// Every operation of this service that resolves a queue from a `QueueUrl`, each with the rest
/// of a request that is otherwise well formed — so the only thing its refusal can be about is
/// the queue.
///
/// The list is every url-addressed operation of the service's own surface
/// (`smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// that takes a `QueueUrl` and can be called with no other resource of its own.
fn url_requests(url: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("SendMessage", json!({"QueueUrl": url, "MessageBody": "gone"})),
        (
            "SendMessageBatch",
            json!({"QueueUrl": url, "Entries": [{"Id": "one", "MessageBody": "gone"}]}),
        ),
        ("ReceiveMessage", json!({"QueueUrl": url})),
        (
            "DeleteMessage",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle"}),
        ),
        (
            "DeleteMessageBatch",
            json!({"QueueUrl": url,
                   "Entries": [{"Id": "one", "ReceiptHandle": "not-a-handle"}]}),
        ),
        (
            "ChangeMessageVisibility",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle", "VisibilityTimeout": 30}),
        ),
        (
            "GetQueueAttributes",
            json!({"QueueUrl": url, "AttributeNames": ["All"]}),
        ),
        (
            "SetQueueAttributes",
            json!({"QueueUrl": url, "Attributes": {"VisibilityTimeout": "10"}}),
        ),
        ("ListQueueTags", json!({"QueueUrl": url})),
        (
            "TagQueue",
            json!({"QueueUrl": url, "Tags": {"owner": "pipeline"}}),
        ),
        ("UntagQueue", json!({"QueueUrl": url, "TagKeys": ["owner"]})),
        ("PurgeQueue", json!({"QueueUrl": url})),
        ("ListDeadLetterSourceQueues", json!({"QueueUrl": url})),
    ]
}

/// Assert one answer is the absence refusal, whole: the status, the class, the legacy report
/// beside it, and AWS's fixed sentence — and that the sentence interpolated nothing of the
/// request.
fn assert_absence_refusal(
    case: &str,
    operation: &str,
    dialect: Dialect,
    probed: &[&str],
    answered: (u16, http::HeaderMap, Value),
) {
    let (status, headers, body) = answered;
    let report = header_of(&headers, QUERY_ERROR_HEADER);
    let message = message_of(&body);
    assert_eq!(
        (
            status,
            class_of(&body).as_str(),
            message.as_str(),
            report.clone()
        ),
        (
            REFUSAL_STATUS,
            QUEUE_DOES_NOT_EXIST,
            AWS_SENTENCE,
            Some(NON_EXISTENT_QUEUE_REPORT.to_string())
        ),
        "case {case:?} / {operation} as a {dialect:?} client: expected HTTP {REFUSAL_STATUS} \
         {QUEUE_DOES_NOT_EXIST} reading {AWS_SENTENCE:?} reported as \
         {NON_EXISTENT_QUEUE_REPORT:?}, got {status} with {body} and {QUERY_ERROR_HEADER} \
         {report:?}"
    );
    for value in probed {
        assert!(
            !message.contains(value),
            "case {case:?} / {operation} as a {dialect:?} client: the sentence must interpolate \
             nothing of the request, got {message:?} naming {value:?}"
        );
    }
}

/// Every operation that resolves a queue reads the one fixed sentence for a queue that is gone,
/// in both dialects, over every form of the absence — with the gone queue still named by a live
/// SNS subscription.
#[tokio::test]
async fn every_sqs_call_on_a_queue_gone_from_its_sns_pipeline_reads_awss_absence_sentence() {
    let case = "a queue deleted out from under its SNS subscription";
    let router = router();

    // The pipeline under test: a topic with this queue subscribed to it, which delivers before
    // the queue is deleted — so the absence below is the absence of a queue that WAS serving a
    // fan-out, not of one that never worked.
    let name = "pipeline-destination-gone";
    let (topic, url) = given_topic_with_subscriber(&router, case, REGION, name, Grant::Unscoped)
        .await;
    let published = sns(
        &router,
        case,
        "Publish",
        &[("TopicArn", &topic), ("Message", "before the deletion")],
    )
    .await;
    let message_id = element(&published, "MessageId");
    assert!(
        !message_id.is_empty(),
        "case {case:?}: an accepted publish must answer a MessageId, got {published}"
    );

    // The subscription stays exactly as it was: only the queue goes away.
    sqs_ok(&router, case, "DeleteQueue", &json!({"QueueUrl": url})).await;
    let subscriptions = sns(
        &router,
        case,
        "ListSubscriptionsByTopic",
        &[("TopicArn", &topic)],
    )
    .await;
    assert!(
        subscriptions.contains(name),
        "case {case:?}: expected the subscription to go on naming the deleted queue, got \
         {subscriptions}"
    );

    let never = "pipeline-destination-never-created";
    let forms: Vec<(&str, String)> = vec![
        ("the url of the queue deleted from the pipeline", url.clone()),
        (
            "a url of a queue this account never created",
            url_of_name(&url, never),
        ),
    ];

    let mut rows: Vec<(&str, &'static str, Value)> = Vec::new();
    for (form, probed_url) in &forms {
        for (operation, request) in url_requests(probed_url) {
            rows.push((form, operation, request));
        }
    }
    rows.push((
        "the name of the queue deleted from the pipeline",
        "GetQueueUrl",
        json!({"QueueName": name}),
    ));
    rows.push((
        "a name this account never created",
        "GetQueueUrl",
        json!({"QueueName": never}),
    ));

    for (form, operation, request) in &rows {
        for dialect in [Dialect::Native, Dialect::QueryCompatible] {
            let answered = sqs_answer(&router, operation, request, dialect).await;
            assert_absence_refusal(
                form,
                operation,
                dialect,
                &[name, never, &url, &topic],
                answered,
            );
        }
    }
}

/// How many times a leg polls the control pipeline's queue for the notification before calling
/// it undelivered, and the long poll each of those calls makes.
///
/// Driven off what has ARRIVED rather than off a fixed wait, so a loaded machine delays the leg
/// instead of failing it.
const POLLS: usize = 30;
const WAIT_TIME_SECONDS: u32 = 1;

/// The notification bodies waiting on the queue at `url`, polled until one arrives or the polls
/// run out.
async fn bodies_on(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut bodies: Vec<String> = Vec::new();
    for _ in 0..POLLS {
        if !bodies.is_empty() {
            break;
        }
        let received = sqs_ok(
            router,
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
        }
    }
    bodies
}

/// The absence is the absent queue's own: a deleted destination refuses every call with the one
/// sentence while the pipeline beside it goes on delivering, and the publish to the topic whose
/// destination is gone is still accepted.
#[tokio::test]
async fn a_gone_destination_refuses_while_the_pipeline_beside_it_goes_on_delivering() {
    let case = "one gone destination beside a live one";
    let router = router();

    let gone_name = "stale-destination";
    let (stale_topic, stale_url) =
        given_topic_with_subscriber(&router, case, REGION, gone_name, Grant::Unscoped).await;
    let live_name = "live-destination";
    let (live_topic, live_url) =
        given_topic_with_subscriber(&router, case, REGION, live_name, Grant::Unscoped).await;

    sqs_ok(&router, case, "DeleteQueue", &json!({"QueueUrl": stale_url})).await;

    // SNS accepts the publish to the topic whose only destination is gone — delivery is not what
    // `Publish` answers — and it reaches nothing a caller can read, because the queue refuses.
    let published = sns(
        &router,
        case,
        "Publish",
        &[
            ("TopicArn", &stale_topic),
            ("Message", "nothing holds this"),
        ],
    )
    .await;
    assert!(
        !element(&published, "MessageId").is_empty(),
        "case {case:?}: expected the publish to the stale topic to be accepted, got {published}"
    );
    for (operation, request) in url_requests(&stale_url) {
        let answered = sqs_answer(&router, operation, &request, Dialect::Native).await;
        assert_absence_refusal(
            "the destination the pipeline lost",
            operation,
            Dialect::Native,
            &[gone_name, &stale_url, &stale_topic],
            answered,
        );
    }

    // The pipeline beside it is untouched: its own publish is delivered and received.
    let sent = "the live destination holds this";
    sns(
        &router,
        case,
        "Publish",
        &[("TopicArn", &live_topic), ("Message", sent)],
    )
    .await;
    let bodies = bodies_on(&router, case, &live_url).await;
    assert!(
        bodies.iter().any(|body| body.contains(sent)),
        "case {case:?}: expected the live pipeline's queue to hold the notification carrying \
         {sent:?}, got {bodies:?}"
    );
}
