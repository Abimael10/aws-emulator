// SPDX-License-Identifier: CC0-1.0

//! What Athena's statements that WRITE their output leave in the object store:
//! `CREATE TABLE … AS <query>` and `UNLOAD (<query>) TO '<location>'`.
//!
//! THE RULE UNDER TEST: a statement whose result is OBJECTS runs its query like any other
//! read and then WRITES the rows it produced — as objects of the format its `format`
//! property names, under the location it names, laid out one directory per partition value
//! (`<key>=<value>`) for the columns its `partitioned_by` property names — and the execution
//! reaches `QueryExecutionState` SUCCEEDED. A `CREATE TABLE … AS` registers the table in the
//! Data Catalog over those objects, so a later `SELECT` reads back the rows that were
//! written and `SHOW PARTITIONS` reports the partitions they were written into; an `UNLOAD`
//! registers nothing and its execution names a DATA MANIFEST that lists exactly the objects
//! it wrote. A destination that ALREADY holds objects is refused rather than written over,
//! and a `partitioned_by` that does not name the last columns the query projects is refused
//! rather than written as if it did.
//!
//! Every suite asserts that rule over a TABLE of inputs rather than one statement: the rows
//! differ along the format written, how many columns the output is partitioned by, the
//! values the rows carry (text, a NULL, several partitions), the statement that writes them
//! and — for the refusals — what makes the destination or the property wrong.
//!
//! Athena, Glue and Amazon S3 are mounted on ONE shared engine and driven over the wire the
//! production front door uses, so what is asserted is what an SDK client observes: the
//! catalog entries Glue holds, the objects S3 holds and the rows Athena answers with.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::catalog_sql::ERROR_TYPE_PATH_ALREADY_EXISTS;
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_MANIFEST_SUFFIX, QUERY_EXECUTION_COMPLETION, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table, every unload and every results object here lives in.
const BUCKET: &str = "athena-written-output";

/// Where the queries write their results, which is also where a data manifest lands.
const RESULTS: &str = "s3://athena-written-output/results/";

/// The database every table here lives in.
const DATABASE: &str = "output_lake";

/// The source table every statement below reads: three rows over two partition values, one
/// of them carrying no name at all.
const SOURCE: &str = "src";

/// The rows [`SOURCE`] holds, as the delimited text its objects are written in. The third
/// row's name is Hive's own null text, so one row of every output carries a NULL.
const SOURCE_CSV: &str = "1,north,2024-01-01\n2,south,2024-01-02\n3,\\N,2024-01-01\n";

/// How long past [`QUERY_EXECUTION_COMPLETION`] a poll waits for a started query to be
/// readable in its terminal state. Generous: what is asserted is that the execution reaches
/// a terminal state, never how fast the host got there.
const SETTLE_GRACE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

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
/// service: the catalog Glue holds is the catalog Athena writes, and the objects S3 holds are
/// the data Athena writes and reads.
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

/// One Athena call that must answer 200.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Glue call, and its status and body.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(router, "glue", "AWSGlue", operation, body).await
}

/// One path-style request to the emulator's Amazon S3 front door, answering the BYTES it
/// served — an object of a compressed format is not text — and the header `header` carried
/// beside them.
async fn s3_header(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    header: &str,
) -> (u16, Option<String>) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let carried = response
        .headers()
        .get(header)
        .and_then(|value| value.to_str().ok())
        .map(::std::string::ToString::to_string);
    (response.status().as_u16(), carried)
}

/// One path-style request to the emulator's Amazon S3 front door, answering the BYTES it
/// served — an object of a compressed format is not text.
async fn s3_bytes(
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

/// The same request, with the body as text.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (u16, String) {
    let (status, served) = s3_bytes(router, method, path, body).await;
    (status, String::from_utf8_lossy(&served).to_string())
}

/// Write one object into [`BUCKET`].
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: Vec<u8>) {
    let (status, answer) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: writing s3://{BUCKET}/{key} must succeed, got {status} with {answer}"
    );
}

