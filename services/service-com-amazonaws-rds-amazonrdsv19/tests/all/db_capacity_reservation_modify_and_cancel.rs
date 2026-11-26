// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two DB capacity reservation operations that CHANGE one —
//! `ModifyDBCapacityReservation` and `CancelDBCapacityReservation` — and for the ONE
//! sentence every operation that resolves a `DBCapacityReservationIdentifier` refuses a
//! value naming nothing with.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it (through `db_instances::emulator`, the one mounting helper
//! this test target has), and each request is a real awsQuery form POST carrying the SigV4
//! credential scope the router keys on. The assertions read the XML the emulator answers
//! with, so a handler that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! * a well-formed `DBCapacityReservationIdentifier` that names no reservation of the
//!   request's Region is the modelled `DBCapacityReservationNotFoundFault` — HTTP 404, wire
//!   code `DBCapacityReservationNotFound` — worded `The specified DBCapacityReservation
//!   '<the value the request sent>' does not exist.`, the same sentence from every operation
//!   that resolves one, while the report that names no reservation at all still answers 200;
//! * a modification applies the new reserved capacity to every Availability Zone of the
//!   reservation and the expiry its `EndDateType`/`EndDate` pair asks for, reports `pending`
//!   while it is applied and no available capacity while it does, and reaches `active` again
//!   on the clock; a request that asks for nothing, or for an expiry the pair cannot carry,
//!   is refused and changes nothing;
//! * a cancel moves the reservation to `cancelled`, stops reporting available capacity for
//!   it and leaves the record readable; and both calls refuse a reservation whose `State` is
//!   not one they can act in with the modelled
//!   `InvalidDBCapacityReservationStateFault`.
//!
//! The tables below span what those rules generalise over: each of the three operations that
//! take the identifier; a name, a minted-form identifier, the shortest value the member
//! admits, its longest and a hyphenated one; a capacity raised and lowered; an expiry set,
//! replaced and removed; every combination the `EndDateType`/`EndDate` pair can be sent in;
//! and the states a modification and a cancel each refuse.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    DB_CAPACITY_RESERVATION_ID_PREFIX, END_DATE_TYPE_LIMITED, END_DATE_TYPE_UNLIMITED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The state an accepted create of a reservation that starts immediately reports, and the
/// state a reservation reports again while a modification is applied to it.
const PENDING: &str = "pending";

/// The state a reservation Amazon RDS holds the reserved capacity for reports.
const ACTIVE: &str = "active";

/// The state an accepted create of a FUTURE-DATED reservation reports first.
const ASSESSING: &str = "assessing";

/// The state a future-dated reservation reaches once this service has accepted it, in which
/// it waits for its start date.
const SCHEDULED: &str = "scheduled";

/// The state a cancelled reservation reports.
const CANCELLED: &str = "cancelled";

/// The seconds in a day, the unit the dates below are placed in.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// The seconds since the epoch now, the instant the date members below are placed against.
fn now_in_seconds() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock at or after the epoch");
    i64::try_from(since_epoch.as_secs()).expect("an epoch second inside i64")
}

/// `seconds` since the epoch in the UTC form the `TStamp` members take
/// (`2026-12-31T23:59:59Z`).
fn wire_instant(seconds: i64) -> String {
    ::aws_smithy_types::DateTime::from_secs(seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders in the UTC form a TStamp member takes")
}

/// Create one reservation of `capacity` DB instances in the two zones below, named `name`,
/// and answer with the identifier this service minted for it.
///
/// Creates with nothing but the required members, so the reservation starts immediately and
/// the clock owes it `active`.
async fn create_reservation(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    capacity: i32,
) -> String {
    let created = expect_ok(
        case,
        call(
            router,
            "CreateDBCapacityReservation",
            &[
                ("DBCapacityReservationName", name.to_string()),
                ("Engine", "mysql".to_string()),
                ("DBInstanceClass", "db.m5.large".to_string()),
                ("CapacityPerAvailabilityZone", capacity.to_string()),
                (
                    "AvailabilityZones.AvailabilityZone.1",
                    format!("{REGION}a"),
                ),
                (
                    "AvailabilityZones.AvailabilityZone.2",
                    format!("{REGION}b"),
                ),
            ],
        )
        .await,
    );
    let identifier = text(&created, "DBCapacityReservationIdentifier");
    assert!(
        identifier.starts_with(DB_CAPACITY_RESERVATION_ID_PREFIX),
        "case {case:?}: expected a minted identifier, got {identifier:?}"
    );
    identifier
}

/// `DescribeDBCapacityReservations` for the one reservation `named` addresses, as the HTTP
/// status and the body.
async fn read_reservation(
    router: &MultiServiceRouter,
    region: &str,
    named: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "DescribeDBCapacityReservations",
        &[("DBCapacityReservationIdentifier", named)],
    )
    .await
}

