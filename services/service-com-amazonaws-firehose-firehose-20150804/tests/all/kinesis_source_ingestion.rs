// SPDX-License-Identifier: CC0-1.0

//! The Kinesis-data-stream-to-Firehose-to-Amazon-S3 data plane, driven over the wire.
//!
//! The rule under test: a Firehose stream whose `DeliveryStreamType` names a source service
//! takes its data from the source its source configuration names, and not from a put — for
//! `KinesisStreamAsSource` that is the Kinesis data stream of
//! `KinesisStreamSourceConfiguration{KinesisStreamARN, RoleARN}`, "the stream and role Amazon
//! Resource Names (ARNs) for a Kinesis data stream used as the source for a Firehose stream".
//! Every record that stream takes from the instant retrieval starts at
//! ("Firehose starts retrieving records from the Kinesis data stream starting with this
//! timestamp" — `KinesisStreamSourceDescription$DeliveryStartTimestamp`) is buffered exactly
//! once, in its shard's order, and is then delivered to the destination exactly as a
//! directly-put record is: an object under the destination's `Prefix` with the automatic
//! `YYYY/MM/DD/HH` expansion, on whichever `BufferingHints` condition is met first.
//!
//! The ORDER is part of that rule: a shard appends in arrival order and the records it took are
//! buffered and delivered in that same order, so for the records carrying one partition key —
//! which always addresses one shard — the bytes at the destination carry them in the order the
//! producer published them, however the sequence is cut into buffers.
//!
//! Three services are mounted on ONE shared engine, as the router mounts them, so what is
//! exercised is the hand-off between them: the Kinesis writes and the Firehose calls as
//! `POST /` with `X-Amz-Target`, the destination reads as the path-style Amazon S3 requests a
//! client sends.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::buffering_hints::SHORT_INTERVAL;
use crate::delivery_stream_lifecycle::{REGION, ROLE_ARN};
use crate::destination_delivery::{base64, carries_time_prefix, offsets, Emulator};

/// The `X-Amz-Target` prefix of the Kinesis Data Streams wire.
pub(crate) const KINESIS_TARGET: &str = "Kinesis_20131202";

/// The routing key a Kinesis request is signed for.
pub(crate) const KINESIS_SERVICE: &str = "kinesis";

/// How long a delivery out of a source stream is waited for before the case fails.
///
/// Longer than the `DirectPut` suite's deadline: a record has to be CONSUMED from the source
/// before its buffer even opens, so the wait covers a source poll, the buffer's window and
/// the destination write, with room to spare on a slow machine.
pub(crate) const DELIVERY_DEADLINE: Duration = Duration::from_secs(20);

/// How often the destination is polled while waiting for a delivery.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a destination is WATCHED once every record has arrived, to see that nothing is
/// delivered twice and that nothing the stream should not have retrieved appears: several
/// source polls and several buffer windows. The window is watched by looking again every
/// [`POLL_INTERVAL`], not by waiting it out.
const WATCH: Duration = Duration::from_millis(1_500);

/// One source-ingestion case: the source stream's shape, the destination the Firehose stream
/// delivers to, and the records a producer writes to the source.
struct SourceRow {
    case: &'static str,
    /// How many shards the source stream is created with, so the records of a case are
    /// spread over that many shards.
    shards: u32,
    /// The `CreateDeliveryStream` member carrying the destination.
    member: &'static str,
    /// The `Prefix` the destination is configured with, `None` to leave the member out.
    prefix: Option<&'static str>,
    /// `BufferingHints` as `(SizeInMBs, IntervalInSeconds)`, `None` to leave the member out
    /// and take the documented defaults (5 MiB, 300 s).
    hints: Option<(i64, i64)>,
    /// How many records the producer writes to the source.
    records: usize,
    /// How many bytes of filler each record carries beyond its marker.
    record_filler: usize,
}

