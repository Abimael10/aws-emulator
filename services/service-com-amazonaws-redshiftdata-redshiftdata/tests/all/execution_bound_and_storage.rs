// SPDX-License-Identifier: CC0-1.0

//! What ENDS a statement, and the storage a warehouse reports about the tables
//! it holds — both driven over the wire.
//!
//! Two rules:
//!
//! 1. **a submitted statement reaches a terminal status because it finished,
//!    failed or was cancelled, never because this emulator's own execution bound
//!    cut it off while it was still doing legal work.** A data warehouse runs an
//!    expensive but legal statement to completion, so a statement this emulator
//!    stopped part-way through would report a failure the warehouse never had.
//!    What the bound is still for is a statement that would not finish at all,
//!    which it stops — and it is far longer than any patience `WaitTimeSeconds`
//!    admits, so a waiting caller never observes it.
//! 2. **the blocks a warehouse reports for its own storage
//!    (`STV_BLOCKLIST`) are one row per column of each table it holds**, with
//!    the number of values the column really carries — read out of the engine's
//!    own catalogue rather than invented — so a statement that reads that table
//!    reads the tables this database actually has.
//!
//! The emulator this suite builds is the shared harness, which holds its
//! executions to a bound a suite can watch
//! ([`super::emulator::TEST_ENGINE_DEADLINE`]) rather than to the service's own
//! default: the rules below are about WHICH bound ends a statement, so both are
//! asserted against the bound in force.

use serde_json::{json, Value};

use service_com_amazonaws_redshiftdata_redshiftdata::state::MAX_ENGINE_DEADLINE;

use super::emulator::{list, number, Emulator, TEST_ENGINE_DEADLINE, WAIT_SECONDS};

/// The warehouse and database every statement here runs on, which the harness
/// resolves without a control plane mounted beside it.
const CLUSTER: &str = "my-cluster";
const DATABASE: &str = "dev";
const DB_USER: &str = "awsuser";

/// RULE 1, the half a test can state exactly: the execution bound a client of
/// this emulator runs under outlasts every patience the model admits, so a
/// statement that is still working when a caller's wait expires is still
/// RUNNING rather than stopped — the bound can never be what a waiting caller
/// sees.
///
/// `WaitTimeSeconds` is modelled with the range 1..30 and documented "The
/// maximum value is 30 seconds", so that is the whole of what a caller can ask
/// for.
#[test]
fn the_execution_bound_outlasts_every_patience_the_model_admits() {
    let longest_wait = u64::try_from(WAIT_SECONDS).unwrap_or_default();
    assert!(
        MAX_ENGINE_DEADLINE.as_secs() > longest_wait,
        "expected the engine's own bound to outlast the longest modelled wait of {longest_wait}s, \
         got {MAX_ENGINE_DEADLINE:?}"
    );
}

