// SPDX-License-Identifier: CC0-1.0

//! What a mapping on an Amazon Kinesis data stream does with a batch its function keeps
//! erroring on, end to end over the wire with Kinesis, Amazon DynamoDB, Amazon SQS and IAM
//! mounted beside Lambda on ONE engine — so the records are put where a client puts them, the
//! shard is read as the function's own execution role, and every attempt this suite counts was
//! recorded by the HANDLER itself through the AWS SDK its runtime supplies.
//!
//! The RULE asserted over the table below: `MaximumRetryAttempts` is "Discard records after the
//! specified number of retries. The default value is infinite (-1). When set to infinite (-1),
//! failed records are retried until the record expires", so a mapping stating a finite count
//! gets the first attempt plus that many retries at ONE batch, each attempt carrying the SAME
//! records; once they are spent the whole batch is discarded — the records of it the function
//! never processed included — and the shard is read on from past it, so a record written behind
//! it is delivered exactly once and in shard order. Where the mapping's
//! `DestinationConfig$OnFailure` names a destination ("the destination configuration for failed
//! invocations"), exactly one record is written there for the DISCARDED batch, bounding it by
//! the sequence number its first and last record carry and by how many records it held, and a
//! batch the function processed writes nothing there at all.
//!
//! Every attempt is observable because the handler appends one entry per invocation to an item
//! of an audit table before it decides whether to refuse the batch: the entry carries the
//! records it was handed and whether it processed them, so the list is the whole delivery
//! history in order — a mapping that delivers nothing leaves it empty, one that retries a batch
//! with other records than the first attempt had leaves a different entry, and one that stalls
//! on the batch never reaches the record written behind it.
//!
//! The rows differ along what the rule generalises over: how many retries the mapping allows
//! (none, one, two), how many records the discarded batch holds and where in it the record the
//! handler refuses sits (alone, at the head, between healthy siblings).
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes, and each row's own ordering is established by waiting
//! for the effect that precedes it rather than by a clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{dynamodb, given_table, KEY};
use crate::event_source_mapping_failure_destination::{given_queue, sqs};
use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::kinesis_event_source_delivery::{
    get_mapping, given_stream, kinesis, lambda, LOOK_AGAIN, MAPPINGS_URI,
};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the module and attribute its package
/// exposes its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well past
/// what an interpreted run needs, because what is under test is the delivery and not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StartingPosition` that reads the records already sitting in the stream: "Required for
/// Amazon Kinesis and Amazon DynamoDB Stream event sources"
/// (`CreateEventSourceMappingInput$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// The `BatchSize` every mapping below reads with, well above any row's record count, so the
/// records sitting in the shard are read as ONE batch.
const BATCH_SIZE: i64 = 10;

/// The `MaximumRecordAgeInSeconds` every mapping below states — a finite age inside the range
/// the member admits (`-1..604800`), far above the span a row takes, so no record of a row ages
/// out of its batch: what is under test is the retry budget beside it.
const MAXIMUM_RECORD_AGE: i64 = 3_600;

/// The `MaximumBatchingWindowInSeconds` every mapping below states: the stream default, which
/// asks for no wait before a batch is handed over.
const BATCHING_WINDOW: i64 = 0;

/// The key the audit item is held under, and the attribute the handler appends its entries to.
/// Neither is read by this emulator: both are the handler's own, passed to it through its
/// environment.
const AUDIT_KEY: &str = "log";
/// See [`AUDIT_KEY`].
const ENTRIES_ATTRIBUTE: &str = "entries";

/// The marker a record carries to be one the handler refuses. Nothing in the emulator reads it:
/// it is read by the handler, out of the record's own data.
const POISON: &str = "poison";

/// The record every row writes LAST, once the record behind the discarded batch has been
/// processed: its own delivery is what proves the one before it was delivered exactly once,
/// because one shard hands its records over in order.
const SENTINEL: &str = "sentinel-ok";

