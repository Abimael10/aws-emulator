// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping on a SHARD source does with a batch it gives up on, end to
//! end over the wire, with Amazon Kinesis, Amazon DynamoDB, DynamoDB Streams, Amazon SQS and
//! Amazon SNS mounted beside Lambda on ONE engine — so the records are put where a client
//! puts them and what the mapping recorded is read off the destination where a client reads
//! it.
//!
//! The RULE asserted over the tables below: `MaximumRetryAttempts` is "Discard records after
//! the specified number of retries. The default value is infinite (-1). When set to infinite
//! (-1), failed records are retried until the record expires", so the count a mapping states
//! bounds the attempts ONE batch gets — the first attempt plus that many retries — and once
//! they are spent the batch is DISCARDED. The only thing a client can still learn about the
//! discarded records is what the mapping's `DestinationConfig$OnFailure` names ("the
//! destination configuration for failed invocations", whose `Destination` is "the Amazon
//! Resource Name (ARN) of the destination resource"): exactly one record per discarded batch
//! is written there, describing the BATCH — the function, what ended it, how many attempts it
//! had and what the last answered, beside the stream it was read from, the shard, the
//! sequence-number window it covers and how many records it held — and never the record
//! payloads. A batch the function PROCESSED is not a batch Lambda gave up on, so it writes
//! nothing there at all.
//!
//! Every row's verdict is a consequence of the handler having been invoked with the batch:
//! the function's package RUNS, and whether it refuses the batch is decided inside the handler
//! from each record's own data.
//!
//! The rows differ along what the rule generalises over: which shard source the records sit in
//! (a Kinesis data stream, a DynamoDB table's stream), which kind of resource the failure
//! destination is (a queue, a topic that relays to one), how many retries the mapping allows
//! (none, one), how many records the batch holds (one, several) and where in the batch the
//! record the handler refuses sits.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// The values the record of a discarded batch carries on the WIRE, written out here rather
/// than imported from the emulator so a change to either side of them fails a row.
///
/// Source: AWS's published on-failure record for a stream event source — the schema version
/// the record carries, the condition it reports for a batch whose retries ran out, and the
/// member each of the two shard sources describes its batch under.
const RECORD_SCHEMA_VERSION: &str = "1.0";
/// See [`RECORD_SCHEMA_VERSION`].
const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";
/// See [`RECORD_SCHEMA_VERSION`].
const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";
/// See [`RECORD_SCHEMA_VERSION`].
const DYNAMODB_STREAM_BATCH_INFO: &str = "DDBStreamBatchInfo";

/// The Region every call below is made in, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about it and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `X-Amz-Target` prefixes of the services this suite drives (each service's own
/// `OPERATIONS.md`, "Wire protocol").
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";
/// See [`KINESIS_TARGET_PREFIX`].
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";
/// See [`KINESIS_TARGET_PREFIX`].
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `StartingPosition` both shard sources admit, which reads the records already sitting in
/// the stream: "Required for Amazon Kinesis and Amazon DynamoDB Stream event sources"
/// (`CreateEventSourceMappingInput$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// What `LastProcessingResult` reports for an attempt that processed the batch
/// (`EventSourceMappingConfiguration$LastProcessingResult`).
const RESULT_OK: &str = "OK";

/// The partition key the table of every table-stream row is keyed on.
const KEY: &str = "pk";

/// The marker a record carries to be one the handler refuses. Nothing in the emulator reads
/// it: it is read by the handler, out of the record's own data.
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

/// How long a row waits for work the CLOCK owes — the mapping settling, the batch being read,
/// the attempts being spent and the record reaching the destination.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Which shard source a row's records sit in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// An Amazon Kinesis data stream.
    Kinesis,
    /// An Amazon DynamoDB table's stream.
    TableStream,
}

impl Source {
    /// The member the batch of this source is described under in the record of a discarded
    /// batch.
    fn batch_info_member(self) -> &'static str {
        match self {
            Self::Kinesis => KINESIS_BATCH_INFO,
            Self::TableStream => DYNAMODB_STREAM_BATCH_INFO,
        }
    }
}

/// Which kind of resource a row's failure destination is — the kinds `OnFailure$Destination`
/// enumerates for a shard source whose write is one message of the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// An Amazon SQS queue, named by its own ARN.
    Queue,
    /// An Amazon SNS topic whose subscriber is that queue.
    Topic,
}