/// Poll the report of `named` until it carries `state`, and answer with that body.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    named: &str,
    state: &str,
) -> String {
    let (settled, _) = poll_until(
        case,
        &format!("the reservation in {state} state"),
        || read_reservation(router, REGION, named),
        |status, body| status == 200 && text(body, "State") == state,
    )
    .await;
    settled
}

/// One of the three operations that take a `DBCapacityReservationIdentifier` for one
/// reservation, with the members it needs beside the identifier.
struct ReservationOperation {
    /// What the operation is, for the case names below.
    which: &'static str,
    /// The awsQuery `Action`.
    action: &'static str,
    /// The members the request carries beside the identifier, from the operation's own input
    /// shape: the modification "must specify CapacityPerAvailabilityZone, EndDateType, or
    /// EndDate", and the other two take the identifier alone.
    members: &'static [(&'static str, &'static str)],
}

/// The three operations that take a `DBCapacityReservationIdentifier` for one reservation.
const RESERVATION_OPERATIONS: &[ReservationOperation] = &[
    ReservationOperation {
        which: "the report",
        action: "DescribeDBCapacityReservations",
        members: &[],
    },
    ReservationOperation {
        which: "the modification",
        action: "ModifyDBCapacityReservation",
        members: &[("CapacityPerAvailabilityZone", "2")],
    },
    ReservationOperation {
        which: "the cancel",
        action: "CancelDBCapacityReservation",
        members: &[],
    },
];

#[tokio::test]
async fn every_operation_that_names_a_reservation_words_its_not_found_the_same_way() {
    let router = emulator();

    // One reservation exists, so a value that names nothing is a miss rather than an empty
    // store: the rows below are refused beside a reservation the same calls resolve.
    let identifier = create_reservation(&router, "the stored reservation", "emu-dbcr-stored", 1)
        .await;
    poll_until_state(&router, "the stored reservation", &identifier, ACTIVE).await;

    // The rows differ along what the rule generalises over: a name nothing created, an
    // identifier of the minted form, the shortest value the member admits, its longest
    // (`@length` 1..63) and a hyphenated one, plus a name one character off the stored one.
    let longest = format!("a{}", "b".repeat(62));
    let absent: Vec<(&str, String)> = vec![
        ("a name nothing created", "emu-dbcr-ghost".to_string()),
        (
            "an identifier nothing minted",
            format!("{DB_CAPACITY_RESERVATION_ID_PREFIX}0123456789abcdef"),
        ),
        ("the shortest value the member admits", "a".to_string()),
        ("the longest value the member admits", longest),
        ("a hyphenated value", "emu-dbcr-ghost-seventeen".to_string()),
        (
            "a name one character off a stored one",
            "emu-dbcr-store".to_string(),
        ),
    ];
    for operation in RESERVATION_OPERATIONS {
        for (case, named) in &absent {
            let mut params: Vec<(&str, String)> =
                vec![("DBCapacityReservationIdentifier", named.clone())];
            params.extend(
                operation
                    .members
                    .iter()
                    .map(|(name, value)| (*name, (*value).to_string())),
            );
            let case = format!("{}: {case}", operation.which);
            let refused = expect_error(
                &case,
                call(&router, operation.action, &params).await,
                404,
                "DBCapacityReservationNotFound",
            );
            assert_eq!(
                text(&refused, "Message"),
                format!("The specified DBCapacityReservation '{named}' does not exist."),
                "case {case:?}: expected AWS's not-found sentence naming the value the \
                 request sent, got {refused}"
            );
        }
    }

    // The positive control: the report that names no reservation at all still answers 200,
    // and the reservation the refusals were served beside is untouched.
    let all = expect_ok(
        "the unfiltered report",
        call(
            &router,
            "DescribeDBCapacityReservations",
            &[("MaxRecords", "20")],
        )
        .await,
    );
    assert_eq!(
        texts(&all, "DBCapacityReservationName"),
        vec!["emu-dbcr-stored".to_string()],
        "expected the stored reservation in the unfiltered report, got {all}"
    );
    assert_eq!(
        text(&all, "State"),
        ACTIVE,
        "expected the refused calls to have left the stored reservation active, got {all}"
    );
    assert_eq!(
        text(&all, "ReservedCapacityPerAvailabilityZone"),
        "1",
        "expected the refused calls to have changed no capacity, got {all}"
    );
}

