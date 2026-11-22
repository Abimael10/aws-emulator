// SPDX-License-Identifier: CC0-1.0

//! The ACTIVE PARTITIONS a dynamically partitioned destination may write at once.
//!
//! The rule under test: a Firehose stream holds at most the published quota of 500 active
//! partitions, so a flush writes an object for each of the first 500 distinct partition-key
//! sets its buffer's records derive, in the records' arrival order, and a record whose keys
//! would open a partition beyond that quota is NOT written under the data prefix — it goes to
//! the destination's `ErrorOutputPrefix` as an error document naming
//! `DynamicPartitioning.ActivePartitionsLimitExceeded` and carrying the record's own bytes as
//! `rawData`. A record of a partition already open is not charged against the quota again, and
//! ingestion is unaffected: `PutRecordBatch` accepts an over-quota record like any other,
//! because the quota belongs to the delivery and not to the put.
//!
//! Driven over the wire on the shared harness of the parent suite: the Firehose calls as
//! `POST /` with `X-Amz-Target`, the destination reads as the path-style Amazon S3 requests a
//! client sends.
//!
//! WHEN the flush happens is not what these cases are about, so each stream is created with a
//! long buffering interval — which is what puts every record of a case in ONE buffer, whatever
//! order the puts and the clock take — and the buffer is then made due by an
//! `UpdateDestination` that asks for the shortest interval `IntervalInSeconds` admits.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{error_documents, raw_data};
use crate::delivery_stream_lifecycle::ROLE_ARN;
use crate::destination_delivery::Emulator;

/// The most partitions one delivery stream may have active at a time: a published Firehose
/// quota ("active partitions per delivery stream"), which is the number a client designing its
/// partition keys has to design around.
const ACTIVE_PARTITION_QUOTA: usize = 500;

/// The `errorCode` the error document of a record beyond that quota names.
const QUOTA_EXCEEDED_CODE: &str = "DynamicPartitioning.ActivePartitionsLimitExceeded";

/// The `errorMessage` it carries beside that code.
const QUOTA_EXCEEDED_MESSAGE: &str = "active-partition-count-exceeded";

/// The `Prefix` every case configures: one partition directory per derived key.
const DATA_PREFIX: &str = "data/";

/// The `ErrorOutputPrefix` every case configures, before expansion.
const ERROR_PREFIX: &str = "errors/";

/// How long a case waits for the destination to hold what it expects, and the same generous
/// room for the clock-owed `CREATING` -> `ACTIVE` span. It bounds nothing a case measures: it
/// only stops a wait that would never end.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often the destination is polled while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One quota case: how many distinct partitions the one buffer creates, and how many records
/// fall in each of them.
struct Row {
    case: &'static str,
    /// The distinct partition-key values the buffer's records derive, in arrival order.
    partitions: usize,
    /// How many records fall in each of those partitions.
    records_per_partition: usize,
}

/// The quota cases.
///
/// The rows differ along what the rule generalises over: how many partitions the buffer
/// creates relative to the quota (well inside it, exactly at it, and past it — the published
/// bound and one past it), and how many records each partition takes, so a partition that is
/// already open is seen not to be charged again.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handful of partitions, several records each",
            partitions: 4,
            records_per_partition: 3,
        },
        Row {
            case: "exactly the quota, one record each",
            partitions: ACTIVE_PARTITION_QUOTA,
            records_per_partition: 1,
        },
        Row {
            case: "three partitions past the quota",
            partitions: ACTIVE_PARTITION_QUOTA + 3,
            records_per_partition: 1,
        },
    ]
}

impl Row {
    /// The partition value of the `index`th partition, the value the record carries and the
    /// prefix directory it lands under both name.
    fn slot(index: usize) -> String {
        format!("slot-{index:04}")
    }

    /// The records the producer puts, in arrival order: every record of a partition before the
    /// first record of the next, so the partitions are created in `slot` order.
    fn records(&self) -> Vec<Vec<u8>> {
        let mut records = Vec::new();
        for partition in 0..self.partitions {
            for ordinal in 0..self.records_per_partition {
                records.push(
                    format!(
                        "{{\"slot\":\"{}\",\"n\":{ordinal}}}",
                        Self::slot(partition)
                    )
                    .into_bytes(),
                );
            }
        }
        records
    }

    /// The partitions this buffer's flush delivers: the first ones up to the quota.
    fn delivered_partitions(&self) -> BTreeSet<String> {
        (0..self.partitions.min(ACTIVE_PARTITION_QUOTA))
            .map(Self::slot)
            .collect()
    }

    /// The records diverted to the error output: every record of a partition past the quota.
    fn over_quota_records(&self) -> Vec<String> {
        (ACTIVE_PARTITION_QUOTA..self.partitions)
            .flat_map(|partition| {
                (0..self.records_per_partition).map(move |ordinal| {
                    format!(
                        "{{\"slot\":\"{}\",\"n\":{ordinal}}}",
                        Self::slot(partition)
                    )
                })
            })
            .collect()
    }
}

