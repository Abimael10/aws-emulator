// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping on an Amazon DynamoDB table's STREAM does with NO client
//! action at all, end to end over the wire, with DynamoDB, DynamoDB Streams and CloudWatch
//! mounted beside Lambda on ONE engine — so the item is written where a client writes it
//! and what the mapping did is read back where a client reads it.
//!
//! The RULE asserted over the tables below: once a mapping whose `EventSourceArn` names a
//! table's stream is `Enabled`, Lambda reads that stream's shards from the position the
//! mapping carries and INVOKES the mapped function with one envelope per stream record
//! ("Lambda reads items from the event source and invokes the function"), each envelope
//! naming the stream it came from (`eventSource` `aws:dynamodb`, `eventSourceARN`) and
//! carrying the change itself under `dynamodb` — which mutation it was (`eventName`), the
//! item's `Keys`, and the images the stream's `StreamViewType` captures. The attempt at the
//! batch is reported as the mapping's own `LastProcessingResult`, "the result of the event
//! source mapping's last processing attempt", and the mapping is checkpointed past only the
//! records the attempt processed.
//!
//! Every row's verdict is computed INSIDE the handler from the envelope it was handed: a
//! handler that was never invoked, or invoked with an envelope of the wrong shape, reports
//! nothing or raises, and the row fails.
//!
//! The first suite's rows differ along what the rule generalises over: which mutation the
//! item write is (an insert, a change, a removal), which `StreamViewType` the table's
//! stream captures (each of the four the model declares), which `StartingPosition` fixes
//! the mapping's position, how many items are written, the `BatchSize` they are read in,
//! whether the mapping asks for its function's report of which records failed, and how the
//! handler ends (a return, a report, a raise).
//!
//! The second suite is the flow a client wires this up for: the handler reacts to the
//! change by writing a DERIVED item into a second table through the AWS SDK, and that item
//! is read back through `GetItem` — so the record's own values have to have reached the
//! handler for the row to pass.
//!
//! The third suite is the same hand-over for a mapping that aggregates over a TUMBLING
//! WINDOW: "(Kinesis and DynamoDB Streams only) The duration in seconds of a processing
//! window for DynamoDB and Kinesis Streams event sources. A value of 0 seconds indicates no
//! tumbling window" (`TumblingWindowInSeconds`), so an invocation of such a mapping is told
//! which window it is in, which shard and stream its records came from and the state that
//! window has reached, the state the handler returns is handed to the next invocation of
//! that window, and each window ends in one final invocation carrying that state and no
//! records. Its rows differ along the window configured, and each row's verdict is the
//! envelope the handler audited into a second table, so nothing it was not handed can pass.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every call below signs for, and the region every ARN carries.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `X-Amz-Target` prefix of the DynamoDB service (its `OPERATIONS.md`, "Wire
/// protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The `X-Amz-Target` prefix of the CloudWatch service.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The one value `FunctionResponseType` admits
/// (`smithy_query(query="shape", shape="FunctionResponseType")`).
const REPORT_BATCH_ITEM_FAILURES: &str = "ReportBatchItemFailures";

/// The `StartingPosition` values a DynamoDB stream source may be read from: "AT_TIMESTAMP
/// is supported only for Amazon Kinesis streams, Amazon DocumentDB, Amazon MSK, and
/// self-managed Apache Kafka" (`CreateEventSourceMappingInput$StartingPosition`), so the
/// other two of the model's `EventSourcePosition` enum are what this source admits.
pub(crate) const TRIM_HORIZON: &str = "TRIM_HORIZON";
/// See [`TRIM_HORIZON`].
pub(crate) const LATEST: &str = "LATEST";

/// The `StreamViewType` values the model declares
/// (`smithy_query(query="shape", shape="StreamViewType")`, DynamoDB): what the stream
/// captures for each change.
const KEYS_ONLY: &str = "KEYS_ONLY";
/// See [`KEYS_ONLY`].
const NEW_IMAGE: &str = "NEW_IMAGE";
/// See [`KEYS_ONLY`].
const OLD_IMAGE: &str = "OLD_IMAGE";
/// See [`KEYS_ONLY`].
pub(crate) const NEW_AND_OLD_IMAGES: &str = "NEW_AND_OLD_IMAGES";

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// What `LastProcessingResult` reports for an attempt that processed the batch, and for one
/// that did not (`EventSourceMappingConfiguration$LastProcessingResult`).
pub(crate) const RESULT_OK: &str = "OK";
/// See [`RESULT_OK`].
const RESULT_FUNCTION_CALL_FAILED: &str = "PROBLEM: Function call failed";

