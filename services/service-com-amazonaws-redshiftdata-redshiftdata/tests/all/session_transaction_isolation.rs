// SPDX-License-Identifier: CC0-1.0

//! What a SESSION's open transaction holds, driven over the wire through the
//! shared harness in [`super::emulator`]: every call below is a real `POST /`
//! with `X-Amz-Target: RedshiftData.<Operation>`, so what is asserted is the
//! emulator's HTTP behaviour and not a handler's return value.
//!
//! The rule under test: a `SessionId` is a database CONNECTION, so statements
//! submitted on different sessions run on different connections. The work a
//! statement does inside a transaction one connection has open is therefore
//! visible to the statements of THAT connection and to no other, until the
//! transaction commits — a `COMMIT` makes all of it visible at once, and a
//! `ROLLBACK` makes none of it visible ever.
//!
//! It is asserted over a TABLE of transactions rather than one case: the rows
//! differ in what the transaction does (insert, several inserts, delete,
//! update), in what is read back (a row count, a sum), in how the transaction
//! ends (`COMMIT`, `ROLLBACK`) and in what the other connection IS (a
//! connection of its own, or a second session). The relation a transaction
//! creates is asserted too, and so is the complement that makes the isolation
//! real rather than a delay: work another connection commits while a
//! transaction is open is never lost.

use serde_json::{json, Value};

use super::emulator::{id_of, list, wait_for_terminal, Emulator, WAIT_SECONDS};

/// How long the sessions below are kept alive: longer than any case takes, so a
/// session is closed because a case closed it and never because the clock ran
/// out while a loaded machine was running the test.
const KEEP_ALIVE_SECONDS: i32 = 600;

/// The connection a submission that names no session is made with: one
/// documented authorization combination (a provisioned cluster as a database
/// user).
fn connection() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// Open a session by asking to keep one alive, and answer its `SessionId`.
async fn open_session(emulator: &Emulator, case: &str) -> String {
    let mut request = connection();
    request["Sql"] = json!("SELECT 1");
    request["SessionKeepAliveSeconds"] = json!(KEEP_ALIVE_SECONDS);
    request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
    let opened = emulator.ok("ExecuteStatement", &request).await;
    let described = wait_for_terminal(emulator, case, &id_of(&opened)).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: the statement that opens a session must run, got {described}"
    );
    opened["SessionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: a kept-alive submission reports its SessionId: {opened}")
        })
        .to_string()
}

/// Run `sql` in the session `session`, wait for it, and answer the record it
/// reached.
async fn in_session(emulator: &Emulator, case: &str, session: &str, sql: &str) -> Value {
    let submitted = emulator
        .execute_and_wait(&json!({"SessionId": session, "Sql": sql}))
        .await;
    wait_for_terminal(emulator, case, &id_of(&submitted)).await
}

/// Run `sql` in the session `session` and require that it FINISHED.
async fn finished_in_session(emulator: &Emulator, case: &str, session: &str, sql: &str) -> Value {
    let described = in_session(emulator, case, session, sql).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: [{sql}] must FINISH in its session, got {described}"
    );
    described
}

/// Run `sql` on a connection of its own — no `SessionId` — and require that it
/// FINISHED.
async fn finished_unsessioned(emulator: &Emulator, case: &str, sql: &str) -> Value {
    let mut request = connection();
    request["Sql"] = json!(sql);
    let submitted = emulator.execute_and_wait(&request).await;
    let described = wait_for_terminal(emulator, case, &id_of(&submitted)).await;
    assert_eq!(
        described["Status"], "FINISHED",
        "case {case}: [{sql}] must FINISH on a connection of its own, got {described}"
    );
    described
}

/// The single number the statement's result carries.
async fn single_number(emulator: &Emulator, case: &str, described: &Value) -> i64 {
    let fetched = emulator
        .ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    let records = list(&fetched, "Records");
    assert_eq!(
        records.len(),
        1,
        "case {case}: expected one row, got {fetched}"
    );
    records[0]
        .as_array()
        .and_then(|cells| cells.first())
        .and_then(|cell| cell["longValue"].as_i64())
        .unwrap_or_else(|| panic!("case {case}: expected one numeric field, got {fetched}"))
}

/// Read `sql` on the connection the case gives the OTHER reader, and answer the
/// number it read.
async fn read_elsewhere(
    emulator: &Emulator,
    case: &str,
    other: Option<&str>,
    sql: &str,
) -> i64 {
    let described = match other {
        Some(session) => finished_in_session(emulator, case, session, sql).await,
        None => finished_unsessioned(emulator, case, sql).await,
    };
    single_number(emulator, case, &described).await
}

