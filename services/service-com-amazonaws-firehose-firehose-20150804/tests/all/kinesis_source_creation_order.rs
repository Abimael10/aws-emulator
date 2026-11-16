// SPDX-License-Identifier: CC0-1.0

//! Kinesis-to-Firehose-to-Amazon-S3 ingestion driven WITHOUT waiting for the two resources it
//! is made of to settle.
//!
//! The rule under test: a Firehose stream whose `DeliveryStreamType` is `KinesisStreamAsSource`
//! — "the Firehose stream uses a Kinesis data stream as a source"
//! (`CreateDeliveryStreamInput$DeliveryStreamType`) — consumes the Kinesis data stream its
//! `KinesisStreamSourceConfiguration` names from the instant its own retrieval starts at
//! ("Firehose starts retrieving records from the Kinesis data stream starting with this
//! timestamp" — `KinesisStreamSourceDescription$DeliveryStartTimestamp`, which the create
//! stamps) and writes every record it took into an object of the Amazon S3 destination the
//! create configured. That binding is made by the CREATE, so neither resource's own lifecycle
//! transition can cost a record: a Firehose stream created while its source is not yet serving
//! its shards consumes that source as soon as it is, and a record a producer writes before the
//! Firehose stream itself reports `ACTIVE` is retrieved once it does. A record is therefore
//! delivered exactly once however the two creates and the producer's write are interleaved.
//!
//! What this suite adds to its siblings: `kinesis_source_ingestion` and
//! `kinesis_source_fan_out` write to the source only after BOTH resources report `ACTIVE`, and
//! `source_fed_buffering_window` likewise, so every one of their rows is the settled case. Here
//! the waits are the variable: each row drops one or both of them, which is what a client
//! chaining the two creates and its first write does, and the one invariant asserted across the
//! rows is that the record still lands in the destination, byte for byte and exactly once.
//!
//! Firehose, Amazon S3 and Kinesis Data Streams are mounted on ONE shared engine by the
//! `destination_delivery` harness, as the router mounts them, so the Firehose and Kinesis calls
//! go as `POST /` with `X-Amz-Target` and the destination is read with the path-style Amazon S3
//! requests a client sends.

use serde_json::json;

use crate::delivery_stream_lifecycle::ROLE_ARN;
use crate::destination_delivery::{carries_time_prefix, Emulator};
// The source-ingestion suite's harness, reused rather than stood up a second time: the same
// source create and wait, the same wait for a Firehose stream to be ACTIVE, the same producer
// put, the same record markers, the same destination reads and the same exactly-once watcher.
use crate::kinesis_source_ingestion::{
    await_active_source_fed_stream, await_source_active, create_source, delivered_payloads, once,
    payload, put_to_source, source_stream_arn, stays_delivered,
};

/// The two lifecycle values a Firehose stream this suite writes for can be reporting when its
/// producer writes: the status a create leaves it in and the status it reaches
/// (`DeliveryStreamDescription$DeliveryStreamStatus`, whose value set is `CREATING`,
/// `CREATING_FAILED`, `DELETING`, `DELETING_FAILED` and `ACTIVE` — the deleting values cannot
/// be reached by a stream nothing has asked to delete, and a create this suite makes is
/// accepted).
///
/// What a row expects of it is narrower: a row that WAITED for the stream owes exactly `ACTIVE`,
/// and only a row that skipped that wait may be either.
const STATUS_AT_PUT: [&str; 2] = ["CREATING", "ACTIVE"];

/// One case: which of the two settling waits the client makes before it writes, and the
/// destination the Firehose stream is created with.
struct Row {
    case: &'static str,
    /// Whether the source Kinesis data stream is waited for before `CreateDeliveryStream` is
    /// called — `false` binds the Firehose stream to a source that is not serving its shards
    /// yet.
    source_settled_before_create: bool,
    /// Whether the Firehose stream is waited for before the producer writes — `false` writes to
    /// the source while that stream may still be reporting `CREATING`.
    stream_settled_before_put: bool,
    /// How many shards the source Kinesis data stream is created with.
    shards: u32,
    /// The `CreateDeliveryStream` member carrying the Amazon S3 destination.
    member: &'static str,
    /// The `Prefix` the destination is configured with, `None` to leave the member out.
    prefix: Option<&'static str>,
    /// How many records the producer writes to the source.
    records: usize,
}

/// The cases.
///
/// The rows differ along what the rule generalises over: which of the two settling waits the
/// client makes (both, each one alone, neither), how many shards the source spreads its records
/// over, which destination member carries the Amazon S3 location, the form of the `Prefix` (a
/// single segment, a nested one, and the member left out) and how many records the producer
/// writes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "both settled: the control a client polling everything gets",
            source_settled_before_create: true,
            stream_settled_before_put: true,
            shards: 1,
            member: "ExtendedS3DestinationConfiguration",
            prefix: Some("settled/"),
            records: 1,
        },
        Row {
            case: "written before the Firehose stream is waited for",
            source_settled_before_create: true,
            stream_settled_before_put: false,
            shards: 1,
            member: "ExtendedS3DestinationConfiguration",
            prefix: None,
            records: 3,
        },
        Row {
            case: "bound to a source that is not serving its shards yet",
            source_settled_before_create: false,
            stream_settled_before_put: true,
            shards: 2,
            member: "S3DestinationConfiguration",
            prefix: Some("landing/by-day/"),
            records: 2,
        },
        Row {
            case: "neither wait made: the two creates and the write chained",
            source_settled_before_create: false,
            stream_settled_before_put: false,
            shards: 3,
            member: "S3DestinationConfiguration",
            prefix: Some("chained/"),
            records: 4,
        },
    ]
}

