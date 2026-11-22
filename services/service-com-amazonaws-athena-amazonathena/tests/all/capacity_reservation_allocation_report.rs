// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ALLOCATION a capacity reservation reports, and for what a
//! cancellation does to the capacity it requested — driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots, so
//! every assertion is about what an SDK client observes.
//!
//! The rules under test:
//!
//! 1. A reservation reports the allocation its stored instants imply under BOTH names the
//!    wire carries it under — `LastAllocation` and `LastCapacityAllocation` — each with the
//!    allocation's status under both `Status` and `LastAllocationStatus` and the instant it
//!    was submitted under both `RequestTime` and `SubmissionTime`, and it reports
//!    `CapacityLockTime` once the capacity is locked: the instant the most recent allocation
//!    that succeeded completed.
//! 2. A cancellation releases the capacity the reservation requested, so a read reports no
//!    requested amount from the instant the cancellation is asked for, while the allocation
//!    it already completed is still reported.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along what
//! the rules generalise over — the point of the lifecycle the reservation is at, the
//! requested number of data processing units, and whether an allocation is in flight, due,
//! superseded by an update or released by a cancellation. Both readers of a reservation
//! (`GetCapacityReservation` and `ListCapacityReservations`) are asserted to report one
//! answer. Nothing here waits on the wall clock: a reservation that must be at a given point
//! of its lifecycle is seeded with the instants that put it there, and the one test that
//! drives the wire lifecycle waits for the CONDITION it needs.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, CapacityReservationRecord, CAPACITY_ALLOCATION, CAPACITY_ALLOCATION_STATUS_PENDING,
    CAPACITY_ALLOCATION_STATUS_SUCCEEDED, CAPACITY_RESERVATION_STATUS_ACTIVE,
    CAPACITY_RESERVATION_STATUS_CANCELLED, CAPACITY_RESERVATION_STATUS_CANCELLING,
    CAPACITY_RESERVATION_STATUS_PENDING, CAPACITY_RESERVATION_STATUS_UPDATE_PENDING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{await_reservation_status, call, engine_and_router, router};

/// A span long enough that a reservation seeded as still allocating, or still cancelling,
/// stays that way for as long as any test run takes: the transition is due an hour out, so no
/// assertion here depends on how fast the host runs.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The amount a row that says nothing about the amount asks for: inside the modelled
/// `TargetDpusInteger` range (minimum 4) and above its floor.
const SOME_TARGET_DPUS: i32 = 24;

/// `GetCapacityReservation`'s view of one reservation.
async fn read_reservation(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "GetCapacityReservation({name}) must answer 200, got {status} with {body:?}"
    );
    body["CapacityReservation"].clone()
}

/// `ListCapacityReservations`' view of the same reservation.
async fn listed_reservation(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, body) = call(
        router,
        "ListCapacityReservations",
        &json!({ "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListCapacityReservations must answer 200, got {status} with {body:?}"
    );
    body["CapacityReservations"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["Name"].as_str() == Some(name))
                .cloned()
        })
        .unwrap_or_else(|| panic!("the listing must carry {name}, got {body:?}"))
}

/// One seeded reservation: a record written straight into the store with the instants that
/// put it at a given point of its lifecycle, and what the two rules then require of it.
struct Seeded {
    case: &'static str,
    name: &'static str,
    /// The status the stored instants imply.
    status: &'static str,
    /// The number of data processing units a read must report as REQUESTED.
    target_dpus: i32,
    /// The status of the allocation a read must report, under both names it travels under.
    allocation_status: &'static str,
    /// Whether that allocation has completed, so `RequestCompletionTime` reports when.
    allocation_complete: bool,
    /// Whether the reservation's capacity is locked, so `CapacityLockTime` reports when.
    capacity_locked: bool,
}

