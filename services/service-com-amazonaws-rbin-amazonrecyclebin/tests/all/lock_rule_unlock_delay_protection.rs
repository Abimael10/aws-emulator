// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the lock a `LockRule` request may CHANGE.
//!
//! The rule under test: a lock request that would change the unlock delay in force on a
//! retention rule whose lock still forbids modifying it — `locked`, or unlocked but
//! still inside its unlock delay — is refused with the modelled `ConflictException`
//! (HTTP 409) and changes nothing, so the delay a lock was taken under can be replaced
//! only once that protection has lapsed. A request naming the delay the rule is already
//! serving changes no lock at all, so it stays the documented re-lock that returns a
//! rule inside its unlock delay to `locked`.
//!
//! `tests/all/lock_rule_idempotence.rs` owns the same refusal on a rule that is still
//! `locked`, together with the repeat of a lock request the rule already satisfies; this
//! suite holds the other half of the same predicate — the rule that has been unlocked
//! and is still serving its delay — and the window past it in which another delay IS
//! accepted.
//!
//! An unlock delay is at least 7 days, so reaching that window means moving the instant
//! the clock owes the transition at into the past (`end_unlock_delay`), never waiting.
//!
//! Every case drives the service through the real router, exactly as a signed SDK
//! request would, and every row states its own case so a failure names the input.

use serde_json::json;

use crate::error_response_bodies::{error_code, message};
use crate::retention_rule_lifecycle::{
    create_rule, delete_rule, get_rule, lock_configuration, retention, update_rule, RESOURCE_TYPES,
    UNLOCK_DELAY_MAX, UNLOCK_DELAY_MIN,
};
use crate::rule_lock_transitions::{lock_end_seconds, lock_rule, unlock_rule};
use crate::update_rule_lock_invariant::{end_unlock_delay, fixture, Fixture};

/// An unlock delay between the published bounds, so no table is only its edges.
/// Source: smithy_query(query="shape", shape="UnlockDelay") — `UnlockDelayValue` range
/// {min: 7, max: 30}.
const UNLOCK_DELAY_MIDDLE: i64 = 19;

/// The retention period every rule here is created with, in days — one every modelled
/// resource type supports.
const CREATED_RETENTION: i64 = 7;

/// Which door the rule's lock arrived through: `CreateRule` with a `LockConfiguration`,
/// or `LockRule` on a rule created without one. Both leave the same `locked` rule, so
/// both must answer the same way once it is unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    AtCreate,
    ByLockRule,
}

