// SPDX-License-Identifier: CC0-1.0

//! WHEN a destination whose data format conversion is in force writes its objects, and WHAT
//! that flush leaves behind: the destination's own `BufferingHints`.
//!
//! The rule under test: a destination with `DataFormatConversionConfiguration` in force
//! flushes on the FIRST of that destination's two buffering conditions — `IntervalInSeconds`
//! after the buffer opened ("Buffer incoming data for the specified period of time, in
//! seconds, before delivering it to the destination") or as soon as the buffer holds
//! `SizeInMBs` ("Buffer incoming data to the specified size, in MiBs, before delivering it to
//! the destination") — and whichever of the two fires, that flush writes BOTH halves of the
//! conversion: one columnar object under `Prefix` holding the records that converted against
//! "the column information from the Amazon Web Services Glue table" the `SchemaConfiguration`
//! binds, and ONE object under the resolved `ErrorOutputPrefix` ("a prefix that Firehose
//! evaluates and adds to failed records before writing them to S3") holding one
//! newline-delimited document per record that did not — each carrying `rawData`, the base64 of
//! that record's OWN pre-conversion bytes, under its own conversion error code: the
//! malformed-data code for a record carrying a value the bound column does not hold and the
//! parse-error code for bytes that are not parseable JSON at all. A half with no records in it
//! is no object at all: a flush every record of which converted writes nothing under the error
//! prefix, and one no record of which converted writes nothing under `Prefix`.
//!
//! So neither half is lost to a buffer whose window is longer than the shortest one
//! `IntervalInSeconds` admits: a destination configured with a long interval and a size its
//! records never reach still writes both once that interval has elapsed, and one whose records
//! cross its size inside a long interval writes both as soon as they do.
//!
//! Firehose, Amazon S3 and Glue are mounted on ONE shared engine, exactly as the router mounts
//! them: the bound table is created through Glue's own wire, the records are put through
//! Firehose's, and the objects are read back through Amazon S3's. No case sleeps a window out
//! or reads the wall clock: each polls its own destination until the objects its hints owe are
//! there.

use std::time::{Duration, Instant};

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use serde_json::{json, Value};

use crate::buffering_hints::{BIG_RECORD_BYTES, SHORT_INTERVAL};
use crate::delivery_stream_lifecycle::{REGION, ROLE_ARN};
use crate::destination_delivery::{base64, contains, Emulator};
use crate::format_conversion_delivery::{
    create_database, create_table, FAILED_OUTPUT_TYPE, MALFORMED_DATA, PARSE_ERROR,
};
use crate::parquet_serializer_parameters::await_active;

/// The four bytes the Parquet format frames a file between, at each end of it: what makes the
/// object under `Prefix` a file a Parquet reader reads rather than the records' own JSON.
const PARQUET_MAGIC: &[u8] = b"PAR1";

/// The longest wait `IntervalInSeconds` admits, in seconds — its published maximum
/// (`smithy_query(query="shape", shape="IntervalInSeconds")`: range 0..900). A row configured
/// with it is flushed by its SIZE condition or not at all while a case runs.
const LONGEST_INTERVAL: i64 = 900;

/// The largest `SizeInMBs` the member admits (`smithy_query(query="shape",
/// shape="SizeInMBs")`: range 1..128), which a row's records never reach.
const LARGEST_SIZE_IN_MIB: i64 = 128;

/// A `SizeInMBs` far above what a row of small records holds, inside the same published range.
const UNREACHED_SIZE_IN_MIB: i64 = 64;

/// The columns of the bound Glue table, as `(Name, Type)`. The types are Hive's own data type
/// names, which is what a Glue column's `Type` carries (`Column$Type`: "the data type of the
/// Column"): a scalar string, a scalar `int`, a string column the filler of a size-crossing
/// row is carried in, and one column that NESTS, so a record can contradict the schema deep
/// inside it rather than at the top level.
const COLUMNS: &[(&str, &str)] = &[
    ("marker", "string"),
    ("n", "int"),
    ("pad", "string"),
    ("tags", "array<struct<sku:string,qty:int>>"),
];

/// How long the objects a row's hints owe are waited for before the case fails: generous
/// beside the longest window any row here is configured with, so a loaded machine does not
/// fail a case the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);

/// How often a destination is polled while waiting for those objects.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One flush case: the destination's buffering hints and the records put into the one buffer
/// they govern.
struct FlushRow {
    case: &'static str,
    /// `BufferingHints` as `(SizeInMBs, IntervalInSeconds)`.
    hints: (i64, i64),
    /// How many records that convert against [`COLUMNS`] are put.
    conforming: usize,
    /// How many filler bytes each of those records carries in its `pad` column, which is what
    /// makes a row's buffer cross its `SizeInMBs`.
    filler: usize,
    /// How many records carrying a value the bound column does not hold — a string where the
    /// nested `qty` column declares an `int` — are put.
    malformed: usize,
    /// How many records that are not parseable JSON at all are put.
    unparseable: usize,
}

