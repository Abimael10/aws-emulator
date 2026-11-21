// SPDX-License-Identifier: CC0-1.0

//! A statement held back by ANOTHER connection's table lock, and the whole
//! `CancelStatement` lifecycle that becomes reachable behind it.
//!
//! Every call below goes over the wire through the [`Warehouses`] harness — a
//! real `POST /` per operation, with both Redshift control planes mounted beside
//! the Data API on one engine — so the warehouse a statement runs on is the
//! owning service's resource and what is asserted is the emulator's HTTP
//! behaviour.
//!
//! The RULE asserted here: a submitted statement whose tables need a lock that
//! CONFLICTS with one another connection holds inside an open transaction does
//! not run — it keeps reporting a non-terminal status (`SUBMITTED`, `PICKED` or
//! `STARTED`, every one a modelled `StatusString`) for as long as that
//! transaction stays open, and the holder's lock survives from one call to the
//! next instead of being released with the statement that took it.
//! `CancelStatement` on a statement that is still running answers `Status` true
//! and the statement then reports `ABORTED`; `CancelStatement` on a statement
//! that has already STOPPED is refused with `ValidationException` at HTTP 400,
//! naming the terminal state the record is in and the `Id` the request named,
//! and changes nothing about it.
//!
//! The rows span what the rule generalises over rather than the one recorded
//! case: WHICH kind of warehouse the statements run on (a serverless workgroup,
//! a provisioned cluster), WHICH mode the holder takes (named explicitly, named
//! not at all, and weaker modes that conflict with a write rather than a read),
//! HOW MANY tables one `LOCK` names, and WHAT the blocked statement does — a
//! read, an aggregate, an `INSERT`, an `UPDATE` — since the mode a statement
//! implies is what decides whether it conflicts at all. The refusal of a cancel
//! is asserted on both terminal states a row produces, `ABORTED` and `FINISHED`,
//! because the state in that sentence is the record's and not a constant.

use serde_json::{json, Value};
use std::time::Instant;

use crate::emulator::{error_code, flag, id_of, Warehouses, RUNNING_STATUSES, WAIT_SECONDS};
use crate::statement_status_change_events::{wait_for_terminal, POLL_BUDGET, POLL_INTERVAL};

/// The warehouses the rows run on, and the database both are created with.
const CLUSTER: &str = "lock-contention-cluster";
const WORKGROUP: &str = "lock-contention-workgroup";
const DATABASE: &str = "dev";
const DB_USER: &str = "awsuser";

/// The two tables every row seeds: the one a holder locks, and a second one, so
/// a row can lock both and contend on the one it did not name first.
const FIRST_TABLE: &str = "locked_rows";
const SECOND_TABLE: &str = "other_rows";

/// How long the holder session asks to be kept alive: longer than any row takes,
/// so a holder's transaction is open because the row left it open and never
/// because the clock closed its session while a loaded machine ran the test.
const HOLDER_KEEP_ALIVE_SECONDS: i32 = 3_600;

/// Which kind of warehouse a row's statements run on.
#[derive(Debug, Clone, Copy)]
enum Warehouse {
    Cluster,
    Workgroup,
}

/// One row of the contention table.
struct Row {
    case: &'static str,
    warehouse: Warehouse,
    /// The tables the holder's `LOCK` names, as that statement spells the list.
    locked: &'static str,
    /// The mode clause the holder names, or `None` for a `LOCK` that names none
    /// — which is the `ACCESS EXCLUSIVE` Amazon Redshift documents `LOCK` as
    /// taking.
    held: Option<&'static str>,
    /// What the OTHER connection submits against a locked table.
    contending: &'static str,
}

