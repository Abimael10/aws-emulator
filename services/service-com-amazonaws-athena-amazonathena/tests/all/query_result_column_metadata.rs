// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the COLUMN METADATA and the flattened views a read of a query
//! execution's results reports.
//!
//! The rules under test, from `com.amazonaws.athena#ColumnInfo` and from what a read of a
//! result set is:
//!
//! * every column of every result set says where it came from — `CatalogName` is the
//!   catalog that served the query, and `SchemaName` and `TableName` are the empty string
//!   for a column of a projection, which belongs to no one named relation. All three are
//!   STRINGS, never absent members;
//! * a column's TYPE is a property of the expression projected, not of the values that
//!   expression happened to produce: where Athena's own signature fixes the result type of
//!   a projected function, that type is reported however many rows the run counted and
//!   whatever values it computed; only a column no signature fixes is typed by its values;
//! * the flattened views reported beside the modelled members — `ResultSet.ColumnInfos` and
//!   `ResultSet.ResultRows` — carry the SAME page: the same columns the modelled metadata
//!   describes and the same rows the modelled `Rows` reports, present and empty rather than
//!   absent for a result set with nothing in it.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the class
//! of statement, the number and declared types of the columns projected, which function the
//! projection applies, and how many rows the run produced for it — including none, so a
//! type that cannot be read off a value is asserted on a case that has no value to read.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_QUEUED, QUERY_STATE_RUNNING, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, execution, router, start};

/// Where the queries in these tests write their results. Nothing is written to it.
const OUTPUT_LOCATION: &str = "s3://athena-results/metadata/";

/// `ColumnInfo$CatalogName` — "the catalog to which the query results belong" — for a
/// statement Athena's own engine served: the Hive/Trino engine its `AwsDataCatalog` is read
/// through, which is the catalog every result column of every such query belongs to.
const RESULT_CATALOG: &str = "hive";

/// `ColumnInfo$SchemaName` and `ColumnInfo$TableName` for a column of a projection, which
/// has no originating database or table: the EMPTY string, a value rather than an omitted
/// member.
const NO_ORIGIN_RELATION: &str = "";