/// Create the table both connections read and put two rows in it, on a
/// connection of its own, so every case starts from committed data.
async fn seed(emulator: &Emulator, case: &str) {
    finished_unsessioned(emulator, case, "CREATE TABLE iso (n INT)").await;
    finished_unsessioned(emulator, case, "INSERT INTO iso VALUES (10), (20)").await;
}

/// One row of the isolation table.
struct Row {
    label: &'static str,
    /// The statements the transaction runs after its `BEGIN`, each its own
    /// submission, so the transaction really spans several calls.
    writes: &'static [&'static str],
    /// What both connections read.
    read: &'static str,
    /// What that read answers while the transaction is open, on any connection
    /// but the transaction's own: the COMMITTED value.
    committed: i64,
    /// What it answers inside the transaction, which sees its own writes.
    uncommitted: i64,
    /// How the transaction ends.
    ending: &'static str,
    /// What the read answers on the other connection once it has ended.
    after: i64,
    /// Whether the other connection is a second SESSION rather than a
    /// connection of its own.
    other_session: bool,
}

const ROWS: &[Row] = &[
    Row {
        label: "an insert committed, read by a connection of its own",
        writes: &["INSERT INTO iso VALUES (30)"],
        read: "SELECT count(*) FROM iso",
        committed: 2,
        uncommitted: 3,
        ending: "COMMIT",
        after: 3,
        other_session: false,
    },
    Row {
        label: "an insert rolled back, read by a second session",
        writes: &["INSERT INTO iso VALUES (30)"],
        read: "SELECT count(*) FROM iso",
        committed: 2,
        uncommitted: 3,
        ending: "ROLLBACK",
        after: 2,
        other_session: true,
    },
    Row {
        label: "two inserts committed together",
        writes: &["INSERT INTO iso VALUES (30)", "INSERT INTO iso VALUES (40)"],
        read: "SELECT count(*) FROM iso",
        committed: 2,
        uncommitted: 4,
        ending: "COMMIT",
        after: 4,
        other_session: false,
    },
    Row {
        label: "a delete committed, read by a second session",
        writes: &["DELETE FROM iso"],
        read: "SELECT count(*) FROM iso",
        committed: 2,
        uncommitted: 0,
        ending: "COMMIT",
        after: 0,
        other_session: true,
    },
    Row {
        label: "an update rolled back, read as a sum",
        writes: &["UPDATE iso SET n = n + 5"],
        read: "SELECT sum(n) FROM iso",
        committed: 30,
        uncommitted: 40,
        ending: "ROLLBACK",
        after: 30,
        other_session: false,
    },
    Row {
        label: "an update committed, read as a sum by a second session",
        writes: &["UPDATE iso SET n = n * 2"],
        read: "SELECT sum(n) FROM iso",
        committed: 30,
        uncommitted: 60,
        ending: "COMMIT",
        after: 60,
        other_session: true,
    },
];

/// The work a transaction one session has open has done is read by that session
/// and by nobody else until it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_transaction_is_read_only_by_the_session_that_opened_it() {
    for row in ROWS {
        let case = row.label;
        // One emulator per row, so no row reads what another row left behind.
        let emulator = Emulator::new();
        seed(&emulator, case).await;

        let session = open_session(&emulator, case).await;
        let other = if row.other_session {
            let second = open_session(&emulator, case).await;
            assert_ne!(
                second, session,
                "case {case}: the second session must be a session of its own"
            );
            Some(second)
        } else {
            None
        };

        // The transaction is opened by one submission and written by the next,
        // so it is held by the SESSION and not by one statement.
        finished_in_session(&emulator, case, &session, "BEGIN").await;
        for write in row.writes {
            finished_in_session(&emulator, case, &session, write).await;
        }

        // Its own session reads what it wrote.
        let owner = finished_in_session(&emulator, case, &session, row.read).await;
        let inside = single_number(&emulator, case, &owner).await;
        assert_eq!(
            inside, row.uncommitted,
            "case {case}: the session that opened the transaction must read its own uncommitted \
             work: expected {}, got {inside}",
            row.uncommitted
        );

        // And no other connection does: it reads the committed data.
        let elsewhere = read_elsewhere(&emulator, case, other.as_deref(), row.read).await;
        assert_eq!(
            elsewhere, row.committed,
            "case {case}: another connection must read only committed data while the \
             transaction is open: expected {}, got {elsewhere}",
            row.committed
        );

        // The transaction ends, and what the other connection reads is what
        // that ending left committed.
        finished_in_session(&emulator, case, &session, row.ending).await;
        let after = read_elsewhere(&emulator, case, other.as_deref(), row.read).await;
        assert_eq!(
            after, row.after,
            "case {case}: after the {} the other connection must read {}, got {after}",
            row.ending, row.after
        );

        // And so does the session itself, which is on the same data as everyone
        // else once its transaction is over.
        let owner_after = finished_in_session(&emulator, case, &session, row.read).await;
        let inside_after = single_number(&emulator, case, &owner_after).await;
        assert_eq!(
            inside_after, row.after,
            "case {case}: after the {} its own session must read {}, got {inside_after}",
            row.ending, row.after
        );
    }
}

