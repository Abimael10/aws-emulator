// SPDX-License-Identifier: CC0-1.0

//! The WHOLE enhanced-fan-out pipeline, end to end over the wire with no client action after
//! the records are put: a record put to an Amazon Kinesis data stream reaches the function an
//! event source mapping on a registered STREAM CONSUMER of that stream maps, and the AWS call
//! that function's own handler makes is served by this emulator — so the effect a client polls
//! for is readable through the called service's own API.
//!
//! `EventSourceArn` is "Amazon Kinesis – The ARN of the data stream or a stream consumer"
//! (`smithy_query(query="shape", shape="EventSourceMappingConfiguration",
//! member="EventSourceArn")`), and `RegisterStreamConsumer` "registers a consumer with a
//! Kinesis data stream… so that this consumer can read data" — the enhanced fan-out form of
//! reading a stream, whose records a consumer is pushed over a `SubscribeToShard`
//! subscription. So the RULE asserted over the table below, in one sentence: an `Enabled`
//! mapping on EITHER resource that member admits reads its stream's shards from the position
//! its `StartingPosition` fixes and hands the mapped function the `aws:kinesis` envelope of
//! each record ONCE, so what the mapping names decides which reader Lambda consumes the shard
//! with and nothing about which records the function is handed, how many times, or what the
//! envelope carries.
//!
//! What a sibling suite watches through the handler's LOG lines
//! ([`crate::kinesis_fan_out_consumer_delivery`]), this one watches through the handler's own
//! AWS call: each delivery writes ONE Amazon DynamoDB item, keyed by the record's payload
//! together with the invocation's own request id, so a record delivered twice leaves two items
//! and the row fails on the count — a delivery path that double-counts is as wrong as one that
//! delivers nothing. Nothing here invokes the function: every item read back was written by a
//! handler the poller handed a batch to.
//!
//! The rows differ along what the rule generalises over: which of the two resources the event
//! source names (a registered consumer, the data stream itself), which `StartingPosition` the
//! mapping reads from, how many shards the stream has, how many records are put and how many of
//! them one batch may hold.
//!
//! Every test drives real HTTP through the production `MultiServiceRouter` with Lambda, Amazon
//! Kinesis, Amazon DynamoDB and IAM on ONE engine, as `core/router` assembles them: the stream
//! and its consumer are created through Kinesis's own surface, the role Lambda reads the stream
//! as is the one IAM holds, the function carries a REAL `python3.x` deployment package, and the
//! handler's own SDK call is dispatched back into this same router.
//!
//! Nothing here waits a span of its own choosing: each `PutRecords` opens one deadline
//! ([`ROW_BUDGET`]) for the chain it starts, and every look-again after it shares that one
//! deadline and waits for THE STATE THE ROW ASSERTS, so no assertion depends on how fast the
//! host runs.
//!
//! Every test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    STARTING_POSITION_LATEST, STARTING_POSITION_TRIM_HORIZON,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::event_source_mappings::{get_mapping, given_enabled_mapping, router_on, LOOK_AGAIN};
use crate::invocation_log_delivery::{create_function, Created, PYTHON_RUNTIME};
use crate::kinesis_event_source_delivery::{given_stream, put_records};
use crate::kinesis_event_source_handler_writes_dynamodb::{dynamodb, given_table, KEY_ATTRIBUTE};
use crate::kinesis_fan_out_consumer_delivery::given_active_consumer;

/// How long the chain ONE `PutRecords` starts is given for every state the clock owes it
/// together — the mapping's poller taking its batches, the invocations running and their own
/// writes landing in the sink table.
///
/// PATIENCE, not a weakened expectation, and taken from what the EMULATOR itself allows an
/// invocation rather than picked here: the wait for a share of the run capacity
/// ([`PERMIT_WAIT`]) and the wait for the run's own answer ([`HOST_WAIT_CEILING`]), for each of
/// the two batches a row can be delivered in, with room on top because a whole-workspace run
/// shares its cores with every other suite.
const ROW_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));

