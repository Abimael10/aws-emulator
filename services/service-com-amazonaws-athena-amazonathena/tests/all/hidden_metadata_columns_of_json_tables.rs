// SPDX-License-Identifier: CC0-1.0

//! The HIDDEN per-object metadata columns of a Data Catalog table read one JSON document per
//! line — the layout an ingest pipeline that lands objects under a prefix writes — driven end
//! to end over the same router the emulator boots.
//!
//! THE RULE UNDER TEST: on a scan of a Hive table, the hidden metadata columns a statement
//! NAMES are resolved against the OBJECT each row was read from — `"$path"` to that object's
//! full `s3://<bucket>/<key>` URI, `"$file_size"` to the number of bytes it stores and
//! `"$file_modified_time"` to when it was last written — and the scan enumerates every
//! non-hidden object under the table's `StorageDescriptor$Location`, the objects of its nested
//! sub-prefixes included. So a statement that projects, groups by or filters on one of those
//! columns SUCCEEDS and answers per source object: grouping by `"$path"` gives one row per
//! object, carrying that object's URI and the number of records it holds (for a table read one
//! JSON document per line, one row per non-empty line). The columns stay hidden: one is read
//! only where the statement names it, so it is no part of what `SELECT *` expands to.
//!
//! Nothing here writes into a store to arrange what it asserts: the bucket and every object
//! come from Amazon S3's own writes, the database and the table from Glue's `CreateDatabase`
//! and `CreateTable`, and every reading from `StartQueryExecution`, `GetQueryExecution` and
//! `GetQueryResults`.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: how the
//! objects are keyed under the location (directly under it, in the one-level-per-date
//! sub-prefixes a delivery to a prefix writes, deeper still, mixed), how many objects there
//! are, how many documents each holds, whether blank lines stand between those documents, and
//! whether the registered location carries the trailing separator. The rest span what each
//! hidden column carries for the object a row came from, and restricting a read to one object
//! through the hidden location column.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_CANCELLED, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its objects in.
const BUCKET: &str = "athena-hidden-metadata-json";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-hidden-metadata-json/athena-results/";

/// The database every table here lives in.
const DATABASE: &str = "ingest_lake";

/// The serialization library of a SerDe that reads one JSON document per line, the one an
/// ingest table over delivered objects carries.
const JSON_SERDE: &str = "org.openx.data.jsonserde.JsonSerDe";

/// The input format of a table whose objects are text.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";

/// The output format such a table carries beside it.
const TEXT_OUTPUT_FORMAT: &str = "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat";

/// How long a poll waits for a started query to settle. Generous: what is asserted is that
/// the execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One row of the first suite: how a table's objects are laid out under its location.
struct Row {
    /// What this row is about, named in every assertion it makes.
    case: &'static str,
    /// The table this row registers, which is also its own prefix of [`BUCKET`].
    table: &'static str,
    /// Whether the registered `StorageDescriptor$Location` carries the trailing separator.
    trailing_separator: bool,
    /// Whether a blank line stands between two documents of an object, which holds no
    /// document at all.
    blank_lines: bool,
    /// The objects under the location: the key each is written under BELOW the table's own
    /// prefix, and how many documents it holds.
    objects: &'static [(&'static str, usize)],
}

/// The rows: one per layout the objects under a table's location take.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one object directly under the location",
            table: "single",
            trailing_separator: true,
            blank_lines: false,
            objects: &[("part-0.json", 1)],
        },
        Row {
            case: "several objects directly under the location, holding different numbers of \
                   documents",
            table: "several",
            trailing_separator: true,
            blank_lines: false,
            objects: &[("part-a.json", 3), ("part-b.json", 1), ("part-c.json", 7)],
        },
        Row {
            case: "objects in the date-and-hour sub-prefixes a delivery to a prefix writes",
            table: "delivered",
            trailing_separator: true,
            blank_lines: false,
            objects: &[
                ("2026/11/14/15/stream-1-2026-11-14-15-22-41-0", 2),
                ("2026/11/14/16/stream-1-2026-11-14-16-02-11-0", 4),
            ],
        },
        Row {
            case: "an object nested deeper than the others, beside one directly under the \
                   location",
            table: "mixed_depth",
            trailing_separator: true,
            blank_lines: false,
            objects: &[("part-0.json", 1), ("a/b/c/d/part-1.json", 5)],
        },
        Row {
            case: "a location registered without the trailing separator",
            table: "no_separator",
            trailing_separator: false,
            blank_lines: false,
            objects: &[("2026/11/14/15/part-0", 2), ("part-1.json", 1)],
        },
        Row {
            case: "documents separated by blank lines, which hold no document",
            table: "blank_lines",
            trailing_separator: true,
            blank_lines: true,
            objects: &[("part-0.json", 3), ("part-1.json", 2)],
        },
    ]
}

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by. Nothing is verified: this harness turns request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service: the catalog Glue holds is the catalog Athena resolves against, and the objects S3
/// holds are the data Athena reads.
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
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsJson1_1 call to `service`, and its HTTP status and parsed body.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Glue call that must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "glue", "AWSGlue", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Athena call that must answer 200.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object into [`BUCKET`], whose bytes are what the table's location then holds.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}/{key}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{BUCKET}/{key} must succeed, got {}",
        response.status()
    );
}

