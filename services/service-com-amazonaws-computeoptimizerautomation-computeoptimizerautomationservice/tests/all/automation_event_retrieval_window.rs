// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the retrieval window every reader of the automation
//! event store answers under.
//!
//! The rule: an automation event is reportable only while it was created inside
//! the retrieval window (`ListAutomationEvents` documents "You can retrieve
//! events that were created within the past year"), so an event created before
//! that floor is out of reach of EVERY query of the store — the per-`eventId`
//! readers refuse it with the modelled `ResourceNotFoundException` (404), as
//! they do an id the store never held, and the listings neither report nor count
//! it however far back the caller's start bound points.
//!
//! The floor bounds the `eventId`-scoped WRITE as well: `RollbackAutomationEvent`
//! names the same resource through the same id, so an event before the floor is
//! not found for it either, and the refusal comes before anything is written —
//! no rollback is ever started, and no status, step or client token recorded,
//! for an event no reader of this service can reach.
//!
//! Each case drives the service the way a client does: an awsJson1_0 `POST /`
//! through the same router the emulator binary mounts. The
//! events are SEEDED into the shared store, because an event created through
//! `StartAutomationEvent` is stamped with the instant of the call and no wire
//! call can place one a year in the past.

use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizerautomation_computeoptimizerautomationservice::state::{
    self, StoredEvent, StoredStep, EVENT_RETRIEVAL_WINDOW,
};

use crate::automation_events::{emulator, now_secs, seed, stored_event};
use crate::automation_rule_lifecycle::{assert_refused, call, call_ok, member, string_member};
use crate::recommended_actions;

/// The retrieval window in whole seconds, which is what a case offsets an
/// event's `createdTimestamp` by.
fn window_secs() -> i64 {
    i64::try_from(EVENT_RETRIEVAL_WINDOW.as_secs()).expect("a representable window")
}

/// One row of the table: what a failing assertion calls it, how long before the
/// base instant the event was created, and whether an event created then is
/// still reportable.
struct Row {
    case: &'static str,
    age_secs: i64,
    reachable: bool,
}

/// One seeded event, created `age_secs` before `base`, carrying one step so the
/// step reader has something to report for it.
///
/// `eventType` alternates over the two values the model's `EventType` enum
/// declares (`smithy_query(query="shape",shape="EventType")`:
/// `SnapshotAndDeleteUnattachedEbsVolume` | `UpgradeEbsVolumeType`), and the
/// step type is the one that event type performs, so no row depends on the one
/// pair the issue cited.
fn seeded_event(index: usize, base: i64, age_secs: i64) -> StoredEvent {
    let snapshot_and_delete = index.is_multiple_of(2);
    let (event_type, step_type) = if snapshot_and_delete {
        (
            state::EVENT_TYPE_SNAPSHOT_AND_DELETE_UNATTACHED_EBS_VOLUME,
            state::STEP_TYPE_CREATE_EBS_SNAPSHOT,
        )
    } else {
        (
            state::EVENT_TYPE_UPGRADE_EBS_VOLUME_TYPE,
            state::STEP_TYPE_MODIFY_EBS_VOLUME,
        )
    };
    let stored = stored_event(
        index,
        engine::EMULATOR_ACCOUNT_ID,
        event_type,
        state::EVENT_STATUS_COMPLETE,
        base - age_secs,
    );
    let step = StoredStep {
        step_id: format!("stp{index:013}"),
        step_type: step_type.to_string(),
        step_status: state::STEP_STATUS_COMPLETE.to_string(),
        resource_id: stored.resource_id.clone(),
        start_timestamp: None,
        completed_timestamp: None,
        estimated_monthly_savings: None,
    };
    StoredEvent {
        steps: vec![step],
        ..stored
    }
}

/// The `eventId`s one `ListAutomationEvents` response reports, in response
/// order.
///
/// `pub(crate)` with [`summarised_count`] below, because the sibling suite about
/// the instant this window is judged at reads a response the same way rather
/// than growing a copy.
pub(crate) fn listed_ids(body: &Value) -> Vec<String> {
    member(body, "automationEvents")
        .as_array()
        .unwrap_or_else(|| panic!("automationEvents must be a list, got {body}"))
        .iter()
        .map(|event| string_member(event, "eventId"))
        .collect()
}