/// One row of the modification table: the members one `ModifyDBCapacityReservation` carries,
/// and what the reservation reports once the modification has settled.
struct ModificationCase {
    case: &'static str,
    /// `CapacityPerAvailabilityZone`, for a row that changes it.
    capacity: Option<i32>,
    /// `EndDateType`, for a row that changes the expiry.
    end_date_type: Option<&'static str>,
    /// How many days ahead the `EndDate` falls, for a row that names one.
    end_date_in_days: Option<i64>,
    /// The capacity the reservation reserves in each zone afterwards.
    reserved: i32,
    /// Whether the reservation reports an `EndDate` afterwards.
    expires: bool,
}

/// The modifications applied in order to ONE reservation, each read back before the next.
///
/// The rows differ along what the rule generalises over: a capacity raised and a capacity
/// lowered; an expiry set, moved and removed; a modification that changes capacity alone, one
/// that changes the expiry alone, and one that changes both.
const MODIFICATIONS: &[ModificationCase] = &[
    ModificationCase {
        case: "a capacity raised on its own",
        capacity: Some(4),
        end_date_type: None,
        end_date_in_days: None,
        reserved: 4,
        expires: false,
    },
    ModificationCase {
        case: "an expiry set on its own",
        capacity: None,
        end_date_type: Some(END_DATE_TYPE_LIMITED),
        end_date_in_days: Some(30),
        reserved: 4,
        expires: true,
    },
    ModificationCase {
        case: "a capacity lowered and the expiry moved together",
        capacity: Some(2),
        end_date_type: Some(END_DATE_TYPE_LIMITED),
        end_date_in_days: Some(60),
        reserved: 2,
        expires: true,
    },
    ModificationCase {
        case: "the expiry removed",
        capacity: None,
        end_date_type: Some(END_DATE_TYPE_UNLIMITED),
        end_date_in_days: None,
        reserved: 2,
        expires: false,
    },
];

