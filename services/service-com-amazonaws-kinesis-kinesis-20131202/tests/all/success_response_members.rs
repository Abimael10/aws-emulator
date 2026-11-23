// SPDX-License-Identifier: CC0-1.0

//! The MEMBER SET of this service's success responses, end to end over the wire:
//! `CreateStream` -> `DescribeStreamSummary`, and `PutRecord` / `PutRecords` across a
//! stream's encryption changes.
//!
//! RULE: a success response states a member exactly when the stream's stored state has a
//! value for it, and states it in the form the protocol carries that member's type in.
//! Three halves of that rule are checked here:
//!
//! * a maximum record size is part of EVERY stream's configuration — "The maximum record
//!   size of a single record in kibibyte (KiB) that you can write to, and read from a
//!   stream" (`StreamDescriptionSummary$MaxRecordSizeInKiB`) — so a summary always reports
//!   one: the value the create named, else the default record size a stream that names none
//!   falls back to, which is also the number every write to the stream is measured against;
//! * the `EncryptionType` a write's answer carries is OPTIONAL and reports the encryption
//!   the write applied — "you can verify that encryption is applied by inspecting the API
//!   response from PutRecord or PutRecords" (`StartStreamEncryption`) — so it is present,
//!   naming the type, only while server-side encryption is configured on the stream, and
//!   absent for a stream that encrypts nothing. The stream's own description reports the
//!   CONFIGURATION instead ("The encryption type used",
//!   `StreamDescriptionSummary$EncryptionType`), which every stream has, so that member
//!   keeps naming `NONE` and is asserted present here;
//! * a member whose modelled type is a `Timestamp` travels on this `awsJson1_1` service as
//!   an epoch-SECONDS number, and the instants this service keeps are kept to the
//!   millisecond, so every such number is written with at most three fractional digits —
//!   whichever operation reports it and whichever shape carries it.
//!
//! The recorded flows this was derived from describe ONE stream created without a maximum
//! record size, write to ONE unencrypted stream and read ONE record back, which are one row
//! of each table below. The rows differ along what the rule generalises over: whether the
//! create named a maximum and where in the published range it sat, the capacity mode of the
//! stream, the write path (one record or a batch), how many entries the batch carries, the
//! encryption in force when the write arrives (none, KMS, none again), and — for the
//! timestamp half — which operation reports an instant, which shape and member carries it,
//! how many of them one answer carries, and the sub-millisecond part of the instant itself.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the bodies
//! asserted here are the ones a client sees.
//!
//! The wire harness and the ARN builder are [`crate::stream_tags`]', reused rather than
//! copied.

use std::time::{Duration, UNIX_EPOCH};

use aws_smithy_types::date_time::Format;
use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    timestamp, DEFAULT_MAX_RECORD_SIZE_KI_B, ENCRYPTION_TYPE_KMS, ENCRYPTION_TYPE_NONE,
    ENCRYPTION_UPDATE_DELAY, SHARD_ITERATOR_TYPE_TRIM_HORIZON, STREAM_MODE_ON_DEMAND,
    STREAM_MODE_PROVISIONED,
};

use crate::shard_scaling::{shard_id, shard_map};
use crate::stream_tags::{
    error_code, past_activation, poll_until_active, stream_arn, Emulator, REGION,
};

/// The published bounds of `MaxRecordSizeInKiB`, in KiB
/// (`smithy_query(query="shape",shape="MaxRecordSizeInKiB")` -> range 1024..10240).
const MAX_RECORD_SIZE_BOUNDS: (i64, i64) = (1024, 10240);

/// How many bytes one KiB is, the unit `MaxRecordSizeInKiB` is expressed in.
const BYTES_PER_KI_B: usize = 1024;

/// The partition key every write of this suite carries. It is bounded by its OWN modelled
/// `@length` of 1..256 (`PutRecordInput$PartitionKey`) and is not charged against the
/// record-size ceiling, which bounds the data blob alone.
const PARTITION_KEY: &str = "pk";

/// A `KeyId` in one of the forms `StartStreamEncryptionInput$KeyId` documents: "You can
/// also use a master key owned by Kinesis Data Streams by specifying the alias
/// aws/kinesis".
const KEY_ID: &str = "alias/aws/kinesis";

