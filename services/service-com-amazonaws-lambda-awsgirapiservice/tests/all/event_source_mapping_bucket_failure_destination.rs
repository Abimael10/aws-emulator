// SPDX-License-Identifier: CC0-1.0

//! What a mapping on a SHARD source does with a batch it gives up on when its failure
//! destination is an Amazon S3 BUCKET — Lambda, Amazon Kinesis, Amazon DynamoDB, DynamoDB
//! Streams and Amazon S3 on ONE engine, so the records are put where a client puts them and
//! the object the mapping wrote is listed and read off the bucket where a client reads it.
//!
//! RULE: `MaximumRetryAttempts` is "Discard records after the specified number of retries", so
//! the count a mapping states bounds the attempts ONE batch gets and once they are spent the
//! batch is discarded; the only account of it is the one record written to the resource the
//! mapping's `DestinationConfig$OnFailure` names ("the destination configuration for failed
//! invocations", whose `Destination` is "the Amazon Resource Name (ARN) of the destination
//! resource", for which "you can configure an Amazon SNS topic, Amazon SQS queue, Amazon S3
//! bucket, or Kafka topic as the destination"). The write is the destination service's OWN
//! write, so a destination that is a BUCKET takes one OBJECT per discarded batch: an S3
//! bucket's ARN names the bucket in its resource field and anything after a `/` is a key
//! prefix inside it, so the object appears under exactly that prefix, keyed below it by the
//! batch's own coordinates, and its payload is the same record every other destination kind
//! gets — the function, what ended the batch, how many attempts it had and what the last
//! answered, beside the stream it was read from, the shard, the sequence-number window it
//! covers, when its first and last record arrived and how many records it held, and never the
//! record payloads.
//!
//! The rows differ along what the rule generalises over: which shard source the records sit in
//! (a Kinesis data stream, a DynamoDB table's stream), how the destination ARN states the
//! place inside the bucket (the bucket alone, a one-segment prefix, a nested prefix spelled
//! with a trailing separator), how many retries the mapping allows (none, one) and how many
//! records the batch holds. None of those is what the rule turns on.
//!
//! Every verdict is a consequence of the handler having been invoked with the batch: the
//! function's package RUNS, and whether it refuses the batch is decided inside the handler
//! from each record's own data.
//!
//! The sibling suite `event_source_mapping_failure_destination.rs` pins the record itself over
//! the two destination kinds whose write is one MESSAGE; this one pins the kind whose write is
//! an object of a bucket.
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

/// The Region every call below is made in, and the Region the stream's ARN carries.
///
/// Not this emulator's default Region, deliberately: an Amazon S3 bucket's ARN names no
/// Region at all, so the one the bucket is reached in has to come from the mapping's own
/// stream rather than from a default that happens to agree with it.
const REGION: &str = "us-west-2";

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
/// (`InvocationResponse$FunctionError`).
const UNHANDLED: &str = "Unhandled";

/// The status an invocation that executed answers (`InvocationResponse$StatusCode`: "The HTTP
/// status code is in the 200 range for a successful request").
const INVOKED_STATUS: i64 = 200;

/// The media type a JSON document is stored under, which Amazon S3 reports back on a read of
/// the object the record was written as.
const JSON_MEDIA_TYPE: &str = "application/json";

/// How long a row waits for work the CLOCK owes — the mapping settling, the batch being read,
/// the attempts being spent and the object reaching the bucket.
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

/// One row: where the records sit, what they are, how many retries the mapping allows and
/// what the destination ARN states inside the bucket.
struct Row {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    source: Source,
    /// What the destination ARN carries after the bucket name, exactly as a caller spells it:
    /// the empty string for an ARN naming the bucket alone.
    stated_prefix: &'static str,
    /// The prefix a caller then LISTS to find the record, which is the same place however the
    /// value above is spelled.
    listed_prefix: &'static str,
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

    /// The bucket this row's destination names.
    fn bucket(&self) -> String {
        format!("sink-{}", self.name)
    }

