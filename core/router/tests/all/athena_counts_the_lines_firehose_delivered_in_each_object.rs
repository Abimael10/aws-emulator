// SPDX-License-Identifier: CC0-1.0

//! Attributing the rows of a Data Catalog table back to the DELIVERED OBJECT they were read
//! out of: a Firehose delivery stream lands its buffer under a key prefix, a Glue table is
//! registered at that prefix, and Athena is asked for the per-object row count through the
//! hidden location column.
//!
//! THE RULE UNDER TEST: a scan of a Hive table enumerates every non-hidden object under its
//! `StorageDescriptor$Location`, the objects of its nested sub-prefixes included, and the
//! hidden location column of a row is the full `s3://<bucket>/<key>` URI of the object that
//! row was read from — the same bucket-and-key spelling `ListObjectsV2` reports for it. So
//! `SELECT "$path", count(*) … GROUP BY 1` answers ONE row per object present under the
//! location, carrying that object's URI and the number of records it holds, which for a table
//! read one JSON document per line is its number of non-empty lines. An object a delivery
//! wrote is an object like any other: it is scanned wherever under the location its key puts
//! it, and none of the records it holds is attributed anywhere else.
//!
//! Four services have to agree for that to hold — Firehose decides when to flush and what key
//! it writes under, Amazon S3 holds the object, Glue holds the table that location belongs to
//! and Athena scans it, with the delivery made under an IAM role — so the suite lives here
//! rather than in any of their crates, over ONE engine exactly as `core/router/src/lib.rs`
//! mounts them.
//!
//! Nothing here writes into a store to arrange what it asserts, and no expectation is a value
//! this emulator invented: every object comes from a Firehose delivery of records a client
//! put, the expected URIs are the keys `ListObjectsV2` reports, the expected counts are the
//! lines `GetObject` returns, and every reading is `StartQueryExecution`, `GetQueryExecution`
//! and `GetQueryResults`.
//!
//! The rows differ along what the rule generalises over: which buffering hint triggers the
//! flush (the size hint crossed by one batch, or the interval), how many flushes land under
//! one location, how many records each carries and how large each record is, how deep the
//! destination prefix is, which destination configuration (plain or extended) carries the
//! hints, and whether the registered location carries the trailing separator.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_CANCELLED, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{given_role, send, REGION};
use super::firehose_buffer_flush_carries_mid_interval_arrivals::{body_of, json_call, keys_under};

/// The bucket every stream here delivers into, and that every table here reads.
const BUCKET: &str = "athena-firehose-delivered-objects";

/// Where the queries write their results: a prefix of [`BUCKET`] no table is registered at,
/// so a result object is never one of the objects a scan reads.
const OUTPUT_LOCATION: &str = "s3://athena-firehose-delivered-objects/athena-results/";

/// The database every table here lives in.
const DATABASE: &str = "delivery_lake";

/// The role every stream delivers under: Firehose has to be able to assume it, and it has to
/// allow the object write a delivery makes.
const DELIVERY_ROLE: &str = "athena-firehose-delivery-role";

/// The action that role allows.
const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The serialization library of a SerDe that reads one JSON document per line, which is what
/// a table over newline-delimited delivered records carries.
const JSON_SERDE: &str = "org.openx.data.jsonserde.JsonSerDe";

/// The input format of a table whose objects are text.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";

/// The output format such a table carries beside it.
const TEXT_OUTPUT_FORMAT: &str = "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat";