/// How long a case waits before treating a stream whose encryption changed as ready.
fn past_encryption_update() -> Duration {
    ENCRYPTION_UPDATE_DELAY + Duration::from_millis(400)
}

/// The top-level member names a response carries, sorted, so a case can assert the whole
/// member set rather than one key at a time.
fn members(case: &str, response: &Value) -> Vec<String> {
    let object = response
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: expected a response body, got {response}"));
    let mut names: Vec<String> = object.keys().cloned().collect();
    names.sort();
    names
}

/// The `StreamDescriptionSummary` of the stream named `name`.
async fn summary(emulator: &Emulator, case: &str, name: &str) -> Value {
    emulator
        .ok(case, "DescribeStreamSummary", &json!({ "StreamName": name }))
        .await
        .get("StreamDescriptionSummary")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: DescribeStreamSummary must carry a summary"))
}

/// A `PutRecord` body for `arn` whose DATA blob is `data_bytes` long, with a partition key
/// on top of it: "Each data record can be up to 1 MiB in size by default" bounds the blob,
/// and `PutRecordInput$PartitionKey` carries its own `@length` of 1..256 beside it.
fn write_of(arn: &str, data_bytes: usize) -> Value {
    json!({
        "StreamARN": arn,
        "PartitionKey": PARTITION_KEY,
        "Data": aws_smithy_types::base64::encode(vec![b'x'; data_bytes]),
    })
}

/// One row of the maximum-record-size table: what the create names, and the maximum the
/// stream is then configured for.
struct SizeRow {
    label: &'static str,
    /// `StreamModeDetails$StreamMode` the create names.
    mode: &'static str,
    /// `MaxRecordSizeInKiB` the create names, `None` on a row whose create names none.
    created_with: Option<i64>,
    /// The maximum the stream carries from then on, in KiB.
    expected: i64,
}

/// The table the summary half of the rule is checked over.
fn size_rows() -> Vec<SizeRow> {
    vec![
        // The recorded shape: a provisioned stream created without a maximum, which carries
        // the default record size.
        SizeRow {
            label: "summary-created-without-a-maximum",
            mode: STREAM_MODE_PROVISIONED,
            created_with: None,
            expected: i64::from(DEFAULT_MAX_RECORD_SIZE_KI_B),
        },
        SizeRow {
            label: "summary-created-at-the-published-minimum",
            mode: STREAM_MODE_PROVISIONED,
            created_with: Some(MAX_RECORD_SIZE_BOUNDS.0),
            expected: MAX_RECORD_SIZE_BOUNDS.0,
        },
        SizeRow {
            label: "summary-created-between-the-published-bounds",
            mode: STREAM_MODE_PROVISIONED,
            created_with: Some(2 * MAX_RECORD_SIZE_BOUNDS.0),
            expected: 2 * MAX_RECORD_SIZE_BOUNDS.0,
        },
        SizeRow {
            label: "summary-created-at-the-published-ceiling",
            mode: STREAM_MODE_PROVISIONED,
            created_with: Some(MAX_RECORD_SIZE_BOUNDS.1),
            expected: MAX_RECORD_SIZE_BOUNDS.1,
        },
        // The shard count of an on-demand stream is the service's to manage; its maximum
        // record size is configured the same way a provisioned stream's is.
        SizeRow {
            label: "summary-on-demand-created-without-a-maximum",
            mode: STREAM_MODE_ON_DEMAND,
            created_with: None,
            expected: i64::from(DEFAULT_MAX_RECORD_SIZE_KI_B),
        },
    ]
}