/// The keys `ListObjectsV2` lists under `prefix`, sorted.
async fn listed_keys(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        Vec::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: listing s3://{BUCKET}/{prefix} must answer 200, got {status} with {body}"
    );
    let mut keys: Vec<String> = body
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split_once("</Key>"))
        .map(|(key, _)| key.to_string())
        .collect();
    keys.sort();
    keys
}

/// The bytes the object at `uri` holds, which must be there.
async fn object_bytes(router: &MultiServiceRouter, case: &str, uri: &str) -> Vec<u8> {
    let path = uri
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| panic!("case {case:?}: expected an object of {BUCKET}, got {uri:?}"));
    let (status, body) = s3_bytes(router, "GET", &format!("/{BUCKET}/{path}"), Vec::new()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {uri} must answer 200, got {status}"
    );
    body
}

/// The bytes the object at `uri` holds, as text.
async fn object_body(router: &MultiServiceRouter, case: &str, uri: &str) -> String {
    String::from_utf8_lossy(&object_bytes(router, case, uri).await).to_string()
}

/// Create the bucket, the database and the source table every suite here starts from.
async fn given_a_source_table(router: &MultiServiceRouter) {
    let (status, answer) = s3(router, "PUT", &format!("/{BUCKET}"), Vec::new()).await;
    assert!(
        (200..300).contains(&status),
        "setup: creating s3://{BUCKET} must succeed, got {status} with {answer}"
    );
    let (status, created) = glue(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the database {DATABASE} must be created, got {status} with {created}"
    );
    put_object(
        router,
        "setup",
        &format!("{SOURCE}/rows.csv"),
        SOURCE_CSV.as_bytes().to_vec(),
    )
    .await;
    let (status, created) = glue(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": SOURCE,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{SOURCE}/"),
                    "Columns": [
                        { "Name": "id", "Type": "int" },
                        { "Name": "name", "Type": "string" },
                        { "Name": "dt", "Type": "string" },
                    ],
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "OutputFormat":
                        "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
                        "Parameters": { "field.delim": "," },
                    },
                },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the table {SOURCE} must be created, got {status} with {created}"
    );
}

/// Start `sql` and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let started = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
            "ResultConfiguration": { "OutputLocation": RESULTS },
        }),
    )
    .await;
    started["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {started}"))
        .to_string()
}

/// Poll `id` until it is terminal and answer its `QueryExecution`.
///
/// POLLED rather than slept through, and polled for the CONDITION: the suites start every
/// row's query before reading any of them back, so the span a completion is owed after
/// elapses once for the whole table instead of once per row.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let attempts = ((QUERY_EXECUTION_COMPLETION + SETTLE_GRACE).as_millis()
        / POLL_INTERVAL.as_millis())
        + 2;
    for _ in 0..attempts {
        let body = athena_ok(
            router,
            case,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        let execution = body["QueryExecution"].clone();
        let state = execution["Status"]["State"].as_str().unwrap_or_default();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return execution;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the execution {id} never reached a terminal state");
}

/// The `QueryExecution` of `sql`, run to its terminal state.
async fn run(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
    let id = start(router, case, sql).await;
    settled(router, case, &id).await
}

/// Assert `execution` SUCCEEDED.
fn succeeded(case: &str, execution: &Value) {
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the execution must succeed, got {}",
        execution["Status"]
    );
}

/// The `StateChangeReason` of a FAILED execution.
fn failure_reason(case: &str, execution: &Value) -> String {
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: the execution must fail, got {}",
        execution["Status"]
    );
    execution["Status"]["StateChangeReason"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a failed execution must say why, got {}",
                execution["Status"]
            )
        })
        .to_string()
}

