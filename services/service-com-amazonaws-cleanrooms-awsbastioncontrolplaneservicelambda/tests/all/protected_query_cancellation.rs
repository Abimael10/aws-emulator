// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateProtectedQuery`.
//!
//! The rule under test: an operation that moves a protected query to a target status
//! moves it only while the query is still being processed. A query that has not settled
//! takes the request, enters the catalog's transitional status (CANCELLING) and has the
//! target status owed by the clock; a query already in the target status, or already
//! moving to it, is answered with the stored record unchanged, because the model marks
//! the operation idempotent; and a query that settled in any OTHER status is refused as
//! an inconsistent state. Nothing is invented on top of state: the update answers out of
//! the stored record, so `GetProtectedQuery` reports exactly what it returned.
//!
//! The table varies what the rule generalises over — the status the query is in when the
//! request arrives (still being processed, already being cancelled, already cancelled,
//! settled in another status) — so the recorded case the issue cited, a cancel of a
//! freshly started query, is one row among four. The refusal table varies which
//! addressed resource the store does not hold.
//!
//! When a row needs the query in a particular status, it MOVES the instant the clock owes
//! the transition rather than racing it: the span is driven, never waited out, and where
//! a transition has to land the test polls for the condition under a generous deadline.
//!
//! Every request goes over the wire through the same `MultiServiceRouter` the production
//! router uses.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_cleanrooms_awsbastioncontrolplaneservicelambda::state as cleanrooms_state;

use super::cleanrooms_collaboration_lifecycle::{error_code, error_message, Emulator};
use super::protected_queries::{
    get_query, minimal_query, simple_membership, start_query, without_settled_members,
    OTHER_ABSENT_UUID,
};

/// How long a poll loop waits for a transition the clock owes before giving up. The owed
/// span is `cleanrooms_state::PROTECTED_QUERY_SETTLE_DELAY`; this budget is deliberately
/// far larger, so a loaded host cannot fail the test.
const POLL_DEADLINE: Duration = Duration::from_secs(120);
/// How long the poll loop waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How far out a row pushes an owed transition it does not want to fall due yet.
const FAR_FUTURE: Duration = Duration::from_secs(3600);

/// Ask for `target_status` on the protected query `query_id` under `membership_id`.
async fn update_query(
    emulator: &Emulator,
    membership_id: &str,
    query_id: &str,
    target_status: &str,
) -> (u16, Value) {
    emulator
        .call(
            "PATCH",
            &format!("/memberships/{membership_id}/protectedQueries/{query_id}"),
            Some(json!({ "targetStatus": target_status })),
        )
        .await
}

/// Start one protected query under `membership_id` and return its id.
async fn started_query_id(emulator: &Emulator, membership_id: &str) -> String {
    let (status, body) = start_query(emulator, membership_id, minimal_query()).await;
    assert_eq!(
        status, 200,
        "StartProtectedQuery must succeed to set up the case, got {status} with {body}"
    );
    body["protectedQuery"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no protectedQuery.id in {body}"))
        .to_string()
}

/// Move the instant the clock owes this query's terminal status to `due`, so a row
/// decides when that transition falls due instead of racing it. A query that is owed
/// nothing is left alone.
fn reschedule_owed_transition(
    emulator: &Emulator,
    membership_id: &str,
    query_id: &str,
    due: SystemTime,
) {
    let store = emulator.store();
    let mut store = store.lock();
    let record = store
        .protected_query_mut(membership_id, query_id)
        .unwrap_or_else(|| panic!("the store holds no protected query {query_id}"));
    if record.terminal_due.is_some() {
        record.terminal_due = Some(due);
    }
}

/// Push an owed transition out of the way of the next call.
fn defer_owed_transition(emulator: &Emulator, membership_id: &str, query_id: &str) {
    reschedule_owed_transition(
        emulator,
        membership_id,
        query_id,
        SystemTime::now() + FAR_FUTURE,
    );
}

/// Make an owed transition due now, so the next read materializes it.
fn owe_transition_now(emulator: &Emulator, membership_id: &str, query_id: &str) {
    reschedule_owed_transition(emulator, membership_id, query_id, SystemTime::now());
}

/// Read the query until it reports `wanted`, or fail once the deadline has passed. Waits
/// for the CONDITION, never for a fixed time.
async fn poll_until_status(
    case: &str,
    emulator: &Emulator,
    membership_id: &str,
    query_id: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = get_query(emulator, membership_id, query_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetProtectedQuery 200 while waiting for {wanted}, got {body}"
        );
        let query = body["protectedQuery"].clone();
        if query["status"].as_str() == Some(wanted) {
            return query;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the query never reached {wanted}, last read {query}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The status a row leaves the query in before it asks for the target status.
#[derive(Debug, Clone, Copy)]
enum Prior {
    /// Freshly started: the clock owes it the outcome of a run.
    StillProcessing,
    /// A cancellation is already under way.
    BeingCancelled,
    /// The cancellation has settled.
    Cancelled,
    /// Settled in another status: the run the clock owed it has fallen due.
    SettledOtherwise,
}

/// One row of the "which status the query is in when the request arrives" table.
struct UpdateCase {
    label: &'static str,
    prior: Prior,
    /// The HTTP status the update answers with.
    expected_status: u16,
    /// The `ProtectedQueryStatus` the update reports, when it succeeds.
    expected_query_status: Option<&'static str>,
}

fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            label: "a query that is still being processed enters CANCELLING",
            prior: Prior::StillProcessing,
            expected_status: 200,
            expected_query_status: Some(cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLING),
        },
        UpdateCase {
            label: "a query already being cancelled reports the same CANCELLING record",
            prior: Prior::BeingCancelled,
            expected_status: 200,
            expected_query_status: Some(cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLING),
        },
        UpdateCase {
            label: "a query already CANCELLED reports the settled record",
            prior: Prior::Cancelled,
            expected_status: 200,
            expected_query_status: Some(cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED),
        },
        UpdateCase {
            label: "a query settled in another status is an inconsistent state",
            prior: Prior::SettledOtherwise,
            expected_status: 409,
            expected_query_status: None,
        },
    ]
}