/// How many events one `ListAutomationEventSummaries` response counted, over
/// every group it reports.
pub(crate) fn summarised_count(body: &Value) -> i64 {
    let listed = member(body, "automationEventSummaries");
    if listed.is_null() {
        return 0;
    }
    listed
        .as_array()
        .unwrap_or_else(|| panic!("automationEventSummaries must be a list, got {body}"))
        .iter()
        .map(|summary| {
            member(member(summary, "total"), "automationEventCount")
                .as_i64()
                .unwrap_or_default()
        })
        .sum()
}

/// Every reader of the event store agrees about one event: inside the retrieval
/// window it is reportable by id, listable and counted; before the window it is
/// as absent as an id the store never held, to the per-id readers and the
/// listings alike.
#[tokio::test]
async fn every_event_reader_agrees_on_the_retrieval_window() {
    let (router, engine) = emulator();
    let base = now_secs();
    let window = window_secs();

    // The class the rule generalises over is the event's AGE: a fresh event,
    // ages well inside the window, one just inside its far edge, and three ways
    // of being before it (just past the edge, a day past it, and many windows
    // back).
    let rows = vec![
        Row {
            case: "created-at-the-base-instant",
            age_secs: 0,
            reachable: true,
        },
        Row {
            case: "an-hour-old",
            age_secs: 3_600,
            reachable: true,
        },
        Row {
            case: "a-day-old",
            age_secs: 86_400,
            reachable: true,
        },
        Row {
            case: "half-the-window-old",
            age_secs: window / 2,
            reachable: true,
        },
        Row {
            case: "just-inside-the-far-edge",
            age_secs: window - 60,
            reachable: true,
        },
        Row {
            case: "just-outside-the-far-edge",
            age_secs: window + 60,
            reachable: false,
        },
        Row {
            case: "a-day-outside",
            age_secs: window + 86_400,
            reachable: false,
        },
        Row {
            case: "five-windows-old",
            age_secs: 5 * window,
            reachable: false,
        },
        Row {
            case: "created-at-the-epoch",
            age_secs: base,
            reachable: false,
        },
    ];

    let events: Vec<StoredEvent> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| seeded_event(index, base, row.age_secs))
        .collect();
    seed(&engine, &events);

    for (row, event) in rows.iter().zip(&events) {
        let case = row.case;
        let request = json!({"eventId": &event.event_id});
        if row.reachable {
            let reported = call_ok(&router, "GetAutomationEvent", &request, case).await;
            for (field, expected) in [
                ("eventId", event.event_id.clone()),
                ("eventType", event.event_type.clone()),
                ("eventStatus", event.event_status.clone()),
                ("accountId", event.account_id.clone()),
            ] {
                assert_eq!(
                    string_member(&reported, field),
                    expected,
                    "case {case:?}: expected {field} {expected:?}, got {reported}"
                );
            }
            let created = member(&reported, "createdTimestamp").as_f64();
            let expected_created = (base - row.age_secs) as f64;
            assert_eq!(
                created,
                Some(expected_created),
                "case {case:?}: expected createdTimestamp {expected_created}, got {reported}"
            );

            let steps = call_ok(&router, "ListAutomationEventSteps", &request, case).await;
            let step_ids: Vec<String> = member(&steps, "automationEventSteps")
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a step list, got {steps}"))
                .iter()
                .map(|step| string_member(step, "stepId"))
                .collect();
            assert_eq!(
                step_ids,
                vec![event.steps[0].step_id.clone()],
                "case {case:?}: a reportable event's steps are listable, got {steps}"
            );
        } else {
            for operation in ["GetAutomationEvent", "ListAutomationEventSteps"] {
                let refused = call(&router, operation, &request).await;
                assert_refused(refused, 404, "ResourceNotFoundException", case);
            }
        }
    }

    // The id readers and the listings agree: exactly the events inside the
    // window are reported, however far back the caller asks.
    let reachable: Vec<String> = rows
        .iter()
        .zip(&events)
        .filter(|(row, _)| row.reachable)
        .map(|(_, event)| event.event_id.clone())
        .collect();
    for (case, request) in [
        ("listing-with-no-bounds", json!({})),
        (
            "listing-from-the-epoch",
            json!({"startTimeInclusive": 0, "endTimeExclusive": base + 60}),
        ),
        (
            "listing-from-ten-windows-back",
            json!({"startTimeInclusive": base - 10 * window}),
        ),
    ] {
        let listed = call_ok(&router, "ListAutomationEvents", &request, case).await;
        let mut ids = listed_ids(&listed);
        ids.sort();
        let mut expected = reachable.clone();
        expected.sort();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected exactly the events inside the retrieval window, got {listed}"
        );
    }

    let expected_count = i64::try_from(reachable.len()).expect("a representable count");
    for (case, request) in [
        ("summary-with-no-bounds", json!({})),
        (
            "summary-from-the-epoch",
            json!({"startDateInclusive": "1970-01-01"}),
        ),
    ] {
        let summarised = call_ok(&router, "ListAutomationEventSummaries", &request, case).await;
        assert_eq!(
            summarised_count(&summarised),
            expected_count,
            "case {case:?}: expected {expected_count} events counted, got {summarised}"
        );
    }
}

