// SPDX-License-Identifier: CC0-1.0

//! What a `DescribeTable` response says it DESCRIBES: its `TableName`.
//!
//! One rule, asserted over a TABLE of inputs rather than one request:
//! `TableName` is the path the request itself asked about — its `Database`, its
//! `Schema` and its `Table` joined with dots, carrying an EMPTY component
//! wherever the request named no selector — and it is reported on every
//! successful answer, whether the catalogue held a relation of that name or
//! nothing at all.
//!
//! The rows differ along what the rule generalises over: which selectors the
//! request carries (all three, no schema, no table, neither), which database it
//! names, whether the named schema and table exist, and whether the same bare
//! table name lives in more than one schema. `ColumnList` is asserted beside
//! the path, so a row proves the path is composed from the REQUEST and not read
//! back out of whatever the catalogue matched.
//!
//! Every call goes over the wire through the shared harness, so what is
//! asserted is the emulator's HTTP behaviour.

use serde_json::{json, Value};

use super::emulator::{list, Emulator, WAIT_SECONDS};

/// The database the harness runs its own statements against.
const DEV: &str = "dev";

/// A SECOND database of the same warehouse, so the first component of the path
/// varies along the rule rather than being fixed by the harness.
const ANALYTICS: &str = "analytics";

/// The schema the engine holds a relation of no schema of its own in.
const DEFAULT_SCHEMA: &str = "public";

/// A request about `database` of the provisioned cluster the suites here use.
fn connect(database: &str) -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": database, "DbUser": "awsuser"})
}