/// The values the record of a discarded batch carries on the WIRE, written out here rather than
/// imported from the emulator so a change to either side of them fails a row.
///
/// Source: AWS's published on-failure record for a stream event source — the schema version the
/// record carries, the condition it reports for a batch whose retries ran out, and the member a
/// Kinesis batch is described under.
const RECORD_SCHEMA_VERSION: &str = "1.0";
/// See [`RECORD_SCHEMA_VERSION`].
const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";
/// See [`RECORD_SCHEMA_VERSION`].
const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";

/// How long a row waits for work the clock owes — a stream and a table settling, a mapping
/// settling, every attempt at a batch being spent, the batch being discarded and its record
/// reaching the destination.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what it
/// waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);

/// One row: how many retries the mapping allows, and which records the batch it gives up on
/// holds.
struct Row {
    case: &'static str,
    /// A name no other row uses, short enough for `FunctionName` and `RoleName`.
    name: &'static str,
    /// The `MaximumRetryAttempts` the mapping states.
    retries: i64,
    /// The data of each record put on the stream BEFORE the mapping is created, in order: one
    /// of them carries [`POISON`], which is what the handler refuses the batch for.
    batch: &'static [&'static str],
    /// The data of the one record written BEHIND that batch, once its attempts are spent.
    behind: &'static str,
}

impl Row {
    /// How many attempts the batch gets: the first one plus the retries the mapping states.
    fn attempts(&self) -> usize {
        usize::try_from(self.retries).unwrap_or_default() + 1
    }

    /// The whole delivery history the handler must have recorded: every attempt at the batch,
    /// each carrying the SAME records and refused, then the record written behind it, then the
    /// sentinel behind that — and nothing else, so no record of the discarded batch was ever
    /// processed and nothing was delivered twice.
    fn expected_entries(&self) -> Vec<(Vec<String>, bool)> {
        let handed: Vec<String> = self.batch.iter().map(|data| (*data).to_string()).collect();
        let mut expected: Vec<(Vec<String>, bool)> = Vec::new();
        for _ in 0..self.attempts() {
            expected.push((handed.clone(), false));
        }
        expected.push((vec![self.behind.to_string()], true));
        expected.push((vec![SENTINEL.to_string()], true));
        expected
    }
}

/// The rows: each retry count the rule is stated over, and each place the refused record can
/// sit in the batch.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no retry, a batch of one refused record",
            name: "solo",
            retries: 0,
            batch: &["solo-poison"],
            behind: "behind-solo",
        },
        Row {
            case: "one retry, the refused record between healthy siblings",
            name: "mid",
            retries: 1,
            batch: &["head-ok", "mid-poison", "tail-ok"],
            behind: "behind-mid",
        },
        Row {
            case: "two retries, the refused record at the head of the batch",
            name: "head",
            retries: 2,
            batch: &["head-poison", "second-ok"],
            behind: "behind-head",
        },
    ]
}

/// Lambda, Kinesis, DynamoDB, Amazon SQS and IAM over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the records are put where a client puts them, the role
/// Lambda reads the stream and writes the destination as is the one IAM holds, and the handler's
/// own SDK call is dispatched back into this same router — so the audit item this suite reads is
/// the item the handler wrote.
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
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One DynamoDB call that must be served. The credential this harness signs with is the
/// account's own, so these are the audit table OWNER's reads and writes rather than the
/// function's.
async fn dynamodb_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = dynamodb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DynamoDB {operation} answered {status} {answered}"
    );
    answered
}

/// The audit table the handler appends its entries to, seeded with the one item it updates and
/// an empty list in it; answers the ARN DynamoDB persisted for the table.
async fn given_audit_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    given_table(router, case, name, None).await;
    let described = dynamodb_ok(router, case, "DescribeTable", &json!({ "TableName": name })).await;
    let arn = described["Table"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {described}"))
        .to_string();
    dynamodb_ok(
        router,
        case,
        "PutItem",
        &json!({
            "TableName": name,
            "Item": { KEY: { "S": AUDIT_KEY }, ENTRIES_ATTRIBUTE: { "L": [] } },
        }),
    )
    .await;
    arn
}