/// Reading a reportable event changes nothing a later caller sees: the same
/// `GetAutomationEvent` answer twice over, and the listings unmoved by the read.
#[tokio::test]
async fn reading_a_reportable_event_leaves_the_store_as_it_was() {
    let (router, engine) = emulator();
    let base = now_secs();
    let window = window_secs();

    // One event at each end of the reportable range, so the read is exercised
    // both on a fresh event and on one at the far edge of the window.
    let events = vec![
        seeded_event(0, base, 0),
        seeded_event(1, base, window - 3_600),
    ];
    seed(&engine, &events);

    let before = call_ok(&router, "ListAutomationEvents", &json!({}), "before").await;
    for event in &events {
        let case = event.event_id.clone();
        let request = json!({"eventId": &event.event_id});
        let first = call_ok(&router, "GetAutomationEvent", &request, &case).await;
        let second = call_ok(&router, "GetAutomationEvent", &request, &case).await;
        assert_eq!(
            first, second,
            "case {case:?}: a read reports the same event twice, got {first} then {second}"
        );
    }
    let after = call_ok(&router, "ListAutomationEvents", &json!({}), "after").await;
    assert_eq!(
        listed_ids(&after),
        listed_ids(&before),
        "case \"after-the-reads\": reading an event by id changes no listing, got {after}"
    );
}

/// An event before the window is refused in exactly the words, code and status
/// an `eventId` the store never held is: the retrieval floor makes an event as
/// absent as one that was never stored, so the refusal discloses nothing about
/// which of the two it was.
#[tokio::test]
async fn an_id_the_store_never_held_is_refused_like_one_outside_the_window() {
    let (router, engine) = emulator();
    let base = now_secs();
    let outside = seeded_event(0, base, window_secs() + 1);
    seed(&engine, std::slice::from_ref(&outside));

    // Well-formed `EventId`s (`^[0-9A-Za-z]{16}$`) no stored event carries,
    // differing in case, digits and letters.
    let cases: Vec<(&str, String)> = vec![
        ("outside-the-window", outside.event_id.clone()),
        ("absent-uppercase", "AAAAAAAAAAAAAAAA".to_string()),
        ("absent-digits", "0000000000000000".to_string()),
        ("absent-mixed", "zZ9aA0bB1cC2dD3e".to_string()),
    ];
    for (case, event_id) in &cases {
        let (status, body) = call(&router, "GetAutomationEvent", &json!({"eventId": event_id})).await;
        assert_refused((status, body.clone()), 404, "ResourceNotFoundException", case);
        assert_eq!(
            string_member(&body, "message"),
            state::RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: every unreachable event is refused in the same words, got {body}"
        );
    }
}

/// One seeded event in a status [`seeded_event`] does not use, for the rows that
/// span the statuses the window floor is judged ahead of.
fn seeded_event_in_status(
    index: usize,
    base: i64,
    age_secs: i64,
    event_status: &str,
) -> StoredEvent {
    StoredEvent {
        event_status: event_status.to_string(),
        ..seeded_event(index, base, age_secs)
    }
}

/// The event the store holds under `event_id`, read where the handlers write it.
///
/// An event before the window is reportable through no operation of this
/// service, so reading the store itself is the only way to assert that a call
/// refused for one wrote NOTHING to it.
fn stored(engine: &engine::InMemoryEngine, event_id: &str) -> StoredEvent {
    let service_state = state::service_state(engine).expect("the service state is reachable");
    let event = service_state.lock().events.get(event_id).cloned();
    event.unwrap_or_else(|| panic!("the seeded event {event_id} is still stored"))
}