/// How long a row waits for a stream to settle and for its records to be delivered: many
/// times any row's own buffering interval, so a loaded host never fails a row the clock would
/// have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll waits for a started query to reach a terminal state. Generous: what is
/// asserted is that the execution settles, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: the stream, the table registered over its destination prefix, and how its
/// producer feeds it.
struct Row {
    /// What this row is about, named in every assertion it makes.
    case: &'static str,
    /// The delivery stream this row creates.
    stream: &'static str,
    /// `S3DestinationConfiguration$Prefix` the stream delivers under, which is also the
    /// prefix of [`BUCKET`] the row's table is registered at.
    prefix: &'static str,
    /// The table this row registers over that prefix.
    table: &'static str,
    /// `BufferingHints$SizeInMBs` for this row's destination.
    size_in_mbs: u64,
    /// `BufferingHints$IntervalInSeconds` for it.
    interval_seconds: u64,
    /// Whether the hints are carried by an `ExtendedS3DestinationConfiguration` rather than
    /// by a plain `S3DestinationConfiguration`.
    extended: bool,
    /// How many records each successive `PutRecordBatch` puts; the next batch is put only
    /// once everything before it has been delivered, so each one lands in its own flush.
    batches: &'static [usize],
    /// How many bytes of padding each record's document carries beside its id.
    pad_bytes: usize,
    /// Whether the registered `StorageDescriptor$Location` carries the trailing separator.
    trailing_separator: bool,
}

/// The rows: one per way a delivery fills the objects under a table's location.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one size-triggered flush of a batch of large records, read through a \
                   location registered without the trailing separator",
            stream: "size-hint-orders",
            prefix: "orders/",
            table: "orders",
            // 140 records of a little over 8 KiB cross the 1 MiB size hint, so the SIZE is
            // what triggers this row's flush and its interval never comes due.
            size_in_mbs: 1,
            interval_seconds: 900,
            extended: true,
            batches: &[140],
            pad_bytes: 8_000,
            trailing_separator: false,
        },
        Row {
            case: "two interval-triggered flushes of small records into a plain destination",
            stream: "interval-clicks",
            prefix: "clicks/",
            table: "clicks",
            // A size threshold far above anything this row puts, so the INTERVAL decides
            // both of its flushes.
            size_in_mbs: 64,
            interval_seconds: 5,
            extended: false,
            batches: &[4, 3],
            pad_bytes: 0,
            trailing_separator: true,
        },
        Row {
            case: "three interval-triggered flushes under a nested destination prefix",
            stream: "nested-events",
            prefix: "lake/events/",
            table: "nested_events",
            size_in_mbs: 64,
            interval_seconds: 5,
            extended: true,
            batches: &[2, 5, 1],
            pad_bytes: 64,
            trailing_separator: true,
        },
    ]
}

/// Athena, Glue, Firehose, Amazon S3 and IAM mounted on ONE shared engine, the way the router
/// mounts every service: the objects a delivery writes are the data Athena reads, and the
/// catalog Glue holds is the catalog Athena resolves against.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One Glue call that must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    json_call(router, case, "glue", &format!("AWSGlue.{operation}"), body).await
}

/// One Athena call that must succeed.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    json_call(
        router,
        case,
        "athena",
        &format!("AmazonAthena.{operation}"),
        body,
    )
    .await
}

/// One Firehose call that must succeed.
async fn firehose_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    json_call(
        router,
        case,
        "firehose",
        &format!("Firehose_20150804.{operation}"),
        body,
    )
    .await
}

/// Create the bucket every stream delivers into and every query writes its results under.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: creating s3://{BUCKET} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Register a row's table as the EXTERNAL table its delivered objects are read through: one
/// JSON document per line, over the destination prefix the stream writes under.
async fn given_table(router: &MultiServiceRouter, row: &Row) {
    let location = if row.trailing_separator {
        format!("s3://{BUCKET}/{}", row.prefix)
    } else {
        format!("s3://{BUCKET}/{}", row.prefix.trim_end_matches('/'))
    };
    glue_ok(
        router,
        row.case,
        "CreateTable",
        json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": row.table,
                "TableType": "EXTERNAL_TABLE",
                "Parameters": { "classification": "json", "EXTERNAL": "TRUE" },
                "StorageDescriptor": {
                    "Location": location,
                    "Columns": [{ "Name": "id", "Type": "int" }],
                    "InputFormat": TEXT_INPUT_FORMAT,
                    "OutputFormat": TEXT_OUTPUT_FORMAT,
                    "SerdeInfo": { "SerializationLibrary": JSON_SERDE },
                },
            },
        }),
    )
    .await;
}

