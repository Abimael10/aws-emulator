// SPDX-License-Identifier: CC0-1.0

//! Several delivery streams of one account, side by side, each delivering to its own Amazon
//! S3 location.
//!
//! The rule under test: a stream's buffered records are delivered to the Amazon S3 location
//! ITS OWN destination configuration names — "Firehose buffers records before delivering
//! them to the destination" (`PutRecord`) — so every stream that accepted data has an object
//! readable under its own prefix, whatever the sibling streams of the same account hold and
//! whatever data processing its destination enables. A record a put REFUSED is never
//! delivered, and that absence is readable precisely because the siblings' prefixes do
//! receive their objects: an empty prefix beside delivered ones is a destination that
//! received nothing rather than one whose delivery the clock has not reached yet.
//!
//! `cloudwatch_logs_pipeline_delivery` pins what ONE destination's pipeline leaves of a
//! record; this suite pins that the delivery itself is per-stream, which is what makes a
//! stream's delivery — and its absence — observable while other streams are being written
//! to. The rows differ along what the rule generalises over: which parts of the CloudWatch
//! Logs pipeline the destination enables (none, both but disabled, decompression alone, the
//! whole pipeline, extraction alone), which producer feeds the stream, which ingestion
//! operation the records arrive through, and how many records one put carries.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cloudwatch_logs_pipeline_delivery::{
    as_cloudwatch_logs, decompression, envelope, gzip, message_extraction, processing,
};
// The refusal a direct put to a CloudWatch-Logs-only stream reads, stated once by the suite
// that pins the gate itself.
use crate::cloudwatch_logs_source_gate::expected_refusal;
use crate::delivery_stream_lifecycle::{error_code, error_message, BUCKET_ARN, ROLE_ARN};
use crate::destination_delivery::{contains, Emulator};
// The poll for a stream reaching the status a put is accepted in, as the suite that first
// needed it spells it — reused rather than re-spelled here.
use crate::parquet_serializer_parameters::await_active;

/// The bucket every stream here delivers into — the one [`BUCKET_ARN`] names.
const BUCKET: &str = "demo-bucket";

/// How long a delivery is waited for before a case fails: far longer than the emulator's own
/// delivery span, so a loaded machine does not fail a case the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How often the destination is read while waiting for a delivery.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which producer a row's records arrive from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Producer {
    /// A caller of the ingestion API: "`DirectPut`: Provider applications access the Firehose
    /// stream directly" (`CreateDeliveryStreamInput$DeliveryStreamType`).
    Api,
    /// CloudWatch Logs, forwarding a subscription filter's log events.
    CloudWatchLogs,
}

/// Which ingestion operation a row's records arrive through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Put {
    One,
    Batch,
}

/// One row: a stream of the shared emulator, the records it is fed, and what its own prefix
/// then holds.
struct Row {
    case: &'static str,
    /// The stream's name, which also names the prefix its destination delivers under.
    stream: &'static str,
    /// The destination's `ProcessingConfiguration`, or `None` for one that configures none.
    processing: Option<Value>,
    producer: Producer,
    put: Put,
    /// The records the producer puts, in order. Empty for a stream nothing is accepted on.
    records: Vec<Vec<u8>>,
    /// The bytes every object under this stream's prefix holds, concatenated in key order.
    /// Empty for a stream whose prefix must stay empty.
    delivered: Vec<u8>,
    /// Whether an ordinary API producer's put is refused on this destination — true exactly
    /// where it enables a processor of the CloudWatch Logs pipeline.
    gated: bool,
}

fn rows() -> Vec<Row> {
    let one = envelope("DATA_MESSAGE", &["ERROR disk full"]);
    let two = envelope("DATA_MESSAGE", &["WARN disk filling", "INFO log rotated"]);
    vec![
        Row {
            case: "a destination configuring no processing, fed by a direct put",
            stream: "plain-direct",
            processing: None,
            producer: Producer::Api,
            put: Put::Batch,
            records: vec![b"first line\n".to_vec(), b"second line\n".to_vec()],
            delivered: b"first line\nsecond line\n".to_vec(),
            gated: false,
        },
        Row {
            case: "a destination configuring no processing, fed by CloudWatch Logs",
            stream: "plain-forwarded",
            processing: None,
            producer: Producer::CloudWatchLogs,
            put: Put::One,
            records: vec![gzip(&one)],
            delivered: gzip(&one),
            gated: false,
        },
        Row {
            case: "the whole pipeline configured but disabled keeps the frames",
            stream: "disabled-pipeline",
            processing: Some(processing(
                false,
                vec![decompression(), message_extraction()],
            )),
            producer: Producer::Api,
            put: Put::One,
            records: vec![gzip(&two)],
            delivered: gzip(&two),
            gated: false,
        },
        Row {
            case: "decompression alone delivers the envelope documents",
            stream: "gated-inflated",
            processing: Some(processing(true, vec![decompression()])),
            producer: Producer::CloudWatchLogs,
            put: Put::Batch,
            records: vec![gzip(&one), gzip(&two)],
            delivered: format!("{one}{two}").into_bytes(),
            gated: true,
        },
        Row {
            case: "the whole pipeline delivers each event's message on its own line",
            stream: "gated-extracted",
            processing: Some(processing(
                true,
                vec![decompression(), message_extraction()],
            )),
            producer: Producer::CloudWatchLogs,
            put: Put::One,
            records: vec![gzip(&two)],
            delivered: b"WARN disk filling\nINFO log rotated\n".to_vec(),
            gated: true,
        },
        Row {
            case: "a gated destination fed only by a refused put receives nothing",
            stream: "gated-refused",
            processing: Some(processing(true, vec![message_extraction()])),
            producer: Producer::Api,
            put: Put::One,
            records: Vec::new(),
            delivered: Vec::new(),
            gated: true,
        },
    ]
}

