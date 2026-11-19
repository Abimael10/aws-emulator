// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a read of a query execution's results reports BESIDE the
//! modelled result set: the statement's RESULT OUTPUT TEXT, and the type names the engine
//! that served the statement describes its columns in.
//!
//! The rules under test:
//!
//! * a read of a terminal execution's results always reports the statement's result output
//!   text — a present string, empty or not. Only a statement whose result IS that text (the
//!   classes Athena performs on its Hive DDL engine, which return no result set of rows) has
//!   any: a run the engine reported done produced the lines of its answer, one the engine
//!   reported in error produced the diagnostic the execution reports for the failure, and a
//!   statement whose results are ROWS produced no text output at all;
//! * a result column is typed in the type names of the ENGINE that served the statement: a
//!   character column of a statement the Hive DDL engine performs is Hive's `string`, which
//!   declares no width (so `Precision` stays at the modelled default of 0) and compares
//!   without regard to case, while a character column the query engine computes is that
//!   engine's unbounded `varchar`, which reports its own maximum length and compares
//!   case-sensitively.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: which engine
//! served the statement, the class of statement, how many rows it produced (including none),
//! whether the engine ran it or reported it in error, and what the column is called.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, router, settled, start};

/// Where the queries in these tests write their results. Nothing is read back from it.
const OUTPUT_LOCATION: &str = "s3://athena-results/output-text/";

/// The database the statements of these suites run in, so they see each other's writes and
/// another suite's statements do not.
const CONTEXT_DATABASE: &str = "otext_context";

/// Hive's own type name for an unbounded character column, which is what a statement the
/// Hive DDL engine performs reports its columns as. Source: the `tab_name` and
/// `database_name` columns a `SHOW` statement answers with, described by
/// `smithy_query(query="shape", shape="ColumnInfo")` members `Type`, `Precision` (`@default`
/// 0, documented as meaningful "For DECIMAL data types") and `CaseSensitive` (`@default`
/// false).
const HIVE_STRING: (&str, i64, bool) = ("string", 0, false);

/// The query engine's own name for an unbounded character column, with that type's own
/// maximum length — the type a column the query engine COMPUTED reports instead.
const ENGINE_VARCHAR: (&str, i64, bool) = ("varchar", 2_147_483_647, true);

/// The query engine's name for a 32-bit whole-number column, with that type's decimal digit
/// width: a column that is not character data, so it does not compare case-sensitively.
const ENGINE_INTEGER: (&str, i64, bool) = ("integer", 10, false);

/// A `StartQueryExecution` body for `sql`, run in [`CONTEXT_DATABASE`].
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": CONTEXT_DATABASE },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// Run every statement of `setup` in order, waiting for each to be terminal, and fail the
/// suite where one of them did not succeed.
async fn seed(router: &MultiServiceRouter, setup: &[&str]) {
    for sql in setup {
        let id = start(router, &query(sql)).await;
        let found = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup {sql:?}: the statement must succeed, got {found:?}"
        );
    }
}

/// Read the whole result set of `id` back, as the one page it fits in.
async fn results(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body:?}"
    );
    body
}

/// What a row of the result-output table expects of the `Output` member.
enum Output {
    /// Exactly this text.
    Text(&'static str),
    /// The diagnostic the execution reports for its failure, whatever the engine worded it
    /// as — asserted against the execution's own `StateChangeReason` rather than against a
    /// copy of that wording.
    Diagnostic,
}

/// One row of the result-output table.
struct OutputCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The state the statement must reach.
    state: &'static str,
    /// The result output text the read must report for it.
    output: Output,
}