#[tokio::test]
async fn a_modification_is_applied_to_every_zone_and_read_back_then_the_cancel_releases_it() {
    let router = emulator();
    let now = now_in_seconds();
    let identifier = create_reservation(&router, "the modified reservation", "emu-dbcr-modify", 1)
        .await;
    poll_until_state(&router, "the modified reservation", &identifier, ACTIVE).await;

    for row in MODIFICATIONS {
        let mut params: Vec<(&str, String)> =
            vec![("DBCapacityReservationIdentifier", identifier.clone())];
        if let Some(capacity) = row.capacity {
            params.push(("CapacityPerAvailabilityZone", capacity.to_string()));
        }
        if let Some(kind) = row.end_date_type {
            params.push(("EndDateType", kind.to_string()));
        }
        let expected_end_date = row
            .end_date_in_days
            .map(|days| wire_instant(now + days * SECONDS_PER_DAY));
        if let Some(end_date) = &expected_end_date {
            params.push(("EndDate", end_date.clone()));
        }
        let modified = expect_ok(
            row.case,
            call(&router, "ModifyDBCapacityReservation", &params).await,
        );
        // "A reservation reports the pending state while Amazon RDS modifies it", and a
        // reservation that is not active reports an available capacity of 0 in every zone.
        assert_eq!(
            text(&modified, "State"),
            PENDING,
            "case {:?}: expected the state a reservation reports while it is modified, got \
             {modified}",
            row.case
        );
        assert_eq!(
            texts(&modified, "AvailableCapacity"),
            vec!["0".to_string(); 2],
            "case {:?}: expected no available capacity while the modification is applied, \
             got {modified}",
            row.case
        );
        assert_eq!(
            text(&modified, "ReservedCapacityPerAvailabilityZone"),
            row.reserved.to_string(),
            "case {:?}: expected the modification's capacity in the answer, got {modified}",
            row.case
        );

        // The reservation reaches `active` again on the clock, and reports the capacity the
        // modification applied to EVERY zone of the reservation as available.
        let settled = poll_until_state(&router, row.case, &identifier, ACTIVE).await;
        assert_eq!(
            text(&settled, "ReservedCapacityPerAvailabilityZone"),
            row.reserved.to_string(),
            "case {:?}: expected the report to read back the modified capacity, got {settled}",
            row.case
        );
        assert_eq!(
            texts(&settled, "AvailableCapacity"),
            vec![row.reserved.to_string(); 2],
            "case {:?}: expected every reserved unit of the active reservation available in \
             both zones, got {settled}",
            row.case
        );
        assert_eq!(
            texts(&settled, "AvailabilityZone"),
            vec![format!("{REGION}a"), format!("{REGION}b")],
            "case {:?}: the zones of a reservation can't be changed, got {settled}",
            row.case
        );
        let reported_end_date = optional_text(&settled, "EndDate");
        match (row.expires, &expected_end_date) {
            (true, Some(end_date)) => assert_eq!(
                reported_end_date.as_ref(),
                Some(end_date),
                "case {:?}: expected the expiry the modification set, got {settled}",
                row.case
            ),
            // `unlimited` is "the reservation doesn't expire", and Amazon RDS "doesn't
            // return this value if the reservation doesn't expire".
            _ => assert_eq!(
                reported_end_date, None,
                "case {:?}: expected no expiry at all, got {settled}",
                row.case
            ),
        }
        // Nothing the modification does not name changes: the identifier, the name, the
        // engine and the class the create resolved are what the report still carries.
        assert_eq!(
            text(&settled, "DBCapacityReservationName"),
            "emu-dbcr-modify",
            "case {:?}: expected the stored name, got {settled}",
            row.case
        );
        assert_eq!(
            text(&settled, "DBInstanceClass"),
            "db.m5.large",
            "case {:?}: the DB instance class of a reservation can't be changed, got {settled}",
            row.case
        );
    }

    // "Amazon RDS changes the state of the reservation to cancelled and stops reporting
    // available capacity for it."
    let cancelled = expect_ok(
        "the cancel",
        call(
            &router,
            "CancelDBCapacityReservation",
            &[("DBCapacityReservationIdentifier", identifier.clone())],
        )
        .await,
    );
    assert_eq!(
        text(&cancelled, "State"),
        CANCELLED,
        "expected the cancel to report the cancelled reservation, got {cancelled}"
    );
    assert_eq!(
        texts(&cancelled, "AvailableCapacity"),
        vec!["0".to_string(); 2],
        "expected a cancelled reservation to report no available capacity, got {cancelled}"
    );
    // The record stays readable, in the state the cancel left it: nothing here removes it.
    let read_back = expect_ok(
        "the cancelled reservation",
        read_reservation(&router, REGION, &identifier).await,
    );
    assert_eq!(
        text(&read_back, "State"),
        CANCELLED,
        "expected the report to read the cancelled reservation back, got {read_back}"
    );
    // A cancelled reservation is past every state either call can act in, and the modelled
    // state fault says so.
    for operation in RESERVATION_OPERATIONS {
        if operation.action == "DescribeDBCapacityReservations" {
            continue;
        }
        let mut params: Vec<(&str, String)> =
            vec![("DBCapacityReservationIdentifier", identifier.clone())];
        params.extend(
            operation
                .members
                .iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        let case = format!("{} of a cancelled reservation", operation.which);
        let refused = expect_error(
            &case,
            call(&router, operation.action, &params).await,
            400,
            "InvalidDBCapacityReservationState",
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(&format!("DBCapacityReservation {identifier} is in {CANCELLED} state")),
            "case {case:?}: expected the fault to name the state the reservation is in, got \
             {message:?}"
        );
    }
}

/// One row of the refused-modification table: the members one request carries beside the
/// identifier, and the refusal it must answer.
struct RefusedModification {
    case: &'static str,
    /// The members the request carries beside `DBCapacityReservationIdentifier`.
    members: &'static [(&'static str, &'static str)],
    /// The wire error `<Code>` of the refusal.
    code: &'static str,
}

/// The modifications a reservation in the one state the call admits still refuses.
///
/// The rows span every combination the `EndDateType`/`EndDate` pair cannot be sent in — the
/// pair is "specified together", `limited` "requires EndDate" and `unlimited` takes none —
/// plus the request that asks for nothing at all, an expiry that is not in the future, and a
/// capacity below the floor the member publishes.
const REFUSED_MODIFICATIONS: &[RefusedModification] = &[
    RefusedModification {
        case: "a request that asks for no modification",
        members: &[],
        code: "InvalidParameterCombination",
    },
    RefusedModification {
        case: "an end date with no end date type",
        members: &[("EndDate", "2031-12-31T23:59:59Z")],
        code: "InvalidParameterCombination",
    },
    RefusedModification {
        case: "a limited expiry with no end date",
        members: &[("EndDateType", END_DATE_TYPE_LIMITED)],
        code: "InvalidParameterCombination",
    },
    RefusedModification {
        case: "an unlimited expiry carrying an end date",
        members: &[
            ("EndDateType", END_DATE_TYPE_UNLIMITED),
            ("EndDate", "2031-12-31T23:59:59Z"),
        ],
        code: "InvalidParameterCombination",
    },
    RefusedModification {
        case: "an end date that is not in the future",
        members: &[
            ("EndDateType", END_DATE_TYPE_LIMITED),
            ("EndDate", "2020-01-01T00:00:00Z"),
        ],
        code: "InvalidParameterValue",
    },
    RefusedModification {
        case: "a capacity that reserves no DB instance",
        members: &[("CapacityPerAvailabilityZone", "0")],
        code: "InvalidParameterValue",
    },
];

#[tokio::test]
async fn a_modification_that_cannot_be_applied_is_refused_and_changes_nothing() {
    let router = emulator();
    let identifier = create_reservation(&router, "the refused modifications", "emu-dbcr-bad", 2)
        .await;
    poll_until_state(&router, "the refused modifications", &identifier, ACTIVE).await;

    for row in REFUSED_MODIFICATIONS {
        let mut params: Vec<(&str, String)> =
            vec![("DBCapacityReservationIdentifier", identifier.clone())];
        params.extend(
            row.members
                .iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        expect_error(
            row.case,
            call(&router, "ModifyDBCapacityReservation", &params).await,
            400,
            row.code,
        );
        // A refused modification leaves the reservation where it was: still active, still
        // reserving what the create reserved, and with no expiry it never had.
        let unchanged = expect_ok(
            row.case,
            read_reservation(&router, REGION, &identifier).await,
        );
        assert_eq!(
            text(&unchanged, "State"),
            ACTIVE,
            "case {:?}: a refused modification must leave the reservation active, got \
             {unchanged}",
            row.case
        );
        assert_eq!(
            text(&unchanged, "ReservedCapacityPerAvailabilityZone"),
            "2",
            "case {:?}: a refused modification must change no capacity, got {unchanged}",
            row.case
        );
        assert_eq!(
            optional_text(&unchanged, "EndDate"),
            None,
            "case {:?}: a refused modification must set no expiry, got {unchanged}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_future_dated_reservation_refuses_both_calls_until_it_starts() {
    let router = emulator();
    let now = now_in_seconds();
    // A future-dated reservation: "Amazon RDS accepted a future-dated reservation, and the
    // reservation is waiting for its start date" — the start and the commitment are at the
    // earliest bound each member publishes, so the reservation is still waiting.
    let created = expect_ok(
        "a future-dated reservation",
        call(
            &router,
            "CreateDBCapacityReservation",
            &[
                ("DBCapacityReservationName", "emu-dbcr-future".to_string()),
                ("Engine", "mysql".to_string()),
                ("DBInstanceClass", "db.m7g.4xlarge".to_string()),
                ("CapacityPerAvailabilityZone", "2".to_string()),
                (
                    "AvailabilityZones.AvailabilityZone.1",
                    format!("{REGION}a"),
                ),
                (
                    "StartDate",
                    wire_instant(now + 5 * SECONDS_PER_DAY + 600),
                ),
                // The shortest commitment the member admits, 14 days.
                ("CommitmentDuration", "1209600".to_string()),
            ],
        )
        .await,
    );
    let identifier = text(&created, "DBCapacityReservationIdentifier");
    assert_eq!(
        text(&created, "State"),
        ASSESSING,
        "expected the state an accepted future-dated create reports, got {created}"
    );

    // While the reservation is waiting for its start date a modification is refused: "a
    // future-dated reservation that hasn't started isn't modifiable". Which of the two
    // waiting states it reports is the clock's business — the sentence states the state the
    // reservation is in and the one state the call admits, whichever that is.
    let refused = expect_error(
        "a modification of a future-dated reservation that has not started",
        call(
            &router,
            "ModifyDBCapacityReservation",
            &[
                ("DBCapacityReservationIdentifier", identifier.clone()),
                ("CapacityPerAvailabilityZone", "4".to_string()),
            ],
        )
        .await,
        400,
        "InvalidDBCapacityReservationState",
    );
    let message = text(&refused, "Message");
    let opening = format!("DBCapacityReservation {identifier} is in ");
    let closing = format!(" state but expected it to be one of {ACTIVE}.");
    assert!(
        message.starts_with(&opening) && message.ends_with(&closing),
        "expected the fault to name the state the reservation is in and the state the call \
         needs, got {message:?}"
    );
    let named_state = message[opening.len()..message.len() - closing.len()].to_string();
    assert!(
        [ASSESSING, SCHEDULED].contains(&named_state.as_str()),
        "expected the refusal to name a state a future-dated reservation waits in, got \
         {named_state:?}"
    );

    // Once this service has accepted it the reservation is `scheduled`, which neither call
    // admits: "you can't cancel a future-dated reservation in the scheduled state".
    poll_until_state(&router, "the scheduled reservation", &identifier, SCHEDULED).await;
    for (case, operation, extra) in [
        (
            "a modification of a scheduled reservation",
            "ModifyDBCapacityReservation",
            vec![("CapacityPerAvailabilityZone", "4".to_string())],
        ),
        (
            "a cancel of a scheduled reservation",
            "CancelDBCapacityReservation",
            Vec::new(),
        ),
    ] {
        let mut params: Vec<(&str, String)> =
            vec![("DBCapacityReservationIdentifier", identifier.clone())];
        params.extend(extra);
        let refused = expect_error(
            case,
            call(&router, operation, &params).await,
            400,
            "InvalidDBCapacityReservationState",
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(&format!(
                "DBCapacityReservation {identifier} is in {SCHEDULED} state"
            )),
            "case {case:?}: expected the fault to name the state the reservation is in, got \
             {message:?}"
        );
        // The refused call changed nothing: the reservation is still scheduled, with the
        // commitment the create granted it.
        let unchanged = expect_ok(case, read_reservation(&router, REGION, &identifier).await);
        assert_eq!(
            text(&unchanged, "State"),
            SCHEDULED,
            "case {case:?}: a refused call must leave the reservation scheduled, got \
             {unchanged}"
        );
        assert_eq!(
            text(&unchanged, "CommittedCapacityPerAvailabilityZone"),
            "2",
            "case {case:?}: a refused call must change no committed capacity, got {unchanged}"
        );
    }
}
