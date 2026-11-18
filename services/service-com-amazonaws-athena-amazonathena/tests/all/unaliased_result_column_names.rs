// SPDX-License-Identifier: CC0-1.0

//! How a query's result columns are NAMED when the query names none of them itself.
//!
//! The rule under test: a result column the query projects as an EXPRESSION and does not
//! alias is named by that expression's POSITION in the select list — `_col` followed by the
//! zero-based position — and never by the text of the expression; a projected column
//! REFERENCE keeps the name of the column it reads, and an aliased projection carries its
//! alias. The same name is reported in both places a result set's schema is observable: the
//! `ColumnInfo` a read of the results carries, and the label line the saved results object
//! begins with.
//!
//! Each suite asserts that over a TABLE whose rows differ along what the rule generalises
//! over: the kind of projected expression (an aggregate, arithmetic, a literal, a scalar
//! function), the POSITION the unaliased column sits at, how wide the projection is, and
//! whether the projection names the column itself (an alias, a column reference) — the
//! cases where nothing is renamed.
//!
//! Athena and Amazon S3 are mounted on ONE shared engine and driven over the wire, through
//! the harness the results-object suite already builds, so what is asserted is what an SDK
//! client observes of both services.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::QUERY_STATE_SUCCEEDED;

use crate::harness::{call, settled};
use crate::query_result_objects::{create_bucket, emulator, s3_served, start, succeeded};

/// The bucket these suites write their results into — their own, so the keys listed here
/// are only theirs.
const BUCKET: &str = "athena-unaliased-columns";

/// The prefix under it the results objects land at.
const PREFIX: &str = "named";

/// The database the statements of these suites run in, so they see each other's writes and
/// another suite's do not.
const DATABASE: &str = "unaliased_columns";

/// The table every row below reads, and the rows in it: three rows, so a count over them
/// answers a figure no single row could, and each row that reads values addresses ONE of
/// them, so the saved object holds exactly one data line.
const SETUP: &[&str] = &[
    "CREATE TABLE readings (id integer, price double, label varchar)",
    "INSERT INTO readings VALUES (1, 10.0, 'desk'), (2, 20.5, 'lamp'), (3, 30.0, 'rug')",
];

/// One row of the naming table.
struct Naming {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The names its result columns must carry, in projection order.
    names: &'static [&'static str],
    /// The first data line of the saved CSV, as the object holds it.
    first_row: &'static str,
}

/// The rows: an unaliased expression is named by its position whatever kind of expression it
/// is and wherever in the projection it sits, and a projection that names its own column is
/// left alone.
fn naming_cases() -> Vec<Naming> {
    vec![
        Naming {
            case: "an unaliased aggregate, the only column",
            sql: "SELECT count(*) FROM readings",
            names: &["_col0"],
            first_row: "\"3\"",
        },
        Naming {
            case: "the same aggregate under an alias",
            sql: "SELECT count(*) AS n FROM readings",
            names: &["n"],
            first_row: "\"3\"",
        },
        Naming {
            case: "an unaliased arithmetic expression in second position",
            sql: "SELECT id, id * 2 FROM readings WHERE id = 1",
            names: &["id", "_col1"],
            first_row: "\"1\",\"2\"",
        },
        Naming {
            case: "two unaliased aggregates",
            sql: "SELECT min(id), max(id) FROM readings",
            names: &["_col0", "_col1"],
            first_row: "\"1\",\"3\"",
        },
        Naming {
            case: "an unaliased expression between two aliased ones",
            sql: "SELECT min(id) AS lowest, count(*), max(id) AS highest FROM readings",
            names: &["lowest", "_col1", "highest"],
            first_row: "\"1\",\"3\",\"3\"",
        },
        Naming {
            case: "an unaliased scalar function over a column",
            sql: "SELECT upper(label) FROM readings WHERE id = 1",
            names: &["_col0"],
            first_row: "\"DESK\"",
        },
        Naming {
            case: "an unaliased literal, read from no table at all",
            sql: "SELECT 7",
            names: &["_col0"],
            first_row: "\"7\"",
        },
        Naming {
            case: "a projected column reference, which names itself",
            sql: "SELECT label FROM readings WHERE id = 1",
            names: &["label"],
            first_row: "\"desk\"",
        },
    ]
}

/// The `StartQueryExecution` request for `sql`, writing its results into this suite's own
/// prefix.
fn request(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": DATABASE },
        "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/{PREFIX}/") },
    })
}

/// Run the setup statements in order, failing the suite if any of them did not succeed.
async fn seed(router: &MultiServiceRouter) {
    for sql in SETUP {
        let id = start(router, "setup", &request(sql)).await;
        let found = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup: {sql:?} must succeed, got {found:?}"
        );
    }
}

/// The column names `GetQueryResults` reports for `id`, in projection order, read off the
/// modelled `ResultSetMetadata`.
async fn reported_names(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<String> {
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
    let columns = body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the column metadata must be a present list, got {body:?}")
        });
    columns
        .iter()
        .map(|column| {
            column["Name"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every column reports a Name, got {column:?}")
                })
                .to_string()
        })
        .collect()
}

/// A result column the query projects as an expression and does not alias is named `_colN`
/// by its position, in the metadata a read reports and in the label line of the saved
/// results object alike; a column the projection names is reported under that name.
#[tokio::test]
async fn an_unaliased_projected_expression_is_named_by_its_position() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    seed(&router).await;

    let cases = naming_cases();
    let mut started: Vec<(&Naming, String)> = Vec::new();
    for naming in &cases {
        started.push((naming, start(&router, naming.case, &request(naming.sql)).await));
    }

    for (naming, id) in &started {
        let case = naming.case;
        succeeded(&router, case, id).await;

        // What a client keying on the result-set metadata binds to.
        let reported = reported_names(&router, case, id).await;
        let expected: Vec<String> = naming.names.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the result columns {expected:?}, got {reported:?}"
        );

        // And what a reader of the saved CSV parses out of its header line, which is the
        // same schema written down.
        let key = format!("{PREFIX}/{id}.csv");
        let (status, _media_type, saved) =
            s3_served(&router, "GET", &format!("/{BUCKET}/{key}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject(s3://{BUCKET}/{key}) must answer 200, got {status} with \
             {saved:?}"
        );
        let header: String = naming
            .names
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<String>>()
            .join(",");
        let expected_lines = vec![header, naming.first_row.to_string()];
        let lines: Vec<String> = saved.lines().map(str::to_string).collect();
        assert_eq!(
            lines, expected_lines,
            "case {case:?}: expected the saved CSV {expected_lines:?}, got {lines:?}"
        );
    }
}
