// SPDX-License-Identifier: CC0-1.0

//! What a destination asking to buffer for LONGER than this emulator holds data for gets:
//! the hint is improved on, not waited out.
//!
//! The rule under test: `BufferingHints` is documented as hints rather than as a contract —
//! "These options are treated as hints, and therefore Firehose might choose to use different
//! values when it is optimal" — so `IntervalInSeconds` states the LONGEST a producer asks for
//! its records to be held and not a wait the service owes. A buffer is therefore held for the
//! interval its destination asks for only while that interval is inside the span this
//! emulator holds data for, and a destination asking for more than that span has its records
//! delivered at the end of that span instead. Nothing is held forever: whatever a destination
//! configures, a record a put accepted becomes one object at that destination, so a producer
//! that writes through Firehose and verifies by reading the destination back finds its data
//! there. How a configuration is SPELLED makes no difference to that — the documented default
//! interval stated explicitly is the same configuration as the member left out — and neither
//! does a tag mutation on the stream, because `TagDeliveryStream` / `UntagDeliveryStream`
//! carry no delivery semantics.
//!
//! The parent suite [`crate::buffering_hints`] pins the other half of the same rule: an
//! interval INSIDE that span is honoured to the second, so a buffer configured with one holds
//! nothing until it has elapsed.
//!
//! No case here sleeps a window out or reads the wall clock. Every row's destination is
//! polled until it holds the object its records owe, and the rows whose interval is longer
//! than the span are required to hold NOTHING at the reading taken when the control row —
//! configured with the shortest interval its member admits — has already been delivered, which
//! is what says their records were held rather than written by the put itself.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::ROLE_ARN;
use crate::destination_delivery::Emulator;
use crate::parquet_serializer_parameters::await_active;

/// The bucket every row here delivers into; each row has its own `Prefix` under it, so a
/// listing is per case.
const BUCKET: &str = "hint-ceiling-bucket";

/// How long the delivery of a row whose interval is longer than the span this emulator holds
/// data for is waited for before the case fails.
///
/// Generous beside that span, so a loaded machine does not fail a case the clock would have
/// served; it bounds nothing the case measures.
const CEILING_DELIVERY_DEADLINE: Duration = Duration::from_secs(240);

/// How long the control row's delivery — the one asking for the shortest interval its member
/// admits — is waited for before the case fails.
const PROMPT_DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

/// How often a destination is polled while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Whether a row's records are expected at its destination promptly or only once the span
/// this emulator holds data for is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// Not held at all: the row asks for the shortest interval `IntervalInSeconds` admits.
    No,
    /// Held: the row asks for an interval longer than the span, so its records arrive at the
    /// end of that span rather than at the end of the interval it asked for.
    PastTheSpan,
}

/// What the stream's tags are made to do between the create and the put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tagging {
    /// Nothing: the stream is never tagged.
    Untouched,
    /// Tagged, re-tagged with another value, and then stripped of its last tag — the
    /// mutations `TagDeliveryStream` and `UntagDeliveryStream` perform, neither of which says
    /// anything about delivery.
    ChangedAndRemoved,
}

/// One case: how the destination's `BufferingHints` are spelled, what happens to the stream's
/// tags, and when its records are due.
struct Row {
    case: &'static str,
    /// `BufferingHints` as `(SizeInMBs, IntervalInSeconds)`; `None` leaves the member out
    /// altogether, which is the documented defaults.
    hints: Option<(i64, i64)>,
    /// How many records are put, one `PutRecord` each.
    records: usize,
    tagging: Tagging,
    held: Held,
}

/// The cases.
///
/// The rows differ along what the rule generalises over: the configured `IntervalInSeconds`
/// (the documented default 300 stated explicitly, the member left out so that default stands
/// unstated, the published maximum 900, and the published minimum 0 as the control), the
/// configured `SizeInMBs` beside it (the documented default 5, the published minimum 1 and the
/// published maximum 128 — none of which a handful of small records comes near), how many
/// records one buffer holds, and whether the stream's tags are changed and removed between the
/// create and the put. The published ranges are `SizeInMBs` 1..128 and `IntervalInSeconds`
/// 0..900 (`smithy_query(query="shape", shape="SizeInMBs,IntervalInSeconds")`), and the
/// defaults are `BufferingHints`' own ("The default value is 5", "The default value is 300").
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the documented defaults stated explicitly, with the tags changed and \
                   then removed",
            hints: Some((5, 300)),
            records: 1,
            tagging: Tagging::ChangedAndRemoved,
            held: Held::PastTheSpan,
        },
        Row {
            case: "the same defaults stated explicitly, with the stream never tagged",
            hints: Some((5, 300)),
            records: 1,
            tagging: Tagging::Untouched,
            held: Held::PastTheSpan,
        },
        Row {
            case: "no BufferingHints member at all, so the documented default interval \
                   stands unstated",
            hints: None,
            records: 2,
            tagging: Tagging::Untouched,
            held: Held::PastTheSpan,
        },
        Row {
            case: "the longest interval the member admits, under the largest size it admits",
            hints: Some((128, 900)),
            records: 3,
            tagging: Tagging::ChangedAndRemoved,
            held: Held::PastTheSpan,
        },
        Row {
            case: "the shortest interval the member admits, as the control",
            hints: Some((1, 0)),
            records: 1,
            tagging: Tagging::ChangedAndRemoved,
            held: Held::No,
        },
    ]
}

