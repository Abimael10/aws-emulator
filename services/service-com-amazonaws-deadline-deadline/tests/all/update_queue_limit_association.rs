// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateQueueLimitAssociation`, and for what
//! `GetQueueLimitAssociation` reports once an update has landed.
//!
//! The rules under test:
//!
//! * an update puts the `UpdateQueueLimitAssociationStatus` the request asks for on the
//!   STORED association and records that a caller modified it, so the read reports that
//!   status, `updatedAt` (modelled `@timestampFormat("date-time")`, so an RFC 3339 string)
//!   and `updatedBy` (the calling principal) — none of which an association carries before
//!   it is updated;
//! * either `STOP_LIMIT_USAGE_AND_*` status is the association winding the queue's usage of
//!   the limit down, a non-terminal status the service finishes by moving it to `STOPPED`
//!   after a delay (`smithy_query(query="doc", shape="UpdateQueueLimitAssociation")`), so a
//!   client polling the read for `STOPPED` converges; `ACTIVE` subjects the queue's work to
//!   the limit again and is terminal at once. The clock-owed move records no new update of
//!   its own, so `updatedAt`/`updatedBy` stay at the update that asked for the stop however
//!   late the read comes;
//! * the operation is `@idempotent` (`smithy_query(query="operation",
//!   operation="UpdateQueueLimitAssociation")`), so asking again for the status an
//!   association already carries answers the same success and leaves the stored association —
//!   its status, its pending stop and its update members — exactly as the first call left it;
//! * an association joins a queue and a limit OF ONE FARM, so an update naming a level
//!   nothing holds, or a pair that is not associated, writes nothing and answers a 404 naming
//!   the level that is missing;
//! * `status` is an enum, so a value outside `UpdateQueueLimitAssociationStatus` — including
//!   `STOPPED`, which only the READ's `QueueLimitAssociationStatus` carries — is refused with
//!   a 400 `ValidationException` and nothing is written.
//!
//! The harness (`Emulator`) and the association helpers are the ones the other suites drive
//! the service with, so every call goes through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.

use std::time::{Duration, Instant};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use crate::farm_queue_job::{absent_id, caller, error_code, string_at, Emulator};
use crate::limit::create_limit;
// An association is reached, created and described through the helpers the create/read suite
// owns, so every suite in this target drives the same request shapes.
use crate::queue_limit_association::{
    associate_ok, association_id, association_path, limit_body, read_association,
};

/// The status an association holds when the queue's usage of the limit has stopped — the one
/// terminal value of `QueueLimitAssociationStatus` (`smithy_query(query="shape",
/// shape="QueueLimitAssociationStatus")`: `ACTIVE`, `STOP_LIMIT_USAGE_AND_COMPLETE_TASKS`,
/// `STOP_LIMIT_USAGE_AND_CANCEL_TASKS`, `STOPPED`).
const STOPPED: &str = "STOPPED";

/// The queue's work subject to the limit: terminal in the other direction.
const ACTIVE: &str = "ACTIVE";

/// How long a poll for a clock-owed status keeps looking before the test gives up.
///
/// Generous on purpose: the test waits for the CONDITION, not for a span, so a loaded or slow
/// machine takes longer to converge rather than failing. Nothing in the emulator sleeps for
/// the move — the due instant is stored and compared on read.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Ask the association of `queue_id` and `limit_id` for `status`, as the model lays the
/// request out (`PATCH .../queue-limit-associations/{queueId}/{limitId}`).
async fn ask_for_status(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
    status: &str,
) -> (u16, Value) {
    emulator
        .call(
            "PATCH",
            &association_path(farm_id, queue_id, limit_id),
            Some(json!({"status": status})),
            None,
        )
        .await
}

/// Ask for `status` and fail the test unless the update was accepted with the modelled empty
/// response.
async fn update(
    emulator: &Emulator,
    case: &str,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
    status: &str,
) {
    let (got, body) = ask_for_status(emulator, farm_id, queue_id, limit_id, status).await;
    assert_eq!(
        got, 200,
        "case {case:?}: expected UpdateQueueLimitAssociation to {status} to answer 200, got \
         {got} with {body}"
    );
    assert!(
        body.is_null() || body.as_object().is_some_and(|members| members.is_empty()),
        "case {case:?}: expected an empty UpdateQueueLimitAssociation response, got {body}"
    );
}

