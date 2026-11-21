// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what an `UpdateFleet` of a fleet's worker-count window leaves
//! the fleet's DERIVED scaling members reporting.
//!
//! The rules under test:
//!
//! * a fleet's `targetWorkerCount` is the floor its own window sets and its
//!   `autoScalingStatus` is where the workers it holds stand relative to that target
//!   (`GROWING` below it, `STEADY` at it, `SHRINKING` above it — the whole of
//!   `smithy_query(query="shape", shape="AutoScalingStatus")`), so an `UpdateFleet` that
//!   moves either bound of the window moves those two members with it, in `GetFleet` and in
//!   the `ListFleets` summary of the same fleet alike, while leaving `workerCount` — the
//!   workers the fleet actually holds — alone;
//! * an update writes only the members it carries, so the clock-owed
//!   `CREATE_IN_PROGRESS -> ACTIVE` move of a fleet updated while it is still being stood up
//!   still comes due, and materialising it keeps what the update stored.
//!
//! The harness (`Emulator`), the fleet helpers and `create_worker` are the ones the `fleet`
//! and `worker` suites drive the service with, so these tests go through routing,
//! deserialization, constraint validation, the handler and serialization, exactly as the
//! router does in production.

use std::time::Duration;

use serde_json::{json, Value};

use crate::farm_queue_job::{string_at, Emulator};
use crate::fleet::{create_fleet_path, create_fleet_with_window, get_fleet_path};
use crate::update_fleet::read_fleet;
use crate::worker::create_worker;

/// How long a poll for the fleet's clock-owed status keeps looking before the test gives up.
///
/// Generous on purpose: the wait is for the CONDITION, so a loaded machine takes longer to
/// converge rather than failing, and nothing in the emulator sleeps for the move.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two reads of the fleet.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Register `count` workers with `fleet_id`, failing the test if any registration is refused.
async fn register_workers(emulator: &Emulator, farm_id: &str, fleet_id: &str, count: usize) {
    for worker in 0..count {
        let (status, body) = create_worker(
            emulator,
            farm_id,
            fleet_id,
            json!({"hostProperties": {"hostName": format!("render-{worker}")}}),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateWorker {worker} of {count}: expected 200, got {status} with {body}"
        );
    }
}

/// The scaling members of one fleet document: the window it holds, the workers it holds, the
/// target it is scaling towards and where it stands relative to that target.
fn scaling(fleet: &Value) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>, String) {
    (
        fleet.get("minWorkerCount").and_then(Value::as_i64),
        fleet.get("maxWorkerCount").and_then(Value::as_i64),
        fleet.get("workerCount").and_then(Value::as_i64),
        fleet.get("targetWorkerCount").and_then(Value::as_i64),
        string_at(fleet, "autoScalingStatus"),
    )
}

/// The one `ListFleets` summary of the fleet called `display_name` under `farm_id`.
async fn read_summary(emulator: &Emulator, farm_id: &str, display_name: &str, case: &str) -> Value {
    let (status, page) = emulator
        .get(&format!(
            "{}?displayName={display_name}",
            create_fleet_path(farm_id)
        ))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListFleets to answer 200, got {status} with {page}"
    );
    let fleets = page
        .get("fleets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        fleets.len(),
        1,
        "case {case:?}: expected ListFleets to report the one fleet named {display_name:?}, got {page}"
    );
    fleets[0].clone()
}