/// Re-stamp a stored event's `createdTimestamp` to `created_secs`, which is what
/// the clock eventually does to every event: the window's floor rises past one
/// that was perfectly reportable when it was created. No wire call can do this —
/// an event is stamped with the instant of the call that created it.
fn restamp(engine: &engine::InMemoryEngine, event_id: &str, created_secs: i64) {
    let service_state = state::service_state(engine).expect("the service state is reachable");
    let mut guard = service_state.lock();
    let event = guard
        .events
        .get_mut(event_id)
        .unwrap_or_else(|| panic!("the event {event_id} is stored"));
    event.created_timestamp = DateTime::from_secs(created_secs);
}

/// What one row of the rollback table expects of `RollbackAutomationEvent`.
#[derive(Clone, Copy)]
enum Expected {
    /// The event is reachable by id and its automation was carried out, so the
    /// rollback starts.
    RollbackStarts,
    /// The event is out of reach by id, so it is refused exactly as an id the
    /// store never held is.
    NotFound,
    /// The event is reachable, but in no status a rollback starts from.
    NotRollbackable,
}

/// One row of the rollback table: what a failing assertion calls it, how long
/// before the base instant the event was created, the status it was stored in,
/// and what a rollback of it answers.
struct RollbackRow {
    case: &'static str,
    age_secs: i64,
    event_status: &'static str,
    expected: Expected,
}

