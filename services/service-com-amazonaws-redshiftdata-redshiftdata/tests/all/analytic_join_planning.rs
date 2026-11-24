// SPDX-License-Identifier: CC0-1.0

//! An ANALYTIC JOIN of two relations, driven over the wire through the shared
//! harness in [`super::emulator`]: every call below is a real `POST /` with
//! `X-Amz-Target: RedshiftData.<Operation>`, so what is asserted is the
//! emulator's HTTP behaviour and not a handler's return value.
//!
//! The rule under test: a statement the Data API ACCEPTS always reaches a
//! terminal status, and an equality that constrains two joined relations is what
//! bounds the work that takes — a data warehouse keys the join on that equality
//! instead of comparing every pair of rows, so a join of two relations of tens
//! of thousands of rows is seconds of work and not hours of it. The statements
//! below join a relation of [`JOINED_ROWS`] rows to itself, which is nine orders
//! of magnitude more pairs than rows: a server that enumerated the pairs could
//! not answer any of them inside the engine's own execution bound, so each case
//! asserts BOTH that the statement finished and that it answered the rows the
//! predicate really selects.
//!
//! It is asserted over a TABLE of predicates rather than one, because what the
//! rule generalises over is the FORM the equality is written in: which side each
//! relation appears on, whether the terms are added or subtracted, whether a
//! constant sits with them, and whether the join is written as a comma or as an
//! explicit `JOIN ... ON`. The last two rows are the counter-cases: a predicate
//! over values whose arithmetic is not exact, and one that constrains three
//! relations, which must answer the rows they select whatever the engine does
//! with them.

use serde_json::{json, Value};

use super::emulator::{id_of, list, wait_for_terminal, Emulator, WAIT_SECONDS};

/// The connection every submission below is made with: one documented
/// authorization combination (a provisioned cluster as a database user).
fn connection() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// How many rows the joined relation holds. The recorded client loads a filler
/// table of exactly this many rows and then counts a self-join of it, which is
/// six and a half billion pairs of rows.
const JOINED_ROWS: i64 = 80_000;

/// Run `sql`, wait for it, and require that it FINISHED.
async fn finished(emulator: &Emulator, case: &str, sql: &str) -> Value {
    let mut request = connection();
    request["Sql"] = json!(sql);
    request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
    let submitted = emulator.ok("ExecuteStatement", &request).await;
    let described = wait_for_terminal(emulator, case, &id_of(&submitted)).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: [{sql}] must reach FINISHED, got {described}"
    );
    assert!(
        described["Duration"].as_i64().unwrap_or_default() > 0,
        "case {case}: a statement that ran reports the time it took, got {described}"
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

/// One joined statement: how its equality constrains the two relations, and how
/// many pairs of rows that equality really selects out of `filler`, which holds
/// the integers 1 to [`JOINED_ROWS`].
struct Joined {
    /// The case's name.
    case: &'static str,
    /// The statement, with `{rows}` standing for [`JOINED_ROWS`].
    sql: &'static str,
    /// The pairs the predicate selects, as a function of how many rows the
    /// relation holds.
    selected: fn(i64) -> i64,
}

/// The joins asserted over. Every one of them is the same cross product
/// constrained by an equality between the two relations; what differs is the
/// form that equality is written in.
const JOINS: &[Joined] = &[
    Joined {
        // The recorded statement: both columns added on one side, a negative
        // constant on the other. No pair of two positive integers sums to -1.
        case: "both columns summed, against a negative constant",
        sql: "SELECT count(*) FROM filler a, filler b WHERE a.n + b.n = -1",
        selected: |_rows| 0,
    },
    Joined {
        // Pairs summing to one more than the highest value: for every a there
        // is exactly one b, so the predicate selects one pair per row.
        case: "both columns summed, against a reachable constant",
        sql: "SELECT count(*) FROM filler a, filler b WHERE a.n + b.n = {rows} + 1",
        selected: |rows| rows,
    },
    Joined {
        // The same constraint with the constant brought over to the left.
        case: "both columns and the constant on one side",
        sql: "SELECT count(*) FROM filler a, filler b WHERE b.n + a.n - 2 = 0",
        selected: |_rows| 1,
    },
    Joined {
        // A difference rather than a sum: the pairs on the diagonal.
        case: "one column subtracted from the other",
        sql: "SELECT count(*) FROM filler a, filler b WHERE a.n - b.n = 0",
        selected: |rows| rows,
    },
    Joined {
        // One relation on each side already, which is the form an engine can
        // key on as written: it must keep answering the same rows.
        case: "one column on each side",
        sql: "SELECT count(*) FROM filler a, filler b WHERE a.n = b.n",
        selected: |rows| rows,
    },
    Joined {
        // The same mixed equality as a join condition rather than a filter.
        case: "a mixed equality as an explicit join condition",
        sql: "SELECT count(*) FROM filler a JOIN filler b ON a.n + b.n = {rows} + 1",
        selected: |rows| rows,
    },
    Joined {
        // A mixed equality with a term on each side, and both relations on
        // both: a + 1 = r + 1 - b selects the pairs summing to the row count.
        case: "a mixed equality with terms on both sides",
        sql: "SELECT count(*) FROM filler a, filler b WHERE a.n + 1 = {rows} + 1 - b.n",
        selected: |rows| rows - 1,
    },
];

/// Every form of the equality finishes, and answers the pairs it selects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_equality_between_two_joined_relations_bounds_the_join() {
    let emulator = Emulator::new();
    seed(&emulator, "filler", JOINED_ROWS).await;
    // The relation really holds the rows the cases below are computed against.
    let held = number(
        &emulator,
        "the joined relation",
        "SELECT count(*) FROM filler",
    )
    .await;
    assert_eq!(
        held, JOINED_ROWS,
        "the joined relation must hold {JOINED_ROWS} rows, got {held}"
    );

    for joined in JOINS {
        let sql = joined.sql.replace("{rows}", &JOINED_ROWS.to_string());
        let counted = number(&emulator, joined.case, &sql).await;
        let expected = (joined.selected)(JOINED_ROWS);
        assert_eq!(
            counted, expected,
            "case {}: [{sql}] selects {expected} pairs of the {JOINED_ROWS} rows, got {counted}",
            joined.case
        );
    }
}