/// The data rows `id` answers with: one entry per row, each cell `None` for a NULL.
async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    labelled: bool,
) -> Vec<Vec<Option<String>>> {
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
        .skip(usize::from(labelled))
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

/// The rows every statement here writes, as a read of them reports them.
fn source_rows() -> Vec<Vec<Option<String>>> {
    let row = |id: &str, name: Option<&str>, dt: &str| {
        vec![
            Some(id.to_string()),
            name.map(::std::string::ToString::to_string),
            Some(dt.to_string()),
        ]
    };
    vec![
        row("1", Some("north"), "2024-01-01"),
        row("2", Some("south"), "2024-01-02"),
        row("3", None, "2024-01-01"),
    ]
}

/// One row of the table-writing suite: what the statement declares, and what it must leave.
struct Written {
    /// What the row is, which is also the prefix the table's data lands under.
    case: &'static str,
    /// The `format` property it declares, or `None` to declare none at all.
    format: Option<&'static str>,
    /// Whether the output is partitioned by the query's last column.
    partitioned: bool,
    /// The extension the objects written carry.
    extension: &'static str,
}

/// RULE: a `CREATE TABLE … AS <query>` writes the rows its query produced as objects of the
/// format it names, under the location it names and in the Hive layout its `partitioned_by`
/// asks for, and registers the table over them — so a later read of that table answers with
/// the rows that were written and `SHOW PARTITIONS` reports the partitions they went into.
///
/// The rows differ along the format written (each one this engine writes, and the default a
/// statement that declares none takes) and along whether the output is partitioned.
#[tokio::test]
async fn a_table_defined_by_a_query_writes_its_rows_and_is_read_back_from_them() {
    let router = emulator();
    given_a_source_table(&router).await;

    // (case, the format declared, partitioned, the extension the objects carry)
    let cases: Vec<Written> = vec![
        Written {
            case: "parquet_partitioned",
            format: Some("PARQUET"),
            partitioned: true,
            extension: "parquet",
        },
        Written {
            case: "json_partitioned",
            format: Some("JSON"),
            partitioned: true,
            extension: "json.gz",
        },
        Written {
            case: "textfile_partitioned",
            format: Some("TEXTFILE"),
            partitioned: true,
            extension: "txt.gz",
        },
        Written {
            case: "parquet_whole",
            format: Some("PARQUET"),
            partitioned: false,
            extension: "parquet",
        },
        Written {
            case: "json_whole",
            format: Some("JSON"),
            partitioned: false,
            extension: "json.gz",
        },
        Written {
            case: "textfile_whole",
            format: Some("TEXTFILE"),
            partitioned: false,
            extension: "txt.gz",
        },
        Written {
            // No `format` property at all: the documented default is Parquet.
            case: "default_format",
            format: None,
            partitioned: true,
            extension: "parquet",
        },
    ];

    // Every statement is started first, so the span one completion is owed after elapses
    // once for the whole table.
    let mut started: Vec<(&Written, String)> = Vec::new();
    for case in &cases {
        let properties: Vec<String> = case
            .format
            .map(|format| format!("format = '{format}'"))
            .into_iter()
            .chain(
                case.partitioned
                    .then(|| "partitioned_by = ARRAY['dt']".to_string()),
            )
            .chain(::std::iter::once(format!(
                "external_location = 's3://{BUCKET}/{}/'",
                case.case
            )))
            .collect();
        let sql = format!(
            "CREATE TABLE \"{DATABASE}\".\"{}\" WITH ({}) AS SELECT id, name, dt FROM \
             \"{DATABASE}\".\"{SOURCE}\"",
            case.case,
            properties.join(", ")
        );
        started.push((case, start(&router, case.case, &sql).await));
    }

    for (case, id) in &started {
        let execution = settled(&router, case.case, id).await;
        succeeded(case.case, &execution);

        // WHERE the objects landed: one object per partition the rows fall into, each under
        // the Hive directory that partition's value names, and one object for the whole
        // output where nothing partitions it.
        let keys = listed_keys(&router, case.case, &format!("{}/", case.case)).await;
        let directories: Vec<String> = keys
            .iter()
            .map(|key| key.rsplit_once('/').map_or(String::new(), |(at, _)| at.to_string()))
            .collect();
        let expected: Vec<String> = if case.partitioned {
            vec![
                format!("{}/dt=2024-01-01", case.case),
                format!("{}/dt=2024-01-02", case.case),
            ]
        } else {
            vec![case.case.to_string()]
        };
        assert_eq!(
            directories, expected,
            "case {:?}: expected the objects under {expected:?}, got {keys:?}",
            case.case
        );
        for key in &keys {
            assert!(
                key.ends_with(&format!(".{}", case.extension)),
                "case {:?}: expected an object of the {} format, got {key:?}",
                case.case,
                case.extension
            );
            // An object of a TEXT format is written COMPRESSED, which is what its key's
            // extension says it is: the gzip stream any reader of that codec reads.
            if case.extension.ends_with(".gz") {
                let written = object_bytes(&router, case.case, &format!("s3://{BUCKET}/{key}"))
                    .await;
                assert!(
                    written.starts_with(&[0x1f, 0x8b]),
                    "case {:?}: expected {key} to hold a gzip stream, got {} bytes starting \
                     {:?}",
                    case.case,
                    written.len(),
                    written.first().copied()
                );
            }
        }
    }

    // The rows the new table holds, read back through the catalog entry the statement
    // registered, and the partitions that entry carries.
    let mut reads: Vec<(&Written, String, String)> = Vec::new();
    for (case, _) in &started {
        let select = start(
            &router,
            case.case,
            &format!(
                "SELECT id, name, dt FROM \"{DATABASE}\".\"{}\" ORDER BY id",
                case.case
            ),
        )
        .await;
        let partitions = start(
            &router,
            case.case,
            &format!("SHOW PARTITIONS \"{DATABASE}\".\"{}\"", case.case),
        )
        .await;
        reads.push((case, select, partitions));
    }

    for (case, select, partitions) in &reads {
        let execution = settled(&router, case.case, select).await;
        succeeded(case.case, &execution);
        let actual = rows_of(&router, case.case, select, true).await;
        assert_eq!(
            actual,
            source_rows(),
            "case {:?}: expected the rows that were written, got {actual:?}",
            case.case
        );

        let execution = settled(&router, case.case, partitions).await;
        succeeded(case.case, &execution);
        let reported = rows_of(&router, case.case, partitions, false).await;
        let expected: Vec<Vec<Option<String>>> = if case.partitioned {
            vec![
                vec![Some("dt=2024-01-01".to_string())],
                vec![Some("dt=2024-01-02".to_string())],
            ]
        } else {
            Vec::new()
        };
        assert_eq!(
            reported, expected,
            "case {:?}: expected SHOW PARTITIONS to report {expected:?}, got {reported:?}",
            case.case
        );
    }
}

/// RULE: an `UNLOAD` writes the rows its query produced as objects under the destination it
/// names, and the execution names a data manifest that lists exactly those objects.
///
/// The rows differ along the format written, how many rows the query produced (every row of
/// the source, one of them, and none at all) and whether the query orders them.
#[tokio::test]
async fn unload_writes_its_objects_and_a_manifest_that_lists_exactly_them() {
    let router = emulator();
    given_a_source_table(&router).await;

    // (case, the format, the query unloaded, how many objects it must write)
    let cases: Vec<(&str, &str, String, usize)> = vec![
        (
            "json",
            "JSON",
            format!("SELECT id, name FROM \"{DATABASE}\".\"{SOURCE}\""),
            1,
        ),
        (
            "parquet_ordered",
            "PARQUET",
            format!("SELECT id, name FROM \"{DATABASE}\".\"{SOURCE}\" ORDER BY id"),
            1,
        ),
        (
            "textfile_one_row",
            "TEXTFILE",
            format!("SELECT id FROM \"{DATABASE}\".\"{SOURCE}\" WHERE id = 1"),
            1,
        ),
        (
            // A query that produced nothing writes no data object, and its manifest is
            // empty — there is a manifest, and nothing in it.
            "no_rows",
            "JSON",
            format!("SELECT id FROM \"{DATABASE}\".\"{SOURCE}\" WHERE id = 99"),
            0,
        ),
    ];

    let mut started: Vec<(&str, usize, String)> = Vec::new();
    for (case, format, query, objects) in &cases {
        let sql = format!(
            "UNLOAD ({query}) TO 's3://{BUCKET}/unloaded/{case}/' WITH (format = '{format}')"
        );
        started.push((case, *objects, start(&router, case, &sql).await));
    }

    for (case, objects, id) in &started {
        let execution = settled(&router, case, id).await;
        succeeded(case, &execution);

        // The manifest the execution NAMES, which is an object of the query result location
        // keyed by the execution's own id.
        let manifest_uri = execution["Statistics"]["DataManifestLocation"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the execution must name a data manifest, got {execution}")
            })
            .to_string();
        assert_eq!(
            manifest_uri,
            format!("{}{id}{DATA_MANIFEST_SUFFIX}", RESULTS),
            "case {case:?}: the manifest must be keyed by the execution id, got {manifest_uri:?}"
        );

        let manifest = object_body(&router, case, &manifest_uri).await;
        let listed: BTreeSet<String> = manifest
            .lines()
            .filter(|line| !line.is_empty())
            .map(::std::string::ToString::to_string)
            .collect();
        let written: BTreeSet<String> =
            listed_keys(&router, case, &format!("unloaded/{case}/"))
                .await
                .into_iter()
                .map(|key| format!("s3://{BUCKET}/{key}"))
                .collect();
        assert_eq!(
            listed, written,
            "case {case:?}: the manifest must list exactly the objects written, listed \
             {listed:?} and wrote {written:?}"
        );
        assert_eq!(
            written.len(),
            *objects,
            "case {case:?}: expected {objects} object(s) under the destination, got {written:?}"
        );
        for object in &written {
            let body = object_body(&router, case, object).await;
            assert!(
                !body.is_empty(),
                "case {case:?}: the object {object} must hold the rows that were written"
            );
        }

        // The same manifest, read through the operation that pages it in rows.
        let rows = rows_of(&router, case, id, false).await;
        let paged: BTreeSet<String> = athena_ok(
            &router,
            case,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id, "QueryResultType": "DATA_MANIFEST" }),
        )
        .await["ResultSet"]["Rows"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|row| row["Data"][0]["VarCharValue"].as_str())
            .map(::std::string::ToString::to_string)
            .collect();
        assert_eq!(
            paged, listed,
            "case {case:?}: the manifest read in rows must be the manifest object, got \
             {paged:?}"
        );
        assert!(
            rows.is_empty(),
            "case {case:?}: an UNLOAD answers with no rows of its own, got {rows:?}"
        );
    }
}