/// `RollbackAutomationEvent` reaches exactly the events the readers reach.
///
/// The table spans the event AGE either side of the retrieval floor and, at each
/// side, the event STATUS: before the floor the event is not found whatever
/// status it is in — the floor is judged ahead of the status, because an event
/// no query can reach names no resource to judge — while inside it the completed
/// automation rolls back and every other status is the modelled invalid
/// parameter value. A refused call leaves the stored event exactly as it was
/// seeded, so nothing is written behind a refusal.
#[tokio::test]
async fn a_rollback_reaches_exactly_the_events_the_readers_reach() {
    let (router, engine) = emulator();
    let base = now_secs();
    let window = window_secs();

    // The statuses are the ones this service's own constants name
    // (`smithy_query(query="shape",shape="EventStatus")` declares the whole set,
    // and the sibling rollback suite drives all of it inside the window).
    let rows = vec![
        RollbackRow {
            case: "fresh-complete",
            age_secs: 0,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::RollbackStarts,
        },
        RollbackRow {
            case: "an-hour-old-complete",
            age_secs: 3_600,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::RollbackStarts,
        },
        RollbackRow {
            case: "half-the-window-old-complete",
            age_secs: window / 2,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::RollbackStarts,
        },
        RollbackRow {
            case: "just-inside-the-far-edge-complete",
            age_secs: window - 60,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::RollbackStarts,
        },
        RollbackRow {
            case: "just-outside-the-far-edge-complete",
            age_secs: window + 60,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "a-day-outside-complete",
            age_secs: window + 86_400,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "five-windows-old-complete",
            age_secs: 5 * window,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "created-at-the-epoch-complete",
            age_secs: base,
            event_status: state::EVENT_STATUS_COMPLETE,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "outside-and-running",
            age_secs: window + 3_600,
            event_status: state::EVENT_STATUS_IN_PROGRESS,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "outside-and-failed",
            age_secs: window + 7_200,
            event_status: state::EVENT_STATUS_FAILED,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "outside-and-already-rolled-back",
            age_secs: window + 120,
            event_status: state::EVENT_STATUS_ROLLBACK_COMPLETE,
            expected: Expected::NotFound,
        },
        RollbackRow {
            case: "inside-and-running",
            age_secs: 7_200,
            event_status: state::EVENT_STATUS_IN_PROGRESS,
            expected: Expected::NotRollbackable,
        },
        RollbackRow {
            case: "inside-and-failed",
            age_secs: 14_400,
            event_status: state::EVENT_STATUS_FAILED,
            expected: Expected::NotRollbackable,
        },
        RollbackRow {
            case: "inside-and-already-rolled-back",
            age_secs: 86_400,
            event_status: state::EVENT_STATUS_ROLLBACK_COMPLETE,
            expected: Expected::NotRollbackable,
        },
    ];

    let events: Vec<StoredEvent> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| seeded_event_in_status(index, base, row.age_secs, row.event_status))
        .collect();
    seed(&engine, &events);

    for (row, event) in rows.iter().zip(&events) {
        let case = row.case;
        let request = json!({"eventId": &event.event_id});
        match row.expected {
            Expected::RollbackStarts => {
                let answer = call_ok(&router, "RollbackAutomationEvent", &request, case).await;
                assert_eq!(
                    (
                        string_member(&answer, "eventId"),
                        string_member(&answer, "eventStatus")
                    ),
                    (
                        event.event_id.clone(),
                        state::EVENT_STATUS_ROLLBACK_IN_PROGRESS.to_string()
                    ),
                    "case {case:?}: a rollback of a reachable completed automation starts and \
                     reports it running, got {answer}"
                );

                // The readers agree. The rollback is non-terminal, so the clock
                // may already have settled it by the time this read lands:
                // either rollback status reports the rollback this call started.
                let reported = call_ok(&router, "GetAutomationEvent", &request, case).await;
                let reported_status = string_member(&reported, "eventStatus");
                assert!(
                    [
                        state::EVENT_STATUS_ROLLBACK_IN_PROGRESS,
                        state::EVENT_STATUS_ROLLBACK_COMPLETE
                    ]
                    .contains(&reported_status.as_str()),
                    "case {case:?}: the readers report the rollback this call started, got \
                     {reported_status:?} in {reported}"
                );

                // The rollback appends its own step to the one the seeded
                // automation performed — one step for either event type, per
                // `state::rollback_step_types_for_event_type`.
                let steps = call_ok(&router, "ListAutomationEventSteps", &request, case).await;
                let listed = member(&steps, "automationEventSteps")
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    listed.len(),
                    event.steps.len() + 1,
                    "case {case:?}: the rollback's step is appended to the automation's, got \
                     {steps}"
                );
            }
            Expected::NotFound => {
                let (status, body) = call(&router, "RollbackAutomationEvent", &request).await;
                assert_refused((status, body.clone()), 404, "ResourceNotFoundException", case);
                assert_eq!(
                    string_member(&body, "message"),
                    state::RESOURCE_NOT_FOUND_MESSAGE,
                    "case {case:?}: an event before the floor is refused in the words an id the \
                     store never held is refused in, got {body}"
                );

                // The readers still cannot reach it, and the store holds exactly
                // what was seeded: no status moved, no step appended, no
                // transition armed.
                for operation in ["GetAutomationEvent", "ListAutomationEventSteps"] {
                    assert_refused(
                        call(&router, operation, &request).await,
                        404,
                        "ResourceNotFoundException",
                        case,
                    );
                }
                let after = stored(&engine, &event.event_id);
                assert_eq!(
                    (
                        after.event_status.as_str(),
                        after.steps.len(),
                        after.pending.is_some()
                    ),
                    (row.event_status, event.steps.len(), false),
                    "case {case:?}: a rollback refused for an unreachable event writes nothing, \
                     got {after:?}"
                );
            }
            Expected::NotRollbackable => {
                let (status, body) = call(&router, "RollbackAutomationEvent", &request).await;
                assert_refused(
                    (status, body.clone()),
                    400,
                    "InvalidParameterValueException",
                    case,
                );
                assert_eq!(
                    string_member(&body, "message"),
                    state::rollback_not_allowed_message(),
                    "case {case:?}: a reachable event in another status is refused as an invalid \
                     parameter value, got {body}"
                );
                let reported = call_ok(&router, "GetAutomationEvent", &request, case).await;
                assert_eq!(
                    string_member(&reported, "eventStatus"),
                    row.event_status,
                    "case {case:?}: a refused rollback leaves the event where it was, got \
                     {reported}"
                );
            }
        }
    }
}