/// The handler every row runs: for each invocation it reads the data of every record of the
/// batch it was handed, appends ONE entry naming those records and whether it processed them to
/// the audit item through the SDK the runtime supplies, and only then raises for a batch holding
/// the marker — so an attempt is recorded whether or not it ends in an error.
fn handler_source() -> String {
    format!(
        r#"
import base64
import json
import os

import boto3

TABLE = os.environ["AUDIT_TABLE"]
LOG_KEY = os.environ["AUDIT_KEY"]
MARKER = os.environ["POISON_MARKER"]
CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    handed = []
    refused = False
    for record in event["Records"]:
        if record["eventSource"] != "aws:kinesis":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        text = base64.b64decode(record["kinesis"]["data"]).decode()
        handed.append({{"S": text}})
        if MARKER in text:
            refused = True
    entry = {{"M": {{"batch": {{"L": handed}}, "ok": {{"BOOL": not refused}}}}}}
    CLIENT.update_item(
        TableName=TABLE,
        Key={{"{KEY}": {{"S": LOG_KEY}}}},
        UpdateExpression="SET {ENTRIES_ATTRIBUTE} = list_append({ENTRIES_ATTRIBUTE}, :entry)",
        ExpressionAttributeValues={{":entry": {{"L": [entry]}}}},
    )
    if refused:
        raise RuntimeError("the handler cannot process " + json.dumps(handed))
    return {{"processed": len(handed)}}
"#
    )
}

/// A function whose package RUNS [`handler_source`], running as `role_arn` and told through its
/// environment which item to append its entries to, waited out to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    table: &str,
) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": {
                "Variables": {
                    "AUDIT_TABLE": table,
                    "AUDIT_KEY": AUDIT_KEY,
                    "POISON_MARKER": POISON,
                },
            },
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