/// How long a test waits for a started query to reach a terminal state. Generous beside the
/// span a completion is due after, so how fast the host runs never decides the outcome; the
/// wait ends on the CONDITION (a terminal state), not on the clock.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look at a started query's state waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A `StartQueryExecution` body for `sql`, in `database` so the statements of one suite see
/// each other's writes and another suite's do not.
fn query_in(sql: &str, database: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": database },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// Poll `id` until it is terminal, and assert it got there by SUCCEEDING.
async fn succeeded(router: &MultiServiceRouter, case: &str, id: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let found = execution(router, id).await;
        let state = found["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state != QUERY_STATE_QUEUED && state != QUERY_STATE_RUNNING {
            assert_eq!(
                state, QUERY_STATE_SUCCEEDED,
                "case {case:?}: the statement must succeed, got {found:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the query {id} never reached a terminal state, last state \
             {state:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
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

/// The table every suite here reads, and the rows in it: one column of each type the engine
/// holds, and a row whose number and its text are both NULL, so an aggregate over a column
/// that answers nothing is a case among the rows.
const SETUP: &[&str] = &[
    "CREATE TABLE metrics (id integer, price double, label varchar)",
    "INSERT INTO metrics VALUES (1, 10.0, 'desk'), (2, 20.5, 'lamp'), (3, NULL, NULL)",
];

/// Run the setup statements in `database`, in order, and fail the suite if any of them did
/// not succeed.
async fn seed(router: &MultiServiceRouter, database: &str) {
    for sql in SETUP {
        let id = start(router, &query_in(sql, database)).await;
        succeeded(router, "setup", &id).await;
    }
}

/// The columns a read reports, as the `ColumnInfo` list it carries.
fn columns_of(case: &str, body: &Value) -> Vec<Value> {
    body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the column metadata must be a present list, got {body:?}")
        })
}

/// Every column of every result set reports the catalog that served the query, and the
/// empty string for the database and the table a column of a projection comes from — all
/// three present, whatever the statement was.
#[tokio::test]
async fn every_result_column_reports_the_catalog_that_served_it() {
    let router = router();
    let database = "metadata_identity";
    seed(&router, database).await;
    // A database so the listing case has an entry to answer with.
    let defined = start(
        &router,
        &query_in("CREATE DATABASE metadata_listed", database),
    )
    .await;
    succeeded(&router, "setup", &defined).await;

    // (case, the SQL, how many columns it projects). The rows differ along the class of
    // statement — a literal projection, a read of declared columns, an aggregate, and a
    // listing the data catalog itself answers — and along how wide the projection is.
    let cases: Vec<(&str, &str, usize)> = vec![
        ("a literal projection", "SELECT 1 AS one", 1),
        (
            "a read of two declared columns",
            "SELECT id, label FROM metrics ORDER BY id",
            2,
        ),
        ("an aggregate", "SELECT count(*) AS n FROM metrics", 1),
        (
            "a listing the data catalog answers",
            "SHOW DATABASES",
            1,
        ),
    ];

    let mut started: Vec<(&str, String, usize)> = Vec::new();
    for (case, sql, width) in &cases {
        started.push((case, start(&router, &query_in(sql, database)).await, *width));
    }

    for (case, id, width) in &started {
        succeeded(&router, case, id).await;
        let body = results(&router, case, id).await;
        let columns = columns_of(case, &body);
        assert_eq!(
            columns.len(),
            *width,
            "case {case:?}: expected {width} column(s), got {columns:?}"
        );
        for column in &columns {
            assert_eq!(
                column["CatalogName"].as_str(),
                Some(RESULT_CATALOG),
                "case {case:?}: expected CatalogName {RESULT_CATALOG:?}, got {column:?}"
            );
            for member in ["SchemaName", "TableName"] {
                assert_eq!(
                    column[member].as_str(),
                    Some(NO_ORIGIN_RELATION),
                    "case {case:?}: expected {member} {NO_ORIGIN_RELATION:?}, got {column:?}"
                );
            }
        }
    }
}

/// One row of the typing table.
struct TypeCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `ColumnInfo$Type` its one column must report.
    column_type: &'static str,
    /// The `ColumnInfo$Precision` that type carries.
    precision: i64,
    /// The value of that column in the first data row, or `None` for the SQL NULL, which
    /// carries no value at all.
    value: Option<&'static str>,
}

/// The rows: a projected function whose result type Athena's signature fixes keeps that
/// type however many rows the run counted — including none, where there is no value to read
/// a type off at all — and a column no signature fixes is typed by the values it produced.
fn type_cases() -> Vec<TypeCase> {
    vec![
        TypeCase {
            case: "a count over every row",
            sql: "SELECT count(*) AS n FROM metrics",
            column_type: "bigint",
            precision: 19,
            value: Some("3"),
        },
        TypeCase {
            // The same column over a result the engine computed no rows for: the type is
            // the signature's, so a count of zero is still a 64-bit integer.
            case: "a count over no rows at all",
            sql: "SELECT count(*) AS n FROM metrics WHERE 1 = 0",
            column_type: "bigint",
            precision: 19,
            value: Some("0"),
        },
        TypeCase {
            case: "a count of the values one column holds",
            sql: "SELECT count(price) AS priced FROM metrics",
            column_type: "bigint",
            precision: 19,
            value: Some("2"),
        },
        TypeCase {
            case: "a count of the distinct values one column holds",
            sql: "SELECT count(DISTINCT label) AS labels FROM metrics",
            column_type: "bigint",
            precision: 19,
            value: Some("2"),
        },
        TypeCase {
            case: "a count written in upper case",
            sql: "SELECT COUNT(*) AS n FROM metrics",
            column_type: "bigint",
            precision: 19,
            value: Some("3"),
        },
        TypeCase {
            // An average is a double whatever it averages — a function the reported issue
            // never named, and the second entry of the signature table.
            case: "an average over a column of numbers",
            sql: "SELECT avg(price) AS mean FROM metrics",
            column_type: "double",
            precision: 17,
            value: Some("15.25"),
        },
        TypeCase {
            // The case no value could type: the average of nothing is NULL, and the column
            // is a double all the same.
            case: "an average over no rows at all",
            sql: "SELECT avg(price) AS mean FROM metrics WHERE 1 = 0",
            column_type: "double",
            precision: 17,
            value: None,
        },
        TypeCase {
            case: "a row's position in its window",
            sql: "SELECT row_number() OVER (ORDER BY id) AS position FROM metrics ORDER BY id",
            column_type: "bigint",
            precision: 19,
            value: Some("1"),
        },
        TypeCase {
            // A total's type is its argument's, which no signature fixes, so the values
            // decide it — and these are doubles.
            case: "a total over a column of doubles",
            sql: "SELECT sum(price) AS total FROM metrics",
            column_type: "double",
            precision: 17,
            value: Some("30.5"),
        },
        TypeCase {
            case: "a small integer literal, which the value types",
            sql: "SELECT 1 AS one",
            column_type: "integer",
            precision: 10,
            value: Some("1"),
        },
        TypeCase {
            case: "a literal no 32-bit integer holds",
            sql: "SELECT 4294967296 AS big",
            column_type: "bigint",
            precision: 19,
            value: Some("4294967296"),
        },
        TypeCase {
            case: "a column the engine declares a character type for",
            sql: "SELECT label FROM metrics ORDER BY id",
            column_type: "varchar",
            // The unbounded character type's own maximum length.
            precision: 2_147_483_647,
            value: Some("desk"),
        },
    ]
}

/// A projected column's `Type` and `Precision` are the ones Athena's signature for the
/// expression gives it, and the rows beside them are the ones the engine computed.
#[tokio::test]
async fn a_projected_column_is_typed_by_its_signature_and_not_by_its_values() {
    let router = router();
    let database = "metadata_types";
    seed(&router, database).await;

    let cases = type_cases();
    let mut started: Vec<(&TypeCase, String)> = Vec::new();
    for case in &cases {
        started.push((case, start(&router, &query_in(case.sql, database)).await));
    }

    for (case, id) in &started {
        let name = case.case;
        succeeded(&router, name, id).await;
        let body = results(&router, name, id).await;
        let columns = columns_of(name, &body);
        let column = columns
            .first()
            .unwrap_or_else(|| panic!("case {name:?}: expected one column, got {body:?}"));
        assert_eq!(
            column["Type"].as_str(),
            Some(case.column_type),
            "case {name:?}: expected Type {:?}, got {column:?}",
            case.column_type
        );
        assert_eq!(
            column["Precision"].as_i64(),
            Some(case.precision),
            "case {name:?}: expected Precision {}, got {column:?}",
            case.precision
        );
        assert_eq!(
            column["Scale"].as_i64(),
            Some(0),
            "case {name:?}: no type here carries digits after a decimal point, got \
             {column:?}"
        );
        // A DML statement's results are read with the column labels in front of the data
        // rows, so the value under test is in the SECOND row.
        let rows = body["ResultSet"]["Rows"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let value = rows
            .get(1)
            .map(|row| row["Data"][0]["VarCharValue"].clone())
            .unwrap_or_else(|| panic!("case {name:?}: expected a data row, got {body:?}"));
        let reported = value.as_str();
        assert_eq!(
            reported, case.value,
            "case {name:?}: expected the value {:?}, got {value:?}",
            case.value
        );
    }
}

/// The flattened views report the SAME page as the modelled members: `ColumnInfos` is the
/// column list the modelled metadata carries, and `ResultRows` is the modelled `Rows` with
/// each value as a plain string — both present, empty or not.
#[tokio::test]
async fn the_flattened_views_carry_the_same_page_as_the_modelled_members() {
    let router = router();
    let database = "metadata_flat";
    seed(&router, database).await;

    // (case, the SQL). The rows differ along how wide and how long the page is: one column
    // and several rows, one column and none, a value that is NULL, several columns and
    // several rows, and a statement whose result set has nothing in it at all.
    let cases: Vec<(&str, &str)> = vec![
        ("one column and several rows", "SELECT id FROM metrics ORDER BY id"),
        ("one column and no rows", "SELECT id FROM metrics WHERE 1 = 0"),
        ("a value that is NULL", "SELECT price FROM metrics WHERE id = 3"),
        (
            "several columns and several rows",
            "SELECT id, label FROM metrics ORDER BY id",
        ),
        (
            "a definition statement, whose result set is empty",
            "CREATE TABLE flattened (id integer)",
        ),
    ];

    let mut started: Vec<(&str, String)> = Vec::new();
    for (case, sql) in &cases {
        started.push((case, start(&router, &query_in(sql, database)).await));
    }

    for (case, id) in &started {
        succeeded(&router, case, id).await;
        let body = results(&router, case, id).await;
        let result_set = &body["ResultSet"];

        // Both lists are read as PRESENT lists — a missing member panics rather than
        // comparing equal to another missing one.
        let flattened_columns =
            result_set["ColumnInfos"].as_array().cloned().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the flattened columns must be a present list, got \
                     {result_set:?}"
                )
            });
        let modelled_columns = columns_of(case, &body);
        assert_eq!(
            flattened_columns, modelled_columns,
            "case {case:?}: the flattened columns must be the modelled ones, got {result_set:?}"
        );

        let modelled_rows = result_set["Rows"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the rows must be a present list, got {result_set:?}")
        });
        // Every value of every row, with the SQL NULL — which the modelled view reports as
        // a datum carrying no value — flattened to the empty string.
        let expected: Vec<Value> = modelled_rows
            .iter()
            .map(|row| {
                let data: Vec<Value> = row["Data"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|datum| json!(datum["VarCharValue"].as_str().unwrap_or_default()))
                    .collect();
                json!({ "Data": data })
            })
            .collect();
        let flattened_rows = result_set["ResultRows"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the flattened rows must be a present list, got {result_set:?}")
        });
        assert_eq!(
            flattened_rows, expected,
            "case {case:?}: expected the flattened rows {expected:?}, got {flattened_rows:?}"
        );
    }
}
