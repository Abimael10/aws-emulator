// SPDX-License-Identifier: CC0-1.0

//! The stream-to-stream pipeline, end to end over the wire with no client action after the
//! record is written: a record put to an Amazon Kinesis data stream reaches the function an
//! event source mapping maps, the handler's own AWS SDK call writes it onto a SECOND Kinesis
//! stream, and a client reads it back off that stream's shard.
//!
//! Two rules are asserted here, each over a table of its own.
//!
//! 1. THE ENVELOPE. Once a mapping on a Kinesis data stream is `Enabled`, Lambda reads that
//!    stream's shards and invokes the mapped function with one envelope per record ("Lambda
//!    reads items from the event source and invokes the function"), each carrying — under
//!    `kinesis` — that record's own `data` as the base64 of the exact bytes that were put, the
//!    `partitionKey` it was put under, its `sequenceNumber`, its numeric
//!    `approximateArrivalTimestamp` and the `kinesisSchemaVersion`, and at record level the
//!    `eventID` `"<ShardId>:<SequenceNumber>"` of that write, `eventName`
//!    `aws:kinesis:record`, `eventSource` `aws:kinesis`, `eventVersion` `1.0`, the stream's
//!    own ARN as `eventSourceARN`, the stream's Region as `awsRegion` and the function's
//!    execution role as `invokeIdentityArn`.
//! 2. THE CRITERIA. "An object that defines the filter criteria that determine whether Lambda
//!    should process an event" (`FilterCriteria`): the criteria are applied to each record the
//!    mapping read, so only the records they admit are part of an invocation at all, and the
//!    records they reject are read past rather than handed over again.
//!
//! Nothing here invokes the function: the only data-plane calls either test makes are the
//! writes onto the SOURCE stream and the reads off the SINK stream. Every record read off the
//! sink was put there by the handler, out of the envelope the poller handed it, so a mapping
//! that reports healthy and delivers nothing leaves the sink empty and the row fails — and a
//! handler whose own `boto3.client("kinesis")` call cannot be made writes nothing either.
//!
//! The first table's rows differ along what the rule generalises over: what the record's data
//! is — bytes that are no text at all, a JSON document, plain text and no data — and the
//! partition key it was put under. The second table's rows differ along the payload the
//! criteria judge: one the pattern names, one whose value disagrees, one carrying the named
//! field deeper down, one that spells no document at all, and a second one the pattern names,
//! put LAST so that its arrival proves every record before it was read and decided.
//!
//! The Lambda, Kinesis and mapping helpers are the ones `kinesis_event_source_delivery.rs`
//! publishes, and the role fixture is `event_source_mapping_role_access.rs`'s, so every suite
//! over this surface drives the same wire.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::STARTING_POSITION_TRIM_HORIZON;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::kinesis_event_source_delivery::{
    get_mapping, given_stream, kinesis, lambda, LOOK_AGAIN, MAPPINGS_URI,
};
use crate::synchronous_invoke_run::package;

/// The Region every call below signs for, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the module and attribute its package
/// exposes its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well
/// past what a run needs, because what is under test is the delivery and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long a wait for work nothing asked for — a mapping settling, a batch read, the
/// handler's own write landing — is given before the row is a failure. Well above the poller's
/// interval and the state-settle span, and above one interpreted run of the handler, so a row
/// fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);

/// The one `State` a mapping that is reading rests in
/// (`smithy_query(query="shape", shape="EventSourceMappingConfiguration", member="State")`).
const STATE_ENABLED: &str = "Enabled";

/// The `eventSource` every record of a Kinesis batch carries, the `eventName` each one
/// carries, the schema version of its `kinesis` member and the version of the envelope
/// itself, as the Kinesis event Lambda hands a function spells them.
const EVENT_SOURCE: &str = "aws:kinesis";
/// See [`EVENT_SOURCE`].
const EVENT_NAME: &str = "aws:kinesis:record";
/// See [`EVENT_SOURCE`].
const SCHEMA_VERSION: &str = "1.0";
/// See [`EVENT_SOURCE`].
const EVENT_VERSION: &str = "1.0";

