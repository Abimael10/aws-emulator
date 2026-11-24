// SPDX-License-Identifier: CC0-1.0

//! `enable_case_sensitive_identifier`: whether the cluster's engine reads the
//! CASE of a delimited identifier.
//!
//! The rule under test: the engine runs with the value in force for that
//! parameter — the session's own where `SET` gave it one, else the value the
//! restart that applied the cluster's parameter group left it with — and when
//! that value is on, two delimited identifiers differing only in case are two
//! distinct names, each resolving to its own storage and reported under the name
//! it was declared with. With the value off they are one name, and the
//! declaration of both is refused.
//!
//! Asserted over a TABLE of name pairs rather than one recorded statement: the
//! rows differ in WHERE the case differs (the whole name, its first letter, a
//! letter inside it), in how many names of one table differ only in case, and in
//! whether the name is a column's or a table's.
//!
//! The harness is the one `warehouse_data_plane` owns, so these suites drive the
//! same emulator every other suite of this pair does.

use serde_json::json;

use crate::warehouse_data_plane::{Harness, DATABASE, DB_USER};

/// The parameter the whole rule hangs on.
const PARAMETER: &str = "enable_case_sensitive_identifier";

/// One pair (or triple) of names differing only in case: the table to declare
/// them on, the declaration, and the names the catalogue must then report in
/// declaration order.
struct Row {
    case: &'static str,
    table: &'static str,
    declaration: &'static str,
    columns: &'static [&'static str],
    /// A query over the created table that reads each column back by its own
    /// delimited name, and the values it must answer in order.
    read: &'static str,
    values: &'static [&'static str],
}