/// The destination of a quota case: dynamic partitioning on, the partition key derived by a
/// `MetadataExtraction` processor, and a buffer held long enough that every put of the case
/// lands in it.
fn destination(bucket: &str) -> Value {
    json!({
        "RoleARN": ROLE_ARN,
        "BucketARN": format!("arn:aws:s3:::{bucket}"),
        "Prefix": format!("{DATA_PREFIX}slot=!{{partitionKeyFromQuery:slot}}/"),
        "ErrorOutputPrefix": format!("{ERROR_PREFIX}!{{firehose:error-output-type}}/"),
        "BufferingHints": { "SizeInMBs": 128, "IntervalInSeconds": 900 },
        "DynamicPartitioningConfiguration": { "Enabled": true },
        "ProcessingConfiguration": {
            "Enabled": true,
            "Processors": [{
                "Type": "MetadataExtraction",
                "Parameters": [
                    { "ParameterName": "MetadataExtractionQuery", "ParameterValue": "{slot:.slot}" },
                    { "ParameterName": "JsonParsingEngine", "ParameterValue": "JQ-1.6" },
                ],
            }],
        },
    })
}

/// Wait until Firehose reports `stream` `ACTIVE`, which is when it takes a put: the create
/// leaves it `CREATING` and the clock settles it. The wait is for that STATUS and not for a
/// span, so a slow or a fast host reads the same.
async fn await_active(emulator: &Emulator, stream: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].as_str();
        if status == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {stream:?}: the stream must become ACTIVE within {DELIVERY_DEADLINE:?}, it is \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `(VersionId, DestinationId)` of a stream's sole destination.
async fn version_and_destination(emulator: &Emulator, stream: &str) -> (String, String) {
    let description = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await["DeliveryStreamDescription"]
        .clone();
    let version = description["VersionId"].as_str().unwrap_or_default();
    let destination_id = description["Destinations"][0]["DestinationId"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !version.is_empty() && !destination_id.is_empty(),
        "describe {stream}: expected a version and a destination id, got {description:?}"
    );
    (version.to_string(), destination_id.to_string())
}

