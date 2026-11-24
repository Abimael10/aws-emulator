// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call on the queue an Amazon S3 object notification was fanned out
//! to, once that queue is DELETED — over the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a request naming a queue this account no longer holds is refused HTTP
//! 400 in the `QueueDoesNotExist` class — reported beside the legacy query code the model binds
//! to it, `AWS.SimpleQueueService.NonExistentQueue` — carrying the FIXED sentence the wire
//! protocol the request arrived in words that class with, whatever operation was called, however
//! the queue was addressed (its `QueueUrl`, or the `QueueName` the one name-addressed operation
//! resolves), and whatever the queue had been serving a moment earlier.** On this service's
//! `awsJson1_0` surface that sentence is "The specified queue does not exist."; on its
//! WSDL-versioned AWS Query surface it is the same statement naming that surface's version.
//! Either way the sentence is a CONSTANT: the code is what identifies the condition, so nothing
//! of the request — no queue name, no url, no message body — is interpolated into it, and no
//! clause of the other surface's wording leaks into this one's.
//!
//! Three crates have to agree for the flow this suite refuses a call on to exist at all: S3
//! publishes an object change to the configured topic, SNS fans that publication out to its
//! subscriptions, SQS holds the subscribed queue and then decides every later call on it. That
//! makes this the delete-side negative control of the notification chain — the call a client
//! makes to CONFIRM the queue that captured an object event is gone — so a client matching AWS's
//! own absence sentence can confirm it.
//!
//! Overlap, stated rather than hidden: the Amazon SQS crate's `queue_does_not_exist` suite drives
//! the operation table and the absence forms against that service mounted alone, its
//! `query_compatible_error_envelope` suite pins the AWS Query surface's own sentence, and
//! `s3_object_events_fan_out_through_sns` pins the chain's delivery side. What only this suite
//! reaches is the refusal a client reads AFTER that chain filled the queue, on the production
//! service set, where which mounted model claims each in-process call is decidable at all.
//!
//! The rows differ along what the rule generalises over: the OPERATION called and the rest of an
//! otherwise well-formed request it carries, the FORM of the absence (the queue the chain filled
//! and then deleted, a url of a queue the account never created, the deleted name and a name
//! never created), and the SURFACE the client speaks (the native JSON one, the query-compatible
//! JSON one an SDK opts into with the query-mode header, and the AWS Query form wire).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The whole emulator over one engine and the SigV4 header that routes a call to one service:
// a sibling suite declares both `pub(crate)`, so neither is written twice.
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, form_encode};

/// The Region every request here is signed for, and so the Region the bucket, the topic and the
/// queues belong to.
const REGION: &str = "us-west-2";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a notification under, and the one Amazon SNS
/// delivers to a subscribed queue under — each the identity the destination's own policy has to
/// grant for that leg to be made.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action S3 performs on a topic destination, and the one SNS performs on a subscribed queue.
const PUBLISH_ACTION: &str = "sns:Publish";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// Amazon SQS's own sentence for a queue the account does not hold, on the `awsJson1_0` surface
/// every modern SDK speaks — written out here rather than read off the service, so what this
/// suite asserts is AWS's text and not the emulator's idea of it.
///
/// RECORDED: a `SendMessage` on the url of a queue that was deleted, and a `GetQueueUrl` for a
/// name the account holds no queue under, are refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` with exactly this text. The model
/// (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no message text, so the served sentence is the requirement; it names no queue, so it has
/// no variable part.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The sentence the WSDL-versioned AWS Query surface of the same API words the same class with,
/// which the JSON surface above never serves. One API, two protocols, two sentences for one
/// class, with the same code and status on both.
const QUERY_API_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The class the refusal names, as the `awsJson1_0` discriminator carries it, and the status the
/// model binds to it (`httpError: 400`).
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";
const REFUSAL_STATUS: u16 = 400;

/// The legacy query code the model's `awsQueryError` trait binds to that class — what a query
/// client reads as `Error.Code` — and the `code;fault` report an `awsQueryCompatible` service
/// puts beside the class on the JSON surface.
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";

/// The response header that report arrives in, and the request header a query-compatible SDK opts
/// into it with.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";
const QUERY_MODE_HEADER: &str = "x-amzn-query-mode";

/// The API version an AWS Query client of this endpoint names in every request
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// version `2012-11-05`).
const QUERY_API_VERSION: &str = "2012-11-05";

