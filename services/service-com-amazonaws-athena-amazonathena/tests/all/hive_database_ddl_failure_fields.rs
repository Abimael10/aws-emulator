// SPDX-License-Identifier: CC0-1.0

//! What a catalog DDL statement the Hive engine refused reports in its TWO failure fields.
//!
//! The rule under test: an execution that failed reports `AthenaError.ErrorMessage` and
//! `Status.StateChangeReason` as the service words EACH of them for the condition that
//! failed — the error message is the sentence for the condition, the state change reason is
//! what the engine that performed the statement wrote — so neither field is the other's text
//! re-used (`AthenaError$ErrorMessage`: "Contains a short description of the error that
//! occurred"). Four conditions of the catalog DDL show it, and the name the statement wrote
//! is the only part either text takes from the request:
//!
//! * a `CREATE DATABASE`/`CREATE SCHEMA` naming a database the catalog already holds, and a
//!   `CREATE EXTERNAL TABLE` naming a table it already holds, report the metastore's
//!   sentence closed with a full stop as the error message, and the DDL task's line followed
//!   by that sentence without the stop as the reason;
//! * a `DROP DATABASE`/`DROP SCHEMA` naming a database the catalog does not hold reports the
//!   drop's own sentence as the error message, and the Hive compiler's semantic exception for
//!   the name it could not resolve as the reason;
//! * a definition in a database the catalog does not hold reports the catalog's own
//!   not-found sentence as the error message, beside that same compiler exception.
//!
//! Each suite is a TABLE whose rows differ along the spelling of the statement
//! (`DATABASE`/`SCHEMA`), the clauses it carries, the object it addresses, and the name —
//! the one variable part of both texts — so a field that must not vary with the request is
//! seen not to.
//!
//! Every call goes over the wire the production front door uses, against the Data Catalog in
//! the shared engine.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{engine_and_router, router, settled, start};

/// Where this suite's queries write their results, in a bucket the harness holds.
const OUTPUT_LOCATION: &str = "s3://athena-results/ddl-failure-fields/";

/// The bucket a table this suite defines keeps its data in, which the statement's
/// `LOCATION` has to name for the definition to be accepted.
const DATA_BUCKET: &str = "athena-ddl-fields-data";

/// The `AthenaError$ErrorCategory` of a failure the submitted statement caused: the
/// documented list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena publishes for a DDL task the metastore refused.
const ERROR_TYPE_DDL_TASK: i64 = 1006;

/// The `AthenaError$ErrorType` Athena publishes for a name the catalog does not hold.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// What the Hive DDL engine writes in front of what a failed task reported: the task that
/// failed and the return code it failed with, which carries nothing from the request.
const DDL_TASK_FAILED: &str =
    "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask.";

/// One row: a statement that must fail, and the two fields the failure must report.
struct Row {
    /// What this row is.
    case: &'static str,
    /// The statement submitted.
    sql: String,
    /// The `AthenaError$ErrorType` the failure must publish.
    error_type: i64,
    /// `AthenaError.ErrorMessage`, word for word.
    message: String,
    /// `Status.StateChangeReason`, word for word.
    reason: String,
}

/// Submit `sql` and answer the id it minted.
async fn submit(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let id = start(
        router,
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": "default", "Catalog": "AwsDataCatalog" },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    assert!(
        !id.is_empty(),
        "case {case:?}: {sql} must mint an execution id"
    );
    id
}

/// Define the databases `names`, and assert every definition succeeded.
///
/// Every statement is submitted first and only then polled, so the suite waits out ONE
/// completion span rather than one per row.
async fn define(router: &MultiServiceRouter, names: &[&str]) {
    let mut started: Vec<(&str, String)> = Vec::new();
    for name in names {
        started.push((name, submit(router, name, &format!("CREATE DATABASE {name}")).await));
    }
    for (name, id) in started {
        let found = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup: {name} must be defined, got {found:?}"
        );
    }
}

