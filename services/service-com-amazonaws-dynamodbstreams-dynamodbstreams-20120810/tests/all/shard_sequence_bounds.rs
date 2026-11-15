// SPDX-License-Identifier: CC0-1.0

//! Where a record's `SequenceNumber` sits inside the range its own shard advertises.
//!
//! THE RULE: a shard's `SequenceNumberRange` BOUNDS the records that shard carries — its
//! `StartingSequenceNumber` is the position the shard opens at, so every record of that
//! shard sequences strictly after it, and a closed shard's `EndingSequenceNumber` is at
//! or after its last record. Within a shard the numbers grow strictly with append order.
//! That is what lets a consumer resume strictly after a checkpoint and de-duplicate what
//! it has already seen: a record sharing its shard's opening number would order with the
//! bound instead of after it, so a poller re-reading from the bound could neither skip it
//! nor tell it apart from the shard's own start.
//!
//! The rows span what the rule generalises over: how many records a shard carries, a
//! shard that carries none, an OPEN shard and a CLOSED one, a parent shard and the child
//! that was rolled onto it, and more than one stream in the registry — the shards of
//! which are minted from one counter, so one stream's bounds never swallow another's
//! records.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::{AttributeValue, KeySchemaElement};
use streams::state::{
    compare_sequence_numbers, AttributeMap,
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, OperationType,
    RecordAppend, StreamRegistration, StreamViewType,
};

/// A SigV4 credential scope naming `dynamodb`, which is how a DynamoDB Streams request
/// is signed. Request authentication is off here, so only its routing signal matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

fn build() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(streams::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("authorization", AUTHORIZATION)
        .header(
            "x-amz-target",
            format!("DynamoDBStreams_20120810.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

/// A whole-second instant `seconds` before now — far enough back that the enable the
/// clock owes a new registration is already due, so the stream serves its shards.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs();
    UNIX_EPOCH + Duration::from_secs(now - seconds)
}

fn register(engine: &engine::InMemoryEngine, table_name: &str, at: SystemTime) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type: StreamViewType::NewImage,
                key_schema: vec![KeySchemaElement {
                    attribute_name: "Id".to_string(),
                    key_type: "HASH".to_string(),
                }],
                region: REGION.to_string(),
            },
            at,
        )
        .expect("the registry accepts the registration")
}

fn keys_of(id: &str) -> AttributeMap {
    HashMap::from([("Id".to_string(), AttributeValue::S(id.to_string()))])
}

/// Publish one `INSERT` onto `stream_arn` — the seam DynamoDB's item writes call — and
/// return the sequence number it was given.
fn append(engine: &engine::InMemoryEngine, stream_arn: &str, id: &str, at: SystemTime) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .append_record(
            stream_arn,
            &RecordAppend {
                event_name: OperationType::Insert,
                keys: keys_of(id),
                new_image: Some(keys_of(id)),
                old_image: None,
            },
            at,
        )
        .unwrap_or_else(|rejected| panic!("the registry must take the record, got {rejected}"))
}

/// Close the stream's open shard and open a child on it, the way a stream rolls a shard
/// over as it ages.
fn roll(engine: &engine::InMemoryEngine, stream_arn: &str, at: SystemTime) {
    engine
        .state::<StreamsState>()
        .lock()
        .roll_shard(stream_arn, at)
        .unwrap_or_else(|| panic!("the stream {stream_arn} must roll its open shard"));
}