/// RULE: a destination that already holds objects is REFUSED: the statement fails naming the
/// collision, nothing is written over what is there, and no table is registered.
///
/// The rows differ along the statement that writes (an unload and a table definition) and
/// along whether the destination is occupied at all.
#[tokio::test]
async fn a_destination_that_already_holds_objects_is_refused() {
    let router = emulator();
    given_a_source_table(&router).await;
    put_object(
        &router,
        "setup",
        "occupied/already-here.txt",
        b"held".to_vec(),
    )
    .await;

    let query = format!("SELECT id, name FROM \"{DATABASE}\".\"{SOURCE}\"");
    let unload_occupied = start(
        &router,
        "unload into an occupied prefix",
        &format!("UNLOAD ({query}) TO 's3://{BUCKET}/occupied/' WITH (format = 'PARQUET')"),
    )
    .await;
    let table_occupied = start(
        &router,
        "a table over an occupied location",
        &format!(
            "CREATE TABLE \"{DATABASE}\".\"over_occupied\" WITH (format = 'PARQUET', \
             external_location = 's3://{BUCKET}/occupied/') AS {query}"
        ),
    )
    .await;
    let unload_free = start(
        &router,
        "unload into a free prefix",
        &format!("UNLOAD ({query}) TO 's3://{BUCKET}/free/' WITH (format = 'PARQUET')"),
    )
    .await;

    // (case, the execution, the reason the refusal must carry — which names the destination
    // the statement wrote, separator and all)
    let refusals: Vec<(&str, &String, String)> = vec![
        (
            "unload into an occupied prefix",
            &unload_occupied,
            format!(
                "HIVE_PATH_ALREADY_EXISTS: Target directory already exists: \
                 s3://{BUCKET}/occupied/"
            ),
        ),
        (
            "a table over an occupied location",
            &table_occupied,
            format!(
                "HIVE_PATH_ALREADY_EXISTS: Target directory for table \
                 '{DATABASE}.over_occupied' already exists: s3://{BUCKET}/occupied/"
            ),
        ),
    ];
    for (case, id, expected) in &refusals {
        let execution = settled(&router, case, id).await;
        let reason = failure_reason(case, &execution);
        assert_eq!(
            &reason, expected,
            "case {case:?}: expected the refusal {expected:?}, got {reason:?}"
        );
        // The class the refusal belongs to, which a client branches on instead of reading
        // the message: the caller's own input, of Athena's published type for a destination
        // that is already there.
        let error = execution["Status"]["AthenaError"].clone();
        assert_eq!(
            (
                error["ErrorCategory"].as_i64(),
                error["ErrorType"].as_i64()
            ),
            (Some(2), Some(i64::from(ERROR_TYPE_PATH_ALREADY_EXISTS))),
            "case {case:?}: expected the user-error category and the path-already-exists \
             type, got {error}"
        );
    }

    // Nothing was written over what the destination held, and the table the refused
    // definition named is not registered.
    let kept = listed_keys(&router, "the occupied prefix", "occupied/").await;
    assert_eq!(
        kept,
        vec!["occupied/already-here.txt".to_string()],
        "case \"the occupied prefix\": nothing may be written over it, got {kept:?}"
    );
    let (status, answer) = glue(
        &router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": "over_occupied" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a table over an occupied location\": the table must not be registered, got \
         {status} with {answer}"
    );

    // The control row: the same statement into a prefix that holds nothing succeeds.
    let execution = settled(&router, "unload into a free prefix", &unload_free).await;
    succeeded("unload into a free prefix", &execution);
    let written = listed_keys(&router, "unload into a free prefix", "free/").await;
    assert_eq!(
        written.len(),
        1,
        "case \"unload into a free prefix\": expected one object written, got {written:?}"
    );
}

