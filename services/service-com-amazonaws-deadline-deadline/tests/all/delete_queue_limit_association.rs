// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteQueueLimitAssociation`, and for what the reads that
//! depend on it answer once it has landed.
//!
//! The rules under test:
//!
//! * an association is removed only once it has stopped subjecting the queue's work to the
//!   limit (`STOPPED`, the one terminal value of `QueueLimitAssociationStatus`): a delete
//!   asked for in any other status is refused with HTTP 409 `ConflictException` naming the
//!   association and the status it is in, and removes nothing;
//! * a delete of a stopped association removes it from its queue, so a later
//!   `GetQueueLimitAssociation` of the same three ids answers the modelled 404
//!   `ResourceNotFoundException` naming the association, and so does a repeat of the delete;
//! * an association joins a queue and a limit OF ONE FARM, so a delete naming an unheld
//!   farm, an unheld queue, an unheld limit, another farm's queue or limit, or a pair that is
//!   not associated removes nothing and answers a 404 naming the level that is missing;
//! * the association is a live reference to its limit, so `DeleteLimit` is refused while any
//!   queue still holds one and succeeds once every one of them has been deleted.
//!
//! The harness (`Emulator`) and the association, limit and poll helpers are the ones the
//! other suites drive the service with, so every call goes through routing, deserialization,
//! constraint validation, the handler and serialization, exactly as the router does in
//! production.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::limit::{create_limit, limit_path};
use crate::queue_limit_association::{
    associate_ok, association_id, association_path, limit_body, read_association,
};
use crate::status_conflict_wording::status_conflict_message;
// A stop is asked for and waited out exactly the way the update suite does it.
use crate::update_queue_limit_association::{poll_until_status, update};

/// The one terminal value of `QueueLimitAssociationStatus` (`smithy_query(query="shape",
/// shape="QueueLimitAssociationStatus")`: `ACTIVE`, `STOP_LIMIT_USAGE_AND_COMPLETE_TASKS`,
/// `STOP_LIMIT_USAGE_AND_CANCEL_TASKS`, `STOPPED`) — the status in which the queue's work no
/// longer uses the limit, so the association can be taken away.
const STOPPED: &str = "STOPPED";

/// The queue's work subject to the limit.
const ACTIVE: &str = "ACTIVE";

/// How far ahead a row that asserts the refusal pushes the pending stop's due instant, so the
/// transient status it is testing is still the one the association carries when the delete
/// lands however slowly this machine runs. Generous on purpose: nothing waits for it.
const A_STOP_THAT_STAYS_PENDING: Duration = Duration::from_secs(3_600);

/// The `resourceType` this service reports for a queue-limit association: the way the API
/// path (`/queue-limit-associations/`) spells the resource.
const RESOURCE_TYPE: &str = "queue-limit-association";

/// `DELETE` the association of `queue_id` and `limit_id` under `farm_id`, as the model lays
/// the request out (`smithy_query(query="operation",
/// operation="DeleteQueueLimitAssociation")`).
async fn delete_association(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
) -> (u16, Value) {
    emulator
        .call(
            "DELETE",
            &association_path(farm_id, queue_id, limit_id),
            None,
            None,
        )
        .await
}

/// `DELETE` the limit `limit_id` of `farm_id`.
async fn delete_limit(emulator: &Emulator, farm_id: &str, limit_id: &str) -> (u16, Value) {
    emulator
        .call("DELETE", &limit_path(farm_id, limit_id), None, None)
        .await
}

/// Assert the answer is the 404 this service gives for an association it does not hold,
/// naming the three ids of the request's path.
fn assert_association_not_found(
    case: &str,
    what: &str,
    got: u16,
    body: &Value,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
) {
    let missing = association_id(farm_id, queue_id, limit_id);
    assert_eq!(
        (
            got,
            error_code(body),
            string_at(body, "message"),
            string_at(body, "resourceType"),
            string_at(body, "resourceId"),
        ),
        (
            404,
            "ResourceNotFoundException".to_string(),
            format!("Resource of type {RESOURCE_TYPE} with id {missing} does not exist."),
            RESOURCE_TYPE.to_string(),
            missing.clone(),
        ),
        "case {case:?}: expected {what} to be the 404 naming the association, got {got} with \
         {body}"
    );
}