/// The source-ingestion cases.
///
/// The rows differ along what the rule generalises over: how many shards the records are
/// spread over, which destination member carries the Amazon S3 location, whether the
/// destination names a `Prefix` of its own, the `BufferingHints` the stream was created with
/// — a size the records cross well inside a long interval, and each published bound of
/// `SizeInMBs` (1..128) under the shortest interval the member admits — and the
/// number and size of the records written to the source.
///
/// Every row whose records do not cross `SizeInMBs` asks for `IntervalInSeconds` 0, the
/// shortest wait the member admits, because what these cases assert is that a source's
/// records reach the destination at all; the wait a configured interval buys is asserted by
/// the `buffering_hints` cases.
fn source_rows() -> Vec<SourceRow> {
    vec![
        SourceRow {
            case: "one record, an extended destination, a 1 MiB buffer",
            shards: 1,
            member: "ExtendedS3DestinationConfiguration",
            prefix: None,
            hints: Some((1, 0)),
            records: 1,
            record_filler: 0,
        },
        SourceRow {
            case: "twenty ~60 KiB records crossing SizeInMBs=1 inside IntervalInSeconds=900",
            shards: 1,
            member: "S3DestinationConfiguration",
            prefix: None,
            hints: Some((1, 900)),
            records: 20,
            record_filler: 60 * 1024,
        },
        SourceRow {
            case: "eight records spread over four shards, the documented default size",
            shards: 4,
            member: "ExtendedS3DestinationConfiguration",
            prefix: Some("landing/"),
            hints: Some((5, 0)),
            records: 8,
            record_filler: 32,
        },
        SourceRow {
            case: "the largest SizeInMBs and the smallest IntervalInSeconds",
            shards: 2,
            member: "S3DestinationConfiguration",
            prefix: Some("edge/bounds/"),
            hints: Some((128, 0)),
            records: 5,
            record_filler: 16,
        },
    ]
}

#[tokio::test]
async fn a_kinesis_source_streams_records_are_delivered_to_the_destination() {
    for (ordinal, row) in source_rows().into_iter().enumerate() {
        let emulator = Emulator::new();
        let bucket = format!("source-bucket-{ordinal}");
        let source = format!("source-stream-{ordinal}");
        let stream = format!("source-fed-stream-{ordinal}");
        emulator.create_bucket(&bucket).await;
        let source_arn = create_source_stream(&emulator, &source, row.shards).await;

        create_source_fed_stream(&emulator, &stream, &source_arn, &row, &bucket).await;

        // Nothing has been written to the source yet, so there is nothing to consume and
        // nothing to deliver.
        let before = emulator.keys_under(&bucket, "").await;
        assert!(
            before.is_empty(),
            "case {:?}: an idle source delivers nothing, got {before:?}",
            row.case
        );

        // What the producer writes to the SOURCE — never to Firehose, which refuses a put to
        // a stream fed by a configured source.
        let payloads: Vec<Vec<u8>> = (0..row.records)
            .map(|index| payload(ordinal, index, row.record_filler))
            .collect();
        for (index, payload) in payloads.iter().enumerate() {
            put_to_source(&emulator, &source, &format!("pk-{index}"), payload).await;
        }

        let bodies = delivered_payloads(&emulator, row.case, &bucket, "", &payloads).await;
        let prefix = row.prefix.unwrap_or_default();
        for key in bodies.keys() {
            assert!(
                key.starts_with(prefix),
                "case {:?}: expected a key under {prefix:?}, got {key:?}",
                row.case
            );
            assert!(
                carries_time_prefix(key, prefix),
                "case {:?}: a delivered key carries the automatic YYYY/MM/DD/HH prefix, \
                 got {key:?}",
                row.case
            );
            assert!(
                key.contains(&stream),
                "case {:?}: a delivered object is named for its Firehose stream, got {key:?}",
                row.case
            );
        }

        // Each record of the source reached the destination exactly once: a consumed record
        // is not consumed again, and a delivered buffer is not delivered again.
        stays_delivered(&emulator, row.case, &bucket, "", &once(&payloads)).await;
    }
}

