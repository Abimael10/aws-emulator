// SPDX-License-Identifier: CC0-1.0

//! What a Data Catalog table's LOCATION enumerates, and what the objects Athena itself saved
//! under that location hold, driven end to end over the same router the emulator boots.
//!
//! TWO RULES UNDER TEST, and the second is what the first makes observable:
//!
//! * A read of a Hive table scans every object under its `StorageDescriptor$Location`
//!   RECURSIVELY — the location names a prefix, so an object nested any number of sub-prefixes
//!   below it is part of that table's data, a table located at the bucket root therefore holds
//!   every object of the bucket, and the hidden `$path` column names the object each row was
//!   read from.
//! * Every object this service SAVES is a line-oriented document whose records are one per
//!   line: the results of a run, its text output, its data manifest and the metadata
//!   information beside its results. So a results location that lies INSIDE a table's own
//!   location makes a run's own objects part of that table's data, and a later read of the
//!   table reads records out of them — the feedback loop a root-located table creates — rather
//!   than finding a single line a table that declares header lines skips whole.
//!
//! Nothing here asserts on the wall clock: a started execution is polled until it is terminal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    query_results::RESULT_METADATA_SUFFIX, QUERY_STATE_SUCCEEDED, RESULT_OBJECT_ROWS_SUFFIX,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The database every table here lives in.
const DATABASE: &str = "location_lake";

/// The serialization library of the delimited-text SerDe a Hive text table carries.
const LAZY_SIMPLE_SERDE: &str = "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe";

/// The input format of a table whose objects are text.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";

/// The output format such a table carries beside it.
const TEXT_OUTPUT_FORMAT: &str = "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat";

/// How long a poll waits for a started query to settle. Generous: what is asserted is that the
/// execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The two lines a standard web-log object opens with, which such a table declares as header
/// lines rather than data.
const HEADER_LINES: &str = "#Version: 1.0\n#Fields: id item\n";

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
/// holds are the data Athena reads and the destination its results land in.
pub(crate) fn emulator() -> MultiServiceRouter {
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
pub(crate) async fn athena_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One path-style request to the emulator's Amazon S3 front door, and its status and body.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// Create `bucket`.
pub(crate) async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}"), Vec::new()).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: creating s3://{bucket} must succeed, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
}

/// Write one object into `bucket`.
async fn given_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: Vec<u8>,
) {
    let (status, answer) = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: writing s3://{bucket}/{key} must succeed, got {status} with {}",
        String::from_utf8_lossy(&answer)
    );
}

/// Fetch one object's bytes.
pub(crate) async fn object_body(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
) -> Vec<u8> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}/{key}"), Vec::new()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetObject(s3://{bucket}/{key}) must answer 200, got {status}"
    );
    body
}

/// Every key `bucket` currently holds, as the object store lists them.
async fn listed_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}?list-type=2"), Vec::new()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListObjectsV2(s3://{bucket}) must answer 200, got {status}"
    );
    let listing = String::from_utf8_lossy(&body).to_string();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split("</Key>").next())
        .map(::std::string::ToString::to_string)
        .collect()
}

/// Create the one database every table here lives in.
pub(crate) async fn given_database(router: &MultiServiceRouter, case: &str) {
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
}

/// Register `table` over `location`: a Hive tab-delimited TEXT table of two columns whose own
/// properties declare `header_lines` header lines (none at all when that is zero).
async fn given_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    location: &str,
    header_lines: usize,
) {
    let mut parameters = json!({ "EXTERNAL": "TRUE" });
    if header_lines > 0 {
        parameters["skip.header.line.count"] = json!(header_lines.to_string());
    }
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "Parameters": parameters,
                "StorageDescriptor": {
                    "Location": location,
                    "Columns": [
                        { "Name": "id", "Type": "string" },
                        { "Name": "item", "Type": "string" },
                    ],
                    "InputFormat": TEXT_INPUT_FORMAT,
                    "OutputFormat": TEXT_OUTPUT_FORMAT,
                    "SerdeInfo": {
                        "SerializationLibrary": LAZY_SIMPLE_SERDE,
                        "Parameters": { "field.delim": "\t" },
                    },
                },
            },
        }),
    )
    .await;
}