/// Push the pending stop of the association of `queue_id` and `limit_id` far enough into the
/// future that it is still underway when the next request lands.
///
/// The precondition itself — a stop asked for and not yet come due — is the one an update
/// through the API produces; only the instant it is due at is arranged here, because the span
/// the service picks for it is shorter than a slow machine's round trip and no operation lets
/// a caller choose it. The behaviour under test is still driven entirely through requests.
fn keep_the_stop_pending(
    emulator: &Emulator,
    case: &str,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
) {
    let store = emulator.store();
    let mut held = store.lock();
    let association = held
        .queue_limit_association_mut(farm_id, queue_id, limit_id)
        .unwrap_or_else(|| panic!("case {case:?}: expected {queue_id}/{limit_id} to be stored"));
    assert!(
        association.stopped_due_at.is_some(),
        "case {case:?}: expected the update to have left a stop pending, got {association:?}"
    );
    association.stopped_due_at = Some(Instant::now() + A_STOP_THAT_STAYS_PENDING);
}

#[tokio::test]
async fn delete_queue_limit_association_removes_a_stopped_association() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("stopped then deleted").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Maya", "amount.maya")).await;
    // One association nothing in this test deletes, so the delete is shown to remove the one
    // pair its path names.
    let bystander = emulator.queue(&farm_id, "untouched").await;
    associate_ok(&emulator, &farm_id, &bystander, &limit_id).await;

    // The rule: an association that has stopped is removed. The rows differ along the route
    // it took to `STOPPED` — which `UpdateQueueLimitAssociationStatus` stop was asked for
    // (`smithy_query(query="shape", shape="UpdateQueueLimitAssociationStatus")`: `ACTIVE`,
    // `STOP_LIMIT_USAGE_AND_COMPLETE_TASKS`, `STOP_LIMIT_USAGE_AND_CANCEL_TASKS`), whether
    // the same stop was asked for twice, and whether the association was put back to `ACTIVE`
    // before being stopped again.
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "stopped completing its tasks",
            vec!["STOP_LIMIT_USAGE_AND_COMPLETE_TASKS"],
        ),
        (
            "stopped cancelling its tasks",
            vec!["STOP_LIMIT_USAGE_AND_CANCEL_TASKS"],
        ),
        (
            "the same stop asked for twice",
            vec![
                "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
                "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
            ],
        ),
        (
            "reactivated, then stopped the other way",
            vec![
                "STOP_LIMIT_USAGE_AND_COMPLETE_TASKS",
                ACTIVE,
                "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
            ],
        ),
    ];

    for (case, asked_for) in cases {
        let queue_id = emulator.queue(&farm_id, case).await;
        associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;
        for status in &asked_for {
            update(&emulator, case, &farm_id, &queue_id, &limit_id, status).await;
        }
        // The documented sequence: poll the read until the stop has come through, then delete.
        poll_until_status(&emulator, case, &farm_id, &queue_id, &limit_id, STOPPED).await;

        let (got, body) = delete_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        assert_eq!(
            got, 200,
            "case {case:?}: expected DeleteQueueLimitAssociation to answer 200, got {got} with \
             {body}"
        );
        assert!(
            body.is_null() || body.as_object().is_some_and(|members| members.is_empty()),
            "case {case:?}: expected an empty DeleteQueueLimitAssociation response, got {body}"
        );

        // The association is gone from its queue: the read of the same three ids is the 404
        // naming it, and a second read answers the same.
        let (got, read) = emulator
            .get(&association_path(&farm_id, &queue_id, &limit_id))
            .await;
        assert_association_not_found(
            case,
            "the read after the delete",
            got,
            &read,
            &farm_id,
            &queue_id,
            &limit_id,
        );
        let (got, again) = emulator
            .get(&association_path(&farm_id, &queue_id, &limit_id))
            .await;
        assert_eq!(
            (got, again),
            (404, read),
            "case {case:?}: a second read of the deleted association answered differently"
        );

        // And the delete does not come back a second time: the pair is now one nothing holds.
        let (got, body) = delete_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        assert_association_not_found(
            case,
            "the repeated delete",
            got,
            &body,
            &farm_id,
            &queue_id,
            &limit_id,
        );

        // The association nothing deleted is still there, still subjecting its queue's work
        // to the limit: the delete took only the pair its path named.
        let other = read_association(&emulator, &farm_id, &bystander, &limit_id).await;
        assert_eq!(
            string_at(&other, "status"),
            ACTIVE,
            "case {case:?}: expected another queue's association to be untouched, got {other}"
        );
    }
}