/// Create the bucket and the database every suite here starts from.
async fn given_bucket_and_database(router: &MultiServiceRouter, case: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: creating s3://{BUCKET} must succeed, got {}",
        response.status()
    );
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
}

/// Register `table` as the EXTERNAL table an ingest pipeline's objects are read through: one
/// JSON document per line, over the table's own prefix of [`BUCKET`].
async fn given_json_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    trailing_separator: bool,
) {
    let separator = if trailing_separator { "/" } else { "" };
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "Parameters": { "classification": "json", "EXTERNAL": "TRUE" },
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}{separator}"),
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

/// The bytes one object holds: `documents` newline-delimited JSON documents, with a blank line
/// between them where `blank_lines` says so — a line that holds no document at all.
fn object_body(from: usize, documents: usize, blank_lines: bool) -> String {
    let separator = if blank_lines { "\n\n" } else { "\n" };
    let mut body = String::new();
    for offset in 0..documents {
        body.push_str(&format!("{{\"id\": {}}}", from + offset));
        body.push_str(separator);
    }
    body
}

/// Run `sql` to completion and answer its terminal `Status` beside the execution's id.
async fn settled(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
    let started = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
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
    loop {
        let body = athena_ok(
            router,
            case,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED
            || state == QUERY_STATE_FAILED
            || state == QUERY_STATE_CANCELLED
        {
            return json!({ "Status": reported, "QueryExecutionId": id });
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql` to completion, require SUCCEEDED — carrying neither a state-change reason nor an
/// error, which is what a resolved statement settles as — and answer its rows: one entry per
/// data row, each cell `None` where the datum carries no value (SQL NULL).
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Vec<Vec<Option<String>>> {
    let settled = settled(router, case, sql).await;
    assert_eq!(
        settled["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {}",
        settled["Status"]
    );
    assert_eq!(
        (
            settled["Status"]["StateChangeReason"].as_str(),
            settled["Status"]["AthenaError"].as_object(),
        ),
        (None, None),
        "case {case:?}: a succeeded execution carries no reason and no error, got {}",
        settled["Status"]
    );
    let id = settled["QueryExecutionId"].as_str().unwrap_or_default();
    let body = athena_ok(
        router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    body["ResultSet"]["Rows"]
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

/// The one cell `text` names, for an expectation written as text.
fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

/// Grouping a JSON table by the hidden location column answers ONE row per source object,
/// whatever layout the objects take under the table's location: the row carries that object's
/// `s3://<bucket>/<key>` URI and the number of documents it holds.
#[tokio::test]
async fn grouping_a_json_table_by_the_hidden_path_column_answers_one_row_per_object() {
    let router = emulator();
    given_bucket_and_database(&router, "the ingest lake").await;

    for row in rows() {
        let case = row.case;
        given_json_table(&router, case, row.table, row.trailing_separator).await;
        // Every object's documents are numbered from a different point, so a count read off
        // one object could not be another's.
        let mut from = 1usize;
        let mut expected: Vec<Vec<Option<String>>> = Vec::new();
        for (key, documents) in row.objects {
            let full = format!("{}/{key}", row.table);
            put_object(
                &router,
                case,
                &full,
                &object_body(from, *documents, row.blank_lines),
            )
            .await;
            from += documents;
            expected.push(vec![
                some(&format!("s3://{BUCKET}/{full}")),
                some(&documents.to_string()),
            ]);
        }
        // `ORDER BY 1` is by the location column, so the expectation is in that order too.
        expected.sort();

        let actual = rows_of(
            &router,
            case,
            &format!(
                "SELECT \"$path\", count(*) FROM \"{DATABASE}\".\"{}\" GROUP BY 1 ORDER BY 1",
                row.table
            ),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected one row per object with the records it holds, got {actual:?}"
        );
    }
}

/// What each hidden column carries is a property of the OBJECT the row was read from: where it
/// lives, how many bytes it stores and when it was last written. They stay hidden, so the same
/// table read with `*` carries its declared columns and nothing else.
#[tokio::test]
async fn each_hidden_column_describes_the_object_the_row_was_read_from() {
    let case = "two objects in different sub-prefixes of one location";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_json_table(&router, case, "described", true).await;
    // (the key below the table's prefix, the documents the object holds)
    let objects = [
        ("2026/11/14/15/part-0", object_body(1, 2, false)),
        ("2026/11/14/16/part-1", object_body(3, 1, false)),
    ];
    for (key, body) in &objects {
        put_object(&router, case, &format!("described/{key}"), body).await;
    }

    let actual = rows_of(
        &router,
        case,
        &format!(
            "SELECT id, \"$path\", \"$file_size\", \"$file_modified_time\" \
             FROM \"{DATABASE}\".\"described\" ORDER BY id"
        ),
    )
    .await;
    // (the id of the row, the object it was read from, the bytes that object stores)
    let expected: Vec<(&str, &str, usize)> = vec![
        ("1", objects[0].0, objects[0].1.len()),
        ("2", objects[0].0, objects[0].1.len()),
        ("3", objects[1].0, objects[1].1.len()),
    ];
    assert_eq!(
        actual.len(),
        expected.len(),
        "case {case:?}: expected one row per document, got {actual:?}"
    );
    for (read, (id, key, size)) in actual.iter().zip(expected) {
        let named = vec![
            some(id),
            some(&format!("s3://{BUCKET}/described/{key}")),
            some(&size.to_string()),
        ];
        assert_eq!(
            read[..3].to_vec(),
            named,
            "case {case:?}: expected the row of {id} to name described/{key}, got {read:?}"
        );
        // The instant is the object's own, so what is asserted is that the column holds one
        // written the way a column of instants is read here — never one run's clock value.
        let modified = read[3].clone().unwrap_or_default();
        assert!(
            chrono::NaiveDateTime::parse_from_str(&modified, "%Y-%m-%d %H:%M:%S%.3f").is_ok(),
            "case {case:?}: expected the row of {id} to carry an instant, got {modified:?}"
        );
    }

    let declared = rows_of(
        &router,
        case,
        &format!("SELECT * FROM \"{DATABASE}\".\"described\" ORDER BY id"),
    )
    .await;
    let expected_declared: Vec<Vec<Option<String>>> =
        vec![vec![some("1")], vec![some("2")], vec![some("3")]];
    assert_eq!(
        declared, expected_declared,
        "case {case:?}: a hidden column is no part of what `*` expands to, got {declared:?}"
    );
}

/// A statement can restrict a read to ONE source object through the hidden location column —
/// which is how a row is traced back to the object it was delivered in — whether it names that
/// object's whole URI or matches the sub-prefix the object sits under.
#[tokio::test]
async fn the_hidden_path_column_restricts_a_read_to_the_objects_it_matches() {
    let case = "three objects, one per hour sub-prefix";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_json_table(&router, case, "traced", true).await;
    // (the key below the table's prefix, the first id the object's documents carry, how many)
    let objects = [
        ("2026/11/14/15/part-0", 1, 2),
        ("2026/11/14/16/part-1", 3, 1),
        ("2026/11/15/15/part-2", 4, 3),
    ];
    for (key, from, documents) in objects {
        put_object(
            &router,
            case,
            &format!("traced/{key}"),
            &object_body(from, documents, false),
        )
        .await;
    }

    let one = rows_of(
        &router,
        case,
        &format!(
            "SELECT id FROM \"{DATABASE}\".\"traced\" \
             WHERE \"$path\" = 's3://{BUCKET}/traced/2026/11/14/15/part-0' ORDER BY id"
        ),
    )
    .await;
    assert_eq!(
        one,
        vec![vec![some("1")], vec![some("2")]],
        "case {case:?}: expected only the documents of the object named, got {one:?}"
    );

    let by_prefix = rows_of(
        &router,
        case,
        &format!(
            "SELECT count(*) FROM \"{DATABASE}\".\"traced\" \
             WHERE \"$path\" LIKE 's3://{BUCKET}/traced/2026/11/14/%'"
        ),
    )
    .await;
    assert_eq!(
        by_prefix,
        vec![vec![some("3")]],
        "case {case:?}: expected the documents of the objects under that sub-prefix, got \
         {by_prefix:?}"
    );

    let unmatched = rows_of(
        &router,
        case,
        &format!(
            "SELECT count(*) FROM \"{DATABASE}\".\"traced\" \
             WHERE \"$path\" = 's3://{BUCKET}/traced/2026/11/14/15/nothing-here'"
        ),
    )
    .await;
    assert_eq!(
        unmatched,
        vec![vec![some("0")]],
        "case {case:?}: an object nothing holds matches no row, got {unmatched:?}"
    );
}
