// SPDX-License-Identifier: CC0-1.0

//! Delivery to an APACHE ICEBERG destination, driven over the wire with Firehose, Amazon S3
//! and Glue mounted on one shared engine.
//!
//! The rules under test:
//!
//! * A stream whose destination is an Iceberg table COMMITS the records of each flush to that
//!   table: a Parquet data object under the table's own `data/` directory, the manifest and
//!   manifest list the snapshot records it in under `metadata/`, a new metadata file whose
//!   snapshot summary counts the records it added, and the catalog's `metadata_location` moved
//!   to that file. A record whose value the destination table's column does not hold is not
//!   dropped: it is written under the destination's error output with
//!   `Iceberg.MalformedFieldWithinRecord`, its own bytes in `rawData`, while its well-formed
//!   siblings in the same buffer still commit. An Iceberg `timestamp` column takes the
//!   ISO-8601 spellings (with and without a zone offset) and epoch milliseconds; the
//!   space-separated SQL spelling is not an ISO-8601 timestamp.
//! * A destination table the catalog holds no Iceberg metadata for has nothing to commit onto,
//!   so the stream accepts its records and writes NOTHING under that table's prefix — and
//!   never retro-fits a `metadata_location` onto the row.
//! * `DescribeDeliveryStream` reports an Iceberg destination's `BufferingHints` with the
//!   documented defaults materialised when the create omitted them.
//! * `StartDeliveryStreamEncryption` on a stream whose `DeliveryStreamType` is not `DirectPut`
//!   is refused with `InvalidArgumentException` and the service's own sentence.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::{error_code, error_message, REGION, ROLE_ARN};
use crate::destination_delivery::{base64, Emulator};
use crate::format_conversion_delivery::{create_database, create_table};

/// How long a commit is waited for before a case fails: far longer than the emulator's
/// delivery span, so a slow machine does not fail a case the clock would have served.
const COMMIT_DEADLINE: Duration = Duration::from_secs(20);

/// How often the destination is polled while waiting for a commit.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The bucket every table of this suite is located in, and where the error output goes.
const BUCKET: &str = "iceberg-destination-bucket";

/// The Glue database every destination table lives in.
const DATABASE: &str = "iceberg_delivery_db";

/// The `errorCode` an error-output document names for a record the destination table does not
/// hold. Source: the Iceberg delivery failure class this emulator reports it under, which is
/// the code AWS names for a malformed field within a record.
const MALFORMED_FIELD: &str = "Iceberg.MalformedFieldWithinRecord";

/// What `!{firehose:error-output-type}` expands to for such a record.
const FAILED_OUTPUT_TYPE: &str = "iceberg-failed";

/// The Data Catalog the destination's `CatalogConfiguration` names.
fn catalog_arn() -> String {
    format!("arn:aws:glue:{REGION}:000000000000:catalog")
}

/// Create the Glue table `table` as an ICEBERG table through Glue's own wire, at its own
/// location inside [`BUCKET`], answering that location.
async fn create_iceberg_table(
    emulator: &Emulator,
    table: &str,
    columns: &[(&str, &str)],
) -> String {
    let location = format!("s3://{BUCKET}/iceberg/{table}/");
    let declared: Vec<Value> = columns
        .iter()
        .map(|(name, declared)| json!({ "Name": name, "Type": declared }))
        .collect();
    let (status, answered) = emulator
        .json_rpc(
            "glue",
            "AWSGlue",
            "CreateTable",
            json!({
                "DatabaseName": DATABASE,
                "TableInput": {
                    "Name": table,
                    "TableType": "EXTERNAL_TABLE",
                    "StorageDescriptor": { "Columns": declared, "Location": location },
                },
                "OpenTableFormatInput": { "IcebergInput": { "MetadataOperation": "CREATE" } },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the Iceberg table {table} must be created, got {answered:?}"
    );
    location
}

/// One table parameter of the stored Glue table, read back through `GetTable`.
async fn table_parameter(emulator: &Emulator, table: &str, parameter: &str) -> Option<String> {
    let (status, read) = emulator
        .json_rpc(
            "glue",
            "AWSGlue",
            "GetTable",
            json!({ "DatabaseName": DATABASE, "Name": table }),
        )
        .await;
    assert_eq!(status, 200, "GetTable on {table} must succeed, got {read:?}");
    read["Table"]["Parameters"][parameter]
        .as_str()
        .map(str::to_string)
}

/// The `IcebergDestinationConfiguration` a stream of this suite is created with.
///
/// `hints` is the `BufferingHints` member, left out when `None`.
fn iceberg_destination(table: &str, error_prefix: &str, hints: Option<Value>) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "CatalogConfiguration": { "CatalogARN": catalog_arn() },
        "DestinationTableConfigurationList": [
            { "DestinationDatabaseName": DATABASE, "DestinationTableName": table }
        ],
        "S3Configuration": {
            "RoleARN": ROLE_ARN,
            "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            "ErrorOutputPrefix": error_prefix,
        },
    });
    if let Some(hints) = hints {
        config["BufferingHints"] = hints;
    }
    config
}