/// The flush cases.
///
/// The rows differ along what the rule generalises over: WHICH of the destination's two
/// buffering conditions fires (the shortest interval its member admits, a longer interval
/// under a size the buffer never reaches, and a size the records cross inside the longest
/// interval the member admits), and WHICH records the one flush holds (both failure classes
/// beside the records that convert, several of one class, only failures, and no failure at
/// all).
fn flush_rows() -> Vec<FlushRow> {
    vec![
        FlushRow {
            case: "the shortest interval the member admits, one record of each class",
            hints: (1, 0),
            conforming: 1,
            filler: 0,
            malformed: 1,
            unparseable: 1,
        },
        FlushRow {
            case: "a longer interval under a SizeInMBs the buffer never reaches",
            hints: (UNREACHED_SIZE_IN_MIB, SHORT_INTERVAL),
            conforming: 2,
            filler: 0,
            malformed: 2,
            unparseable: 1,
        },
        FlushRow {
            case: "records crossing SizeInMBs inside the longest interval the member admits",
            hints: (1, LONGEST_INTERVAL),
            conforming: 2,
            filler: BIG_RECORD_BYTES,
            malformed: 1,
            unparseable: 1,
        },
        FlushRow {
            case: "a flush no record of which is parseable JSON",
            hints: (1, 0),
            conforming: 0,
            filler: 0,
            malformed: 0,
            unparseable: 2,
        },
        FlushRow {
            case: "a flush every record of which contradicts the bound columns",
            hints: (LARGEST_SIZE_IN_MIB, SHORT_INTERVAL),
            conforming: 0,
            filler: 0,
            malformed: 3,
            unparseable: 0,
        },
        FlushRow {
            case: "a flush every record of which converts",
            hints: (1, 0),
            conforming: 3,
            filler: 0,
            malformed: 0,
            unparseable: 0,
        },
    ]
}

/// One record a case puts: a word unique to it, the bytes the producer puts, and what must
/// become of it.
struct PutRecord {
    /// A word carried by this record and by no other, which is what the converted object and
    /// the error output are read for.
    marker: String,
    data: Vec<u8>,
    /// `None` for a record that converts; the conversion error code its own error-output
    /// document must name for one that does not.
    diverted: Option<&'static str>,
}

/// One row under test: its stream, where its two halves land, and the records it put.
struct Leg {
    case: &'static str,
    stream: String,
    main_prefix: String,
    error_prefix: String,
    records: Vec<PutRecord>,
}

impl Leg {
    /// The markers of the records that must convert, in put order: the rows the object under
    /// `Prefix` must hold, in the order the flush read them.
    fn converted_markers(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|record| record.diverted.is_none())
            .map(|record| record.marker.clone())
            .collect()
    }

    /// The records that must be diverted instead, in put order.
    fn diverted(&self) -> Vec<&PutRecord> {
        self.records
            .iter()
            .filter(|record| record.diverted.is_some())
            .collect()
    }
}