/// Make the stream's held buffer due, by updating its destination to the shortest buffering
/// interval `IntervalInSeconds` admits. Everything else the destination configures is
/// retained: "If any of the parameters are not specified in the call, the existing values are
/// retained" (`UpdateDestination`).
async fn flush_now(emulator: &Emulator, stream: &str) {
    let (version, destination_id) = version_and_destination(emulator, stream).await;
    emulator
        .firehose_ok(
            "UpdateDestination",
            json!({
                "DeliveryStreamName": stream,
                "CurrentDeliveryStreamVersionId": version,
                "DestinationId": destination_id,
                "ExtendedS3DestinationUpdate": {
                    "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                },
            }),
        )
        .await;
}

/// The partition directory a delivered key sits in — `slot=<value>` out of
/// `data/slot=<value>/<object name>` — or `None` for a key under no such directory.
fn partition_of(key: &str) -> Option<String> {
    key.strip_prefix(DATA_PREFIX)?
        .split('/')
        .next()?
        .strip_prefix("slot=")
        .map(str::to_string)
}

/// Every partition the data prefix holds an object under, with those objects' keys.
///
/// The listing is scoped to the data prefix and the partition directories are read off the
/// keys, because a partition directory carries the `=` of the `Prefix` expression it came from
/// and a prefix read through the query string is sent as written.
async fn delivered_partitions(emulator: &Emulator, bucket: &str) -> BTreeMap<String, Vec<String>> {
    let mut held: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in emulator.keys_under(bucket, DATA_PREFIX).await {
        let partition = partition_of(&key)
            .unwrap_or_else(|| panic!("a delivered key sits in a partition directory: {key:?}"));
        held.entry(partition).or_default().push(key);
    }
    held
}

#[tokio::test]
async fn the_partitions_past_the_active_partition_quota_go_to_the_error_output() {
    let emulator = Emulator::new();

    // Every stream is created, and every record put, before any flush is awaited: the suite
    // spends ONE clock-owed CREATING -> ACTIVE span rather than one per row, and each case's
    // whole buffer is held by the long interval its destination configures.
    let legs: Vec<(String, String, Row)> = rows()
        .into_iter()
        .enumerate()
        .map(|(ordinal, row)| {
            (
                format!("quota-bucket-{ordinal}"),
                format!("quota-stream-{ordinal}"),
                row,
            )
        })
        .collect();
    for (bucket, stream, _) in &legs {
        emulator.create_bucket(bucket).await;
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration": destination(bucket),
                }),
            )
            .await;
    }
    for (_, stream, _) in &legs {
        await_active(&emulator, stream).await;
    }

    for (_, stream, row) in &legs {
        let records = row.records();
        // `PutRecordBatch` takes at most the 500 records its modelled `Records` list admits,
        // so a case creating more partitions than that puts several batches into the one
        // buffer.
        for batch in records.chunks(500) {
            let (status, put) = emulator.put_batch(stream, batch).await;
            assert_eq!(status, 200, "case {:?}: the put got {put:?}", row.case);
            assert_eq!(
                put["FailedPutCount"],
                json!(0),
                "case {:?}: an over-quota partition is still ACCEPTED at ingestion, got \
                 {put:?}",
                row.case
            );
        }
        // The premise of every assertion below: the whole case is one buffer, which the
        // destination's own interval is what holds.
        let buffered = emulator.buffered(stream).len();
        assert_eq!(
            buffered,
            records.len(),
            "case {:?}: the configured interval holds every record of the case in one \
             buffer, got {buffered} of {} still buffered",
            row.case,
            records.len()
        );
    }

    for (_, stream, _) in &legs {
        flush_now(&emulator, stream).await;
    }

    for (bucket, _, row) in &legs {
        let wanted = row.delivered_partitions();
        let over_quota = row.over_quota_records();
        // One object per delivered partition, plus one error-output object for the records of
        // the partitions past the quota: the whole flush is written at once, so seeing that
        // many objects is seeing the delivery settled.
        let expected_objects = wanted.len() + usize::from(!over_quota.is_empty());
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let keys = emulator.keys_under(bucket, "").await;
            if keys.len() >= expected_objects {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected {expected_objects} delivered object(s) in {bucket}, got \
                 {} so far",
                row.case,
                keys.len()
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // Exactly the partitions inside the quota are delivered, one object each.
        let held = delivered_partitions(&emulator, bucket).await;
        let delivered: BTreeSet<String> = held.keys().cloned().collect();
        assert_eq!(
            delivered,
            wanted,
            "case {:?}: expected exactly the {} partitions inside the quota under {DATA_PREFIX:?}, \
             got {} of them ({:?} missing, {:?} unexpected)",
            row.case,
            wanted.len(),
            delivered.len(),
            wanted.difference(&delivered).collect::<Vec<_>>(),
            delivered.difference(&wanted).collect::<Vec<_>>()
        );
        for (partition, objects) in &held {
            assert_eq!(
                objects.len(),
                1,
                "case {:?}: one flush writes one object per partition, got {objects:?} under \
                 {partition:?}",
                row.case
            );
        }

        // The records of an open partition are all in its object, however many of them fell
        // in it: a partition already open takes every later record of its own keys.
        let first = Row::slot(0);
        let key = held
            .get(&first)
            .and_then(|keys| keys.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: {first:?} is a delivered partition", row.case));
        let body = emulator.object(bucket, &key).await;
        let carried_records = String::from_utf8_lossy(&body).into_owned();
        for ordinal in 0..row.records_per_partition {
            let record = format!("{{\"slot\":\"{first}\",\"n\":{ordinal}}}");
            assert!(
                carried_records.contains(&record),
                "case {:?}: expected {record:?} in the object of {first:?}, got \
                 {carried_records:?}",
                row.case
            );
        }

        // And the error output: one document per record of a partition past the quota, each
        // naming the quota and carrying the record's own bytes — and nothing at all where
        // every partition fitted.
        let error_keys = emulator.keys_under(bucket, ERROR_PREFIX).await;
        if over_quota.is_empty() {
            assert!(
                error_keys.is_empty(),
                "case {:?}: every partition fitted the quota, so nothing is diverted, got \
                 {error_keys:?}",
                row.case
            );
            continue;
        }
        assert_eq!(
            error_keys.len(),
            1,
            "case {:?}: the diverted records of one flush are one error-output object, got \
             {error_keys:?}",
            row.case
        );
        let key = error_keys[0].clone();
        let wanted_prefix = format!("{ERROR_PREFIX}{QUOTA_EXCEEDED_MESSAGE}/");
        assert!(
            key.starts_with(&wanted_prefix),
            "case {:?}: expected the diverted records under {wanted_prefix:?}, got {key:?}",
            row.case
        );
        let documents = error_documents(&emulator.object(bucket, &key).await);
        let carried: Vec<String> = documents.iter().map(raw_data).collect();
        assert_eq!(
            carried, over_quota,
            "case {:?}: the error output carries each over-quota record's own bytes, got \
             {carried:?}",
            row.case
        );
        for document in &documents {
            assert_eq!(
                document["errorCode"],
                json!(QUOTA_EXCEEDED_CODE),
                "case {:?}: the error document names the quota that rejected the record, got \
                 {document:?}",
                row.case
            );
            assert_eq!(
                document["errorMessage"],
                json!(QUOTA_EXCEEDED_MESSAGE),
                "case {:?}: the error document states what the quota was, got {document:?}",
                row.case
            );
            assert!(
                document["attemptsMade"].as_u64().is_some_and(|made| made >= 1)
                    && document["arrivalTimestamp"].as_u64().is_some()
                    && document["attemptEndingTimestamp"].as_u64().is_some(),
                "case {:?}: the error document carries the attempt it reports, got {document:?}",
                row.case
            );
        }
    }
}