/// Every key under `prefix` once the bucket holds at least `want` of them, or a failure
/// naming `case` once [`COMMIT_DEADLINE`] has passed.
async fn committed(
    emulator: &Emulator,
    case: &str,
    prefix: &str,
    want: usize,
) -> Vec<String> {
    let deadline = Instant::now() + COMMIT_DEADLINE;
    loop {
        let keys = emulator.keys_under(BUCKET, prefix).await;
        if keys.len() >= want {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {want} object(s) under {prefix}, got {keys:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `stream` is `ACTIVE` — the state a put is accepted in — by LOOKING, not by
/// waiting out the creation span: the clock owes the transition, so the test polls for it
/// until [`COMMIT_DEADLINE`] has passed.
async fn active(emulator: &Emulator, stream: &str) {
    let deadline = Instant::now() + COMMIT_DEADLINE;
    loop {
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the stream {stream} must reach ACTIVE, still {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One record a commit must take, with the timestamp spelling it carries.
struct Committed {
    /// What makes this row a case of its own: the spelling of the timestamp value.
    case: &'static str,
    /// The record a producer puts.
    put: &'static str,
}

/// The timestamp spellings an Iceberg `timestamp` column takes.
///
/// The rows differ along the FORM of the value: ISO-8601 with a zone offset, ISO-8601 with
/// `Z`, ISO-8601 with no zone at all, and epoch milliseconds as a JSON number.
const COMMITTED_RECORDS: &[Committed] = &[
    Committed {
        case: "ISO-8601 with a zone offset",
        put: r#"{"id": "a", "data": "one", "timestamp": "2026-01-01T00:00:00+02:00"}"#,
    },
    Committed {
        case: "ISO-8601 in UTC",
        put: r#"{"id": "b", "data": "two", "timestamp": "2026-01-01T00:00:00Z"}"#,
    },
    Committed {
        case: "ISO-8601 with no zone",
        put: r#"{"id": "c", "data": "three", "timestamp": "2026-01-01T00:00:00"}"#,
    },
    Committed {
        case: "epoch milliseconds",
        put: r#"{"id": "d", "data": "four", "timestamp": 1767225600000}"#,
    },
];

/// The record whose timestamp is the space-separated SQL spelling, which is not an ISO-8601
/// timestamp and so not a value the column holds.
const MALFORMED_RECORD: &str =
    r#"{"id": "e", "data": "five", "timestamp": "2026-01-01 00:00:00"}"#;

/// RULE: an Iceberg-destination stream commits the records of EACH flush to its destination
/// table — data object, manifest, manifest list, metadata file and the catalog pointer — and
/// routes the records that table does not hold to the error output. A later flush leaves the
/// earlier one's data file part of the table: a snapshot's manifest list is the whole of the
/// table's state, so it names the manifests the table already held beside the new one, and the
/// summary's totals accumulate.
#[tokio::test]
async fn an_iceberg_destination_commits_each_buffer_and_diverts_what_the_table_refuses() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;
    let table = "events";
    let location = create_iceberg_table(
        &emulator,
        table,
        &[("id", "string"), ("data", "string"), ("timestamp", "timestamp")],
    )
    .await;
    let first_pointer = table_parameter(&emulator, table, "metadata_location")
        .await
        .expect("the Iceberg create mints a metadata location");

    let stream = "iceberg-commit";
    emulator
        .firehose_ok(
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "DeliveryStreamType": "DirectPut",
                "IcebergDestinationConfiguration": iceberg_destination(
                    table,
                    "errors/",
                    Some(json!({ "SizeInMBs": 1, "IntervalInSeconds": 0 })),
                ),
            }),
        )
        .await;
    active(&emulator, stream).await;

    // The well-formed records and the malformed one in ONE buffer: what the error output
    // takes must not keep its siblings from committing.
    let mut records: Vec<Vec<u8>> = COMMITTED_RECORDS
        .iter()
        .map(|record| record.put.as_bytes().to_vec())
        .collect();
    records.push(MALFORMED_RECORD.as_bytes().to_vec());
    let (status, answered) = emulator.put_batch(stream, &records).await;
    assert_eq!(
        status, 200,
        "the batch must be accepted, got {status} with {answered:?}"
    );

    let data_prefix = format!("iceberg/{table}/data/");
    let metadata_prefix = format!("iceberg/{table}/metadata/");

    // The data object: one Parquet file under the table's own data directory.
    let data_keys = committed(&emulator, "the data object", &data_prefix, 1).await;
    assert!(
        data_keys.iter().all(|key| key.ends_with(".parquet")),
        "the committed data objects are Parquet files, got {data_keys:?}"
    );
    let data = emulator.object(BUCKET, &data_keys[0]).await;
    assert!(
        data.starts_with(b"PAR1"),
        "a Parquet file opens with its magic, got {:?}",
        data.get(..4)
    );

    // The snapshot's own objects: the manifest, the manifest list and a NEW metadata file
    // beside the one the create wrote.
    let metadata_keys = committed(&emulator, "the snapshot", &metadata_prefix, 4).await;
    let avro: Vec<&String> = metadata_keys
        .iter()
        .filter(|key| key.ends_with(".avro"))
        .collect();
    assert!(
        avro.iter().any(|key| key.contains("/snap-")),
        "a commit writes the manifest list of its snapshot, got {metadata_keys:?}"
    );
    assert!(
        avro.iter().any(|key| key.ends_with("-m0.avro")),
        "a commit writes the manifest of its data file, got {metadata_keys:?}"
    );
    for key in &avro {
        let body = emulator.object(BUCKET, key).await;
        assert!(
            body.starts_with(b"Obj"),
            "the Avro object {key} opens with its container magic, got {:?}",
            body.get(..4)
        );
    }

    // The catalog pointer moved to the metadata file the commit wrote, and that file is
    // there and carries the snapshot.
    let advanced = advanced_pointer(&emulator, table, &first_pointer).await;
    assert!(
        advanced.starts_with(&location) && advanced.ends_with(".metadata.json"),
        "the advanced pointer names a metadata file under the table's location, got {advanced}"
    );
    let document = metadata_document(&emulator, &advanced).await;
    let snapshots = document["snapshots"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        snapshots.len(),
        1,
        "the commit appends exactly one snapshot, got {document}"
    );
    let summary = &snapshots[0]["summary"];
    // The cases the committed records stand for, named in the assertion so a failure says
    // which timestamp spellings the commit was expected to take.
    let cases: Vec<&str> = COMMITTED_RECORDS.iter().map(|record| record.case).collect();
    assert_eq!(
        summary["added-records"].as_str(),
        Some(COMMITTED_RECORDS.len().to_string().as_str()),
        "case {cases:?}: the snapshot counts the records it added, got {summary}"
    );
    assert_eq!(
        summary["total-records"].as_str(),
        Some(COMMITTED_RECORDS.len().to_string().as_str()),
        "case {cases:?}: the first commit's totals are its own, got {summary}"
    );
    assert_eq!(
        summary["total-data-files"].as_str(),
        Some("1"),
        "the first commit holds the one data file it wrote, got {summary}"
    );
    assert_eq!(
        summary["operation"].as_str(),
        Some("append"),
        "a delivery commits an append, got {summary}"
    );
    assert_eq!(
        document["current-snapshot-id"], snapshots[0]["snapshot-id"],
        "the appended snapshot is the table's current one, got {document}"
    );
    let first_manifests = manifest_paths(&emulator, &snapshots[0]).await;
    assert_eq!(
        first_manifests.len(),
        1,
        "the first snapshot holds the one manifest it wrote, got {first_manifests:?}"
    );

    // The record the column does not hold: under the error output, with its own bytes and
    // the Iceberg failure code — and nothing else there.
    let error_keys = committed(&emulator, "the error output", "errors/", 1).await;
    assert!(
        error_keys
            .iter()
            .all(|key| key.starts_with(&format!("errors/{FAILED_OUTPUT_TYPE}/"))),
        "an Iceberg failure is written under its own segment of the error output prefix, got \
         {error_keys:?}"
    );
    let envelope: Value = serde_json::from_slice(&emulator.object(BUCKET, &error_keys[0]).await)
        .unwrap_or_else(|error| panic!("the error document must be JSON, got {error}"));
    assert_eq!(
        envelope["errorCode"].as_str(),
        Some(MALFORMED_FIELD),
        "the error document names the Iceberg failure, got {envelope}"
    );
    assert_eq!(
        envelope["rawData"].as_str(),
        Some(base64(MALFORMED_RECORD.as_bytes()).as_str()),
        "the error document carries the record's own bytes, got {envelope}"
    );
    assert!(
        envelope["attemptsMade"].as_u64().unwrap_or_default() >= 1,
        "the error document states the attempts the record cost, got {envelope}"
    );

    // THE SECOND FLUSH. One more record, one more commit: the table then holds both data
    // files, its new snapshot's manifest list names both manifests, and its totals are the
    // sum of the two commits.
    let (status, answered) = emulator
        .put_record(
            stream,
            br#"{"id": "f", "data": "six", "timestamp": "2026-01-02T00:00:00Z"}"#,
        )
        .await;
    assert_eq!(
        status, 200,
        "the second put must be accepted, got {status} with {answered:?}"
    );
    let second_pointer = advanced_pointer(&emulator, table, &advanced).await;
    let second = metadata_document(&emulator, &second_pointer).await;
    let snapshots = second["snapshots"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        snapshots.len(),
        2,
        "the second commit appends a second snapshot beside the first, got {second}"
    );
    let current = snapshots
        .iter()
        .find(|snapshot| snapshot["snapshot-id"] == second["current-snapshot-id"])
        .unwrap_or_else(|| panic!("the document must hold its current snapshot, got {second}"));
    let summary = &current["summary"];
    assert_eq!(
        summary["added-records"].as_str(),
        Some("1"),
        "the second snapshot added one record, got {summary}"
    );
    assert_eq!(
        summary["total-records"].as_str(),
        Some((COMMITTED_RECORDS.len() + 1).to_string().as_str()),
        "the second snapshot's totals account for the first flush too, got {summary}"
    );
    assert_eq!(
        summary["total-data-files"].as_str(),
        Some("2"),
        "the table holds both flushes' data files, got {summary}"
    );
    let manifests = manifest_paths(&emulator, current).await;
    assert_eq!(
        manifests.len(),
        2,
        "the current snapshot's manifest list names both manifests, got {manifests:?}"
    );
    assert!(
        manifests.contains(&first_manifests[0]),
        "the manifest of the first flush stays part of the table, got {manifests:?}"
    );
    // Both data files are there, and each is reachable from a manifest the current snapshot
    // names.
    let data_keys = emulator.keys_under(BUCKET, &data_prefix).await;
    assert_eq!(
        data_keys.len(),
        2,
        "each flush wrote its own data object, got {data_keys:?}"
    );
}

/// The table's `metadata_location` once it differs from `before`, or a failure once
/// [`COMMIT_DEADLINE`] has passed: the commit advances the pointer, so the test polls for it.
async fn advanced_pointer(emulator: &Emulator, table: &str, before: &str) -> String {
    let deadline = Instant::now() + COMMIT_DEADLINE;
    loop {
        let pointer = table_parameter(emulator, table, "metadata_location")
            .await
            .expect("the table keeps its metadata location");
        if pointer != before {
            return pointer;
        }
        assert!(
            Instant::now() < deadline,
            "the commit must advance the catalog pointer, still {pointer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The metadata document stored at the `s3://` location `pointer`.
async fn metadata_document(emulator: &Emulator, pointer: &str) -> Value {
    let key = pointer
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| panic!("the pointer {pointer} must name an object of {BUCKET}"));
    serde_json::from_slice(&emulator.object(BUCKET, key).await)
        .unwrap_or_else(|error| panic!("the metadata file {pointer} must be JSON, got {error}"))
}

/// The manifests one snapshot's manifest list names, read back out of the object store with
/// the Avro implementation — the entries an Iceberg reader resolves the snapshot's data files
/// through.
async fn manifest_paths(emulator: &Emulator, snapshot: &Value) -> Vec<String> {
    let list = snapshot["manifest-list"]
        .as_str()
        .unwrap_or_else(|| panic!("the snapshot must name its manifest list, got {snapshot}"));
    let key = list
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| panic!("the manifest list {list} must name an object of {BUCKET}"));
    let body = emulator.object(BUCKET, key).await;
    apache_avro::Reader::new(&body[..])
        .unwrap_or_else(|error| panic!("the manifest list {list} must read back, got {error}"))
        .filter_map(|entry| match entry {
            Ok(apache_avro::types::Value::Record(members)) => members
                .into_iter()
                .find(|(member, _)| member == "manifest_path")
                .and_then(|(_, value)| match value {
                    apache_avro::types::Value::String(path) => Some(path),
                    _ => None,
                }),
            _ => None,
        })
        .collect()
}

/// RULE: a destination table the catalog holds no Iceberg metadata for is committed nothing:
/// the stream accepts its records, writes nothing under that table's prefix and leaves the
/// catalog row exactly as it was.
#[tokio::test]
async fn an_unmarked_destination_table_is_committed_nothing() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;

    // A table created WITHOUT an `OpenTableFormatInput`: a Hive-style catalog row.
    let unmarked = "unmarked";
    create_table(
        &emulator,
        DATABASE,
        unmarked,
        &[("id", "string"), ("data", "string")],
    )
    .await;
    // And a MARKED table beside it, delivered to by a stream of its own: its commit is the
    // CONDITION this case waits on, so "nothing was written for the unmarked table" is read
    // once a buffer put at the same time has demonstrably been committed — never after a
    // fixed wait.
    let marked = "marked";
    create_iceberg_table(
        &emulator,
        marked,
        &[("id", "string"), ("data", "string")],
    )
    .await;

    for (stream, table, error_prefix) in [
        ("iceberg-unmarked", unmarked, "unmarked-errors/"),
        ("iceberg-marked", marked, "marked-errors/"),
    ] {
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "DeliveryStreamType": "DirectPut",
                    "IcebergDestinationConfiguration": iceberg_destination(
                        table,
                        error_prefix,
                        Some(json!({ "SizeInMBs": 1, "IntervalInSeconds": 0 })),
                    ),
                }),
            )
            .await;
        active(&emulator, stream).await;
        let (status, answered) = emulator
            .put_record(stream, br#"{"id": "a", "data": "one"}"#)
            .await;
        assert_eq!(
            status, 200,
            "case {table}: the put must be accepted, got {status} with {answered:?}"
        );
        assert!(
            answered["RecordId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {table}: an accepted put answers with a RecordId, got {answered:?}"
        );
    }

    // The marked table committed — which is what says the delivery window has passed for
    // both streams.
    committed(
        &emulator,
        "the marked table",
        &format!("iceberg/{marked}/data/"),
        1,
    )
    .await;

    let keys = emulator
        .keys_under(BUCKET, &format!("iceberg/{unmarked}/"))
        .await;
    assert!(
        keys.is_empty(),
        "an unmarked table is committed nothing, got {keys:?}"
    );
    let errors = emulator.keys_under(BUCKET, "unmarked-errors/").await;
    assert!(
        errors.is_empty(),
        "nothing failed either, so the error output stays empty, got {errors:?}"
    );
    assert_eq!(
        table_parameter(&emulator, unmarked, "metadata_location").await,
        None,
        "a delivery never makes a table that was not an Iceberg table one"
    );
}

/// One description case: the `BufferingHints` and `RetryOptions` the create supplies, and
/// what the description must report for them.
struct HintsRow {
    case: &'static str,
    /// The `BufferingHints` member as the create supplies it, or `None` to leave it out.
    supplied: Option<Value>,
    /// The `SizeInMBs` and `IntervalInSeconds` the description must carry.
    reported: (i64, i64),
    /// The `RetryOptions` member as the create supplies it, or `None` to leave it out.
    supplied_retry: Option<Value>,
    /// The `DurationInSeconds` the description must carry.
    reported_retry: i64,
    /// The `S3BackupMode` the create supplies, or `None` to leave it out, and the value the
    /// description must report for it.
    supplied_backup_mode: Option<&'static str>,
    reported_backup_mode: &'static str,
    /// The `AppendOnly` the create supplies, or `None` to leave it out, and the value the
    /// description must report for it.
    supplied_append_only: Option<bool>,
    reported_append_only: bool,
}

/// RULE: `DescribeDeliveryStream` reports an Iceberg destination's optional members with
/// every documented default materialised — `BufferingHints` ("If no value is specified,
/// BufferingHints object default values are used", those being `SizeInMBs` 5 and
/// `IntervalInSeconds` 300), `RetryOptions` (whose `DurationInSeconds` defaults to 300
/// seconds), `S3BackupMode` ("Currently, Firehose only supports `FailedDataOnly`") and
/// `AppendOnly` ("The default value is false").
///
/// The rows differ along what the create supplied: nothing at all, each published bound of
/// the members (`SizeInMBs` 1..128, `IntervalInSeconds` 0..900 and `DurationInSeconds`
/// 0..7200, from `smithy_query(query="shape", shape="BufferingHints,RetryOptions")`), one
/// member of a pair without the other, and each value of `IcebergS3BackupMode`.
#[tokio::test]
async fn an_iceberg_destination_reports_its_documented_defaults() {
    let rows: Vec<HintsRow> = vec![
        HintsRow {
            case: "nothing optional supplied at all, so every default is reported",
            supplied: None,
            reported: (5, 300),
            supplied_retry: None,
            reported_retry: 300,
            supplied_backup_mode: None,
            reported_backup_mode: "FailedDataOnly",
            supplied_append_only: None,
            reported_append_only: false,
        },
        HintsRow {
            case: "the published lower bounds",
            supplied: Some(json!({ "SizeInMBs": 1, "IntervalInSeconds": 0 })),
            reported: (1, 0),
            supplied_retry: Some(json!({ "DurationInSeconds": 0 })),
            reported_retry: 0,
            supplied_backup_mode: Some("FailedDataOnly"),
            reported_backup_mode: "FailedDataOnly",
            supplied_append_only: Some(false),
            reported_append_only: false,
        },
        HintsRow {
            case: "the published upper bounds",
            supplied: Some(json!({ "SizeInMBs": 128, "IntervalInSeconds": 900 })),
            reported: (128, 900),
            supplied_retry: Some(json!({ "DurationInSeconds": 7200 })),
            reported_retry: 7200,
            supplied_backup_mode: Some("AllData"),
            reported_backup_mode: "AllData",
            supplied_append_only: Some(true),
            reported_append_only: true,
        },
        HintsRow {
            case: "only the size, so the interval's default is reported",
            supplied: Some(json!({ "SizeInMBs": 64 })),
            reported: (64, 300),
            supplied_retry: Some(json!({})),
            reported_retry: 300,
            supplied_backup_mode: None,
            reported_backup_mode: "FailedDataOnly",
            supplied_append_only: None,
            reported_append_only: false,
        },
        HintsRow {
            case: "only the interval, so the size's default is reported",
            supplied: Some(json!({ "IntervalInSeconds": 60 })),
            reported: (5, 60),
            supplied_retry: Some(json!({ "DurationInSeconds": 60 })),
            reported_retry: 60,
            supplied_backup_mode: None,
            reported_backup_mode: "FailedDataOnly",
            supplied_append_only: Some(true),
            reported_append_only: true,
        },
    ];

    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;
    let table = "described";
    create_iceberg_table(&emulator, table, &[("id", "string")]).await;

    for (ordinal, row) in rows.iter().enumerate() {
        let case = row.case;
        let stream = format!("iceberg-described-{ordinal}");
        let mut config = iceberg_destination(table, "described-errors/", row.supplied.clone());
        if let Some(retry) = row.supplied_retry.clone() {
            config["RetryOptions"] = retry;
        }
        if let Some(mode) = row.supplied_backup_mode {
            config["S3BackupMode"] = json!(mode);
        }
        if let Some(append_only) = row.supplied_append_only {
            config["AppendOnly"] = json!(append_only);
        }
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "DeliveryStreamType": "DirectPut",
                    "IcebergDestinationConfiguration": config,
                }),
            )
            .await;
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        let iceberg = &described["DeliveryStreamDescription"]["Destinations"][0]
            ["IcebergDestinationDescription"];
        let hints = &iceberg["BufferingHints"];
        assert_eq!(
            (
                hints["SizeInMBs"].as_i64().unwrap_or_default(),
                hints["IntervalInSeconds"].as_i64().unwrap_or_default(),
            ),
            row.reported,
            "case {case:?}: expected the hints {:?}, got {hints}",
            row.reported
        );
        assert_eq!(
            iceberg["RetryOptions"]["DurationInSeconds"].as_i64(),
            Some(row.reported_retry),
            "case {case:?}: expected the retry duration {}, got {iceberg}",
            row.reported_retry
        );
        assert_eq!(
            iceberg["S3BackupMode"].as_str(),
            Some(row.reported_backup_mode),
            "case {case:?}: expected the backup mode {}, got {iceberg}",
            row.reported_backup_mode
        );
        assert_eq!(
            iceberg["AppendOnly"].as_bool(),
            Some(row.reported_append_only),
            "case {case:?}: expected AppendOnly {}, got {iceberg}",
            row.reported_append_only
        );
    }
}