/// Put one record per entry of `data` on the stream `stream` names, as ONE `PutRecords` call,
/// and answer the sequence number the stream reported for each of them in the order they were
/// written — which is the window the record of a discarded batch has to bound.
async fn put_records(
    router: &MultiServiceRouter,
    case: &str,
    stream: &str,
    data: &[&str],
) -> Vec<String> {
    let records: Vec<Value> = data
        .iter()
        .map(|text| {
            json!({
                "Data": base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
                "PartitionKey": *text,
            })
        })
        .collect();
    let (status, written) = kinesis(
        router,
        "PutRecords",
        &json!({ "StreamName": stream, "Records": records }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRecords answered {status} {written}"
    );
    assert_eq!(
        written["FailedRecordCount"],
        json!(0),
        "case {case:?}: every record must be placed, got {written}"
    );
    let placed = written["Records"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        placed.len(),
        data.len(),
        "case {case:?}: expected one answer per record, got {written}"
    );
    placed
        .iter()
        .map(|record| {
            record["SequenceNumber"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no SequenceNumber in {record}"))
                .to_string()
        })
        .collect()
}

/// The mapping of `function` onto `stream_arn`, with the row's retry budget, the record age
/// beside it and the queue `destination` names as where a discarded batch is recorded; answers
/// its UUID and the `FunctionArn` it reports.
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
            "BatchSize": BATCH_SIZE,
            "MaximumBatchingWindowInSeconds": BATCHING_WINDOW,
            "BisectBatchOnFunctionError": false,
            "MaximumRetryAttempts": retries,
            "MaximumRecordAgeInSeconds": MAXIMUM_RECORD_AGE,
            "DestinationConfig": { "OnFailure": { "Destination": destination } },
            "Enabled": true,
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
    // The bounds the mapping was created with are the bounds it reports: a mapping that read
    // them back differently would be bounded by something other than what the row asked for.
    let read = get_mapping(router, case, &uuid).await;
    for (member, expected) in [
        ("MaximumRetryAttempts", json!(retries)),
        ("MaximumRecordAgeInSeconds", json!(MAXIMUM_RECORD_AGE)),
        ("BisectBatchOnFunctionError", json!(false)),
        ("BatchSize", json!(BATCH_SIZE)),
    ] {
        assert_eq!(
            read[member], expected,
            "case {case:?}: expected {member} {expected}, got {read}"
        );
    }
    assert_eq!(
        read["DestinationConfig"]["OnFailure"]["Destination"],
        json!(destination),
        "case {case:?}: expected the mapping to report its failure destination, got {read}"
    );
    (uuid, function_arn)
}

/// The entries the handler has appended to the audit item so far, in order: the records one
/// invocation was handed, and whether that invocation processed them.
async fn recorded_attempts(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
) -> Vec<(Vec<String>, bool)> {
    let read = dynamodb_ok(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": table,
            "Key": { KEY: { "S": AUDIT_KEY } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read["Item"][ENTRIES_ATTRIBUTE]["L"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            let handed: Vec<String> = entry["M"]["batch"]["L"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|value| value["S"].as_str().map(str::to_string))
                .collect();
            let processed = entry["M"]["ok"]["BOOL"].as_bool().unwrap_or(false);
            (handed, processed)
        })
        .collect()
}

/// Wait until the handler has recorded at least `wanted` attempts, answering everything it has
/// recorded — more than `wanted` when more arrived, which is what makes "exactly this many"
/// assertable.
async fn awaited_attempts(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    wanted: usize,
    waiting_for: &str,
) -> Vec<(Vec<String>, bool)> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let recorded = recorded_attempts(router, case, table).await;
        if recorded.len() >= wanted {
            return recorded;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {waiting_for} ({wanted} recorded attempts), got \
             {recorded:?}"
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

/// Wait until a message has arrived on the queue `url` addresses, answering everything that was
/// on it.
async fn awaited_messages(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let messages = drained(router, case, url).await;
        if !messages.is_empty() {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the record of the discarded batch on the destination, got \
             nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a mapping on a Kinesis data stream spends the attempts its `MaximumRetryAttempts`
/// allows on ONE batch, each attempt carrying the same records; then it discards that batch
/// whole, reads the shard on from past it — so the record written behind it is delivered exactly
/// once — and records the DISCARDED batch, and only it, at the destination its
/// `DestinationConfig$OnFailure` names, bounded by the sequence numbers its first and last
/// record carry.
#[tokio::test]
async fn a_poison_batch_is_attempted_its_budget_of_times_then_discarded() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        let stream = format!("retry-stream-{}", row.name);
        let table = format!("retry-audit-{}", row.name);
        let function = format!("retry-fn-{}", row.name);

        let stream_arn = given_stream(&router, case, &stream, 1).await;
        let table_arn = given_audit_table(&router, case, &table).await;
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("retry-sink-{}", row.name)).await;
        // Lambda reads the stream and writes the failure destination acting as the function's
        // execution role, and the handler appends its entries as itself: the role carries each
        // of those grants and nothing wider, as a client that wires this pipeline gives it.
        let role = given_role(
            &router,
            case,
            &format!("retry-role-{}", row.name),
            Some(json!([
                logs_only(),
                allow("kinesis", &KINESIS_READS, &stream_arn),
                allow("dynamodb", &["UpdateItem"], &table_arn),
                allow("sqs", &["SendMessage"], &queue_arn),
            ])),
        )
        .await;
        given_function(&router, case, &function, &role, &table).await;

        // The batch goes in BEFORE the mapping exists, so the mapping's first read from
        // TRIM_HORIZON takes exactly these records as one batch.
        let placed = put_records(&router, case, &stream, row.batch).await;
        let (uuid, function_arn) =
            given_mapping(&router, case, &function, &stream_arn, row.retries, &queue_arn).await;

        // Every attempt the budget allows, and no more: each one was handed the same records
        // and refused them.
        let spent = awaited_attempts(
            &router,
            case,
            &table,
            row.attempts(),
            "every attempt the mapping's retry budget allows",
        )
        .await;
        let handed: Vec<String> = row.batch.iter().map(|data| (*data).to_string()).collect();
        for (index, (records, processed)) in spent.iter().take(row.attempts()).enumerate() {
            assert_eq!(
                (records.clone(), *processed),
                (handed.clone(), false),
                "case {case:?}: attempt {} must be the same batch, refused, got \
                 {records:?}/{processed}",
                index + 1
            );
        }

        // The batch was DISCARDED, which is what the destination's one record says — and it
        // bounds that batch: the window its own first and last record sit at, and the count it
        // held.
        let messages = awaited_messages(&router, case, &queue_url).await;
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: exactly one record must be written per discarded batch, got \
             {messages:?}"
        );
        let body = messages[0]["Body"].as_str().unwrap_or_default().to_string();
        let record: Value = serde_json::from_str(&body)
            .unwrap_or_else(|_| panic!("case {case:?}: the record must be a document: {body}"));
        assert_eq!(
            record["version"],
            json!(RECORD_SCHEMA_VERSION),
            "case {case:?}: the record must carry its schema version, got {record}"
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
        let info = &record[KINESIS_BATCH_INFO];
        assert_eq!(
            info["streamArn"],
            json!(stream_arn),
            "case {case:?}: the batch must name the stream it was read from, got {record}"
        );
        assert_eq!(
            info["batchSize"],
            json!(row.batch.len()),
            "case {case:?}: the batch must report how many records it held, got {record}"
        );
        assert_eq!(
            info["startSequenceNumber"],
            json!(placed.first().cloned().unwrap_or_default()),
            "case {case:?}: the window must start at the sequence number the stream gave the \
             batch's first record, got {record}"
        );
        assert_eq!(
            info["endSequenceNumber"],
            json!(placed.last().cloned().unwrap_or_default()),
            "case {case:?}: the window must end at the sequence number the stream gave the \
             batch's last record, got {record}"
        );
        for data in row.batch {
            assert!(
                !body.contains(data),
                "case {case:?}: the record must describe the batch and carry no record \
                 payload, but it holds {data:?}: {body}"
            );
        }

        // The shard has moved past the discarded batch: the record written behind it now is
        // delivered and processed, and so is the sentinel written once it has been — which is
        // what makes the one delivery of the record before it the only one.
        put_records(&router, case, &stream, &[row.behind]).await;
        awaited_attempts(
            &router,
            case,
            &table,
            row.attempts() + 1,
            "the record written behind the discarded batch to be delivered",
        )
        .await;
        put_records(&router, case, &stream, &[SENTINEL]).await;
        let history = awaited_attempts(
            &router,
            case,
            &table,
            row.attempts() + 2,
            "the sentinel written behind that record to be delivered",
        )
        .await;
        assert_eq!(
            history,
            row.expected_entries(),
            "case {case:?}: expected exactly the attempts at the discarded batch and one \
             delivery of each record behind it, got {history:?}"
        );

        // And nothing more was recorded at the destination: the record is written per
        // discarded batch, and a batch the function processed is no batch to give up on.
        let again = drained(&router, case, &queue_url).await;
        assert!(
            again.is_empty(),
            "case {case:?}: a processed batch must write nothing to the failure destination, \
             got {again:?}"
        );
        // The mapping is still the one the row created, reading the same stream.
        let read = get_mapping(&router, case, &uuid).await;
        assert_eq!(
            read["EventSourceArn"],
            json!(stream_arn),
            "case {case:?}: the mapping must still name the stream it was created for, got \
             {read}"
        );
    }
}