/// One row under test: its stream, where it delivers and what its producer put.
struct Leg {
    case: &'static str,
    stream: String,
    /// The `Prefix` its destination is configured with, which isolates its objects.
    prefix: String,
    /// The records put, in arrival order.
    records: Vec<Vec<u8>>,
    tagging: Tagging,
    held: Held,
}

#[tokio::test]
async fn an_interval_longer_than_the_span_this_emulator_holds_data_for_is_improved_on() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;

    // Every stream is created before any of them is awaited, so the suite spends ONE
    // clock-owed CREATING -> ACTIVE span rather than one per row.
    let mut legs: Vec<Leg> = Vec::new();
    for (ordinal, row) in rows().into_iter().enumerate() {
        let stream = format!("hint-ceiling-stream-{ordinal}");
        let prefix = format!("row-{ordinal}/");
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": &stream,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration":
                        destination(BUCKET, &prefix, row.hints),
                }),
            )
            .await;
        legs.push(Leg {
            case: row.case,
            stream,
            prefix,
            records: (0..row.records)
                .map(|index| format!("row-{ordinal}-record-{index}").into_bytes())
                .collect(),
            tagging: row.tagging,
            held: row.held,
        });
    }
    for leg in &legs {
        await_active(&emulator, leg.case, &leg.stream).await;
    }

    // The tag mutations, before anything is put: a stream whose tags were changed and then
    // removed is the same stream as far as its destination is concerned.
    for leg in legs.iter().filter(|leg| leg.tagging == Tagging::ChangedAndRemoved) {
        change_and_remove_tags(&emulator, leg.case, &leg.stream).await;
    }

    // The puts. A put is the only Firehose call a producer makes, so what follows is what the
    // emulator's own delivery clock does with the records these accepted.
    for leg in &legs {
        for record in &leg.records {
            let (status, answered) = emulator.put_record(&leg.stream, record).await;
            assert_eq!(
                status, 200,
                "case {:?}: the put must be accepted, got {answered:?}",
                leg.case
            );
        }
    }

    // The control row is delivered with no window to wait out; at the reading that says so,
    // every row asking for more than the span this emulator holds data for still holds
    // nothing, so its records are being HELD rather than written by the put.
    for leg in legs.iter().filter(|leg| leg.held == Held::No) {
        await_object(&emulator, leg, PROMPT_DELIVERY_DEADLINE).await;
    }
    for leg in legs.iter().filter(|leg| leg.held == Held::PastTheSpan) {
        let keys = emulator.keys_under(BUCKET, &leg.prefix).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a buffer whose interval is still running holds its records, \
             got {keys:?}",
            leg.case
        );
    }

    // And none of them is held forever: each is delivered once the span is spent, as one
    // object carrying every record of its buffer in arrival order.
    for leg in legs.iter().filter(|leg| leg.held == Held::PastTheSpan) {
        await_object(&emulator, leg, CEILING_DELIVERY_DEADLINE).await;
    }
}

/// The `ExtendedS3DestinationConfiguration` a row is created with: a location in `bucket`
/// under `prefix`, carrying `hints` when the row states them.
fn destination(bucket: &str, prefix: &str, hints: Option<(i64, i64)>) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "BucketARN": format!("arn:aws:s3:::{bucket}"),
        "Prefix": prefix,
        "CompressionFormat": "UNCOMPRESSED",
    });
    if let Some((size, interval)) = hints {
        config["BufferingHints"] = json!({
            "SizeInMBs": size,
            "IntervalInSeconds": interval,
        });
    }
    config
}

/// Tag `stream`, change that tag's value, and then remove the last tag it carries — a tag
/// mutation that says nothing about delivery.
async fn change_and_remove_tags(emulator: &Emulator, case: &str, stream: &str) {
    for value in ["first", "second"] {
        emulator
            .firehose_ok(
                "TagDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "Tags": [{ "Key": "stage", "Value": value }],
                }),
            )
            .await;
    }
    emulator
        .firehose_ok(
            "UntagDeliveryStream",
            json!({ "DeliveryStreamName": stream, "TagKeys": ["stage"] }),
        )
        .await;
    let listed = emulator
        .firehose_ok(
            "ListTagsForDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    let tags = listed["Tags"].as_array().cloned().unwrap_or_default();
    assert!(
        tags.is_empty(),
        "case {case:?}: the last tag was removed, so none is left, got {tags:?}"
    );
}

/// Wait until `leg`'s destination holds exactly one object carrying every record of its buffer
/// in arrival order, or fail naming the case once `deadline` has passed.
async fn await_object(emulator: &Emulator, leg: &Leg, deadline: Duration) {
    let expire = Instant::now() + deadline;
    let keys = loop {
        let keys = emulator.keys_under(BUCKET, &leg.prefix).await;
        if !keys.is_empty() {
            break keys;
        }
        assert!(
            Instant::now() < expire,
            "case {:?}: expected the records put to be delivered under {:?}, it holds nothing",
            leg.case,
            leg.prefix
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        keys.len(),
        1,
        "case {:?}: one flushed buffer is one object, got {keys:?}",
        leg.case
    );
    let body = emulator.object(BUCKET, &keys[0]).await;
    let expected = leg.records.concat();
    assert!(
        body == expected,
        "case {:?}: the object holds the buffer's {} record(s) in arrival order ({} bytes), \
         got {} bytes: {:?}",
        leg.case,
        leg.records.len(),
        expected.len(),
        body.len(),
        String::from_utf8_lossy(&body)
    );
}
