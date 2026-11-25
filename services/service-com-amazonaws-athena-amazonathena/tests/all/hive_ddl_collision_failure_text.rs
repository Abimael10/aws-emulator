// SPDX-License-Identifier: CC0-1.0

//! What a query execution reports when a catalog DEFINITION collides with an object the
//! Data Catalog already holds.
//!
//! The rule under test: a definition the Hive DDL engine refuses because the object is
//! already there reports its two failure texts in the words of their own writers, and
//! neither is derived from the other —
//!
//! * `AthenaError.ErrorMessage` is the service's own sentence for the condition, closed with
//!   a full stop;
//! * `Status.StateChangeReason` is the DDL task's line followed, word for word, by what that
//!   task WROTE for the condition: the metastore's bare sentence where it wrote one, and the
//!   metastore's already-exists EXCEPTION rendering where the task wrapped that exception —
//!   which is what a DATABASE collision and a TABLE collision respectively report.
//!
//! The name the statement wrote is the only part either text takes from the request, and the
//! `QueryExecutionDetail.Status.StateChangeReason` projection carries the same text as
//! `QueryExecution.Status.StateChangeReason`.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: the KIND of
//! object the definition names (database, table), the spelling of the statement
//! (`DATABASE`/`SCHEMA`), and the name itself (one character, a name carrying digits and
//! underscores, a long name) — so a field that must not vary with the request is seen not to.
//! A definition that TOLERATES the collision (`IF NOT EXISTS`) succeeds instead, which is the
//! other side of the same rule: the texts are reported for the refusal, not for the name.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router, settled, start};

/// Where this suite's queries write their results, in a bucket the harness holds.
const OUTPUT_LOCATION: &str = "s3://athena-results/ddl-collision-text/";

/// The bucket the tables this suite defines keep their data in, which a definition's
/// `LOCATION` has to name for the definition to be accepted.
const DATA_BUCKET: &str = "athena-ddl-collision-data";

/// The database the tables of this suite are defined in.
const TABLES_DATABASE: &str = "ddlc_tables";

/// What the Hive DDL engine writes in front of what a failed task reported: the task that
/// failed and the return code it failed with, which carries nothing from the request.
const DDL_TASK_FAILED: &str =
    "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask.";

/// The `AthenaError$ErrorCategory` of a failure the submitted statement caused: the
/// documented list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// The `AthenaError$ErrorType` Athena publishes for a DDL task the metastore refused.
const ERROR_TYPE_DDL_TASK: i64 = 1006;

/// One row: a definition that must collide, and the two texts the failure must report.
struct Collision {
    /// What this row is.
    case: String,
    /// The statement submitted.
    sql: String,
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

/// Submit every statement of `statements`, then wait for each and assert it SUCCEEDED.
///
/// Every statement is submitted before any of them is polled, so the setup waits out one
/// completion span rather than one per statement.
async fn succeeds(router: &MultiServiceRouter, statements: &[String]) {
    let mut started: Vec<(&String, String)> = Vec::new();
    for sql in statements {
        started.push((sql, submit(router, sql, sql).await));
    }
    for (sql, id) in started {
        let found = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {sql:?}: the statement must succeed, got {found:?}"
        );
    }
}

/// The `CREATE EXTERNAL TABLE` that defines `table` in [`TABLES_DATABASE`] over this suite's
/// data bucket.
fn define_table(table: &str) -> String {
    format!(
        "CREATE EXTERNAL TABLE {TABLES_DATABASE}.{table} (id string, name string) ROW FORMAT \
         DELIMITED FIELDS TERMINATED BY ',' LOCATION 's3://{DATA_BUCKET}/{table}/'"
    )
}

/// Run every row and assert the two texts it names, in both places a read reports the
/// state change reason.
async fn assert_collisions(router: &MultiServiceRouter, rows: &[Collision]) {
    let mut started: Vec<(&Collision, String)> = Vec::new();
    for row in rows {
        let id = submit(router, &row.case, &row.sql).await;
        started.push((row, id));
    }
    for (row, id) in started {
        let found: Value = settled(router, &id).await;
        let case = &row.case;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: {} must fail, got {found:?}",
            row.sql
        );
        assert_eq!(
            (
                found["Status"]["AthenaError"]["ErrorCategory"].as_i64(),
                found["Status"]["AthenaError"]["ErrorType"].as_i64(),
            ),
            (Some(ERROR_CATEGORY_USER), Some(ERROR_TYPE_DDL_TASK)),
            "case {case:?}: expected the user category and the DDL task type, got {found:?}"
        );
        assert_eq!(
            found["Status"]["AthenaError"]["ErrorMessage"].as_str(),
            Some(row.message.as_str()),
            "case {case:?}: expected the error message {:?}, got {found:?}",
            row.message
        );
        assert_eq!(
            found["Status"]["StateChangeReason"].as_str(),
            Some(row.reason.as_str()),
            "case {case:?}: expected the state change reason {:?}, got {found:?}",
            row.reason
        );

        // The flattened view of the same execution carries the same text, so a client
        // reading either place is told the same thing.
        let (status, body) = call(
            router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body:?}"
        );
        assert_eq!(
            body["QueryExecutionDetail"]["Status"]["StateChangeReason"].as_str(),
            Some(row.reason.as_str()),
            "case {case:?}: expected the projected state change reason {:?}, got {body:?}",
            row.reason
        );
    }
}