/// Run every row and assert the two fields it names.
async fn assert_rows(router: &MultiServiceRouter, rows: &[Row]) {
    let mut started: Vec<(&Row, String)> = Vec::new();
    for row in rows {
        let id = submit(router, row.case, &row.sql).await;
        started.push((row, id));
    }
    for (row, id) in started {
        let found: Value = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {:?}: {} must fail, got {found:?}",
            row.case,
            row.sql
        );
        assert_eq!(
            (
                found["Status"]["AthenaError"]["ErrorCategory"].as_i64(),
                found["Status"]["AthenaError"]["ErrorType"].as_i64(),
            ),
            (Some(ERROR_CATEGORY_USER), Some(row.error_type)),
            "case {:?}: expected the user category and type {}, got {found:?}",
            row.case,
            row.error_type
        );
        assert_eq!(
            found["Status"]["AthenaError"]["ErrorMessage"].as_str(),
            Some(row.message.as_str()),
            "case {:?}: expected the error message {:?}, got {found:?}",
            row.case,
            row.message
        );
        assert_eq!(
            found["Status"]["StateChangeReason"].as_str(),
            Some(row.reason.as_str()),
            "case {:?}: expected the state change reason {:?}, got {found:?}",
            row.case,
            row.reason
        );
    }
}

/// A definition of a database the catalog already holds reports the metastore's sentence as
/// the error message and the DDL task's own line as the reason — the sentence closed with a
/// full stop in the one, bare in the other, and in no exception rendering.
#[tokio::test]
async fn an_already_held_database_reports_the_metastores_sentence_in_both_fields() {
    let router = router();
    // The rows differ along the spelling of the statement and the database name, which is
    // the only part either text takes from the request.
    let long_name = "d".repeat(60);
    let held = ["a", "ddlf_orders", "ddlf_2026_events", long_name.as_str()];
    define(&router, &held).await;

    let mut rows: Vec<Row> = Vec::new();
    for (index, name) in held.iter().enumerate() {
        // Every other row is written in the Presto spelling of the same statement, which
        // defines the same catalog database and so is refused for the same reason.
        let keyword = if index % 2 == 0 { "DATABASE" } else { "SCHEMA" };
        rows.push(Row {
            case: "a definition of a database the catalog already holds",
            sql: format!("CREATE {keyword} {name}"),
            error_type: ERROR_TYPE_DDL_TASK,
            message: format!("Database {name} already exists."),
            reason: format!("{DDL_TASK_FAILED} Database {name} already exists"),
        });
    }

    assert_rows(&router, &rows).await;

    // The refusals left the databases as they were: each is still there, so the drop of it
    // is a drop the catalog performs rather than a not-found refusal.
    let mut dropped: Vec<(&str, String)> = Vec::new();
    for name in held {
        dropped.push((name, submit(&router, name, &format!("DROP DATABASE {name}")).await));
    }
    for (name, id) in dropped {
        let found = settled(&router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {name:?}: a refused definition leaves the database in the catalog, so the \
             drop of it succeeds, got {found:?}"
        );
    }
}

/// A drop of a database the catalog does not hold reports the DROP's own sentence as the
/// error message and the Hive compiler's semantic exception as the reason.
#[tokio::test]
async fn a_drop_of_an_absent_database_reports_each_field_in_its_own_words() {
    let router = router();

    // The rows differ along the spelling of the statement, the clause it carries, and the
    // database name — the only variable part of either text.
    let long_name = "n".repeat(60);
    let statements: Vec<(&str, String, String)> = vec![
        (
            "a drop naming a database the catalog does not hold",
            "DROP DATABASE ddlf_absent".to_string(),
            "ddlf_absent".to_string(),
        ),
        (
            "the Presto spelling of the same drop",
            "DROP SCHEMA ddlf_also_absent".to_string(),
            "ddlf_also_absent".to_string(),
        ),
        (
            "a drop that asks for the contents to go with it",
            "DROP DATABASE ddlf_absent_cascade CASCADE".to_string(),
            "ddlf_absent_cascade".to_string(),
        ),
        (
            "a drop of a database named by a single character",
            "DROP DATABASE z".to_string(),
            "z".to_string(),
        ),
        (
            "a drop of a database with a long name",
            format!("DROP DATABASE {long_name}"),
            long_name.clone(),
        ),
    ];

    let rows: Vec<Row> = statements
        .into_iter()
        .map(|(case, sql, name)| Row {
            case,
            sql,
            error_type: ERROR_TYPE_ENTITY_NOT_FOUND,
            message: format!("Cannot drop database {name}, as it does not exist."),
            reason: format!(
                "FAILED: SemanticException [Error 10072]: Database does not exist: {name}"
            ),
        })
        .collect();

    assert_rows(&router, &rows).await;
}

