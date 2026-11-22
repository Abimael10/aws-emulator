// SPDX-License-Identifier: CC0-1.0

//! The cluster's STORAGE system table, driven over the wire through the shared
//! harness in [`super::emulator`]: every call below is a real `POST /` with
//! `X-Amz-Target: RedshiftData.<Operation>`, so what is asserted is the
//! emulator's HTTP behaviour and not a handler's return value.
//!
//! The rule under test: a Redshift database exposes its system tables as
//! RELATIONS, so a statement that reads one runs like any other statement and
//! answers rows about this database. `STV_BLOCKLIST` is "the number of 1 MB
//! disk blocks used by each slice, table, or column in a database", one row per
//! block of one column of one table — so it reports the tables this database
//! holds, the columns each of them declares and the values each column holds,
//! and nothing for a table that holds no row.
//!
//! It is asserted over a TABLE of relations rather than one case: the rows
//! differ in how many columns a table declares and in how many rows it holds,
//! including none.

use serde_json::{json, Value};

use super::emulator::{id_of, list, wait_for_terminal, Emulator, WAIT_SECONDS};

/// The connection every submission below is made with: one documented
/// authorization combination (a provisioned cluster as a database user).
fn connection() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// Run `sql`, wait for it, and require that it FINISHED.
async fn finished(emulator: &Emulator, case: &str, sql: &str) -> Value {
    let mut request = connection();
    request["Sql"] = json!(sql);
    request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
    let submitted = emulator.ok("ExecuteStatement", &request).await;
    let described = wait_for_terminal(emulator, case, &id_of(&submitted)).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: [{sql}] must FINISH, got {described}"
    );
    described
}

/// The single number `sql` answers.
async fn number(emulator: &Emulator, case: &str, sql: &str) -> i64 {
    let described = finished(emulator, case, sql).await;
    let fetched = emulator
        .ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    let records = list(&fetched, "Records");
    assert_eq!(
        records.len(),
        1,
        "case {case}: [{sql}] answers one row, got {fetched}"
    );
    records[0]
        .as_array()
        .and_then(|cells| cells.first())
        .and_then(|cell| cell["longValue"].as_i64())
        .unwrap_or_else(|| panic!("case {case}: [{sql}] answers one number, got {fetched}"))
}

/// One table of the database: what it declares, what it holds, and the blocks
/// `STV_BLOCKLIST` therefore reports for it.
struct Relation {
    /// The table's name.
    table: &'static str,
    /// The statement that creates it.
    create: &'static str,
    /// The statement that fills it, if the case fills it at all.
    fill: Option<&'static str>,
    /// How many columns it declares, which is how many blocks of it are
    /// reported.
    columns: i64,
    /// How many rows it holds, which is the `num_values` each of those blocks
    /// carries.
    rows: i64,
}

const RELATIONS: &[Relation] = &[
    Relation {
        table: "one_column",
        create: "CREATE TABLE one_column (a INT)",
        fill: Some("INSERT INTO one_column VALUES (1), (2), (3)"),
        columns: 1,
        rows: 3,
    },
    Relation {
        table: "three_columns",
        create: "CREATE TABLE three_columns (a INT, b BIGINT, c VARCHAR(8))",
        fill: Some("INSERT INTO three_columns VALUES (1, 2, 'x'), (3, 4, 'y')"),
        columns: 3,
        rows: 2,
    },
    Relation {
        table: "six_columns",
        create: "CREATE TABLE six_columns (a INT, b INT, c INT, d INT, e INT, f INT)",
        fill: Some("INSERT INTO six_columns VALUES (1, 2, 3, 4, 5, 6)"),
        columns: 6,
        rows: 1,
    },
    Relation {
        table: "no_rows",
        create: "CREATE TABLE no_rows (a INT, b INT)",
        fill: None,
        columns: 2,
        rows: 0,
    },
];