/// RULE: the columns a `partitioned_by` names must be the LAST columns the query projects,
/// in that order; a statement whose projection is laid out otherwise is refused naming that
/// rule, and nothing is written.
///
/// The rows differ along how the projection is laid out: the keys last, one key first, two
/// keys in the other order, a key the query does not project at all, and the layouts that
/// satisfy the rule.
#[tokio::test]
async fn the_partition_columns_must_be_the_last_columns_the_query_projects() {
    let router = emulator();
    given_a_source_table(&router).await;

    // (case, the `partitioned_by` list, the projection, the refusal expected)
    let cases: Vec<(&str, &str, &str, Option<&str>)> = vec![
        ("one key last", "ARRAY['dt']", "id, name, dt", None),
        (
            "two keys last, in the property's order",
            "ARRAY['name', 'dt']",
            "id, name, dt",
            None,
        ),
        (
            "the key projected first",
            "ARRAY['dt']",
            "dt, id, name",
            Some("dt"),
        ),
        (
            "two keys in the other order",
            "ARRAY['name', 'dt']",
            "id, dt, name",
            Some("name, dt"),
        ),
        (
            "a key the query does not project",
            "ARRAY['missing']",
            "id, name",
            Some("missing"),
        ),
    ];

    let mut started: Vec<(&str, Option<&str>, String)> = Vec::new();
    for (index, (case, keys, projection, refusal)) in cases.iter().enumerate() {
        let sql = format!(
            "CREATE TABLE \"{DATABASE}\".\"layout_{index}\" WITH (format = 'PARQUET', \
             partitioned_by = {keys}, external_location = 's3://{BUCKET}/layout_{index}/') \
             AS SELECT {projection} FROM \"{DATABASE}\".\"{SOURCE}\""
        );
        started.push((case, *refusal, start(&router, case, &sql).await));
    }

    for (index, (case, refusal, id)) in started.iter().enumerate() {
        let execution = settled(&router, case, id).await;
        match refusal {
            None => succeeded(case, &execution),
            Some(keys) => {
                let reason = failure_reason(case, &execution);
                let expected = format!(
                    "INVALID_TABLE_PROPERTY: Partition keys must be the last columns in the \
                     table and in the same order as the table property 'partitioned_by': \
                     [{keys}]"
                );
                assert_eq!(
                    reason, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {reason:?}"
                );
                let written = listed_keys(&router, case, &format!("layout_{index}/")).await;
                assert!(
                    written.is_empty(),
                    "case {case:?}: a refused statement writes nothing, got {written:?}"
                );
            }
        }
    }
}