/// Every shard the stream serves, as `DescribeStream` reports it (creation order).
async fn shards(router: &MultiServiceRouter, stream_arn: &str) -> Vec<serde_json::Value> {
    let (status, body) = call(
        router,
        "DescribeStream",
        serde_json::json!({"StreamArn": stream_arn}),
    )
    .await;
    assert_eq!(status, 200, "DescribeStream answered {status} with {body}");
    body["StreamDescription"]["Shards"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Every record of one shard, read from its trim horizon through the surface a consumer
/// reads it with.
async fn records_of(
    router: &MultiServiceRouter,
    case: &str,
    stream_arn: &str,
    shard_id: &str,
) -> Vec<serde_json::Value> {
    let (status, minted) = call(
        router,
        "GetShardIterator",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: GetShardIterator answered {status} with {minted}"
    );
    let mut iterator = minted["ShardIterator"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut collected = Vec::new();
    // Bounded: a shard that never advances its iterator fails rather than hanging.
    for _ in 0..8 {
        let (status, page) = call(
            router,
            "GetRecords",
            serde_json::json!({"ShardIterator": iterator}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: GetRecords answered {status} with {page}"
        );
        let batch = page["Records"].as_array().cloned().unwrap_or_default();
        collected.extend(batch.iter().cloned());
        match page["NextShardIterator"].as_str() {
            Some(next) if !batch.is_empty() => iterator = next.to_string(),
            _ => break,
        }
    }
    collected
}

/// The `SequenceNumber` of each record, in the order the shard delivered them.
fn sequence_numbers(records: &[serde_json::Value]) -> Vec<String> {
    records
        .iter()
        .map(|record| {
            record["dynamodb"]["SequenceNumber"]
                .as_str()
                .unwrap_or_else(|| panic!("every record carries a SequenceNumber, got {record}"))
                .to_string()
        })
        .collect()
}

/// One shard row: how many records the shard is given, and whether it is rolled over
/// (and so closed) afterwards.
struct ShardRow {
    case: &'static str,
    records: usize,
    closed: bool,
}

/// The rows differ in the record COUNT the shard carries — none, one, several — and in
/// whether the shard is still open or was closed by a rollover.
fn shard_table() -> Vec<ShardRow> {
    vec![
        ShardRow {
            case: "a closed shard carrying several records",
            records: 3,
            closed: true,
        },
        ShardRow {
            case: "a closed shard carrying one record",
            records: 1,
            closed: true,
        },
        ShardRow {
            case: "a closed shard that carried none",
            records: 0,
            closed: true,
        },
        ShardRow {
            case: "the open shard, carrying two records",
            records: 2,
            closed: false,
        },
    ]
}

#[tokio::test]
async fn every_record_sequences_after_the_start_of_the_shard_that_carries_it() {
    let (engine, router) = build();
    // Two streams in one registry, so a shard of one is minted beside the other's: the
    // bounds each shard advertises are its own.
    let other = register(&engine, "OtherTable", seconds_ago(60));
    append(&engine, &other, "other-1", seconds_ago(50));
    let stream = register(&engine, "SequencedTable", seconds_ago(60));
    append(&engine, &other, "other-2", seconds_ago(45));

    let rows = shard_table();
    for (at, row) in rows.iter().enumerate() {
        for ordinal in 0..row.records {
            append(
                &engine,
                &stream,
                &format!("{at}-{ordinal}"),
                seconds_ago(40 - at as u64),
            );
        }
        if row.closed {
            roll(&engine, &stream, seconds_ago(40 - at as u64));
        }
    }

    let served = shards(&router, &stream).await;
    assert_eq!(
        served.len(),
        rows.len(),
        "the stream serves one shard per row, got {served:?}"
    );
    let mut previous_start: Option<String> = None;
    for (row, shard) in rows.iter().zip(served.iter()) {
        let case = row.case;
        let shard_id = shard["ShardId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no ShardId in {shard}"))
            .to_string();
        let range = &shard["SequenceNumberRange"];
        let starting = range["StartingSequenceNumber"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a shard advertises its start, got {shard}"))
            .to_string();
        let ending = range["EndingSequenceNumber"].as_str().map(str::to_string);
        assert_eq!(
            ending.is_some(),
            row.closed,
            "case {case}: an end is advertised only by a closed shard, got {shard}"
        );

        // A child shard opens after its parent did, so the shards of one stream carry
        // their records in the order the shards were minted.
        if let Some(previous) = previous_start.as_deref() {
            assert_eq!(
                compare_sequence_numbers(&starting, previous),
                std::cmp::Ordering::Greater,
                "case {case}: expected the shard to open after the previous one \
                 ({previous}), got {starting}"
            );
        }
        previous_start = Some(starting.clone());

        let records = records_of(&router, case, &stream, &shard_id).await;
        let delivered = sequence_numbers(&records);
        assert_eq!(
            delivered.len(),
            row.records,
            "case {case}: expected {} record(s), got {delivered:?}",
            row.records
        );
        let mut highest = starting.clone();
        for (position, number) in delivered.iter().enumerate() {
            assert_eq!(
                compare_sequence_numbers(number, &starting),
                std::cmp::Ordering::Greater,
                "case {case}: record {position} must sequence after its shard's start \
                 ({starting}), got {number}"
            );
            assert_eq!(
                compare_sequence_numbers(number, &highest),
                std::cmp::Ordering::Greater,
                "case {case}: record {position} must sequence after the record before it \
                 ({highest}), got {number}"
            );
            highest = number.clone();
            if let Some(ending) = ending.as_deref() {
                assert_ne!(
                    compare_sequence_numbers(number, ending),
                    std::cmp::Ordering::Greater,
                    "case {case}: record {position} must sequence at or before its shard's \
                     end ({ending}), got {number}"
                );
            }
        }
        // A closed shard's end is at or after its start whether or not it ever took a
        // record: one that took none closes on its own opening position.
        if let Some(ending) = ending.as_deref() {
            assert_ne!(
                compare_sequence_numbers(ending, &starting),
                std::cmp::Ordering::Less,
                "case {case}: a closed shard's end ({ending}) is at or after its start \
                 ({starting})"
            );
            let expected_end = if row.records == 0 {
                starting.clone()
            } else {
                highest.clone()
            };
            assert_eq!(
                ending, expected_end,
                "case {case}: a closed shard ends on its last record, or on its own start \
                 when it took none, got {ending}"
            );
        }
    }
}

#[tokio::test]
async fn a_record_can_be_addressed_from_its_own_shards_start() {
    let (engine, router) = build();
    let stream = register(&engine, "AddressedTable", seconds_ago(60));
    let first = append(&engine, &stream, "one", seconds_ago(50));
    let second = append(&engine, &stream, "two", seconds_ago(49));

    let served = shards(&router, &stream).await;
    let shard = served.first().cloned().unwrap_or(serde_json::Value::Null);
    let shard_id = shard["ShardId"].as_str().unwrap_or_default().to_string();
    let starting = shard["SequenceNumberRange"]["StartingSequenceNumber"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // AT the shard's own opening position delivers the whole shard, because every record
    // of it sequences after that position; AFTER the first record delivers the rest.
    // (case, the iterator type, the number it names, the records it must deliver)
    let rows: [(&str, &str, &str, Vec<String>); 3] = [
        (
            "at the shard's start",
            "AT_SEQUENCE_NUMBER",
            starting.as_str(),
            vec![first.clone(), second.clone()],
        ),
        (
            "after the shard's start",
            "AFTER_SEQUENCE_NUMBER",
            starting.as_str(),
            vec![first.clone(), second.clone()],
        ),
        (
            "after the first record",
            "AFTER_SEQUENCE_NUMBER",
            first.as_str(),
            vec![second.clone()],
        ),
    ];
    for (case, iterator_type, number, expected) in rows {
        let (status, minted) = call(
            &router,
            "GetShardIterator",
            serde_json::json!({
                "StreamArn": stream,
                "ShardId": shard_id,
                "ShardIteratorType": iterator_type,
                "SequenceNumber": number,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: GetShardIterator answered {status} with {minted}"
        );
        let (status, page) = call(
            &router,
            "GetRecords",
            serde_json::json!({"ShardIterator": minted["ShardIterator"]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: GetRecords answered {status} with {page}"
        );
        let delivered = sequence_numbers(&page["Records"].as_array().cloned().unwrap_or_default());
        assert_eq!(
            delivered, expected,
            "case {case}: expected the records {expected:?}, got {delivered:?}"
        );
    }
}
