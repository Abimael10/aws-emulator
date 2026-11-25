// SPDX-License-Identifier: CC0-1.0

//! What CLASS Athena puts each FORM of statement in: `QueryExecution.StatementType` and the
//! finer `SubstatementType` beside it.
//!
//! The rule under test: the class a statement belongs to follows from the FORM its grammar
//! gives it, not from what running it happens to touch. A statement that DEFINES, alters or
//! drops an object is DDL — a `CREATE TABLE` whichever way it states the rows the table will
//! hold, so the `AS <query>` form is DDL beside the plain one; a statement that queries or
//! changes DATA, and one that only writes rows a query produced without defining anything, is
//! DML; everything else — the statements that ask the engine about itself — is UTILITY
//! (Smithy: `QueryExecution$StatementType`, enum `StatementType` DDL/DML/UTILITY, and
//! `QueryExecution$SubstatementType`, "the kind of query statement that was run").
//!
//! The suite is a TABLE whose rows differ along the form submitted, so what is asserted is the
//! rule over the forms rather than the classification of one statement.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::query_engine::{
    STATEMENT_TYPE_DDL, STATEMENT_TYPE_DML, STATEMENT_TYPE_UTILITY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{router, settled, start};

/// Where the statements below write their results: a bucket the shared harness holds.
const OUTPUT_LOCATION: &str = "s3://athena-results/classified/";

/// The database the statements below address, so each sees what the one before it defined.
const DATABASE: &str = "classified";

/// One row of the table: a statement, and the two classifications a read of its execution
/// must report for it.
struct Form {
    /// What the row is, named in every assertion it makes.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `StatementType` its form is classified in.
    statement_type: &'static str,
    /// The `SubstatementType` beside it, or `None` for a form that has no such name.
    substatement_type: Option<&'static str>,
}

/// Start `sql` in the suite's database and answer the id it minted.
async fn start_query(router: &MultiServiceRouter, sql: &str) -> String {
    start(
        router,
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await
}

/// Every form of statement is classified by what its GRAMMAR makes it, and reports the finer
/// kind of statement beside that class.
#[tokio::test]
async fn every_statement_form_is_classified_by_the_form_its_grammar_gives_it() {
    let router = router();

    let cases: Vec<Form> = vec![
        Form {
            case: "a table defined column by column",
            sql: "CREATE TABLE classified_tbl (id integer, name varchar)",
            statement_type: STATEMENT_TYPE_DDL,
            substatement_type: Some("CREATE_TABLE"),
        },
        Form {
            case: "rows written into that table",
            sql: "INSERT INTO classified_tbl VALUES (1, 'desk'), (2, 'lamp')",
            statement_type: STATEMENT_TYPE_DML,
            substatement_type: Some("INSERT_INTO"),
        },
        Form {
            case: "a projection of those rows",
            sql: "SELECT id, name FROM classified_tbl ORDER BY id",
            statement_type: STATEMENT_TYPE_DML,
            substatement_type: Some("SELECT"),
        },
        Form {
            // The form this suite is here for: a definition that states its rows with a
            // query is still a DEFINITION.
            case: "a table defined by the rows a query produced",
            sql: "CREATE TABLE classified_ctas AS SELECT id, name FROM classified_tbl",
            statement_type: STATEMENT_TYPE_DDL,
            substatement_type: Some("CREATE_TABLE_AS_SELECT"),
        },
        Form {
            // `UNLOAD` writes its objects under a prefix of the harness's own results
            // bucket, which no other row writes to.
            case: "the rows of a query written as objects, defining nothing",
            sql: "UNLOAD (SELECT id FROM classified_tbl) \
                  TO 's3://athena-results/classified-unloaded/' WITH (format = 'JSON')",
            statement_type: STATEMENT_TYPE_DML,
            substatement_type: Some("UNLOAD"),
        },
        Form {
            case: "a view defined over a query",
            sql: "CREATE VIEW classified_view AS SELECT id FROM classified_tbl",
            statement_type: STATEMENT_TYPE_DDL,
            substatement_type: Some("CREATE_VIEW"),
        },
        Form {
            case: "a statement that asks the engine about a statement",
            sql: "EXPLAIN SELECT 1",
            statement_type: STATEMENT_TYPE_UTILITY,
            substatement_type: Some("EXPLAIN"),
        },
        Form {
            case: "the tables of a database listed",
            sql: "SHOW TABLES",
            statement_type: STATEMENT_TYPE_UTILITY,
            substatement_type: Some("SHOW_TABLES"),
        },
        Form {
            case: "a table dropped",
            sql: "DROP TABLE classified_ctas",
            statement_type: STATEMENT_TYPE_DDL,
            substatement_type: Some("DROP_TABLE"),
        },
    ];

    // The rows are started in order — one of them reads what another defined — and each is
    // waited out before the next, so what each statement sees is what the ones before it left.
    for case in &cases {
        let id = start_query(&router, case.sql).await;
        let found = settled(&router, &id).await;
        assert_eq!(
            found["StatementType"].as_str(),
            Some(case.statement_type),
            "case {:?}: expected the statement type {:?} for {:?}, got {found}",
            case.case,
            case.statement_type,
            case.sql
        );
        let reported: Option<&str> = found["SubstatementType"].as_str();
        assert_eq!(
            reported, case.substatement_type,
            "case {:?}: expected the substatement type {:?}, got {reported:?}",
            case.case, case.substatement_type
        );
    }
}

/// The same classification is what the LISTING of a query's history reports for it: one
/// execution has one class, whichever reader answers for it.
#[tokio::test]
async fn the_class_a_read_reports_is_the_one_the_batch_read_reports() {
    let router = router();
    let ctas = start_query(
        &router,
        "CREATE TABLE classified_twice AS SELECT 1 AS n UNION ALL SELECT 2",
    )
    .await;
    let single = settled(&router, &ctas).await;
    assert_eq!(
        single["StatementType"].as_str(),
        Some(STATEMENT_TYPE_DDL),
        "a table defined by a query is a definition, got {single}"
    );

    let (status, batch) = crate::harness::call(
        &router,
        "BatchGetQueryExecution",
        &json!({ "QueryExecutionIds": [ctas] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetQueryExecution must answer 200, got {status} with {batch}"
    );
    let batched: Vec<Value> = batch["QueryExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let reported: Vec<Option<&str>> = batched
        .iter()
        .map(|execution| execution["StatementType"].as_str())
        .collect();
    assert_eq!(
        reported,
        vec![Some(STATEMENT_TYPE_DDL)],
        "the batch read must report the same class, got {batch}"
    );
}
