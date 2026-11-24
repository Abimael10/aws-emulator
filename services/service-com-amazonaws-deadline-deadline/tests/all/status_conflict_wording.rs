// SPDX-License-Identifier: CC0-1.0

//! How this service words a call the status of the resource it names does not permit.
//!
//! RULE: when a call is refused because the status the resource it names is in does not
//! permit that call, this service answers HTTP 409 `ConflictException` with `reason`
//! `STATUS_CONFLICT`, `resourceId`/`resourceType` naming that resource, and words `message`
//! as `Request could not be completed. Status <status> of resource <resourceId> does not
//! permit the operation. The valid statuses are [<the statuses the operation is valid in>]`
//! — `<status>` read from the stored resource and `<resourceId>` from the request's path,
//! with no trailing period. The model spells the error's shape but no text
//! (`smithy_query(query="shape", shape="ConflictException")`: `httpError` 409, `message`,
//! `reason`, `resourceId` and `resourceType` all `@required`, `reason` targeting
//! `ConflictExceptionReason` = [CONFLICT_EXCEPTION, CONCURRENT_MODIFICATION,
//! RESOURCE_ALREADY_EXISTS, RESOURCE_IN_USE, STATUS_CONFLICT]), so the sentence is the
//! service's own: a client that keys remediation on it — or that parses the valid-status
//! list to decide which transition to drive the resource through before retrying — must see
//! the same text and the same id it sees against AWS.
//!
//! The rows below span the class rather than the one operation a divergence was observed on:
//! every modelled worker status a delete is refused in, and the two composite associations
//! whose own status refuses their delete, so both the per-operation valid-status list and the
//! per-resource id are asserted. The last test closes the loop the message opens — every
//! status the sentence lists as valid is a status the delete really accepts — so the list
//! cannot drift into advertising a status the service would then refuse.
//!
//! The harness (`Emulator`) and the resource helpers are the ones the other suites drive the
//! service with, so every call goes through routing, deserialization, constraint validation,
//! the handler and serialization, exactly as the router does in production.

use crate::delete_worker::{delete_worker, worker_in_status, WORKER_STATUSES};
use crate::farm_queue_job::{error_code, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::limit::create_limit;
use crate::worker::create_fleet_with_max;

/// The sentence this service words a status conflict with, as the responses recorded from
/// AWS show it.
///
/// `pub(crate)` so every suite in this test target expects the one sentence from the one
/// place rather than spelling it out again.
pub(crate) fn status_conflict_message(
    status: &str,
    resource_id: &str,
    valid_statuses: &[&str],
) -> String {
    format!(
        "Request could not be completed. Status {status} of resource {resource_id} does not \
         permit the operation. The valid statuses are [{}]",
        valid_statuses.join(", ")
    )
}

/// The statuses a worker's deletion is valid in, as this service lists them in its refusal.
///
/// `DELETED` is not one of the modelled `WorkerStatus` values
/// (`smithy_query(query="shape", shape="WorkerStatus")`: `CREATED`, `STARTED`, `STOPPING`,
/// `STOPPED`, `NOT_RESPONDING`, `NOT_COMPATIBLE`, `RUNNING`, `IDLE`) — a worker already let
/// go of is gone, and reading it is a 404 — so it is part of the sentence but not a status
/// any stored worker is read in. The three that are modelled are exactly the rows
/// [`WORKER_STATUSES`] marks deletable, which the last test here checks.
const WORKER_VALID_STATUSES: &[&str] = &["CREATED", "DELETED", "STOPPED", "NOT_RESPONDING"];

/// The statuses an association's deletion is valid in: the one status in which it holds none
/// of the queue's work (`smithy_query(query="shape", shape="QueueFleetAssociationStatus")`
/// and `smithy_query(query="shape", shape="QueueLimitAssociationStatus")` both make `STOPPED`
/// the terminal one, the others being the association scheduling or draining).
const ASSOCIATION_VALID_STATUSES: &[&str] = &["STOPPED"];

#[tokio::test]
async fn a_worker_delete_refusal_states_the_status_and_the_valid_statuses() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("status conflicts").await;
    let fleet_id = create_fleet_with_max(&emulator, &farm_id, "busy", 16).await;

    // The rows are every modelled worker status the valid-status list leaves out, so the
    // sentence is asserted over the whole refused class and not over the one status a
    // divergence was observed in.
    let cases: Vec<&str> = WORKER_STATUSES
        .iter()
        .filter(|(_, deletable)| !*deletable)
        .map(|(status, _)| *status)
        .collect();
    assert!(
        cases.len() > 1,
        "expected more than one status a worker's delete is refused in, got {cases:?}"
    );

    for case in cases {
        let worker_id = worker_in_status(&emulator, &farm_id, &fleet_id, case).await;

        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "reason"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                409,
                "ConflictException".to_string(),
                status_conflict_message(case, &worker_id, WORKER_VALID_STATUSES),
                "STATUS_CONFLICT".to_string(),
                "worker".to_string(),
                worker_id.clone(),
            ),
            "case {case:?}: expected the refusal to state the worker's status, its id and the \
             statuses the delete is valid in, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_worker_delete_refusal_names_the_worker_the_request_named() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("two workers").await;
    let fleet_id = create_fleet_with_max(&emulator, &farm_id, "both running", 16).await;

    // The rule's `<resourceId>` is the id the REQUEST named, so two workers refused in the
    // same status get two sentences differing in exactly that: an id lifted from anywhere
    // else — a fixed string, or the other teardown in flight — could not do this.
    let first = worker_in_status(&emulator, &farm_id, &fleet_id, "STARTED").await;
    let second = worker_in_status(&emulator, &farm_id, &fleet_id, "STARTED").await;
    assert_ne!(
        first, second,
        "expected two distinct workers, got {first} twice"
    );

    for (case, worker_id, other_id) in [
        ("the first worker", &first, &second),
        ("the second worker", &second, &first),
    ] {
        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, worker_id).await;
        assert_eq!(
            (status, string_at(&body, "message")),
            (
                409,
                status_conflict_message("STARTED", worker_id, WORKER_VALID_STATUSES)
            ),
            "case {case:?}: expected the refusal to name {worker_id}, got {status} with {body}"
        );
        assert!(
            !string_at(&body, "message").contains(other_id.as_str()),
            "case {case:?}: expected the refusal not to name the other worker {other_id}, got \
             {body}"
        );
    }
}