/// Seed one reservation per point of the lifecycle and hand back what each must be reported
/// as.
fn seed(engine: &engine::InMemoryEngine) -> Vec<Seeded> {
    let now = SystemTime::now();
    let long_ago = now - WELL_PAST_ANY_RUN;
    let store = state::service_state(engine).expect("this service's state");
    let mut held = store.lock();

    // A first allocation still in flight: it is due an hour out, so no capacity is locked
    // yet.
    let mut allocating =
        CapacityReservationRecord::created("allocating".to_string(), 48, Vec::new(), now);
    allocating.allocation_complete_at = now + WELL_PAST_ANY_RUN;
    held.capacity_reservations.insert(allocating);

    // An allocation the clock completed long ago.
    held.capacity_reservations
        .insert(CapacityReservationRecord::created(
            "allocated".to_string(),
            SOME_TARGET_DPUS,
            Vec::new(),
            long_ago,
        ));

    // Allocated once and now allocating a NEW amount: the capacity the first allocation
    // locked is still locked while the second allocation is in flight.
    let mut updating =
        CapacityReservationRecord::created("updating".to_string(), 16, Vec::new(), long_ago);
    updating.request_allocation(96, now);
    updating.allocation_complete_at = now + WELL_PAST_ANY_RUN;
    held.capacity_reservations.insert(updating);

    // Cancelled just now, with the cancellation due an hour out, so it is still cancelling.
    let mut cancelling =
        CapacityReservationRecord::created("cancelling".to_string(), 32, Vec::new(), long_ago);
    cancelling.cancel(now);
    cancelling.cancel_complete_at = Some(now + WELL_PAST_ANY_RUN);
    held.capacity_reservations.insert(cancelling);

    // Cancelled long enough ago that the cancellation is complete, and well inside the
    // documented retention of a cancelled reservation.
    let mut cancelled =
        CapacityReservationRecord::created("cancelled".to_string(), 128, Vec::new(), long_ago);
    cancelled.cancel(long_ago + CAPACITY_ALLOCATION);
    held.capacity_reservations.insert(cancelled);

    vec![
        Seeded {
            case: "a first allocation still in flight",
            name: "allocating",
            status: CAPACITY_RESERVATION_STATUS_PENDING,
            target_dpus: 48,
            allocation_status: CAPACITY_ALLOCATION_STATUS_PENDING,
            allocation_complete: false,
            capacity_locked: false,
        },
        Seeded {
            case: "an allocation the clock completed",
            name: "allocated",
            status: CAPACITY_RESERVATION_STATUS_ACTIVE,
            target_dpus: SOME_TARGET_DPUS,
            allocation_status: CAPACITY_ALLOCATION_STATUS_SUCCEEDED,
            allocation_complete: true,
            capacity_locked: true,
        },
        Seeded {
            case: "a new amount still being allocated",
            name: "updating",
            status: CAPACITY_RESERVATION_STATUS_UPDATE_PENDING,
            target_dpus: 96,
            allocation_status: CAPACITY_ALLOCATION_STATUS_PENDING,
            allocation_complete: false,
            // The capacity the first allocation locked stays locked while the second
            // allocation is in flight.
            capacity_locked: true,
        },
        Seeded {
            case: "a cancellation still in flight",
            name: "cancelling",
            status: CAPACITY_RESERVATION_STATUS_CANCELLING,
            target_dpus: 0,
            allocation_status: CAPACITY_ALLOCATION_STATUS_SUCCEEDED,
            allocation_complete: true,
            capacity_locked: true,
        },
        Seeded {
            case: "a cancellation the clock completed",
            name: "cancelled",
            status: CAPACITY_RESERVATION_STATUS_CANCELLED,
            target_dpus: 0,
            allocation_status: CAPACITY_ALLOCATION_STATUS_SUCCEEDED,
            allocation_complete: true,
            capacity_locked: true,
        },
    ]
}