/// The rows: the two engines, a listing with entries and one with none, a definition
/// statement the engine ran and one it reported in error, a utility statement it reported in
/// error, and the statement classes whose results are rows.
fn output_cases() -> Vec<OutputCase> {
    vec![
        OutputCase {
            case: "a listing of the databases this suite defined",
            sql: "SHOW DATABASES LIKE 'otext_held%'",
            state: QUERY_STATE_SUCCEEDED,
            // The lines of the answer, in the order the listing reports them, joined by
            // the line feed and with no line terminating the last one.
            output: Output::Text("otext_held_a\notext_held_b"),
        },
        OutputCase {
            case: "the same listing narrowed to nothing",
            sql: "SHOW DATABASES LIKE 'otext_none%'",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
        OutputCase {
            case: "a listing of a database that holds no relation",
            sql: "SHOW TABLES IN otext_held_a",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
        OutputCase {
            case: "a definition statement the engine ran",
            sql: "CREATE DATABASE otext_defined",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
        OutputCase {
            case: "a definition statement naming a database the catalog does not hold",
            sql: "DROP SCHEMA otext_absent",
            state: QUERY_STATE_FAILED,
            // The Hive DDL engine's semantic error for a name it cannot resolve while it
            // compiles the statement; the only variable part is the database named.
            output: Output::Text(
                "FAILED: SemanticException [Error 10072]: Database does not exist: \
                 otext_absent",
            ),
        },
        OutputCase {
            case: "a utility statement the engine reported in error",
            sql: "EXPLAIN SELECT * FROM otext_nowhere_at_all",
            state: QUERY_STATE_FAILED,
            output: Output::Diagnostic,
        },
        OutputCase {
            case: "a projection, whose results are rows",
            sql: "SELECT 'north' AS region",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
        OutputCase {
            case: "a write, whose results are rows",
            sql: "INSERT INTO otext_rows VALUES (2, 'lamp')",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
        OutputCase {
            case: "a read of a stored table, whose results are rows",
            sql: "SELECT label FROM otext_rows ORDER BY id",
            state: QUERY_STATE_SUCCEEDED,
            output: Output::Text(""),
        },
    ]
}

/// A read of a terminal execution's results reports the statement's result OUTPUT TEXT
/// beside the result set: the lines a statement whose result is that text produced, the
/// diagnostic of one the engine reported in error, and the empty string for a statement
/// whose results are rows — present in every case.
#[tokio::test]
async fn a_read_reports_the_result_output_text_of_the_statement_that_ran() {
    let router = router();
    seed(
        &router,
        &[
            "CREATE DATABASE otext_held_a",
            "CREATE DATABASE otext_held_b",
            "CREATE TABLE otext_rows (id integer, label varchar)",
            "INSERT INTO otext_rows VALUES (1, 'desk')",
        ],
    )
    .await;

    let cases = output_cases();
    // Started together, so the suite waits one completion span rather than one per row.
    let mut started: Vec<(&OutputCase, String)> = Vec::new();
    for case in &cases {
        started.push((case, start(&router, &query(case.sql)).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(case.state),
            "case {name:?}: expected the state {:?}, got {found:?}",
            case.state
        );

        let body = results(&router, name, id).await;
        let reported = body["Output"].as_str().map(str::to_string);
        let expected = match case.output {
            Output::Text(text) => text.to_string(),
            Output::Diagnostic => found["Status"]["StateChangeReason"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {name:?}: a failed execution reports a reason, got {found:?}")
                })
                .to_string(),
        };
        assert_eq!(
            reported.as_deref(),
            Some(expected.as_str()),
            "case {name:?}: expected the result output {expected:?}, got {reported:?}"
        );
        if matches!(case.output, Output::Diagnostic) {
            assert!(
                !expected.is_empty(),
                "case {name:?}: a statement the engine reported in error carries a \
                 diagnostic, got {expected:?}"
            );
        }
    }
}

/// The result output text is reported on EVERY page of a paged read, because it belongs to
/// the whole output rather than to one page of it — and the pages of a statement whose
/// results are rows each report it as the empty string.
#[tokio::test]
async fn every_page_of_one_read_reports_the_same_result_output_text() {
    let router = router();
    seed(
        &router,
        &[
            "CREATE TABLE otext_paged (id integer)",
            "INSERT INTO otext_paged VALUES (1), (2), (3), (4)",
        ],
    )
    .await;

    let id = start(&router, &query("SELECT id FROM otext_paged ORDER BY id")).await;
    let found = settled(&router, &id).await;
    assert_eq!(
        found["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "the read must succeed, got {found:?}"
    );

    // One row per page, so the label row and the four data rows are five pages.
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let mut request = json!({ "QueryExecutionId": id, "MaxResults": 1 });
        if let Some(token) = token.as_deref() {
            request["NextToken"] = json!(token);
        }
        let (status, body) = call(&router, "GetQueryResults", &request).await;
        assert_eq!(
            status, 200,
            "page {pages}: GetQueryResults must answer 200, got {status} with {body:?}"
        );
        assert_eq!(
            body["Output"].as_str(),
            Some(""),
            "page {pages}: a statement whose results are rows wrote no text output, got \
             {body:?}"
        );
        pages = pages.saturating_add(1);
        token = body["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            pages <= 8,
            "the read must end: five one-row pages were expected, and {pages} were served"
        );
    }
    assert_eq!(
        pages, 5,
        "the label row and the four data rows are five one-row pages, got {pages}"
    );
}

/// One row of the column-typing table.
struct TypeCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The name of its one column.
    column: &'static str,
    /// The `(Type, Precision, CaseSensitive)` that column must report.
    reported: (&'static str, i64, bool),
}

/// The rows: the one character column each of the Hive DDL engine's reporting statements
/// answers with, a listing that answered with no row at all, and the character and
/// whole-number columns the query engine computes.
fn type_cases() -> Vec<TypeCase> {
    vec![
        TypeCase {
            case: "the column a listing of the databases answers with",
            sql: "SHOW DATABASES LIKE 'otype_held%'",
            column: "database_name",
            reported: HIVE_STRING,
        },
        TypeCase {
            case: "the Presto spelling of the same listing",
            sql: "SHOW SCHEMAS LIKE 'otype_held%'",
            column: "database_name",
            reported: HIVE_STRING,
        },
        TypeCase {
            case: "the same listing, which answered with no row at all",
            sql: "SHOW DATABASES LIKE 'otype_none%'",
            column: "database_name",
            reported: HIVE_STRING,
        },
        TypeCase {
            case: "the column a listing of a database's relations answers with",
            sql: "SHOW TABLES IN otype_held_a",
            column: "tab_name",
            reported: HIVE_STRING,
        },
        TypeCase {
            case: "a character column the query engine computed",
            sql: "SELECT 'north' AS region",
            column: "region",
            reported: ENGINE_VARCHAR,
        },
        TypeCase {
            case: "a character column the query engine read from a stored table",
            sql: "SELECT label FROM otype_rows ORDER BY id",
            column: "label",
            reported: ENGINE_VARCHAR,
        },
        TypeCase {
            case: "a whole-number column the query engine computed",
            sql: "SELECT 1 AS one",
            column: "one",
            reported: ENGINE_INTEGER,
        },
    ]
}

/// A result column is typed in the type names of the engine that SERVED the statement: the
/// Hive DDL engine's character columns are Hive's unbounded `string`, and the query engine's
/// are its own `varchar`.
#[tokio::test]
async fn a_result_column_is_typed_in_the_names_of_the_engine_that_served_it() {
    let router = router();
    seed(
        &router,
        &[
            "CREATE DATABASE otype_held_a",
            "CREATE TABLE otype_rows (id integer, label varchar)",
            "INSERT INTO otype_rows VALUES (1, 'desk')",
        ],
    )
    .await;

    let cases = type_cases();
    let mut started: Vec<(&TypeCase, String)> = Vec::new();
    for case in &cases {
        started.push((case, start(&router, &query(case.sql)).await));
    }

    for (case, id) in &started {
        let name = case.case;
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {name:?}: the statement must succeed, got {found:?}"
        );
        let body = results(&router, name, id).await;

        // Both views of the metadata are read: a column is described the same way whichever
        // of them a client reads.
        let modelled = body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"].clone();
        let flattened = body["ResultSet"]["ColumnInfos"].clone();
        assert_eq!(
            modelled, flattened,
            "case {name:?}: the two views of one column list must agree, got {body:?}"
        );
        let columns = modelled.as_array().cloned().unwrap_or_else(|| {
            panic!("case {name:?}: the column metadata must be a present list, got {body:?}")
        });
        let column = columns
            .first()
            .unwrap_or_else(|| panic!("case {name:?}: expected one column, got {body:?}"));

        let (expected_type, precision, case_sensitive) = case.reported;
        assert_eq!(
            column["Name"].as_str(),
            Some(case.column),
            "case {name:?}: expected the column {:?}, got {column:?}",
            case.column
        );
        assert_eq!(
            column["Type"].as_str(),
            Some(expected_type),
            "case {name:?}: expected Type {expected_type:?}, got {column:?}"
        );
        assert_eq!(
            column["Precision"].as_i64(),
            Some(precision),
            "case {name:?}: expected Precision {precision}, got {column:?}"
        );
        assert_eq!(
            column["CaseSensitive"].as_bool(),
            Some(case_sensitive),
            "case {name:?}: expected CaseSensitive {case_sensitive}, got {column:?}"
        );
        assert_eq!(
            column["Scale"].as_i64(),
            Some(0),
            "case {name:?}: no type here carries digits after a decimal point, got \
             {column:?}"
        );
    }
}