/// Run `sql` to completion with its results landing under `output_location`, and answer the
/// execution's id. The poll looks again until the execution is terminal.
pub(crate) async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
    output_location: &str,
) -> String {
    let started = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
            "ResultConfiguration": { "OutputLocation": output_location },
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
        assert!(
            state != "FAILED" && state != "CANCELLED",
            "case {case:?}: {sql} must not fail, got {reported}"
        );
        if state == QUERY_STATE_SUCCEEDED {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution of {sql} never settled, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql` to completion and answer its data rows, each cell `None` for a SQL NULL.
pub(crate) async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
    output_location: &str,
) -> Vec<Vec<Option<String>>> {
    let id = settled(router, case, sql, output_location).await;
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

/// The `($path, count(*))` groups a read of `table` reports, sorted, as `(uri, rows)`.
async fn path_groups(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    output_location: &str,
) -> Vec<(String, i64)> {
    let rows = rows_of(
        router,
        case,
        &format!(
            "SELECT \"$path\", count(*) FROM \"{DATABASE}\".\"{table}\" GROUP BY 1 ORDER BY 1"
        ),
        output_location,
    )
    .await;
    rows.iter()
        .map(|row| {
            let path = row.first().cloned().flatten().unwrap_or_default();
            let counted = row
                .get(1)
                .cloned()
                .flatten()
                .unwrap_or_default()
                .parse::<i64>()
                .unwrap_or_else(|_| {
                    panic!("case {case:?}: expected a row count per group, got {row:?}")
                });
            (path, counted)
        })
        .collect()
}

/// `held` tab-delimited records, as one object of a two-column text table holds them.
fn records(held: usize) -> Vec<u8> {
    let mut text = String::new();
    for index in 1..=held {
        text.push_str(&format!("{index}\titem-{index}\n"));
    }
    text.into_bytes()
}

/// How many LINES a saved object holds: one per line separator, with a last line that carries
/// none counted as well.
pub(crate) fn lines_in(body: &[u8]) -> usize {
    let text = String::from_utf8_lossy(body);
    text.lines().count()
}

/// One layout of a bucket: where the table is located, and which objects lie under it.
struct Layout {
    /// What the row is.
    case: &'static str,
    /// The bucket this row's own objects live in.
    bucket: &'static str,
    /// The key prefix the table's location names, empty for the bucket root.
    location: &'static str,
    /// The objects placed under that location: the key suffix below it, and how many records
    /// that object holds.
    objects: &'static [(&'static str, usize)],
}

/// A table's location names a PREFIX, and a read of the table scans every object under that
/// prefix however deep it lies — so the `$path` groups of a read are one per object, each
/// carrying the records that object holds.
///
/// The rows differ along what the rule generalises over: how deep the location itself is (the
/// bucket root, a one-part prefix, a three-part prefix) and how many sub-prefixes separate an
/// object from that location (none, one, three).
#[tokio::test]
async fn a_table_reads_every_object_under_its_location_however_deep() {
    let layouts: &[Layout] = &[
        Layout {
            case: "located at the bucket root",
            bucket: "athena-scan-root",
            location: "",
            objects: &[("top.tsv", 2), ("nested/one.tsv", 3), ("a/b/c/deep.tsv", 1)],
        },
        Layout {
            case: "located at a one-part prefix",
            bucket: "athena-scan-prefix",
            location: "warehouse/",
            objects: &[("flat.tsv", 4), ("day=1/part.tsv", 2), ("day=1/hour=2/part.tsv", 5)],
        },
        Layout {
            case: "located at a three-part prefix",
            bucket: "athena-scan-deep",
            location: "a/b/c/",
            objects: &[("x.tsv", 1), ("d/y.tsv", 6)],
        },
    ];

    let router = emulator();
    given_database(&router, "setup").await;
    // The results land in a bucket of their own, so what each location enumerates is the
    // objects this suite put there and nothing a query of its own wrote.
    given_bucket(&router, "setup", "athena-scan-results").await;
    let output_location = "s3://athena-scan-results/out/";

    for (index, layout) in layouts.iter().enumerate() {
        let case = layout.case;
        given_bucket(&router, case, layout.bucket).await;
        let mut expected: Vec<(String, i64)> = Vec::new();
        for (suffix, held) in layout.objects {
            let key = format!("{}{suffix}", layout.location);
            given_object(&router, case, layout.bucket, &key, records(*held)).await;
            expected.push((
                format!("s3://{}/{key}", layout.bucket),
                i64::try_from(*held).expect("a record count"),
            ));
        }
        expected.sort();

        let table = format!("located_{index}");
        given_table(
            &router,
            case,
            &table,
            &format!("s3://{}/{}", layout.bucket, layout.location),
            0,
        )
        .await;

        let actual = path_groups(&router, case, &table, output_location).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected one group per object under the location, {expected:?}, got \
             {actual:?}"
        );
    }
}

/// Where a run's own results land relative to the table that is read afterwards.
struct Feedback {
    /// What the row is.
    case: &'static str,
    /// The bucket holding both the data object and the results.
    bucket: &'static str,
    /// The key prefix the table's location names, empty for the bucket root.
    location: &'static str,
    /// The key prefix the results land under, which lies inside that location.
    results: &'static str,
    /// How many header lines the table declares.
    header_lines: usize,
}

/// A results location INSIDE a table's own location makes the objects a run saved part of that
/// table's data: a first query over the table reads the data object alone, and a second query
/// — after the first has saved its results and the metadata information beside them — reads
/// those saved objects too, one `$path` group each, every one of them an object the bucket
/// really holds.
///
/// That only holds because a saved object is a line-oriented document whose records are one per
/// line: a table that declares header lines skips that many of its lines, not the whole
/// document.
///
/// The rows differ along what the rule generalises over: where the results prefix sits under
/// the location (directly below the bucket root, three sub-prefixes below it, below a table
/// located at a prefix of its own) and how many header lines the table declares.
#[tokio::test]
async fn a_table_reads_the_result_objects_an_earlier_query_saved_under_its_location() {
    let feedbacks: &[Feedback] = &[
        Feedback {
            case: "results directly below a root-located table",
            bucket: "athena-feedback-root",
            location: "",
            results: "athena-results/",
            header_lines: 2,
        },
        Feedback {
            case: "results three sub-prefixes below a root-located table",
            bucket: "athena-feedback-deep",
            location: "",
            results: "a/b/c/",
            header_lines: 2,
        },
        Feedback {
            case: "results below a table located at a prefix, and no header lines declared",
            bucket: "athena-feedback-prefix",
            location: "estate/",
            results: "estate/out/",
            header_lines: 0,
        },
    ];

    let router = emulator();
    given_database(&router, "setup").await;

    for (index, feedback) in feedbacks.iter().enumerate() {
        let case = feedback.case;
        given_bucket(&router, case, feedback.bucket).await;

        // The one data object: the header lines the table declares, then four records.
        let held = 4usize;
        let mut body = Vec::new();
        if feedback.header_lines > 0 {
            body.extend_from_slice(HEADER_LINES.as_bytes());
        }
        body.extend_from_slice(&records(held));
        let data_key = format!("{}log.tsv", feedback.location);
        given_object(&router, case, feedback.bucket, &data_key, body).await;
        let data_uri = format!("s3://{}/{data_key}", feedback.bucket);

        let table = format!("feedback_{index}");
        given_table(
            &router,
            case,
            &table,
            &format!("s3://{}/{}", feedback.bucket, feedback.location),
            feedback.header_lines,
        )
        .await;
        let output_location = format!("s3://{}/{}", feedback.bucket, feedback.results);

        // The FIRST read sees the data object alone: nothing else is under the location yet.
        let first = path_groups(&router, case, &table, &output_location).await;
        assert_eq!(
            first,
            vec![(data_uri.clone(), i64::try_from(held).expect("a count"))],
            "case {case:?}: expected the first read to see the data object alone, got {first:?}"
        );

        // That read saved its results and the metadata beside them under the location.
        let keys = listed_keys(&router, case, feedback.bucket).await;
        let saved: Vec<String> = keys
            .iter()
            .filter(|key| key.starts_with(feedback.results))
            .map(::std::string::ToString::to_string)
            .collect();
        assert!(
            saved.len() >= 2,
            "case {case:?}: expected the run to save its results and their metadata under \
             {}, got {keys:?}",
            feedback.results
        );

        // So the SECOND read sees them: the data object with its own records, and at least one
        // group whose object is one the first run saved.
        let second = path_groups(&router, case, &table, &output_location).await;
        let data_group = second.iter().find(|(path, _)| *path == data_uri);
        assert_eq!(
            data_group.map(|(_, counted)| *counted),
            Some(i64::try_from(held).expect("a count")),
            "case {case:?}: expected the data object to keep its {held} records, got {second:?}"
        );
        let from_the_run: Vec<&(String, i64)> = second
            .iter()
            .filter(|(path, _)| {
                saved
                    .iter()
                    .any(|key| *path == format!("s3://{}/{key}", feedback.bucket))
            })
            .collect();
        assert!(
            !from_the_run.is_empty(),
            "case {case:?}: expected a group for an object the first run saved under {}, got \
             {second:?} over the keys {keys:?}",
            feedback.results
        );
        assert!(
            from_the_run.iter().all(|(_, counted)| *counted > 0),
            "case {case:?}: every group of a saved object must carry the records it holds, got \
             {from_the_run:?}"
        );

        // And every group names an object the bucket really holds.
        let listed = listed_keys(&router, case, feedback.bucket).await;
        for (path, counted) in &second {
            let key = path
                .strip_prefix(&format!("s3://{}/", feedback.bucket))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected every group to name an object of the bucket, got {path:?}")
                });
            assert!(
                listed.iter().any(|held| held == key),
                "case {case:?}: the group {path:?} ({counted} rows) names no object the bucket \
                 holds, which are {listed:?}"
            );
        }
    }
}

/// The metadata information saved beside a result set is a MULTI-RECORD document: a reader over
/// it reads one record per line, so a table that declares `H` header lines reads every line of
/// it but the first `H`.
///
/// The rows differ along what the rule generalises over: how many columns the result set
/// described has (which is how long the document is) and how many header lines the reading
/// table declares — including none at all, where every line is a record.
#[tokio::test]
async fn the_metadata_saved_beside_a_result_set_is_read_record_by_record() {
    let router = emulator();
    given_database(&router, "setup").await;
    given_bucket(&router, "setup", "athena-metadata-source").await;
    given_bucket(&router, "setup", "athena-metadata-read").await;
    let output_location = "s3://athena-metadata-source/out/";

    // (case, the projection whose schema the metadata describes)
    let projections: &[(&str, &str)] = &[
        ("one described column", "SELECT 1 AS only_one"),
        ("three described columns", "SELECT 1 AS a, 'b' AS b, 2.5 AS c"),
    ];
    // How many header lines the table reading the saved document declares.
    let header_lines: &[usize] = &[0, 1, 2];

    let mut widths: Vec<usize> = Vec::new();
    for (index, (case, sql)) in projections.iter().enumerate() {
        let id = settled(&router, case, sql, output_location).await;
        let metadata_key = format!("out/{id}{RESULT_OBJECT_ROWS_SUFFIX}{RESULT_METADATA_SUFFIX}");
        let document =
            object_body(&router, case, "athena-metadata-source", &metadata_key).await;

        // It still describes the result set's columns, whatever its layout.
        let parsed: Value = serde_json::from_slice(&document).unwrap_or(Value::Null);
        assert!(
            parsed["ColumnInfo"].as_array().is_some_and(|columns| !columns.is_empty()),
            "case {case:?}: expected the saved metadata to describe the result set's columns, \
             got {:?}",
            String::from_utf8_lossy(&document)
        );

        let held = lines_in(&document);
        assert!(
            held > *header_lines.iter().max().unwrap_or(&0),
            "case {case:?}: expected the metadata document to hold more lines than a reader \
             skips as headers, got {held} in {:?}",
            String::from_utf8_lossy(&document)
        );
        widths.push(held);

        // The same bytes, read back as the data of a table located over them.
        for lines in header_lines {
            let key = format!("described-{index}-{lines}/metadata");
            given_object(
                &router,
                case,
                "athena-metadata-read",
                &key,
                document.clone(),
            )
            .await;
            let table = format!("described_{index}_{lines}");
            given_table(
                &router,
                case,
                &table,
                &format!("s3://athena-metadata-read/described-{index}-{lines}/"),
                *lines,
            )
            .await;
            let groups = path_groups(&router, case, &table, output_location).await;
            let expected = vec![(
                format!("s3://athena-metadata-read/{key}"),
                i64::try_from(held - lines).expect("a record count"),
            )];
            assert_eq!(
                groups, expected,
                "case {case:?}: with {lines} header lines declared, expected {expected:?}, got \
                 {groups:?}"
            );
        }
    }

    // A longer schema is a longer document: the records are per described member, so the
    // document grows with the columns it describes.
    assert!(
        widths.windows(2).all(|pair| pair[0] < pair[1]),
        "expected the saved metadata to hold more lines for a wider result set, got {widths:?}"
    );
}