/// The `State` a mapping a client has deleted reports
/// (`EventSourceMappingConfiguration$State`).
const DELETING: &str = "Deleting";

/// The namespace Lambda's own metrics live in, the series one admitted invocation is
/// counted on and the dimension it is counted per (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";
/// See [`NAMESPACE`].
const INVOCATIONS: &str = "Invocations";
/// See [`NAMESPACE`].
const FUNCTION_DIMENSION: &str = "FunctionName";

/// How long a wait for work the mapping has to do is given before the row is a failure.
/// Well above the table's and the stream's own settle spans and the poller's interval, so a
/// row fails only when the mapping did not do the work at all.
pub(crate) const BUDGET: Duration = Duration::from_secs(45);

/// How long between two looks while waiting.
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The partition key every table below is keyed on.
pub(crate) const KEY: &str = "pk";

/// The key value whose record the handler reports as one it did not process, and the one
/// that makes it raise instead. Nothing in the emulator reads either: they are read by the
/// handler, out of the record's own keys.
const POISON: &str = "poison";
/// See [`POISON`].
const RAISE: &str = "raise";

/// Lambda, DynamoDB, DynamoDB Streams and CloudWatch on one engine: the mapping's event
/// source is a table's stream and the series it publishes is CloudWatch's, so all of them
/// are reached in-process — the shape `core/router/src/lib.rs` mounts them in.
pub(crate) fn emulator() -> MultiServiceRouter {
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
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`]. Nothing in it is
/// verified: the harness turns request authentication off.
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

/// One RPC call against the service whose signing name is `signing_name`.
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

/// One DynamoDB call.
pub(crate) async fn dynamodb(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
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

/// One CloudWatch call.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        "monitoring",
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// A table keyed on [`KEY`], with a stream capturing `view_type` when one is named, waited
/// out to `ACTIVE`; answers the table's `LatestStreamArn` (empty for a table with no
/// stream).
pub(crate) async fn given_table(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    view_type: Option<&str>,
) -> String {
    let mut request = json!({
        "TableName": name,
        "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
        "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if let Some(view_type) = view_type {
        request["StreamSpecification"] =
            json!({ "StreamEnabled": true, "StreamViewType": view_type });
    }
    let (status, created) = dynamodb(router, "CreateTable", &request).await;
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
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory
/// the way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// A function whose package RUNS `source`, told `environment` about what it must see,
/// waited out to `Active`.
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source: &str,
    environment: Value,
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
            "Timeout": 30,
            "Environment": { "Variables": environment },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
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

/// Create the mapping between `function` and `stream_arn`, answering its UUID.
pub(crate) async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    starting_position: &str,
    batch_size: i64,
    reads_report: bool,
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": starting_position,
        "BatchSize": batch_size,
        "Enabled": true,
    });
    if reads_report {
        request["FunctionResponseTypes"] = json!([REPORT_BATCH_ITEM_FAILURES]);
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, &request).await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["EventSourceArn"],
        json!(stream_arn),
        "case {case:?}: the mapping must name the stream it was created for, got {created}"
    );
    created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string()
}

/// `GetEventSourceMapping` on one mapping.
async fn get_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) -> Value {
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
    read
}

/// `DeleteEventSourceMapping` on one mapping, which is what a client tears the trigger down
/// with: "When you delete an event source mapping, it enters a `Deleting` state"
/// (`EventSourceMappingConfiguration$State`), so the mapping reports that state and reads
/// nothing more off its source.
pub(crate) async fn stop_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let (status, deleted) = lambda(
        router,
        "DELETE",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: DeleteEventSourceMapping answered {status} {deleted}"
    );
    assert_eq!(
        deleted["State"],
        json!(DELETING),
        "case {case:?}: a deleted mapping must report that it is being deleted, got {deleted}"
    );
}

/// Wait until the mapping reports `expected` as its `LastProcessingResult`, answering every
/// result it reported on the way.
///
/// Every pass the mapping makes OVERWRITES that member, so a reader that took whatever result
/// it found first could be reading a pass that was retried rather than the one it is waiting
/// for: looking again until `expected` is there waits for THAT attempt, whatever the mapping
/// came to before it. `deadline` is the CALLER's, so a suite that opens one deadline for the
/// whole chain it drives shares that patience here instead of this wait taking a span of its
/// own.
pub(crate) async fn results_until(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    expected: &str,
    deadline: Instant,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    loop {
        let read = get_mapping(router, case, uuid).await;
        if let Some(result) = read["LastProcessingResult"].as_str() {
            if seen.last().map(String::as_str) != Some(result) {
                seen.push(result.to_string());
            }
            if result == expected {
                return seen;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must come to report {expected:?} as its \
             LastProcessingResult, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How many invocations CloudWatch holds for `function`, summed over a window around now.
pub(crate) async fn invocation_count(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": INVOCATIONS,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": function }],
            "StartTime": now - 600.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics answered {status} {stats}"
    );
    stats["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// The handler the first suite runs, with the markers above bound to the names it reads
/// them by — so there is ONE spelling of each on both sides of the interpreter.
///
/// It asserts the shape of the envelope Lambda hands a DynamoDB stream batch over as — one
/// record per envelope, naming its source and stream, carrying the change's name and the
/// item's keys, and carrying exactly the images the stream's view type captures — and
/// decides what to report from the record's own KEY, so the report is a consequence of the
/// record having arrived.
fn inspecting_handler() -> String {
    format!(
        r#"
import os

STREAM_ARN = os.environ["STREAM_ARN"]
VIEW_TYPE = os.environ["VIEW_TYPE"]
KEY = "{KEY}"
POISON = "{POISON}"
RAISE = "{RAISE}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    failures = []
    for record in records:
        if record["eventSource"] != "aws:dynamodb":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if record["eventSourceARN"] != STREAM_ARN:
            raise ValueError("eventSourceARN is " + repr(record["eventSourceARN"]))
        if not record.get("eventID"):
            raise ValueError("a record carries no eventID")
        if not record.get("eventVersion"):
            raise ValueError("a record carries no eventVersion")
        if record["eventName"] not in ("{INSERT}", "{MODIFY}", "{REMOVE}"):
            raise ValueError("eventName is " + repr(record["eventName"]))
        change = record["dynamodb"]
        if change["StreamViewType"] != VIEW_TYPE:
            raise ValueError("StreamViewType is " + repr(change["StreamViewType"]))
        sequence_number = change["SequenceNumber"]
        if not sequence_number:
            raise ValueError("a record carries no SequenceNumber")
        key = change["Keys"][KEY]["S"]
        if not key:
            raise ValueError("a record carries no key")
        carries_new = VIEW_TYPE in ("{NEW_IMAGE}", "{NEW_AND_OLD_IMAGES}")
        carries_old = VIEW_TYPE in ("{OLD_IMAGE}", "{NEW_AND_OLD_IMAGES}")
        has_new = "NewImage" in change
        has_old = "OldImage" in change
        if has_new and not carries_new:
            raise ValueError(VIEW_TYPE + " must capture no NewImage")
        if has_old and not carries_old:
            raise ValueError(VIEW_TYPE + " must capture no OldImage")
        if carries_new and record["eventName"] != "{REMOVE}":
            if not has_new:
                raise ValueError(VIEW_TYPE + " must capture a NewImage")
            if change["NewImage"][KEY]["S"] != key:
                raise ValueError("NewImage names another item than the keys do")
        if carries_old and record["eventName"] in ("{MODIFY}", "{REMOVE}"):
            if not has_old:
                raise ValueError(VIEW_TYPE + " must capture an OldImage")
        if key.startswith(RAISE):
            raise RuntimeError("the handler cannot process " + key)
        if key.startswith(POISON):
            failures.append({{"itemIdentifier": sequence_number}})
    return {{"batchItemFailures": failures}}
"#
    )
}

/// What one item write of a row is, and what change it makes to the table.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Write {
    /// `PutItem` of an item that was not there: an `INSERT`.
    Insert,
    /// `PutItem` over an item that was: a `MODIFY`.
    Overwrite,
    /// `UpdateItem` on an item that was there: a `MODIFY`.
    Update,
    /// `DeleteItem`: a `REMOVE`.
    Delete,
}

/// Make one change to `table` for the item `key` names.
pub(crate) async fn write_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    write: Write,
) {
    let (operation, body) = match write {
        Write::Insert | Write::Overwrite => (
            "PutItem",
            json!({
                "TableName": table,
                "Item": { KEY: { "S": key }, "note": { "S": format!("{write:?}") } },
            }),
        ),
        Write::Update => (
            "UpdateItem",
            json!({
                "TableName": table,
                "Key": { KEY: { "S": key } },
                "UpdateExpression": "SET #n = :v",
                "ExpressionAttributeNames": { "#n": "note" },
                "ExpressionAttributeValues": { ":v": { "S": "updated" } },
            }),
        ),
        Write::Delete => (
            "DeleteItem",
            json!({ "TableName": table, "Key": { KEY: { "S": key } } }),
        ),
    };
    let (status, written) = dynamodb(router, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} answered {status} {written}"
    );
}

/// One row of the first suite: the stream's shape, the writes made to the table, and what
/// the mapping must report once the handler has run on the records they produced.
struct Row {
    case: &'static str,
    /// What the table's stream captures.
    view_type: &'static str,
    /// The `StartingPosition` the mapping is created with.
    starting_position: &'static str,
    /// The key and the change of each write, in order.
    writes: &'static [(&'static str, Write)],
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// Whether the mapping asks for its function's report of which records failed.
    reads_report: bool,
    /// The `LastProcessingResult` the mapping must come to report.
    expected_result: &'static str,
}

/// RULE: an `Enabled` mapping on a DynamoDB table's stream reads that stream's records and
/// invokes the mapped function with them, and what the handler makes of each record it was
/// handed is what the mapping reports as the result of the attempt.
#[tokio::test]
async fn a_dynamodb_stream_mapping_invokes_its_function_with_the_records_it_read() {
    let rows = vec![
        // The recorded shape: an INSERT on a NEW_AND_OLD_IMAGES stream read from the
        // oldest retained record.
        Row {
            case: "an-insert-with-both-images",
            view_type: NEW_AND_OLD_IMAGES,
            starting_position: TRIM_HORIZON,
            writes: &[("trigger-insert", Write::Insert)],
            batch_size: 100,
            reads_report: true,
            expected_result: RESULT_OK,
        },
        // Every kind of change, on a stream that captures only the new image, read one
        // record at a time from the newest position.
        Row {
            case: "insert-modify-and-remove-with-the-new-image",
            view_type: NEW_IMAGE,
            starting_position: LATEST,
            writes: &[
                ("item-a", Write::Insert),
                ("item-a", Write::Overwrite),
                ("item-b", Write::Insert),
                ("item-a", Write::Delete),
            ],
            batch_size: 1,
            reads_report: true,
            expected_result: RESULT_OK,
        },
        // A stream that captures neither image: the record carries the keys alone.
        Row {
            case: "a-keys-only-stream-carries-no-image",
            view_type: KEYS_ONLY,
            starting_position: TRIM_HORIZON,
            writes: &[("item-c", Write::Insert), ("item-c", Write::Update)],
            batch_size: 10,
            reads_report: false,
            expected_result: RESULT_OK,
        },
        // A stream that captures only the old image, over an update and a delete.
        Row {
            case: "an-old-image-stream-over-a-change-and-a-removal",
            view_type: OLD_IMAGE,
            starting_position: TRIM_HORIZON,
            writes: &[
                ("item-d", Write::Insert),
                ("item-d", Write::Update),
                ("item-d", Write::Delete),
            ],
            batch_size: 100,
            reads_report: false,
            expected_result: RESULT_OK,
        },
        // The handler names the batch's FIRST record as one it did not process, so the
        // shard stays where it is and the mapping reports the function-call problem.
        Row {
            case: "the-first-record-is-not-processed",
            view_type: NEW_AND_OLD_IMAGES,
            starting_position: TRIM_HORIZON,
            writes: &[(POISON, Write::Insert), ("item-e", Write::Insert)],
            batch_size: 100,
            reads_report: true,
            expected_result: RESULT_FUNCTION_CALL_FAILED,
        },
        // A handler that RAISES on a record answers no report at all: the invocation
        // failed, so no record of the batch is processed.
        Row {
            case: "the-handler-raises-on-a-record",
            view_type: NEW_IMAGE,
            starting_position: TRIM_HORIZON,
            writes: &[("item-f", Write::Insert), (RAISE, Write::Insert)],
            batch_size: 100,
            reads_report: true,
            expected_result: RESULT_FUNCTION_CALL_FAILED,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let table = format!("source-{case}");
        let function = format!("consume-{case}");
        let stream_arn = given_table(&router, case, &table, Some(row.view_type)).await;
        assert!(
            stream_arn.contains("/stream/"),
            "case {case:?}: a stream-enabled table must report its stream ARN, got \
             {stream_arn:?}"
        );
        given_function(
            &router,
            case,
            &function,
            &inspecting_handler(),
            json!({ "STREAM_ARN": stream_arn, "VIEW_TYPE": row.view_type }),
        )
        .await;
        let uuid = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.starting_position,
            row.batch_size,
            row.reads_report,
        )
        .await;

        for (key, write) in row.writes {
            write_item(&router, case, &table, key, *write).await;
        }

        // The records reach the handler, and what it made of them is what the mapping
        // reports: no other component of this emulator decides this value.
        let seen =
            results_until(&router, case, &uuid, row.expected_result, Instant::now() + BUDGET).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(row.expected_result),
            "case {case:?}: expected the mapping to report {:?}, saw {seen:?}",
            row.expected_result
        );

        // And the invocations it made are the function's own, metered where a client reads
        // them: a function nothing invoked has none.
        let invocations = invocation_count(&router, case, &function).await;
        assert!(
            invocations >= 1.0,
            "case {case:?}: the batch read off the stream must be handed to {function} as an \
             invocation, but {NAMESPACE}/{INVOCATIONS} for it summed to {invocations}"
        );

        // The trigger is torn down as a client tears one down, which is also what stops the
        // mapping from reading its source any further.
        stop_mapping(&router, case, &uuid).await;
    }
}

/// The handler the second suite runs: for each record of the change it was handed, it
/// writes a DERIVED item into the output table through the AWS SDK — the item's key and the
/// change's name, both taken out of the record — so the item can only appear if the record
/// reached it carrying those values.
fn writing_handler() -> String {
    format!(
        r#"
import os

import boto3

OUT_TABLE = os.environ["OUT_TABLE"]
KEY = "{KEY}"


def lambda_handler(event, context):
    client = boto3.client("dynamodb")
    written = 0
    for record in event["Records"]:
        change = record["dynamodb"]
        image = change.get("NewImage") or change["Keys"]
        client.put_item(
            TableName=OUT_TABLE,
            Item={{
                KEY: {{"S": image[KEY]["S"]}},
                "event": {{"S": record["eventName"]}},
            }},
        )
        written += 1
    return {{"written": written}}
"#
    )
}

/// Wait until `table` holds the item `key` names CARRYING the change `event` names,
/// answering it.
///
/// The change under test is not always the only record a mapping delivers: a removal has an
/// item to remove only once one is there, so its stream carries the insert that put it there
/// too, and the handler writes the derived item once per record. What the case is about is
/// therefore the item as the record under test leaves it, which is what this waits for
/// rather than for the key to appear.
async fn wait_for_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    event: &str,
) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = dynamodb(
            router,
            "GetItem",
            &json!({ "TableName": table, "Key": { KEY: { "S": key } } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetItem answered {status} {read}"
        );
        if read
            .pointer("/Item/event/S")
            .and_then(Value::as_str)
            .is_some_and(|written| written == event)
        {
            return read["Item"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {table} never came to hold the item {key:?} the handler had to \
             write for the {event} record: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: the records a mapping delivers carry the change itself, so a handler can act on it
/// — the downstream write it makes from the record's own values is the proof the record
/// arrived.
///
/// The rows differ along which change the handler reacts to and which image the stream's
/// view type gives it the item's values from.
#[tokio::test]
async fn a_delivered_change_reaches_a_handler_that_writes_it_downstream() {
    // (case, what the stream captures, the key written, the change made, the eventName the
    // derived item must carry)
    let rows: Vec<(&str, &str, &str, Write, &str)> = vec![
        (
            "an-insert-under-both-images",
            NEW_AND_OLD_IMAGES,
            "trigger-insert",
            Write::Insert,
            INSERT,
        ),
        (
            "a-removal-under-a-keys-only-stream",
            KEYS_ONLY,
            "trigger-remove",
            Write::Delete,
            REMOVE,
        ),
    ];

    for (case, view_type, key, write, expected_event) in rows {
        let router = emulator();
        let source = format!("source-{case}");
        let output = format!("output-{case}");
        let function = format!("writer-{case}");
        let stream_arn = given_table(&router, case, &source, Some(view_type)).await;
        given_table(&router, case, &output, None).await;
        given_function(
            &router,
            case,
            &function,
            &writing_handler(),
            json!({ "OUT_TABLE": output }),
        )
        .await;

        // A removal has an item to remove only once one is there, and that write is made
        // BEFORE the mapping exists: a mapping reading from the oldest retained record
        // reads the backlog too, so the record of the removal is the one the handler must
        // act on either way.
        if matches!(write, Write::Delete) {
            write_item(&router, case, &source, key, Write::Insert).await;
        }
        let uuid = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            TRIM_HORIZON,
            100,
            false,
        )
        .await;
        write_item(&router, case, &source, key, write).await;

        let item = wait_for_item(&router, case, &output, key, expected_event).await;
        assert_eq!(
            item["event"]["S"],
            json!(expected_event),
            "case {case:?}: the derived item must carry the change the record reported, got \
             {item}"
        );
        assert_eq!(
            item[KEY]["S"],
            json!(key),
            "case {case:?}: the derived item must be keyed on the changed item's key, got \
             {item}"
        );

        // The delivery is what the mapping reports it came to — recorded by the poller
        // just after the invocation returned, so it is waited for rather than read at the
        // instant the item appeared.
        let seen = results_until(&router, case, &uuid, RESULT_OK, Instant::now() + BUDGET).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(RESULT_OK),
            "case {case:?}: a mapping whose batch was processed must report it, saw {seen:?}"
        );

        // The trigger is torn down as a client tears one down, which is also what stops the
        // mapping from reading its source any further.
        stop_mapping(&router, case, &uuid).await;
    }
}

/// The value `TumblingWindowInSeconds` documents as no window: "A value of 0 seconds
/// indicates no tumbling window."
const NO_TUMBLING_WINDOW: i64 = 0;

/// The key the handler of the third suite audits a windowed invocation under, and the one it
/// audits the window's FINAL invocation under. Each is overwritten by the next invocation of
/// its kind, so what the row reads is what the last of them was handed.
const WINDOWED_AUDIT: &str = "windowed";
/// See [`WINDOWED_AUDIT`].
const FINAL_AUDIT: &str = "final";

/// The handler the third suite runs: it audits the envelope of EVERY invocation it is handed
/// into the output table — the window it was told it is in, the shard and stream its records
/// came from, the state it was handed and how many records it was given — and returns the
/// aggregate as the window's next state.
///
/// Everything it writes is taken out of the event, so a member the invocation did not carry
/// is audited as absent and no row can pass on a value the handler was never handed. What
/// each window's FINAL invocation was handed is ACCUMULATED rather than overwritten: the
/// records of a row may fall into more than one window — where they are read in two passes
/// either side of a window's end, which is the clock's to decide and not the rule's — and the
/// aggregate over the row is then the sum of what those windows' finals were handed.
fn aggregating_handler() -> String {
    format!(
        r#"
import json
import os

import boto3

AUDIT_TABLE = os.environ["AUDIT_TABLE"]
KEY = "{KEY}"


def lambda_handler(event, context):
    client = boto3.client("dynamodb")
    records = event.get("Records") or []
    window = event.get("window") or {{}}
    state = event.get("state")
    if state is None:
        state = {{}}
    final = bool(event.get("isFinalInvokeForWindow"))
    carried = int(state.get("seen", 0))
    seen = carried + len(records)
    audited = "{FINAL_AUDIT}" if final else "{WINDOWED_AUDIT}"
    item = {{
        KEY: {{"S": audited}},
        "has_window": {{"S": "true" if event.get("window") else "false"}},
        "window_start": {{"S": window.get("start") or ""}},
        "window_end": {{"S": window.get("end") or ""}},
        "shard_id": {{"S": event.get("shardId") or ""}},
        "source_arn": {{"S": event.get("eventSourceARN") or ""}},
        "state_in": {{"S": json.dumps(state, sort_keys=True)}},
        "records": {{"N": str(len(records))}},
        "early": {{"S": "true" if event.get("isWindowTerminatedEarly") else "false"}},
        "seen": {{"N": str(seen)}},
    }}
    if final:
        held = client.get_item(
            TableName=AUDIT_TABLE, Key={{KEY: {{"S": audited}}}}
        ).get("Item") or {{}}
        item["finals_seen"] = {{
            "N": str(int(held.get("finals_seen", {{}}).get("N", "0")) + carried)
        }}
        item["finals_count"] = {{
            "N": str(int(held.get("finals_count", {{}}).get("N", "0")) + 1)
        }}
    client.put_item(TableName=AUDIT_TABLE, Item=item)
    return {{"state": {{"seen": seen}}}}
"#
    )
}

/// The row's mapping on `stream_arn`, carrying the tumbling window the row states; read back
/// from the create's own answer, so a row cannot pass against a window the mapping never
/// carried.
async fn given_windowed_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    window_seconds: Option<i64>,
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": TRIM_HORIZON,
        "BatchSize": 10,
        "Enabled": true,
    });
    if let Some(seconds) = window_seconds {
        request["TumblingWindowInSeconds"] = json!(seconds);
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, &request).await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["TumblingWindowInSeconds"],
        json!(window_seconds.unwrap_or(NO_TUMBLING_WINDOW)),
        "case {case:?}: the mapping must carry the window it was created with, got {created}"
    );
    created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string()
}

/// The audited invocation `key` names, looked for again until `deadline`; `None` when none
/// was audited by then.
async fn audited(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    deadline: Instant,
) -> Option<Value> {
    loop {
        let (status, read) = dynamodb(
            router,
            "GetItem",
            &json!({ "TableName": table, "Key": { KEY: { "S": key } } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetItem answered {status} {read}"
        );
        if read["Item"].is_object() {
            return Some(read["Item"].clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One audited member, as the string the handler wrote it as.
fn audited_text(item: &Value, member: &str) -> String {
    item[member]["S"].as_str().unwrap_or_default().to_string()
}

/// One audited count, as the number the handler wrote it as; a member it wrote none for
/// answers a count no row expects.
fn audited_count(item: &Value, member: &str) -> i64 {
    item[member]["N"]
        .as_str()
        .and_then(|held| held.parse::<i64>().ok())
        .unwrap_or(-1)
}

/// One instant the handler audited, in whole seconds since the epoch.
fn audited_instant(case: &str, item: &Value, member: &str) -> i64 {
    let held = audited_text(item, member);
    ::aws_smithy_types::DateTime::from_str(&held, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {member} must be an ISO-8601 instant, got {held:?} ({error})")
        })
        .secs()
}

/// The audited FINAL invocations of `table` once the states they were handed together reach
/// `expected`, looked for again until `deadline`; the record as it last stood when they do
/// not, which is what the row then reports.
///
/// The records of a row can fall into more than one window — they are read in one pass here,
/// but where a window's end falls between two passes is the clock's to decide — so the figure
/// the rule is about is the sum of what those windows' finals were handed, and it is complete
/// only once every window that took a record has finalised.
async fn await_aggregate(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    expected: i64,
    deadline: Instant,
) -> Value {
    loop {
        let held = audited(router, case, table, FINAL_AUDIT, deadline).await;
        let reached = held
            .as_ref()
            .is_some_and(|item| audited_count(item, "finals_seen") >= expected);
        if reached || Instant::now() >= deadline {
            return held.unwrap_or(Value::Null);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a mapping carrying a `TumblingWindowInSeconds` window tells every invocation which
/// window it is in — the window's own bounds, the shard and the stream its records came from
/// and the state that window has reached — carries the state the handler returned into the
/// next invocation of that window, and ends each window with one final invocation holding
/// that state and no records; a mapping with no window tells an invocation none of it.
///
/// The rows differ along what the rule generalises over: the window configured (the member
/// absent, and two different durations), and so the bounds the window's own invocations
/// report. The value the model documents as no window (0) and both ends of the member's own
/// range are read off the member by the unit table in
/// `state/event_source_poller/tumbling_window.rs`; the rows here are the ones that need a
/// stream, a function and a delivery to decide.
#[tokio::test]
async fn a_tumbling_window_mapping_carries_its_window_and_its_state_into_each_invocation() {
    // (case, `TumblingWindowInSeconds` as the create states it, the window it aggregates
    //  over)
    let rows: Vec<(&str, Option<i64>, Option<i64>)> = vec![
        ("no-window-configured", None, None),
        ("a-one-second-window", Some(1), Some(1)),
        ("a-three-second-window", Some(3), Some(3)),
    ];

    for (case, configured, window_seconds) in rows {
        let router = emulator();
        let source = format!("windowed-{case}");
        let audit = format!("windowed-audit-{case}");
        let function = format!("aggregate-{case}");
        let stream_arn = given_table(&router, case, &source, Some(NEW_AND_OLD_IMAGES)).await;
        given_table(&router, case, &audit, None).await;
        given_function(
            &router,
            case,
            &function,
            &aggregating_handler(),
            json!({ "AUDIT_TABLE": audit }),
        )
        .await;

        // The records are written BEFORE the mapping exists, so the mapping reads them as the
        // backlog of the window it opens: what the row is about is the envelope they are
        // handed over in, not when they arrived.
        for key in ["window-1", "window-2"] {
            write_item(&router, case, &source, key, Write::Insert).await;
        }
        let uuid = given_windowed_mapping(&router, case, &function, &stream_arn, configured).await;

        let deadline = Instant::now() + BUDGET + BUDGET;
        let windowed = audited(&router, case, &audit, WINDOWED_AUDIT, deadline)
            .await
            .unwrap_or_else(|| {
                panic!("case {case:?}: the records must be handed to the function at all")
            });
        match window_seconds {
            // A mapping with no window hands over an event carrying no window member at all,
            // and so names neither the shard nor the stream beside the records either.
            None => {
                assert_eq!(
                    audited_text(&windowed, "has_window"),
                    "false",
                    "case {case:?}: a mapping with no tumbling window carries no window on \
                     its invocations, got {windowed}"
                );
                assert_eq!(
                    audited_text(&windowed, "shard_id"),
                    String::new(),
                    "case {case:?}: a mapping with no tumbling window names no shard on its \
                     invocations, got {windowed}"
                );
                assert_eq!(
                    audited_text(&windowed, "source_arn"),
                    String::new(),
                    "case {case:?}: a mapping with no tumbling window names no event source \
                     on its invocations, got {windowed}"
                );
                // And no window ever finalises, so there is no final invocation to make: the
                // batch above was delivered before this is read.
                let ended = audited(&router, case, &audit, FINAL_AUDIT, Instant::now()).await;
                assert!(
                    ended.is_none(),
                    "case {case:?}: a mapping with no tumbling window makes no final \
                     invocation, got {ended:?}"
                );
            }
            Some(seconds) => {
                assert_eq!(
                    audited_text(&windowed, "has_window"),
                    "true",
                    "case {case:?}: an invocation inside a window carries that window, got \
                     {windowed}"
                );
                let start = audited_instant(case, &windowed, "window_start");
                let end = audited_instant(case, &windowed, "window_end");
                assert_eq!(
                    end - start,
                    seconds,
                    "case {case:?}: the window must span the {seconds} second(s) the mapping \
                     configures, got {windowed}"
                );
                assert_eq!(
                    start % seconds,
                    0,
                    "case {case:?}: the windows of one duration tile the clock from a whole \
                     multiple of it, got {windowed}"
                );
                assert!(
                    audited_text(&windowed, "shard_id").starts_with("shardId-"),
                    "case {case:?}: an invocation inside a window names the shard its records \
                     came off, got {windowed}"
                );
                assert_eq!(
                    audited_text(&windowed, "source_arn"),
                    stream_arn,
                    "case {case:?}: an invocation inside a window names the stream its \
                     records were read from, got {windowed}"
                );
                assert_eq!(
                    audited_text(&windowed, "early"),
                    "false",
                    "case {case:?}: a window whose state is inside its bound is not \
                     terminated early, got {windowed}"
                );

                // The window ENDS in one final invocation, carrying the state the handler
                // built over the window and no records of its own.
                let ended = audited(&router, case, &audit, FINAL_AUDIT, deadline + BUDGET)
                    .await
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: every window ends in a final invocation carrying \
                             its state"
                        )
                    });
                assert_eq!(
                    audited_count(&ended, "records"),
                    0,
                    "case {case:?}: the final invocation of a window carries no records of \
                     its own, got {ended}"
                );
                assert_eq!(
                    audited_text(&ended, "has_window"),
                    "true",
                    "case {case:?}: the final invocation names the window it finalises, got \
                     {ended}"
                );
                assert_eq!(
                    audited_text(&ended, "source_arn"),
                    stream_arn,
                    "case {case:?}: the final invocation names the stream the window's \
                     records were read from, got {ended}"
                );
                // The state it was handed is the one the window's invocations returned: the
                // handler counted the records itself, so what the finals of the row's windows
                // were handed TOGETHER is the proof the state carried from one invocation of a
                // window to the next — 2 for the two records written, and 0 had no state
                // carried at all. It is the sum over the windows rather than one window's own
                // figure, because which window a record falls into is the clock's to decide.
                let aggregated = await_aggregate(&router, case, &audit, 2, deadline + BUDGET).await;
                assert_eq!(
                    audited_count(&aggregated, "finals_seen"),
                    2,
                    "case {case:?}: the final invocation of each window is handed the state \
                     that window reached, which over the row's windows counts every record \
                     written, got {aggregated}"
                );
                assert!(
                    audited_count(&aggregated, "finals_count") >= 1,
                    "case {case:?}: every window ends in a final invocation, got {aggregated}"
                );
                assert!(
                    audited_text(&aggregated, "state_in").contains("\"seen\""),
                    "case {case:?}: the final invocation is handed the window's own state, got \
                     {aggregated}"
                );
            }
        }

        // The trigger is torn down as a client tears one down, which is also what stops the
        // mapping from reading its source any further.
        stop_mapping(&router, case, &uuid).await;
    }
}