/// RULE 1: a statement whose work takes real time runs to FINISHED, and its
/// result is the engine's own — the bound in force is not a budget on how much
/// work a statement may do.
///
/// The rows differ along the SHAPE of the work rather than along one recorded
/// statement: a self-join that multiplies a table by itself and counts, the
/// same with a predicate that matches nothing, a three-way self-join, and a
/// write built from one. Each is sized so the work it does is far more than one
/// row and still inside the bound the harness holds this emulator to, so what
/// the assertion proves is that the statement's own completion ended it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expensive_but_legal_statement_runs_to_finished() {
    let emulator = Emulator::new();
    seed(&emulator, "filler", 240).await;

    // (case, the statement, the single value it answers)
    let rows = [
        (
            "a self-join counted",
            "SELECT count(*) FROM filler a, filler b",
            "57600",
        ),
        (
            "a self-join with a predicate that matches nothing",
            "SELECT count(*) FROM filler a, filler b WHERE a.n + b.n = -1",
            "0",
        ),
        (
            "a three-way self-join, narrowed",
            "SELECT count(*) FROM filler a, filler b, filler c WHERE a.n = b.n AND b.n = c.n",
            "240",
        ),
        (
            "an aggregate over the product",
            "SELECT count(DISTINCT a.n + b.n) FROM filler a, filler b",
            "479",
        ),
    ];
    for (case, sql, answered) in rows {
        let response = run(&emulator, sql).await;
        assert_eq!(
            response["Status"], "FINISHED",
            "case {case}: expected the statement's own completion to end it, got {response}"
        );
        assert!(
            response["Error"].is_null(),
            "case {case}: expected no error on a statement that completed, got {response}"
        );
        let counted = first_value(&emulator, &response).await;
        assert_eq!(
            counted,
            answered.to_string(),
            "case {case}: expected the engine's own answer {answered}, got {counted}"
        );
    }

    // A WRITE built from the same work: the rows it inserts are the rows the
    // join produced, which a read back counts.
    let written = run(
        &emulator,
        "CREATE TABLE pairs AS SELECT a.n AS left_n, b.n AS right_n FROM filler a, filler b \
         WHERE a.n < 20 AND b.n < 20",
    )
    .await;
    assert_eq!(
        written["Status"], "FINISHED",
        "case a write built from a join: expected it to complete, got {written}"
    );
    let counted = first_value(&emulator, &run(&emulator, "SELECT count(*) FROM pairs").await).await;
    assert_eq!(
        counted, "361",
        "case a write built from a join: expected the join's own rows to be in the table, got \
         {counted}"
    );
}

/// RULE 1, the other half: a statement that would NOT finish is still stopped,
/// and the statement says so — the bound is a non-termination backstop and
/// removing it is not what makes the rule above true.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_statement_that_would_never_finish_is_still_stopped() {
    let emulator = Emulator::new();
    let response = run(
        &emulator,
        "WITH RECURSIVE forever(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM forever) \
         SELECT count(*) FROM forever",
    )
    .await;
    assert_eq!(
        response["Status"], "FAILED",
        "case a statement that cannot finish: expected the bound to stop it, got {response}"
    );
    let error = response["Error"].as_str().unwrap_or_default();
    assert!(
        error.to_ascii_lowercase().contains("interrupt"),
        "case a statement that cannot finish: expected the engine's own reason, got {response}"
    );
}

/// RULE 2: `STV_BLOCKLIST` reports one row per column of each table the
/// database HOLDS ROWS IN, each carrying the number of values that column
/// holds — read out of the engine's own catalogue, so a statement built from
/// that table is built from the tables this database actually has.
///
/// The rows differ along what the rule generalises over: how many columns a
/// table declares and how many rows it holds, including a table that holds none
/// and so occupies no block at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_blocks_a_warehouse_reports_are_the_columns_of_the_tables_it_holds() {
    let emulator = Emulator::new();
    // (case, the table, how many columns it declares, how many rows it holds,
    //  how many blocks it occupies)
    let rows = [
        ("one column, three rows", "blocks_one", 1, 3, 1),
        ("two columns, no rows", "blocks_empty", 2, 0, 0),
        ("three columns, five rows", "blocks_three", 3, 5, 3),
    ];
    for (case, table, columns, held, expected_blocks) in rows {
        let declared = (0..columns)
            .map(|index| format!("c{index} INTEGER"))
            .collect::<Vec<String>>()
            .join(", ");
        run(&emulator, &format!("CREATE TABLE {table} ({declared})")).await;
        for row in 0..held {
            let values = (0..columns)
                .map(|index| (row + index).to_string())
                .collect::<Vec<String>>()
                .join(", ");
            run(&emulator, &format!("INSERT INTO {table} VALUES ({values})")).await;
        }
        let counted = first_value(
            &emulator,
            &run(
                &emulator,
                &format!(
                    "SELECT count(*) FROM stv_blocklist b JOIN svv_table_info t \
                     ON b.tbl = t.table_id WHERE t.\"table\" = '{table}'"
                ),
            )
            .await,
        )
        .await;
        assert_eq!(
            counted,
            expected_blocks.to_string(),
            "case {case}: expected {expected_blocks} block(s) for {table}, got {counted}"
        );
        if expected_blocks == 0 {
            continue;
        }
        // The values each block carries are the table's own rows, and the
        // columns are numbered from zero.
        let values = first_value(
            &emulator,
            &run(
                &emulator,
                &format!(
                    "SELECT max(b.num_values) FROM stv_blocklist b JOIN svv_table_info t \
                     ON b.tbl = t.table_id WHERE t.\"table\" = '{table}'"
                ),
            )
            .await,
        )
        .await;
        assert_eq!(
            values,
            held.to_string(),
            "case {case}: expected a block of {table} to carry its {held} values, got {values}"
        );
        let highest = first_value(
            &emulator,
            &run(
                &emulator,
                &format!(
                    "SELECT max(b.col) FROM stv_blocklist b JOIN svv_table_info t \
                     ON b.tbl = t.table_id WHERE t.\"table\" = '{table}'"
                ),
            )
            .await,
        )
        .await;
        assert_eq!(
            highest,
            (columns - 1).to_string(),
            "case {case}: expected the columns of {table} numbered from zero, got {highest}"
        );
    }
}