/// Create a rule of `resource_type`, lock it under `unlock_delay` through `door`, then
/// unlock it so it is serving that delay. Answers its identifier and the `LockEndTime`
/// it reports while the delay runs.
async fn rule_inside_its_unlock_delay(
    fixture: &Fixture,
    case: &str,
    resource_type: &str,
    unlock_delay: i64,
    door: Door,
) -> (String, f64) {
    let configuration = lock_configuration(unlock_delay);
    let mut body = json!({
        "ResourceType": resource_type,
        "RetentionPeriod": retention(CREATED_RETENTION),
    });
    if door == Door::AtCreate {
        body["LockConfiguration"] = configuration.clone();
    }
    let (status, created) = create_rule(&fixture.router, body).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRule expected HTTP 201, got {status} / {created:?}"
    );
    let identifier = created["Identifier"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateRule returned no Identifier: {created:?}"))
        .to_string();

    if door == Door::ByLockRule {
        let (status, locked) = lock_rule(
            &fixture.router,
            &identifier,
            json!({ "LockConfiguration": configuration.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: LockRule expected HTTP 200, got {status} / {locked:?}"
        );
    }

    let (status, unlocked) = unlock_rule(&fixture.router, &identifier).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UnlockRule expected HTTP 200, got {status} / {unlocked:?}"
    );
    assert_eq!(
        unlocked["LockState"].as_str(),
        Some("pending_unlock"),
        "case {case:?}: expected LockState=pending_unlock, got {:?}",
        unlocked["LockState"]
    );
    let lock_end = lock_end_seconds(&unlocked).unwrap_or_else(|| {
        panic!("case {case:?}: expected a LockEndTime inside the unlock delay: {unlocked:?}")
    });
    (identifier, lock_end)
}

/// Assert the rule's lock still forbids modifying and deleting it at this instant — the
/// protection a lock request must not be able to replace.
async fn assert_still_lock_protected(fixture: &Fixture, case: &str, identifier: &str) {
    // Each row is the operation, the verb its refusal states, and its answer.
    for (operation, verb, (status, body)) in [
        (
            "UpdateRule",
            "modify",
            update_rule(
                &fixture.router,
                identifier,
                json!({ "Description": "changed" }),
            )
            .await,
        ),
        (
            "DeleteRule",
            "delete",
            delete_rule(&fixture.router, identifier).await,
        ),
    ] {
        assert_eq!(
            status, 409,
            "case {case:?}: {operation} expected HTTP 409 while the lock protects the rule, \
             got {status} / {body:?}"
        );
        let expected = format!("Cannot {verb} Recycle Bin rule {identifier} because it is locked.");
        assert_eq!(
            message(&body),
            expected,
            "case {case:?}: {operation} expected Message {expected:?}, got {:?}",
            message(&body)
        );
    }
}

#[tokio::test]
async fn a_lock_request_changing_the_delay_still_being_served_is_refused_and_changes_nothing() {
    let fixture = fixture();
    // Each row is the delay in force when the rule was unlocked and the delay a lock
    // request names while that delay is still running: shorter, longer, and one day
    // either side of it, across the published bounds, through both doors a lock arrives
    // through and over every modelled resource type.
    let rows: [(i64, i64); 6] = [
        (UNLOCK_DELAY_MAX, UNLOCK_DELAY_MIN),
        (UNLOCK_DELAY_MIN, UNLOCK_DELAY_MAX),
        (UNLOCK_DELAY_MIDDLE, UNLOCK_DELAY_MIDDLE - 1),
        (UNLOCK_DELAY_MIDDLE, UNLOCK_DELAY_MIDDLE + 1),
        (UNLOCK_DELAY_MAX, UNLOCK_DELAY_MAX - 1),
        (UNLOCK_DELAY_MIN, UNLOCK_DELAY_MIN + 1),
    ];
    for (nth, (in_force, requested)) in rows.into_iter().enumerate() {
        let resource_type = RESOURCE_TYPES[nth % RESOURCE_TYPES.len()];
        let door = if nth % 2 == 0 {
            Door::AtCreate
        } else {
            Door::ByLockRule
        };
        let case = format!(
            "{resource_type} serving a {in_force}-day unlock delay ({door:?}), a lock request \
             asking for {requested} days"
        );
        let (identifier, lock_end) =
            rule_inside_its_unlock_delay(&fixture, &case, resource_type, in_force, door).await;
        // The rule is lock-protected at this very instant: the refusal below is the same
        // predicate, answered for the same rule.
        assert_still_lock_protected(&fixture, &case, &identifier).await;

        let (status, refused) = lock_rule(
            &fixture.router,
            &identifier,
            json!({ "LockConfiguration": lock_configuration(requested) }),
        )
        .await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409, got {status} / {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ConflictException",
            "case {case:?}: expected the modelled ConflictException, got {refused:?}"
        );
        assert_eq!(
            refused["Reason"].as_str(),
            Some("INVALID_RULE_STATE"),
            "case {case:?}: expected Reason INVALID_RULE_STATE, got {:?}",
            refused["Reason"]
        );
        let expected = format!(
            "Cannot lock Recycle Bin rule {identifier} because it has a different unlock \
             delay already set."
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected Message {expected:?}, got {:?}",
            message(&refused)
        );
        // The stored lock-state token is internal and never reaches the caller.
        assert!(
            !message(&refused).contains("pending_unlock"),
            "case {case:?}: the refusal leaked the stored lock-state token: {:?}",
            message(&refused)
        );

        // The refusal left the lock in force exactly as it was: the same delay, the same
        // lock state, and the same end instant — so no caller shortened the protection.
        let (status, fetched) = get_rule(&fixture.router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetRule after the refusal expected HTTP 200, got {status} / {fetched:?}"
        );
        assert_eq!(
            fetched["LockState"].as_str(),
            Some("pending_unlock"),
            "case {case:?}: expected LockState=pending_unlock after the refusal, got {:?}",
            fetched["LockState"]
        );
        assert_eq!(
            fetched["LockConfiguration"],
            lock_configuration(in_force),
            "case {case:?}: expected the stored LockConfiguration to stay at {in_force} days, \
             got {:?}",
            fetched["LockConfiguration"]
        );
        assert_eq!(
            lock_end_seconds(&fetched),
            Some(lock_end),
            "case {case:?}: expected the LockEndTime to stay at {lock_end}, got {:?}",
            fetched["LockEndTime"]
        );
    }
}

#[tokio::test]
async fn a_lock_request_naming_the_delay_being_served_returns_the_rule_to_locked() {
    let fixture = fixture();
    // The documented re-lock: locking during the unlock delay period returns the rule to
    // `locked`. It names the delay the rule is serving, so it changes no lock — asserted
    // at both published bounds and one value between them, through both doors.
    for (nth, unlock_delay) in [UNLOCK_DELAY_MIN, UNLOCK_DELAY_MIDDLE, UNLOCK_DELAY_MAX]
        .into_iter()
        .enumerate()
    {
        for door in [Door::AtCreate, Door::ByLockRule] {
            let resource_type = RESOURCE_TYPES[nth % RESOURCE_TYPES.len()];
            let case = format!(
                "{resource_type} re-locked under the {unlock_delay}-day delay it is serving \
                 ({door:?})"
            );
            let (identifier, _) =
                rule_inside_its_unlock_delay(&fixture, &case, resource_type, unlock_delay, door)
                    .await;

            let (status, relocked) = lock_rule(
                &fixture.router,
                &identifier,
                json!({ "LockConfiguration": lock_configuration(unlock_delay) }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: re-locking expected HTTP 200, got {status} / {relocked:?}"
            );
            assert_eq!(
                relocked["LockState"].as_str(),
                Some("locked"),
                "case {case:?}: expected LockState=locked after re-locking, got {:?}",
                relocked["LockState"]
            );
            // A locked rule is inside no unlock delay period, so it reports no end time.
            let (status, fetched) = get_rule(&fixture.router, &identifier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetRule expected HTTP 200, got {status} / {fetched:?}"
            );
            assert_eq!(
                fetched["LockState"].as_str(),
                Some("locked"),
                "case {case:?}: expected a persisted LockState=locked, got {:?}",
                fetched["LockState"]
            );
            assert_eq!(
                lock_end_seconds(&fetched),
                None,
                "case {case:?}: expected no LockEndTime after re-locking, got {:?}",
                fetched["LockEndTime"]
            );
            assert_eq!(
                fetched["LockConfiguration"],
                lock_configuration(unlock_delay),
                "case {case:?}: expected the stored LockConfiguration to stay at \
                 {unlock_delay} days, got {:?}",
                fetched["LockConfiguration"]
            );
        }
    }
}

#[tokio::test]
async fn another_unlock_delay_is_accepted_once_the_unlock_delay_has_run_out() {
    let fixture = fixture();
    // The complement of the refusal: once the delay the lock was taken under has really
    // expired the rule is `unlocked` and no longer protected, so a lock request may name
    // ANY delay — shorter as well as longer than the one it held.
    let rows: [(i64, i64); 4] = [
        (UNLOCK_DELAY_MAX, UNLOCK_DELAY_MIN),
        (UNLOCK_DELAY_MIN, UNLOCK_DELAY_MAX),
        (UNLOCK_DELAY_MIDDLE, UNLOCK_DELAY_MIDDLE - 1),
        (UNLOCK_DELAY_MIN, UNLOCK_DELAY_MIN),
    ];
    for (nth, (in_force, requested)) in rows.into_iter().enumerate() {
        let resource_type = RESOURCE_TYPES[nth % RESOURCE_TYPES.len()];
        let door = if nth % 2 == 0 {
            Door::ByLockRule
        } else {
            Door::AtCreate
        };
        let case = format!(
            "{resource_type} whose {in_force}-day unlock delay has run out, locked again under \
             {requested} days ({door:?})"
        );
        let (identifier, _) =
            rule_inside_its_unlock_delay(&fixture, &case, resource_type, in_force, door).await;
        // The one thing no request can bring forward: the instant the clock owes the
        // transition at.
        end_unlock_delay(&fixture.engine, &case, &identifier);

        let (status, locked) = lock_rule(
            &fixture.router,
            &identifier,
            json!({ "LockConfiguration": lock_configuration(requested) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: LockRule past the unlock delay expected HTTP 200, got {status} / \
             {locked:?}"
        );
        assert_eq!(
            locked["LockState"].as_str(),
            Some("locked"),
            "case {case:?}: expected LockState=locked, got {:?}",
            locked["LockState"]
        );
        assert_eq!(
            locked["LockConfiguration"],
            lock_configuration(requested),
            "case {case:?}: expected the requested LockConfiguration ({requested} days), got {:?}",
            locked["LockConfiguration"]
        );

        // ROUND TRIP: the new delay is the stored one, and the rule is protected again.
        let (status, fetched) = get_rule(&fixture.router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetRule expected HTTP 200, got {status} / {fetched:?}"
        );
        assert_eq!(
            fetched["LockConfiguration"],
            lock_configuration(requested),
            "case {case:?}: expected the persisted LockConfiguration ({requested} days), got {:?}",
            fetched["LockConfiguration"]
        );
        assert_eq!(
            lock_end_seconds(&fetched),
            None,
            "case {case:?}: expected no LockEndTime on the re-locked rule, got {:?}",
            fetched["LockEndTime"]
        );
        assert_still_lock_protected(&fixture, &case, &identifier).await;
    }
}