/// Read the association until it reports `wanted`, or until [`POLL_DEADLINE`] passes — the
/// loop a client polling for a clock-owed status runs. Answers the document that reported it.
async fn poll_until_status(
    emulator: &Emulator,
    case: &str,
    farm_id: &str,
    queue_id: &str,
    limit_id: &str,
    wanted: &str,
) -> Value {
    let started = Instant::now();
    let mut attempts = 0_usize;
    loop {
        let body = read_association(emulator, farm_id, queue_id, limit_id).await;
        attempts += 1;
        if string_at(&body, "status") == wanted {
            return body;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected the association to reach {wanted:?} within the poll \
             deadline, still {:?} after {attempts} attempt(s): {body}",
            string_at(&body, "status")
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert `member` of `document` is the RFC 3339 date-time string the model's
/// `@timestampFormat("date-time")` requires, and answer the instant it names.
fn instant_at(case: &str, document: &Value, member: &str) -> DateTime {
    let text = document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {member} as an RFC 3339 string, got {document}")
        });
    DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} = {text:?} must parse as an RFC 3339 date-time: {error}")
    })
}

/// Assert the association `document` reports an update by this test's caller, and answer the
/// `updatedAt` text it carries.
fn assert_updated(case: &str, document: &Value) -> String {
    instant_at(case, document, "updatedAt");
    assert_eq!(
        string_at(document, "updatedBy"),
        caller(),
        "case {case:?}: expected the update to be recorded as made by the caller, got {document}"
    );
    string_at(document, "updatedAt")
}

#[tokio::test]
async fn update_queue_limit_association_writes_the_status_it_is_asked_for() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("updating").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Maya", "amount.maya")).await;
    // An association of the same limit nothing in this test updates, so an update is shown to
    // reach the one pair its path names.
    let bystander = emulator.queue(&farm_id, "untouched").await;
    associate_ok(&emulator, &farm_id, &bystander, &limit_id).await;

    // The rule: the status the request asks for is written to the stored association, and a
    // stop is finished at STOPPED by the clock. The rows are every value of
    // `UpdateQueueLimitAssociationStatus` (`smithy_query(query="shape",
    // shape="UpdateQueueLimitAssociationStatus")`: `ACTIVE`,
    // `STOP_LIMIT_USAGE_AND_COMPLETE_TASKS`, `STOP_LIMIT_USAGE_AND_CANCEL_TASKS`), asked for
    // on their own and in sequences, so the rows differ along which status is asked for, how
    // many updates the association has seen, and whether it had already stopped.
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "stop limit usage and complete tasks",
            vec!["STOP_LIMIT_USAGE_AND_COMPLETE_TASKS"],
            STOPPED,
        ),
        (
            "stop limit usage and cancel tasks",
            vec!["STOP_LIMIT_USAGE_AND_CANCEL_TASKS"],
            STOPPED,
        ),
        (
            "put back to active after a stop",
            vec!["STOP_LIMIT_USAGE_AND_CANCEL_TASKS", ACTIVE],
            ACTIVE,
        ),
        (
            "stopped, reactivated, then stopped the other way",
            vec![
                "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
                ACTIVE,
                "STOP_LIMIT_USAGE_AND_COMPLETE_TASKS",
            ],
            STOPPED,
        ),
    ];

    for (case, asked_for, terminal) in cases {
        let queue_id = emulator.queue(&farm_id, case).await;
        associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;

        // Nothing has updated it yet: the queue's work is subject to the limit and the
        // association carries no update members, which is what makes the members below the
        // update's own work.
        let before = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        assert_eq!(
            string_at(&before, "status"),
            ACTIVE,
            "case {case:?}: expected a fresh association to be active, got {before}"
        );
        assert!(
            before.get("updatedAt").is_none() && before.get("updatedBy").is_none(),
            "case {case:?}: expected no updatedAt/updatedBy before an update, got {before}"
        );

        for status in &asked_for {
            update(&emulator, case, &farm_id, &queue_id, &limit_id, status).await;

            // The update landed on the stored association: the read reports the status it
            // asked for, or — for a stop, whose wind-down the clock may already have
            // finished — the STOPPED it leads to.
            let after = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
            let reported = string_at(&after, "status");
            let allowed: Vec<&str> = if *status == ACTIVE {
                vec![ACTIVE]
            } else {
                vec![status, STOPPED]
            };
            assert!(
                allowed.contains(&reported.as_str()),
                "case {case:?}: after asking for {status}, expected the read to report one of \
                 {allowed:?}, got {reported:?} in {after}"
            );
            assert_updated(case, &after);
        }

        // A client polling for the terminal status converges, and the clock-owed move records
        // no update of its own: whatever the last update stamped is still what the read
        // reports once the association has settled.
        let stamped = assert_updated(
            case,
            &read_association(&emulator, &farm_id, &queue_id, &limit_id).await,
        );
        let settled =
            poll_until_status(&emulator, case, &farm_id, &queue_id, &limit_id, terminal).await;
        assert_eq!(
            (
                string_at(&settled, "queueId"),
                string_at(&settled, "limitId"),
                string_at(&settled, "createdBy"),
                string_at(&settled, "updatedAt"),
                string_at(&settled, "updatedBy"),
            ),
            (
                queue_id.clone(),
                limit_id.clone(),
                caller(),
                stamped,
                caller(),
            ),
            "case {case:?}: expected the settled association to keep the update it was \
             stamped with, got {settled}"
        );
        // The update's instant is the one the service wrote, not the creation instant it
        // started from.
        let created = instant_at(case, &settled, "createdAt");
        let updated = instant_at(case, &settled, "updatedAt");
        assert!(
            updated.as_secs_f64() >= created.as_secs_f64(),
            "case {case:?}: expected updatedAt at or after createdAt, got {settled}"
        );

        // The association nothing updated is still active, still carrying no update members:
        // the update took only the pair its path named.
        let other = read_association(&emulator, &farm_id, &bystander, &limit_id).await;
        assert_eq!(
            (
                string_at(&other, "status"),
                other.get("updatedAt").cloned(),
                other.get("updatedBy").cloned(),
            ),
            (ACTIVE.to_string(), None, None),
            "case {case:?}: expected another queue's association to be untouched, got {other}"
        );
    }
}

