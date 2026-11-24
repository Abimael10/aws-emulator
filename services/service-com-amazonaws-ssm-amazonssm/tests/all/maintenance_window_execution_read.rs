// SPDX-License-Identifier: CC0-1.0

//! The single read of ONE run of a maintenance window: what
//! `GetMaintenanceWindowExecution` reports off the record the endpoint holds.
//!
//! "Retrieves details about a specific a maintenance window execution"
//! (`smithy_query(query="doc",shape="GetMaintenanceWindowExecution")`): the read resolves the
//! `WindowExecutionId` against the runs THIS endpoint holds for the Region it was asked
//! through and reports what that record holds, member for member — the status it reached, the
//! instants it ran between, the detail that status carries and the task executions it reached
//! — and it writes nothing, so a second caller sees exactly what the first one did. An id no
//! record is held under names no execution, which is the modelled `DoesNotExistException`
//! (HTTP 400); that refusal, and the ids the boundary walk never lets through, are driven in
//! `all/maintenance_window_executions.rs`, and the LISTING of the same records in
//! `all/maintenance_window_execution_listing.rs`.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call_in_region`], [`error_code`], [`router_with_engine`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service
//! router. The ONE thing seeded through the engine is a run of a window, because nothing in
//! this emulator picks up a maintenance window's tasks — the same reading Run Command takes of
//! a command's invocations — so no call of this service records one.
//!
//! The suites are tables: the rows differ along the status a run reached, whether it has
//! finished, how long a detail its status carries, how many task executions it reached and the
//! Region it is held in — rather than the single id a recorded unit happened to read.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::maintenance_window_executions::MaintenanceWindowExecutionRecord;
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, router_with_engine, REGION};

/// A second Region, to pin that a run is reached through the endpoint of the Region its window
/// lives in.
const OTHER_REGION: &str = "eu-central-1";

/// The instant the seeded runs start from, as whole seconds since the epoch
/// (2030-01-01T00:00:00Z).
const START: u64 = 1_893_456_000;

/// The millisecond remainder one seeded run starts at, to pin that an instant is reported at
/// the resolution this service states one at (`state::wire_instant`).
const START_MILLIS: u32 = 250;

/// Every value the modelled `MaintenanceWindowExecutionStatus` enum declares
/// (`smithy_query(query="shape",shape="MaintenanceWindowExecutionStatus")`), so the table
/// covers the statuses the issue did not cite as well as the one it did.
const EXECUTION_STATUSES: &[&str] = &[
    "PENDING",
    "IN_PROGRESS",
    "SUCCESS",
    "FAILED",
    "TIMED_OUT",
    "CANCELLING",
    "CANCELLED",
    "SKIPPED_OVERLAPPING",
];

/// The longest detail a status may carry (`GetMaintenanceWindowExecutionResult$StatusDetails`,
/// len 0..250).
const MAX_STATUS_DETAILS: usize = 250;

/// A well-formed `MaintenanceWindowId` (len 20, `^mw-[0-9a-f]{17}$`) to hold the seeded runs
/// under; which window a run belongs to is not what this read reports, so no window is created.
const WINDOW: &str = "mw-0123456789abcdef0";

/// The `MaintenanceWindowExecutionId` of the run at `ordinal`, in the modelled hyphenated
/// hexadecimal form (len 36, `^[0-9a-fA-F]{8}-...$`).
fn run_id(ordinal: usize) -> String {
    format!("{ordinal:08x}-1111-2222-3333-444444444444")
}

/// The `MaintenanceWindowExecutionTaskId` of the task execution at `ordinal` — the same
/// modelled form as a run's own id.
fn task_id(ordinal: usize) -> String {
    format!("{ordinal:08x}-aaaa-bbbb-cccc-dddddddddddd")
}

/// A clock reading `seconds` and `millis` after the epoch.
fn epoch(seconds: u64, millis: u32) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(u64::from(millis))
}

/// `seconds` and `millis` after the epoch as this protocol puts a timestamp on the wire:
/// epoch seconds with a fraction.
fn wire(seconds: u64, millis: u32) -> f64 {
    seconds as f64 + f64::from(millis) / 1_000.0
}

/// What one seeded run holds beyond its ids.
struct SeededRun<'a> {
    region: &'a str,
    status: &'a str,
    started: (u64, u32),
    ended: Option<(u64, u32)>,
    status_details: Option<String>,
    task_execution_ids: Vec<String>,
}