/// RULE: server-side encryption is a direct-put-only feature — "You can enable SSE for a
/// Firehose stream only if it's a Firehose stream that uses DirectPut as its source" — so
/// `StartDeliveryStreamEncryption` on a stream of any other `DeliveryStreamType` is refused
/// with `InvalidArgumentException` and the service's own sentence, carrying the stream the
/// request named and the type the stream was created with. Such a stream has no encryption
/// state at all, so its description carries no `DeliveryStreamEncryptionConfiguration` —
/// before the refused call and after it — while a `DirectPut` stream always carries one.
///
/// The rows differ along the type the stream was created with: each value of
/// `DeliveryStreamType` that is not `DirectPut` and whose source this emulator takes.
#[tokio::test]
async fn start_encryption_on_a_sourced_stream_is_refused_in_aws_wording() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;

    // (the type the stream is created with, the source configuration it needs)
    let rows: Vec<(&str, Option<Value>)> = vec![
        (
            "KinesisStreamAsSource",
            Some(json!({
                "KinesisStreamSourceConfiguration": {
                    "KinesisStreamARN":
                        format!("arn:aws:kinesis:{REGION}:000000000000:stream/source"),
                    "RoleARN": ROLE_ARN,
                }
            })),
        ),
        (
            "MSKAsSource",
            Some(json!({
                "MSKSourceConfiguration": {
                    "MSKClusterARN":
                        format!("arn:aws:kafka:{REGION}:000000000000:cluster/demo/0a0a0a0a"),
                    "TopicName": "orders",
                    "AuthenticationConfiguration": { "Connectivity": "PRIVATE",
                                                     "RoleARN": ROLE_ARN },
                }
            })),
        ),
        // The direct-put stream beside them: the type SSE DOES apply to, which carries the
        // member with the documented disabled state even though nothing configured it.
        ("DirectPut", None),
    ];

    for (ordinal, (stream_type, source)) in rows.iter().enumerate() {
        let stream = format!("sourced-{ordinal}");
        let mut request = json!({
            "DeliveryStreamName": stream,
            "DeliveryStreamType": stream_type,
            "S3DestinationConfiguration": {
                "RoleARN": ROLE_ARN,
                "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            },
        });
        if let Some(source) = source {
            for (member, value) in source.as_object().cloned().unwrap_or_default() {
                request[member] = value;
            }
        }
        emulator.firehose_ok("CreateDeliveryStream", request).await;
        active(&emulator, &stream).await;

        let direct_put = *stream_type == "DirectPut";
        let before = described_encryption(&emulator, &stream).await;
        assert_eq!(
            before.is_null(),
            !direct_put,
            "case {stream_type}: only a direct-put stream has an SSE state, got {before}"
        );

        let (status, refused) = emulator
            .firehose(
                "StartDeliveryStreamEncryption",
                json!({
                    "DeliveryStreamName": stream,
                    "DeliveryStreamEncryptionConfigurationInput": { "KeyType": "AWS_OWNED_CMK" },
                }),
            )
            .await;
        if direct_put {
            assert_eq!(
                status, 200,
                "case {stream_type}: SSE applies to this stream, got {status} with {refused:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {stream_type}: the refusal is a client error, got {status} with {refused:?}"
        );
        assert!(
            refused["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with("InvalidArgumentException"),
            "case {stream_type}: expected InvalidArgumentException, got {refused:?}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(
                format!(
                    "Cannot enable encryption for a delivery stream {stream} with delivery \
                     stream type {stream_type}"
                )
                .as_str()
            ),
            "case {stream_type}: expected AWS's sentence, got {refused:?}"
        );
        // A refused call leaves the stream exactly as it was: still with no SSE state.
        let after = described_encryption(&emulator, &stream).await;
        assert_eq!(
            after, before,
            "case {stream_type}: the refused call must leave the stream as it was, got {after}"
        );
    }
}

/// The `DeliveryStreamEncryptionConfiguration` a describe reports for `stream`, and
/// `Value::Null` for a stream whose description carries no such member.
async fn described_encryption(emulator: &Emulator, stream: &str) -> Value {
    let described = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    described["DeliveryStreamDescription"]["DeliveryStreamEncryptionConfiguration"].clone()
}

// ---------------------------------------------------------------------------------------
// The Data Catalog target of an Iceberg destination, resolved at the call that writes it.
//
// An Iceberg destination delivers to a TABLE of a Data Catalog, and Firehose reads that table
// with the configuration's `RoleARN` at the call that submits the configuration — so a call
// naming a table the catalog cannot serve is refused with `InvalidArgumentException` and writes
// NOTHING: the create registers no stream and leaves the name free, and the update leaves the
// destination and the version id it found. Once the catalog holds the table the very same call
// is served.
// ---------------------------------------------------------------------------------------

/// A destination table this account's catalog holds.
const PRESENT_TABLE: &str = "present";

/// A destination table no catalog holds.
const ABSENT_TABLE: &str = "absent_table";

/// A Glue database no catalog holds.
const ABSENT_DATABASE: &str = "absent_db";

/// A Data Catalog of ANOTHER account, which this account's estate does not reach.
fn foreign_catalog_arn() -> String {
    format!("arn:aws:glue:{REGION}:111122223333:catalog")
}

/// What Firehose states for a destination table it could not read, whichever half of the
/// answer applies: the call it makes for the table, named with the role it would have made it
/// with.
fn unreadable_table_message() -> String {
    format!(
        "Role {ROLE_ARN} is not authorized to perform: glue:GetTable for the given table or \
         the table does not exist."
    )
}

/// The modelled `DeliveryStreamARN` of `stream`:
/// `arn:aws:firehose:<region>:<accountId>:deliverystream/<deliveryStreamName>`.
fn stream_arn(stream: &str) -> String {
    format!("arn:aws:firehose:{REGION}:000000000000:deliverystream/{stream}")
}

/// One `DestinationTableConfiguration` per `(database, table)` pair.
fn table_list(tables: &[(&str, &str)]) -> Value {
    Value::Array(
        tables
            .iter()
            .map(|(database, table)| {
                json!({
                    "DestinationDatabaseName": database,
                    "DestinationTableName": table,
                })
            })
            .collect(),
    )
}

/// An `IcebergDestinationConfiguration` naming `tables` in the Data Catalog `catalog`, or no
/// table list at all for `None`.
fn catalog_target(catalog: &str, tables: Option<&[(&str, &str)]>) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "CatalogConfiguration": { "CatalogARN": catalog },
        "S3Configuration": {
            "RoleARN": ROLE_ARN,
            "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            "ErrorOutputPrefix": "catalog-errors/",
        },
    });
    if let Some(tables) = tables {
        config["DestinationTableConfigurationList"] = table_list(tables);
    }
    config
}

