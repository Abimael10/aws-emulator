// SPDX-License-Identifier: CC0-1.0

//! What a read of a table a `CREATE TABLE … AS SELECT` wrote GOES THROUGH.
//!
//! The rule under test: how much data a query scans depends on how the objects it reads hold
//! a row. A COLUMNAR object (Parquet) holds each column apart and says in its own metadata
//! where each one lives, so a read goes through the column chunks of the columns the
//! statement NAMES and no others — projecting one column of a wide table scans a fraction of
//! what projecting all of them does. Every ROW-FORMAT object holds a whole row together, so a
//! read goes through all of it however few columns the statement projects. `DataScannedInBytes`
//! is that figure either way, and it is never zero for a read that returned rows.
//!
//! The fixture is built the way a client builds one: a text table over an object this suite
//! writes, and then two tables defined from it by `CREATE TABLE … AS SELECT` — one columnar
//! and one row-format — so both hold exactly the same rows and differ only in their format.
//!
//! Athena, Glue and Amazon S3 are mounted on ONE shared engine and every call goes over the
//! wire of the service that owns it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket the tables below keep their data in.
const BUCKET: &str = "athena-ctas-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-ctas-data/results/";

/// The database every table below lives in.
const DATABASE: &str = "ctas_warehouse";

/// The source table the two definitions read.
const SOURCE_TABLE: &str = "readings";

/// The values the source table's third column carries, which the partitioned definition
/// below partitions by: two of them, so a predicate on one admits half the data.
const CATEGORIES: &[&str] = &["alpha", "beta"];

/// How many rows that table holds: enough that one column's chunk is a small share of the
/// object, and few enough that the suite stays fast.
const ROWS: usize = 200;

/// How wide the second column of each row is: the column a narrow projection does NOT read,
/// and the one that makes up most of the object.
const PAD_WIDTH: usize = 400;

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A read of a COLUMNAR table goes through the columns the statement names; a read of a
/// ROW-FORMAT table goes through the whole object whatever it projects.
#[tokio::test]
async fn a_columnar_read_goes_through_the_columns_the_statement_names() {
    let router = emulator();
    seed(&router).await;

    // (case, the table defined, its format)
    let tables: Vec<(&str, &str, &str)> = vec![
        ("the columnar format", "parq_readings", "PARQUET"),
        ("a row format", "text_readings", "TEXTFILE"),
    ];
    // Both definitions are STARTED before either is waited on, so the span a completion is
    // owed after the submit is waited out once rather than once per table.
    let mut defined: Vec<(&str, &str, String)> = Vec::new();
    for (case, table, format) in &tables {
        let sql = format!(
            "CREATE TABLE {table} WITH (format = '{format}', \
             external_location = 's3://{BUCKET}/{table}/') AS SELECT n, pad FROM {SOURCE_TABLE}"
        );
        let id = start_query(&router, case, &sql).await;
        defined.push((case, table, id));
    }
    for (case, _, id) in &defined {
        let status = settled_status(&router, case, id).await;
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: the definition must succeed, got {status}"
        );
    }

    // What each table costs to read column by column, and both at once — every read started
    // before any of them is waited on.
    let mut reads: Vec<(&str, String, String, String)> = Vec::new();
    for (case, table, _) in &tables {
        let narrow =
            start_query(&router, case, &format!("SELECT n FROM {table} ORDER BY n")).await;
        let other = start_query(&router, case, &format!("SELECT pad FROM {table}")).await;
        let wide = start_query(
            &router,
            case,
            &format!("SELECT n, pad FROM {table} ORDER BY n"),
        )
        .await;
        reads.push((case, narrow, other, wide));
    }
    let mut scanned: Vec<(&str, i64, i64, i64)> = Vec::new();
    for (case, narrow, other, wide) in &reads {
        scanned.push((
            case,
            scan_of(&router, case, narrow, ROWS).await,
            scan_of(&router, case, other, ROWS).await,
            scan_of(&router, case, wide, ROWS).await,
        ));
    }

    for (case, narrow, other, wide) in &scanned {
        assert!(
            *narrow > 0,
            "case {case:?}: a read that returned rows must scan something, got {narrow}"
        );
        if *case == "the columnar format" {
            assert!(
                narrow < wide,
                "case {case:?}: projecting one column must go through less than projecting \
                 both, got {narrow} and {wide}"
            );
            // Per-column costs ADD UP: what reading both columns goes through is exactly
            // what reading each of them goes through, because each column carries its own
            // chunks and its own share of the object's metadata.
            assert_eq!(
                narrow + other,
                *wide,
                "case {case:?}: reading both columns must cost reading each ({narrow} + \
                 {other}), got {wide}"
            );
        } else {
            assert_eq!(
                narrow, wide,
                "case {case:?}: a row-format object is read whole whatever is projected, got \
                 {narrow} and {wide}"
            );
            assert_eq!(
                other, wide,
                "case {case:?}: and so is a read of its other column, got {other} and {wide}"
            );
        }
    }
}

