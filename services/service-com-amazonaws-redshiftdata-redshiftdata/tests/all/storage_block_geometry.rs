// SPDX-License-Identifier: CC0-1.0

//! The STORAGE GEOMETRY the cluster's system tables report, driven over the wire
//! through the shared harnesses in [`super::emulator`]: every call below is a
//! real `POST /` with `X-Amz-Target: RedshiftData.<Operation>`, so what is
//! asserted is the emulator's HTTP behaviour and not a handler's return value.
//!
//! The rule under test: Amazon Redshift stores a table COLUMN by column, in
//! blocks of one megabyte, on EVERY slice of the warehouse's compute —
//! `STV_BLOCKLIST` "contains one row per 1 MB disk block", and the published
//! minimum size of a table is "the number of columns × the number of slices ×
//! the block size". So the rows that table answers with are the product of three
//! real things: the columns each table carries (the ones it declares plus the
//! three Amazon Redshift appends to every table it stores), the slices the
//! warehouse spreads a table over, and the blocks each column's values occupy —
//! and a relation it reports is never a single synthetic row.
//!
//! Three things follow, and each is asserted over a TABLE of cases rather than
//! on one:
//!
//! * the slices come from the warehouse's own SHAPE, so a cluster of a node type
//!   with more slices per node, or of more nodes, reports proportionally more
//!   blocks for the same table;
//! * the blocks come from the DATA, so the same number of values of a wider
//!   declared type occupies more of them;
//! * the block list is an ordinary relation of the database, so a bounded read
//!   of it answers the rows it was asked for — which is what makes it the row
//!   generator a Redshift client uses it as.

use serde_json::{json, Value};

use super::emulator::{id_of, list, wait_for_terminal, Emulator, Warehouses, WAIT_SECONDS};

/// The columns Amazon Redshift appends to every table it stores beyond the ones
/// the table declares: `INSERT_XID`, `DELETE_XID` and `ROW_ID`, which the
/// `STV_BLOCKLIST` reference names as the three hidden columns of every table.
const HIDDEN_COLUMNS: i64 = 3;

/// The width one value of a hidden column takes: those three are 64-bit values,
/// which the *Data types* reference stores in 8 bytes.
const HIDDEN_COLUMN_WIDTH: i64 = 8;

/// The size of one disk block: `STV_BLOCKLIST` reports "the number of 1 MB disk
/// blocks used by each slice, table, or column in a database".
const BLOCK_BYTES: i64 = 1024 * 1024;

/// How many blocks `rows` values of a column `width` bytes wide occupy on one
/// slice. A column of a stored table occupies at least one, whether or not it
/// holds a row: the published minimum size of a table is one block for each of
/// its columns on each slice.
fn blocks(width: i64, rows: i64) -> i64 {
    let bytes = u64::try_from(width * rows).unwrap_or_default();
    let block = u64::try_from(BLOCK_BYTES).unwrap_or(1);
    i64::try_from(bytes.div_ceil(block)).unwrap_or(1).max(1)
}

/// Run `sql` on the cluster `cluster` of the warehouse harness, wait for it, and
/// require that it FINISHED.
async fn finished(warehouses: &Warehouses, case: &str, cluster: &str, sql: &str) -> Value {
    let submitted = warehouses
        .data_ok(
            "ExecuteStatement",
            &json!({
                "ClusterIdentifier": cluster,
                "Database": "dev",
                "DbUser": "awsuser",
                "Sql": sql,
                "WaitTimeSeconds": WAIT_SECONDS,
            }),
        )
        .await;
    let described = poll(warehouses, case, &id_of(&submitted)).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: [{sql}] must FINISH, got {described}"
    );
    described
}