/// A static parameter change reaches the engine at the reboot that applies it:
/// before it the mixed-case declaration is refused, after it the engine holds
/// each delimited name as its own column.
#[tokio::test]
async fn a_rebooted_cluster_reads_the_case_of_a_delimited_identifier() {
    let harness = Harness::new();
    let cluster = "case-sensitive-identifiers";
    let group = "case-sensitive-group";
    harness.create_cluster(cluster).await;

    // Before anything changes the parameter, the engine folds a delimited
    // identifier: the two columns are one name, so the declaration is refused
    // and the parameter reports its published default.
    let refused = harness
        .run_failing(
            "the declaration before the parameter changed",
            cluster,
            "CREATE TABLE mixed (\"c\" INT, \"C\" INT)",
        )
        .await;
    assert!(
        !refused.is_empty(),
        "with the parameter off the declaration of two names differing only in case is refused"
    );
    let reported = harness
        .one_value("the published default", cluster, &format!("SHOW {PARAMETER}"))
        .await;
    assert_eq!(
        reported, "false",
        "case the published default: SHOW {PARAMETER} must report what the engine runs with"
    );

    // The parameter group the cluster is configured with, and the change to it.
    harness
        .redshift_ok(
            "CreateClusterParameterGroup",
            &[
                ("ParameterGroupName", group),
                ("ParameterGroupFamily", "redshift-1.0"),
                ("Description", "the group this cluster is configured with"),
            ],
        )
        .await;
    harness
        .redshift_ok(
            "ModifyCluster",
            &[
                ("ClusterIdentifier", cluster),
                ("ClusterParameterGroupName", group),
            ],
        )
        .await;
    harness
        .await_cluster_available(cluster, "configured with its own parameter group")
        .await;
    harness
        .redshift_ok(
            "ModifyClusterParameterGroup",
            &[
                ("ParameterGroupName", group),
                ("Parameters.Parameter.1.ParameterName", PARAMETER),
                ("Parameters.Parameter.1.ParameterValue", "true"),
            ],
        )
        .await;

    // The change is static, so until the restart applies it the engine is still
    // running with the old value — and still refuses the declaration.
    let before = harness
        .one_value(
            "the value before the restart that applies it",
            cluster,
            &format!("SHOW {PARAMETER}"),
        )
        .await;
    assert_eq!(
        before, "false",
        "a static parameter change is not what the engine runs with until the cluster reboots"
    );
    harness
        .run_failing(
            "the declaration before the restart",
            cluster,
            "CREATE TABLE mixed (\"c\" INT, \"C\" INT)",
        )
        .await;

    harness
        .redshift_ok("RebootCluster", &[("ClusterIdentifier", cluster)])
        .await;
    harness.await_cluster_available(cluster, "restarted").await;
    let after = harness
        .one_value(
            "the value the restart applied",
            cluster,
            &format!("SHOW {PARAMETER}"),
        )
        .await;
    assert_eq!(
        after, "true",
        "the restart applies the change, which is what the engine then runs with"
    );

    let rows = [
        Row {
            case: "two columns differing in the case of the whole name",
            table: "mixed",
            declaration: "\"c\" INT, \"C\" INT",
            columns: &["c", "C"],
            read: "SELECT \"c\", \"C\" FROM mixed",
            values: &["1", "2"],
        },
        Row {
            case: "two columns differing in their first letter",
            table: "leading",
            declaration: "\"id\" INT, \"Id\" INT",
            columns: &["id", "Id"],
            read: "SELECT \"id\", \"Id\" FROM leading",
            values: &["1", "2"],
        },
        Row {
            case: "two columns differing in a letter inside the name",
            table: "inner_case",
            declaration: "\"aB\" INT, \"Ab\" INT",
            columns: &["aB", "Ab"],
            read: "SELECT \"aB\", \"Ab\" FROM inner_case",
            values: &["1", "2"],
        },
        Row {
            case: "three columns of one table differing only in case",
            table: "three",
            declaration: "\"value\" INT, \"Value\" INT, \"VALUE\" INT",
            columns: &["value", "Value", "VALUE"],
            read: "SELECT \"value\", \"Value\", \"VALUE\" FROM three",
            values: &["1", "2", "3"],
        },
        Row {
            case: "a column whose name is already lower case, held as it always was",
            table: "folded",
            declaration: "\"plain\" INT, other INT",
            columns: &["plain", "other"],
            read: "SELECT \"plain\", other FROM folded",
            values: &["1", "2"],
        },
    ];

    for row in &rows {
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("CREATE TABLE {} ({})", row.table, row.declaration),
            )
            .await;
        // The catalogue describes the columns the engine really declares, in
        // declaration order, under the names they were declared with.
        let described = harness
            .data_ok(
                "DescribeTable",
                &json!({
                    "ClusterIdentifier": cluster,
                    "Database": DATABASE,
                    "DbUser": DB_USER,
                    "Table": row.table,
                }),
            )
            .await;
        let described_columns: Vec<String> = described["ColumnList"]
            .as_array()
            .map(|columns| {
                columns
                    .iter()
                    .map(|column| column["name"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            described_columns,
            row.columns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {:?}: the catalogue must describe the declared names in declaration order, got \
             {described}",
            row.case
        );

        // Each column is its own storage: a value written into one is read back
        // out of that one.
        let values = row.values.join(", ");
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("INSERT INTO {} VALUES ({values})", row.table),
            )
            .await;
        let read = harness.query_rows(row.case, cluster, row.read).await;
        let answered: Vec<String> = read
            .first()
            .map(|row| {
                row.iter()
                    .map(crate::warehouse_data_plane::field_text)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            answered,
            row.values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {:?}: each delimited name must read back its own column",
            row.case
        );
    }

    // An UNQUOTED name is still folded, whatever the parameter says, so it
    // names the lower-case column and not the upper-case one beside it.
    let folded = harness
        .one_value(
            "an unquoted reference, which this dialect folds",
            cluster,
            "SELECT c FROM mixed",
        )
        .await;
    assert_eq!(
        folded, "1",
        "an unquoted identifier names the lower-case column whatever the parameter says"
    );

    // A delimited TABLE name keeps its case too, so two tables differing only
    // in case are two tables.
    harness
        .run_ok(
            "two tables differing only in case",
            cluster,
            "CREATE TABLE \"Sales\" (amount INT)",
        )
        .await;
    harness
        .run_ok(
            "two tables differing only in case",
            cluster,
            "CREATE TABLE \"sales\" (amount INT)",
        )
        .await;
    harness
        .run_ok(
            "a row in the upper-case table alone",
            cluster,
            "INSERT INTO \"Sales\" (amount) VALUES (11)",
        )
        .await;
    let counted = harness
        .one_value(
            "the lower-case table, which is a table of its own",
            cluster,
            "SELECT count(*) FROM \"sales\"",
        )
        .await;
    assert_eq!(
        counted, "0",
        "two table names differing only in case must be two tables"
    );
    let listed = harness
        .data_ok(
            "ListTables",
            &json!({
                "ClusterIdentifier": cluster,
                "Database": DATABASE,
                "DbUser": DB_USER,
                "TablePattern": "%ales",
            }),
        )
        .await;
    let names: Vec<String> = listed["Tables"]
        .as_array()
        .map(|tables| {
            tables
                .iter()
                .map(|table| table["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.contains(&"Sales".to_string()) && names.contains(&"sales".to_string()),
        "the catalogue must list both tables under the names they were created with, got {names:?}"
    );
}

/// A SESSION that sets the parameter runs with the value it set, which is the
/// same rule read one level higher: the value in force on the connection.
#[tokio::test]
async fn a_session_that_sets_the_parameter_reads_the_case_of_its_own_names() {
    let harness = Harness::new();
    let cluster = "case-sensitive-session";
    harness.create_cluster(cluster).await;

    // One submission, because a session parameter belongs to the CONNECTION the
    // statements share: the set and the declaration run on one.
    harness
        .run_ok(
            "a declaration under the session's own value",
            cluster,
            &format!(
                "SET {PARAMETER} TO true; CREATE TABLE sessioned (\"d\" INT, \"D\" INT); \
                 INSERT INTO sessioned VALUES (4, 5)"
            ),
        )
        .await;

    // The catalogue reports the names as declared, whatever the connection
    // reading it has set: the names are the engine's, not the session's.
    let described = harness
        .data_ok(
            "DescribeTable",
            &json!({
                "ClusterIdentifier": cluster,
                "Database": DATABASE,
                "DbUser": DB_USER,
                "Table": "sessioned",
            }),
        )
        .await;
    let columns: Vec<String> = described["ColumnList"]
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|column| column["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        columns,
        vec!["d".to_string(), "D".to_string()],
        "the session's value is what its declaration ran under: {described}"
    );

    // And a connection that has NOT set it runs with the engine's value, which
    // is off: the declaration of two names differing only in case is refused
    // there.
    harness
        .run_failing(
            "a connection that set nothing",
            cluster,
            "CREATE TABLE unsessioned (\"e\" INT, \"E\" INT)",
        )
        .await;
}