/// RULE: a read of a PARTITIONED table scans the data of the partitions its own predicate
/// admits and no others — so what a filtered read reports as the data it scanned is the size
/// of those partitions' objects — while the rows it answers with are the rows the predicate
/// keeps either way.
///
/// The rows differ along what the predicate says about the partition key: one value, a list
/// of them, a value no partition carries, a predicate on another column, and no predicate at
/// all.
#[tokio::test]
async fn a_read_of_a_partitioned_table_scans_only_the_partitions_its_predicate_admits() {
    let router = emulator();
    given_a_source_table(&router).await;

    // The partitioned table the reads below are of, written by a statement of its own.
    let ctas = run(
        &router,
        "setup",
        &format!(
            "CREATE TABLE \"{DATABASE}\".\"pruned\" WITH (format = 'PARQUET', \
             partitioned_by = ARRAY['dt'], external_location = 's3://{BUCKET}/pruned/') \
             AS SELECT id, name, dt FROM \"{DATABASE}\".\"{SOURCE}\""
        ),
    )
    .await;
    succeeded("setup", &ctas);

    // How many bytes each partition's objects hold, which is what a read of that partition
    // scans.
    let mut sizes: Vec<(String, usize)> = Vec::new();
    for key in listed_keys(&router, "setup", "pruned/").await {
        let partition = key
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let held = object_bytes(&router, "setup", &format!("s3://{BUCKET}/{key}"))
            .await
            .len();
        sizes.push((partition, held));
    }
    let scanned_by = |partitions: &[&str]| -> usize {
        sizes
            .iter()
            .filter(|(partition, _)| partitions.contains(&partition.as_str()))
            .map(|(_, held)| held)
            .sum()
    };

    // (case, the predicate, the bytes the read may scan, how many rows it answers)
    let cases: Vec<(&str, String, usize, usize)> = vec![
        (
            "one partition value",
            "WHERE dt = '2024-01-01'".to_string(),
            scanned_by(&["dt=2024-01-01"]),
            2,
        ),
        (
            "the other partition value",
            "WHERE dt = '2024-01-02'".to_string(),
            scanned_by(&["dt=2024-01-02"]),
            1,
        ),
        (
            "a list of both values",
            "WHERE dt IN ('2024-01-01', '2024-01-02')".to_string(),
            scanned_by(&["dt=2024-01-01", "dt=2024-01-02"]),
            3,
        ),
        (
            "a value no partition carries",
            "WHERE dt = '2024-12-31'".to_string(),
            0,
            0,
        ),
        (
            "a predicate on a column that is no partition key",
            "WHERE id = 1".to_string(),
            scanned_by(&["dt=2024-01-01", "dt=2024-01-02"]),
            1,
        ),
        (
            "no predicate at all",
            String::new(),
            scanned_by(&["dt=2024-01-01", "dt=2024-01-02"]),
            3,
        ),
        (
            // A statement that reads MORE than this one relation narrows nothing: a column
            // its predicate names is resolved against the relations in scope, which the
            // read layer does not do, so every partition is scanned — and the second
            // relation's own data with them — and the engine's evaluation of the predicate
            // is what decides the rows.
            "a predicate beside a second relation",
            format!(
                "WHERE dt = '2024-01-01' AND id IN (SELECT id FROM \"{DATABASE}\".\"{SOURCE}\")"
            ),
            scanned_by(&["dt=2024-01-01", "dt=2024-01-02"]) + SOURCE_CSV.len(),
            2,
        ),
    ];

    let mut started: Vec<(&str, usize, usize, String)> = Vec::new();
    for (case, predicate, expected, rows) in &cases {
        let id = start(
            &router,
            case,
            &format!("SELECT id, name, dt FROM \"{DATABASE}\".\"pruned\" {predicate}"),
        )
        .await;
        started.push((case, *expected, *rows, id));
    }

    for (case, expected, rows, id) in &started {
        let execution = settled(&router, case, id).await;
        succeeded(case, &execution);
        let scanned = execution["Statistics"]["DataScannedInBytes"]
            .as_u64()
            .unwrap_or_default();
        assert_eq!(
            usize::try_from(scanned).unwrap_or(usize::MAX),
            *expected,
            "case {case:?}: expected the read to scan {expected} bytes, got {scanned}"
        );
        let answered = rows_of(&router, case, id, true).await;
        assert_eq!(
            answered.len(),
            *rows,
            "case {case:?}: expected {rows} row(s), got {answered:?}"
        );
    }
}