#[tokio::test]
async fn retrieval_starts_at_the_delivery_start_timestamp() {
    let emulator = Emulator::new();
    let bucket = "delivery-start-bucket".to_string();
    let source = "delivery-start-source".to_string();
    let stream = "delivery-start-stream".to_string();
    emulator.create_bucket(&bucket).await;
    let source_arn = create_source_stream(&emulator, &source, 1).await;

    // A record the source held BEFORE the Firehose stream existed: retrieval starts at the
    // stream's own `DeliveryStartTimestamp`, so this one is not its data.
    let before = b"written-before-the-firehose-stream".to_vec();
    put_to_source(&emulator, &source, "pk-before", &before).await;

    let row = SourceRow {
        case: "retrieval starts at the delivery start timestamp",
        shards: 1,
        member: "ExtendedS3DestinationConfiguration",
        prefix: None,
        hints: Some((1, 0)),
        records: 0,
        record_filler: 0,
    };
    create_source_fed_stream(&emulator, &stream, &source_arn, &row, &bucket).await;

    let after = b"written-after-the-firehose-stream".to_vec();
    put_to_source(&emulator, &source, "pk-after", &after).await;

    let delivered =
        delivered_payloads(&emulator, row.case, &bucket, "", std::slice::from_ref(&after)).await;
    let held: Vec<u8> = delivered.values().flatten().copied().collect();
    assert_eq!(
        occurrences(&held, &before),
        0,
        "a record the source held before the Firehose stream was created is not retrieved, \
         got {:?}",
        String::from_utf8_lossy(&held)
    );

    // And it stays unretrieved: the position the first poll took is past it for good, and
    // the record written after retrieval started is delivered exactly once.
    stays_delivered(
        &emulator,
        row.case,
        &bucket,
        "",
        &[(before.as_slice(), 0), (after.as_slice(), 1)],
    )
    .await;
}

#[tokio::test]
async fn a_source_fed_stream_takes_no_direct_put() {
    let emulator = Emulator::new();
    let bucket = "no-direct-put-bucket".to_string();
    let source = "no-direct-put-source".to_string();
    let stream = "no-direct-put-stream".to_string();
    emulator.create_bucket(&bucket).await;
    let source_arn = create_source_stream(&emulator, &source, 1).await;
    let row = SourceRow {
        case: "a source-fed stream takes no direct put",
        shards: 1,
        member: "S3DestinationConfiguration",
        prefix: None,
        hints: Some((1, 0)),
        records: 0,
        record_filler: 0,
    };
    create_source_fed_stream(&emulator, &stream, &source_arn, &row, &bucket).await;

    // The ingestion operations are not available on a stream fed by a configured source, so
    // the destination's data can only have come from the source.
    let (status, answered) = emulator.put_record(&stream, b"direct").await;
    assert_eq!(
        status, 400,
        "a put to a KinesisStreamAsSource stream is refused, got {answered:?}"
    );

    let through_the_source = b"through-the-source".to_vec();
    put_to_source(&emulator, &source, "pk-1", &through_the_source).await;
    let delivered = delivered_payloads(
        &emulator,
        "a source-fed stream takes no direct put",
        &bucket,
        "",
        std::slice::from_ref(&through_the_source),
    )
    .await;
    let held: Vec<u8> = delivered.values().flatten().copied().collect();
    assert_eq!(
        occurrences(&held, b"direct"),
        0,
        "the refused put is nowhere in the destination, got {:?}",
        String::from_utf8_lossy(&held)
    );
}

/// How long a record-order case waits for the records it published to be delivered.
///
/// Longer than the suite's own [`DELIVERY_DEADLINE`]: every row of this table waits out a
/// source poll AND a non-zero buffering window before its last buffer can be an object, so the
/// wait is kept generous enough that a loaded machine cannot decide a case. The case fails only
/// if a record is still undelivered once this has passed.
const ORDER_DEADLINE: Duration = Duration::from_secs(90);

/// One record-ORDER case: the case shape the consumption rows already describe, plus how the
/// producer spreads the records over the source's partition keys.
struct OrderRow {
    /// The source's shards, the destination member, its `Prefix`, its `BufferingHints` and the
    /// records published — the SAME row type the consumption cases use, so the stream is
    /// created by the one helper rather than by a second spelling of it.
    source: SourceRow,
    /// How many partition keys those records are published round-robin over. One partition
    /// key always addresses ONE shard, so the records carrying it are a subsequence of that
    /// shard's own order.
    partition_keys: usize,
}