/// A definition of a TABLE the catalog already holds reports the same pair in the
/// metastore's own words for that object: the rule is the condition's sentence as the error
/// message and the DDL task's line in front of it as the reason, whatever object the
/// statement defines.
#[tokio::test]
async fn an_already_held_table_reports_the_metastores_sentence_in_both_fields() {
    let (engine, router) = engine_and_router();
    // A table is defined over storage that is already there, so the bucket its LOCATION
    // names is held before the definition is submitted.
    crate::results_bucket::hold_bucket(&engine, DATA_BUCKET);
    define(&router, &["ddlf_tables"]).await;

    // The rows differ along the table the statement defines and the clauses it carries;
    // each is defined once and then defined again, which is the refusal under test.
    let tables = ["items", "orders_2026", "t"];
    let mut defined: Vec<(&str, String)> = Vec::new();
    for table in tables {
        let sql = format!(
            "CREATE EXTERNAL TABLE ddlf_tables.{table} (id string, name string) ROW FORMAT \
             DELIMITED FIELDS TERMINATED BY ',' LOCATION 's3://{DATA_BUCKET}/{table}/'"
        );
        defined.push((table, submit(&router, table, &sql).await));
    }
    for (table, id) in defined {
        let found = settled(&router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup: the table {table} must be defined, got {found:?}"
        );
    }

    let rows: Vec<Row> = tables
        .iter()
        .map(|table| Row {
            case: "a definition of a table the catalog already holds",
            sql: format!(
                "CREATE EXTERNAL TABLE ddlf_tables.{table} (id string, name string) ROW \
                 FORMAT DELIMITED FIELDS TERMINATED BY ',' LOCATION \
                 's3://{DATA_BUCKET}/{table}/'"
            ),
            error_type: ERROR_TYPE_DDL_TASK,
            message: format!("Table {table} already exists."),
            reason: format!("{DDL_TASK_FAILED} Table {table} already exists"),
        })
        .collect();

    assert_rows(&router, &rows).await;
}

/// A definition whose DATABASE the catalog does not hold reports the catalog's own
/// not-found sentence as the error message and the Hive compiler's semantic exception as the
/// reason — the two fields worded independently, as every DDL refusal words them.
#[tokio::test]
async fn a_definition_in_an_absent_database_reports_each_field_in_its_own_words() {
    let (engine, router) = engine_and_router();
    crate::results_bucket::hold_bucket(&engine, DATA_BUCKET);

    // The rows differ along the database the statement names and the table it would define
    // in it; the database is the only variable part of either text.
    let absent = ["ddlf_nowhere", "ddlf_not_created_2026", "q"];
    let rows: Vec<Row> = absent
        .iter()
        .map(|database| Row {
            case: "a definition in a database the catalog does not hold",
            sql: format!(
                "CREATE EXTERNAL TABLE {database}.items (id string) ROW FORMAT DELIMITED \
                 FIELDS TERMINATED BY ',' LOCATION 's3://{DATA_BUCKET}/items/'"
            ),
            error_type: ERROR_TYPE_ENTITY_NOT_FOUND,
            // The Data Catalog's own sentence for a database it does not hold, which is
            // what the compiler could not resolve the name against.
            message: format!("Database {database} not found."),
            reason: format!(
                "FAILED: SemanticException [Error 10072]: Database does not exist: {database}"
            ),
        })
        .collect();

    assert_rows(&router, &rows).await;
}