/// A conversion flush writes the converted object and the diverted records' object on the
/// first of its destination's own buffering conditions, whichever that is.
#[tokio::test]
async fn a_conversion_flush_writes_both_halves_on_its_first_buffering_condition() {
    let emulator = Emulator::new();
    let bucket = "conversion-flush-bucket";
    emulator.create_bucket(bucket).await;
    let database = "flush_window_db";
    let table = "flush_window_events";
    create_database(&emulator, database).await;
    create_table(&emulator, database, table, COLUMNS).await;

    // Every stream is created before any of them is awaited, so the suite spends ONE
    // clock-owed CREATING -> ACTIVE span rather than one per row.
    let mut legs: Vec<Leg> = Vec::new();
    for (ordinal, row) in flush_rows().iter().enumerate() {
        let stream = format!("conversion-flush-{ordinal}");
        let main_prefix = format!("{stream}-pq/");
        let error_prefix = format!("{stream}-err/");
        let (size, interval) = row.hints;
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": &stream,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration": {
                        "RoleARN": ROLE_ARN,
                        "BucketARN": format!("arn:aws:s3:::{bucket}"),
                        "Prefix": &main_prefix,
                        "ErrorOutputPrefix":
                            format!("{error_prefix}!{{firehose:error-output-type}}/"),
                        "CompressionFormat": "UNCOMPRESSED",
                        "BufferingHints": {
                            "SizeInMBs": size,
                            "IntervalInSeconds": interval,
                        },
                        "DataFormatConversionConfiguration": {
                            "Enabled": true,
                            "SchemaConfiguration": {
                                "RoleARN": ROLE_ARN,
                                "DatabaseName": database,
                                "TableName": table,
                                "Region": REGION,
                                "VersionId": "LATEST",
                            },
                            "InputFormatConfiguration": {
                                "Deserializer": { "OpenXJsonSerDe": {} },
                            },
                            "OutputFormatConfiguration": {
                                "Serializer": { "ParquetSerDe": { "Compression": "UNCOMPRESSED" } },
                            },
                        },
                    },
                }),
            )
            .await;
        legs.push(Leg {
            case: row.case,
            records: records_of(row, &stream),
            stream,
            main_prefix,
            error_prefix,
        });
    }
    for leg in &legs {
        await_active(&emulator, leg.case, &leg.stream).await;
    }

    // One put per row, carrying the records that convert and the records that do not in the
    // SAME buffer: what the error output takes must not keep its siblings from arriving.
    for leg in &legs {
        let records: Vec<Vec<u8>> = leg
            .records
            .iter()
            .map(|record| record.data.clone())
            .collect();
        let (status, answered) = emulator.put_batch(&leg.stream, &records).await;
        assert_eq!(
            status, 200,
            "case {:?}: the batch must be accepted, got {answered:?}",
            leg.case
        );
        assert_eq!(
            answered["FailedPutCount"],
            json!(0),
            "case {:?}: every record of the batch must be accepted, got {answered:?}",
            leg.case
        );
    }

    for leg in &legs {
        assert_converted_half(&emulator, bucket, leg).await;
        assert_diverted_half(&emulator, bucket, database, table, leg).await;
    }
}

/// The records one row puts: the ones that convert against [`COLUMNS`], the ones carrying a
/// value the nested column does not hold and the ones that are not JSON at all, INTERLEAVED,
/// so what each half holds is read in put order rather than in class order.
fn records_of(row: &FlushRow, stream: &str) -> Vec<PutRecord> {
    let filler = "x".repeat(row.filler);
    let mut records: Vec<PutRecord> = Vec::new();
    let longest = row.conforming.max(row.malformed).max(row.unparseable);
    for index in 0..longest {
        if index < row.conforming {
            let marker = format!("{stream}-ok-{index}");
            let data = json!({
                "marker": &marker,
                "n": index,
                "pad": &filler,
                "tags": [{ "sku": "a", "qty": index }],
            });
            records.push(PutRecord {
                marker,
                data: data.to_string().into_bytes(),
                diverted: None,
            });
        }
        if index < row.malformed {
            let marker = format!("{stream}-mismatch-{index}");
            // The nested `qty` column declares an `int` and this record carries text there,
            // which is a value that column does not hold: the record was READ but does not
            // match the schema.
            let data = json!({
                "marker": &marker,
                "n": index,
                "pad": "",
                "tags": [{ "sku": "b", "qty": "not-an-int" }],
            });
            records.push(PutRecord {
                marker,
                data: data.to_string().into_bytes(),
                diverted: Some(MALFORMED_DATA),
            });
        }
        if index < row.unparseable {
            let marker = format!("{stream}-raw-{index}");
            // Bytes that are not one JSON document at all: the OTHER failure class.
            let data = format!("not-json-at-all {marker}").into_bytes();
            records.push(PutRecord {
                marker,
                data,
                diverted: Some(PARSE_ERROR),
            });
        }
    }
    records
}

/// The half under `Prefix`: ONE Parquet object holding one row per record that converted, in
/// put order, and nothing of the records that did not — or no object at all for a flush none
/// of whose records converted.
async fn assert_converted_half(emulator: &Emulator, bucket: &str, leg: &Leg) {
    let case = leg.case;
    let expected = leg.converted_markers();
    if expected.is_empty() {
        // Nothing converted, so there is nothing to deliver: the flush is observed through the
        // error-output half, and the delivered prefix must still hold nothing afterwards.
        assert_objects_under(emulator, case, bucket, &leg.error_prefix).await;
        let keys = emulator.keys_under(bucket, &leg.main_prefix).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a flush no record of which converted delivers no object, got \
             {keys:?}"
        );
        return;
    }
    let keys = assert_objects_under(emulator, case, bucket, &leg.main_prefix).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: one flushed buffer is one converted object, got {keys:?}"
    );
    let body = emulator.object(bucket, &keys[0]).await;
    assert!(
        body.starts_with(PARQUET_MAGIC) && body.ends_with(PARQUET_MAGIC),
        "case {case:?}: expected a Parquet file, got {:?}..{:?}",
        String::from_utf8_lossy(&body[..body.len().min(8)]),
        String::from_utf8_lossy(&body[body.len().saturating_sub(8)..])
    );
    let read = converted_markers(case, &body);
    assert_eq!(
        read, expected,
        "case {case:?}: expected the converted object to hold the rows {expected:?}, got \
         {read:?}"
    );
    for record in leg.diverted() {
        assert!(
            !contains(&body, record.marker.as_bytes()),
            "case {case:?}: expected nothing of the diverted record {:?} in the converted \
             object",
            record.marker
        );
    }
}