/// The rows: every one of them submits a statement that conflicts with the lock
/// its row's holder is holding.
const ROWS: &[Row] = &[
    Row {
        case: "a workgroup reader against an explicit ACCESS EXCLUSIVE lock",
        warehouse: Warehouse::Workgroup,
        locked: FIRST_TABLE,
        held: Some("ACCESS EXCLUSIVE"),
        contending: "SELECT * FROM locked_rows",
    },
    Row {
        case: "a workgroup aggregate against a LOCK that names no mode",
        warehouse: Warehouse::Workgroup,
        locked: FIRST_TABLE,
        held: None,
        contending: "SELECT count(*) FROM locked_rows",
    },
    Row {
        case: "a cluster INSERT against an EXCLUSIVE lock",
        warehouse: Warehouse::Cluster,
        locked: FIRST_TABLE,
        held: Some("EXCLUSIVE"),
        contending: "INSERT INTO locked_rows VALUES (7)",
    },
    Row {
        case: "a cluster UPDATE against a SHARE lock",
        warehouse: Warehouse::Cluster,
        locked: FIRST_TABLE,
        held: Some("SHARE"),
        contending: "UPDATE locked_rows SET id = 9 WHERE id = 1",
    },
    Row {
        case: "a workgroup reader of the second table of a two-table LOCK",
        warehouse: Warehouse::Workgroup,
        locked: "locked_rows, other_rows",
        held: Some("ACCESS EXCLUSIVE"),
        contending: "SELECT * FROM other_rows",
    },
];

/// The connection a row's submissions are made with: a serverless workgroup
/// names no database user ("the database user name is derived from the IAM
/// identity"), a provisioned cluster is connected to as a database user.
fn connection(row: &Row) -> Value {
    match row.warehouse {
        Warehouse::Cluster => json!({
            "ClusterIdentifier": CLUSTER,
            "Database": DATABASE,
            "DbUser": DB_USER,
        }),
        Warehouse::Workgroup => json!({
            "WorkgroupName": WORKGROUP,
            "Database": DATABASE,
        }),
    }
}

/// Create the warehouse a row runs on, with the database every call names.
async fn given_warehouse(emulator: &Warehouses, row: &Row) {
    match row.warehouse {
        Warehouse::Cluster => {
            emulator
                .create_cluster_with_database(CLUSTER, Some(DATABASE))
                .await;
        }
        Warehouse::Workgroup => {
            emulator
                .create_workgroup_with_database(WORKGROUP, Some(DATABASE))
                .await;
        }
    }
}

/// Submit `sql` on a connection of its own, wait for it inside the submit, and
/// answer the record it reached.
async fn run(emulator: &Warehouses, row: &Row, sql: &str) -> Value {
    let mut request = connection(row);
    request["Sql"] = json!(sql);
    request["WaitTimeSeconds"] = json!(WAIT_SECONDS);
    let submitted = emulator.data_ok("ExecuteStatement", &request).await;
    wait_for_terminal(emulator, row.case, &id_of(&submitted)).await
}

/// Seed both tables of a row, each with one row in it, so a blocked read has
/// something to read and a blocked write something to write.
async fn seed(emulator: &Warehouses, row: &Row) {
    for sql in [
        format!("CREATE TABLE {FIRST_TABLE} (id INT)"),
        format!("INSERT INTO {FIRST_TABLE} VALUES (1)"),
        format!("CREATE TABLE {SECOND_TABLE} (id INT)"),
        format!("INSERT INTO {SECOND_TABLE} VALUES (2)"),
    ] {
        let described = run(emulator, row, &sql).await;
        assert_eq!(
            described["Status"], "FINISHED",
            "case {:?}: seeding [{sql}] must FINISH, got {described}",
            row.case
        );
    }
}

