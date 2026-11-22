// SPDX-License-Identifier: CC0-1.0

//! A column whose value is a STRUCTURE: how it is declared, and how a statement reads into
//! it.
//!
//! The rules under test:
//!
//! * a `CREATE EXTERNAL TABLE` may declare a column with a COLLECTION or RECORD type
//!   (`array<…>`, `map<…,…>`, `struct<…>`, nested in each other), the statement SUCCEEDS,
//!   and the catalog records each column with the type text the statement wrote;
//! * a statement reads INTO such a value three ways — a record's field by name, an array's
//!   element by position counted from ONE, and a map's entry by key — and each answers the
//!   value the object carried at that place.
//!
//! The rows of the table below differ along what the rules generalise over: the kind of
//! structure, how deep the reading goes, where it sits in the statement, and the object the
//! value is read from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket the table below keeps its data in.
const BUCKET: &str = "athena-nested-json";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-nested-json/results/";

/// The database the table below lives in.
const DATABASE: &str = "nested_json";

/// The table every statement here reads.
const TABLE: &str = "documents";

/// Its one object: one JSON document per line, each carrying a list, a map and a record.
const DATA: &str = concat!(
    r#"{"id":1,"tags":["alpha","beta"],"#,
    r#""attrs":{"colour":"red","size":"L","shade.dark one":"navy"},"#,
    r#""addr":{"city":"Ely","zip":123}}"#,
    "\n",
    r#"{"id":2,"tags":["gamma"],"attrs":{"colour":"blue"},"#,
    r#""addr":{"city":"Hull","zip":456}}"#,
    "\n",
);

/// The serialization library the table is read by: one JSON document per line.
const JSON_SERDE: &str = "org.openx.data.jsonserde.JsonSerDe";

/// The columns the table declares, each with the type text the statement writes for it.
const COLUMNS: &[(&str, &str)] = &[
    ("id", "int"),
    ("tags", "array<string>"),
    ("attrs", "map<string,string>"),
    ("addr", "struct<city:string,zip:int>"),
];

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A table declared with collection and record columns is registered with those types, and a
/// statement reads into the values at every one of them.
#[tokio::test]
async fn a_statement_reads_into_the_structures_a_column_holds() {
    let router = emulator();
    seed(&router).await;

    // The catalog records each column with the type text the definition wrote.
    let metadata = athena_ok(
        &router,
        "the declared types",
        "GetTableMetadata",
        &json!({
            "CatalogName": "AwsDataCatalog",
            "DatabaseName": DATABASE,
            "TableName": TABLE,
        }),
    )
    .await;
    let reported: Vec<(String, String)> = metadata["TableMetadata"]["Columns"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|column| {
            (
                column["Name"].as_str().unwrap_or_default().to_string(),
                column["Type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = COLUMNS
        .iter()
        .map(|(name, data_type)| ((*name).to_string(), (*data_type).to_string()))
        .collect();
    assert_eq!(
        reported, expected,
        "the catalog must record the declared types {expected:?}, got {metadata}"
    );

    // (case, the statement, the rows it must answer)
    let cases: Vec<(&str, String, Vec<Vec<&str>>)> = vec![
        (
            "a record's field by name",
            format!("SELECT addr.city AS city FROM {TABLE} WHERE id = 1"),
            vec![vec!["Ely"]],
        ),
        (
            "a record's field of another type",
            format!("SELECT addr.zip AS zip FROM {TABLE} WHERE id = 2"),
            vec![vec!["456"]],
        ),
        (
            "an array's elements, counted from one",
            format!("SELECT tags[1] AS first, tags[2] AS second FROM {TABLE} WHERE id = 1"),
            vec![vec!["alpha", "beta"]],
        ),
        (
            "a map's entry by key",
            format!("SELECT attrs['colour'] AS colour FROM {TABLE} ORDER BY id"),
            vec![vec!["red"], vec!["blue"]],
        ),
        (
            "an entry one document carries and the other does not",
            format!("SELECT attrs['size'] AS size FROM {TABLE} WHERE id = 1"),
            vec![vec!["L"]],
        ),
        (
            "a key carrying the characters a path is written with",
            format!("SELECT attrs['shade.dark one'] AS shade FROM {TABLE} WHERE id = 1"),
            vec![vec!["navy"]],
        ),
        (
            "a reading in a predicate rather than in the select list",
            format!("SELECT id AS id FROM {TABLE} WHERE addr.city = 'Hull'"),
            vec![vec!["2"]],
        ),
        (
            "a reading beside a scalar column, over every row",
            format!("SELECT id AS id, addr.city AS city FROM {TABLE} ORDER BY id"),
            vec![vec!["1", "Ely"], vec!["2", "Hull"]],
        ),
    ];

    // Started first, all of them, so the span a completion is owed after the submit is
    // waited out once rather than once per row.
    let mut started: Vec<(&str, &String, &Vec<Vec<&str>>, String)> = Vec::new();
    for (case, sql, expected) in &cases {
        started.push((case, sql, expected, start_query(&router, case, sql).await));
    }
    for (case, sql, expected, id) in &started {
        let rows = rows_of(&router, case, id).await;
        let expected: Vec<Vec<Option<String>>> = expected
            .iter()
            .map(|row| row.iter().map(|cell| Some((*cell).to_string())).collect())
            .collect();
        assert_eq!(
            rows, expected,
            "case {case:?}: {sql} must answer {expected:?}, got {rows:?}"
        );
    }
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

/// The bucket, the object and the catalog table every statement here reads — the table
/// through Athena's own DDL, so what is asserted is that the DDL performs it.
async fn seed(router: &MultiServiceRouter) {
    s3_put(router, &format!("/{BUCKET}"), "").await;
    s3_put(router, &format!("/{BUCKET}/{TABLE}/rows.json"), DATA).await;
    let columns = COLUMNS
        .iter()
        .map(|(name, data_type)| format!("{name} {data_type}"))
        .collect::<Vec<String>>()
        .join(", ");
    for sql in [
        format!("CREATE DATABASE {DATABASE}"),
        format!(
            "CREATE EXTERNAL TABLE {DATABASE}.{TABLE} ({columns}) \
             ROW FORMAT SERDE '{JSON_SERDE}' LOCATION 's3://{BUCKET}/{TABLE}/'"
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

/// Require the execution `id` SUCCEEDED, and answer its data rows — the label row a DML
/// result set opens with dropped, and `None` for a datum carrying no value.
async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> Vec<Vec<Option<String>>> {
    let status = settled_status(router, case, id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the statement must succeed, got {status}"
    );
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
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().map(str::to_string))
                .collect()
        })
        .collect()
}