/// A definition of a DATABASE the catalog already holds reports the service's sentence as the
/// error message and the task line followed by the metastore's BARE sentence as the reason.
#[tokio::test]
async fn an_already_held_database_reports_the_bare_metastore_sentence() {
    let (_engine, router) = engine_and_router();

    // The rows differ along the spelling of the statement and the name — one character, a
    // name carrying digits and underscores, a long one.
    let long_name = "c".repeat(60);
    let held = ["q", "ddlc_orders_2026", long_name.as_str()];
    let definitions: Vec<String> = held
        .iter()
        .map(|name| format!("CREATE DATABASE {name}"))
        .collect();
    succeeds(&router, &definitions).await;

    let rows: Vec<Collision> = held
        .iter()
        .enumerate()
        .map(|(index, name)| {
            // Every other row is written in the Presto spelling of the same statement, which
            // defines the same catalog database and so collides for the same reason.
            let keyword = if index % 2 == 0 { "DATABASE" } else { "SCHEMA" };
            Collision {
                case: format!("a second CREATE {keyword} naming the database {name}"),
                sql: format!("CREATE {keyword} {name}"),
                message: format!("Database {name} already exists."),
                reason: format!("{DDL_TASK_FAILED} Database {name} already exists"),
            }
        })
        .collect();

    assert_collisions(&router, &rows).await;
}

/// A definition of a TABLE the catalog already holds reports the service's sentence as the
/// error message and the task line followed by the metastore's already-exists EXCEPTION
/// rendering as the reason.
#[tokio::test]
async fn an_already_held_table_reports_the_metastores_exception_rendering() {
    let (engine, router) = engine_and_router();
    // A table is defined over storage that is already there, so the bucket its LOCATION
    // names is held before the definition is submitted.
    crate::results_bucket::hold_bucket(&engine, DATA_BUCKET);
    succeeds(&router, &[format!("CREATE DATABASE {TABLES_DATABASE}")]).await;

    // The rows differ along the table name: one character, a name carrying digits and
    // underscores, a long one.
    let long_name = "t".repeat(60);
    let tables = ["z", "items_2026", long_name.as_str()];
    let definitions: Vec<String> = tables.iter().map(|table| define_table(table)).collect();
    succeeds(&router, &definitions).await;

    let rows: Vec<Collision> = tables
        .iter()
        .map(|table| Collision {
            case: format!("a second definition of the table {table}"),
            sql: define_table(table),
            message: format!("Table {table} already exists."),
            reason: format!(
                "{DDL_TASK_FAILED} AlreadyExistsException(message:Table {table} already \
                 exist)"
            ),
        })
        .collect();

    assert_collisions(&router, &rows).await;
}

/// A definition that TOLERATES the collision changes nothing and SUCCEEDS, so neither failure
/// text is reported at all: what decides the texts is the refusal, not the name the statement
/// wrote.
#[tokio::test]
async fn a_definition_that_tolerates_the_collision_reports_no_failure_text() {
    let (engine, router) = engine_and_router();
    crate::results_bucket::hold_bucket(&engine, DATA_BUCKET);
    succeeds(&router, &[format!("CREATE DATABASE {TABLES_DATABASE}")]).await;
    succeeds(&router, &[define_table("tolerated")]).await;

    let tolerant = [
        format!("CREATE DATABASE IF NOT EXISTS {TABLES_DATABASE}"),
        format!("CREATE SCHEMA IF NOT EXISTS {TABLES_DATABASE}"),
        format!(
            "CREATE EXTERNAL TABLE IF NOT EXISTS {TABLES_DATABASE}.tolerated (id string, \
             name string) ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' LOCATION \
             's3://{DATA_BUCKET}/tolerated/'"
        ),
    ];
    let mut started: Vec<(&String, String)> = Vec::new();
    for sql in &tolerant {
        started.push((sql, submit(&router, sql, sql).await));
    }
    for (sql, id) in started {
        let found = settled(&router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {sql:?}: a definition that tolerates the collision must succeed, got \
             {found:?}"
        );
        assert_eq!(
            found["Status"]["StateChangeReason"], Value::Null,
            "case {sql:?}: a statement that did not fail reports no state change reason, got \
             {found:?}"
        );
        assert_eq!(
            found["Status"]["AthenaError"], Value::Null,
            "case {sql:?}: a statement that did not fail reports no error, got {found:?}"
        );
    }
}