/// The `eventSource` a record of a Kinesis data stream names itself with, which the handler
/// refuses to process anything else under.
const EVENT_SOURCE: &str = "aws:kinesis";

/// The attribute carrying the record's own payload on a sink item, which is what makes the
/// item evidence of THAT record having been delivered.
const RECORD_ATTRIBUTE: &str = "record";

/// The attribute carrying the `eventSourceARN` the envelope named.
const SOURCE_ATTRIBUTE: &str = "source";

/// The attributes carrying the record's own position in its shard, as the envelope spells
/// them.
const PARTITION_KEY_ATTRIBUTE: &str = "partitionKey";
/// See [`PARTITION_KEY_ATTRIBUTE`].
const SEQUENCE_ATTRIBUTE: &str = "sequenceNumber";

/// The reads of a stream CONSUMER a role that polls one through enhanced fan-out makes, on
/// the consumer's own resource. Source: the actions AWS's own managed execution-role policy
/// for this event source (`AWSLambdaKinesisExecutionRole`) grants on a consumer beside the
/// stream's own reads ([`KINESIS_READS`]).
const CONSUMER_READS: [&str; 2] = ["SubscribeToShard", "DescribeStreamConsumer"];

/// Lambda, Amazon Kinesis, Amazon DynamoDB and IAM over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the records are put where a client puts them, the
/// role Lambda reads the stream as is the one IAM holds, and the handler's own SDK call is
/// dispatched back into this same router — so the table this suite reads is the table the
/// handler wrote.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// The handler every row runs: for each record of the batch it was handed it decodes that
/// record's own data and writes ONE item to the sink table through the SDK the runtime
/// supplies, keyed by that payload together with the invocation's own request id.
///
/// Every value it writes comes from the envelope it was handed, so an item in the table is
/// evidence that the record reached the handler in the shape Lambda hands a Kinesis batch over
/// in — and one item per (record, invocation) pair is what makes a second delivery of the same
/// record a second item rather than an overwrite.
fn handler_source(table: &str) -> String {
    format!(
        r##"
import base64

import boto3

TABLE = "{table}"
EVENT_SOURCE = "{EVENT_SOURCE}"
CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    written = []
    for record in records:
        if record["eventSource"] != EVENT_SOURCE:
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        stream_record = record["kinesis"]
        payload = base64.b64decode(stream_record["data"]).decode("utf-8")
        CLIENT.put_item(
            TableName=TABLE,
            Item={{
                "{KEY_ATTRIBUTE}": {{"S": payload + "#" + context.aws_request_id}},
                "{RECORD_ATTRIBUTE}": {{"S": payload}},
                "{SOURCE_ATTRIBUTE}": {{"S": record["eventSourceARN"]}},
                "{PARTITION_KEY_ATTRIBUTE}": {{"S": stream_record["partitionKey"]}},
                "{SEQUENCE_ATTRIBUTE}": {{"S": stream_record["sequenceNumber"]}},
            }},
        )
        written.append(payload)
    return {{"written": written}}
"##
    )
}

/// One row: which resource the mapping's event source names, where it reads from, how many
/// shards the stream has, how many records are put and how many of them one batch may hold.
struct Row {
    case: &'static str,
    /// Whether the mapping's `EventSourceArn` names a registered CONSUMER of the stream
    /// rather than the data stream itself.
    through_consumer: bool,
    /// The `StartingPosition` the mapping is created with.
    starting_position: &'static str,
    /// How many shards the stream is created with.
    shards: i64,
    /// How many records are put to the stream, once the mapping reports `Enabled`.
    records: usize,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
}

/// The payload the `index`th record [`put_records`] puts for `case` carries, which is the
/// value the handler writes as the item's [`RECORD_ATTRIBUTE`].
fn payload(case: &str, index: usize) -> String {
    format!("{case}-{index}")
}