/// Create this row's stream with its own hints and destination form, and answer once it is
/// `ACTIVE` — a stream still `CREATING` takes no record.
async fn given_active_stream(router: &MultiServiceRouter, row: &Row, role_arn: &str) {
    let destination = json!({
        "RoleARN": role_arn,
        "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
        "Prefix": row.prefix,
        "BufferingHints": {
            "SizeInMBs": row.size_in_mbs,
            "IntervalInSeconds": row.interval_seconds,
        },
    });
    let mut request = json!({
        "DeliveryStreamName": row.stream,
        "DeliveryStreamType": "DirectPut",
    });
    if row.extended {
        request["ExtendedS3DestinationConfiguration"] = destination;
    } else {
        request["S3DestinationConfiguration"] = destination;
    }
    firehose_ok(router, row.case, "CreateDeliveryStream", request).await;

    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose_ok(
            router,
            row.case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": row.stream }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the stream must reach ACTIVE, got {status}",
            row.case
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The newline-terminated JSON document the record carrying `id` holds, padded to the width
/// this row's records carry.
fn record_body(id: usize, pad_bytes: usize) -> String {
    format!("{{\"id\": {id}, \"pad\": \"{}\"}}\n", "x".repeat(pad_bytes))
}

/// Put `count` records carrying the ids from `from`, and answer the id after the last one.
async fn put_batch(router: &MultiServiceRouter, row: &Row, from: usize, count: usize) -> usize {
    let records: Vec<Value> = (from..from + count)
        .map(|id| {
            json!({
                "Data": ::aws_smithy_types::base64::encode(
                    record_body(id, row.pad_bytes).as_bytes(),
                ),
            })
        })
        .collect();
    let answered = firehose_ok(
        router,
        row.case,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": row.stream, "Records": records }),
    )
    .await;
    assert_eq!(
        answered["FailedPutCount"].as_i64(),
        Some(0),
        "case {:?}: every record of the batch must be accepted, got {answered}",
        row.case
    );
    from + count
}

/// Every object the bucket holds under `prefix`, in key order: the key `ListObjectsV2` reports
/// for it, and the ids of the documents its non-empty lines hold.
async fn objects_under(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> Vec<(String, Vec<i64>)> {
    let mut found = Vec::new();
    for key in keys_under(router, case, BUCKET, prefix).await {
        let body = body_of(router, case, BUCKET, &key).await;
        let ids = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                ::serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|document| document["id"].as_i64())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected every delivered line to be a document \
                             carrying an id, got {line:?}"
                        )
                    })
            })
            .collect();
        found.push((key, ids));
    }
    found
}