/// The record-order cases.
///
/// The rows differ along what the rule generalises over: how many records are published, how
/// many partition keys they are spread over and how many shards the source has, which
/// destination member carries the Amazon S3 location, whether that destination names a
/// `Prefix`, the size of each record, and — the point of the table — WHICH buffering
/// condition flushes the buffers the sequence passes through: an interval that takes every
/// record of the sequence in one window, the shortest interval the member admits so a window
/// closes while the producer is still publishing, and a `SizeInMBs` several of the records
/// cross so the size condition cuts the sequence part-way through. The order has to hold
/// however the sequence is cut into buffers.
///
/// Every row names a non-zero interval or the shortest one its member admits, so whatever a
/// row's last buffer holds is delivered on one of the two conditions: a tail left below
/// `SizeInMBs` is still closed by the interval, and no row depends on how the consumption of
/// its source happened to group the records.
fn order_rows() -> Vec<OrderRow> {
    vec![
        OrderRow {
            source: SourceRow {
                case: "eight records on one partition key of a single shard, one window",
                shards: 1,
                member: "ExtendedS3DestinationConfiguration",
                prefix: Some("ordered/"),
                hints: Some((1, SHORT_INTERVAL)),
                records: 8,
                record_filler: 0,
            },
            partition_keys: 1,
        },
        OrderRow {
            source: SourceRow {
                case: "twenty records on one partition key, flushed as each buffer opens",
                shards: 1,
                member: "S3DestinationConfiguration",
                prefix: None,
                hints: Some((1, 0)),
                records: 20,
                record_filler: 0,
            },
            partition_keys: 1,
        },
        OrderRow {
            source: SourceRow {
                case: "records crossing SizeInMBs part-way through the sequence",
                shards: 1,
                member: "ExtendedS3DestinationConfiguration",
                prefix: Some("by-size/"),
                hints: Some((1, SHORT_INTERVAL)),
                records: 6,
                record_filler: 400 * 1024,
            },
            partition_keys: 1,
        },
        OrderRow {
            source: SourceRow {
                case: "twelve records over four partition keys of a four-shard source",
                shards: 4,
                member: "ExtendedS3DestinationConfiguration",
                prefix: Some("fan/in/"),
                hints: Some((5, SHORT_INTERVAL)),
                records: 12,
                record_filler: 24,
            },
            partition_keys: 4,
        },
    ]
}

/// Where one published record was found at the destination.
#[derive(Debug)]
struct Placement {
    /// The key of the object holding it.
    object: String,
    /// Its offset inside that object's bytes, which is the flushed buffer's own order.
    offset: usize,
}

#[tokio::test]
async fn the_order_a_shard_took_its_records_in_is_the_order_they_are_delivered_in() {
    for (ordinal, row) in order_rows().into_iter().enumerate() {
        let case = row.source.case;
        let emulator = Emulator::new();
        let bucket = format!("record-order-bucket-{ordinal}");
        let source = format!("record-order-source-{ordinal}");
        let stream = format!("record-order-stream-{ordinal}");
        emulator.create_bucket(&bucket).await;
        let source_arn = create_source_stream(&emulator, &source, row.source.shards).await;
        create_source_fed_stream(&emulator, &stream, &source_arn, &row.source, &bucket).await;

        // What the producer publishes, in order: the `index`th record on the partition key
        // `index % partition_keys`, never through a put to Firehose, which a stream fed by a
        // configured source refuses.
        let records = row.source.records;
        let keys: Vec<usize> = (0..records).map(|index| index % row.partition_keys).collect();
        let payloads: Vec<Vec<u8>> = (0..records)
            .map(|index| ordered_payload(ordinal, keys[index], index, row.source.record_filler))
            .collect();
        for (index, payload) in payloads.iter().enumerate() {
            put_to_source(&emulator, &source, &partition_key(keys[index]), payload).await;
        }

        let prefix = row.source.prefix.unwrap_or_default();
        let bodies =
            delivered_payloads_within(&emulator, case, &bucket, prefix, &payloads, ORDER_DEADLINE)
                .await;
        let placements = placements(case, &bodies, ordinal, &keys);
        for key in 0..row.partition_keys {
            let published: Vec<usize> = (0..records).filter(|index| keys[*index] == key).collect();
            assert_delivered_in_order(case, key, &published, &placements);
        }
    }
}

/// Where each published record was delivered, by its index in put order.
///
/// A record missing from `bodies`, or carried by more than one of its objects, fails the case
/// here: an order is only an order over a sequence each of whose records was delivered exactly
/// once.
fn placements(
    case: &str,
    bodies: &BTreeMap<String, Vec<u8>>,
    ordinal: usize,
    keys: &[usize],
) -> Vec<Placement> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| {
            let marker = marker(ordinal, *key, index).into_bytes();
            let mut found: Vec<Placement> = Vec::new();
            for (object, body) in bodies {
                found.extend(offsets(body, &marker).into_iter().map(|offset| Placement {
                    object: object.clone(),
                    offset,
                }));
            }
            assert_eq!(
                found.len(),
                1,
                "case {case:?}: the record {index} published to the source is delivered exactly \
                 once, got {found:?}"
            );
            found.remove(0)
        })
        .collect()
}