/// The same rule for a relation a transaction CREATES: another connection does
/// not reach it at all while the transaction is open, and reads it once the
/// transaction commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_table_created_inside_an_open_transaction_is_reachable_only_inside_it() {
    let case = "a table created inside a transaction";
    let emulator = Emulator::new();
    let session = open_session(&emulator, case).await;

    finished_in_session(&emulator, case, &session, "BEGIN").await;
    finished_in_session(
        &emulator,
        case,
        &session,
        "CREATE TABLE pending_rows (n INT)",
    )
    .await;
    finished_in_session(&emulator, case, &session, "INSERT INTO pending_rows VALUES (7)").await;

    // Its own session reads it.
    let owner = finished_in_session(&emulator, case, &session, "SELECT n FROM pending_rows").await;
    let value = single_number(&emulator, case, &owner).await;
    assert_eq!(
        value, 7,
        "case {case}: the session that created the table must read it, got {value}"
    );

    // Another connection does not reach the table at all: it is not in the
    // database that connection reads.
    let mut request = connection();
    request["Sql"] = json!("SELECT n FROM pending_rows");
    let submitted = emulator.execute_and_wait(&request).await;
    let elsewhere = wait_for_terminal(&emulator, case, &id_of(&submitted)).await;
    assert_eq!(
        elsewhere["Status"], "FAILED",
        "case {case}: a table created inside an open transaction is not another connection's to \
         read, got {elsewhere}"
    );
    assert!(
        elsewhere["Error"]
            .as_str()
            .is_some_and(|error| error.contains("pending_rows")),
        "case {case}: the failure must name the relation it could not resolve, got {elsewhere}"
    );

    // Once the transaction commits, every connection reads it.
    finished_in_session(&emulator, case, &session, "COMMIT").await;
    let committed = finished_unsessioned(&emulator, case, "SELECT n FROM pending_rows").await;
    let value = single_number(&emulator, case, &committed).await;
    assert_eq!(
        value, 7,
        "case {case}: once committed, the table is the database's: expected 7, got {value}"
    );
}

/// The complement that makes the isolation real rather than a delay: work
/// another connection COMMITS while a transaction is open is never undone by
/// that transaction. The transaction is refused at its `COMMIT`, says why, and
/// the committed work is still there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transaction_never_commits_over_work_committed_while_it_was_open() {
    let case = "a transaction overlapping a committed write";
    let emulator = Emulator::new();
    seed(&emulator, case).await;

    let session = open_session(&emulator, case).await;
    finished_in_session(&emulator, case, &session, "BEGIN").await;
    finished_in_session(&emulator, case, &session, "INSERT INTO iso VALUES (30)").await;

    // Another connection commits a row of its own while the transaction is
    // open.
    finished_unsessioned(&emulator, case, "INSERT INTO iso VALUES (40)").await;

    // The transaction cannot commit over it.
    let refused = in_session(&emulator, case, &session, "COMMIT").await;
    assert_eq!(
        refused["Status"], "FAILED",
        "case {case}: a transaction that would undo committed work must not commit, got {refused}"
    );
    assert!(
        refused["Error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "case {case}: a transaction that did not commit must report why, got {refused}"
    );

    // The committed row is there, and the transaction's row is not: three rows
    // (the two seeded and the committed one).
    let read = read_elsewhere(&emulator, case, None, "SELECT count(*) FROM iso").await;
    assert_eq!(
        read, 3,
        "case {case}: the committed work must survive and the rolled-back transaction's row must \
         not be there: expected 3 rows, got {read}"
    );
    let sum = read_elsewhere(&emulator, case, None, "SELECT sum(n) FROM iso").await;
    assert_eq!(
        sum, 70,
        "case {case}: the surviving rows are the seeded ones and the committed one: expected 70, \
         got {sum}"
    );
}