/// `CreateDeliveryStream` with `config` as the stream's only destination.
async fn create_with_target(emulator: &Emulator, stream: &str, config: Value) -> (u16, Value) {
    emulator
        .firehose(
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "DeliveryStreamType": "DirectPut",
                "IcebergDestinationConfiguration": config,
            }),
        )
        .await
}

/// Assert that the answer is the refusal of an unreadable destination table.
fn assert_table_refusal(case: &str, status: u16, answered: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the call to be refused, got {status} with {answered}"
    );
    assert_eq!(
        error_code(answered),
        "InvalidArgumentException",
        "case {case:?}: expected InvalidArgumentException, got {answered}"
    );
    assert_eq!(
        error_message(answered),
        unreadable_table_message(),
        "case {case:?}: expected the glue:GetTable sentence, got {answered}"
    );
}

/// Assert that `stream` is no stream of this account: neither listed nor describable.
async fn assert_unregistered(emulator: &Emulator, case: &str, stream: &str) {
    let listed = emulator.firehose_ok("ListDeliveryStreams", json!({})).await;
    let names = listed["DeliveryStreamNames"].clone();
    assert!(
        !names
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|name| name == &json!(stream)),
        "case {case:?}: a refused create registers no stream, got the listing {names}"
    );
    let (status, described) = emulator
        .firehose(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: no stream of that name exists, got {described}"
    );
    assert_eq!(
        error_code(&described),
        "ResourceNotFoundException",
        "case {case:?}: expected the not-found refusal, got {described}"
    );
}