#[tokio::test]
async fn delete_queue_limit_association_refuses_an_association_that_has_not_stopped() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("still using the limit").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Nuke", "amount.nuke")).await;

    // The rule: the delete is refused while the association still subjects the queue's work
    // to the limit. The rows are every value of `QueueLimitAssociationStatus` other than the
    // terminal one: the association using the limit, and each way of winding that usage down,
    // deleted while the wind-down is still underway.
    let cases: Vec<(&str, Option<&str>)> = vec![
        (ACTIVE, None),
        (
            "STOP_LIMIT_USAGE_AND_COMPLETE_TASKS",
            Some("STOP_LIMIT_USAGE_AND_COMPLETE_TASKS"),
        ),
        (
            "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
            Some("STOP_LIMIT_USAGE_AND_CANCEL_TASKS"),
        ),
    ];

    for (case, asked_for) in cases {
        let queue_id = emulator.queue(&farm_id, case).await;
        associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;
        if let Some(status) = asked_for {
            update(&emulator, case, &farm_id, &queue_id, &limit_id, status).await;
            keep_the_stop_pending(&emulator, case, &farm_id, &queue_id, &limit_id);
        }

        let (got, body) = delete_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        assert_eq!(
            got, 409,
            "case {case:?}: expected DeleteQueueLimitAssociation to answer 409, got {got} with \
             {body}"
        );
        assert_eq!(
            (
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "reason"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                "ConflictException".to_string(),
                status_conflict_message(
                    case,
                    &association_id(&farm_id, &queue_id, &limit_id),
                    &[STOPPED],
                ),
                "STATUS_CONFLICT".to_string(),
                RESOURCE_TYPE.to_string(),
                association_id(&farm_id, &queue_id, &limit_id),
            ),
            "case {case:?}: expected the 409 to name the association and the status it is in, \
             got {body}"
        );

        // The refusal removed nothing and moved nothing: the association is still there, in
        // the status it was refused in.
        let read = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        assert_eq!(
            string_at(&read, "status"),
            case,
            "case {case:?}: expected the refused association to be left as it was, got {read}"
        );
        // And the limit it references is still blocked by it.
        let (got, body) = delete_limit(&emulator, &farm_id, &limit_id).await;
        assert_eq!(
            got, 409,
            "case {case:?}: expected the limit to stay blocked by the refused association, got \
             {got} with {body}"
        );
    }
}

