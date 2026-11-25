// SPDX-License-Identifier: CC0-1.0

//! What `INSERT INTO` does to a table the Data Catalog holds.
//!
//! The rule under test: `INSERT INTO "<db>"."<tbl>" <rows>` APPENDS the rows its `VALUES` or
//! its query produced to the table that name resolves to in the catalog — one NEW data object
//! under the table's own location, in the format that table's data is already stored in and
//! replacing none of the objects already there — and settles SUCCEEDED with `StatementType`
//! DML and `UpdateCount` the number of rows it wrote, so a later read of the table answers
//! with the union of what was there and what was inserted (Smithy:
//! `QueryExecutionStatus$State` enum QUEUED/RUNNING/SUCCEEDED/FAILED/CANCELLED,
//! `QueryExecution$StatementType`, `GetQueryResultsOutput$UpdateCount` "the number of rows
//! inserted with a CREATE TABLE AS SELECT, INSERT INTO, or UPDATE statement"). A name the
//! catalog resolves no table for is Athena's own not-found refusal for that relation, in
//! Athena's words — never the embedded engine's.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: the
//! FORMAT the target table's data is stored in (columnar, delimited text written by a
//! `CREATE TABLE … AS`, delimited text declared with a separator of its own), whether the
//! target is partitioned, the form of the rows (`VALUES` and a query over another table),
//! and whether the statement names the columns it writes — in the table's own order or in
//! another one.
//!
//! Every table and object is created over the real wire of the service that owns it, and
//! every Athena call goes through the same `MultiServiceRouter` the emulator boots.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    query_engine, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table below keeps its data in, and where the queries write results.
const BUCKET: &str = "athena-insert-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-insert-data/results/";

/// The database every table below is registered in.
const DATABASE: &str = "insert_warehouse";

/// The source table the rows a query inserts are read from.
const SOURCE: &str = "src";

/// A database the Data Catalog holds no record of at all, which a statement below names.
const ABSENT_DATABASE: &str = "no_such_db";

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The rows a result set reports, each a row of values where `None` is the SQL NULL.
type Rows = Vec<Vec<Option<String>>>;

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

/// One Athena call, and its HTTP status and parsed body.
async fn athena(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization("athena"))
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
async fn athena_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = athena(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object at `path`, which for a bare bucket name creates the bucket.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
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
        "case {case:?}: writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// The keys the object store holds under `prefix`, in the order it lists them.
async fn listed_keys(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/{BUCKET}?list-type=2&prefix={prefix}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: listing {prefix} must succeed, got {}",
        response.status()
    );
    let body = String::from_utf8_lossy(response.body()).to_string();
    body.split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split("</Key>").next().map(str::to_string))
        .collect()
}

/// Start one query and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
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