/// One row: the window the create stored, the workers registered with the fleet, the update
/// applied to it, and the scaling members before and after that update.
struct Row {
    case: &'static str,
    /// `minWorkerCount`/`maxWorkerCount` the create stored.
    created: (i64, i64),
    workers: usize,
    patch: Value,
    /// `targetWorkerCount`/`autoScalingStatus` before the update.
    before: (i64, &'static str),
    /// The window the update leaves the fleet with.
    window: (i64, i64),
    /// `targetWorkerCount`/`autoScalingStatus` after it.
    after: (i64, &'static str),
}

#[tokio::test]
async fn update_fleet_moves_the_derived_scaling_members() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("scaling").await;

    // The rows differ along which bound the update moves (the minimum, the maximum, both),
    // the direction it moves it, and how many workers the fleet holds while it moves — so
    // each of the three `AutoScalingStatus` values is both arrived at and left.
    let rows = [
        Row {
            case: "raising the minimum above the workers held leaves the fleet growing",
            created: (0, 4),
            workers: 0,
            patch: json!({"minWorkerCount": 2}),
            before: (0, "STEADY"),
            window: (2, 4),
            after: (2, "GROWING"),
        },
        Row {
            case: "raising the maximum alone leaves the target at the minimum",
            created: (0, 4),
            workers: 0,
            patch: json!({"maxWorkerCount": 2147483647}),
            before: (0, "STEADY"),
            window: (0, 2147483647),
            after: (0, "STEADY"),
        },
        Row {
            case: "a minimum the workers held already meet leaves the fleet steady",
            created: (0, 4),
            workers: 1,
            patch: json!({"minWorkerCount": 1}),
            before: (0, "SHRINKING"),
            window: (1, 4),
            after: (1, "STEADY"),
        },
        Row {
            case: "a minimum above the workers held leaves the fleet growing",
            created: (0, 4),
            workers: 1,
            patch: json!({"minWorkerCount": 3}),
            before: (0, "SHRINKING"),
            window: (3, 4),
            after: (3, "GROWING"),
        },
        Row {
            case: "lowering the minimum below the workers held leaves the fleet shrinking",
            created: (2, 4),
            workers: 2,
            patch: json!({"minWorkerCount": 0}),
            before: (2, "STEADY"),
            window: (0, 4),
            after: (0, "SHRINKING"),
        },
        Row {
            case: "closing the window on both bounds at once",
            created: (2, 4),
            workers: 2,
            patch: json!({"minWorkerCount": 0, "maxWorkerCount": 0}),
            before: (2, "STEADY"),
            window: (0, 0),
            after: (0, "SHRINKING"),
        },
        Row {
            case: "an update that moves no bound moves no derived member either",
            created: (1, 4),
            workers: 1,
            patch: json!({"description": "the window stayed where it was"}),
            before: (1, "STEADY"),
            window: (1, 4),
            after: (1, "STEADY"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let display_name = format!("scaling-{index}");
        let fleet_id = create_fleet_with_window(
            &emulator,
            &farm_id,
            &display_name,
            row.created.0,
            row.created.1,
        )
        .await;
        register_workers(&emulator, &farm_id, &fleet_id, row.workers).await;

        let held = i64::try_from(row.workers).expect("a row registers few workers");
        let before = read_fleet(&emulator, &farm_id, &fleet_id, case).await;
        assert_eq!(
            scaling(&before),
            (
                Some(row.created.0),
                Some(row.created.1),
                Some(held),
                Some(row.before.0),
                row.before.1.to_string(),
            ),
            "case {case:?}: expected the created window to scale as {:?}, got {before}",
            row.before
        );

        let (status, answered) = emulator
            .call(
                "PATCH",
                &get_fleet_path(&farm_id, &fleet_id),
                Some(row.patch.clone()),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateFleet to answer 200, got {status} with {answered}"
        );

        let expected = (
            Some(row.window.0),
            Some(row.window.1),
            Some(held),
            Some(row.after.0),
            row.after.1.to_string(),
        );
        let after = read_fleet(&emulator, &farm_id, &fleet_id, case).await;
        assert_eq!(
            scaling(&after),
            expected,
            "case {case:?}: expected GetFleet to report {expected:?} after the update, got {after}"
        );
        let summary = read_summary(&emulator, &farm_id, &display_name, case).await;
        assert_eq!(
            scaling(&summary),
            expected,
            "case {case:?}: expected the ListFleets summary to report {expected:?} too, got {summary}"
        );
    }
}

#[tokio::test]
async fn an_update_leaves_the_clock_owed_activation_of_the_fleet_alone() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("activating").await;
    let case = "updated while still being stood up";
    let fleet_id = create_fleet_with_window(&emulator, &farm_id, "activating-fleet", 0, 4).await;

    // The update lands on a fleet the service is still standing up, and moves members the
    // status has nothing to do with.
    let (status, answered) = emulator
        .call(
            "PATCH",
            &get_fleet_path(&farm_id, &fleet_id),
            Some(json!({"displayName": "updated-while-creating", "minWorkerCount": 2})),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateFleet to answer 200, got {status} with {answered}"
    );

    // The move the clock owed the fleet when it was created is still owed after the update: a
    // client polling the read converges on `ACTIVE`, and every status it sees on the way is one
    // the model declares — the crate's own `FleetStatus` value list, so a model update that adds
    // a status is picked up here rather than re-listed.
    let declared = service_com_amazonaws_deadline_deadline::ops::types::FLEET_STATUS_VALUES;
    let deadline = std::time::Instant::now() + STATUS_DEADLINE;
    let settled = loop {
        let fleet = read_fleet(&emulator, &farm_id, &fleet_id, case).await;
        let status = string_at(&fleet, "status");
        assert!(
            declared.contains(&status.as_str()),
            "case {case:?}: expected a modelled FleetStatus, got {status:?} in {fleet}"
        );
        if status == "ACTIVE" {
            break fleet;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the fleet never reached ACTIVE, last read {fleet}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Materialising that move wrote the status and nothing else, so the update is still what
    // the fleet reports.
    assert_eq!(
        (
            string_at(&settled, "displayName"),
            settled.get("minWorkerCount").and_then(Value::as_i64),
            settled.get("maxWorkerCount").and_then(Value::as_i64),
        ),
        ("updated-while-creating".to_string(), Some(2), Some(4)),
        "case {case:?}: expected the activated fleet to keep what the update stored, got {settled}"
    );
    assert!(
        settled.get("updatedAt").and_then(Value::as_str).is_some()
            && !string_at(&settled, "updatedBy").is_empty(),
        "case {case:?}: expected the update to be recorded as updatedAt/updatedBy, got {settled}"
    );
}
