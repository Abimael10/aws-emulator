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
//! block of one column of one table on one slice — so it reports the tables this
//! database holds, the columns each of them carries, the values each column
//! holds and the slices this warehouse spreads a table over. A table occupies a
//! block per column on every slice from the moment it exists, whether or not it
//! holds a row (the published minimum size of a table is one block for each of
//! its columns on each slice), and the columns it carries are the ones it
//! declares plus the three Amazon Redshift appends to every table it stores
//! (`INSERT_XID`, `DELETE_XID`, `ROW_ID`). How the slices themselves follow the
//! warehouse's shape is asserted in [`super::storage_block_geometry`].
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
    /// How many columns it DECLARES; the blocks reported for it are one per
    /// declared column plus one per hidden column ([`HIDDEN_COLUMNS`]).
    columns: i64,
    /// How many rows it holds, which is the `num_values` each of those blocks
    /// carries.
    rows: i64,
}

impl Relation {
    /// How many blocks `STV_BLOCKLIST` reports for this table: one per column it
    /// carries, on each slice this warehouse spreads it over.
    fn blocks(&self) -> i64 {
        (self.columns + HIDDEN_COLUMNS) * SLICES
    }
}

/// The columns Amazon Redshift appends to every table it stores beyond the ones
/// the table declares: `INSERT_XID`, `DELETE_XID` and `ROW_ID`, which the
/// `STV_BLOCKLIST` reference names as the three hidden columns of every table.
const HIDDEN_COLUMNS: i64 = 3;

/// The slices the warehouse these statements run on spreads a table over.
///
/// This harness mounts the Data API alone, so no control plane answers for the
/// cluster the statements name and its shape is the published floor: the
/// smallest slices-per-node figure the Amazon Redshift Cluster Management Guide
/// publishes, over one node.
const SLICES: i64 = 2;

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

/// `STV_BLOCKLIST` reports one block per column of every table the database
/// holds — the declared columns and the hidden ones — on each slice this
/// warehouse spreads a table over, including for a table that holds no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_block_list_reports_a_block_per_column_of_every_table() {
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
        let expected = relation.blocks();
        assert_eq!(
            blocks, expected,
            "case {case}: {} holds {} rows in {} declared columns, so {expected} blocks are \
             reported, got {blocks}",
            relation.table, relation.rows, relation.columns
        );

        // Each of them is a block of one of that table's columns, numbered from
        // zero, on one of the slices this warehouse spreads it over, and carries
        // the values that column holds.
        let described = number(
            &emulator,
            case,
            &format!(
                "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = b.tbl \
                 WHERE t.\"table\" = '{}' AND b.slice >= 0 AND b.slice < {SLICES} AND \
                 b.blocknum = 0 AND b.num_values = {} AND b.col >= 0 AND b.col < {}",
                relation.table,
                relation.rows,
                relation.columns + HIDDEN_COLUMNS
            ),
        )
        .await;
        assert_eq!(
            described,
            relation.blocks(),
            "case {case}: every block of {} is one of its {} columns' and carries its {} values, \
             got {described} such blocks",
            relation.table,
            relation.blocks(),
            relation.rows
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
            columns,
            relation.columns + HIDDEN_COLUMNS,
            "case {case}: {} has one block per column, so its blocks name {} distinct columns, \
             got {columns}",
            relation.table,
            relation.columns + HIDDEN_COLUMNS
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
    let total: i64 = RELATIONS.iter().map(Relation::blocks).sum();
    let reported = number(&emulator, case, "SELECT count(*) FROM stv_blocklist").await;
    assert_eq!(
        reported, total,
        "case {case}: the blocks reported are the ones the database's tables hold: expected \
         {total}, got {reported}"
    );

    // And it is a relation a statement can select out of into a table of its
    // own, bounded by a `LIMIT`. The rows differ in the bound: one row, several,
    // and a bound a database holding only a couple of tables still satisfies —
    // the block list of a table that has just been created is not empty.
    finished(&emulator, case, "CREATE TABLE copied (n INT)").await;
    for limit in [1_i64, 2, 5] {
        let before = number(&emulator, case, "SELECT count(*) FROM copied").await;
        finished(
            &emulator,
            case,
            &format!("INSERT INTO copied SELECT 1 FROM stv_blocklist LIMIT {limit}"),
        )
        .await;
        let copied = number(&emulator, case, "SELECT count(*) FROM copied").await;
        assert_eq!(
            copied - before,
            limit,
            "case {case} with LIMIT {limit}: a bounded read of the block list inserts the rows it \
             answered: expected {limit} more than {before}, got {copied}"
        );
    }
}
