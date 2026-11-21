// SPDX-License-Identifier: CC0-1.0

//! How a FAILED query execution that MANIPULATES data is classified: the `AthenaError` a
//! read of such an execution reports.
//!
//! The rule under test, stated over the class and not over one statement: a query execution
//! that settled FAILED reports an `AthenaError` carrying BOTH halves of Athena's
//! standardized classification — the `ErrorCategory` of whose doing the failure was (the
//! model's `@range` 1 to 3) and the `ErrorType`, "an integer value that provides specific
//! information about an Athena query error" (the model's `@range` 0 to 9999) — whatever KIND
//! of statement failed. A statement that WRITES data is classified exactly as one that reads
//! it: the two paths cannot report the same condition two different ways, and neither may
//! drop a half of the pair a client branches on.
//!
//! The table below is one suite whose rows differ along the form the data-manipulating
//! statement takes (`INSERT` of literal rows, `INSERT` of a projection, `UPDATE`, `DELETE`,
//! `CREATE TABLE AS SELECT`) and along the class the failure belongs to (a write target the
//! statement's data catalog does not hold, and a relation a read inside the statement cannot
//! resolve), with a plain `SELECT` of the same dropped table as the READ row of the same
//! condition. Each row is started against the catalog state the seeding leaves behind: a
//! database, an external table over stored objects that is READ BACK while it is registered,
//! and then dropped.
//!
//! Every answer is also read back through `BatchGetQueryExecution`, because one failure has
//! ONE classification however it is read.
//!
//! Every call goes over the wire the production front door uses: Athena through `POST /` with
//! `X-Amz-Target`, and Amazon S3 through its own REST data plane, both on one
//! `MultiServiceRouter` over ONE shared engine.

use std::ops::RangeInclusive;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::catalog_sql::{
    ERROR_CATEGORY_USER, ERROR_TYPE_ENTITY_NOT_FOUND,
};
use service_com_amazonaws_athena_amazonathena::state::query_engine::STATEMENT_TYPE_DML;
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_QUERY_DATABASE, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, settled, start};

/// The name Athena reaches the account's AWS Glue Data Catalog by.
const GLUE_CATALOG: &str = "AwsDataCatalog";

/// The database the table below is registered in.
const DATABASE: &str = "dml_shop";

/// The table every row of the table below addresses, registered and then dropped.
const TABLE: &str = "sales";

/// A table name the data catalog never holds, which the rows that READ one address.
const ABSENT_TABLE: &str = "ledger";

/// The bucket holding both the table's data and the query results.
const BUCKET: &str = "athena-dml-data";

/// The prefix the external table's data lives under.
const DATA_PREFIX: &str = "data";

/// Where every submission here writes its results.
const OUTPUT_LOCATION: &str = "s3://athena-dml-data/results/";

/// The records the object under the table's location holds, as the declared columns.
const DATA_RECORDS: &str = "1,foo\n2,bar\n";

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The values `AthenaError$ErrorType` admits: the model carries `@range` min 0, max 9999 on
/// it, so an error type a client reads is an integer inside this range.
const ERROR_TYPE_RANGE: RangeInclusive<i64> = 0..=9999;

/// The values `AthenaError$ErrorCategory` admits: the model carries `@range` min 1, max 3 on
/// it, the documented list "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_RANGE: RangeInclusive<i64> = 1..=3;

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the router claims a request by. Neither the key id nor the signature is a
/// credential: this harness turns request authentication off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Athena and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// `PUT` one Amazon S3 path over that service's own REST data plane, which must succeed.
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
        "case {case:?}: PUT {path} must succeed, got {}",
        response.status()
    );
}

/// One submission, as a client makes it: the statement, the catalog and database an
/// unqualified name resolves against, and where the results go.
///
/// Every statement below QUALIFIES the names it addresses, so the context names the database
/// the Data Catalog holds for every account rather than one this suite created.
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": {
            "Database": DEFAULT_QUERY_DATABASE,
            "Catalog": GLUE_CATALOG,
        },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// Start `sql` and assert it settled SUCCEEDED, answering with the execution that reported