/// `STV_BLOCKLIST` reports one block per column of every table that holds a
/// row, on the single slice this engine runs, and none for a table that holds
/// no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_block_list_reports_a_block_per_column_of_every_table_that_holds_rows() {
    let case = "the block list";
    let emulator = Emulator::new();
    for relation in RELATIONS {
        finished(&emulator, case, relation.create).await;
        if let Some(fill) = relation.fill {
            finished(&emulator, case, fill).await;
        }
    }

    for relation in RELATIONS {
        // The blocks reported for this table, joined to the table's own id as
        // the database's own catalogue view reports it: a table keeps one id
        // across the system tables that name it.
        let blocks = number(
            &emulator,
            case,
            &format!(
                "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = b.tbl \
                 WHERE t.\"table\" = '{}'",
                relation.table
            ),
        )
        .await;
        let expected = if relation.rows == 0 {
            0
        } else {
            relation.columns
        };
        assert_eq!(
            blocks, expected,
            "case {case}: {} holds {} rows in {} columns, so {expected} blocks are reported, got \
             {blocks}",
            relation.table, relation.rows, relation.columns
        );
        if relation.rows == 0 {
            continue;
        }

        // Each of them is a block of one of that table's columns, numbered from
        // zero, on the one slice this engine runs, and carries the values that
        // column holds.
        let described = number(
            &emulator,
            case,
            &format!(
                "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = b.tbl \
                 WHERE t.\"table\" = '{}' AND b.slice = 0 AND b.blocknum = 0 AND b.num_values = \
                 {} AND b.col >= 0 AND b.col < {}",
                relation.table, relation.rows, relation.columns
            ),
        )
        .await;
        assert_eq!(
            described, relation.columns,
            "case {case}: every block of {} is one of its {} columns' and carries its {} values, \
             got {described} such blocks",
            relation.table, relation.columns, relation.rows
        );
        let columns = number(
            &emulator,
            case,
            &format!(
                "SELECT count(DISTINCT b.col) FROM stv_blocklist b JOIN svv_table_info t ON \
                 t.table_id = b.tbl WHERE t.\"table\" = '{}'",
                relation.table
            ),
        )
        .await;
        assert_eq!(
            columns, relation.columns,
            "case {case}: {} has one block per column, so its blocks name {} distinct columns, \
             got {columns}",
            relation.table, relation.columns
        );
    }
}

/// A statement that reads the block list is an ordinary statement: it can be
/// read from, inserted from, and bounded with `LIMIT`, like any other relation
/// of the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_block_list_is_read_like_any_other_relation() {
    let case = "the block list as a relation";
    let emulator = Emulator::new();
    for relation in RELATIONS {
        finished(&emulator, case, relation.create).await;
        if let Some(fill) = relation.fill {
            finished(&emulator, case, fill).await;
        }
    }

    // Every block the table reports belongs to a table of this database, so the
    // whole list is what the rows above account for.
    let total: i64 = RELATIONS
        .iter()
        .filter(|relation| relation.rows > 0)
        .map(|relation| relation.columns)
        .sum();
    let reported = number(&emulator, case, "SELECT count(*) FROM stv_blocklist").await;
    assert_eq!(
        reported, total,
        "case {case}: the blocks reported are the ones the database's tables hold: expected \
         {total}, got {reported}"
    );

    // And it is a relation a statement can select out of into a table of its
    // own, bounded by a `LIMIT`.
    finished(&emulator, case, "CREATE TABLE copied (n INT)").await;
    let limit = total.min(2);
    finished(
        &emulator,
        case,
        &format!("INSERT INTO copied SELECT 1 FROM stv_blocklist LIMIT {limit}"),
    )
    .await;
    let copied = number(&emulator, case, "SELECT count(*) FROM copied").await;
    assert_eq!(
        copied, limit,
        "case {case}: a bounded read of the block list inserts the rows it answered: expected \
         {limit}, got {copied}"
    );
}