/// Require the records `published` on ONE partition key — one shard's own subsequence, given
/// in put order — to be delivered in that order: each object holds a run of them in put order,
/// and the runs, taken in the order the sequence enters them, are the whole sequence.
///
/// The runs are ordered by the first record each holds rather than by object key, because a
/// delivered object's name ends in a value unique to the object: the keys of two objects of one
/// stream say nothing about which buffer was flushed first. What this pins is what the rule
/// states — a buffer carries a CONSECUTIVE run of its shard in the shard's own order, so no
/// record is delivered before one the shard took earlier or after one it took later. A buffer
/// holding a single record carries its order trivially, which is why the rows whose window
/// takes several records at once are in the table beside the ones that flush as they fill.
fn assert_delivered_in_order(
    case: &str,
    partition_key: usize,
    published: &[usize],
    placements: &[Placement],
) {
    let mut per_object: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for index in published {
        let placement = &placements[*index];
        per_object
            .entry(placement.object.as_str())
            .or_default()
            .push((placement.offset, *index));
    }
    let mut runs: Vec<Vec<usize>> = per_object
        .into_values()
        .map(|mut placed| {
            placed.sort_unstable();
            placed.into_iter().map(|(_, index)| index).collect()
        })
        .collect();
    runs.sort_by_key(|run| run.first().copied().unwrap_or_default());
    let delivered: Vec<usize> = runs.concat();
    assert_eq!(
        delivered.as_slice(),
        published,
        "case {case:?}: the records published on partition key {partition_key} are delivered in \
         the order the shard took them, got the delivered order {delivered:?} over {} object \
         run(s)",
        runs.len()
    );
}

/// The partition key the `key`th group of a case's records is published under. One key always
/// addresses one shard, whatever the source's shard count is, so the records of a group are a
/// subsequence of one shard.
fn partition_key(key: usize) -> String {
    format!("order-key-{key}")
}

/// The marker the `index`th record of the `ordinal`th case carries: unique over the suite, and
/// terminated, so no marker is a prefix of another and counting one counts that record alone.
fn marker(ordinal: usize, key: usize, index: usize) -> String {
    format!("order-{ordinal}/key-{key}/{index:04}|")
}

/// The bytes of one record of a record-order case: its marker, padded to the row's record size
/// with a filler byte no marker holds, so a row can cross its destination's `SizeInMBs`.
fn ordered_payload(ordinal: usize, key: usize, index: usize, filler: usize) -> Vec<u8> {
    let mut payload = marker(ordinal, key, index).into_bytes();
    payload.extend(std::iter::repeat_n(b'z', filler));
    payload
}

/// Create the source Kinesis data stream `name` with `shards` shards and answer its
/// `StreamARN`, once the stream reports `ACTIVE`.
pub(crate) async fn create_source_stream(emulator: &Emulator, name: &str, shards: u32) -> String {
    create_source(emulator, name, shards).await;
    await_source_active(emulator, name, name).await
}

/// Create the source Kinesis data stream `name` with `shards` shards, WITHOUT waiting for it to
/// serve its shards — what a client that chains its creates does.
pub(crate) async fn create_source(emulator: &Emulator, name: &str, shards: u32) {
    let (status, answered) = emulator
        .json_rpc(
            KINESIS_SERVICE,
            KINESIS_TARGET,
            "CreateStream",
            json!({ "StreamName": name, "ShardCount": shards }),
        )
        .await;
    assert_eq!(status, 200, "create source stream {name}: got {answered:?}");
}

/// `DescribeStreamSummary` on the source stream `name`, whatever lifecycle status it is in.
pub(crate) async fn describe_source(
    emulator: &Emulator,
    case: &str,
    name: &str,
) -> ::serde_json::Value {
    let (status, answered) = emulator
        .json_rpc(
            KINESIS_SERVICE,
            KINESIS_TARGET,
            "DescribeStreamSummary",
            json!({ "StreamName": name }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: describe source stream {name}: got {answered:?}"
    );
    answered
}

/// The `StreamARN` the source stream `name` reports right now, whatever lifecycle status it is
/// in — which is what a client binds a Firehose stream to.
pub(crate) async fn source_stream_arn(emulator: &Emulator, case: &str, name: &str) -> String {
    let answered = describe_source(emulator, case, name).await;
    answered["StreamDescriptionSummary"]["StreamARN"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the source stream reports its ARN, got {answered:?}")
        })
        .to_string()
}