/// The media types the two Amazon SQS surfaces carry their requests in.
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// The `Id` the bucket's notification configuration carries, the event it names, the `Event` of
/// the notification a validated destination is sent while the configuration is being resolved,
/// and the `eventSource` of every record S3 publishes about an object.
const CONFIGURATION_ID: &str = "object-created";
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const TEST_EVENT: &str = "s3:TestEvent";
const EVENT_SOURCE: &str = "aws:s3";

/// The bytes every write below stores, so a record's `size` has one expected value.
const OBJECT_BODY: &str = "notified-object";

/// How long a wait for an asynchronous delivery lasts before the suite calls it undelivered, and
/// how often it looks again. The wait is for what has ARRIVED and never for a span, so a loaded
/// machine delays a leg instead of failing it, and the budget is far above anything this flow
/// takes because no case here is about how fast a delivery is made.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The surface one Amazon SQS JSON call is made on.
#[derive(Debug, Clone, Copy)]
enum Surface {
    /// The `awsJson1_0` wire a modern SDK speaks.
    Json,
    /// The same wire with the query-compatibility opt-in a migrating SDK sends.
    JsonQueryCompatible,
}

impl Surface {
    fn opt_in(self) -> Option<&'static str> {
        match self {
            Self::Json => None,
            Self::JsonQueryCompatible => Some("true"),
        }
    }
}