/// The columns a response describes, in the order it reports them.
fn described_columns(response: &Value) -> Vec<String> {
    list(response, "ColumnList")
        .iter()
        .map(|column| column["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Run one statement against `database` and wait for it to finish.
async fn run_on(emulator: &Emulator, database: &str, sql: &str) {
    let mut request = connect(database);
    request["Sql"] = json!(sql);
    request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
    let response = emulator.ok("ExecuteStatement", &request).await;
    assert_eq!(
        response["Status"], "FINISHED",
        "a waited-for statement must finish: {sql} on {database} -> {response}"
    );
}

/// What a row expects of the described `ColumnList`.
enum Columns {
    /// Exactly these columns, in this order.
    Exactly(&'static [&'static str]),
    /// Nothing at all: the request named something the catalogue holds no
    /// relation for.
    None,
    /// Several relations were described at once, so the row asserts only that
    /// the answer is not empty.
    Several,
}

/// One row of the path table: the selectors a request carries, the path its
/// answer must report, and what it must describe.
struct PathCase {
    label: &'static str,
    database: &'static str,
    schema: Option<&'static str>,
    table: Option<&'static str>,
    expected_path: &'static str,
    columns: Columns,
}

/// `DescribeTable` reports the path the REQUEST asked about, on every answer.
#[tokio::test]
async fn describe_table_reports_the_path_the_request_asked_about() {
    let emulator = Emulator::new();

    // One table in a schema of its own, one of the SAME bare name in the
    // default schema, one that lives only in the default schema, and a table of
    // a second database.
    run_on(&emulator, DEV, "CREATE SCHEMA sales").await;
    run_on(
        &emulator,
        DEV,
        "CREATE TABLE sales.regions (id INTEGER, name VARCHAR(20))",
    )
    .await;
    run_on(
        &emulator,
        DEV,
        "CREATE TABLE regions (id INTEGER, note VARCHAR(8))",
    )
    .await;
    run_on(&emulator, DEV, "CREATE TABLE orders (order_id INTEGER)").await;
    run_on(&emulator, ANALYTICS, "CREATE TABLE events (at VARCHAR(32))").await;

    let cases = [
        PathCase {
            label: "all three selectors, a table of a schema of its own",
            database: DEV,
            schema: Some("sales"),
            table: Some("regions"),
            expected_path: "dev.sales.regions",
            columns: Columns::Exactly(&["id", "name"]),
        },
        PathCase {
            label: "the same bare table name in the default schema",
            database: DEV,
            schema: Some(DEFAULT_SCHEMA),
            table: Some("regions"),
            expected_path: "dev.public.regions",
            columns: Columns::Exactly(&["id", "note"]),
        },
        PathCase {
            label: "a second database of the same warehouse",
            database: ANALYTICS,
            schema: Some(DEFAULT_SCHEMA),
            table: Some("events"),
            expected_path: "analytics.public.events",
            columns: Columns::Exactly(&["at"]),
        },
        PathCase {
            label: "no schema named, so the path carries an empty component for it",
            database: DEV,
            schema: None,
            table: Some("orders"),
            expected_path: "dev..orders",
            columns: Columns::Exactly(&["order_id"]),
        },
        PathCase {
            label: "a schema carrying only whitespace names no schema",
            database: DEV,
            schema: Some("   "),
            table: Some("orders"),
            expected_path: "dev..orders",
            columns: Columns::Exactly(&["order_id"]),
        },
        PathCase {
            label: "a table the database holds nothing of that name for",
            database: DEV,
            schema: Some(DEFAULT_SCHEMA),
            table: Some("never_created"),
            expected_path: "dev.public.never_created",
            columns: Columns::None,
        },
        PathCase {
            label: "a schema the database does not hold",
            database: DEV,
            schema: Some("absent_schema"),
            table: Some("regions"),
            expected_path: "dev.absent_schema.regions",
            columns: Columns::None,
        },
        PathCase {
            label: "a schema and a table the database holds neither of",
            database: DEV,
            schema: Some("absent_schema"),
            table: Some("never_created"),
            expected_path: "dev.absent_schema.never_created",
            columns: Columns::None,
        },
        PathCase {
            label: "an absent table of the second database",
            database: ANALYTICS,
            schema: Some(DEFAULT_SCHEMA),
            table: Some("never_created"),
            expected_path: "analytics.public.never_created",
            columns: Columns::None,
        },
        PathCase {
            label: "no table named, so every table of the schema is described",
            database: DEV,
            schema: Some(DEFAULT_SCHEMA),
            table: None,
            expected_path: "dev.public.",
            columns: Columns::Several,
        },
        PathCase {
            label: "neither selector named, so both components are empty",
            database: DEV,
            schema: None,
            table: None,
            expected_path: "dev..",
            columns: Columns::Several,
        },
    ];

    for case in &cases {
        let label = case.label;
        let mut request = connect(case.database);
        if let Some(schema) = case.schema {
            request["Schema"] = json!(schema);
        }
        if let Some(table) = case.table {
            request["Table"] = json!(table);
        }
        let described = emulator.ok("DescribeTable", &request).await;

        let expected = case.expected_path;
        assert_eq!(
            described["TableName"], json!(expected),
            "case {label:?}: expected TableName {expected:?}, got {:?} ({described})",
            described.get("TableName")
        );

        let reported = described_columns(&described);
        match case.columns {
            Columns::Exactly(expected_columns) => assert_eq!(
                reported, expected_columns,
                "case {label:?}: expected the columns {expected_columns:?}, got {reported:?} \
                 ({described})"
            ),
            Columns::None => assert!(
                reported.is_empty(),
                "case {label:?}: a name the catalogue matches nothing for describes no column, \
                 got {reported:?} ({described})"
            ),
            Columns::Several => assert!(
                !reported.is_empty(),
                "case {label:?}: naming no table describes every matching table, got \
                 {reported:?} ({described})"
            ),
        }
    }
}

/// The path survives the table: a DROPped table is described with the SAME path
/// the request asked about and an empty `ColumnList`, which is what lets a
/// caller prove through `DescribeTable` that a table is gone.
#[tokio::test]
async fn a_dropped_table_is_described_with_the_path_that_was_asked_for() {
    let emulator = Emulator::new();
    // A statement that builds nothing, so the database this probe reads the
    // catalogue of exists while the table it asks about does not.
    run_on(&emulator, DEV, "SELECT 1").await;
    let path = format!("{DEV}.{DEFAULT_SCHEMA}.transient");
    let mut request = connect(DEV);
    request["Schema"] = json!(DEFAULT_SCHEMA);
    request["Table"] = json!("transient");

    // Before the table exists at all.
    let before = emulator.ok("DescribeTable", &request).await;
    assert_eq!(
        before["TableName"], json!(path),
        "the pre-create probe names the path it asked about ({path}), got {before}"
    );
    assert!(
        list(&before, "ColumnList").is_empty(),
        "a table that was never created describes no column, got {before}"
    );

    run_on(
        &emulator,
        DEV,
        "CREATE TABLE transient (id INTEGER, label VARCHAR(12))",
    )
    .await;
    let created = emulator.ok("DescribeTable", &request).await;
    assert_eq!(
        created["TableName"], json!(path),
        "the created table is described under the same path ({path}), got {created}"
    );
    assert_eq!(
        described_columns(&created),
        vec!["id".to_string(), "label".to_string()],
        "the created table is described with the columns it declares, got {created}"
    );

    run_on(&emulator, DEV, "DROP TABLE transient").await;
    let dropped = emulator.ok("DescribeTable", &request).await;
    assert_eq!(
        dropped["TableName"], json!(path),
        "the dropped table's probe still names the path it asked about ({path}), got {dropped}"
    );
    assert!(
        list(&dropped, "ColumnList").is_empty(),
        "a dropped table describes no column, got {dropped}"
    );
}