/// One create whose catalog target the Data Catalog cannot serve.
struct Unreadable {
    case: &'static str,
    stream: &'static str,
    /// The `(database, table)` pairs the destination lists.
    tables: &'static [(&'static str, &'static str)],
    /// Whether the configuration names another account's Data Catalog.
    foreign_catalog: bool,
}

/// RULE: a create whose Iceberg destination names a table the Data Catalog cannot serve is
/// refused with `InvalidArgumentException` and the glue:GetTable sentence, and registers
/// nothing — so the name it asked for is still free.
///
/// The rows differ along WHY the target cannot be served: the database absent, the table
/// absent inside a database that is there, one absent among two listed, and a table that IS
/// held named in another account's catalog.
#[tokio::test]
async fn a_create_naming_an_unreadable_destination_table_is_refused_and_registers_nothing() {
    const ROWS: &[Unreadable] = &[
        Unreadable {
            case: "the database the destination names is not in the catalog",
            stream: "iceberg-absent-database",
            tables: &[(ABSENT_DATABASE, PRESENT_TABLE)],
            foreign_catalog: false,
        },
        Unreadable {
            case: "the database is there and the table is not",
            stream: "iceberg-absent-table",
            tables: &[(DATABASE, ABSENT_TABLE)],
            foreign_catalog: false,
        },
        Unreadable {
            case: "one of the two tables listed is absent",
            stream: "iceberg-one-absent-of-two",
            tables: &[(DATABASE, PRESENT_TABLE), (DATABASE, ABSENT_TABLE)],
            foreign_catalog: false,
        },
        Unreadable {
            case: "the table is held by this account and the destination names another \
                   account's catalog",
            stream: "iceberg-foreign-catalog",
            tables: &[(DATABASE, PRESENT_TABLE)],
            foreign_catalog: true,
        },
    ];

    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;
    create_iceberg_table(&emulator, PRESENT_TABLE, &[("id", "string")]).await;

    for row in ROWS {
        let catalog = if row.foreign_catalog {
            foreign_catalog_arn()
        } else {
            catalog_arn()
        };
        let (status, answered) = create_with_target(
            &emulator,
            row.stream,
            catalog_target(&catalog, Some(row.tables)),
        )
        .await;
        assert_table_refusal(row.case, status, &answered);
        assert!(
            answered.get("DeliveryStreamARN").is_none(),
            "case {}: a refused create mints no ARN, got {answered}",
            row.case
        );
        assert_unregistered(&emulator, row.case, row.stream).await;
    }
}