/// Every payload the row's records must be delivered as, one per record. Each record is put
/// AFTER the mapping reports `Enabled`, so both positions a row reads from are owed every one
/// of them.
fn expected_payloads(row: &Row) -> Vec<String> {
    let mut payloads: Vec<String> = (0..row.records)
        .map(|index| payload(row.case, index))
        .collect();
    payloads.sort();
    payloads
}

/// Every item the sink table holds, read through DynamoDB's own `Scan` with `ConsistentRead`
/// as a client that polls for the effect reads it.
async fn sink_items(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<Value> {
    let scanned = dynamodb(
        router,
        case,
        "Scan",
        &json!({ "TableName": table, "ConsistentRead": true }),
    )
    .await;
    scanned["Items"].as_array().cloned().unwrap_or_default()
}

/// The payload of every item the sink table holds, sorted — one entry per DELIVERY, because
/// each item is keyed by its record's payload together with the invocation that wrote it.
fn delivered_payloads(items: &[Value]) -> Vec<String> {
    let mut payloads: Vec<String> = items
        .iter()
        .filter_map(|item| item[RECORD_ATTRIBUTE]["S"].as_str().map(str::to_string))
        .collect();
    payloads.sort();
    payloads
}

/// Watch the sink table until it holds an item for every payload of `wanted`, answering what
/// was last read.
///
/// The whole chain is waited for TOGETHER — the poller taking the batch, the invocation running
/// and the handler's own write landing — because a reader that stopped at the first of them
/// would be reading an instant of its own choosing in the middle of it.
async fn sink_until(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    wanted: &[String],
    deadline: Instant,
) -> Vec<Value> {
    loop {
        let items = sink_items(router, case, table).await;
        let delivered = delivered_payloads(&items);
        let settled = wanted.iter().all(|payload| delivered.contains(payload));
        if settled || Instant::now() >= deadline {
            return items;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Stand one row up — a stream, the resource its mapping names, a sink table, a role, a
/// function and the mapping — put the row's records, and assert the handler wrote exactly one
/// item for each of them, carrying the envelope it was handed.
///
/// `label` keeps one row's stream, table, role and function names apart from another's; every
/// row runs on an engine of its own.
async fn assert_every_record_is_written_once(label: &str, row: &Row) {
    let case = row.case;
    let router = emulator();
    let stream = format!("fanout-sink-{label}");
    let table = format!("fanout-sink-{label}");
    let function = format!("fanout-sink-{label}");

    let stream_arn = given_stream(&router, case, &stream, row.shards).await;
    let table_arn = given_table(&router, case, &table).await;

    // What the mapping names: a registered consumer of the stream, or the data stream itself.
    let event_source = if row.through_consumer {
        let consumer = given_active_consumer(&router, case, &stream_arn, label).await;
        assert!(
            consumer.starts_with(&stream_arn) && consumer.contains("/consumer/"),
            "case {case:?}: a consumer's ARN extends its stream's, got {consumer:?}"
        );
        consumer
    } else {
        stream_arn.clone()
    };

    // Lambda reads the source acting as the function's execution role, and the handler writes
    // the table as itself: the role carries the stream's reads, the consumer's own reads and
    // the write, as a client that wires this pipeline together gives it.
    let role = given_role(
        &router,
        case,
        &format!("fanout-sink-{label}"),
        Some(json!([
            logs_only(),
            allow("kinesis", &KINESIS_READS, &stream_arn),
            allow("kinesis", &CONSUMER_READS, &event_source),
            allow("dynamodb", &["PutItem"], &table_arn),
        ])),
    )
    .await;
    let source = handler_source(&table);
    create_function(
        &router,
        &Created {
            case,
            name: &function,
            source: &source,
            runtime: PYTHON_RUNTIME,
            role: &role,
            log_group: None,
        },
    )
    .await;

    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": event_source,
            "StartingPosition": row.starting_position,
            "BatchSize": row.batch_size,
            "Enabled": true,
        }),
    )
    .await;

    // The ONE client action: the records are put where a client puts them. Everything after it
    // is the mapping's own work, and the row's patience is for THAT.
    put_records(&router, case, &stream, row.records).await;
    let deadline = Instant::now() + ROW_BUDGET;
    let wanted = expected_payloads(row);
    let items = sink_until(&router, case, &table, &wanted, deadline).await;
    let delivered = delivered_payloads(&items);
    // EXACTLY the records that were put, each exactly once: a record the chain never delivered
    // is missing from this list and a record it delivered twice is in it twice, because one
    // item is one delivery. What the mapping says about its own last attempt is read only to
    // SAY why, so a row that fails names the cause.
    assert_eq!(
        delivered,
        wanted,
        "case {case:?}: expected the handler to write one item per record {wanted:?}, got \
         {delivered:?}; the mapping reports {}",
        get_mapping(&router, &uuid).await.text()
    );

    for item in &items {
        let written = item[RECORD_ATTRIBUTE]["S"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // The envelope's own members, as the handler read them out of the record it was handed:
        // the event source the mapping polls — whichever of the two resources it names — and
        // the record's own place in its shard. `put_records` puts each record under its own
        // payload as its partition key, so that is what the envelope has to carry.
        assert_eq!(
            item[SOURCE_ATTRIBUTE],
            json!({ "S": event_source }),
            "case {case:?}: expected the envelope of {written:?} to name {event_source:?} as its \
             eventSourceARN, got {item}"
        );
        assert_eq!(
            item[PARTITION_KEY_ATTRIBUTE],
            json!({ "S": written }),
            "case {case:?}: expected the record's own partition key {written:?}, got {item}"
        );
        assert!(
            item[SEQUENCE_ATTRIBUTE]["S"]
                .as_str()
                .is_some_and(|sequence| !sequence.is_empty()),
            "case {case:?}: expected the envelope of {written:?} to carry a sequence number, got \
             {item}"
        );
    }
}

/// RULE: a mapping whose `EventSourceArn` names a registered stream CONSUMER is delivered
/// every record put to that consumer's stream — through the consumer's own enhanced fan-out
/// subscription — and the AWS call the invocation's handler makes for each of them is served
/// by this emulator, so the sink holds one item per record.
#[tokio::test]
async fn a_consumer_sourced_mapping_drives_its_handlers_own_write_for_every_record() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_is_written_once(
        "horizon",
        &Row {
            case: "a consumer read from the horizon, three records in one batch",
            through_consumer: true,
            starting_position: STARTING_POSITION_TRIM_HORIZON,
            shards: 1,
            records: 3,
            batch_size: 10,
        },
    )
    .await;
}

/// The same RULE over the dimensions a consumer-sourced mapping varies along: records spread
/// over several shards, read from the position a mapping created `LATEST` starts at, and one
/// record per batch — so each record is a batch, and an invocation, of its own, and a record
/// written twice would be written by two different invocations.
#[tokio::test]
async fn a_consumer_sourced_mapping_writes_a_record_per_batch_over_several_shards() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_is_written_once(
        "latest",
        &Row {
            case: "a consumer read from the latest position, two records one per batch",
            through_consumer: true,
            starting_position: STARTING_POSITION_LATEST,
            shards: 2,
            records: 2,
            batch_size: 1,
        },
    )
    .await;
}

/// The OTHER resource `EventSourceArn` admits, as the row the rule generalises from: a mapping
/// on the data stream itself drives the same write for the same records, which is what a
/// consumer-sourced mapping is asserted to match.
#[tokio::test]
async fn a_stream_sourced_mapping_drives_its_handlers_own_write_for_every_record() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_is_written_once(
        "stream",
        &Row {
            case: "the data stream itself, read from the horizon, two records in one batch",
            through_consumer: false,
            starting_position: STARTING_POSITION_TRIM_HORIZON,
            shards: 1,
            records: 2,
            batch_size: 10,
        },
    )
    .await;
}