/// Create the table `table` and fill it with `rows` consecutive integers,
/// through the same self-join a client builds a filler table with.
async fn seed(emulator: &Emulator, table: &str, rows: i64) {
    run(emulator, &format!("CREATE TABLE {table} (n INTEGER)")).await;
    run(
        emulator,
        &format!(
            "WITH RECURSIVE counter(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM counter \
             WHERE n < {rows}) INSERT INTO {table} SELECT n FROM counter"
        ),
    )
    .await;
    let counted = first_value(
        emulator,
        &run(emulator, &format!("SELECT count(*) FROM {table}")).await,
    )
    .await;
    assert_eq!(
        counted,
        rows.to_string(),
        "the seeded table must hold {rows} rows, got {counted}"
    );
}

/// Submit `sql` and answer the statement as its own terminal status reports it.
///
/// The submit asks for the longest wait the model admits, which is shorter than
/// the bound in force, so what it reads back is the EXECUTION's verdict.
async fn run(emulator: &Emulator, sql: &str) -> Value {
    let submitted = emulator
        .ok(
            "ExecuteStatement",
            &json!({
                "ClusterIdentifier": CLUSTER,
                "Database": DATABASE,
                "DbUser": DB_USER,
                "Sql": sql,
                "WaitTimeSeconds": WAIT_SECONDS,
            }),
        )
        .await;
    let id = submitted["Id"].as_str().unwrap_or_default().to_string();
    super::emulator::wait_for_terminal(emulator, sql, &id).await
}

/// The one value a single-row, single-column result carries.
async fn first_value(emulator: &Emulator, described: &Value) -> String {
    assert_eq!(
        described["Status"], "FINISHED",
        "a statement whose result is read must have finished: {described}"
    );
    let id = described["Id"].as_str().unwrap_or_default();
    let result = emulator
        .ok("GetStatementResult", &json!({ "Id": id }))
        .await;
    assert_eq!(
        number(&result, "TotalNumRows"),
        1,
        "expected one row of result, got {result}"
    );
    let records = list(&result, "Records");
    let first = records.first().cloned().unwrap_or(Value::Null);
    let fields = first.as_array().cloned().unwrap_or_default();
    let field = fields.first().cloned().unwrap_or(Value::Null);
    // A count is a whole number, which the Data API reports as `longValue`.
    field
        .get("longValue")
        .map(|value| value.to_string())
        .or_else(|| {
            field
                .get("stringValue")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{field}"))
}

/// The bound the harness holds this emulator to, named here so a reader of the
/// rows above can see the budget they are inside.
const _: () = assert!(
    TEST_ENGINE_DEADLINE.as_secs() > 0,
    "the harness must hold its executions to a positive bound"
);