/// A rollback refused for an unreachable event records no client token either:
/// the refusal is ahead of every write, so the same token still names no earlier
/// request and a retry is refused in exactly the same words — not as a token
/// already in use or a parameter mismatch. The same token then naming an
/// `eventId` the store never held is answered identically, so the refusal says
/// nothing about which of the two the id was.
#[tokio::test]
async fn a_rollback_for_an_unreachable_event_records_no_client_token() {
    let (router, engine) = emulator();
    let base = now_secs();
    let outside = seeded_event(0, base, window_secs() + 600);
    seed(&engine, std::slice::from_ref(&outside));

    let request = json!({"eventId": &outside.event_id, "clientToken": "window-token"});
    for attempt in ["first-call", "retry-with-the-same-token"] {
        let case = format!("outside-the-window-{attempt}");
        let (status, body) = call(&router, "RollbackAutomationEvent", &request).await;
        assert_refused((status, body.clone()), 404, "ResourceNotFoundException", &case);
        assert_eq!(
            string_member(&body, "message"),
            state::RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: the refused call consumed no token, so the retry is refused the \
             same way, got {body}"
        );
    }

    let absent = json!({"eventId": "Absent0000000001", "clientToken": "window-token"});
    let (status, body) = call(&router, "RollbackAutomationEvent", &absent).await;
    assert_refused(
        (status, body.clone()),
        404,
        "ResourceNotFoundException",
        "an-id-the-store-never-held",
    );
    assert_eq!(
        string_member(&body, "message"),
        state::RESOURCE_NOT_FOUND_MESSAGE,
        "case \"an-id-the-store-never-held\": the same words as an event before the floor, \
         got {body}"
    );

    let after = stored(&engine, &outside.event_id);
    assert_eq!(
        (
            after.event_status.clone(),
            after.steps.len(),
            after.pending.is_some()
        ),
        (outside.event_status.clone(), outside.steps.len(), false),
        "case \"outside-the-window\": no refused call wrote to the event, got {after:?}"
    );
}

/// An idempotent retry answers from the store, so it answers under the same
/// floor: once the floor has risen past the event a client token acted on, the
/// retry is not found rather than reporting an event no reader can reach. Both
/// `eventId`-bearing idempotent operations are driven — the rollback a token
/// started, and the automation a token started.
#[tokio::test]
async fn a_replayed_client_token_answers_under_the_same_floor() {
    let (router, engine) = emulator();
    let base = now_secs();
    let window = window_secs();

    // RollbackAutomationEvent: a completed automation well inside the window is
    // rolled back under a token, and then the floor rises past its event.
    let event = seeded_event(0, base, 3_600);
    seed(&engine, std::slice::from_ref(&event));
    let rollback = json!({"eventId": &event.event_id, "clientToken": "rollback-replay-token"});
    let answer = call_ok(
        &router,
        "RollbackAutomationEvent",
        &rollback,
        "rollback-inside-the-window",
    )
    .await;
    assert_eq!(
        string_member(&answer, "eventStatus"),
        state::EVENT_STATUS_ROLLBACK_IN_PROGRESS,
        "case \"rollback-inside-the-window\": the rollback starts while the event is \
         reachable, got {answer}"
    );
    restamp(&engine, &event.event_id, base - window - 600);
    let (status, body) = call(&router, "RollbackAutomationEvent", &rollback).await;
    assert_refused(
        (status, body.clone()),
        404,
        "ResourceNotFoundException",
        "rollback-replay-after-the-floor-rose",
    );
    assert_eq!(
        string_member(&body, "message"),
        state::RESOURCE_NOT_FOUND_MESSAGE,
        "case \"rollback-replay-after-the-floor-rose\": a replay reports nothing a reader \
         cannot reach, got {body}"
    );

    // StartAutomationEvent: the same rule on the other idempotent operation that
    // answers with an event. Its recommended action is recorded through the
    // suite that owns that fixture (`crate::recommended_actions`), because
    // Compute Optimizer's analysis — not this API — is what produces one.
    recommended_actions::record(
        &engine,
        vec![recommended_actions::stored(
            0,
            "vol-000000000000000w1",
            engine::EMULATOR_ACCOUNT_ID,
            "us-east-1",
            14,
            state::EVENT_TYPE_UPGRADE_EBS_VOLUME_TYPE,
            Some("gp2"),
            false,
            &[],
        )],
    );
    let start = json!({
        "recommendedActionId": recommended_actions::action_id(0),
        "clientToken": "start-replay-token",
    });
    let started = call_ok(&router, "StartAutomationEvent", &start, "start-inside-the-window").await;
    let started_id = string_member(&started, "eventId");
    restamp(&engine, &started_id, base - window - 600);
    let (status, body) = call(&router, "StartAutomationEvent", &start).await;
    assert_refused(
        (status, body.clone()),
        404,
        "ResourceNotFoundException",
        "start-replay-after-the-floor-rose",
    );
    assert_eq!(
        string_member(&body, "message"),
        state::RESOURCE_NOT_FOUND_MESSAGE,
        "case \"start-replay-after-the-floor-rose\": a replay reports nothing a reader cannot \
         reach, got {body}"
    );
    assert_refused(
        call(
            &router,
            "GetAutomationEvent",
            &json!({"eventId": &started_id}),
        )
        .await,
        404,
        "ResourceNotFoundException",
        "start-replay-reader-agrees",
    );
}
