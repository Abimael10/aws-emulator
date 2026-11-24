// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteWorker`.
//!
//! The rules under test:
//!
//! * `DeleteWorker` removes the worker its path names from the fleet its path names when
//!   the fleet's scheduler holds no work on it — the worker never started, or has reported
//!   itself stopped — so a later `GetWorker` of it is a 404 and the fleet's reported
//!   `workerCount` falls by one, which makes room for another registration;
//! * a worker in any other status is still registered with that scheduler, so a delete of
//!   it is refused with a 409 naming the worker, the status it is in and the statuses the
//!   deletion is valid in, and removes nothing;
//! * a worker is reached only through its own fleet and a fleet only through its own farm,
//!   so a delete naming an unheld farm, an unheld fleet, a fleet that is not the worker's
//!   or an unheld worker removes nothing and answers a 404 naming the level that is
//!   missing.
//!
//! The harness (`Emulator`) and the worker helpers are the ones the `worker` suite drives
//! the service with, so these tests go through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.
use serde_json::{json, Value};

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::{create_fleet_with_max, create_worker, fleet_worker_count, get_worker_path};

/// Every value of the modeled `WorkerStatus` enum
/// (`smithy_query(query="shape", shape="WorkerStatus")`), paired with whether a worker in
/// it can be deleted: the statuses the service's own valid-status list for a worker's
/// deletion names are `CREATED`, `DELETED`, `STOPPED` and `NOT_RESPONDING`, and `DELETED`
/// is not a modelled `WorkerStatus` (a deleted worker is gone), so the modelled statuses a
/// delete is valid in are the three left.
///
/// `pub(crate)` so every suite in this test target spans the one catalog.
pub(crate) const WORKER_STATUSES: &[(&str, bool)] = &[
    ("CREATED", true),
    ("STARTED", false),
    ("STOPPING", false),
    ("STOPPED", true),
    ("NOT_RESPONDING", true),
    ("NOT_COMPATIBLE", false),
    ("RUNNING", false),
    ("IDLE", false),
];

#[tokio::test]
async fn the_status_table_covers_every_modelled_worker_status() {
    // The table above is the catalog this suite spans, so it has to BE the catalog: the crate
    // generates the modelled values from the Smithy model as
    // `ops::types::WORKER_STATUS_VALUES`, and a model update that adds a status must fail here
    // rather than leave that status silently untested.
    let mut covered: Vec<&str> = WORKER_STATUSES.iter().map(|(status, _)| *status).collect();
    covered.sort_unstable();
    let mut modelled: Vec<&str> =
        service_com_amazonaws_deadline_deadline::ops::types::WORKER_STATUS_VALUES.to_vec();
    modelled.sort_unstable();
    assert_eq!(
        covered, modelled,
        "expected the suite's status table to span the modelled WorkerStatus catalog"
    );
}

/// Every value of `UpdatedWorkerStatus` — the statuses a worker's own agent reports through
/// `UpdateWorker` (`smithy_query(query="shape", shape="UpdatedWorkerStatus")`). The
/// remaining `WorkerStatus` values are the scheduler's own, so no operation of this service
/// reports them.
const REPORTED_STATUSES: &[&str] = &["STARTED", "STOPPING", "STOPPED"];

/// Register a worker with `fleet_id` and leave it in `status`, returning its id.
///
/// A registration leaves the worker `CREATED` and `UpdateWorker` reports each status of
/// [`REPORTED_STATUSES`] onward, so those statuses are reached the way a worker agent reaches
/// them. The rest are the scheduler's own, which nothing in this emulator schedules, so they
/// are arranged in the very store the handlers read. Either way the arrangement is read back
/// through `GetWorker` before anything is asserted about the delete, and the call under test
/// goes over the wire.
///
/// `pub(crate)` so every suite in this test target arranges a worker's status the same way.
pub(crate) async fn worker_in_status(
    emulator: &Emulator,
    farm_id: &str,
    fleet_id: &str,
    status: &str,
) -> String {
    let (code, body) = create_worker(emulator, farm_id, fleet_id, json!({}), None).await;
    assert_eq!(
        code, 200,
        "case {status:?}: expected CreateWorker to answer 200, got {code} with {body}"
    );
    let worker_id = string_at(&body, "workerId");

    if REPORTED_STATUSES.contains(&status) {
        let (code, updated) = emulator
            .call(
                "PATCH",
                &get_worker_path(farm_id, fleet_id, &worker_id),
                Some(json!({"status": status})),
                None,
            )
            .await;
        assert_eq!(
            code, 200,
            "case {status:?}: expected UpdateWorker to answer 200, got {code} with {updated}"
        );
    } else if status != "CREATED" {
        let store = emulator.store();
        let mut held = store.lock();
        let worker = held
            .fleet_mut(farm_id, fleet_id)
            .and_then(|fleet| fleet.workers.get_mut(&worker_id))
            .unwrap_or_else(|| {
                panic!("case {status:?}: expected {worker_id} stored under its fleet")
            });
        worker.status = status.to_string();
    }

    let (code, worker) = emulator
        .get(&get_worker_path(farm_id, fleet_id, &worker_id))
        .await;
    assert_eq!(
        (code, string_at(&worker, "status")),
        (200, status.to_string()),
        "case {status:?}: expected the worker to be readable in that status, got {code} with {worker}"
    );
    worker_id
}