/// One row: where the records sit, what they are, how many retries the mapping allows and
/// where it records a batch it gives up on.
struct Row {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    source: Source,
    target: Target,
    /// The `MaximumRetryAttempts` the mapping states.
    retries: i64,
    /// The data of each record put on the source before the mapping is created, in order.
    records: &'static [&'static str],
}

impl Row {
    /// How many attempts the batch gets: the first one plus the retries the mapping states.
    fn attempts(&self) -> i64 {
        self.retries + 1
    }
}

/// The handler every function below runs: it reads each record of the batch it was handed out
/// of that record's OWN data — whichever shard source it came from — and RAISES for a record
/// carrying [`POISON`], so a batch it refuses is a batch it was really handed.
fn handler_source() -> String {
    format!(
        r#"
import base64
import json

MARKER = "{POISON}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    read = []
    for record in records:
        source = record["eventSource"]
        if source == "aws:kinesis":
            text = base64.b64decode(record["kinesis"]["data"]).decode()
        elif source == "aws:dynamodb":
            text = json.dumps(record["dynamodb"]["Keys"])
        else:
            raise ValueError("eventSource is " + repr(source))
        read.append(text)
        if MARKER in text:
            raise RuntimeError("the handler cannot process " + text)
    return {{"processed": read}}
"#
    )
}

/// Lambda, Kinesis, DynamoDB, DynamoDB Streams, Amazon SQS and Amazon SNS on ONE engine: the
/// mapping's event source is a stream of this emulator and the destination it records a
/// discarded batch at is a resource of this emulator too — the shape `core/router/src/lib.rs`
/// mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
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
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One AWS JSON call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
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

/// One Kinesis call.
async fn kinesis(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "kinesis",
        KINESIS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One DynamoDB call.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "dynamodb",
        DYNAMODB_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call.
///
/// `pub(crate)` because every suite that observes a mapping's failure destination through a
/// queue drives Amazon SQS over this one wire
/// (`tests/all/kinesis_event_source_retry_budget.rs`).
pub(crate) async fn sqs(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        "sqs",
        SQS_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
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
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(&response.into_body()).to_string())
}

/// The text one XML element of an Amazon SNS answer carries.
fn element(document: &str, name: &str) -> Option<String> {
    let opening = format!("<{name}>");
    let closing = format!("</{name}>");
    let from = document.find(&opening)? + opening.len();
    let to = document[from..].find(&closing)? + from;
    Some(document[from..to].to_string())
}

/// A queue of this account: its url and its ARN, both as the queue itself reports them.
///
/// `pub(crate)` for the same reason as [`sqs`]: one fixture for the queue a discarded batch's
/// record is read off.
pub(crate) async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
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

/// A topic of this account with `queue_arn` subscribed to it for RAW delivery — "this
/// eliminates the need for the endpoints to process JSON formatting"
/// (`RawMessageDelivery`), so the queue holds the published message itself — and the queue's
/// own `Policy` written to admit that topic's deliveries. Answers the topic's ARN.
///
/// `pub(crate)` because every suite that observes a topic destination through a queue stands
/// the pair up this one way (`tests/all/event_source_mapping_destination_role.rs`).
pub(crate) async fn given_topic_fanning_out(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_url: &str,
    queue_arn: &str,
) -> String {
    let (status, created) = sns(router, "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic {name:?} answered {status} {created}"
    );
    let topic_arn = element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TopicArn in {created}"));
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
    let (status, subscribed) = sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", topic_arn.as_str()),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn),
            ("ReturnSubscriptionArn", "true"),
            ("Attributes.entry.1.key", "RawMessageDelivery"),
            ("Attributes.entry.1.value", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Subscribe answered {status} {subscribed}"
    );
    topic_arn
}