/// One Amazon SQS call on the JSON surface, as its status, its response headers and its parsed
/// body — the seam a refusal is read through, so nothing here asserts a status of its own.
async fn sqs_json(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    surface: Surface,
) -> (u16, http::HeaderMap, Value) {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", JSON_CONTENT_TYPE)
        .header("x-amz-target", format!("AmazonSQS.{operation}"));
    if let Some(value) = surface.opt_in() {
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

/// The same call, which must be served, answering the document it reported.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, _, parsed) = sqs_json(router, operation, body, Surface::Json).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One form parameter of an AWS Query request: the name, which is this suite's own literal, and
/// the value it carries.
type Param = (&'static str, String);

/// One row of the AWS Query table: the case, the action it calls, and the parameters it carries.
type QueryRow = (String, &'static str, Vec<Param>);

/// One Amazon SQS call on the AWS Query form wire, as its status and the XML text it answered.
async fn sqs_query(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={QUERY_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon SNS awsQuery call (API version 2010-03-31) that must answer 200, as its XML body.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = "Action=".to_string();
    body.push_str(action);
    body.push_str("&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sns", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {text}"
    );
    text
}

/// One response of S3's REST surface: its status and the body as text.
struct Answer {
    status: u16,
    body: String,
}

/// Dispatch one request against S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3", REGION))
        .header("content-type", "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// A string member of a document, or `""` where it carries none — so a missing member fails an
/// assertion rather than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The class a JSON refusal names: the local name of its `__type` discriminator.
fn class_of(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The sentence a JSON refusal carries, under either spelling of the member.
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

/// The ARN of an S3 bucket: `arn:aws:s3:::<bucket>`, with no Region or account, because a bucket
/// name is globally unique. It is the value `aws:SourceArn` carries for a notification this
/// bucket published.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// A one-statement policy document granting `principal` `action` on `resource`, scoped by
/// `condition` when the caller states one.
fn grant(principal: &str, action: &str, resource: &str, condition: Option<Value>) -> String {
    let mut statement = json!({
        "Sid": "AllowTheChain",
        "Effect": "Allow",
        "Principal": { "Service": principal },
        "Action": action,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// The url the same endpoint and account address the queue called `name` at, built off a url this
/// emulator itself minted so no case writes the address form by hand.
fn url_of_name(minted: &str, name: &str) -> String {
    match minted.rfind('/') {
        Some(cut) => format!("{}/{name}", &minted[..cut]),
        None => format!("{minted}/{name}"),
    }
}

/// A topic of this account in this Region, answering its ARN.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    element(
        &sns(router, case, "CreateTopic", &[("Name", name)]).await,
        "TopicArn",
    )
}

/// A queue named `name`, granting the Amazon SNS service principal the send ("you must ensure
/// that the Amazon SQS queue policy allows the Amazon SNS topic to send messages to the queue")
/// and subscribed to `topic_arn`. Answers the url the owning service assigned it.
async fn given_subscribed_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic_arn: &str,
) -> String {
    let created = sqs_ok(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let queue_url = text(&created, "QueueUrl");
    assert!(
        queue_url.ends_with(&format!("/{name}")),
        "case {case:?}: expected the created queue's url to address it, got {queue_url:?}"
    );
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = text(&reported["Attributes"], "QueueArn");
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": queue_url,
            "Attributes": {
                "Policy": grant(SNS_PRINCIPAL, SEND_MESSAGE_ACTION, &queue_arn, None),
            },
        }),
    )
    .await;
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    queue_url
}

/// A bucket in this Region whose notification configuration publishes every object creation to
/// `topic_arn`, with that topic's own document granting Amazon S3 the publish it reads before it
/// records anything.
async fn given_notifying_bucket(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    topic_arn: &str,
) {
    let created = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        &format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    sns(
        router,
        case,
        "SetTopicAttributes",
        &[
            ("TopicArn", topic_arn),
            ("AttributeName", "Policy"),
            (
                "AttributeValue",
                &grant(
                    S3_PRINCIPAL,
                    PUBLISH_ACTION,
                    topic_arn,
                    Some(json!({ "ArnLike": { "aws:SourceArn": bucket_arn(bucket) } })),
                ),
            ),
        ],
    )
    .await;
    // `TopicArn` is bound as `Topic` on the query wire S3's REST payload uses
    // (`smithy_query(query="shape", shape="TopicConfiguration",
    // model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`).
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><TopicConfiguration>\
         <Id>{CONFIGURATION_ID}</Id><Topic>{topic_arn}</Topic><Event>{OBJECT_CREATED}</Event>\
         </TopicConfiguration></NotificationConfiguration>"
    );
    let configured = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        configured.status, 200,
        "case {case:?}: configuring the granted topic must succeed, got {} with {}",
        configured.status, configured.body
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`, the object change the configuration above names.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}/{key}"), OBJECT_BODY).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// Everything waiting on the queue at `url` right now, each body parsed as the JSON it carries
/// and DELETED as it is read — so nothing one leg received comes back into view for the next.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs_ok(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::with_capacity(messages.len());
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs_ok(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
        let body = message["Body"].as_str().unwrap_or_default();
        bodies.push(serde_json::from_str(body).unwrap_or(Value::Null));
    }
    bodies
}

/// The first notification the queue receives, looking again until one arrives or
/// [`DELIVERY_BUDGET`] passes.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let mut arrived = drain(router, case, url).await;
        if !arrived.is_empty() {
            return arrived.remove(0);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: nothing reached the queue at {url:?} within {DELIVERY_BUDGET:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The document one SNS envelope on the queue carries, parsed as the one S3 published.
fn published_document(case: &str, envelope: &Value) -> Value {
    assert_eq!(
        envelope["Type"].as_str(),
        Some("Notification"),
        "case {case:?}: what arrived must be an Amazon SNS notification envelope, got {envelope}"
    );
    let message = envelope["Message"].as_str().unwrap_or_default();
    serde_json::from_str(message).unwrap_or_else(|error| {
        panic!("case {case:?}: the envelope's message must be the document S3 published ({error}): {message}")
    })
}

/// Everything a case's wiring names: the bucket that publishes, the topic it publishes to, and
/// the queue subscribed to that topic.
struct Chain {
    bucket: String,
    topic_arn: String,
    queue_url: String,
}

/// The whole chain for one case, with the configuration's own test notification already taken off
/// the queue so a later leg reads only what an object change delivered.
async fn given_chain(router: &MultiServiceRouter, case: &str, name: &str) -> Chain {
    let topic_arn = given_topic(router, case, name).await;
    let queue_url = given_subscribed_queue(router, case, name, &topic_arn).await;
    given_notifying_bucket(router, case, name, &topic_arn).await;
    let envelope = delivered(router, case, &queue_url).await;
    assert_eq!(
        published_document(case, &envelope)["Event"].as_str(),
        Some(TEST_EVENT),
        "case {case:?}: the first notification on the queue must be the test event, got {envelope}"
    );
    Chain {
        bucket: name.to_string(),
        topic_arn,
        queue_url,
    }
}

/// Write one object on the chain's bucket and require its record to reach the queue at `url`,
/// answering nothing — the positive control every refusal below is measured against.
async fn chain_delivers(router: &MultiServiceRouter, case: &str, chain: &Chain, key: &str, url: &str) {
    put_object(router, case, &chain.bucket, key).await;
    let envelope = delivered(router, case, url).await;
    assert_eq!(
        envelope["TopicArn"].as_str(),
        Some(chain.topic_arn.as_str()),
        "case {case:?}: the notification must have come through the configured topic, got \
         {envelope}"
    );
    let published = published_document(case, &envelope);
    let record = &published["Records"][0];
    assert_eq!(
        (
            text(record, "eventSource"),
            text(record, "awsRegion"),
            text(&record["s3"], "configurationId"),
            text(&record["s3"]["bucket"], "name"),
            text(&record["s3"]["object"], "key"),
            record["s3"]["object"]["size"].as_i64(),
        ),
        (
            EVENT_SOURCE.to_string(),
            REGION.to_string(),
            CONFIGURATION_ID.to_string(),
            chain.bucket.clone(),
            key.to_string(),
            Some(OBJECT_BODY.len() as i64),
        ),
        "case {case:?}: expected the record of the object change on {key:?}, got {published}"
    );
    assert!(
        text(record, "eventName").starts_with("ObjectCreated:"),
        "case {case:?}: expected an object-creation record, got {published}"
    );
}

/// Every operation of this service that resolves a queue from a `QueueUrl`, each with the rest of
/// a request that is otherwise well formed — so the only thing its refusal can be about is the
/// queue.
///
/// Taken from the service's own surface (`smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`):
/// the url-addressed operations that need no other resource of their own. `SendMessage` leads,
/// twice, because a send is the call a client makes to the queue a notification chain filled and
/// the rest of its request is one more thing the refusal must not be about.
fn url_requests(url: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("SendMessage", json!({"QueueUrl": url, "MessageBody": "gone"})),
        (
            "SendMessage",
            json!({
                "QueueUrl": url,
                "MessageBody": OBJECT_BODY,
                "DelaySeconds": 10,
                "MessageAttributes": {
                    "source": {"DataType": "String", "StringValue": EVENT_SOURCE},
                },
            }),
        ),
        (
            "SendMessageBatch",
            json!({"QueueUrl": url, "Entries": [{"Id": "one", "MessageBody": "gone"}]}),
        ),
        ("ReceiveMessage", json!({"QueueUrl": url})),
        (
            "GetQueueAttributes",
            json!({"QueueUrl": url, "AttributeNames": ["All"]}),
        ),
        (
            "SetQueueAttributes",
            json!({"QueueUrl": url, "Attributes": {"VisibilityTimeout": "75"}}),
        ),
        (
            "DeleteMessage",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle"}),
        ),
        (
            "ChangeMessageVisibility",
            json!({"QueueUrl": url, "ReceiptHandle": "not-a-handle", "VisibilityTimeout": 30}),
        ),
        ("PurgeQueue", json!({"QueueUrl": url})),
        ("ListQueueTags", json!({"QueueUrl": url})),
        ("DeleteQueue", json!({"QueueUrl": url})),
    ]
}

/// Assert one JSON answer is the absence refusal, whole: the status, the class, the legacy report
/// beside it, AWS's own sentence, and that the sentence interpolated nothing of the request.
fn assert_json_absence(
    case: &str,
    operation: &str,
    surface: Surface,
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
            report.clone(),
        ),
        (
            REFUSAL_STATUS,
            QUEUE_DOES_NOT_EXIST,
            AWS_SENTENCE,
            Some(NON_EXISTENT_QUEUE_REPORT.to_string()),
        ),
        "case {case:?} / {operation} on the {surface:?} surface: expected HTTP {REFUSAL_STATUS} \
         {QUEUE_DOES_NOT_EXIST} reading {AWS_SENTENCE:?} reported as \
         {NON_EXISTENT_QUEUE_REPORT:?}, got {status} with {body} and {QUERY_ERROR_HEADER} \
         {report:?}"
    );
    for value in probed {
        assert!(
            !message.contains(value),
            "case {case:?} / {operation} on the {surface:?} surface: the sentence must \
             interpolate nothing of the request, got {message:?} naming {value:?}"
        );
    }
}

/// The queue an object notification was fanned out to, once deleted, refuses every Amazon SQS
/// call naming it — on both JSON surfaces, over both forms of the absence and both addressing
/// forms — with AWS's own absence sentence.
#[tokio::test]
async fn a_deleted_notified_queue_refuses_every_sqs_call_with_awss_absence_sentence() {
    let router = router();
    let name = "s3-notified-queue-absence";
    let never = "s3-notified-queue-never-created";
    let case = "the queue an S3 object notification reached, then deleted";

    let chain = given_chain(&router, case, name).await;
    // The chain really was serving a moment earlier: this is what the refusals below are the
    // delete side of.
    chain_delivers(&router, case, &chain, "before-the-delete.json", &chain.queue_url).await;

    sqs_ok(
        &router,
        case,
        "DeleteQueue",
        &json!({ "QueueUrl": chain.queue_url }),
    )
    .await;

    let absences: Vec<(String, String)> = vec![
        (
            format!("{case}, by the url the chain delivered to"),
            chain.queue_url.clone(),
        ),
        (
            format!("{case}, by a url of a queue the account never created"),
            url_of_name(&chain.queue_url, never),
        ),
    ];
    for (form, probed_url) in &absences {
        for (operation, request) in url_requests(probed_url) {
            for surface in [Surface::Json, Surface::JsonQueryCompatible] {
                let answered = sqs_json(&router, operation, &request, surface).await;
                assert_json_absence(
                    form,
                    operation,
                    surface,
                    &[name, never, probed_url, OBJECT_BODY],
                    answered,
                );
            }
        }
    }

    // The name-addressed operation resolves it no longer either.
    for (form, probed_name) in [
        ("the name of the queue the chain delivered to", name),
        ("a name the account never created", never),
    ] {
        for surface in [Surface::Json, Surface::JsonQueryCompatible] {
            let answered = sqs_json(
                &router,
                "GetQueueUrl",
                &json!({ "QueueName": probed_name }),
                surface,
            )
            .await;
            assert_json_absence(
                form,
                "GetQueueUrl",
                surface,
                &[probed_name, &chain.queue_url],
                answered,
            );
        }
    }
}

/// The AWS Query surface of the same API answers the same class, with the same code and status,
/// in the sentence that surface words it with — so which sentence a client reads is decided by
/// the protocol its request arrived in and by nothing else.
#[tokio::test]
async fn the_query_surface_words_the_notified_queues_absence_in_its_own_sentence() {
    let router = router();
    let name = "s3-notified-queue-query-surface";
    let never = "s3-notified-queue-query-never-created";
    let case = "the queue an S3 object notification reached, reached on the AWS Query surface";

    let chain = given_chain(&router, case, name).await;
    chain_delivers(&router, case, &chain, "before-the-delete.json", &chain.queue_url).await;
    sqs_ok(
        &router,
        case,
        "DeleteQueue",
        &json!({ "QueueUrl": chain.queue_url }),
    )
    .await;

    // The actions are spelled in this protocol's own request form: a `QueueUrl` field, the
    // `Attribute.N.Name` / `Attribute.N.Value` pairs an attribute map arrives as, and the
    // `QueueName` the name-addressed action takes.
    let url = chain.queue_url.clone();
    let absent = url_of_name(&url, never);
    let rows: Vec<QueryRow> = vec![
        (
            format!("SendMessage to the deleted queue of {case}"),
            "SendMessage",
            vec![("QueueUrl", url.clone()), ("MessageBody", "gone".to_string())],
        ),
        (
            "SendMessage to a queue the account never created".to_string(),
            "SendMessage",
            vec![
                ("QueueUrl", absent.clone()),
                ("MessageBody", "gone".to_string()),
            ],
        ),
        (
            "SendMessageBatch to the deleted queue".to_string(),
            "SendMessageBatch",
            vec![
                ("QueueUrl", url.clone()),
                ("SendMessageBatchRequestEntry.1.Id", "one".to_string()),
                (
                    "SendMessageBatchRequestEntry.1.MessageBody",
                    "gone".to_string(),
                ),
            ],
        ),
        (
            "ReceiveMessage on the deleted queue".to_string(),
            "ReceiveMessage",
            vec![("QueueUrl", url.clone())],
        ),
        (
            "GetQueueAttributes on the deleted queue".to_string(),
            "GetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("AttributeName.1", "All".to_string()),
            ],
        ),
        (
            "SetQueueAttributes on the deleted queue".to_string(),
            "SetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("Attribute.1.Name", "VisibilityTimeout".to_string()),
                ("Attribute.1.Value", "75".to_string()),
            ],
        ),
        (
            "PurgeQueue on the deleted queue".to_string(),
            "PurgeQueue",
            vec![("QueueUrl", url.clone())],
        ),
        (
            "GetQueueUrl for the deleted name".to_string(),
            "GetQueueUrl",
            vec![("QueueName", name.to_string())],
        ),
        (
            "GetQueueUrl for a name the account never created".to_string(),
            "GetQueueUrl",
            vec![("QueueName", never.to_string())],
        ),
    ];

    for (form, action, params) in &rows {
        let (status, answered) = sqs_query(&router, action, params).await;
        let code = element(&answered, "Code");
        let message = element(&answered, "Message");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (REFUSAL_STATUS, NON_EXISTENT_QUEUE, QUERY_API_SENTENCE),
            "case {form:?}: expected HTTP {REFUSAL_STATUS} {NON_EXISTENT_QUEUE} reading \
             {QUERY_API_SENTENCE:?}, got {status} with {answered}"
        );
        for value in [name, never, url.as_str()] {
            assert!(
                !message.contains(value),
                "case {form:?}: the sentence must interpolate nothing of the request, got \
                 {message:?} naming {value:?}"
            );
        }
    }
}

/// The delete refuses the queue it NAMED and nothing else: a second queue subscribed to the same
/// topic goes on receiving the chain's notifications and goes on accepting a direct send, so the
/// absence sentence above is about one queue rather than about the chain.
#[tokio::test]
async fn the_delete_refuses_only_the_queue_it_named() {
    let router = router();
    let name = "s3-notified-queue-pair";
    let sibling = "s3-notified-queue-pair-sibling";
    let case = "two queues subscribed to the topic the bucket notifies, one of them deleted";

    let chain = given_chain(&router, case, name).await;
    let sibling_url = given_subscribed_queue(&router, case, sibling, &chain.topic_arn).await;

    // Both subscribers receive the chain's first object change.
    put_object(&router, case, &chain.bucket, "both.json").await;
    for url in [&chain.queue_url, &sibling_url] {
        let envelope = delivered(&router, case, url).await;
        let published = published_document(case, &envelope);
        assert_eq!(
            text(&published["Records"][0]["s3"]["object"], "key"),
            "both.json",
            "case {case:?}: the queue at {url:?} must receive the object change, got {published}"
        );
    }

    sqs_ok(
        &router,
        case,
        "DeleteQueue",
        &json!({ "QueueUrl": chain.queue_url }),
    )
    .await;

    // The next object change still reaches the queue the delete did not name...
    chain_delivers(&router, case, &chain, "after-the-delete.json", &sibling_url).await;
    // ...which also still takes a direct send, so what the refusal below reports is the ONE
    // queue's absence.
    let sent = sqs_ok(
        &router,
        case,
        "SendMessage",
        &json!({ "QueueUrl": sibling_url, "MessageBody": "still-here" }),
    )
    .await;
    assert!(
        !text(&sent, "MessageId").is_empty(),
        "case {case:?}: the surviving queue must accept a send, got {sent}"
    );

    // ...while the deleted one is refused on both surfaces.
    for surface in [Surface::Json, Surface::JsonQueryCompatible] {
        let answered = sqs_json(
            &router,
            "SendMessage",
            &json!({ "QueueUrl": chain.queue_url, "MessageBody": "gone" }),
            surface,
        )
        .await;
        assert_json_absence(
            case,
            "SendMessage",
            surface,
            &[name, sibling, chain.queue_url.as_str()],
            answered,
        );
    }
    let (status, answered) = sqs_query(
        &router,
        "SendMessage",
        &[
            ("QueueUrl", chain.queue_url.clone()),
            ("MessageBody", "gone".to_string()),
        ],
    )
    .await;
    assert_eq!(
        (
            status,
            element(&answered, "Code").as_str(),
            element(&answered, "Message").as_str(),
        ),
        (REFUSAL_STATUS, NON_EXISTENT_QUEUE, QUERY_API_SENTENCE),
        "case {case:?}: the AWS Query surface must refuse the deleted queue in its own \
         sentence, got {status} with {answered}"
    );
}