/// One create whose catalog target the Data Catalog can serve.
struct Readable {
    case: &'static str,
    stream: &'static str,
    /// The `(database, table)` pairs the destination lists, or `None` for a destination that
    /// lists none at all.
    tables: Option<&'static [(&'static str, &'static str)]>,
    /// Whether the destination asks Firehose to create its tables itself.
    creates_its_tables: bool,
}

/// RULE: a create whose Iceberg destination names tables the catalog CAN serve — including one
/// naming no table at all ("Firehose will write data with insert if table specific
/// configuration is not provided here") and one whose tables Firehose creates itself
/// (`TableCreationConfiguration`, "the configuration to enable automatic table creation") — is
/// served: it answers the modelled `DeliveryStreamARN` for the name it was given, and the
/// stream reads back under it.
///
/// The rows differ along the FORM of the resolvable target: no list, one table, the same table
/// twice, a table named in another case (the catalog stores and looks names up folded), a
/// table that is a plain Hive row rather than an Iceberg one (what Firehose resolves is the
/// catalog's table, not its format), and an absent table under automatic table creation.
#[tokio::test]
async fn a_create_whose_destination_tables_the_catalog_serves_mints_the_modelled_arn() {
    const HIVE_TABLE: &str = "hive_row";
    const ROWS: &[Readable] = &[
        Readable {
            case: "a destination listing no table at all",
            stream: "iceberg-no-table-list",
            tables: None,
            creates_its_tables: false,
        },
        Readable {
            case: "a destination listing one table the catalog holds",
            stream: "iceberg-one-table",
            tables: Some(&[(DATABASE, PRESENT_TABLE)]),
            creates_its_tables: false,
        },
        Readable {
            case: "a destination listing the same table twice",
            stream: "iceberg-two-tables",
            tables: Some(&[(DATABASE, PRESENT_TABLE), (DATABASE, PRESENT_TABLE)]),
            creates_its_tables: false,
        },
        Readable {
            case: "a destination naming the table in another case, which the catalog folds",
            stream: "iceberg-folded-name",
            tables: Some(&[("ICEBERG_DELIVERY_DB", "PRESENT")]),
            creates_its_tables: false,
        },
        Readable {
            case: "a destination naming a table the catalog holds as a plain Hive row",
            stream: "iceberg-hive-row",
            tables: Some(&[(DATABASE, HIVE_TABLE)]),
            creates_its_tables: false,
        },
        Readable {
            case: "an absent table the destination asks Firehose to create itself",
            stream: "iceberg-table-creation",
            tables: Some(&[(DATABASE, ABSENT_TABLE)]),
            creates_its_tables: true,
        },
    ];

    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;
    create_iceberg_table(&emulator, PRESENT_TABLE, &[("id", "string")]).await;
    create_table(&emulator, DATABASE, HIVE_TABLE, &[("id", "string")]).await;

    for row in ROWS {
        let mut config = catalog_target(&catalog_arn(), row.tables);
        if row.creates_its_tables {
            config["TableCreationConfiguration"] = json!({ "Enabled": true });
        }
        let (status, created) = create_with_target(&emulator, row.stream, config).await;
        assert_eq!(
            status, 200,
            "case {}: expected the create to be served, got {status} with {created}",
            row.case
        );
        assert_eq!(
            created["DeliveryStreamARN"],
            json!(stream_arn(row.stream)),
            "case {}: expected the modelled ARN {}, got {created}",
            row.case,
            stream_arn(row.stream)
        );
        active(&emulator, row.stream).await;
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": row.stream }),
            )
            .await;
        assert_eq!(
            described["DeliveryStreamDescription"]["DeliveryStreamARN"],
            json!(stream_arn(row.stream)),
            "case {}: the created stream reads back under its ARN, got {described}",
            row.case
        );
    }
}