/// Lambda, Kinesis and IAM over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the records are put where a client puts them, the role Lambda reads the source stream as
/// is the one IAM holds, and the handler's own SDK call is dispatched back into this same
/// router — so the stream this suite reads is the stream the handler wrote.
///
/// `pub(crate)` because every suite whose mapped handler makes an SDK call of its own needs
/// exactly this trio (`tests/all/kinesis_event_source_batching_window.rs`).
pub(crate) fn emulator() -> MultiServiceRouter {
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// The handler every function below runs: for each record of the batch it was handed it puts
/// ONE record carrying that record's WHOLE envelope onto the sink stream, through the AWS SDK
/// the runtime supplies.
///
/// It computes nothing and asserts nothing: a record read off the sink is the envelope the
/// poller handed the handler, so every value this suite checks is the emulator's own.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3

SINK = os.environ["SINK_STREAM"]
CLIENT = boto3.client("kinesis")


def lambda_handler(event, context):
    forwarded = []
    for record in event["Records"]:
        CLIENT.put_record(
            StreamName=SINK,
            Data=json.dumps(record).encode("utf-8"),
            PartitionKey=record["kinesis"]["partitionKey"],
        )
        forwarded.append(record["eventID"])
    return {"forwarded": forwarded}
"#;

/// A function whose package RUNS [`HANDLER_SOURCE`], running as `role_arn` and told through
/// its environment which stream to write, waited out to `Active`.
async fn given_handler_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    sink: &str,
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
            "Environment": { "Variables": { "SINK_STREAM": sink } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
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

/// Where one record landed, as `PutRecord` reports it: "The shard ID of the shard where the
/// data record was placed" and "The sequence number identifier that was assigned to the put
/// data record".
struct Placed {
    shard_id: String,
    sequence_number: String,
}

/// Put ONE record carrying `data` under `partition_key`, answering where the stream placed it.
async fn put_record(
    router: &MultiServiceRouter,
    case: &str,
    stream: &str,
    data: &[u8],
    partition_key: &str,
) -> Placed {
    let (status, written) = kinesis(
        router,
        "PutRecord",
        &json!({
            "StreamName": stream,
            "Data": base64::engine::general_purpose::STANDARD.encode(data),
            "PartitionKey": partition_key,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRecord answered {status} {written}"
    );
    Placed {
        shard_id: written["ShardId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ShardId in {written}"))
            .to_string(),
        sequence_number: written["SequenceNumber"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SequenceNumber in {written}"))
            .to_string(),
    }
}

/// A reader of the sink stream: one iterator per shard, read from the oldest record the shard
/// holds, advanced by every drain so a record is read exactly once.
///
/// `pub(crate)` because every suite that reads back what a mapped handler WROTE reads it this
/// one way (`tests/all/kinesis_event_source_batching_window.rs`).
pub(crate) struct SinkReader {
    iterators: Vec<String>,
}

impl SinkReader {
    /// A reader positioned at the oldest record of every shard of `stream`.
    pub(crate) async fn of(router: &MultiServiceRouter, case: &str, stream: &str) -> Self {
        let (status, listed) = kinesis(router, "ListShards", &json!({ "StreamName": stream })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListShards answered {status} {listed}"
        );
        let shards: Vec<String> = listed["Shards"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|shard| shard["ShardId"].as_str().map(str::to_string))
            .collect();
        assert!(
            !shards.is_empty(),
            "case {case:?}: the sink stream must report its shards, got {listed}"
        );
        let mut iterators: Vec<String> = Vec::with_capacity(shards.len());
        for shard in shards {
            let (status, answered) = kinesis(
                router,
                "GetShardIterator",
                &json!({
                    "StreamName": stream,
                    "ShardId": shard,
                    "ShardIteratorType": "TRIM_HORIZON",
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetShardIterator answered {status} {answered}"
            );
            iterators.push(
                answered["ShardIterator"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no ShardIterator in {answered}"))
                    .to_string(),
            );
        }
        Self { iterators }
    }

    /// Every record the sink has handed over since the last drain, as the document the
    /// handler wrote: one forwarded record envelope per record.
    pub(crate) async fn drain(&mut self, router: &MultiServiceRouter, case: &str) -> Vec<Value> {
        let mut forwarded: Vec<Value> = Vec::new();
        for iterator in &mut self.iterators {
            let (status, answered) = kinesis(
                router,
                "GetRecords",
                &json!({ "ShardIterator": iterator.clone() }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetRecords answered {status} {answered}"
            );
            if let Some(next) = answered["NextShardIterator"].as_str() {
                *iterator = next.to_string();
            }
            for record in answered["Records"].as_array().cloned().unwrap_or_default() {
                let encoded = record["Data"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: a sink record carries no Data: {record}")
                });
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: a sink record's Data is not base64: {error}")
                    });
                let text = String::from_utf8(bytes).unwrap_or_else(|error| {
                    panic!("case {case:?}: a sink record's Data is not text: {error}")
                });
                forwarded.push(serde_json::from_str(&text).unwrap_or_else(|error| {
                    panic!("case {case:?}: a sink record does not spell a document: {error}")
                }));
            }
        }
        forwarded
    }
}

/// Create an `Enabled` mapping from `stream_arn` to `function`, reading from the oldest record
/// the stream holds, waited out to the one `State` a reading mapping rests in.
async fn given_enabled_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    filter_patterns: &[&str],
) -> String {
    let mut body = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": STARTING_POSITION_TRIM_HORIZON,
        "BatchSize": 10,
        "Enabled": true,
    });
    if !filter_patterns.is_empty() {
        body["FilterCriteria"] = json!({
            "Filters": filter_patterns
                .iter()
                .map(|pattern| json!({ "Pattern": pattern }))
                .collect::<Vec<Value>>(),
        });
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, &body).await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_mapping(router, case, &uuid).await;
        if read["State"] == json!(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The bytes the envelope `forwarded` carries under `kinesis.data`, which travel as the base64
/// of the record's own data.
fn carried_bytes(case: &str, forwarded: &Value) -> Vec<u8> {
    let encoded = forwarded["kinesis"]["data"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the envelope carries no data: {forwarded}"));
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the envelope's data is not base64: {error}")
        })
}

/// Wait until the sink holds one forwarded envelope per entry of `expected`, answering them in
/// the order they arrived; the deadline is this suite's own [`BUDGET`].
async fn wait_for_forwarded(
    router: &MultiServiceRouter,
    case: &str,
    reader: &mut SinkReader,
    expected: usize,
) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        held.extend(reader.drain(router, case).await);
        if held.len() >= expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the mapping to invoke its function, whose handler forwards \
             {expected} record(s) onto the sink stream, got {}: {held:?}",
            held.len()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the envelope table: what was put, and under which partition key.
struct Written {
    case: &'static str,
    data: &'static [u8],
    partition_key: &'static str,
}

/// RULE: an `Enabled` mapping on a Kinesis data stream invokes its function with the
/// `aws:kinesis` envelope of each record it read, and the SDK call that invocation's handler
/// makes is served by this emulator — so the record the handler forwarded is on the stream a
/// client reads, carrying the envelope it was handed.
#[tokio::test]
async fn a_kinesis_mapping_hands_its_function_the_envelope_of_each_record_put() {
    let case = "the envelope of a record forwarded onto a second stream";
    // The rows differ along what the record's data is and the key it was put under. Every
    // record goes onto ONE stream of ONE mapping, because the rule is about a record and not
    // about a mapping.
    let rows: &[Written] = &[
        Written {
            case: "bytes that are no text at all",
            data: b"\x00\xff\x80payload",
            partition_key: "pk-bytes",
        },
        Written {
            case: "a JSON document",
            data: br#"{"order":"abc"}"#,
            partition_key: "pk-document",
        },
        Written {
            case: "plain text",
            data: b"hello",
            partition_key: "pk-text",
        },
        Written {
            case: "no data at all",
            data: b"",
            partition_key: "pk-empty",
        },
    ];

    let router = emulator();
    let source_arn = given_stream(&router, case, "envelope-source", 1).await;
    let sink_arn = given_stream(&router, case, "envelope-sink", 1).await;
    // Lambda reads the source acting as the function's execution role, and the handler writes
    // the sink as itself: the role carries both grants, as a client wiring this pipeline gives
    // it.
    let role = given_role(
        &router,
        case,
        "envelope-role",
        Some(json!([
            logs_only(),
            allow("kinesis", &KINESIS_READS, &source_arn),
            allow("kinesis", &["PutRecord"], &sink_arn),
        ])),
    )
    .await;
    given_handler_function(&router, case, "envelope-forwarder", &role, "envelope-sink").await;
    let mut reader = SinkReader::of(&router, case, "envelope-sink").await;
    let uuid = given_enabled_mapping(
        &router,
        case,
        "envelope-forwarder",
        &source_arn,
        &[],
    )
    .await;

    let mut placed: Vec<Placed> = Vec::with_capacity(rows.len());
    for row in rows {
        placed.push(
            put_record(
                &router,
                row.case,
                "envelope-source",
                row.data,
                row.partition_key,
            )
            .await,
        );
    }

    let forwarded = wait_for_forwarded(&router, case, &mut reader, rows.len()).await;
    for (row, placed) in rows.iter().zip(&placed) {
        let case = row.case;
        let envelope = forwarded
            .iter()
            .find(|envelope| {
                envelope["kinesis"]["sequenceNumber"] == json!(placed.sequence_number)
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the record placed at {} to be handed over, got \
                     {forwarded:?}",
                    placed.sequence_number
                )
            });
        assert_eq!(
            carried_bytes(case, envelope),
            row.data.to_vec(),
            "case {case:?}: expected the envelope to carry the exact bytes put, got {envelope}"
        );
        assert_eq!(
            envelope["kinesis"]["partitionKey"],
            json!(row.partition_key),
            "case {case:?}: expected the record's own partition key, got {envelope}"
        );
        assert_eq!(
            envelope["kinesis"]["kinesisSchemaVersion"],
            json!(SCHEMA_VERSION),
            "case {case:?}: expected the schema version of a Kinesis record, got {envelope}"
        );
        assert!(
            envelope["kinesis"]["approximateArrivalTimestamp"]
                .as_f64()
                .is_some_and(|arrived| arrived > 0.0),
            "case {case:?}: expected a numeric arrival instant, got {envelope}"
        );
        assert_eq!(
            envelope["eventID"],
            json!(format!("{}:{}", placed.shard_id, placed.sequence_number)),
            "case {case:?}: expected the record to be identified by its shard and sequence \
             number, got {envelope}"
        );
        assert_eq!(
            envelope["eventName"],
            json!(EVENT_NAME),
            "case {case:?}: expected the event name of a Kinesis record, got {envelope}"
        );
        assert_eq!(
            envelope["eventSource"],
            json!(EVENT_SOURCE),
            "case {case:?}: expected the event source of a Kinesis record, got {envelope}"
        );
        assert_eq!(
            envelope["eventVersion"],
            json!(EVENT_VERSION),
            "case {case:?}: expected the version of the envelope, got {envelope}"
        );
        assert_eq!(
            envelope["eventSourceARN"],
            json!(source_arn),
            "case {case:?}: expected the envelope to name the stream the record came from, got \
             {envelope}"
        );
        assert_eq!(
            envelope["awsRegion"],
            json!(REGION),
            "case {case:?}: expected the Region the stream lives in, got {envelope}"
        );
        assert_eq!(
            envelope["invokeIdentityArn"],
            json!(role),
            "case {case:?}: expected the function's execution role as the invoking identity, \
             got {envelope}"
        );
    }

    // The mapping that did all of this is the one the client created, still reading.
    let read = get_mapping(&router, case, &uuid).await;
    assert_eq!(
        read["State"],
        json!(STATE_ENABLED),
        "case {case:?}: expected the mapping to still be reading, got {read}"
    );
}

/// One row of the criteria table: the payload put, and whether the criteria admit it.
struct Judged {
    case: &'static str,
    data: &'static [u8],
    /// Whether the mapping's criteria admit the record, so its function is invoked with it.
    admitted: bool,
}

/// RULE: a mapping's `FilterCriteria` "determine whether Lambda should process an event", so
/// only the records they admit are part of an invocation at all — the records they reject
/// reach the function never, and do not hold the shard's position either.
#[tokio::test]
async fn only_the_records_the_criteria_admit_reach_the_function() {
    let case = "a mapping filtering on the decoded data of each record";
    // The pattern names a field INSIDE the record's payload, which a Kinesis record carries
    // base64-encoded.
    let pattern = r#"{"data":{"kind":["keep"]}}"#;
    // The rows differ along the payload the criteria judge. The LAST row is one the criteria
    // admit, so its arrival proves every row before it was read and decided.
    let rows: &[Judged] = &[
        Judged {
            case: "a payload whose value the pattern names",
            data: br#"{"kind":"keep","at":1}"#,
            admitted: true,
        },
        Judged {
            case: "a payload whose value disagrees",
            data: br#"{"kind":"drop","at":2}"#,
            admitted: false,
        },
        Judged {
            case: "a payload carrying the named field deeper down",
            data: br#"{"inner":{"kind":"keep"},"at":3}"#,
            admitted: false,
        },
        Judged {
            case: "a payload that spells no document at all",
            data: b"\x00\xff\x80keep",
            admitted: false,
        },
        Judged {
            case: "another payload whose value the pattern names",
            data: br#"{"kind":"keep","at":5}"#,
            admitted: true,
        },
    ];

    let router = emulator();
    let source_arn = given_stream(&router, case, "filter-source", 1).await;
    let sink_arn = given_stream(&router, case, "filter-sink", 1).await;
    let role = given_role(
        &router,
        case,
        "filter-role",
        Some(json!([
            logs_only(),
            allow("kinesis", &KINESIS_READS, &source_arn),
            allow("kinesis", &["PutRecord"], &sink_arn),
        ])),
    )
    .await;
    given_handler_function(&router, case, "filter-forwarder", &role, "filter-sink").await;
    let mut reader = SinkReader::of(&router, case, "filter-sink").await;
    given_enabled_mapping(
        &router,
        case,
        "filter-forwarder",
        &source_arn,
        &[pattern],
    )
    .await;

    // One shard, written in row order, so the shard holds the rows in the order the table
    // states them.
    for (index, row) in rows.iter().enumerate() {
        put_record(
            &router,
            row.case,
            "filter-source",
            row.data,
            &format!("pk-{index}"),
        )
        .await;
    }

    let admitted: Vec<&Judged> = rows.iter().filter(|row| row.admitted).collect();
    let forwarded = wait_for_forwarded(&router, case, &mut reader, admitted.len()).await;
    let carried: Vec<Vec<u8>> = forwarded
        .iter()
        .map(|envelope| carried_bytes(case, envelope))
        .collect();
    for row in rows {
        let held = carried.iter().filter(|bytes| *bytes == row.data).count();
        let expected = usize::from(row.admitted);
        assert_eq!(
            held,
            expected,
            "case {:?}: expected the criteria to hand the function {expected} record(s) \
             carrying this payload, got {held} of {} forwarded",
            row.case,
            carried.len()
        );
    }
    // And nothing else: the records the criteria rejected are not delivered late either, so a
    // further look finds no record beyond the ones the criteria admitted.
    let later = reader.drain(&router, case).await;
    assert!(
        later.is_empty(),
        "case {case:?}: expected the criteria to leave the rejected records undelivered, got \
         {later:?}"
    );
}