/// Every stream reports the maximum record size it is configured for, and the number it
/// reports is the one its writes are measured against.
#[tokio::test]
async fn every_summary_reports_the_maximum_record_size_the_stream_is_configured_for() {
    let emulator = Emulator::new();
    for row in size_rows() {
        let mut body = json!({
            "StreamName": row.label,
            "StreamModeDetails": { "StreamMode": row.mode },
        });
        // "For the data streams with a provisioned mode, you must specify the number of
        // shards" (`CreateStream`).
        if row.mode == STREAM_MODE_PROVISIONED {
            body["ShardCount"] = json!(1);
        }
        if let Some(created_with) = row.created_with {
            body["MaxRecordSizeInKiB"] = json!(created_with);
        }
        emulator.ok(row.label, "CreateStream", &body).await;
    }
    for row in size_rows() {
        poll_until_active(&emulator, row.label, row.label).await;
    }

    for row in size_rows() {
        let case = row.label;
        let summary = summary(&emulator, case, case).await;
        let reported = summary.get("MaxRecordSizeInKiB").and_then(Value::as_i64);
        assert_eq!(
            reported,
            Some(row.expected),
            "case {case}: expected the summary to report a maximum record size of {} KiB, \
             got {reported:?} in {summary}",
            row.expected
        );
        // The stream's own description reports its encryption CONFIGURATION, which every
        // stream has: a stream nothing encrypted reports the type NONE.
        assert_eq!(
            summary.get("EncryptionType").and_then(Value::as_str),
            Some(ENCRYPTION_TYPE_NONE),
            "case {case}: expected the summary to report the stream's configured encryption \
             type, got {summary}"
        );

        // What the summary reports is the bound the stream enforces on the DATA blob: a
        // blob of exactly that many bytes is stored whatever partition key travels with it,
        // and one byte more fails that member's length.
        let maximum = usize::try_from(row.expected).unwrap_or(0) * BYTES_PER_KI_B;
        let arn = stream_arn(REGION, case);
        let answer = emulator
            .ok(case, "PutRecord", &write_of(&arn, maximum))
            .await;
        assert!(
            answer
                .get("SequenceNumber")
                .and_then(Value::as_str)
                .is_some(),
            "case {case}: a record whose data is exactly the reported maximum ({maximum} \
             bytes) is stored, got {answer}"
        );
        let (status, response) = emulator.call("PutRecord", &write_of(&arn, maximum + 1)).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: a data blob one byte past the reported maximum is refused, got \
             {status} with body {response}"
        );
    }
}

/// One row of the write table: the path a record arrives by and how many entries it
/// carries.
struct WriteRow {
    label: &'static str,
    operation: &'static str,
    /// How many records the write carries; a single write always carries one.
    entries: usize,
    /// The members the answer carries beside an `EncryptionType`, sorted.
    base_members: &'static [&'static str],
}

/// The table the write half of the rule is checked over.
fn write_rows() -> Vec<WriteRow> {
    vec![
        // The recorded shapes: one single write, and one batch.
        WriteRow {
            label: "one-record",
            operation: "PutRecord",
            entries: 1,
            base_members: &["SequenceNumber", "ShardId"],
        },
        WriteRow {
            label: "a-batch-of-one",
            operation: "PutRecords",
            entries: 1,
            base_members: &["FailedRecordCount", "Records"],
        },
        WriteRow {
            label: "a-batch-of-several",
            operation: "PutRecords",
            entries: 4,
            base_members: &["FailedRecordCount", "Records"],
        },
    ]
}

/// The body `row` writes to the stream named `name`.
fn write_body(row: &WriteRow, name: &str) -> Value {
    let data = aws_smithy_types::base64::encode("payload");
    if row.operation == "PutRecord" {
        return json!({
            "StreamName": name,
            "PartitionKey": PARTITION_KEY,
            "Data": data,
        });
    }
    let entries: Vec<Value> = (0..row.entries)
        .map(|index| {
            json!({
                "PartitionKey": format!("{PARTITION_KEY}-{index}"),
                "Data": data,
            })
        })
        .collect();
    json!({ "StreamName": name, "Records": entries })
}

/// Turn encryption on for `name` and wait for the change to fall due.
async fn encrypt(emulator: &Emulator, case: &str, name: &str) {
    emulator
        .ok(
            case,
            "StartStreamEncryption",
            &json!({
                "StreamName": name,
                "EncryptionType": ENCRYPTION_TYPE_KMS,
                "KeyId": KEY_ID,
            }),
        )
        .await;
    tokio::time::sleep(past_encryption_update()).await;
}