    /// The destination ARN this row's mapping is created with: an Amazon S3 bucket ARN, which
    /// names no Region and no account, with whatever the row states inside the bucket after
    /// it.
    fn destination(&self) -> String {
        format!("arn:aws:s3:::{}{}", self.bucket(), self.stated_prefix)
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

/// Lambda, Kinesis, DynamoDB, DynamoDB Streams and Amazon S3 on ONE engine: the mapping's
/// event source is a stream of this emulator and the bucket it records a discarded batch in is
/// a bucket of this emulator too — the shape `core/router/src/lib.rs` mounts them in.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// What one Amazon S3 request answered: the status, the document and the media type the store
/// reports for the object a read returned.
struct Answered {
    status: u16,
    body: String,
    media_type: Option<String>,
}

/// One PATH-STYLE Amazon S3 request, which is how the emulated object store is addressed (its
/// operations are bound under a `{Bucket}` path label).
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &str) -> Answered {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
        media_type: parts
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    }
}

/// A bucket of this account, created through Amazon S3's own `CreateBucket`.
///
/// Outside the Region `CreateBucket` defaults to, the create has to STATE the Region the
/// bucket is to live in — "The unspecified location constraint is incompatible for the region
/// specific endpoint this request was sent to" — so the bucket this suite reads is a bucket of
/// [`REGION`], exactly as a client of that Region makes one.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration \
         xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let answered = s3(router, "PUT", &format!("/{bucket}"), &configuration).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: CreateBucket {bucket:?} answered {} {}",
        answered.status, answered.body
    );
}

/// Every key the bucket holds under `prefix` right now, as `ListObjectsV2` reports them.
async fn listed_keys(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let answered = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        "",
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: ListObjectsV2 of {bucket:?} under {prefix:?} answered {} {}",
        answered.status, answered.body
    );
    elements(&answered.body, "Key")
}

/// The text every `<name>` element of an XML document carries, in the order they appear.
fn elements(document: &str, name: &str) -> Vec<String> {
    let opening = format!("<{name}>");
    let closing = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = document;
    while let Some(from) = rest.find(&opening) {
        let rest_of = &rest[from + opening.len()..];
        let Some(to) = rest_of.find(&closing) else {
            break;
        };
        found.push(rest_of[..to].to_string());
        rest = &rest_of[to + closing.len()..];
    }
    found
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

/// Wait until an object has appeared under `prefix`, answering every key that is there —
/// more than one when more than one arrived, which is what makes "exactly one" assertable.
async fn awaited_keys(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let keys = listed_keys(router, case, bucket, prefix).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the record of the discarded batch under {prefix:?} of \
             {bucket:?} within {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The rows whose batch the handler refuses, so the mapping gives up on it: each shard source,
/// each way of stating the place inside the bucket, both retry counts, and a batch of one
/// record as well as one of several with the refused record last.
fn discarded_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a kinesis batch recorded under the prefix the destination states",
            name: "kinesis-prefixed",
            source: Source::Kinesis,
            stated_prefix: "/esm-failures",
            listed_prefix: "esm-failures/",
            retries: 0,
            records: &["poison-only"],
        },
        Row {
            case: "a kinesis batch of two records recorded at the bucket's root",
            name: "kinesis-root",
            source: Source::Kinesis,
            stated_prefix: "",
            listed_prefix: "",
            retries: 1,
            records: &["first-ok", "poison-last"],
        },
        Row {
            case: "a table-stream batch recorded under a nested prefix with a trailing \
                   separator",
            name: "table-nested",
            source: Source::TableStream,
            stated_prefix: "/failed/orders/",
            listed_prefix: "failed/orders/",
            retries: 0,
            records: &["poison-only"],
        },
    ]
}

/// Everything one row needs standing up: the source, the bucket and the mapping between them,
/// with the row's records already in the source.
struct Wired {
    /// The source's own name, to write further records to.
    source_name: String,
    /// The ARN of the stream the mapping reads.
    stream_arn: String,
    /// The mapping's UUID and the function ARN it reports.
    uuid: String,
    function_arn: String,
}

/// Stand `row` up: its source with its records already in it, its bucket, its function and the
/// mapping between them.
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
    given_bucket(router, case, &row.bucket()).await;
    let function = format!("function-{}", row.name);
    given_function(router, case, &function).await;
    let (uuid, function_arn) = given_mapping(
        router,
        case,
        &function,
        &stream_arn,
        row.retries,
        &row.destination(),
    )
    .await;
    Wired {
        source_name,
        stream_arn,
        uuid,
        function_arn,
    }
}

