// SPDX-License-Identifier: CC0-1.0

//! A status change a submission asked to be told about stays OWED until the event
//! bus TAKES it.
//!
//! The RULE asserted here: `WithEvent` is "a value that indicates whether to send
//! an event to the Amazon EventBridge event bus after the SQL statement runs"
//! (`smithy_query(query="doc",shape="ExecuteStatementInput",member="WithEvent")`),
//! so a submission that asked for one is owed one. The sibling suite
//! `statement_status_change_events` pins what the bus is handed and that a rule
//! matching it delivers it to its target; THIS suite pins the other half — what
//! becomes of that announcement when the publication does NOT succeed. A status
//! change the bus did not take is still queued for publication afterwards, so a
//! later drain offers it again; it is never discarded, because a discarded one
//! makes a submission that asked for an event one that never gets it and never
//! can, and leaves it indistinguishable from a submission that asked for nothing.
//!
//! The emulator these rows run on mounts NO event bus, which is the configuration
//! in which a publication cannot succeed at all: every `PutEvents` this service
//! makes is one nothing answers. What is read back is this service's own count of
//! the status changes still waiting to be published
//! (`pending_statement_event_count`), after every row, so a row that was dropped
//! rather than kept is seen at the row that dropped it AND a later row's drain
//! that discarded an earlier row's announcement is seen too.
//!
//! The rows span what the rule generalises over: whether the submission asked for
//! an event (absent, false, true), WHICH submit operation made it (one statement,
//! a batch), WHICH terminal status the statement reaches (a statement that runs
//! FINISHES, one the engine refuses FAILS), and WHICH kind of warehouse it ran on
//! (a provisioned cluster, a serverless workgroup). The third terminal status — a
//! statement a caller STOPS — drives the second test below, because it is queued by
//! the cancel rather than by the execution.

use std::time::Instant;

use serde_json::{json, Value};

use crate::emulator::Warehouses;
use crate::statement_status_change_events::{wait_for_terminal, POLL_BUDGET, POLL_INTERVAL};

/// The warehouses the rows run on, and the database both are created with.
const CLUSTER: &str = "owed-event-cluster";
const WORKGROUP: &str = "owed-event-workgroup";
const DATABASE: &str = "devdb";
const DB_USER: &str = "adminuser";

/// A statement the engine runs to completion, and one it refuses.
const FINISHING_SQL: &str = "SELECT 1";
const FAILING_SQL: &str = "SELECT * FROM a_table_no_statement_ever_created";

/// Which kind of warehouse a row's statement runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Warehouse {
    /// A provisioned cluster, named by its `ClusterIdentifier`.
    Cluster,
    /// A serverless workgroup, named by its `WorkgroupName`.
    Workgroup,
}

/// One row: the submission made, and whether its status change is owed.
struct Row {
    case: &'static str,
    /// The submit operation: `ExecuteStatement` or `BatchExecuteStatement`.
    operation: &'static str,
    /// The statements submitted: exactly one for `ExecuteStatement`.
    sqls: &'static [&'static str],
    /// The `WithEvent` the request carries, or `None` for a request that omits
    /// the member altogether.
    with_event: Option<bool>,
    warehouse: Warehouse,
    /// Whether this submission asked to be told how its statement ended.
    announced: bool,
    /// The terminal status the statement must reach.
    status: &'static str,
}

/// The table.
const ROWS: &[Row] = &[
    Row {
        case: "a statement that omits WithEvent",
        operation: "ExecuteStatement",
        sqls: &[FINISHING_SQL],
        with_event: None,
        warehouse: Warehouse::Cluster,
        announced: false,
        status: "FINISHED",
    },
    Row {
        case: "a statement that asked for no event",
        operation: "ExecuteStatement",
        sqls: &[FINISHING_SQL],
        with_event: Some(false),
        warehouse: Warehouse::Cluster,
        announced: false,
        status: "FINISHED",
    },
    Row {
        case: "a statement that ran",
        operation: "ExecuteStatement",
        sqls: &[FINISHING_SQL],
        with_event: Some(true),
        warehouse: Warehouse::Cluster,
        announced: true,
        status: "FINISHED",
    },
    Row {
        case: "a statement the engine refused",
        operation: "ExecuteStatement",
        sqls: &[FAILING_SQL],
        with_event: Some(true),
        warehouse: Warehouse::Cluster,
        announced: true,
        status: "FAILED",
    },
    Row {
        case: "a batch that ran",
        operation: "BatchExecuteStatement",
        sqls: &[FINISHING_SQL, "SELECT 2"],
        with_event: Some(true),
        warehouse: Warehouse::Cluster,
        announced: true,
        status: "FINISHED",
    },
    Row {
        case: "a statement on a serverless workgroup",
        operation: "ExecuteStatement",
        sqls: &[FINISHING_SQL],
        with_event: Some(true),
        warehouse: Warehouse::Workgroup,
        announced: true,
        status: "FINISHED",
    },
    Row {
        case: "a batch that asked for no event, after announced rows",
        operation: "BatchExecuteStatement",
        sqls: &[FINISHING_SQL, "SELECT 2"],
        with_event: Some(false),
        warehouse: Warehouse::Cluster,
        announced: false,
        status: "FINISHED",
    },
];