/// Where `stream` delivers: its own prefix of the one shared bucket.
fn prefix(stream: &str) -> String {
    format!("{stream}/")
}

/// The record an ordinary API producer offers every destination — distinct per stream, so an
/// object holding it names the stream whose refusal did not hold.
fn probe(stream: &str) -> Vec<u8> {
    format!("a direct put at {stream}").into_bytes()
}

/// Put `row`'s records, as the ingestion operation it names.
async fn put_records(emulator: &Emulator, row: &Row) {
    let case = row.case;
    match row.put {
        Put::One => {
            for record in &row.records {
                let (status, answered) = emulator.put_record(row.stream, record).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the put must be taken, got {status} {answered}"
                );
            }
        }
        Put::Batch => {
            let (status, answered) = emulator.put_batch(row.stream, &row.records).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the batch must be taken, got {status} {answered}"
            );
            assert_eq!(
                answered["FailedPutCount"],
                json!(0),
                "case {case:?}: every record of the batch was taken, got {answered}"
            );
        }
    }
}

/// Every object under `prefix`, concatenated in key order, once that concatenation is
/// `expected` — or whatever it last was once [`DELIVERY_DEADLINE`] has passed, which the
/// caller's assertion then reports.
///
/// Polling for the WHOLE expected body rather than for the first object is what keeps a row
/// whose records reach the destination in more than one flush from being read half-written.
async fn delivered(emulator: &Emulator, prefix: &str, expected: &[u8]) -> Vec<u8> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let mut keys = emulator.keys_under(BUCKET, prefix).await;
        keys.sort();
        let mut body = Vec::new();
        for key in &keys {
            body.extend_from_slice(&emulator.object(BUCKET, key).await);
        }
        if body == expected || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn each_stream_delivers_what_it_accepted_under_its_own_prefix() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    let rows = rows();

    // Every stream of the account, created together: they differ in their destination's
    // processing and in nothing else.
    for row in &rows {
        let mut destination = json!({
            "RoleARN": ROLE_ARN,
            "BucketARN": BUCKET_ARN,
            "Prefix": prefix(row.stream),
            "ErrorOutputPrefix": format!("failed/{}/", row.stream),
            // The shortest buffering the hints admit, so one put is one object.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        });
        if let Some(configured) = &row.processing {
            destination["ProcessingConfiguration"] = configured.clone();
        }
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": row.stream,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration": destination,
                }),
            )
            .await;
    }
    for row in &rows {
        await_active(&emulator, row.case, row.stream).await;
    }

    // An ordinary API producer offers every gated destination one record: refused with the
    // modelled `InvalidSourceException`, so nothing of it is ever buffered or delivered.
    for row in rows.iter().filter(|row| row.gated) {
        let case = row.case;
        let (status, answered) = emulator.put_record(row.stream, &probe(row.stream)).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidSourceException"),
            "case {case:?}: an API put must be refused, got {status} {answered}"
        );
        let said = error_message(&answered);
        let expected = expected_refusal(row.stream);
        assert_eq!(
            said, expected,
            "case {case:?}: expected {expected:?}, got {said:?}"
        );
    }

    // Each row's own records, from the producer its destination takes them from.
    for row in &rows {
        match row.producer {
            Producer::Api => put_records(&emulator, row).await,
            Producer::CloudWatchLogs => {
                let _caller = as_cloudwatch_logs(&emulator);
                put_records(&emulator, row).await;
            }
        }
    }

    // Every stream that accepted data has its own object, holding what its own destination's
    // processing left of its own records and nothing of a sibling's.
    for row in rows.iter().filter(|row| !row.delivered.is_empty()) {
        let case = row.case;
        let body = delivered(&emulator, &prefix(row.stream), &row.delivered).await;
        assert_eq!(
            String::from_utf8_lossy(&body),
            String::from_utf8_lossy(&row.delivered),
            "case {case:?}: expected {:?} under {:?}, got {:?}",
            String::from_utf8_lossy(&row.delivered),
            prefix(row.stream),
            String::from_utf8_lossy(&body)
        );
    }

    // The siblings have delivered, so a prefix that is still empty is a destination that
    // received nothing — which is how the refusal above is told apart from delivery lag.
    for row in rows.iter().filter(|row| row.delivered.is_empty()) {
        let keys = emulator.keys_under(BUCKET, &prefix(row.stream)).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a stream whose only put was refused delivers nothing, got {keys:?}",
            row.case
        );
    }

    // And no object of the bucket — under any prefix, error output included — holds a record
    // a put refused.
    let mut keys = emulator.keys_under(BUCKET, "").await;
    keys.sort();
    for key in &keys {
        let body = emulator.object(BUCKET, key).await;
        for row in rows.iter().filter(|row| row.gated) {
            assert!(
                !contains(&body, &probe(row.stream)),
                "case {:?}: a refused record is never delivered, got it at {key:?}",
                row.case
            );
        }
    }
}