/// Assert rule 1 of this suite's header over one reported reservation.
fn assert_allocation_report(case: &str, reader: &str, row: &Seeded, reported: &Value) {
    assert_eq!(
        reported["Status"].as_str(),
        Some(row.status),
        "case {case:?} on {reader}: expected the status {}, got {reported:?}",
        row.status
    );

    // The allocation travels under both names the wire carries it under, and both views
    // describe the SAME allocation.
    let modelled = &reported["LastAllocation"];
    let recorded = &reported["LastCapacityAllocation"];
    assert_eq!(
        modelled, recorded,
        "case {case:?} on {reader}: the two views of the allocation must be one allocation, \
         got {modelled:?} and {recorded:?}"
    );

    for (member, view) in [("LastAllocation", modelled), ("LastCapacityAllocation", recorded)] {
        assert!(
            view.is_object(),
            "case {case:?} on {reader}: {member} must report the allocation, got {view:?}"
        );
        // The status under both of the names it travels under.
        for status_member in ["Status", "LastAllocationStatus"] {
            assert_eq!(
                view[status_member].as_str(),
                Some(row.allocation_status),
                "case {case:?} on {reader}: expected {member}.{status_member} {}, got {view:?}",
                row.allocation_status
            );
        }
        // The submission instant under both of the names it travels under.
        let submitted = view["RequestTime"].as_f64();
        assert!(
            submitted.is_some(),
            "case {case:?} on {reader}: {member}.RequestTime must report when the allocation \
             was asked for, got {view:?}"
        );
        assert_eq!(
            view["SubmissionTime"].as_f64(),
            submitted,
            "case {case:?} on {reader}: {member}.SubmissionTime is that same instant, got \
             {view:?}"
        );
        assert_eq!(
            view["RequestCompletionTime"].as_f64().is_some(),
            row.allocation_complete,
            "case {case:?} on {reader}: {member}.RequestCompletionTime is reported exactly \
             when the allocation has completed, got {view:?}"
        );
    }

    // The capacity-lock instant is the completion of the most recent allocation that
    // succeeded: the one this read reports when it has completed, and an earlier one while a
    // new allocation is in flight.
    assert_eq!(
        reported["CapacityLockTime"].as_f64().is_some(),
        row.capacity_locked,
        "case {case:?} on {reader}: CapacityLockTime is reported exactly when the capacity is \
         locked, got {reported:?}"
    );
    if row.allocation_complete {
        assert_eq!(
            reported["CapacityLockTime"].as_f64(),
            modelled["RequestCompletionTime"].as_f64(),
            "case {case:?} on {reader}: the capacity was locked when the allocation it \
             reports completed, got {reported:?}"
        );
    } else if row.capacity_locked {
        let locked = reported["CapacityLockTime"].as_f64();
        assert!(
            locked < modelled["RequestTime"].as_f64(),
            "case {case:?} on {reader}: the capacity locked while a new allocation is in \
             flight was locked by an earlier one, got {reported:?}"
        );
    }
}

/// RULE 1 and the cancellation half of RULE 2, over every point of the lifecycle, through
/// both readers of a reservation.
#[tokio::test]
async fn every_point_of_the_lifecycle_reports_its_allocation() {
    let (engine, router) = engine_and_router();

    for row in seed(&engine) {
        let read = read_reservation(&router, row.name).await;
        assert_allocation_report(row.case, "GetCapacityReservation", &row, &read);

        let listed = listed_reservation(&router, row.name).await;
        assert_allocation_report(row.case, "ListCapacityReservations", &row, &listed);

        assert_eq!(
            read["TargetDpus"].as_i64(),
            Some(i64::from(row.target_dpus)),
            "case {:?}: expected {} requested, got {read:?}",
            row.case,
            row.target_dpus
        );
        assert_eq!(
            read, listed,
            "case {:?}: the read and the listing must report one reservation, got {read:?} \
             and {listed:?}",
            row.case
        );

        // A read changes nothing: a second read reports what the first one did.
        let again = read_reservation(&router, row.name).await;
        assert_eq!(
            again, read,
            "case {:?}: a second read must report what the first one did, got {again:?} \
             after {read:?}",
            row.case
        );
    }
}