/// Poll `id` until the service reports it as being worked on — chosen to be run,
/// or running — and answer the record that reported it.
///
/// The CONDITION is what this waits for, looked at again until it holds: a
/// statement becomes observable as running as soon as its execution is
/// scheduled, however quickly or slowly the host gets to it.
async fn wait_until_running(emulator: &Warehouses, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let described = emulator
            .data_ok("DescribeStatement", &json!({"Id": id}))
            .await;
        let status = described["Status"].as_str().unwrap_or_default().to_string();
        if status == "PICKED" || status == "STARTED" {
            return described;
        }
        assert!(
            RUNNING_STATUSES.contains(&status.as_str()),
            "case {case:?}: a statement waiting for a lock must still be in flight, got \
             {described}"
        );
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: {id} never became observable as running: {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that cancelling the statement `id`, which has already stopped in
/// `state`, is refused as a client-input failure that names both and leaves the
/// record exactly as it was.
async fn assert_cancel_refused(emulator: &Warehouses, case: &str, id: &str, state: &str) {
    let (status, refused) = emulator
        .data("CancelStatement", &json!({"Id": id}))
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: cancelling a statement already in {state} is a client-input failure, \
         got {status} {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case {case:?}: expected ValidationException for a {state} statement, got {refused}"
    );
    assert_eq!(
        refused["Message"],
        json!(format!(
            "Could not cancel a query that is already in {state} state with ID: {id}"
        )),
        "case {case:?}: expected the service's own sentence for a {state} statement, got \
         {refused}"
    );
    let described = emulator
        .data_ok("DescribeStatement", &json!({"Id": id}))
        .await;
    assert_eq!(
        described["Status"], state,
        "case {case:?}: a refused cancel must leave the statement in {state}, got {described}"
    );
}

/// A statement that needs a conflicting lock is never reported as having run: it
/// stays non-terminal while the holder's transaction is open, a cancel stops it
/// and lands it ABORTED, and a second cancel of that stopped statement is
/// refused without changing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_statement_blocked_by_another_connections_lock_is_cancellable_and_then_refused() {
    for row in ROWS {
        // One emulator per row, so no row meets a lock or a table another row
        // left behind.
        let emulator = Warehouses::new();
        given_warehouse(&emulator, row).await;
        seed(&emulator, row).await;

        // The holder opens a transaction and takes its lock in one submission,
        // on a session it keeps alive: the transaction stays open after the
        // statement reports, which is what holds the lock against the other
        // connections.
        let mode = row
            .held
            .map(|mode| format!(" IN {mode} MODE"))
            .unwrap_or_default();
        let mut holding = connection(row);
        holding["Sql"] = json!(format!("BEGIN; LOCK TABLE {}{mode};", row.locked));
        holding["SessionKeepAliveSeconds"] = json!(HOLDER_KEEP_ALIVE_SECONDS);
        holding["WaitTimeSeconds"] = json!(WAIT_SECONDS);
        let opened = emulator.data_ok("ExecuteStatement", &holding).await;
        let holder_id = id_of(&opened);
        let session = opened["SessionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a submission that asked to keep a session alive reports its \
                     SessionId: {opened}",
                    row.case
                )
            })
            .to_string();
        let holder = wait_for_terminal(&emulator, row.case, &holder_id).await;
        assert_eq!(
            holder["Status"], "FINISHED",
            "case {:?}: the statement that takes the lock must run, got {holder}",
            row.case
        );

        // The contending statement is submitted on a NEW default connection and
        // with no wait at all, which is how a client discovers it is blocked:
        // the service is asked what it has reached rather than asked to wait for
        // it.
        let mut contending = connection(row);
        contending["Sql"] = json!(row.contending);
        let submitted = emulator.data_ok("ExecuteStatement", &contending).await;
        assert_eq!(
            submitted["Status"], "SUBMITTED",
            "case {:?}: a submission nobody waited for is answered as accepted, got {submitted}",
            row.case
        );
        let blocked_id = id_of(&submitted);
        // Observable as running: [`wait_until_running`] is what asserts the
        // statuses it walks through, so what is read off the record here is
        // only what it has not run.
        let running = wait_until_running(&emulator, row.case, &blocked_id).await;
        assert!(
            !flag(&running, "HasResultSet"),
            "case {:?}: a statement that has not run has no result set, got {running}",
            row.case
        );

        // The holder's lock is held BETWEEN calls, not released with the
        // statement that took it: the holder runs another statement in the same
        // session and the blocked one is still waiting afterwards.
        let since = emulator
            .data_ok(
                "ExecuteStatement",
                &json!({
                    "SessionId": &session,
                    "Sql": "SELECT 1",
                    "WaitTimeSeconds": WAIT_SECONDS,
                }),
            )
            .await;
        let since = wait_for_terminal(&emulator, row.case, &id_of(&since)).await;
        assert_eq!(
            since["Status"], "FINISHED",
            "case {:?}: the holder's session must keep running statements while the other \
             connection waits, got {since}",
            row.case
        );
        let still = emulator
            .data_ok("DescribeStatement", &json!({"Id": &blocked_id}))
            .await;
        assert!(
            RUNNING_STATUSES.contains(&still["Status"].as_str().unwrap_or_default()),
            "case {:?}: the blocked statement must still be waiting once the holder has run \
             another statement in the same open transaction, got {still}",
            row.case
        );

        // Cancelling the running statement succeeds, and it lands ABORTED.
        let cancelled = emulator
            .data_ok("CancelStatement", &json!({"Id": &blocked_id}))
            .await;
        assert_eq!(
            cancelled["Status"],
            json!(true),
            "case {:?}: cancelling a running statement must report that it succeeded, got \
             {cancelled}",
            row.case
        );
        let stopped = wait_for_terminal(&emulator, row.case, &blocked_id).await;
        assert_eq!(
            stopped["Status"], "ABORTED",
            "case {:?}: a cancelled statement lands ABORTED, got {stopped}",
            row.case
        );

        // And cancelling a statement that has already STOPPED is refused,
        // naming the state the record really is in: the cancelled one is
        // ABORTED, the holder's own statement FINISHED.
        assert_cancel_refused(&emulator, row.case, &blocked_id, "ABORTED").await;
        assert_cancel_refused(&emulator, row.case, &holder_id, "FINISHED").await;
    }
}