/// Turn encryption off for `name` and wait for the change to fall due.
async fn decrypt(emulator: &Emulator, case: &str, name: &str) {
    emulator
        .ok(
            case,
            "StopStreamEncryption",
            &json!({
                "StreamName": name,
                "EncryptionType": ENCRYPTION_TYPE_KMS,
                "KeyId": KEY_ID,
            }),
        )
        .await;
    tokio::time::sleep(past_encryption_update()).await;
}

/// A write names an encryption type exactly while the stream it writes to encrypts — on
/// both write paths, and across the stream's encryption being turned on and off again.
#[tokio::test]
async fn a_write_names_an_encryption_type_only_while_the_stream_encrypts() {
    let emulator = Emulator::new();
    let name = "write-response-members";
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
    tokio::time::sleep(past_activation()).await;

    // (what the phase did to the stream, the type a write must name in it)
    let phases: [(&str, Option<&str>); 3] = [
        ("nothing-encrypted", None),
        ("encryption-on", Some(ENCRYPTION_TYPE_KMS)),
        ("encryption-off-again", None),
    ];

    for (phase, expected_type) in phases {
        match phase {
            "encryption-on" => encrypt(&emulator, phase, name).await,
            "encryption-off-again" => decrypt(&emulator, phase, name).await,
            _ => {}
        }

        for row in write_rows() {
            let case = format!("{phase}/{}", row.label);
            let answer = emulator
                .ok(&case, row.operation, &write_body(&row, name))
                .await;

            let reported = answer.get("EncryptionType").and_then(Value::as_str);
            assert_eq!(
                reported, expected_type,
                "case {case}: expected the answer to name the encryption type \
                 {expected_type:?}, got {reported:?} in {answer}"
            );

            let mut expected: Vec<String> =
                row.base_members.iter().map(|m| (*m).to_string()).collect();
            if expected_type.is_some() {
                expected.push("EncryptionType".to_string());
            }
            expected.sort();
            let actual = members(&case, &answer);
            assert_eq!(
                actual, expected,
                "case {case}: expected exactly the members {expected:?}, got {actual:?} in \
                 {answer}"
            );
        }
    }
}

/// How many fractional digits an epoch-seconds number was written with, read off the RAW
/// bytes of the body: the resolution is in the DIGITS, which a parse into a float hides. A
/// number written with an exponent states no legible resolution at all, so it is reported
/// as past every one and the case naming it fails.
fn fractional_digits(number: &str) -> usize {
    if number.contains('e') || number.contains('E') {
        return usize::MAX;
    }
    number
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len())
}

/// Every modelled `Timestamp` member a RAW response body carries, as (member name, the
/// number exactly as it was serialised).
///
/// Found by the member NAME, which every instant of this model ends in `Timestamp`
/// (`StreamCreationTimestamp`, `ApproximateArrivalTimestamp`, `ConsumerCreationTimestamp`,
/// `ChannelCreationTimestamp`), so a shape this suite does not name is still covered.
fn wire_timestamps(body: &str) -> Vec<(String, String)> {
    const MARKER: &str = "Timestamp\":";
    let mut found: Vec<(String, String)> = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = body[from..].find(MARKER) {
        // Where the quote that closes the member's key sits, and so where its name ends.
        let key_end = from + relative + MARKER.len() - 2;
        let name_start = body[..key_end].rfind('"').map_or(key_end, |quote| quote + 1);
        let value_start = key_end + 2;
        let value_end = body[value_start..]
            .find([',', '}', ']'])
            .map_or(body.len(), |end| value_start + end);
        found.push((
            body[name_start..key_end].to_string(),
            body[value_start..value_end].trim().to_string(),
        ));
        from = value_end;
    }
    found
}