/// RULE: the DATA a statement writes is stored under the same server-side encryption as the
/// results of that execution — the configuration its submission resolved — because every
/// object one execution leaves in the store is written under one rule.
///
/// The rows differ along the statement that writes the data: an unload, and a table defined
/// from a query.
#[tokio::test]
async fn the_data_a_statement_writes_is_stored_under_the_executions_encryption() {
    let router = emulator();
    given_a_source_table(&router).await;

    let query = format!("SELECT id, name FROM \"{DATABASE}\".\"{SOURCE}\"");
    // (case, the statement, the prefix its data lands under)
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "an unload",
            format!(
                "UNLOAD ({query}) TO 's3://{BUCKET}/sealed_unload/' WITH (format = 'JSON')"
            ),
            "sealed_unload/",
        ),
        (
            "a table defined from a query",
            format!(
                "CREATE TABLE \"{DATABASE}\".\"sealed\" WITH (format = 'PARQUET', \
                 external_location = 's3://{BUCKET}/sealed_table/') AS {query}"
            ),
            "sealed_table/",
        ),
    ];

    let mut started: Vec<(&str, &str, String)> = Vec::new();
    for (case, sql, prefix) in &cases {
        // The submission asks for Amazon S3-managed keys, which is what the objects of this
        // execution are then sealed under.
        let answer = athena_ok(
            &router,
            case,
            "StartQueryExecution",
            &json!({
                "QueryString": sql,
                "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
                "ResultConfiguration": {
                    "OutputLocation": RESULTS,
                    "EncryptionConfiguration": { "EncryptionOption": "SSE_S3" },
                },
            }),
        )
        .await;
        let id = answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {answer}"))
            .to_string();
        started.push((case, prefix, id));
    }

    for (case, prefix, id) in &started {
        let execution = settled(&router, case, id).await;
        succeeded(case, &execution);
        let keys = listed_keys(&router, case, prefix).await;
        assert!(
            !keys.is_empty(),
            "case {case:?}: the statement must write its data under {prefix}"
        );
        for key in &keys {
            let (status, sealed) = s3_header(
                &router,
                "GET",
                &format!("/{BUCKET}/{key}"),
                "x-amz-server-side-encryption",
            )
            .await;
            assert_eq!(
                (status, sealed.as_deref()),
                (200, Some("AES256")),
                "case {case:?}: expected {key} to be stored under the execution's own \
                 encryption, got {status} with {sealed:?}"
            );
        }
    }
}

/// RULE: a `format` property naming a file format this engine does not write is reported,
/// and nothing is written — never written as some other format.
///
/// The rows differ along the format named: the two columnar container formats Athena takes
/// that nothing here writes, and a value that names no format at all.
#[tokio::test]
async fn a_format_this_engine_cannot_write_is_reported() {
    let router = emulator();
    given_a_source_table(&router).await;

    for format in ["ORC", "AVRO", "NOT_A_FORMAT"] {
        let execution = run(
            &router,
            format,
            &format!(
                "UNLOAD (SELECT id FROM \"{DATABASE}\".\"{SOURCE}\") TO \
                 's3://{BUCKET}/refused_{format}/' WITH (format = '{format}')"
            ),
        )
        .await;
        let reason = failure_reason(format, &execution);
        assert!(
            reason.contains(format),
            "case {format:?}: the refusal must name the format asked for, got {reason:?}"
        );
        let written = listed_keys(&router, format, &format!("refused_{format}/")).await;
        assert!(
            written.is_empty(),
            "case {format:?}: nothing may be written for a refused statement, got {written:?}"
        );
    }
}