#[tokio::test]
async fn a_source_fed_stream_loses_no_record_to_either_resources_transition() {
    let emulator = Emulator::new();
    for (ordinal, row) in rows().into_iter().enumerate() {
        let bucket = format!("creation-order-bucket-{ordinal}");
        let source = format!("creation-order-source-{ordinal}");
        let stream = format!("creation-order-stream-{ordinal}");
        emulator.create_bucket(&bucket).await;

        // The source exists from its create on — the Firehose stream is bound to a stream that
        // is there, whatever lifecycle status it is reporting.
        create_source(&emulator, &source, row.shards).await;
        let source_arn = if row.source_settled_before_create {
            await_source_active(&emulator, row.case, &source).await
        } else {
            source_stream_arn(&emulator, row.case, &source).await
        };

        create_source_fed_stream(&emulator, &row, &stream, &source_arn, &bucket).await;
        if row.stream_settled_before_put {
            await_active_source_fed_stream(&emulator, row.case, &stream, &source_arn).await;
        }

        // Nothing has been written to the source, so the destination holds nothing: a create
        // delivers no object of its own.
        let prefix = row.prefix.unwrap_or_default();
        let before = emulator.keys_under(&bucket, prefix).await;
        assert!(
            before.is_empty(),
            "case {:?}: a stream whose source nothing has written to delivers nothing, \
             got {before:?}",
            row.case
        );

        // The producer's OWN wait: a Kinesis data stream takes a record once it is serving its
        // shards, which says nothing about the Firehose stream reading it.
        await_source_active(&emulator, row.case, &source).await;
        let at_put = reported_status(&emulator, &stream).await;
        // A row that waited owes exactly the status it waited for; only a row that skipped that
        // wait may be reporting either of the two a created stream can.
        let owed: &[&str] = if row.stream_settled_before_put {
            &["ACTIVE"]
        } else {
            &STATUS_AT_PUT
        };
        assert!(
            owed.contains(&at_put.as_str()),
            "case {:?}: expected {stream} to report one of {owed:?} when the producer writes, \
             got {at_put:?}",
            row.case
        );

        let payloads: Vec<Vec<u8>> = (0..row.records)
            .map(|index| payload(ordinal, index, 0))
            .collect();
        for (index, data) in payloads.iter().enumerate() {
            put_to_source(&emulator, &source, &format!("pk-{index}"), data).await;
        }

        // Every record written reaches the destination, read back through `ListObjectsV2` and
        // `GetObject` as a client reads it.
        let bodies = delivered_payloads(&emulator, row.case, &bucket, prefix, &payloads).await;
        assert!(
            !bodies.is_empty(),
            "case {:?} (the stream reported {at_put:?} when the producer wrote): expected at \
             least one delivered object under {bucket}/{prefix}",
            row.case
        );
        for key in bodies.keys() {
            assert!(
                key.starts_with(prefix),
                "case {:?} (the stream reported {at_put:?} when the producer wrote): expected a \
                 key under {prefix:?}, got {key:?}",
                row.case
            );
            assert!(
                carries_time_prefix(key, prefix),
                "case {:?}: a delivered key carries the automatic YYYY/MM/DD/HH prefix, got \
                 {key:?}",
                row.case
            );
            assert!(
                key.contains(&stream),
                "case {:?}: a delivered object is named for its Firehose stream, got {key:?}",
                row.case
            );
        }

        // And each record is there exactly once: the transition neither lost it nor handed it
        // to a second poll.
        stays_delivered(&emulator, row.case, &bucket, prefix, &once(&payloads)).await;
    }
}

/// The `DeliveryStreamStatus` the Firehose stream `name` reports right now.
async fn reported_status(emulator: &Emulator, name: &str) -> String {
    let described = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": name }),
        )
        .await;
    described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} reports its status, got {described:?}"))
        .to_string()
}

/// Create the row's `KinesisStreamAsSource` Firehose stream reading `source_arn` and delivering
/// to `bucket`, without waiting for it.
///
/// The destination asks for the shortest `IntervalInSeconds` its member admits and a `SizeInMBs`
/// the row's records never reach, so what decides when the records appear is the consumption
/// this suite is about and not the buffering window its siblings pin.
async fn create_source_fed_stream(
    emulator: &Emulator,
    row: &Row,
    stream: &str,
    source_arn: &str,
    bucket: &str,
) {
    let mut destination = json!({
        "RoleARN": ROLE_ARN,
        "BucketARN": format!("arn:aws:s3:::{bucket}"),
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
    });
    if let Some(prefix) = row.prefix {
        destination["Prefix"] = json!(prefix);
    }
    let mut request = json!({
        "DeliveryStreamName": stream,
        "DeliveryStreamType": "KinesisStreamAsSource",
        "KinesisStreamSourceConfiguration": {
            "KinesisStreamARN": source_arn,
            "RoleARN": ROLE_ARN,
        },
    });
    request[row.member] = destination;
    let answered = emulator.firehose_ok("CreateDeliveryStream", request).await;
    assert!(
        answered["DeliveryStreamARN"]
            .as_str()
            .is_some_and(|arn| arn.contains(stream)),
        "case {:?}: the create answers the stream's ARN, got {answered:?}",
        row.case
    );
}