/// Record one run of [`WINDOW`], the way the service picking the window's tasks up would.
///
/// Seeded through the engine because no call of this service records a run; every member the
/// read reports is set here, so a read that invents one is visible.
fn seed_run(
    engine: &Arc<engine::InMemoryEngine>,
    window_execution_id: &str,
    run: &SeededRun<'_>,
) {
    let record = MaintenanceWindowExecutionRecord {
        window_execution_id: window_execution_id.to_string(),
        window_id: WINDOW.to_string(),
        region: run.region.to_string(),
        started_at: epoch(run.started.0, run.started.1),
        ended_at: run.ended.map(|(secs, millis)| epoch(secs, millis)),
        status: run.status.to_string(),
        status_details: run.status_details.clone(),
        task_execution_ids: run.task_execution_ids.clone(),
    };
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .insert_maintenance_window_execution(record);
}

/// Read one execution at `region`'s endpoint.
async fn read_in(
    router: &MultiServiceRouter,
    region: &str,
    window_execution_id: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetMaintenanceWindowExecution",
        &json!({ "WindowExecutionId": window_execution_id }),
    )
    .await
}

/// The `TaskIds` a response reported, in the order it reported them.
fn reported_task_ids(response: &Value) -> Vec<String> {
    response["TaskIds"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| id.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that reading `window_execution_id` at `region` reports exactly what `run` holds.
async fn reads_back(
    router: &MultiServiceRouter,
    case: &str,
    window_execution_id: &str,
    run: &SeededRun<'_>,
) {
    let (status, response) = read_in(router, run.region, window_execution_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the held run must be readable, got {status} {response}"
    );
    assert_eq!(
        response["WindowExecutionId"].as_str(),
        Some(window_execution_id),
        "case {case:?}: expected the id the record is held under, got {response}"
    );
    assert_eq!(
        response["Status"].as_str(),
        Some(run.status),
        "case {case:?}: expected the status the record holds, got {response}"
    );
    assert_eq!(
        response["StatusDetails"].as_str(),
        run.status_details.as_deref(),
        "case {case:?}: expected the detail the record holds, got {response}"
    );
    assert_eq!(
        response["StartTime"].as_f64(),
        Some(wire(run.started.0, run.started.1)),
        "case {case:?}: expected the instant the record started at, got {response}"
    );
    assert_eq!(
        response["EndTime"].as_f64(),
        run.ended.map(|(secs, millis)| wire(secs, millis)),
        "case {case:?}: expected the end the record holds, got {response}"
    );
    assert_eq!(
        reported_task_ids(&response),
        run.task_execution_ids,
        "case {case:?}: expected the task executions the record holds, got {response}"
    );
}

/// The rows of the projection table: one run per modelled status, differing along whether the
/// run has finished, how long a detail its status carries and how many task executions it
/// reached.
fn projection_rows() -> Vec<(String, SeededRun<'static>)> {
    EXECUTION_STATUSES
        .iter()
        .enumerate()
        .map(|(ordinal, status)| {
            // A run still going holds no end; a finished one ran for as many seconds as its
            // ordinal plus one.
            let running = matches!(*status, "PENDING" | "IN_PROGRESS" | "CANCELLING");
            let started = (START + ordinal as u64 * 3_600, 0);
            let ended = if running {
                None
            } else {
                Some((started.0 + ordinal as u64 + 1, 0))
            };
            // The detail a status carries: none, empty, a sentence, and the longest the
            // modelled length admits — so the bound itself is covered.
            let status_details = match ordinal % 4 {
                0 => None,
                1 => Some(String::new()),
                2 => Some(format!("the run reached {status}")),
                _ => Some("d".repeat(MAX_STATUS_DETAILS)),
            };
            let run = SeededRun {
                region: REGION,
                status,
                started,
                ended,
                status_details,
                // 0, 1, 2 ... task executions, in the order the run reached them.
                task_execution_ids: (0..ordinal).map(task_id).collect(),
            };
            (format!("a run that reached {status}"), run)
        })
        .collect()
}

/// RULE: a read of an id this endpoint holds a run under reports that record's own members —
/// its id, status, status detail, start, end and task executions — and nothing derived about
/// work that did not happen.
#[tokio::test]
async fn a_held_run_reads_back_what_the_record_holds() {
    let (router, engine) = router_with_engine();
    let rows = projection_rows();

    for (ordinal, (_, run)) in rows.iter().enumerate() {
        seed_run(&engine, &run_id(ordinal), run);
    }

    for (ordinal, (case, run)) in rows.iter().enumerate() {
        reads_back(&router, case, &run_id(ordinal), run).await;
    }
}

/// RULE: an instant a read reports keeps the sub-second resolution this protocol states one
/// at, so a run that started part-way through a second is not reported as having started on
/// the second.
#[tokio::test]
async fn an_instant_is_reported_at_the_resolution_it_was_recorded_at() {
    let (router, engine) = router_with_engine();
    let run = SeededRun {
        region: REGION,
        status: "SUCCESS",
        started: (START, START_MILLIS),
        ended: Some((START + 7, 500)),
        status_details: None,
        task_execution_ids: vec![task_id(0)],
    };
    seed_run(&engine, &run_id(0), &run);

    reads_back(&router, "a run started part-way through a second", &run_id(0), &run).await;
}

/// RULE: a run is reached through the endpoint of the Region it is held in — an execution of
/// another Region's window is no execution of this one, so the SAME id reads back one run at
/// one endpoint and is refused at the other.
#[tokio::test]
async fn a_run_is_reached_only_through_the_region_holding_it() {
    let (router, engine) = router_with_engine();

    let here = SeededRun {
        region: REGION,
        status: "SUCCESS",
        started: (START, 0),
        ended: Some((START + 30, 0)),
        status_details: None,
        task_execution_ids: vec![task_id(0)],
    };
    let there = SeededRun {
        region: OTHER_REGION,
        status: "FAILED",
        started: (START + 60, 0),
        ended: Some((START + 90, 0)),
        status_details: Some("One or more tasks failed".to_string()),
        task_execution_ids: Vec::new(),
    };
    // One id per Region, and one id held in BOTH, which is a different record each side.
    seed_run(&engine, &run_id(1), &here);
    seed_run(&engine, &run_id(2), &there);
    seed_run(&engine, &run_id(3), &here);
    seed_run(&engine, &run_id(3), &there);

    reads_back(&router, "the run held here", &run_id(1), &here).await;
    reads_back(&router, "the run held there", &run_id(2), &there).await;
    reads_back(&router, "the shared id, here", &run_id(3), &here).await;
    reads_back(&router, "the shared id, there", &run_id(3), &there).await;

    // (case, the id asked for, the Region asked through) — each id names no run at the other
    // endpoint.
    for (case, id, region) in [
        ("the run held here, asked there", run_id(1), OTHER_REGION),
        ("the run held there, asked here", run_id(2), REGION),
    ] {
        let (status, response) = read_in(&router, region, &id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {id} names no execution at {region}, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "DoesNotExistException",
            "case {case:?}: expected the modelled not-found refusal, got {response}"
        );
    }
}

/// RULE: the read reports and does not mutate — reading a run, repeatedly and interleaved with
/// a refused read, leaves every record exactly as it was, so a later caller sees what the first
/// one did.
#[tokio::test]
async fn reading_a_run_changes_nothing_a_later_caller_sees() {
    let (router, engine) = router_with_engine();
    let run = SeededRun {
        region: REGION,
        status: "IN_PROGRESS",
        started: (START, 0),
        ended: None,
        status_details: Some("waiting on one task".to_string()),
        task_execution_ids: vec![task_id(0), task_id(1)],
    };
    seed_run(&engine, &run_id(0), &run);

    let mut first: Option<Value> = None;
    for round in 1..=3 {
        let (status, response) = read_in(&router, REGION, &run_id(0)).await;
        assert_eq!(
            status, 200,
            "round {round}: the held run must stay readable, got {status} {response}"
        );
        match &first {
            None => first = Some(response),
            Some(earlier) => assert_eq!(
                &response, earlier,
                "round {round}: expected the read to report what the first read did, got {response}"
            ),
        }

        // A refused read of an id nothing is held under records nothing either.
        let (status, refused) = read_in(&router, REGION, &run_id(99)).await;
        assert_eq!(
            status, 400,
            "round {round}: an id naming no run is refused, got {status} {refused}"
        );
    }

    // The listing of the same window still reports the ONE run that was ever recorded.
    let (status, listed) = call_in_region(
        &router,
        REGION,
        "DescribeMaintenanceWindowExecutions",
        &json!({ "WindowId": WINDOW }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the listing must succeed, got {status} {listed}"
    );
    let ids: Vec<&str> = listed["WindowExecutions"]
        .as_array()
        .map(|executions| {
            executions
                .iter()
                .map(|execution| execution["WindowExecutionId"].as_str().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec![run_id(0).as_str()],
        "expected the one run recorded, got {listed}"
    );
}