/// The submit request one row makes.
fn submit_request(row: &Row) -> Value {
    let mut request = match row.warehouse {
        Warehouse::Cluster => json!({
            "ClusterIdentifier": CLUSTER,
            "Database": DATABASE,
            "DbUser": DB_USER,
        }),
        // A serverless workgroup: "the database user name is derived from the IAM
        // identity", so the request names no user.
        Warehouse::Workgroup => json!({
            "WorkgroupName": WORKGROUP,
            "Database": DATABASE,
        }),
    };
    if row.operation == "BatchExecuteStatement" {
        request["Sqls"] = json!(row.sqls);
    } else {
        request["Sql"] = json!(row.sqls[0]);
    }
    if let Some(with_event) = row.with_event {
        request["WithEvent"] = json!(with_event);
    }
    request
}

/// Look at this service's own count of the status changes still owed until it is
/// `expected`, and answer what it last read.
///
/// Read by looking again rather than once: a drain takes a status change out
/// before it offers it to the bus and gives it back when the bus does not take
/// it, so between those two instants nothing is owed yet — the settled count is
/// what the rule is about. The deadline is generous ([`POLL_BUDGET`]) and the
/// count is what is asserted, so a count that never settles fails on the count it
/// was stuck at and never on how fast the host ran.
async fn owed_count(emulator: &Warehouses, expected: usize) -> usize {
    let started = Instant::now();
    loop {
        let queued = emulator.store().lock().pending_statement_event_count();
        if queued == expected || started.elapsed() >= POLL_BUDGET {
            return queued;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a status change a submission asked for and the bus did not take is still
/// owed — queued for publication, and left there by every later drain — while a
/// submission that asked for nothing is owed nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_status_change_the_bus_did_not_take_stays_queued() {
    let emulator = Warehouses::with_no_event_bus();
    emulator
        .create_cluster_with_database(CLUSTER, Some(DATABASE))
        .await;
    emulator
        .create_workgroup_with_database(WORKGROUP, Some(DATABASE))
        .await;

    let mut owed = 0usize;
    for row in ROWS {
        let case = row.case;
        let answered = emulator.data_ok(row.operation, &submit_request(row)).await;
        let id = answered["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no Id in {answered}"))
            .to_string();

        // The statement's own record is what the Data API reports whatever came of
        // the announcement: a publication nothing took changes no answer.
        let record = wait_for_terminal(&emulator, case, &id).await;
        assert_eq!(
            record["Status"],
            json!(row.status),
            "case {case:?}: expected the statement to end {}, got {record}",
            row.status
        );

        if row.announced {
            owed += 1;
        }
        // Read once the statement has stopped, which is when its execution has
        // offered the status change to the bus and been turned away.
        let queued = owed_count(&emulator, owed).await;
        assert_eq!(
            queued, owed,
            "case {case:?}: expected {owed} status change(s) still owed after this row, got \
             {queued}"
        );
    }

    // Nothing is owed for the submissions that asked for nothing, which is what
    // the count above would have hidden if every row queued one.
    let announced = ROWS.iter().filter(|row| row.announced).count();
    let queued = owed_count(&emulator, announced).await;
    assert_eq!(
        queued, announced,
        "expected exactly the {announced} announced submissions to be owed an event, got {queued}"
    );
}

/// RULE, the third terminal status: a statement a caller STOPS is a status change
/// too, so a submission that asked for an event is owed one for it — and that one
/// is kept as well when the bus does not take it.
///
/// The statement is submitted while the turn on its database is HELD, so it is
/// still waiting for its turn when the cancel reaches it: the window in which it
/// can be stopped is closed by this test dropping the guard and never by how fast
/// the host ran the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_statement_is_owed_its_status_change_too() {
    let case = "a statement a caller stopped";
    let emulator = Warehouses::with_no_event_bus();
    emulator
        .create_cluster_with_database(CLUSTER, Some(DATABASE))
        .await;

    let held = emulator.hold_turn(CLUSTER, DATABASE);
    let answered = emulator
        .data_ok(
            "ExecuteStatement",
            &json!({
                "ClusterIdentifier": CLUSTER,
                "Database": DATABASE,
                "DbUser": DB_USER,
                "Sql": FINISHING_SQL,
                "WithEvent": true,
            }),
        )
        .await;
    let id = answered["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Id in {answered}"))
        .to_string();

    let cancelled = emulator
        .data_ok("CancelStatement", &json!({"Id": &id}))
        .await;
    assert_eq!(
        cancelled["Status"],
        json!(true),
        "case {case:?}: cancelling a statement that has not stopped answers that it was stopped: \
         {cancelled}"
    );
    drop(held);

    let record = wait_for_terminal(&emulator, case, &id).await;
    assert_eq!(
        record["Status"],
        json!("ABORTED"),
        "case {case:?}: a cancelled statement lands ABORTED: {record}"
    );

    let queued = owed_count(&emulator, 1).await;
    assert_eq!(
        queued, 1,
        "case {case:?}: the status change a stopped statement made must still be owed, got \
         {queued} queued"
    );
}