#[tokio::test]
async fn repeating_update_queue_limit_association_leaves_the_stored_association_alone() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("idempotent").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Nuke", "amount.nuke")).await;

    // The rule: the operation is `@idempotent`, so asking again for the status an association
    // already carries answers the same success and writes nothing again. The rows differ
    // along which status is repeated and how many times, and along whether the association
    // had been put back to active first.
    let cases: Vec<(&str, Vec<&str>, usize, &str)> = vec![
        (
            "a complete-tasks stop asked for twice",
            vec!["STOP_LIMIT_USAGE_AND_COMPLETE_TASKS"],
            2,
            STOPPED,
        ),
        (
            "a cancel-tasks stop asked for four times",
            vec!["STOP_LIMIT_USAGE_AND_CANCEL_TASKS"],
            4,
            STOPPED,
        ),
        (
            "a reactivation asked for three times",
            vec!["STOP_LIMIT_USAGE_AND_CANCEL_TASKS", ACTIVE],
            3,
            ACTIVE,
        ),
    ];

    for (case, asked_for, repeats, terminal) in cases {
        let queue_id = emulator.queue(&farm_id, case).await;
        associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;
        for status in &asked_for {
            update(&emulator, case, &farm_id, &queue_id, &limit_id, status).await;
        }
        let repeated = *asked_for
            .last()
            .expect("every row asks for at least one status");

        let first = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
        let stamped = assert_updated(case, &first);
        for attempt in 2..=repeats {
            update(&emulator, case, &farm_id, &queue_id, &limit_id, repeated).await;
            let again = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
            assert_eq!(
                (
                    string_at(&again, "createdAt"),
                    string_at(&again, "createdBy"),
                    string_at(&again, "updatedAt"),
                    string_at(&again, "updatedBy"),
                ),
                (
                    string_at(&first, "createdAt"),
                    string_at(&first, "createdBy"),
                    stamped.clone(),
                    caller(),
                ),
                "case {case:?}: attempt {attempt} wrote the association again: {first} then \
                 {again}"
            );
        }

        // Neither did the repeats restart the stop the first one asked for: the association
        // still converges on the status that first update aimed it at, with the same stamp.
        let settled =
            poll_until_status(&emulator, case, &farm_id, &queue_id, &limit_id, terminal).await;
        assert_eq!(
            string_at(&settled, "updatedAt"),
            stamped,
            "case {case:?}: expected the settled association to keep the first update's \
             instant, got {settled}"
        );
    }
}

