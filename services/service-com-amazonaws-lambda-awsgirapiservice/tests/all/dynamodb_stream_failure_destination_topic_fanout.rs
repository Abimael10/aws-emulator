// SPDX-License-Identifier: CC0-1.0

//! What a mapping on a DynamoDB table's STREAM does with a batch it gives up on when its
//! failure destination is an Amazon SNS TOPIC — Lambda, Amazon DynamoDB, DynamoDB Streams,
//! Amazon SNS and Amazon SQS on ONE engine, so the record is published where a client
//! publishes one and read off each subscriber where a client reads it.
//!
//! RULE: `MaximumRetryAttempts` is "Discard records after the specified number of retries",
//! so the count a mapping states bounds the attempts ONE batch gets and once they are spent
//! the batch is discarded; the only account of it is the one record written to the resource
//! the mapping's `DestinationConfig$OnFailure` names ("the destination configuration for
//! failed invocations", whose `Destination` is "the Amazon Resource Name (ARN) of the
//! destination resource"), describing the BATCH — the function, what ended it, how many
//! attempts it had and what the last answered, beside the stream it was read from, the
//! shard, the sequence-number window it covers and how many records it held, and never the
//! record payloads. The write is the destination service's OWN write, so a destination that
//! is a topic is written with `sns:Publish` and the topic therefore relays that one
//! publication to EVERY subscription of it, each in the delivery form that subscription asked
//! for.
//!
//! The rows differ along what the rule generalises over: how many retries the mapping allows
//! (none, one), how many records the batch holds (one, several) and where in it the record
//! the handler refuses sits, how many subscriptions the topic fans the record out to (one,
//! two) and which delivery form each subscription takes (the raw message, or the JSON
//! notification SNS wraps it in). None of those is what the rule turns on.
//!
//! Every verdict is a consequence of the handler having been invoked with the batch: the
//! function's package RUNS, and whether it refuses the batch is decided inside the handler
//! from each record's own keys.
//!
//! The sibling suite `event_source_mapping_failure_destination.rs` pins the record itself over
//! both shard sources and both kinds of destination; this one pins the relay that carries it
//! from a topic destination to the subscribers of that topic.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_failure_destination::{given_queue, sqs};
use crate::synchronous_invoke_run::package;

/// The values the record of a discarded batch carries on the WIRE, written out here rather
/// than imported from the emulator so a change to either side of them fails a row.
///
/// Source: AWS's published on-failure record for a stream event source — the schema version
/// the record carries, the condition it reports for a batch whose retries ran out, and the
/// member a DynamoDB table stream's batch is described under.
const RECORD_SCHEMA_VERSION: &str = "1.0";
/// See [`RECORD_SCHEMA_VERSION`].
const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";
/// See [`RECORD_SCHEMA_VERSION`].
const DYNAMODB_STREAM_BATCH_INFO: &str = "DDBStreamBatchInfo";
/// The member a KINESIS batch would be described under, which a table stream's record carries
/// nothing in: each shard source describes its batch under a member of its own.
const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";

/// What the JSON notification SNS wraps a published message in reports itself as, and the
/// members it carries the topic and the message under
/// (`GetSubscriptionAttributesResponse$Attributes`, `RawMessageDelivery`: a subscriber that has
/// not asked for raw delivery receives this envelope).
const NOTIFICATION_TYPE: &str = "Notification";
/// See [`NOTIFICATION_TYPE`].
const NOTIFICATION_TOPIC_MEMBER: &str = "TopicArn";
/// See [`NOTIFICATION_TYPE`].
const NOTIFICATION_MESSAGE_MEMBER: &str = "Message";

/// The Region every call below is made in, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about it and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `X-Amz-Target` prefix of the DynamoDB calls below (that service's own `OPERATIONS.md`,
/// "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The API version every Amazon SNS call below states, which that service's `awsQuery`
/// protocol carries as a parameter of the body.
const SNS_API_VERSION: &str = "2010-03-31";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `StartingPosition` a table stream admits that reads the records already sitting in it:
/// "Required for Amazon Kinesis and Amazon DynamoDB Stream event sources"
/// (`CreateEventSourceMappingInput$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// The stream view type every table below captures its changes under.
const NEW_AND_OLD_IMAGES: &str = "NEW_AND_OLD_IMAGES";

/// How many records one batch may hold, which is more than any row puts on its stream, so a
/// row's records are read as ONE batch.
const BATCH_SIZE: i64 = 10;

/// The partition key every table below is keyed on.
const KEY: &str = "pk";

/// The marker a record carries to be one the handler refuses. Nothing in the emulator reads
/// it: it is read by the handler, out of the record's own keys.
const POISON: &str = "poison";