/// A one-shard Kinesis data stream, waited out to `ACTIVE`; answers its ARN.
async fn given_stream(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = kinesis(
        router,
        "CreateStream",
        &json!({ "StreamName": name, "ShardCount": 1 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStream answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, summary) =
            kinesis(router, "DescribeStreamSummary", &json!({ "StreamName": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStreamSummary answered {status} {summary}"
        );
        let description = &summary["StreamDescriptionSummary"];
        if description["StreamStatus"] == json!("ACTIVE") {
            return description["StreamARN"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no StreamARN in {summary}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stream never became ACTIVE: {summary}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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
                "StreamViewType": "NEW_AND_OLD_IMAGES",
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

/// Put one record carrying `data` onto the row's source, the way a client writes one.
async fn put_record(
    router: &MultiServiceRouter,
    case: &str,
    source: Source,
    name: &str,
    data: &str,
) {
    match source {
        Source::Kinesis => {
            let (status, written) = kinesis(
                router,
                "PutRecord",
                &json!({
                    "StreamName": name,
                    "PartitionKey": data,
                    "Data": base64::engine::general_purpose::STANDARD.encode(data.as_bytes()),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: PutRecord answered {status} {written}"
            );
        }
        Source::TableStream => {
            let (status, written) = dynamodb(
                router,
                "PutItem",
                &json!({ "TableName": name, "Item": { KEY: { "S": data } } }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: PutItem answered {status} {written}"
            );
        }
    }
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
/// recording a batch it gives up on at `destination`; answers its UUID and the `FunctionArn`
/// it reports.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    retries: i64,
    destination: &str,
) -> (String, String) {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": 10,
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
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string();
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string();
    (uuid, function_arn)
}

/// What the mapping reports as its `LastProcessingResult`, right now.
async fn processing_result(router: &MultiServiceRouter, case: &str, uuid: &str) -> Option<String> {
    let (status, read) = lambda(
        router,
        "GET",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetEventSourceMapping answered {status} {read}"
    );
    read["LastProcessingResult"].as_str().map(str::to_string)
}

/// Wait until the mapping reports `RESULT_OK`, which is a pass that PROCESSED a batch.
async fn await_processed_batch(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let result = processing_result(router, case, uuid).await;
        if result.as_deref() == Some(RESULT_OK) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported a processed batch, got {result:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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
            "case {case:?}: expected the record of the discarded batch on {url} within \
             {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The rows whose batch the handler refuses, so the mapping gives up on it: each shard
/// source, each kind of destination, both retry counts, and a batch of one record as well as
/// one of several with the refused record in the middle of it.
fn discarded_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a kinesis batch of one refused record, no retry, recorded on a queue",
            name: "kinesis-no-retry",
            source: Source::Kinesis,
            target: Target::Queue,
            retries: 0,
            records: &["poison-only"],
        },
        Row {
            case: "a kinesis batch of three records, one retry, recorded on a queue",
            name: "kinesis-one-retry",
            source: Source::Kinesis,
            target: Target::Queue,
            retries: 1,
            records: &["first-ok", "poison-middle", "last-ok"],
        },
        Row {
            case: "a kinesis batch recorded through a topic that relays to a queue",
            name: "kinesis-topic",
            source: Source::Kinesis,
            target: Target::Topic,
            retries: 0,
            records: &["poison-only"],
        },
        Row {
            case: "a table-stream batch of one refused record, one retry, recorded on a queue",
            name: "table-one-retry",
            source: Source::TableStream,
            target: Target::Queue,
            retries: 1,
            records: &["poison-only"],
        },
        Row {
            case: "a table-stream batch of two records, no retry, recorded on a queue",
            name: "table-no-retry",
            source: Source::TableStream,
            target: Target::Queue,
            retries: 0,
            records: &["ok-first", "poison-second"],
        },
    ]
}

/// The rows whose batch the handler PROCESSES, which is no batch to give up on.
fn processed_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a kinesis batch every record of which is processed",
            name: "kinesis-healthy",
            source: Source::Kinesis,
            target: Target::Queue,
            retries: 0,
            records: &["first-ok", "second-ok"],
        },
        Row {
            case: "a table-stream batch every record of which is processed",
            name: "table-healthy",
            source: Source::TableStream,
            target: Target::Queue,
            retries: 0,
            records: &["only-ok"],
        },
    ]
}

/// Everything one row needs standing up: the source, the destination and the mapping between
/// them, with the row's records already in the source.
struct Wired {
    /// The source's own name, to write further records to.
    source_name: String,
    /// The ARN of the stream the mapping reads.
    stream_arn: String,
    /// The url of the queue the record is read off.
    queue_url: String,
    /// The mapping's UUID and the function ARN it reports.
    uuid: String,
    function_arn: String,
}

/// Stand `row` up: its source with its records already in it, its destination, its function
/// and the mapping between them.
async fn wire(router: &MultiServiceRouter, row: &Row) -> Wired {
    let case = row.case;
    let source_name = format!("source-{}", row.name);
    let stream_arn = match row.source {
        Source::Kinesis => given_stream(router, case, &source_name).await,
        Source::TableStream => given_table(router, case, &source_name).await,
    };
    // The records go in BEFORE the mapping exists, so the mapping's first read from
    // `TRIM_HORIZON` takes all of them as ONE batch.
    for data in row.records {
        put_record(router, case, row.source, &source_name, data).await;
    }
    let (queue_url, queue_arn) = given_queue(router, case, &format!("sink-{}", row.name)).await;
    let destination = match row.target {
        Target::Queue => queue_arn.clone(),
        Target::Topic => {
            given_topic_fanning_out(
                router,
                case,
                &format!("topic-{}", row.name),
                &queue_url,
                &queue_arn,
            )
            .await
        }
    };
    let function = format!("function-{}", row.name);
    given_function(router, case, &function).await;
    let (uuid, function_arn) = given_mapping(
        router,
        case,
        &function,
        &stream_arn,
        row.retries,
        &destination,
    )
    .await;
    Wired {
        source_name,
        stream_arn,
        queue_url,
        uuid,
        function_arn,
    }
}

/// RULE: once the attempts a mapping's `MaximumRetryAttempts` allows one batch are spent, the
/// batch is discarded and exactly one record describing that batch — the stream, the shard,
/// the sequence-number window, the record count, the function, the condition and the attempt
/// count, and none of the record payloads — is written to the destination its
/// `DestinationConfig$OnFailure` names.
#[tokio::test]
async fn a_batch_whose_retries_are_spent_is_recorded_at_the_failure_destination() {
    for row in discarded_rows() {
        let case = row.case;
        let router = emulator();
        let wired = wire(&router, &row).await;

        let messages = awaited_messages(&router, case, &wired.queue_url).await;
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: exactly one record must be written per discarded batch, got \
             {messages:?}"
        );
        let body = messages[0]["Body"].as_str().unwrap_or_default().to_string();
        let record: Value = serde_json::from_str(&body)
            .unwrap_or_else(|_| panic!("case {case:?}: the record must be a JSON document: {body}"));

        assert_eq!(
            record["version"],
            json!(RECORD_SCHEMA_VERSION),
            "case {case:?}: the record must carry its schema version, got {record}"
        );
        assert!(
            record["timestamp"]
                .as_str()
                .is_some_and(|at| at.contains('T') && at.len() >= "2026-01-01T00:00:00.000".len()),
            "case {case:?}: the record must be stamped with the instant it was made, got \
             {record}"
        );
        assert_eq!(
            record["requestContext"]["functionArn"],
            json!(wired.function_arn),
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
            "case {case:?}: the record must report what the last attempt answered, got {record}"
        );
        assert_eq!(
            record["responseContext"]["functionError"],
            json!(UNHANDLED),
            "case {case:?}: the record must report the error the last attempt reported, got \
             {record}"
        );

        let info = &record[row.source.batch_info_member()];
        assert_eq!(
            info["streamArn"],
            json!(wired.stream_arn),
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
                "case {case:?}: a batch of several records spans a window, got {record}"
            );
        }
        for data in row.records {
            assert!(
                !body.contains(data),
                "case {case:?}: the record must describe the batch and carry no record \
                 payload, but it holds {data:?}: {body}"
            );
        }

        // The batch was DISCARDED, not left in place: the mapping reads on past it, which the
        // record put on the source now — processed, so the mapping reports `OK` — shows. Once
        // it has, nothing more has been written to the destination: the record is written per
        // discarded batch and not per pass.
        put_record(&router, case, row.source, &wired.source_name, "later-ok").await;
        await_processed_batch(&router, case, &wired.uuid).await;
        let again = drained(&router, case, &wired.queue_url).await;
        assert!(
            again.is_empty(),
            "case {case:?}: the destination must hold one record per discarded batch, but a \
             second arrived: {again:?}"
        );
    }
}

/// RULE: a batch the function PROCESSED is no batch the mapping gave up on, so nothing at all
/// is written to the destination its `DestinationConfig$OnFailure` names.
#[tokio::test]
async fn a_processed_batch_is_recorded_at_no_failure_destination() {
    for row in processed_rows() {
        let case = row.case;
        let router = emulator();
        let wired = wire(&router, &row).await;

        // The batch is processed, which is the mapping reporting `OK`.
        await_processed_batch(&router, case, &wired.uuid).await;
        let written = drained(&router, case, &wired.queue_url).await;
        assert!(
            written.is_empty(),
            "case {case:?}: a processed batch must write nothing to the failure destination, \
             got {written:?}"
        );
        assert_eq!(
            processing_result(&router, case, &wired.uuid).await.as_deref(),
            Some(RESULT_OK),
            "case {case:?}: the mapping must report the batch as processed"
        );
    }
}