/// RULE: once the attempts a mapping's `MaximumRetryAttempts` allows one batch are spent, the
/// batch is discarded and the record describing it is written to the destination its
/// `DestinationConfig$OnFailure` names — for a BUCKET destination, as exactly one object under
/// the prefix that ARN states inside the bucket, whose payload is that record.
#[tokio::test]
async fn a_batch_whose_retries_are_spent_is_recorded_as_an_object_of_the_bucket() {
    for row in discarded_rows() {
        let case = row.case;
        let router = emulator();
        let bucket = row.bucket();
        let wired = wire(&router, &row).await;

        let keys = awaited_keys(&router, case, &bucket, row.listed_prefix).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: exactly one object must be written per discarded batch, got {keys:?}"
        );
        let key = keys[0].clone();
        assert!(
            key.starts_with(row.listed_prefix),
            "case {case:?}: the object must sit under the prefix the destination states \
             ({:?}), got {key:?}",
            row.listed_prefix
        );
        assert!(
            key.contains(&wired.uuid),
            "case {case:?}: the key must name the mapping that gave up on the batch ({:?}), \
             got {key:?}",
            wired.uuid
        );

        let read = s3(&router, "GET", &format!("/{bucket}/{key}"), "").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetObject {key:?} answered {} {}",
            read.status, read.body
        );
        assert_eq!(
            read.media_type.as_deref(),
            Some(JSON_MEDIA_TYPE),
            "case {case:?}: the record is a JSON document, got {:?}",
            read.media_type
        );
        let body = read.body.clone();
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
            "case {case:?}: the record must be stamped with the instant it was made, got {record}"
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
        let shard = info["shardId"].as_str().unwrap_or_default().to_string();
        assert!(
            shard.starts_with("shardId-"),
            "case {case:?}: the batch must name the shard it was read from, got {record}"
        );
        assert!(
            key.contains(&shard),
            "case {case:?}: the key must name the shard the batch was read from ({shard:?}), \
             got {key:?}"
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
        assert!(
            key.ends_with(&format!("{start}-{end}")),
            "case {case:?}: the key must name the window the batch covers, got {key:?} for \
             {record}"
        );
        for member in ["approximateArrivalOfFirstRecord", "approximateArrivalOfLastRecord"] {
            assert!(
                info[member].as_str().is_some_and(|at| at.contains('T')),
                "case {case:?}: the batch must report {member}, got {record}"
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
        // it has, nothing more has been written to the bucket: the object is written per
        // discarded batch and not per pass.
        put_record(&router, case, row.source, &wired.source_name, "later-ok").await;
        await_processed_batch(&router, case, &wired.uuid).await;
        let again = listed_keys(&router, case, &bucket, row.listed_prefix).await;
        assert_eq!(
            again,
            vec![key.clone()],
            "case {case:?}: the bucket must hold one object per discarded batch, got {again:?}"
        );
    }
}

/// RULE: a batch the function PROCESSED is no batch the mapping gave up on, so nothing at all
/// is written to the bucket its `DestinationConfig$OnFailure` names.
#[tokio::test]
async fn a_processed_batch_writes_no_object_to_the_bucket() {
    let row = Row {
        case: "a kinesis batch every record of which is processed",
        name: "kinesis-healthy",
        source: Source::Kinesis,
        stated_prefix: "/esm-failures",
        listed_prefix: "esm-failures/",
        retries: 0,
        records: &["first-ok", "second-ok"],
    };
    let case = row.case;
    let router = emulator();
    let bucket = row.bucket();
    let wired = wire(&router, &row).await;

    // The batch is processed, which is the mapping reporting `OK`.
    await_processed_batch(&router, case, &wired.uuid).await;
    let written = listed_keys(&router, case, &bucket, row.listed_prefix).await;
    assert!(
        written.is_empty(),
        "case {case:?}: a processed batch must write no object to the failure destination, got \
         {written:?}"
    );
    assert_eq!(
        processing_result(&router, case, &wired.uuid).await.as_deref(),
        Some(RESULT_OK),
        "case {case:?}: the mapping must report the batch as processed"
    );
}