/// The `FunctionError` an invocation whose handler raised reports
/// (`InvocationResponse$FunctionError`), which is what the record of a discarded batch
/// carries for its last attempt.
const UNHANDLED: &str = "Unhandled";

/// The status an invocation that executed answers (`InvocationResponse$StatusCode`: "The HTTP
/// status code is in the 200 range for a successful request").
const INVOKED_STATUS: i64 = 200;

/// The service principal Amazon SNS delivers to a subscribed queue as, which that queue's own
/// policy has to grant `sqs:SendMessage` to.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long a row waits for work the CLOCK owes — the table settling, the function settling,
/// the batch being read, the attempts being spent and the record reaching each subscriber.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Which form a subscription of the row's topic takes its deliveries in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// The published message itself — "this eliminates the need for the endpoints to process
    /// JSON formatting" (`RawMessageDelivery`).
    Raw,
    /// The JSON notification SNS wraps a published message in, which carries the message as
    /// its own member.
    Envelope,
}

/// One row: what the stream holds, how many retries the mapping allows and which
/// subscriptions the topic it records a discarded batch at fans that record out to.
struct Row {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    /// The `MaximumRetryAttempts` the mapping states.
    retries: i64,
    /// The key of each item written to the table before the mapping is created, in order.
    records: &'static [&'static str],
    /// One queue subscribed to the row's topic per entry, in that entry's delivery form.
    subscribers: &'static [Delivery],
}

impl Row {
    /// How many attempts the batch gets: the first one plus the retries the mapping states.
    fn attempts(&self) -> i64 {
        self.retries + 1
    }
}

/// The rows: both retry counts, a batch of one record and a batch of several, the refused
/// record first and in the middle, one subscription and two, raw delivery and the envelope.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a batch of one refused record, no retry, relayed to one raw subscriber",
            name: "one-raw",
            retries: 0,
            records: &["poison-only"],
            subscribers: &[Delivery::Raw],
        },
        Row {
            case: "a batch of three records whose middle one is refused, one retry, relayed \
                   to a raw subscriber and to one taking the notification envelope",
            name: "two-subscribers",
            retries: 1,
            records: &["ok-first", "poison-middle", "ok-last"],
            subscribers: &[Delivery::Raw, Delivery::Envelope],
        },
    ]
}

/// The handler every function below runs: it reads each record of the batch it was handed out
/// of that record's OWN keys and RAISES for a record carrying [`POISON`], so a batch it
/// refuses is a batch it was really handed.
fn handler_source() -> String {
    format!(
        r#"
import json

MARKER = "{POISON}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    read = []
    for record in records:
        source = record["eventSource"]
        if source != "aws:dynamodb":
            raise ValueError("eventSource is " + repr(source))
        text = json.dumps(record["dynamodb"]["Keys"])
        read.append(text)
        if MARKER in text:
            raise RuntimeError("the handler cannot process " + text)
    return {{"processed": read}}
"#
    )
}

/// Lambda, Amazon DynamoDB, DynamoDB Streams, Amazon SQS and Amazon SNS on ONE engine: the
/// mapping's event source is a table's stream of this emulator, the destination it records a
/// discarded batch at is a topic of this emulator and the subscribers of that topic are its
/// queues — the shape `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`]. Nothing in it is verified:
/// the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda call, over its own restJson1 routes.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let parsed = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One DynamoDB call, in the AWS JSON form that service's protocol binds.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{DYNAMODB_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("dynamodb"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon SNS call, in the `awsQuery` form that service's protocol binds.
///
/// This suite drives `Subscribe` with a delivery form of each row's own choosing, which is
/// what the sibling suite's one fixture does not take, so the two calls it needs travel over
/// this wire.
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", SNS_API_VERSION);
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
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// The text one XML element of an Amazon SNS answer carries.
fn element(document: &str, name: &str) -> Option<String> {
    let opening = format!("<{name}>");
    let closing = format!("</{name}>");
    let from = document.find(&opening)? + opening.len();
    let to = document[from..].find(&closing)? + from;
    Some(document[from..to].to_string())
}

/// A topic of this account, as Amazon SNS reports its ARN.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = sns(router, "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic {name:?} answered {status} {created}"
    );
    element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TopicArn in {created}"))
}

/// Subscribe the queue `queue_arn` names to `topic_arn` in the form `delivery` takes, with
/// that queue's own `Policy` written to admit the topic's deliveries.
async fn subscribed(
    router: &MultiServiceRouter,
    case: &str,
    topic_arn: &str,
    queue_url: &str,
    queue_arn: &str,
    delivery: Delivery,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowDeliveryFromTheTopic",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic_arn } },
        }],
    })
    .to_string();
    let (status, written) = sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {written}"
    );
    let mut parameters = vec![
        ("TopicArn", topic_arn),
        ("Protocol", "sqs"),
        ("Endpoint", queue_arn),
        ("ReturnSubscriptionArn", "true"),
    ];
    if delivery == Delivery::Raw {
        parameters.push(("Attributes.entry.1.key", "RawMessageDelivery"));
        parameters.push(("Attributes.entry.1.value", "true"));
    }
    let (status, answered) = sns(router, "Subscribe", &parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Subscribe {delivery:?} answered {status} {answered}"
    );
}