/// The half under the resolved `ErrorOutputPrefix`: ONE newline-delimited object holding one
/// document per record that did not convert, each under its OWN conversion error code and
/// carrying its own pre-conversion bytes — or no object at all for a flush every record of
/// which converted.
async fn assert_diverted_half(
    emulator: &Emulator,
    bucket: &str,
    database: &str,
    table: &str,
    leg: &Leg,
) {
    let case = leg.case;
    let diverted = leg.diverted();
    if diverted.is_empty() {
        // The delivered half has arrived by now ([`assert_converted_half`] waited for it), so
        // the flush has happened and the error prefix must have taken nothing from it.
        let keys = emulator.keys_under(bucket, &leg.error_prefix).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a flush every record of which converted writes no error output, \
             got {keys:?}"
        );
        return;
    }
    let keys = assert_objects_under(emulator, case, bucket, &leg.error_prefix).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: every failed record of one flush shares one object, got {keys:?}"
    );
    let expected_prefix = format!("{}{FAILED_OUTPUT_TYPE}/", leg.error_prefix);
    assert!(
        keys[0].starts_with(&expected_prefix),
        "case {case:?}: expected the error output under {expected_prefix:?}, got {:?}",
        keys[0]
    );
    // The name of an object an UNCOMPRESSED destination writes carries no file extension: the
    // default extension is the one the compression format carries, and that format carries
    // none.
    let segment = keys[0].rsplit('/').next().unwrap_or_default();
    assert!(
        !segment.contains('.'),
        "case {case:?}: expected the error output's name to carry no file extension, got \
         {segment:?}"
    );
    let body = emulator.object(bucket, &keys[0]).await;
    let text = String::from_utf8_lossy(&body);
    let documents: Vec<Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case {case:?}: an error-output document must be JSON: {error}")
            })
        })
        .collect();
    assert_eq!(
        documents.len(),
        diverted.len(),
        "case {case:?}: expected one document per diverted record, got {documents:?}"
    );
    for (document, record) in documents.iter().zip(&diverted) {
        assert_eq!(
            document["lastErrorCode"].as_str(),
            record.diverted,
            "case {case:?}: expected the record {:?} under {:?}, got {document:?}",
            record.marker,
            record.diverted
        );
        assert_eq!(
            document["rawData"].as_str(),
            Some(base64(&record.data).as_str()),
            "case {case:?}: expected the record {:?} to carry its own pre-conversion bytes, \
             got {document:?}",
            record.marker
        );
        assert_eq!(
            document["dataCatalogTable"]["databaseName"].as_str(),
            Some(database),
            "case {case:?}: expected the bound database {database:?} in the error output, got \
             {document:?}"
        );
        assert_eq!(
            document["dataCatalogTable"]["tableName"].as_str(),
            Some(table),
            "case {case:?}: expected the bound table {table:?} in the error output, got \
             {document:?}"
        );
    }
}

/// The `marker` column of every row the converted object holds, in the order the file holds
/// them.
fn converted_markers(case: &str, body: &[u8]) -> Vec<String> {
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(body))
        .unwrap_or_else(|error| panic!("case {case:?}: the object must be Parquet: {error}"));
    reader
        .get_row_iter(None)
        .unwrap_or_else(|error| panic!("case {case:?}: the rows must read: {error}"))
        .filter_map(Result::ok)
        .map(|row| {
            let marker = row
                .get_column_iter()
                .find(|(name, _)| name.as_str() == "marker")
                .map(|(_, field)| field.clone());
            match marker {
                Some(Field::Str(value)) => value,
                other => panic!(
                    "case {case:?}: every converted row carries its marker column, got {other:?}"
                ),
            }
        })
        .collect()
}

/// Every key under `prefix` once the destination holds at least one, or a failure naming
/// `case` once [`DELIVERY_DEADLINE`] has passed.
///
/// The CONDITION is waited for and not a span: a row is flushed when its own hints say so, and
/// how long the host takes to get there is not what this suite is about.
async fn assert_objects_under(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = emulator.keys_under(bucket, prefix).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the flush to write an object under {prefix:?}, the \
             destination holds none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