#[tokio::test]
async fn delete_queue_limit_association_refuses_a_level_nothing_holds() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("owner").await;
    let other_farm_id = emulator.farm("stranger").await;
    let queue_id = emulator.queue(&farm_id, "associated").await;
    let unassociated = emulator.queue(&farm_id, "associated with nothing").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Owned", "amount.maya")).await;
    let other_limit_id =
        create_limit(&emulator, &farm_id, limit_body("Unassociated", "amount.nuke")).await;
    let other_farm_limit = create_limit(
        &emulator,
        &other_farm_id,
        limit_body("Elsewhere", "amount.maya"),
    )
    .await;
    associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;

    let absent_farm = absent_id("farm", "00000000000000000000000000000071");
    let absent_queue = absent_id("queue", "00000000000000000000000000000072");
    let absent_limit = absent_id("limit", "00000000000000000000000000000073");

    // The rule: an association joins a queue and a limit OF ONE FARM, so a delete naming
    // another farm, an unheld farm, an unheld queue, an unheld limit, another farm's limit or
    // a held limit that queue is not associated with removes nothing and answers a 404 naming
    // the level that is missing. The rows differ along which level that is.
    let cases: Vec<(&str, String, String, String, &str, String)> = vec![
        (
            "a queue of another farm",
            other_farm_id.clone(),
            queue_id.clone(),
            limit_id.clone(),
            "queue",
            queue_id.clone(),
        ),
        (
            "an unheld farm",
            absent_farm.clone(),
            queue_id.clone(),
            limit_id.clone(),
            "farm",
            absent_farm.clone(),
        ),
        (
            "an unheld queue",
            farm_id.clone(),
            absent_queue.clone(),
            limit_id.clone(),
            "queue",
            absent_queue.clone(),
        ),
        (
            "an unheld limit",
            farm_id.clone(),
            queue_id.clone(),
            absent_limit.clone(),
            "limit",
            absent_limit.clone(),
        ),
        (
            "a limit of another farm",
            farm_id.clone(),
            queue_id.clone(),
            other_farm_limit.clone(),
            "limit",
            other_farm_limit.clone(),
        ),
        (
            "a limit this queue was never associated with",
            farm_id.clone(),
            queue_id.clone(),
            other_limit_id.clone(),
            RESOURCE_TYPE,
            association_id(&farm_id, &queue_id, &other_limit_id),
        ),
        (
            "a queue that is associated with nothing",
            farm_id.clone(),
            unassociated.clone(),
            limit_id.clone(),
            RESOURCE_TYPE,
            association_id(&farm_id, &unassociated, &limit_id),
        ),
    ];

    for (case, farm, queue, limit, resource_type, resource_id) in cases {
        let (got, body) = delete_association(&emulator, &farm, &queue, &limit).await;
        assert_eq!(
            got, 404,
            "case {case:?}: expected DeleteQueueLimitAssociation to answer 404, got {got} with \
             {body}"
        );
        assert_eq!(
            (
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                "ResourceNotFoundException".to_string(),
                format!("Resource of type {resource_type} with id {resource_id} does not exist."),
                resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {case:?}: expected the 404 to name the level that is missing, got {body}"
        );
    }

    // None of those refusals removed the association this test does hold.
    let held = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
    assert_eq!(
        string_at(&held, "status"),
        ACTIVE,
        "after the refusals: expected the held association to be untouched, got {held}"
    );
}

#[tokio::test]
async fn a_limit_is_deletable_once_every_association_of_it_is_deleted() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("teardown").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Houdini", "amount.houdini")).await;

    // The rule: the association is a live reference to its limit, so `DeleteLimit` is refused
    // while ANY queue still holds one and succeeds once every one of them has been deleted.
    // The rows are the queues holding the limit, so the limit stays blocked until the last
    // association is gone.
    let holders: Vec<(&str, String)> = vec![
        ("the first holder", emulator.queue(&farm_id, "first").await),
        ("the second holder", emulator.queue(&farm_id, "second").await),
        ("the third holder", emulator.queue(&farm_id, "third").await),
    ];
    for (_, queue_id) in &holders {
        associate_ok(&emulator, &farm_id, queue_id, &limit_id).await;
    }

    for (case, queue_id) in &holders {
        // While this association is held the limit cannot go.
        let (got, body) = delete_limit(&emulator, &farm_id, &limit_id).await;
        assert_eq!(
            got, 409,
            "case {case:?}: expected the limit to be blocked while an association holds it, \
             got {got} with {body}"
        );

        update(
            &emulator,
            case,
            &farm_id,
            queue_id,
            &limit_id,
            "STOP_LIMIT_USAGE_AND_COMPLETE_TASKS",
        )
        .await;
        poll_until_status(&emulator, case, &farm_id, queue_id, &limit_id, STOPPED).await;
        let (got, body) = delete_association(&emulator, &farm_id, queue_id, &limit_id).await;
        assert_eq!(
            got, 200,
            "case {case:?}: expected the stopped association to be deleted, got {got} with \
             {body}"
        );
    }

    // Nothing references the limit any more, so the delete the teardown was blocked on
    // succeeds and the limit is gone.
    let (got, body) = delete_limit(&emulator, &farm_id, &limit_id).await;
    assert_eq!(
        got, 200,
        "once no association is left: expected DeleteLimit to answer 200, got {got} with {body}"
    );
    let (got, read) = emulator.get(&limit_path(&farm_id, &limit_id)).await;
    assert_eq!(
        (got, error_code(&read)),
        (404, "ResourceNotFoundException".to_string()),
        "expected the freed limit to be gone after the delete, got {got} with {read}"
    );
}