/// A read of a PARTITIONED columnar table goes through only the partitions its predicate
/// admits, and inside those only the columns it names.
#[tokio::test]
async fn a_pruned_columnar_read_goes_through_only_the_partitions_it_admits() {
    let router = emulator();
    seed(&router).await;

    let table = "parq_by_cat";
    let sql = format!(
        "CREATE TABLE {table} WITH (format = 'PARQUET', partitioned_by = ARRAY['cat'], \
         external_location = 's3://{BUCKET}/{table}/') AS SELECT n, pad, cat FROM {SOURCE_TABLE}"
    );
    let status = settled_status_of(&router, "a partitioned columnar table", &sql).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "{sql} must succeed, got {status}"
    );

    // Half the rows carry each category, so a predicate on one admits half the data. Every
    // read is started before any of them is waited on.
    let admitted = ROWS / CATEGORIES.len();
    let one = start_query(
        &router,
        "one partition",
        &format!("SELECT n FROM {table} WHERE cat = '{}'", CATEGORIES[0]),
    )
    .await;
    let every = start_query(
        &router,
        "every partition",
        &format!("SELECT n FROM {table}"),
    )
    .await;
    let both = start_query(
        &router,
        "every partition, both columns",
        &format!("SELECT n, pad FROM {table}"),
    )
    .await;
    let pruned = scan_of(&router, "one partition", &one, admitted).await;
    let whole = scan_of(&router, "every partition", &every, ROWS).await;
    let wide = scan_of(&router, "every partition, both columns", &both, ROWS).await;
    assert!(
        pruned > 0 && pruned < whole,
        "a read of one partition must go through some but not all of what every partition \
         holds, got {pruned} of {whole}"
    );
    assert!(
        whole < wide,
        "and inside the partitions it admits, only the column it names, got {whole} of {wide}"
    );
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
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

/// The bucket, the object and the source table every definition here reads.
async fn seed(router: &MultiServiceRouter) {
    s3_put(router, &format!("/{BUCKET}"), "").await;
    let pad = "x".repeat(PAD_WIDTH);
    let mut body = String::new();
    for row in 1..=ROWS {
        let category = CATEGORIES[row % CATEGORIES.len()];
        body.push_str(&format!("{row},{pad},{category}\n"));
    }
    s3_put(router, &format!("/{BUCKET}/{SOURCE_TABLE}/rows.csv"), &body).await;
    for sql in [
        format!("CREATE DATABASE {DATABASE}"),
        format!(
            "CREATE EXTERNAL TABLE {DATABASE}.{SOURCE_TABLE} (n int, pad string, cat string) \
             ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' \
             LOCATION 's3://{BUCKET}/{SOURCE_TABLE}/'"
        ),
    ] {
        let status = settled_status_of(router, "setup", &sql).await;
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup: {sql} must succeed, got {status}"
        );
    }
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

/// One Athena call that must succeed.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object at `path`, which for a bare bucket name creates the bucket.
async fn s3_put(router: &MultiServiceRouter, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// Require the execution `id` SUCCEEDED with `rows` data rows, and answer the
/// `DataScannedInBytes` the execution reports for it.
async fn scan_of(router: &MultiServiceRouter, case: &str, id: &str, rows: usize) -> i64 {
    let status = settled_status(router, case, id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the statement must succeed, got {status}"
    );
    let results = athena_ok(
        router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    // The label row a DML result set opens with is not a data row.
    let answered = results["ResultSet"]["Rows"]
        .as_array()
        .map_or(0, Vec::len)
        .saturating_sub(1);
    assert_eq!(
        answered, rows,
        "case {case:?}: the statement must answer {rows} rows, got {answered}"
    );
    let execution = athena_ok(
        router,
        case,
        "GetQueryExecution",
        &json!({ "QueryExecutionId": id }),
    )
    .await;
    execution["QueryExecution"]["Statistics"]["DataScannedInBytes"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the execution must report the data it scanned, got {execution}")
        })
}

/// Start `sql` and answer the `QueryExecutionId` it minted.
///
/// A suite STARTS every statement it is going to assert on before it waits on any of them:
/// a completion is owed a span after the submit, so waiting per statement would pay that
/// span once per row of the table.
async fn start_query(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let answer = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// The `Status` the execution `id` settles in, polled until it is terminal — the condition
/// is what is waited on, never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
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
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            let mut reported = reported;
            reported["QueryExecutionId"] = json!(id);
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Start `sql` and answer the `Status` it settles in, for a statement nothing else waits
/// beside.
async fn settled_status_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
    let id = start_query(router, case, sql).await;
    settled_status(router, case, &id).await
}