/// `DELETE` the worker `worker_id` under `farm_id`/`fleet_id`.
///
/// `pub(crate)` so every suite in this test target drives the delete through the one request
/// shape the model lays out.
pub(crate) async fn delete_worker(
    emulator: &Emulator,
    farm_id: &str,
    fleet_id: &str,
    worker_id: &str,
) -> (u16, Value) {
    emulator
        .call(
            "DELETE",
            &get_worker_path(farm_id, fleet_id, worker_id),
            None,
            None,
        )
        .await
}

#[tokio::test]
async fn delete_worker_removes_a_worker_no_work_is_held_on() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("deleting workers").await;
    let fleet_id = create_fleet_with_max(&emulator, &farm_id, "deregistering", 8).await;

    // The rule: a worker the fleet's scheduler holds no work on is removed from the fleet
    // its path names. The rows are every status of the catalog above that the service's
    // valid-status list for a worker's deletion names.
    let cases: Vec<&str> = WORKER_STATUSES
        .iter()
        .filter(|(_, deletable)| *deletable)
        .map(|(status, _)| *status)
        .collect();
    assert!(
        cases.len() > 1,
        "expected more than one status a worker can be deleted in, got {cases:?}"
    );

    for case in cases {
        let worker_id = worker_in_status(&emulator, &farm_id, &fleet_id, case).await;
        let before = fleet_worker_count(&emulator, &farm_id, &fleet_id).await;

        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteWorker to answer 200, got {status} with {body}"
        );

        // The deletion is backed by the store every later read goes through: the worker is
        // gone, and the fleet's reported count falls with it.
        let (status, read) = emulator
            .get(&get_worker_path(&farm_id, &fleet_id, &worker_id))
            .await;
        assert_eq!(
            (
                status,
                error_code(&read),
                string_at(&read, "resourceType"),
                string_at(&read, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                "worker".to_string(),
                worker_id.clone(),
            ),
            "case {case:?}: expected GetWorker of the deleted worker to answer a 404, got {status} with {read}"
        );
        let after = fleet_worker_count(&emulator, &farm_id, &fleet_id).await;
        assert_eq!(
            after,
            before - 1,
            "case {case:?}: expected the fleet's workerCount to drop to {}, got {after}",
            before - 1
        );

        // A second delete names a worker this emulator no longer holds, so it is the same
        // 404 as any other unheld worker.
        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                "worker".to_string(),
                worker_id.clone(),
            ),
            "case {case:?}: expected a repeated delete to answer a 404, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn delete_worker_refuses_a_worker_that_has_started() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("started workers").await;
    let fleet_id = create_fleet_with_max(&emulator, &farm_id, "running", 8).await;

    // The rule: a worker in a status the service's valid-status list for a worker's deletion
    // does not name is still registered with the fleet's scheduler, so the delete is refused
    // and removes nothing. The rows are every status of the catalog above that list leaves
    // out — running the fleet's work, waiting for more of it, on its way down, or
    // incompatible with it.
    let cases: Vec<&str> = WORKER_STATUSES
        .iter()
        .filter(|(_, deletable)| !*deletable)
        .map(|(status, _)| *status)
        .collect();
    assert!(
        cases.len() > 1,
        "expected more than one status a worker cannot be deleted in, got {cases:?}"
    );

    let mut refusals: Vec<(&str, String)> = Vec::new();
    for case in cases {
        let worker_id = worker_in_status(&emulator, &farm_id, &fleet_id, case).await;
        let before = fleet_worker_count(&emulator, &farm_id, &fleet_id).await;

        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "reason"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                409,
                "ConflictException".to_string(),
                "STATUS_CONFLICT".to_string(),
                "worker".to_string(),
                worker_id.clone(),
            ),
            "case {case:?}: expected DeleteWorker to be refused with a 409 naming the worker, got {status} with {body}"
        );

        // The refusal states the status the worker is in, and that status is read from the
        // STORE — a delete request carries no status to echo, so a message that does not name
        // the stored one is reporting something it did not look up.
        let message = string_at(&body, "message");
        assert!(
            message.contains(case),
            "case {case:?}: expected the refusal to name the status the worker is in, got {message:?}"
        );
        refusals.push((case, message));

        // The refusal removed nothing: the worker is still readable in that status and the
        // fleet still counts it.
        let (status, read) = emulator
            .get(&get_worker_path(&farm_id, &fleet_id, &worker_id))
            .await;
        assert_eq!(
            (status, string_at(&read, "status")),
            (200, case.to_string()),
            "case {case:?}: expected the refused worker to be untouched, got {status} with {read}"
        );
        let after = fleet_worker_count(&emulator, &farm_id, &fleet_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the fleet's workerCount to stay {before}, got {after}"
        );
    }

    // Every row got its own sentence, which is what a status read per worker produces and a
    // fixed status in the refusal could not.
    for (left, right) in refusals.iter().zip(refusals.iter().skip(1)) {
        assert_ne!(
            left.1, right.1,
            "cases {:?} and {:?}: expected each refusal to name its own status, both said {:?}",
            left.0, right.0, left.1
        );
    }
}