#[tokio::test]
async fn an_association_delete_refusal_is_worded_the_same_way() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("associations").await;

    // The rows are the two composite resources whose own status refuses their delete: both
    // are named by the three ids of their path rather than by an id of their own, and both
    // carry their own one-status valid list, so the same sentence is asserted across two
    // operations and two resource types.
    let fleet_queue_id = emulator.queue(&farm_id, "scheduling queue").await;
    let fleet_id = create_fleet(&emulator, &farm_id, "the fleet").await;
    crate::queue_fleet_association::associate(&emulator, &farm_id, &fleet_queue_id, &fleet_id)
        .await;

    let limit_queue_id = emulator.queue(&farm_id, "limited queue").await;
    let limit_id = create_limit(
        &emulator,
        &farm_id,
        crate::queue_limit_association::limit_body("The limit", "amount.worker.vcpu"),
    )
    .await;
    crate::queue_limit_association::associate_ok(&emulator, &farm_id, &limit_queue_id, &limit_id)
        .await;

    let cases: Vec<(&str, String, String, &str)> = vec![
        (
            "a queue-fleet association still scheduling the queue's work",
            crate::queue_fleet_association::association_path(
                &farm_id,
                &fleet_queue_id,
                &fleet_id,
            ),
            crate::queue_fleet_association::association_id(&farm_id, &fleet_queue_id, &fleet_id),
            "queue-fleet-association",
        ),
        (
            "a queue-limit association still subjecting the queue's work to the limit",
            crate::queue_limit_association::association_path(
                &farm_id,
                &limit_queue_id,
                &limit_id,
            ),
            crate::queue_limit_association::association_id(&farm_id, &limit_queue_id, &limit_id),
            "queue-limit-association",
        ),
    ];

    for (case, path, resource_id, resource_type) in cases {
        let (status, body) = emulator.call("DELETE", &path, None, None).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "reason"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                409,
                "ConflictException".to_string(),
                status_conflict_message("ACTIVE", &resource_id, ASSOCIATION_VALID_STATUSES),
                "STATUS_CONFLICT".to_string(),
                resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {case:?}: expected the refusal to state the association's status, its id \
             and the statuses the delete is valid in, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn every_status_a_worker_refusal_lists_is_one_the_delete_accepts() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("valid statuses").await;
    let fleet_id = create_fleet_with_max(&emulator, &farm_id, "emptying", 16).await;

    // The sentence promises the delete is valid in each listed status, so each one this
    // service can hold a worker in is driven through the delete. `DELETED` is not a modelled
    // `WorkerStatus`, so no stored worker is ever read in it: the rows are the rest.
    let modelled: Vec<&str> =
        service_com_amazonaws_deadline_deadline::ops::types::WORKER_STATUS_VALUES.to_vec();
    let cases: Vec<&str> = WORKER_VALID_STATUSES
        .iter()
        .copied()
        .filter(|status| modelled.contains(status))
        .collect();
    assert_eq!(
        cases.len(),
        WORKER_VALID_STATUSES.len() - 1,
        "expected every listed status but DELETED to be a modelled WorkerStatus, got {cases:?}"
    );

    for case in &cases {
        let worker_id = worker_in_status(&emulator, &farm_id, &fleet_id, case).await;
        let (status, body) = delete_worker(&emulator, &farm_id, &fleet_id, &worker_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refusal lists this status as valid, so expected DeleteWorker \
             to answer 200, got {status} with {body}"
        );
    }

    // And the catalog the delete suite spans agrees: the statuses it marks deletable are
    // exactly the modelled ones the sentence lists, so neither side can drift alone.
    let mut deletable: Vec<&str> = WORKER_STATUSES
        .iter()
        .filter(|(_, deletable)| *deletable)
        .map(|(status, _)| *status)
        .collect();
    deletable.sort_unstable();
    let mut listed = cases.clone();
    listed.sort_unstable();
    assert_eq!(
        deletable, listed,
        "expected the statuses a worker can be deleted in to be the modelled statuses the \
         refusal lists"
    );
}