/// Start a query and leave it in `prior`, then return its id.
async fn query_in(label: &str, emulator: &Emulator, membership_id: &str, prior: Prior) -> String {
    let query_id = started_query_id(emulator, membership_id).await;
    // Nothing in a row's setup races the clock: the owed transition is parked until the
    // row wants it.
    defer_owed_transition(emulator, membership_id, &query_id);
    let cancel = |query_id: String| async move {
        let (status, body) = update_query(
            emulator,
            membership_id,
            &query_id,
            cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: the first cancel must succeed to set up the row, got {body}"
        );
    };
    match prior {
        Prior::StillProcessing => {}
        Prior::BeingCancelled => {
            cancel(query_id.clone()).await;
            defer_owed_transition(emulator, membership_id, &query_id);
        }
        Prior::Cancelled => {
            cancel(query_id.clone()).await;
            owe_transition_now(emulator, membership_id, &query_id);
            poll_until_status(
                label,
                emulator,
                membership_id,
                &query_id,
                cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
            )
            .await;
        }
        Prior::SettledOtherwise => {
            owe_transition_now(emulator, membership_id, &query_id);
            poll_until_status(
                label,
                emulator,
                membership_id,
                &query_id,
                cleanrooms_state::PROTECTED_QUERY_STATUS_FAILED,
            )
            .await;
        }
    }
    query_id
}