/// RULE 2 over the wire, for the amounts the rule generalises over: a cancellation releases
/// the requested capacity whatever amount was requested, and the allocation the reservation
/// completed is still reported afterwards.
#[tokio::test]
async fn a_cancellation_releases_the_requested_capacity() {
    let router = router();

    // (case, the amount the create asks for) — the floor of the modelled range, a middling
    // amount and a large one, since the rule is about the amount being released rather than
    // about its size.
    let cases: [(&str, i32); 3] = [
        ("the smallest amount the model admits", 4),
        ("a middling amount", SOME_TARGET_DPUS),
        ("a large amount", 10_000),
    ];

    for (case, target_dpus) in cases {
        let name = format!("released-{target_dpus}");
        let (status, body) = call(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be accepted, got {status} with {body:?}"
        );

        // Before the cancellation the requested amount is the create's, and the allocation
        // reaches SUCCEEDED when the clock makes it due.
        let allocated =
            await_reservation_status(&router, &name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        assert_eq!(
            allocated["TargetDpus"].as_i64(),
            Some(i64::from(target_dpus)),
            "case {case:?}: an allocated reservation reports the amount asked for, got \
             {allocated:?}"
        );
        assert_eq!(
            allocated["LastCapacityAllocation"]["LastAllocationStatus"].as_str(),
            Some(CAPACITY_ALLOCATION_STATUS_SUCCEEDED),
            "case {case:?}: the allocation completed, got {allocated:?}"
        );
        let locked = allocated["CapacityLockTime"].as_f64();
        assert!(
            locked.is_some(),
            "case {case:?}: an allocated reservation reports when its capacity was locked, \
             got {allocated:?}"
        );

        let (status, body) = call(
            &router,
            "CancelCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the cancellation must be accepted, got {status} with {body:?}"
        );

        let cancelling = read_reservation(&router, &name).await;
        assert_eq!(
            cancelling["TargetDpus"].as_i64(),
            Some(0),
            "case {case:?}: a cancellation releases the requested capacity at once, got \
             {cancelling:?}"
        );
        assert_eq!(
            cancelling["LastAllocation"]["Status"].as_str(),
            Some(CAPACITY_ALLOCATION_STATUS_SUCCEEDED),
            "case {case:?}: the allocation history survives the cancellation, got \
             {cancelling:?}"
        );
        assert_eq!(
            cancelling["CapacityLockTime"].as_f64(),
            locked,
            "case {case:?}: so does the instant the capacity was locked, got {cancelling:?}"
        );

        // And once the cancellation is complete, both are still what they were.
        let cancelled =
            await_reservation_status(&router, &name, CAPACITY_RESERVATION_STATUS_CANCELLED).await;
        assert_eq!(
            cancelled["TargetDpus"].as_i64(),
            Some(0),
            "case {case:?}: a cancelled reservation requests no capacity, got {cancelled:?}"
        );
        assert_eq!(
            cancelled["LastCapacityAllocation"],
            cancelling["LastCapacityAllocation"],
            "case {case:?}: the allocation it completed is unchanged, got {cancelled:?} \
             after {cancelling:?}"
        );
    }
}

/// RULE 1 asserted on the projection itself, across the whole lifecycle of one record and
/// without waiting for any of it: the allocation report follows from the stored instants
/// alone.
#[test]
fn the_allocation_report_follows_the_clock() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let record = CapacityReservationRecord::created("clock".to_string(), 48, Vec::new(), base);

    // (case, how long after the create, the allocation status, whether it has completed)
    let while_allocating: [(&str, Duration, &str, bool); 4] = [
        (
            "the instant of the create",
            Duration::ZERO,
            CAPACITY_ALLOCATION_STATUS_PENDING,
            false,
        ),
        (
            "part way through the allocation",
            CAPACITY_ALLOCATION / 2,
            CAPACITY_ALLOCATION_STATUS_PENDING,
            false,
        ),
        (
            "the instant the allocation is due",
            CAPACITY_ALLOCATION,
            CAPACITY_ALLOCATION_STATUS_SUCCEEDED,
            true,
        ),
        (
            "long after the allocation is due",
            Duration::from_secs(86_400),
            CAPACITY_ALLOCATION_STATUS_SUCCEEDED,
            true,
        ),
    ];

    for (case, elapsed, allocation_status, complete) in while_allocating {
        let now = base + elapsed;
        let reported = record.to_capacity_reservation(now);
        let modelled = reported
            .last_allocation
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: the allocation must be reported"));
        let recorded = reported
            .last_capacity_allocation
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: the allocation must be reported twice"));

        for (member, view) in [("LastAllocation", modelled), ("LastCapacityAllocation", recorded)] {
            assert_eq!(
                view.status, allocation_status,
                "case {case:?}: expected {member}.Status {allocation_status}, got {:?}",
                view.status
            );
            assert_eq!(
                view.last_allocation_status.as_deref(),
                Some(allocation_status),
                "case {case:?}: expected {member}.LastAllocationStatus {allocation_status}, \
                 got {:?}",
                view.last_allocation_status
            );
            assert_eq!(
                view.submission_time, Some(view.request_time),
                "case {case:?}: {member}.SubmissionTime is the instant the allocation was \
                 asked for, got {:?} against {:?}",
                view.submission_time, view.request_time
            );
            assert_eq!(
                view.request_completion_time.is_some(),
                complete,
                "case {case:?}: {member}.RequestCompletionTime is reported exactly once the \
                 allocation has completed, got {:?}",
                view.request_completion_time
            );
        }

        assert_eq!(
            reported.capacity_lock_time,
            modelled.request_completion_time,
            "case {case:?}: the capacity-lock instant is the completion of the allocation \
             reported, got {:?} against {:?}",
            reported.capacity_lock_time,
            modelled.request_completion_time
        );
        assert_eq!(
            reported.capacity_lock_time.is_some(),
            complete,
            "case {case:?}: the capacity is locked exactly once an allocation has completed, \
             got {:?}",
            reported.capacity_lock_time
        );
    }
}
