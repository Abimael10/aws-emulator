// SPDX-License-Identifier: CC0-1.0

//! The COLUMNS a relation names in its own alias.
//!
//! The rule under test: a relation read in a `FROM` clause may be given a name AND the names
//! of its columns (`<relation> AS <name>(<column>, …)`), and those names are the names its
//! columns carry for the rest of the statement — whether the relation is an inline table
//! whose rows the statement writes out (`VALUES`) or a derived query that projects one
//! expression per name. A statement reading such a relation runs: the rows are the rows the
//! relation names, in the order the statement asks for, and the result columns carry the
//! names the alias gave them.
//!
//! The rows of each table below differ along what the rule generalises over: the shape of
//! the relation (an inline table, a derived query), how many rows and columns it carries, how
//! many such relations the statement reads, whether the statement projects them with a
//! wildcard, by name or through an expression, and whether it orders, aggregates or joins
//! over them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::QUERY_STATE_SUCCEEDED;

use crate::harness::{call, router, settled, start};

/// The results location every statement here writes to: one of the harness's own buckets, so
/// nothing of this suite's has to be created first.
const OUTPUT_LOCATION: &str = "s3://athena-results/inline-tables/";

/// The database these statements run in, so they see each other's writes and another suite's
/// do not.
const DATABASE: &str = "inline_table_relations";

/// One row of the inline-relation table.
struct Case {
    /// What this row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The names its result columns must carry, in projection order.
    names: &'static [&'static str],
    /// The data rows it must produce, in order, each cell as the text a datum carries.
    rows: &'static [&'static [&'static str]],
}

/// A statement that reads a relation naming its own columns runs, and reports those columns
/// over the rows the relation holds.
#[tokio::test]
async fn a_relation_that_names_its_columns_is_read_under_those_names() {
    let router = router();

    let cases: Vec<Case> = vec![
        Case {
            case: "an inline table of three rows and two columns, ordered",
            sql: "SELECT * FROM (VALUES (3, 'gamma'), (1, 'alpha'), (2, 'beta')) \
                  AS t(id, name) ORDER BY id",
            names: &["id", "name"],
            rows: &[&["1", "alpha"], &["2", "beta"], &["3", "gamma"]],
        },
        Case {
            case: "an inline table of one column, ordered the other way",
            sql: "SELECT x FROM (VALUES (10), (30), (20)) AS t(x) ORDER BY x DESC",
            names: &["x"],
            rows: &[&["30"], &["20"], &["10"]],
        },
        Case {
            case: "an aggregate over an inline table, beside a literal",
            sql: "SELECT 'probe' AS marker, count(*) AS c FROM (VALUES (1), (2), (3)) AS t(x)",
            names: &["marker", "c"],
            rows: &[&["probe", "3"]],
        },
        Case {
            case: "an expression over two columns of one inline-table row",
            sql: "SELECT a + b AS total FROM (VALUES (1, 2)) AS t(a, b)",
            names: &["total"],
            rows: &[&["3"]],
        },
        Case {
            case: "a column of an inline table addressed through the relation's own name",
            sql: "SELECT t.name AS picked FROM (VALUES (1, 'alpha'), (2, 'beta')) AS t(id, name) \
                  WHERE t.id = 2",
            names: &["picked"],
            rows: &[&["beta"]],
        },
        Case {
            case: "two inline tables joined",
            sql: "SELECT t.name AS name, u.n AS n FROM (VALUES (1, 'alpha')) AS t(id, name) \
                  CROSS JOIN (VALUES (9)) AS u(n)",
            names: &["name", "n"],
            rows: &[&["alpha", "9"]],
        },
        Case {
            case: "a derived query whose columns the alias renames",
            sql: "SELECT b AS second FROM (SELECT 1, 'two') AS t(a, b)",
            names: &["second"],
            rows: &[&["two"]],
        },
        Case {
            case: "an inline table of five rows, grouped",
            sql: "SELECT k AS k, sum(v) AS total FROM \
                  (VALUES ('a', 1), ('b', 2), ('a', 3), ('b', 4), ('a', 5)) AS t(k, v) \
                  GROUP BY k ORDER BY k",
            names: &["k", "total"],
            rows: &[&["a", "9"], &["b", "6"]],
        },
    ];

    // Started first, all of them, so the span a completion is owed after the submit is
    // waited out once rather than once per row.
    let mut started: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        started.push((case, start(&router, &request(case.sql)).await));
    }

    for (case, id) in &started {
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: {} must succeed, got {:?}",
            case.case,
            case.sql,
            found["Status"]
        );
        let (names, rows) = results(&router, case.case, id).await;
        assert_eq!(
            names,
            case.names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the columns {:?}, got {names:?}",
            case.case,
            case.names
        );
        let expected: Vec<Vec<String>> = case
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| (*cell).to_string()).collect())
            .collect();
        assert_eq!(
            rows, expected,
            "case {:?}: expected the rows {expected:?}, got {rows:?}",
            case.case
        );
    }
}

/// The `StartQueryExecution` request for `sql`.
fn request(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": DATABASE },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// What `GetQueryResults` reports for the execution `id`: the column names, and the data
/// rows under the label row every such result set opens with.
async fn results(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> (Vec<String>, Vec<Vec<String>>) {
    let (status, body) = call(
        router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body:?}"
    );
    let names = body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|column| column["Name"].as_str().unwrap_or_default().to_string())
        .collect();
    let rows = body["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        // The first row of a DML result set's first page is the column labels.
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    (names, rows)
}