#[tokio::test]
async fn update_protected_query_moves_only_a_query_that_is_still_being_processed() {
    let emulator = Emulator::new();
    let membership = simple_membership(&emulator, "cancel-table").await;
    let membership_id = membership["id"]
        .as_str()
        .expect("membership id")
        .to_string();

    for case in update_cases() {
        let label = case.label;
        let query_id = query_in(label, &emulator, &membership_id, case.prior).await;
        let (_, before_body) = get_query(&emulator, &membership_id, &query_id).await;
        let before = before_body["protectedQuery"].clone();

        let (status, body) = update_query(
            &emulator,
            &membership_id,
            &query_id,
            cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
        )
        .await;
        assert_eq!(
            status, case.expected_status,
            "case {label:?}: expected HTTP {}, got {status} with {body}",
            case.expected_status
        );

        match case.expected_query_status {
            Some(expected) => {
                let updated = body["protectedQuery"].clone();
                assert_eq!(
                    updated["status"].as_str(),
                    Some(expected),
                    "case {label:?}: expected status {expected}, got {updated}"
                );
                // A cancellation is not a failure and ran nothing, so the query carries
                // neither an error nor a result nor statistics.
                for absent in ["error", "result", "statistics"] {
                    assert!(
                        updated.get(absent).is_none(),
                        "case {label:?}: a cancelled query reports no {absent}, got {updated}"
                    );
                }
                // Only the status moves: everything the start call recorded is untouched.
                assert_eq!(
                    without_settled_members(&updated),
                    without_settled_members(&before),
                    "case {label:?}: the update moves only the status, got {updated}"
                );
                // The response came out of the store, so the read agrees with it while
                // the owed transition is still parked.
                defer_owed_transition(&emulator, &membership_id, &query_id);
                let (read_status, read_body) =
                    get_query(&emulator, &membership_id, &query_id).await;
                assert_eq!(
                    read_status, 200,
                    "case {label:?}: expected GetProtectedQuery 200, got {read_body}"
                );
                assert_eq!(
                    read_body["protectedQuery"], updated,
                    "case {label:?}: the read reports the updated query, got {read_body}"
                );
                // Whatever status the request found, the query ends up CANCELLED once the
                // transition the clock owes falls due, and carries no error there.
                owe_transition_now(&emulator, &membership_id, &query_id);
                let settled = poll_until_status(
                    label,
                    &emulator,
                    &membership_id,
                    &query_id,
                    cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
                )
                .await;
                assert!(
                    settled.get("error").is_none(),
                    "case {label:?}: a CANCELLED query carries no error, got {settled}"
                );
            }
            None => {
                assert_eq!(
                    error_code(&body),
                    "ConflictException",
                    "case {label:?}: expected a ConflictException, got {body}"
                );
                assert_eq!(
                    body["reason"].as_str(),
                    Some(cleanrooms_state::CONFLICT_REASON_INVALID_STATE),
                    "case {label:?}: the refusal reports the INVALID_STATE reason, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    cleanrooms_state::protected_query_not_updatable_message(
                        &query_id,
                        before["status"].as_str().expect("the stored status"),
                        cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
                    ),
                    "case {label:?}: unexpected refusal message, got {body}"
                );
                // A refusal changes nothing a later caller sees.
                let (_, read_body) = get_query(&emulator, &membership_id, &query_id).await;
                assert_eq!(
                    read_body["protectedQuery"], before,
                    "case {label:?}: the refused update left the query alone, got {read_body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn repeating_the_update_does_not_postpone_the_transition_the_clock_owes() {
    let emulator = Emulator::new();
    let membership = simple_membership(&emulator, "cancel-idempotence").await;
    let membership_id = membership["id"].as_str().expect("membership id");
    let query_id = started_query_id(&emulator, membership_id).await;
    defer_owed_transition(&emulator, membership_id, &query_id);

    let (status, first) = update_query(
        &emulator,
        membership_id,
        &query_id,
        cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
    )
    .await;
    assert_eq!(status, 200, "expected the cancel to succeed, got {first}");
    let owed_after_first = owed_instant(&emulator, membership_id, &query_id);
    assert!(
        owed_after_first.is_some(),
        "a CANCELLING query is owed its terminal status, got {owed_after_first:?}"
    );

    let (status, second) = update_query(
        &emulator,
        membership_id,
        &query_id,
        cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
    )
    .await;
    assert_eq!(status, 200, "expected the repeat to succeed, got {second}");
    assert_eq!(
        second["protectedQuery"], first["protectedQuery"],
        "the repeat reports the same record, got {second}"
    );
    assert_eq!(
        owed_instant(&emulator, membership_id, &query_id),
        owed_after_first,
        "the repeat left the owed instant where it was, got {second}"
    );
}

/// The instant the clock owes this query's terminal status, as the store holds it.
fn owed_instant(emulator: &Emulator, membership_id: &str, query_id: &str) -> Option<SystemTime> {
    let store = emulator.store();
    let store = store.lock();
    store
        .protected_query(membership_id, query_id)
        .unwrap_or_else(|| panic!("the store holds no protected query {query_id}"))
        .terminal_due
}

#[tokio::test]
async fn update_protected_query_refuses_a_resource_the_store_does_not_hold() {
    let emulator = Emulator::new();
    let membership = simple_membership(&emulator, "cancel-not-found").await;
    let membership_id = membership["id"]
        .as_str()
        .expect("membership id")
        .to_string();
    let query_id = started_query_id(&emulator, &membership_id).await;

    // A second membership, to hold the query the first membership must not be able to
    // move: a query is addressed under the membership that started it.
    let other = simple_membership(&emulator, "cancel-not-found-other").await;
    let other_membership_id = other["id"].as_str().expect("membership id").to_string();
    let other_query_id = started_query_id(&emulator, &other_membership_id).await;
    defer_owed_transition(&emulator, &other_membership_id, &other_query_id);

    let cases: Vec<(&str, String, String, String)> = vec![
        (
            "a membership the store does not hold",
            OTHER_ABSENT_UUID.to_string(),
            query_id.clone(),
            cleanrooms_state::membership_not_found_message(
                OTHER_ABSENT_UUID,
                cleanrooms_state::caller_account_id(),
            ),
        ),
        (
            "a protected query the store does not hold",
            membership_id.clone(),
            OTHER_ABSENT_UUID.to_string(),
            cleanrooms_state::protected_query_not_found_message(&membership_id, OTHER_ABSENT_UUID),
        ),
        (
            "a protected query another membership started",
            membership_id.clone(),
            other_query_id.clone(),
            cleanrooms_state::protected_query_not_found_message(&membership_id, &other_query_id),
        ),
    ];

    for (label, addressed_membership, addressed_query, expected_message) in cases {
        let (status, body) = update_query(
            &emulator,
            &addressed_membership,
            &addressed_query,
            cleanrooms_state::PROTECTED_QUERY_STATUS_CANCELLED,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected HTTP 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {label:?}: expected a ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {label:?}: unexpected refusal message, got {body}"
        );
    }

    // The query the refused calls addressed is untouched: a refusal moves nothing.
    let (status, read_body) = get_query(&emulator, &other_membership_id, &other_query_id).await;
    assert_eq!(
        status, 200,
        "expected GetProtectedQuery 200, got {read_body}"
    );
    assert_eq!(
        read_body["protectedQuery"]["status"].as_str(),
        Some(cleanrooms_state::PROTECTED_QUERY_STATUS_SUBMITTED),
        "a refused update left the other membership's query alone, got {read_body}"
    );
}