/// Wait for the source stream `name` to report `ACTIVE` and answer the `StreamARN` it reports.
///
/// A poll loop on the STATUS rather than a wait for the clock-owed span, so how fast the host
/// runs cannot decide a case.
pub(crate) async fn await_source_active(emulator: &Emulator, case: &str, name: &str) -> String {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let answered = describe_source(emulator, case, name).await;
        if answered["StreamDescriptionSummary"]["StreamStatus"] == json!("ACTIVE") {
            return source_stream_arn(emulator, case, name).await;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the source stream {name} must reach ACTIVE, got {answered:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the `KinesisStreamAsSource` Firehose stream `name` reading `source_arn` and
/// delivering to `bucket`, and wait for it to be `ACTIVE`.
async fn create_source_fed_stream(
    emulator: &Emulator,
    name: &str,
    source_arn: &str,
    row: &SourceRow,
    bucket: &str,
) {
    let mut destination = json!({
        "RoleARN": ROLE_ARN,
        "BucketARN": format!("arn:aws:s3:::{bucket}"),
    });
    if let Some(prefix) = row.prefix {
        destination["Prefix"] = json!(prefix);
    }
    if let Some((size, interval)) = row.hints {
        destination["BufferingHints"] = json!({
            "SizeInMBs": size,
            "IntervalInSeconds": interval,
        });
    }
    let mut request = json!({
        "DeliveryStreamName": name,
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
            .is_some_and(|arn| arn.contains(name)),
        "case {:?}: the create answers the stream's ARN, got {answered:?}",
        row.case
    );
    await_active_source_fed_stream(emulator, row.case, name, source_arn).await;
}

/// Wait for the Firehose stream `name` to report `ACTIVE`, and assert that the source it
/// reports is `source_arn`, read back through the service's own describe.
///
/// A poll loop on the STATUS rather than a wait for the clock-owed transition span: the case
/// fails only if the stream is still not `ACTIVE` once [`DELIVERY_DEADLINE`] has passed, so
/// how fast the host runs cannot decide it.
pub(crate) async fn await_active_source_fed_stream(
    emulator: &Emulator,
    case: &str,
    name: &str,
    source_arn: &str,
) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": name }),
            )
            .await;
        let description = &described["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!("ACTIVE") {
            assert_eq!(
                description["Source"]["KinesisStreamSourceDescription"]["KinesisStreamARN"],
                json!(source_arn),
                "case {case:?}: {name} reports the source it reads, got {described:?}"
            );
            assert!(
                description["Source"]["KinesisStreamSourceDescription"]
                    ["DeliveryStartTimestamp"]
                    .is_number(),
                "case {case:?}: {name} reports when ITS retrieval starts, got {described:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must reach ACTIVE, got {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Write one record to the source stream, as a producer does.
pub(crate) async fn put_to_source(
    emulator: &Emulator,
    source: &str,
    partition_key: &str,
    data: &[u8],
) {
    let (status, answered) = emulator
        .json_rpc(
            KINESIS_SERVICE,
            KINESIS_TARGET,
            "PutRecord",
            json!({
                "StreamName": source,
                "PartitionKey": partition_key,
                "Data": base64(data),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "put to the source stream {source} in {REGION}: got {answered:?}"
    );
    assert!(
        answered["SequenceNumber"].is_string(),
        "the source stream answers the sequence number it stored the record under, \
         got {answered:?}"
    );
}

/// Every object under `prefix` of the destination bucket once each of `payloads` is in one of
/// them, keyed by object key — or a failure naming `case` once [`DELIVERY_DEADLINE`] has
/// passed.
pub(crate) async fn delivered_payloads(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    prefix: &str,
    payloads: &[Vec<u8>],
) -> BTreeMap<String, Vec<u8>> {
    delivered_payloads_within(emulator, case, bucket, prefix, payloads, DELIVERY_DEADLINE).await
}

/// [`delivered_payloads`], waiting `within` for the delivery rather than the suite's own
/// deadline — for a case whose rows wait out a buffering window of their own as well as the
/// consumption of the source.
async fn delivered_payloads_within(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    prefix: &str,
    payloads: &[Vec<u8>],
    within: Duration,
) -> BTreeMap<String, Vec<u8>> {
    let deadline = Instant::now() + within;
    loop {
        let bodies = bodies_under(emulator, bucket, prefix).await;
        let held: Vec<u8> = bodies.values().flatten().copied().collect();
        let missing: Vec<usize> = payloads
            .iter()
            .enumerate()
            .filter(|(_, payload)| occurrences(&held, payload) == 0)
            .map(|(index, _)| index)
            .collect();
        if missing.is_empty() {
            return bodies;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every record of the source under {bucket}/{prefix}, \
             missing the records at {missing:?} of {} (the keys delivered are {:?})",
            payloads.len(),
            bodies.keys().collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The bytes of every object under `prefix` of the destination bucket, keyed by key.
pub(crate) async fn bodies_under(
    emulator: &Emulator,
    bucket: &str,
    prefix: &str,
) -> BTreeMap<String, Vec<u8>> {
    let mut bodies = BTreeMap::new();
    for key in emulator.keys_under(bucket, prefix).await {
        let body = emulator.object(bucket, &key).await;
        bodies.insert(key, body);
    }
    bodies
}

/// The bytes of every object under `prefix` of the destination bucket, in key order, as one
/// slab — what the records of a source are counted in.
pub(crate) async fn concatenated(emulator: &Emulator, bucket: &str, prefix: &str) -> Vec<u8> {
    bodies_under(emulator, bucket, prefix)
        .await
        .values()
        .flatten()
        .copied()
        .collect()
}

/// Watch `prefix` of `bucket` over [`WATCH`], failing the moment what it holds stops matching
/// `expected`: each record there in exactly the number of copies given, and no byte beyond
/// them.
///
/// A POLL LOOP rather than one look after a wait, so a record delivered a second time — or a
/// record retrieved that this stream should never have retrieved — fails the case whenever in
/// the window it happens, and the window's length decides nothing but how long the watch
/// lasts.
pub(crate) async fn stays_delivered(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    prefix: &str,
    expected: &[(&[u8], usize)],
) {
    let until = Instant::now() + WATCH;
    loop {
        let held = concatenated(emulator, bucket, prefix).await;
        for (record, copies) in expected {
            let counted = occurrences(&held, record);
            assert_eq!(
                counted, *copies,
                "case {case:?}: {bucket}/{prefix} holds the record {:?} exactly {copies} \
                 time(s), got {counted}",
                summarised(record)
            );
        }
        let expected_bytes: usize = expected
            .iter()
            .map(|(record, copies)| record.len() * copies)
            .sum();
        assert_eq!(
            held.len(),
            expected_bytes,
            "case {case:?}: {bucket}/{prefix} holds the records expected of it and nothing \
             else, got {} bytes",
            held.len()
        );
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Each of `payloads`, expected EXACTLY once — what [`stays_delivered`] is given for a record
/// set a stream delivers in full.
pub(crate) fn once(payloads: &[Vec<u8>]) -> Vec<(&[u8], usize)> {
    payloads
        .iter()
        .map(|payload| (payload.as_slice(), 1))
        .collect()
}

/// A record shortened for an assertion message: the head that carries its marker, rather than
/// the filler that can run to tens of kilobytes behind it.
fn summarised(record: &[u8]) -> String {
    let head = record.len().min(40);
    let mut shown = String::from_utf8_lossy(&record[..head]).into_owned();
    if head < record.len() {
        shown.push_str("...");
    }
    shown
}

/// How many times `needle` occurs in `held` — the count of the one byte-window scan the
/// delivery suites share (`crate::destination_delivery::offsets`), so where a record is and how
/// many copies of it there are are read the same way.
pub(crate) fn occurrences(held: &[u8], needle: &[u8]) -> usize {
    offsets(held, needle).len()
}

/// The `index`th record of the `ordinal`th case: a marker no other record of the suite
/// carries, padded to the row's record size so a case can cross a size hint.
///
/// The filler is a repeated digit, which cannot hold another record's marker, so counting a
/// marker in the concatenated objects counts that record and nothing else.
pub(crate) fn payload(ordinal: usize, index: usize, filler: usize) -> Vec<u8> {
    let mut payload = format!("case-{ordinal}-record-{index}:").into_bytes();
    payload.extend(std::iter::repeat_n(b'7', filler));
    payload
}