/// The complement, which is what makes the wait a LOCK and not a stall: the
/// blocked statement runs as soon as the holder's transaction ends, and reads
/// what the table holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_blocked_statement_runs_once_the_holding_transaction_ends() {
    // The two ways a holder ends its transaction, each released by a statement
    // of the holder's own session.
    for ending in ["COMMIT", "ROLLBACK"] {
        let row = &Row {
            case: "a holder that ends its transaction",
            warehouse: Warehouse::Workgroup,
            locked: FIRST_TABLE,
            held: Some("ACCESS EXCLUSIVE"),
            contending: "SELECT id FROM locked_rows",
        };
        let case = format!("{} with {ending}", row.case);
        let emulator = Warehouses::new();
        given_warehouse(&emulator, row).await;
        seed(&emulator, row).await;

        let mut holding = connection(row);
        holding["Sql"] = json!(format!("BEGIN; LOCK TABLE {};", row.locked));
        holding["SessionKeepAliveSeconds"] = json!(HOLDER_KEEP_ALIVE_SECONDS);
        holding["WaitTimeSeconds"] = json!(WAIT_SECONDS);
        let opened = emulator.data_ok("ExecuteStatement", &holding).await;
        let session = opened["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SessionId in {opened}"))
            .to_string();
        let holder = wait_for_terminal(&emulator, &case, &id_of(&opened)).await;
        assert_eq!(
            holder["Status"], "FINISHED",
            "case {case:?}: the statement that takes the lock must run, got {holder}"
        );

        let mut contending = connection(row);
        contending["Sql"] = json!(row.contending);
        let submitted = emulator.data_ok("ExecuteStatement", &contending).await;
        let blocked_id = id_of(&submitted);
        // Waiting for the lock: the poll is what asserts the statuses the
        // statement is allowed to report while it waits.
        let _running = wait_until_running(&emulator, &case, &blocked_id).await;

        // The holder ends its transaction, which releases what it held.
        let ended = emulator
            .data_ok(
                "ExecuteStatement",
                &json!({
                    "SessionId": &session,
                    "Sql": ending,
                    "WaitTimeSeconds": WAIT_SECONDS,
                }),
            )
            .await;
        let ended = wait_for_terminal(&emulator, &case, &id_of(&ended)).await;
        assert_eq!(
            ended["Status"], "FINISHED",
            "case {case:?}: the holder's {ending} must run while the other statement waits, got \
             {ended}"
        );

        let described = wait_for_terminal(&emulator, &case, &blocked_id).await;
        assert_eq!(
            described["Status"], "FINISHED",
            "case {case:?}: the statement must run once the lock is released, got {described}"
        );
        let fetched = emulator
            .data_ok("GetStatementResult", &json!({"Id": &blocked_id}))
            .await;
        let records = fetched["Records"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: the released statement must read the one row the table holds, got \
             {fetched}"
        );
        assert_eq!(
            records[0][0]["longValue"],
            json!(1),
            "case {case:?}: expected the seeded row, got {fetched}"
        );
    }
}