/// Every instant this service reports is written as an epoch-seconds number at millisecond
/// resolution, whichever operation reports it and whichever shape carries it.
#[tokio::test]
async fn every_reported_timestamp_is_written_at_millisecond_resolution() {
    let emulator = Emulator::new();
    let name = "timestamp-resolution";
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, name, name).await;
    let arn = stream_arn(REGION, name);

    let consumer = "timestamp-resolution-consumer";
    emulator
        .ok(
            name,
            "RegisterStreamConsumer",
            &json!({ "StreamARN": arn, "ConsumerName": consumer }),
        )
        .await;

    // Several records, so one row below reads an answer carrying ONE instant and another an
    // answer carrying several.
    let data = aws_smithy_types::base64::encode("payload");
    for index in 0..3 {
        emulator
            .ok(
                name,
                "PutRecord",
                &json!({
                    "StreamName": name,
                    "PartitionKey": format!("{PARTITION_KEY}-{index}"),
                    "Data": data,
                }),
            )
            .await;
    }

    let shards = shard_map(&emulator, name, name).await;
    let shard = shard_id(
        name,
        shards
            .first()
            .unwrap_or_else(|| panic!("case {name}: the stream must hold a shard to read")),
    );
    let answer = emulator
        .ok(
            name,
            "GetShardIterator",
            &json!({
                "StreamName": name,
                "ShardId": shard,
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
        )
        .await;
    let iterator = answer
        .get("ShardIterator")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {name}: GetShardIterator must answer an iterator"))
        .to_string();

    // (the operation, its request, how many instants its answer carries at least). The rows
    // differ along the operation, the output shape the instant sits in, whether it is a
    // member of the top-level shape or of one inside a list, and how many of them one answer
    // carries.
    let rows: Vec<(&str, Value, usize)> = vec![
        ("DescribeStreamSummary", json!({ "StreamName": name }), 1),
        ("DescribeStream", json!({ "StreamName": name }), 1),
        ("ListStreams", json!({}), 1),
        (
            "DescribeStreamConsumer",
            json!({ "StreamARN": arn, "ConsumerName": consumer }),
            1,
        ),
        ("ListStreamConsumers", json!({ "StreamARN": arn }), 1),
        ("GetRecords", json!({ "ShardIterator": iterator }), 3),
    ];

    for (operation, body, at_least) in rows {
        let (status, _, raw) = emulator.call_raw(operation, &body).await;
        let text = String::from_utf8_lossy(&raw).to_string();
        assert_eq!(
            status, 200,
            "case {operation}: expected it to answer 200, got {status} with body {text}"
        );
        let reported = wire_timestamps(&text);
        assert!(
            reported.len() >= at_least,
            "case {operation}: expected at least {at_least} reported instant(s), got \
             {reported:?} in {text}"
        );
        for (member, number) in &reported {
            assert!(
                number.parse::<f64>().is_ok(),
                "case {operation}/{member}: expected an epoch-seconds number, got {number:?} \
                 in {text}"
            );
            assert!(
                fractional_digits(number) <= 3,
                "case {operation}/{member}: expected an epoch-seconds number at millisecond \
                 resolution, got {number:?}"
            );
        }
    }
}

/// The one conversion every reported instant goes through truncates a stored instant to
/// whole milliseconds, over a table of sub-millisecond remainders including both bounds.
#[test]
fn a_stored_instant_is_reported_truncated_to_whole_milliseconds() {
    /// The second the rows sit in, fixed so no case depends on the host's clock.
    const WHOLE_SECONDS: i64 = 1_700_000_000;

    // (case, nanoseconds past that whole second, the millisecond of it the instant reports)
    let rows: [(&str, u64, i64); 6] = [
        ("a whole second", 0, 0),
        ("one nanosecond past it", 1, 0),
        ("one nanosecond short of a millisecond", 999_999, 0),
        ("exactly one millisecond", 1_000_000, 1),
        ("a millisecond and a half", 1_500_000, 1),
        ("the last millisecond of the second", 999_999_999, 999),
    ];

    for (case, nanos, millis) in rows {
        let at = UNIX_EPOCH
            + Duration::from_secs(u64::try_from(WHOLE_SECONDS).unwrap_or_default())
            + Duration::from_nanos(nanos);
        let reported = timestamp(at);
        let actual = reported.to_millis().ok();
        assert_eq!(
            actual,
            Some(WHOLE_SECONDS * 1_000 + millis),
            "case {case}: expected the instant reported at millisecond {millis} of the \
             second, got {actual:?}"
        );
        let written = reported.fmt(Format::EpochSeconds).unwrap_or_default();
        assert!(
            fractional_digits(&written) <= 3,
            "case {case}: expected an epoch-seconds number at millisecond resolution, got \
             {written:?}"
        );
    }
}