/// it.
async fn succeeded(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
    let id = start(router, &query(sql)).await;
    let found = settled(router, &id).await;
    assert_eq!(
        found["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql:?} must succeed, got {found}"
    );
    found
}

/// The catalog state every row below is started against: the bucket and the object the
/// table's data lives in, the database, the external table over that data — READ BACK while
/// it is registered, so the rows that follow fail because it was DROPPED and not because it
/// was never there — and the drop itself.
async fn seed(router: &MultiServiceRouter) {
    s3_put(router, "the bucket", &format!("/{BUCKET}"), "").await;
    s3_put(
        router,
        "the table's data",
        &format!("/{BUCKET}/{DATA_PREFIX}/rows.csv"),
        DATA_RECORDS,
    )
    .await;
    succeeded(
        router,
        "the database the table lives in",
        &format!("CREATE DATABASE {DATABASE}"),
    )
    .await;
    succeeded(
        router,
        "the external table over the stored objects",
        &format!(
            "CREATE EXTERNAL TABLE {DATABASE}.{TABLE} (id INT, name STRING) ROW FORMAT \
             DELIMITED FIELDS TERMINATED BY ',' LOCATION 's3://{BUCKET}/{DATA_PREFIX}/'"
        ),
    )
    .await;
    succeeded(
        router,
        "a read of the registered table",
        &format!("SELECT id, name FROM {DATABASE}.{TABLE}"),
    )
    .await;
    succeeded(
        router,
        "dropping the table the rows below address",
        &format!("DROP TABLE IF EXISTS {DATABASE}.{TABLE}"),
    )
    .await;
}

/// One row of the classification table.
struct Case {
    /// What the row is, named in every assertion it makes.
    name: &'static str,
    /// The statement submitted.
    sql: String,
    /// The relation that is NOT there, which is what the failure has to be about: the
    /// reported reason names it.
    missing: &'static str,
    /// The `AthenaError$ErrorType` this row's failure class publishes, where the class is one
    /// with a published type of its own, and `None` where what the rule requires is that the
    /// member is THERE as an integer inside the modelled range — which is what a client that
    /// branches on the (category, type) pair reads.
    error_type: Option<i64>,
}

/// Every FAILED data-manipulating statement reports an `AthenaError` carrying the integer
/// error type beside the category, exactly as the read of the same dropped table does.
#[tokio::test]
async fn every_failed_data_manipulating_statement_reports_an_error_type_beside_its_category() {
    let router = emulator();
    seed(&router).await;

    let not_found = Some(i64::from(ERROR_TYPE_ENTITY_NOT_FOUND));
    let cases: Vec<Case> = vec![
        Case {
            name: "an INSERT of literal rows into the dropped table",
            sql: format!("INSERT INTO {DATABASE}.{TABLE} VALUES (40, 'qux')"),
            missing: TABLE,
            error_type: None,
        },
        Case {
            name: "an INSERT of a constant projection into the dropped table",
            sql: format!("INSERT INTO {DATABASE}.{TABLE} SELECT 41, 'quux'"),
            missing: TABLE,
            error_type: None,
        },
        Case {
            name: "an UPDATE of the dropped table",
            sql: format!("UPDATE {DATABASE}.{TABLE} SET name = 'qux' WHERE id = 40"),
            missing: TABLE,
            error_type: None,
        },
        Case {
            name: "a DELETE from the dropped table",
            sql: format!("DELETE FROM {DATABASE}.{TABLE} WHERE id = 40"),
            missing: TABLE,
            error_type: None,
        },
        Case {
            name: "an INSERT whose projection reads a table the catalog does not hold",
            sql: format!(
                "INSERT INTO {DATABASE}.{TABLE} SELECT id, name FROM {DATABASE}.{ABSENT_TABLE}"
            ),
            missing: ABSENT_TABLE,
            error_type: not_found,
        },
        Case {
            name: "a CREATE TABLE AS SELECT reading a table the catalog does not hold",
            sql: format!(
                "CREATE TABLE {DATABASE}.copied AS SELECT id FROM {DATABASE}.{ABSENT_TABLE}"
            ),
            missing: ABSENT_TABLE,
            error_type: not_found,
        },
        Case {
            name: "a SELECT of the same dropped table, which is the READ path",
            sql: format!("SELECT id, name FROM {DATABASE}.{TABLE}"),
            missing: TABLE,
            error_type: not_found,
        },
    ];

    let mut started: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        let id = start(&router, &query(&case.sql)).await;
        started.push((case, id));
    }

    for (case, id) in &started {
        let name = case.name;
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {name:?}: {:?} must settle FAILED, got {found}",
            case.sql
        );
        // Every row is a statement Athena classifies as DML, so what the rows below assert
        // is the classification of the DATA path and not of some other kind of statement.
        assert_eq!(
            found["StatementType"].as_str(),
            Some(STATEMENT_TYPE_DML),
            "case {name:?}: expected the {STATEMENT_TYPE_DML} statement type, got {found}"
        );
        let error = found["Status"]["AthenaError"].clone();
        let error_type = error["ErrorType"].as_i64();
        assert!(
            error_type.is_some_and(|value| ERROR_TYPE_RANGE.contains(&value)),
            "case {name:?}: expected an integer ErrorType inside {ERROR_TYPE_RANGE:?}, got \
             {error}"
        );
        if let Some(expected) = case.error_type {
            assert_eq!(
                error_type,
                Some(expected),
                "case {name:?}: expected the published error type {expected}, got {error}"
            );
        }
        let category = error["ErrorCategory"].as_i64();
        assert_eq!(
            category,
            Some(i64::from(ERROR_CATEGORY_USER)),
            "case {name:?}: a statement the caller has to change is a user error, got {error}"
        );
        assert!(
            category.is_some_and(|value| ERROR_CATEGORY_RANGE.contains(&value)),
            "case {name:?}: expected a category inside {ERROR_CATEGORY_RANGE:?}, got {error}"
        );
        assert_eq!(
            error["Retryable"].as_bool(),
            Some(false),
            "case {name:?}: the FAILED state is terminal with no automatic retry, got {error}"
        );
        let message = error["ErrorMessage"].as_str().unwrap_or_default();
        assert!(
            !message.is_empty(),
            "case {name:?}: expected a description beside the classification, got {error}"
        );
        // The failure is about the relation that is not there, so what is reported names it.
        let reason = found["Status"]["StateChangeReason"].as_str().unwrap_or_default();
        assert!(
            reason.to_ascii_lowercase().contains(case.missing),
            "case {name:?}: expected the reason to name {:?}, got {reason:?}",
            case.missing
        );
    }

    // One failure has ONE classification: the batch reader reports each execution's
    // `AthenaError` exactly as the single read of it did.
    let ids: Vec<&str> = started.iter().map(|(_, id)| id.as_str()).collect();
    let (status, batch) = call(
        &router,
        "BatchGetQueryExecution",
        &json!({ "QueryExecutionIds": ids }),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetQueryExecution must answer 200, got {status} with {batch}"
    );
    let batched = batch["QueryExecutions"].as_array().cloned().unwrap_or_default();
    for (case, id) in &started {
        let name = case.name;
        let found = batched
            .iter()
            .find(|execution| execution["QueryExecutionId"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("case {name:?}: the batch read must report {id}, got {batch}"));
        let single = settled(&router, id).await;
        assert_eq!(
            found["Status"]["AthenaError"],
            single["Status"]["AthenaError"],
            "case {name:?}: both readers must report one classification, got {found} beside \
             {single}"
        );
    }
}