#[tokio::test]
async fn update_queue_limit_association_refuses_a_level_nothing_holds() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("owner").await;
    let other_farm_id = emulator.farm("stranger").await;
    let queue_id = emulator.queue(&farm_id, "associated").await;
    let unassociated = emulator.queue(&farm_id, "unassociated").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Owned", "amount.maya")).await;
    let other_limit_id = create_limit(
        &emulator,
        &farm_id,
        limit_body("Unassociated", "amount.nuke"),
    )
    .await;
    let other_farm_limit = create_limit(
        &emulator,
        &other_farm_id,
        limit_body("Elsewhere", "amount.maya"),
    )
    .await;
    associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;

    let absent_farm = absent_id("farm", "00000000000000000000000000000061");
    let absent_queue = absent_id("queue", "00000000000000000000000000000062");
    let absent_limit = absent_id("limit", "00000000000000000000000000000063");

    // The rule: an association joins a queue and a limit OF ONE FARM, so an update naming
    // another farm, an unheld farm, an unheld queue, an unheld limit, another farm's limit or
    // a held limit that queue is not associated with writes nothing and answers a 404 naming
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
            "queue-limit-association",
            association_id(&farm_id, &queue_id, &other_limit_id),
        ),
        (
            "a queue that is associated with nothing",
            farm_id.clone(),
            unassociated.clone(),
            limit_id.clone(),
            "queue-limit-association",
            association_id(&farm_id, &unassociated, &limit_id),
        ),
    ];

    for (case, farm, queue, limit, resource_type, resource_id) in cases {
        let (got, body) = ask_for_status(
            &emulator,
            &farm,
            &queue,
            &limit,
            "STOP_LIMIT_USAGE_AND_CANCEL_TASKS",
        )
        .await;
        assert_eq!(
            got, 404,
            "case {case:?}: expected UpdateQueueLimitAssociation to answer 404, got {got} with \
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

    // None of those refusals touched the association this test does hold: it is still active
    // and still carries no update members.
    let held = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
    assert_eq!(
        (
            string_at(&held, "status"),
            held.get("updatedAt").cloned(),
            held.get("updatedBy").cloned(),
        ),
        (ACTIVE.to_string(), None, None),
        "after the refusals: expected the held association to be untouched, got {held}"
    );
}

#[tokio::test]
async fn update_queue_limit_association_refuses_a_status_outside_its_enum() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("validated").await;
    let queue_id = emulator.queue(&farm_id, "validated queue").await;
    let limit_id = create_limit(&emulator, &farm_id, limit_body("Validated", "amount.maya")).await;
    associate_ok(&emulator, &farm_id, &queue_id, &limit_id).await;

    // The rule: `status` targets `UpdateQueueLimitAssociationStatus`, which carries `ACTIVE`,
    // `STOP_LIMIT_USAGE_AND_COMPLETE_TASKS` and `STOP_LIMIT_USAGE_AND_CANCEL_TASKS` and NOT
    // the `STOPPED` of the read's `QueueLimitAssociationStatus`
    // (`smithy_query(query="shape", shape="UpdateQueueLimitAssociationStatus")`), so any
    // other value is refused at the boundary. The rows differ along how the value misses: a
    // status of the read enum only, the wrong case, a near miss, another resource's enum and
    // an empty value.
    let cases: [(&str, &str); 5] = [
        ("the terminal status only a read reports", STOPPED),
        ("the right status in the wrong case", "active"),
        (
            "a near miss of a stop status",
            "STOP_LIMIT_USAGE_AND_CANCEL_TASK",
        ),
        (
            "a status of another resource's enum",
            "STOP_SCHEDULING_AND_CANCEL_TASKS",
        ),
        ("an empty status", ""),
    ];

    for (case, status) in cases {
        let (got, body) = ask_for_status(&emulator, &farm_id, &queue_id, &limit_id, status).await;
        assert_eq!(
            (got, error_code(&body)),
            (400, "ValidationException".to_string()),
            "case {case:?}: expected the status {status:?} to be refused as invalid, got {got} \
             with {body}"
        );
    }

    // None of those refusals wrote anything: the association is still active and carries no
    // update members, and a modelled status is still accepted.
    let held = read_association(&emulator, &farm_id, &queue_id, &limit_id).await;
    assert_eq!(
        (
            string_at(&held, "status"),
            held.get("updatedAt").cloned(),
            held.get("updatedBy").cloned(),
        ),
        (ACTIVE.to_string(), None, None),
        "after the refusals: expected the association to be untouched, got {held}"
    );
    update(
        &emulator,
        "after the refusals",
        &farm_id,
        &queue_id,
        &limit_id,
        "STOP_LIMIT_USAGE_AND_COMPLETE_TASKS",
    )
    .await;
    let settled = poll_until_status(
        &emulator,
        "after the refusals",
        &farm_id,
        &queue_id,
        &limit_id,
        STOPPED,
    )
    .await;
    assert_updated("after the refusals", &settled);
}