/// The `QueryExecution` of `id` once it is TERMINAL, polled until it is or
/// [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
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
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {execution}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql`, require it SUCCEEDED, and answer its execution.
async fn succeeds(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
    let id = start(router, case, sql).await;
    let execution = settled(router, case, &id).await;
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {execution}"
    );
    execution
}

/// The data rows `id` answered with, without the label row a DML result set opens with.
async fn result_rows(router: &MultiServiceRouter, case: &str, id: &str) -> Rows {
    let body = athena_ok(
        router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    body["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a result set, got {body}"))
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a row of data, got {row}"))
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().map(str::to_string))
                .collect()
        })
        .collect()
}

/// Run `sql`, require it SUCCEEDED and answer its data rows.
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Rows {
    let id = start(router, case, sql).await;
    let execution = settled(router, case, &id).await;
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {execution}"
    );
    result_rows(router, case, &id).await
}

/// One row of the appending table: the target the statement writes into, the statement
/// itself, and what the table holds afterwards.
struct Appended {
    /// What the row is.
    case: &'static str,
    /// The table the rows are appended to, which is also its own prefix under the bucket.
    table: &'static str,
    /// The `INSERT INTO` submitted.
    sql: &'static str,
    /// How many rows it writes, which is the `UpdateCount` it reports.
    inserted: i64,
    /// The statement that reads the table back, and the rows it must then answer with.
    read: &'static str,
    rows: &'static [&'static [&'static str]],
}

/// The rows every suite here starts from: one row in the source table.
const SOURCE_ROWS: &str = "1,seed\n";

/// Register the database and the tables the suites below insert into.
async fn seed(router: &MultiServiceRouter) {
    s3_put(router, "setup", &format!("/{BUCKET}"), "").await;
    s3_put(
        router,
        "setup",
        &format!("/{BUCKET}/{SOURCE}/rows.csv"),
        SOURCE_ROWS,
    )
    .await;
    // An object already under one target's location, so what an append does to objects that
    // are ALREADY there is asserted and not only what it does to an empty prefix.
    s3_put(router, "setup", &format!("/{BUCKET}/csv_tbl/rows.csv"), "9,seed\n").await;

    succeeds(
        router,
        "setup",
        &format!("CREATE DATABASE `{DATABASE}` LOCATION 's3://{BUCKET}/db/'"),
    )
    .await;
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE EXTERNAL TABLE `{DATABASE}`.`{SOURCE}` (`id` int, `name` string) \
             ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' STORED AS TEXTFILE \
             LOCATION 's3://{BUCKET}/{SOURCE}/'"
        ),
    )
    .await;
    // A table whose data is COLUMNAR, defined by a query that produced no rows at all: the
    // append is the first object under its location.
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE TABLE \"{DATABASE}\".\"parq_tbl\" WITH (format = 'PARQUET', \
             external_location = 's3://{BUCKET}/parq_tbl/') AS SELECT id, name FROM \
             \"{DATABASE}\".\"{SOURCE}\" WHERE 1 = 0"
        ),
    )
    .await;
    // A table whose data is ROW-FORMAT text, defined by a query that produced rows.
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE TABLE \"{DATABASE}\".\"text_tbl\" WITH (format = 'TEXTFILE', \
             external_location = 's3://{BUCKET}/text_tbl/') AS SELECT id, name FROM \
             \"{DATABASE}\".\"{SOURCE}\""
        ),
    )
    .await;
    // A table DECLARED with a field separator of its own, over data that is already there.
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE EXTERNAL TABLE `{DATABASE}`.`csv_tbl` (`id` int, `name` string) \
             ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' STORED AS TEXTFILE \
             LOCATION 's3://{BUCKET}/csv_tbl/'"
        ),
    )
    .await;
    // A PARTITIONED table, whose appended rows land in the directory their partition names.
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE EXTERNAL TABLE `{DATABASE}`.`part_tbl` (`id` int) \
             PARTITIONED BY (`dt` string) ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' \
             STORED AS TEXTFILE LOCATION 's3://{BUCKET}/part_tbl/'"
        ),
    )
    .await;
    // A VIEW, which is no relation rows are appended to.
    succeeds(
        router,
        "setup",
        &format!(
            "CREATE VIEW \"{DATABASE}\".\"src_view\" AS SELECT id, name FROM \
             \"{DATABASE}\".\"{SOURCE}\""
        ),
    )
    .await;
}

/// An `INSERT INTO` appends its rows to the catalog table it names, as one new object under
/// that table's location, and a later read answers with the union.
#[tokio::test]
async fn an_insert_appends_one_object_to_the_table_it_names() {
    let cases: Vec<Appended> = vec![
        Appended {
            case: "literal rows into a columnar table",
            table: "parq_tbl",
            sql: "INSERT INTO \"insert_warehouse\".\"parq_tbl\" \
                  VALUES (1, 'alpha'), (2, 'bravo')",
            inserted: 2,
            read: "SELECT id, name FROM \"insert_warehouse\".\"parq_tbl\" ORDER BY id",
            rows: &[&["1", "alpha"], &["2", "bravo"]],
        },
        Appended {
            case: "the rows of a query into a row-format table",
            table: "text_tbl",
            sql: "INSERT INTO \"insert_warehouse\".\"text_tbl\" \
                  SELECT id + 10, name FROM \"insert_warehouse\".\"src\"",
            inserted: 1,
            read: "SELECT id, name FROM \"insert_warehouse\".\"text_tbl\" ORDER BY id",
            rows: &[&["1", "seed"], &["11", "seed"]],
        },
        Appended {
            case: "a column list in the table's own order",
            table: "csv_tbl",
            sql: "INSERT INTO \"insert_warehouse\".\"csv_tbl\" (id, name) VALUES (3, 'gamma')",
            inserted: 1,
            read: "SELECT id, name FROM \"insert_warehouse\".\"csv_tbl\" ORDER BY id",
            rows: &[&["3", "gamma"], &["9", "seed"]],
        },
        Appended {
            case: "a column list in another order",
            table: "csv_tbl",
            sql: "INSERT INTO \"insert_warehouse\".\"csv_tbl\" (name, id) VALUES ('delta', 4)",
            inserted: 1,
            read: "SELECT id, name FROM \"insert_warehouse\".\"csv_tbl\" ORDER BY id",
            rows: &[&["3", "gamma"], &["4", "delta"], &["9", "seed"]],
        },
        Appended {
            case: "rows of two partitions into a partitioned table",
            table: "part_tbl",
            sql: "INSERT INTO \"insert_warehouse\".\"part_tbl\" \
                  VALUES (1, '2024-01-01'), (2, '2024-01-02'), (3, '2024-01-01')",
            inserted: 3,
            read: "SELECT id, dt FROM \"insert_warehouse\".\"part_tbl\" ORDER BY id",
            rows: &[
                &["1", "2024-01-01"],
                &["2", "2024-01-02"],
                &["3", "2024-01-01"],
            ],
        },
    ];

    let router = emulator();
    seed(&router).await;

    for case in &cases {
        let before = listed_keys(&router, case.case, &format!("{}/", case.table)).await;
        let id = start(&router, case.case, case.sql).await;
        let execution = settled(&router, case.case, &id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: {} must succeed, got {execution}",
            case.case,
            case.sql
        );
        // A statement that CHANGES data is a DML statement, and the finer kind beside that
        // class names the statement itself.
        assert_eq!(
            execution["StatementType"].as_str(),
            Some(query_engine::STATEMENT_TYPE_DML),
            "case {:?}: an append manipulates data, got {execution}",
            case.case
        );
        assert_eq!(
            execution["SubstatementType"].as_str(),
            Some("INSERT_INTO"),
            "case {:?}: unexpected substatement in {execution}",
            case.case
        );

        // The rows it wrote, as the read of its results reports them.
        let results = athena_ok(
            &router,
            case.case,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            results["UpdateCount"].as_i64(),
            Some(case.inserted),
            "case {:?}: expected {} rows inserted, got {results}",
            case.case,
            case.inserted
        );

        // ONE new object under the table's location, and every object that was already
        // there still there: an append adds to the data, it does not replace it.
        let after = listed_keys(&router, case.case, &format!("{}/", case.table)).await;
        let added: Vec<&String> = after.iter().filter(|key| !before.contains(key)).collect();
        let expected_objects = if case.table == "part_tbl" { 2 } else { 1 };
        assert_eq!(
            added.len(),
            expected_objects,
            "case {:?}: expected {expected_objects} new object(s) under {}/, got {after:?} \
             after {before:?}",
            case.case,
            case.table
        );
        for key in &before {
            assert!(
                after.contains(key),
                "case {:?}: the object {key} was already there and must still be, got \
                 {after:?}",
                case.case
            );
        }
        // Every object the statement wrote is named after the execution that wrote it, so a
        // second append can never write over the first one's data.
        for key in &added {
            assert!(
                key.contains(id.as_str()),
                "case {:?}: expected the appended object to be named after the execution \
                 {id}, got {key}",
                case.case
            );
        }

        // The data manifest of the execution lists exactly the objects it wrote, and carries
        // no column information: a manifest listing is not tabular query output.
        let manifest = athena_ok(
            &router,
            case.case,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id, "QueryResultType": "DATA_MANIFEST" }),
        )
        .await;
        assert_eq!(
            manifest["ResultSet"]["ResultSetMetadata"]["ColumnInfo"],
            json!([]),
            "case {:?}: a manifest read describes no column, got {manifest}",
            case.case
        );
        let listed: Vec<String> = manifest["ResultSet"]["Rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                row["Data"][0]["VarCharValue"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        let mut expected: Vec<String> = added
            .iter()
            .map(|key| format!("s3://{BUCKET}/{key}"))
            .collect();
        expected.sort();
        let mut listed_sorted = listed.clone();
        listed_sorted.sort();
        assert_eq!(
            listed_sorted, expected,
            "case {:?}: the manifest must list the objects written, got {listed:?}",
            case.case
        );

        // And the table now holds the union of what was there and what was inserted.
        let actual = rows_of(&router, case.case, case.read).await;
        let expected: Rows = case
            .rows
            .iter()
            .map(|row| row.iter().map(|value| Some((*value).to_string())).collect())
            .collect();
        assert_eq!(
            actual, expected,
            "case {:?}: expected the rows {expected:?}, got {actual:?}",
            case.case
        );
    }
}

/// A statement that writes into a name the catalog resolves no table for is refused in
/// ATHENA's own words for that class, naming the catalog, the database and the table and
/// where the statement wrote the name — never in the words of the engine underneath.
#[tokio::test]
async fn an_insert_into_an_absent_table_reports_athenas_own_not_found() {
    let router = emulator();
    seed(&router).await;

    // (case, the statement, the relation its refusal names)
    let cases: Vec<(&str, String, String)> = vec![
        (
            "a table the database does not hold",
            format!("INSERT INTO \"{DATABASE}\".\"nothing_here\" VALUES (1, 'x')"),
            format!("awsdatacatalog.{DATABASE}.nothing_here"),
        ),
        (
            "a table of a database the catalog does not hold",
            format!("INSERT INTO \"{ABSENT_DATABASE}\".\"nothing_here\" VALUES (1, 'x')"),
            ABSENT_DATABASE.to_string(),
        ),
    ];

    for (case, sql, named) in &cases {
        let id = start(&router, case, sql).await;
        let execution = settled(&router, case, &id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: {sql} must fail, got {execution}"
        );
        let reason = execution["Status"]["StateChangeReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            reason.contains(named),
            "case {case:?}: expected the refusal to name {named}, got {reason:?}"
        );
        assert!(
            !reason.contains("no such table"),
            "case {case:?}: the refusal must be Athena's own, got {reason:?}"
        );
    }
    // The exact sentence Athena states for a table that is not there, with the line and
    // column the statement wrote the name at: `INSERT INTO ` is twelve characters, so the
    // name begins at column 13.
    let sql = format!("INSERT INTO \"{DATABASE}\".\"nothing_here\" VALUES (1, 'x')");
    let id = start(&router, "the wording", &sql).await;
    let execution = settled(&router, "the wording", &id).await;
    assert_eq!(
        execution["Status"]["AthenaError"]["ErrorMessage"].as_str(),
        Some(
            format!(
                "TABLE_NOT_FOUND: line 1:13: Table 'awsdatacatalog.{DATABASE}.nothing_here' \
                 does not exist"
            )
            .as_str()
        ),
        "case \"the wording\": unexpected refusal in {execution}"
    );
}

/// A statement whose ANALYSIS Athena refuses — values that do not fit the columns they are
/// written into, a column list naming a column the table does not declare, and a target that
/// is a view — is refused in the class and the words Athena reports for each, before any
/// object is written.
///
/// The rows differ along which analysis rule the statement breaks. `INSERT INTO ` is twelve
/// characters, so every row names its target at line 1, column 13.
#[tokio::test]
async fn an_insert_the_analysis_refuses_reports_the_class_athena_reports() {
    let router = emulator();
    seed(&router).await;

    // (case, the statement, the message its refusal states)
    let cases: Vec<(&str, String, String)> = vec![
        (
            "fewer values than the table has columns",
            format!("INSERT INTO \"{DATABASE}\".\"csv_tbl\" VALUES (1)"),
            "TYPE_MISMATCH: line 1:13: Insert query has mismatched column count: Table has \
             2 columns but query has 1 columns"
                .to_string(),
        ),
        (
            "a column list naming a column the table does not declare",
            format!("INSERT INTO \"{DATABASE}\".\"csv_tbl\" (id, nope) VALUES (1, 'x')"),
            "COLUMN_NOT_FOUND: line 1:13: Insert column name does not exist in target \
             table: nope"
                .to_string(),
        ),
        (
            "a view, which is no relation rows are appended to",
            format!("INSERT INTO \"{DATABASE}\".\"src_view\" VALUES (1, 'x')"),
            "NOT_SUPPORTED: line 1:13: Inserting into views is not supported".to_string(),
        ),
    ];

    for (case, sql, expected) in &cases {
        let id = start(&router, case, sql).await;
        let execution = settled(&router, case, &id).await;
        assert_eq!(
            execution["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: {sql} must fail, got {execution}"
        );
        assert_eq!(
            execution["Status"]["AthenaError"]["ErrorMessage"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: unexpected refusal in {execution}"
        );
        // Nothing was written: a statement the analysis refuses has no data manifest at all.
        let (status, body) = athena(
            &router,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id, "QueryResultType": "DATA_MANIFEST" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused statement wrote no manifest, got {status} with {body}"
        );
    }
}

/// A read of a query's DATA MANIFEST is refused for a query that generates none, in the
/// sentence Athena states the rule in.
#[tokio::test]
async fn a_manifest_read_of_a_query_that_writes_nothing_is_refused() {
    let router = emulator();
    seed(&router).await;

    let id = start(&router, "a plain projection", "SELECT 1 AS one").await;
    let execution = settled(&router, "a plain projection", &id).await;
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "a plain projection must succeed, got {execution}"
    );
    let (status, body) = athena(
        &router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "QueryResultType": "DATA_MANIFEST" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a manifest read of a projection must be refused, got {status} with {body}"
    );
    let message = body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default();
    // The sentence Athena states the rule in: fixed text naming the statements that DO
    // generate a manifest, and then the query that does not.
    let expected = format!(
        "Manifest files are only available for INSERT, CTAS (CREATE TABLE AS SELECT), and \
         UNLOAD queries. query {id} does not generate a manifest file."
    );
    assert_eq!(
        message, expected,
        "expected the sentence Athena states the rule in, got {message:?}"
    );
}