/// RULE: a create refused for its catalog target registers NOTHING, so once the catalog holds
/// the table the very same request is served — the documented order of provisioning a catalog
/// target and then the stream that delivers to it, under the name the client chose.
///
/// The rows differ along WHAT the catalog was missing when the first create was made: the whole
/// database, or only the table inside a database that was already there.
#[tokio::test]
async fn the_same_create_is_served_once_the_catalog_holds_the_destination_table() {
    /// `(case, stream, database, table)`: the target the create names, created between the
    /// refused calls and the identical retry.
    const ROWS: &[(&str, &str, &str, &str)] = &[
        (
            "the whole database was missing",
            "iceberg-retry-database",
            "retry_db",
            "events",
        ),
        (
            "only the table was missing",
            "iceberg-retry-table",
            DATABASE,
            "later_events",
        ),
    ];

    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;

    for (case, stream, database, table) in ROWS {
        let tables = [(*database, *table)];
        let config = catalog_target(&catalog_arn(), Some(&tables));

        // While the catalog cannot serve the table, every create of this stream is refused and
        // none of them registers anything — a second identical attempt is refused the same
        // way, never as a name already taken.
        for attempt in 1..=2 {
            let (status, answered) = create_with_target(&emulator, stream, config.clone()).await;
            assert_table_refusal(&format!("{case} (attempt {attempt})"), status, &answered);
            assert_unregistered(&emulator, case, stream).await;
        }

        // The catalog target is created, and then the IDENTICAL create.
        if *database != DATABASE {
            create_database(&emulator, database).await;
        }
        create_table(&emulator, database, table, &[("id", "string")]).await;
        let (status, created) = create_with_target(&emulator, stream, config.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the retry to be served, got {status} with {created}"
        );
        assert_eq!(
            created["DeliveryStreamARN"],
            json!(stream_arn(stream)),
            "case {case:?}: expected the modelled ARN {}, got {created}",
            stream_arn(stream)
        );
        active(&emulator, stream).await;
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        let iceberg = &described["DeliveryStreamDescription"]["Destinations"][0]
            ["IcebergDestinationDescription"];
        assert_eq!(
            iceberg["DestinationTableConfigurationList"],
            table_list(&tables),
            "case {case:?}: the created stream carries the target it was created with, \
             got {iceberg}"
        );
    }
}