/// A table keyed on [`KEY`] whose stream captures every change, waited out to `ACTIVE`;
/// answers the table's `LatestStreamArn`.
async fn given_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {
                "StreamEnabled": true,
                "StreamViewType": NEW_AND_OLD_IMAGES,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTable answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = dynamodb(router, "DescribeTable", &json!({ "TableName": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable answered {status} {read}"
        );
        let description = &read["Table"];
        if description["TableStatus"] == json!("ACTIVE") {
            return description["LatestStreamArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no LatestStreamArn in {read}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Write the item keyed `key` into the table, the way a client writes one, which is what puts
/// a record on its stream.
async fn put_item(router: &MultiServiceRouter, case: &str, table: &str, key: &str) {
    let (status, written) = dynamodb(
        router,
        "PutItem",
        &json!({ "TableName": table, "Item": { KEY: { "S": key } } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutItem answered {status} {written}"
    );
}

/// A function whose package RUNS [`handler_source`], waited out to `Active`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&handler_source())),
            },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration answered {status} {read}"
        );
        if read["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function never became Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping between `function` and `stream_arn`, allowing `retries` retries per batch and
/// recording a batch it gives up on at `destination`; answers the `FunctionArn` it reports.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    retries: i64,
    destination: &str,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": BATCH_SIZE,
            "Enabled": true,
            "MaximumRetryAttempts": retries,
            "DestinationConfig": { "OnFailure": { "Destination": destination } },
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["DestinationConfig"]["OnFailure"]["Destination"],
        json!(destination),
        "case {case:?}: the mapping must report the destination it was created with, got \
         {created}"
    );
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string()
}

/// Every message visible on the queue `url` addresses right now, removed from it so a later
/// look sees only what arrived after this one.
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
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        let handle = message["ReceiptHandle"].as_str().unwrap_or_default();
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
    messages
}

/// Wait until a message has arrived on the queue `url` addresses, answering everything that
/// was on it — more than one when more than one arrived, which is what makes "exactly one"
/// assertable.
async fn awaited_messages(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let messages = drained(router, case, url).await;
        if !messages.is_empty() {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the record of the discarded batch relayed to {url} within \
             {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The record one subscriber received, read out of the body the way that subscription's own
/// delivery form carries it: a raw delivery holds the published message itself, and every
/// other one holds it inside the notification envelope.
fn relayed_record(case: &str, delivery: Delivery, topic_arn: &str, body: &str) -> Value {
    let carried = match delivery {
        Delivery::Raw => body.to_string(),
        Delivery::Envelope => {
            let envelope: Value = serde_json::from_str(body).unwrap_or_else(|_| {
                panic!("case {case:?}: the notification must be a JSON document: {body}")
            });
            assert_eq!(
                envelope["Type"],
                json!(NOTIFICATION_TYPE),
                "case {case:?}: the notification must report itself as one, got {envelope}"
            );
            assert_eq!(
                envelope[NOTIFICATION_TOPIC_MEMBER],
                json!(topic_arn),
                "case {case:?}: the notification must name the topic it came from, got \
                 {envelope}"
            );
            envelope[NOTIFICATION_MESSAGE_MEMBER]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {NOTIFICATION_MESSAGE_MEMBER} in {envelope}")
                })
                .to_string()
        }
    };
    serde_json::from_str(&carried).unwrap_or_else(|_| {
        panic!("case {case:?}: the record must be a JSON document: {carried}")
    })
}

/// RULE: once the attempts a mapping's `MaximumRetryAttempts` allows one batch of a table
/// stream are spent, the batch is discarded and exactly one record describing that batch is
/// written to the destination its `DestinationConfig$OnFailure` names; a destination that is
/// an Amazon SNS topic is written with that service's own publish, so the topic relays that
/// one record to every subscription of it in the delivery form each asked for.
#[tokio::test]
async fn a_discarded_batch_is_published_to_the_topic_and_relayed_to_every_subscriber() {
    for row in rows() {
        let case = row.case;
        let router = emulator();

        let table = format!("source-{}", row.name);
        let stream_arn = given_table(&router, case, &table).await;
        // The items go in BEFORE the mapping exists, so the mapping's first read from
        // `TRIM_HORIZON` takes all of their records as ONE batch.
        for key in row.records {
            put_item(&router, case, &table, key).await;
        }

        let topic_arn = given_topic(&router, case, &format!("topic-{}", row.name)).await;
        let mut subscribers = Vec::new();
        for (index, delivery) in row.subscribers.iter().enumerate() {
            let (queue_url, queue_arn) =
                given_queue(&router, case, &format!("sink-{}-{index}", row.name)).await;
            subscribed(
                &router,
                case,
                &topic_arn,
                &queue_url,
                &queue_arn,
                *delivery,
            )
            .await;
            subscribers.push((queue_url, *delivery));
        }

        let function = format!("function-{}", row.name);
        given_function(&router, case, &function).await;
        let function_arn = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.retries,
            &topic_arn,
        )
        .await;

        let mut relayed = Vec::new();
        for (queue_url, delivery) in &subscribers {
            let messages = awaited_messages(&router, case, queue_url).await;
            assert_eq!(
                messages.len(),
                1,
                "case {case:?}: every subscriber receives exactly one record per discarded \
                 batch, got {messages:?} on {queue_url}"
            );
            let body = messages[0]["Body"].as_str().unwrap_or_default().to_string();
            let record = relayed_record(case, *delivery, &topic_arn, &body);

            assert_eq!(
                record["version"],
                json!(RECORD_SCHEMA_VERSION),
                "case {case:?}: the record must carry its schema version, got {record}"
            );
            assert!(
                record["timestamp"].as_str().is_some_and(|at| at.contains('T')
                    && at.len() >= "2026-01-01T00:00:00.000".len()),
                "case {case:?}: the record must be stamped with the instant it was made, got \
                 {record}"
            );
            assert_eq!(
                record["requestContext"]["functionArn"],
                json!(function_arn),
                "case {case:?}: the record must name the function the batch was handed to, got \
                 {record}"
            );
            assert_eq!(
                record["requestContext"]["condition"],
                json!(CONDITION_RETRY_ATTEMPTS_EXHAUSTED),
                "case {case:?}: the record must say the retries are what ran out, got {record}"
            );
            assert_eq!(
                record["requestContext"]["approximateInvokeCount"],
                json!(row.attempts()),
                "case {case:?}: a mapping allowing {} retries gives the batch {} attempts, got \
                 {record}",
                row.retries,
                row.attempts()
            );
            assert!(
                record["requestContext"]["requestId"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()),
                "case {case:?}: the record must name the invocation it reports, got {record}"
            );
            assert_eq!(
                record["responseContext"]["statusCode"],
                json!(INVOKED_STATUS),
                "case {case:?}: the record must report what the last attempt answered, got \
                 {record}"
            );
            assert_eq!(
                record["responseContext"]["functionError"],
                json!(UNHANDLED),
                "case {case:?}: the record must report the error the last attempt reported, got \
                 {record}"
            );

            let info = &record[DYNAMODB_STREAM_BATCH_INFO];
            assert!(
                record[KINESIS_BATCH_INFO].is_null(),
                "case {case:?}: a table stream's batch is described under \
                 {DYNAMODB_STREAM_BATCH_INFO} alone, got {record}"
            );
            assert_eq!(
                info["streamArn"],
                json!(stream_arn),
                "case {case:?}: the batch must name the stream it was read from, got {record}"
            );
            assert!(
                info["shardId"]
                    .as_str()
                    .is_some_and(|shard| shard.starts_with("shardId-")),
                "case {case:?}: the batch must name the shard it was read from, got {record}"
            );
            assert_eq!(
                info["batchSize"],
                json!(row.records.len()),
                "case {case:?}: the batch must report how many records it held, got {record}"
            );
            let start = info["startSequenceNumber"].as_str().unwrap_or_default();
            let end = info["endSequenceNumber"].as_str().unwrap_or_default();
            assert!(
                !start.is_empty() && !end.is_empty(),
                "case {case:?}: the batch must report the window it covers, got {record}"
            );
            if row.records.len() == 1 {
                assert_eq!(
                    start, end,
                    "case {case:?}: a batch of one record starts and ends at that record, got \
                     {record}"
                );
            } else {
                assert_ne!(
                    start, end,
                    "case {case:?}: a batch of several records covers a window, got {record}"
                );
            }
            // What is recorded is the BATCH, not its payloads: the keys the records carry are
            // nowhere in it.
            let text = record.to_string();
            for key in row.records {
                assert!(
                    !text.contains(key),
                    "case {case:?}: the record describes the batch and carries no payload of \
                     it, but {key:?} is in {text}"
                );
            }
            relayed.push(record);
        }

        // ONE publication relayed to each subscription: every subscriber read the same record.
        if let Some((first, rest)) = relayed.split_first() {
            for other in rest {
                assert_eq!(
                    first, other,
                    "case {case:?}: every subscription of the topic receives the same published \
                     record, got {first} and {other}"
                );
            }
        }
    }
}