/// A predicate whose arithmetic is NOT exact over the values it reads is
/// answered exactly as the caller wrote it, so the rows it selects are the rows
/// those values really satisfy.
///
/// Asserted on a small relation, because the point is which rows the answer
/// carries rather than how the engine reached them: the two formulations below
/// are the same algebra and select DIFFERENT pairs of these values, so a server
/// that answered one of them with the other would answer rows the caller did not
/// ask for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_predicate_over_inexact_values_selects_the_rows_it_selects() {
    let case = "floating-point values";
    /// The values the relation holds, each of which is the double the engine
    /// and this test both read its literal as.
    const MEASURED: [f64; 6] = [0.1, 0.2, 0.3, 0.7, 0.8, 0.9];
    let emulator = Emulator::new();
    finished(&emulator, case, "CREATE TABLE measured (x DOUBLE PRECISION)").await;
    finished(
        &emulator,
        case,
        "INSERT INTO measured VALUES (0.1), (0.2), (0.3), (0.7), (0.8), (0.9)",
    )
    .await;

    // Each formulation is counted here over the same doubles, so what is
    // asserted is that the engine evaluated the predicate the caller wrote.
    let pairs = |select: fn(f64, f64) -> bool| -> i64 {
        MEASURED
            .iter()
            .flat_map(|a| MEASURED.iter().map(move |b| (*a, *b)))
            .filter(|(a, b)| select(*a, *b))
            .count() as i64
    };
    let rows = [
        (
            "the sum of the two columns",
            "SELECT count(*) FROM measured a, measured b WHERE a.x + b.x = 1.0",
            pairs(|a, b| a + b == 1.0),
        ),
        (
            "the same constraint with one column moved across",
            "SELECT count(*) FROM measured a, measured b WHERE a.x = 1.0 - b.x",
            pairs(|a, b| a == 1.0 - b),
        ),
    ];
    for (label, sql, expected) in rows {
        let counted = number(&emulator, case, sql).await;
        assert_eq!(
            counted, expected,
            "case {case}, {label}: [{sql}] selects {expected} of the {} pairs of {MEASURED:?}, \
             got {counted}",
            MEASURED.len() * MEASURED.len()
        );
    }
}

/// An equality that constrains THREE relations is answered as written: no
/// arrangement of its terms leaves one relation on each side, and the rows it
/// selects are the rows it selects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_equality_over_three_relations_answers_the_rows_it_selects() {
    let case = "three relations";
    let emulator = Emulator::new();
    seed(&emulator, "triple", 40).await;

    // Triples of 1..=40 summing to 6: (1,1,4), (1,4,1), (4,1,1), (1,2,3) and
    // its five other orderings, (2,2,2) — ten in all.
    let counted = number(
        &emulator,
        case,
        "SELECT count(*) FROM triple a, triple b, triple c WHERE a.n + b.n + c.n = 6",
    )
    .await;
    assert_eq!(
        counted, 10,
        "case {case}: ten ordered triples of positive integers sum to six, got {counted}"
    );
}

/// An equality spelled as a LONGER arithmetic chain than the planning reads is
/// answered as written, and answers the rows it selects.
///
/// The chain's length is the caller's to choose, so each case below is longer
/// than the one before: the last two are past the terms the planning reads and
/// past the depth it descends, and what the rule requires of those is that the
/// statement still answers the pairs its predicate selects. A chain longer than
/// the engine's own expression-depth limit is the engine's to refuse, so the
/// cases stay inside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_equality_spelled_as_a_long_chain_answers_the_rows_it_selects() {
    let case = "a long arithmetic chain";
    let emulator = Emulator::new();
    // A small relation, because the point is which rows the answer carries: the
    // pairs of 1..=20 summing to two are just (1, 1).
    let held = 20_i64;
    seed(&emulator, "small", held).await;

    for added in [2_usize, 8, 40, 70] {
        let sql = format!(
            "SELECT count(*) FROM small a, small b WHERE a.n{} + b.n = {}",
            " + 1".repeat(added),
            added + 2
        );
        let counted = number(&emulator, case, &sql).await;
        assert_eq!(
            counted, 1,
            "case {case} of {added} added terms: one pair of the {held} rows sums to two, got \
             {counted}"
        );
    }
}

/// Create `table` with one integer column and fill it with the integers 1 to
/// `rows`, the way a client builds a filler table.
async fn seed(emulator: &Emulator, table: &str, rows: i64) {
    finished(
        emulator,
        "seeding",
        &format!("CREATE TABLE {table} (n INT)"),
    )
    .await;
    finished(
        emulator,
        "seeding",
        &format!(
            "WITH RECURSIVE counter(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM counter \
             WHERE n < {rows}) INSERT INTO {table} SELECT n FROM counter"
        ),
    )
    .await;
}