/// RULE: the same resolution guards the call that REPLACES a destination's table list: an
/// `UpdateDestination` naming a table the catalog cannot serve is refused with the same
/// `InvalidArgumentException`, and the stream keeps the destination and the version id it had;
/// once the catalog holds that table the same update is served and the version id advances.
#[tokio::test]
async fn an_update_naming_an_unreadable_destination_table_leaves_the_destination_alone() {
    let emulator = Emulator::new();
    emulator.create_bucket(BUCKET).await;
    create_database(&emulator, DATABASE).await;
    create_iceberg_table(&emulator, PRESENT_TABLE, &[("id", "string")]).await;

    let stream = "iceberg-updated";
    let created = [(DATABASE, PRESENT_TABLE)];
    let (status, answered) = create_with_target(
        &emulator,
        stream,
        catalog_target(&catalog_arn(), Some(&created)),
    )
    .await;
    assert_eq!(
        status, 200,
        "the stream must be created, got {status} with {answered}"
    );
    active(&emulator, stream).await;
    let described = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    let description = &described["DeliveryStreamDescription"];
    let version = description["VersionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let destination_id = description["Destinations"][0]["DestinationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let repointed = "repointed";
    let update = json!({
        "DeliveryStreamName": stream,
        "CurrentDeliveryStreamVersionId": version,
        "DestinationId": destination_id,
        "IcebergDestinationUpdate": {
            "DestinationTableConfigurationList": table_list(&[(DATABASE, repointed)]),
        },
    });

    // The table does not exist yet: the update is refused and changes nothing.
    let (status, answered) = emulator.firehose("UpdateDestination", update.clone()).await;
    assert_table_refusal("the repointed table is not in the catalog", status, &answered);
    let described = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    let description = &described["DeliveryStreamDescription"];
    assert_eq!(
        description["VersionId"],
        json!(version),
        "a refused update does not advance the version id, got {description}"
    );
    assert_eq!(
        description["Destinations"][0]["IcebergDestinationDescription"]
            ["DestinationTableConfigurationList"],
        table_list(&created),
        "a refused update leaves the destination it found, got {description}"
    );

    // The catalog takes the table, and then the SAME update is served.
    create_iceberg_table(&emulator, repointed, &[("id", "string")]).await;
    let (status, answered) = emulator.firehose("UpdateDestination", update).await;
    assert_eq!(
        status, 200,
        "the update must be served once the table exists, got {status} with {answered}"
    );
    let described = emulator
        .firehose_ok(
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
    let description = &described["DeliveryStreamDescription"];
    assert_ne!(
        description["VersionId"],
        json!(version),
        "a served update advances the version id, got {description}"
    );
    assert_eq!(
        description["Destinations"][0]["IcebergDestinationDescription"]
            ["DestinationTableConfigurationList"],
        table_list(&[(DATABASE, repointed)]),
        "the served update repoints the destination, got {description}"
    );
}