/// Poll one statement of the warehouse harness until it stops.
///
/// The CONDITION is waited for, with a deadline generous enough that a poll
/// gives up only because a statement never terminated: an execution runs
/// independently of the request that submitted it.
async fn poll(warehouses: &Warehouses, case: &str, id: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let described = warehouses
            .data_ok("DescribeStatement", &json!({"Id": id}))
            .await;
        let status = described["Status"].as_str().unwrap_or_default().to_string();
        if ["FINISHED", "FAILED", "ABORTED"].contains(&status.as_str()) {
            return described;
        }
        assert!(
            started.elapsed() < TERMINAL_BUDGET,
            "case {case}: {id} reported {status:?} for {:?} and never reached a terminal status: \
             {described}",
            started.elapsed()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// How long a statement of the warehouse harness is given to stop: the engine's
/// own execution bound and room to spare.
const TERMINAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// The single number `sql` answers, on the warehouse harness.
async fn number(warehouses: &Warehouses, case: &str, cluster: &str, sql: &str) -> i64 {
    let described = finished(warehouses, case, cluster, sql).await;
    let fetched = warehouses
        .data_ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    sole_number(case, sql, &fetched)
}

/// The one number a result page carries.
fn sole_number(case: &str, sql: &str, fetched: &Value) -> i64 {
    let records = list(fetched, "Records");
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

/// One warehouse shape, and the slices its compute therefore has.
struct Shape {
    /// The case's name.
    case: &'static str,
    /// The cluster identifier the case creates.
    cluster: &'static str,
    /// Its node type.
    node_type: &'static str,
    /// How many nodes it runs, or `None` for a single-node cluster.
    nodes: Option<i32>,
    /// The slices that shape has: the slices per node the Amazon Redshift
    /// Cluster Management Guide publishes for the node type, over the nodes the
    /// cluster runs.
    slices: i64,
}

/// The shapes asserted over: node types whose published slices per node differ,
/// and a cluster of more than one node.
const SHAPES: &[Shape] = &[
    Shape {
        case: "the smallest published node type, single node",
        cluster: "two-slice",
        node_type: "ra3.xlplus",
        nodes: None,
        slices: 2,
    },
    Shape {
        case: "a node type with four slices per node, over two nodes",
        cluster: "eight-slice",
        node_type: "ra3.4xlarge",
        nodes: Some(2),
        slices: 8,
    },
    Shape {
        case: "a node type with sixteen slices per node, over two nodes",
        cluster: "thirty-two-slice",
        node_type: "ra3.16xlarge",
        nodes: Some(2),
        slices: 32,
    },
    Shape {
        case: "three nodes of the smallest published node type",
        cluster: "six-slice",
        node_type: "ra3.xlplus",
        nodes: Some(3),
        slices: 6,
    },
];

/// The blocks `STV_BLOCKLIST` reports for one table are its columns' blocks on
/// every slice of the warehouse's compute, so a warehouse of a wider shape
/// reports proportionally more of them for the same table.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_block_list_reports_a_block_per_column_on_every_slice() {
    for shape in SHAPES {
        let warehouses = Warehouses::new();
        warehouses
            .create_cluster_of_shape(shape.cluster, shape.node_type, shape.nodes, Some("dev"))
            .await;
        finished(
            &warehouses,
            shape.case,
            shape.cluster,
            "CREATE TABLE one_column (a INT)",
        )
        .await;
        finished(
            &warehouses,
            shape.case,
            shape.cluster,
            "INSERT INTO one_column VALUES (1), (2), (3)",
        )
        .await;

        // One block per column of the table — the column it declares and the
        // three hidden ones — on each slice.
        let expected = (1 + HIDDEN_COLUMNS) * shape.slices;
        let reported = number(
            &warehouses,
            shape.case,
            shape.cluster,
            "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = b.tbl \
             WHERE t.\"table\" = 'one_column'",
        )
        .await;
        assert_eq!(
            reported, expected,
            "case {}: {} of {} nodes has {} slices, so a one-column table reports {expected} \
             blocks, got {reported}",
            shape.case,
            shape.node_type,
            shape.nodes.unwrap_or(1),
            shape.slices
        );

        // And those blocks are spread over the slices, one per column on each.
        let slices = number(
            &warehouses,
            shape.case,
            shape.cluster,
            "SELECT count(DISTINCT b.slice) FROM stv_blocklist b JOIN svv_table_info t ON \
             t.table_id = b.tbl WHERE t.\"table\" = 'one_column'",
        )
        .await;
        assert_eq!(
            slices, shape.slices,
            "case {}: the blocks of a table are spread over all {} slices, got {slices}",
            shape.case, shape.slices
        );

        // Every block carries the values its column holds, numbered from zero
        // on each slice.
        let described = number(
            &warehouses,
            shape.case,
            shape.cluster,
            "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = b.tbl \
             WHERE t.\"table\" = 'one_column' AND b.blocknum = 0 AND b.num_values = 3 AND \
             b.col >= 0 AND b.col < 4",
        )
        .await;
        assert_eq!(
            described, expected,
            "case {}: every one of the {expected} blocks of a one-column table holding three \
             rows is the first block of one of its four columns and carries its three values, \
             got {described}",
            shape.case
        );

        // `SVV_TABLE_INFO` reports the same table's size in those same blocks,
        // so the two system tables cannot disagree about what it occupies.
        let size = number(
            &warehouses,
            shape.case,
            shape.cluster,
            "SELECT size FROM svv_table_info WHERE \"table\" = 'one_column'",
        )
        .await;
        assert_eq!(
            size, expected,
            "case {}: the size the catalogue view reports is the blocks the block list lists, \
             expected {expected}, got {size}",
            shape.case
        );
    }
}

/// One stored column, and the blocks its values occupy.
struct Stored {
    /// The case's name.
    case: &'static str,
    /// The table the case creates.
    table: &'static str,
    /// The type its one column is declared with.
    declared: &'static str,
    /// The bytes one value of that type is stored in, which the Amazon Redshift
    /// *Data types* reference publishes: 2 for `SMALLINT`, 4 for `INTEGER`, 8
    /// for `BIGINT`, and 16 for a `DECIMAL` of more than 18 digits of precision.
    width: i64,
}

/// The stored columns asserted over: the same number of values, in declared
/// types of four different widths, so what differs between the cases is how much
/// of a block the data takes.
const STORED: &[Stored] = &[
    Stored {
        case: "two-byte values",
        table: "narrow",
        declared: "SMALLINT",
        width: 2,
    },
    Stored {
        case: "four-byte values",
        table: "medium",
        declared: "INTEGER",
        width: 4,
    },
    Stored {
        case: "eight-byte values",
        table: "wide",
        declared: "BIGINT",
        width: 8,
    },
    Stored {
        case: "sixteen-byte values",
        table: "widest",
        declared: "DECIMAL(38,2)",
        width: 16,
    },
];

/// How many values each table of [`STORED`] holds: enough that the widest
/// declared type outgrows a single block and the narrowest does not, so what the
/// block count follows is the data and not the number of columns.
const STORED_ROWS: i64 = 150_000;

/// The blocks `STV_BLOCKLIST` reports for a column are the blocks its values
/// really occupy, so the same number of values of a wider declared type takes
/// more of them — and the values are spread over the blocks that hold them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_block_list_reports_the_blocks_a_column_s_values_occupy() {
    let warehouses = Warehouses::new();
    let cluster = "stored";
    // The smallest published node type, whose slices are what every count below
    // is multiplied by.
    let slices = 2_i64;
    warehouses
        .create_cluster_of_shape(cluster, "ra3.xlplus", None, Some("dev"))
        .await;
    for stored in STORED {
        finished(
            &warehouses,
            stored.case,
            cluster,
            &format!("CREATE TABLE {} (v {})", stored.table, stored.declared),
        )
        .await;
        finished(
            &warehouses,
            stored.case,
            cluster,
            &format!(
                "WITH RECURSIVE counter(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM counter \
                 WHERE n < {STORED_ROWS}) INSERT INTO {} SELECT n FROM counter",
                stored.table
            ),
        )
        .await;
    }

    for stored in STORED {
        let declared_blocks = blocks(stored.width, STORED_ROWS);
        let reported = number(
            &warehouses,
            stored.case,
            cluster,
            &format!(
                "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t ON t.table_id = \
                 b.tbl WHERE t.\"table\" = '{}' AND b.col = 0",
                stored.table
            ),
        )
        .await;
        assert_eq!(
            reported,
            declared_blocks * slices,
            "case {}: {STORED_ROWS} values of {} bytes occupy {declared_blocks} block(s) on each \
             of {slices} slices, got {reported}",
            stored.case,
            stored.width
        );

        // The values of that column are spread over the blocks that hold them,
        // on each slice: every value is in exactly one block.
        let values = number(
            &warehouses,
            stored.case,
            cluster,
            &format!(
                "SELECT sum(b.num_values) FROM stv_blocklist b JOIN svv_table_info t ON \
                 t.table_id = b.tbl WHERE t.\"table\" = '{}' AND b.col = 0",
                stored.table
            ),
        )
        .await;
        assert_eq!(
            values,
            STORED_ROWS * slices,
            "case {}: the blocks of a column hold its {STORED_ROWS} values on each of {slices} \
             slices, got {values}",
            stored.case
        );

        // And the whole table is its declared column's blocks beside the hidden
        // columns', which are eight-byte values of the same count.
        let hidden_blocks = blocks(HIDDEN_COLUMN_WIDTH, STORED_ROWS) * HIDDEN_COLUMNS;
        let total = number(
            &warehouses,
            stored.case,
            cluster,
            &format!(
                "SELECT size FROM svv_table_info WHERE \"table\" = '{}'",
                stored.table
            ),
        )
        .await;
        assert_eq!(
            total,
            (declared_blocks + hidden_blocks) * slices,
            "case {}: the table occupies its column's {declared_blocks} block(s) and the hidden \
             columns' {hidden_blocks}, on each of {slices} slices, got {total}",
            stored.case
        );
    }
}

/// The connection the row-generator cases below submit on: the Data API mounted
/// on its own, where the warehouse's shape is the published floor.
fn connection() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// `STV_BLOCKLIST` is the relation a Redshift client generates rows from, so a
/// bounded read of it answers exactly the rows it was asked for, and an `INSERT`
/// that selects from it commits exactly that many.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bounded_read_of_the_block_list_generates_the_rows_it_was_asked_for() {
    let case = "the block list as a row generator";
    let emulator = Emulator::new();
    for sql in [
        "CREATE TABLE ctrl (n INT)",
        "CREATE TABLE other (a INT, b INT)",
    ] {
        let mut request = connection();
        request["Sql"] = json!(sql);
        request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
        let submitted = emulator.ok("ExecuteStatement", &request).await;
        let described = wait_for_terminal(&emulator, case, &id_of(&submitted)).await;
        assert_eq!(
            described["Status"], "FINISHED",
            "case {case}: [{sql}] must FINISH, got {described}"
        );
    }

    // How many blocks this database really holds, which is what bounds how many
    // rows a read of the list can generate.
    let held = count(&emulator, case, "SELECT count(*) FROM stv_blocklist").await;
    assert!(
        held >= 8,
        "case {case}: two tables of one and two declared columns occupy a block per column on \
         every slice, so the list holds at least eight: got {held}"
    );

    // The bounds differ in how many rows they ask for: one, a few, and the
    // whole list.
    for limit in [1_i64, 2, 5, 8, held] {
        let before = count(&emulator, case, "SELECT count(*) FROM ctrl").await;
        run(
            &emulator,
            case,
            &format!("INSERT INTO ctrl SELECT 1 FROM stv_blocklist LIMIT {limit}"),
        )
        .await;
        let after = count(&emulator, case, "SELECT count(*) FROM ctrl").await;
        assert_eq!(
            after - before,
            limit,
            "case {case} with LIMIT {limit}: a bounded read of the block list commits the rows it \
             answered: expected {limit} more than {before}, got {after}"
        );
    }
}

/// Run `sql` on the Data-API-only harness and require that it FINISHED.
async fn run(emulator: &Emulator, case: &str, sql: &str) -> Value {
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

/// The single number `sql` answers on the Data-API-only harness.
async fn count(emulator: &Emulator, case: &str, sql: &str) -> i64 {
    let described = run(emulator, case, sql).await;
    let fetched = emulator
        .ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    sole_number(case, sql, &fetched)
}