#[tokio::test]
async fn a_worker_is_deleted_only_through_its_own_fleet() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("owner").await;
    let other_farm_id = emulator.farm("stranger").await;
    let fleet_id = create_fleet(&emulator, &farm_id, "owned").await;
    let sibling_fleet_id = create_fleet(&emulator, &farm_id, "sibling").await;
    let other_fleet_id = create_fleet(&emulator, &other_farm_id, "elsewhere").await;
    let absent_farm = absent_id("farm", "00000000000000000000000000000041");
    let absent_fleet = absent_id("fleet", "00000000000000000000000000000042");
    let absent_worker = absent_id("worker", "00000000000000000000000000000043");

    let worker_id = worker_in_status(&emulator, &farm_id, &fleet_id, "STOPPED").await;

    // The rule: the path reaches a worker only through its own fleet and that fleet only
    // through its own farm, so a delete naming a level this emulator does not hold removes
    // nothing and answers a 404 naming that level. The rows differ along which level of the
    // path is wrong.
    let cases: Vec<(&str, String, String, String, &str, String)> = vec![
        (
            "an unheld farm",
            absent_farm.clone(),
            fleet_id.clone(),
            worker_id.clone(),
            "farm",
            absent_farm.clone(),
        ),
        (
            "an unheld fleet",
            farm_id.clone(),
            absent_fleet.clone(),
            worker_id.clone(),
            "fleet",
            absent_fleet.clone(),
        ),
        (
            "another fleet of the same farm",
            farm_id.clone(),
            sibling_fleet_id.clone(),
            worker_id.clone(),
            "worker",
            worker_id.clone(),
        ),
        (
            "a fleet of another farm",
            other_farm_id.clone(),
            other_fleet_id.clone(),
            worker_id.clone(),
            "worker",
            worker_id.clone(),
        ),
        (
            "an unheld worker",
            farm_id.clone(),
            fleet_id.clone(),
            absent_worker.clone(),
            "worker",
            absent_worker.clone(),
        ),
    ];

    for (case, farm, fleet, worker, resource_type, resource_id) in cases {
        let (status, body) = delete_worker(&emulator, &farm, &fleet, &worker).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {case:?}: expected DeleteWorker to answer a 404 naming what is missing, got {status} with {body}"
        );
    }

    // None of those refusals removed anything: the worker is still readable through its own
    // path, and the delete through that path is what removes it.
    let (status, read) = emulator
        .get(&get_worker_path(&farm_id, &fleet_id, &worker_id))
        .await;
    assert_eq!(
        status, 200,
        "after the refusals: expected the worker to still be held, got {status} with {read}"
    );
    let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
    assert_eq!(
        status, 200,
        "through its own path: expected DeleteWorker to answer 200, got {status} with {body}"
    );
}

#[tokio::test]
async fn deleting_a_worker_makes_room_in_a_full_fleet() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("capacity").await;

    // The rule: a fleet's worker count stays at or below its own maxWorkerCount, and the
    // count is the workers it holds — so a delete releases exactly the room its worker took.
    // The rows differ along how many workers the fleet can hold at once.
    for (case, max) in [("a fleet that holds one worker", 1), ("a fleet that holds three workers", 3)] {
        let fleet_id = create_fleet_with_max(&emulator, &farm_id, case, max).await;
        let mut workers: Vec<String> = Vec::new();
        for _ in 0..max {
            workers.push(worker_in_status(&emulator, &farm_id, &fleet_id, "STOPPED").await);
        }

        let (status, body) = create_worker(&emulator, &farm_id, &fleet_id, json!({}), None).await;
        assert_eq!(
            (status, error_code(&body)),
            (409, "ConflictException".to_string()),
            "case {case:?}: expected the fleet to be full, got {status} with {body}"
        );

        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &workers[0]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteWorker to answer 200, got {status} with {body}"
        );

        let (status, body) = create_worker(&emulator, &farm_id, &fleet_id, json!({}), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the freed room to take a registration, got {status} with {body}"
        );
        let count = fleet_worker_count(&emulator, &farm_id, &fleet_id).await;
        assert_eq!(
            count, max,
            "case {case:?}: expected the fleet to hold {max} workers again, got {count}"
        );
    }
}