/// The objects under `prefix` once they hold exactly `records` documents between them: the
/// condition that everything put so far has been delivered, which is what makes the listing a
/// complete expectation rather than a snapshot of a delivery still in flight.
async fn delivered_objects(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    records: usize,
) -> Vec<(String, Vec<i64>)> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let objects = objects_under(router, case, prefix).await;
        let delivered: usize = objects.iter().map(|(_, ids)| ids.len()).sum();
        assert!(
            delivered <= records,
            "case {case:?}: a delivery writes each accepted record once; {records} record(s) \
             were put and {prefix:?} already holds {delivered}"
        );
        if delivered == records {
            return objects;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every record a put accepted must be delivered under {prefix:?}; \
             {delivered} of {records} arrived"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql` to completion and answer its rows: one entry per data row, each cell `None`
/// where the datum carries no value (SQL NULL).
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Vec<Vec<Option<String>>> {
    let started = athena_ok(
        router,
        case,
        "StartQueryExecution",
        json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    let id = started["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {started}"))
        .to_string();

    let deadline = Instant::now() + SETTLE_DEADLINE;
    let status = loop {
        let reported = athena_ok(
            router,
            case,
            "GetQueryExecution",
            json!({ "QueryExecutionId": id }),
        )
        .await;
        let status = reported["QueryExecution"]["Status"].clone();
        let state = status["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED
            || state == QUERY_STATE_FAILED
            || state == QUERY_STATE_CANCELLED
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never reached a terminal state, last saw {status}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {status}"
    );

    let answered = athena_ok(
        router,
        case,
        "GetQueryResults",
        json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    answered["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        // The first row of a DML result set's first page is the column labels.
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| {
                    datum["VarCharValue"]
                        .as_str()
                        .map(::std::string::ToString::to_string)
                })
                .collect()
        })
        .collect()
}

/// Grouping a table over a delivery stream's destination prefix by the hidden location column
/// answers ONE row per object the delivery wrote, carrying that object's `s3://<bucket>/<key>`
/// URI and the number of records it holds — whichever buffering hint flushed it, however deep
/// the prefix it landed under, and however many flushes the location holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn athena_counts_the_records_of_every_object_a_delivery_stream_wrote() {
    let router = emulator();
    given_bucket(&router, "the delivery lake").await;
    glue_ok(
        &router,
        "the delivery lake",
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    let role_arn = given_role(
        &router,
        "the delivery lake",
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some(PUT_OBJECT_ACTION),
    )
    .await;

    for row in rows() {
        let case = row.case;
        given_table(&router, &row).await;
        given_active_stream(&router, &row, &role_arn).await;

        // Each batch is put only once everything before it has been delivered, so the
        // location ends up holding one flush per batch rather than one object for all of
        // them. The ids are unique across the row, so a count read off one object could not
        // be another's.
        let mut next_id = 1usize;
        let mut put = 0usize;
        let mut objects = Vec::new();
        for count in row.batches {
            next_id = put_batch(&router, &row, next_id, *count).await;
            put += count;
            objects = delivered_objects(&router, case, row.prefix, put).await;
        }
        assert!(
            objects.len() >= row.batches.len(),
            "case {case:?}: expected at least one object per flush, got {objects:?}"
        );

        // The expectation is the object store's own answer: the URI of every key
        // `ListObjectsV2` reports under the location, against the documents `GetObject`
        // shows that object holding.
        let expected: BTreeMap<String, usize> = objects
            .iter()
            .map(|(key, ids)| (format!("s3://{BUCKET}/{key}"), ids.len()))
            .collect();

        let answered = rows_of(
            &router,
            case,
            &format!(
                "SELECT \"$path\", count(*) FROM \"{DATABASE}\".\"{}\" GROUP BY 1 ORDER BY 1",
                row.table
            ),
        )
        .await;
        let counted: BTreeMap<String, usize> = answered
            .iter()
            .map(|cells| {
                let path = cells.first().cloned().flatten().unwrap_or_else(|| {
                    panic!("case {case:?}: expected every row to name its object, got {answered:?}")
                });
                let count = cells
                    .get(1)
                    .cloned()
                    .flatten()
                    .and_then(|text| text.parse::<usize>().ok())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected every row to carry a count, got {answered:?}")
                    });
                (path, count)
            })
            .collect();
        assert_eq!(
            counted, expected,
            "case {case:?}: expected one row per delivered object with the records it holds, \
             got {counted:?}"
        );
        let total: usize = counted.values().sum();
        assert_eq!(
            total, put,
            "case {case:?}: expected every record the stream accepted to be counted once, got \
             {total} against {put} put, in {counted:?}"
        );

        // And the attribution is per OBJECT, not just per total: restricting the read to one
        // delivered object answers exactly the ids that object holds.
        let (key, ids) = objects
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a delivered object, got none"));
        let traced = rows_of(
            &router,
            case,
            &format!(
                "SELECT id FROM \"{DATABASE}\".\"{}\" WHERE \"$path\" = 's3://{BUCKET}/{key}' \
                 ORDER BY id",
                row.table
            ),
        )
        .await;
        let mut expected_ids = ids.clone();
        expected_ids.sort_unstable();
        let traced_ids: Vec<i64> = traced
            .iter()
            .map(|cells| {
                cells
                    .first()
                    .cloned()
                    .flatten()
                    .and_then(|text| text.parse::<i64>().ok())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected every row to carry an id, got {traced:?}")
                    })
            })
            .collect();
        assert_eq!(
            traced_ids, expected_ids,
            "case {case:?}: expected the rows of s3://{BUCKET}/{key} to be the documents it \
             holds, got {traced_ids:?}"
        );
    }
}
