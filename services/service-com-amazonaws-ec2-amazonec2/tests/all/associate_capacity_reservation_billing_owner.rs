// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `AssociateCapacityReservationBillingOwner`, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=AssociateCapacityReservationBillingOwner`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: assigning billing of a Capacity Reservation's unused capacity
//! INITIATES a request — "Initiates a request to assign billing of the unused capacity of a
//! shared Capacity Reservation to a consumer account" — so the call writes the `pending`
//! billing assignment request the store holds for that reservation, initiated BY the calling
//! account and sent TO the consumer account the request names, carrying the resolved
//! reservation's own details, and reports `Return=true`. The reservation has to be one this
//! account holds and one that still HOLDS capacity: an id this operation's validator cannot
//! resolve is refused `InvalidCapacityReservationId.Malformed`, a reservation outside `active`
//! is refused `IncorrectState` naming the state it is in, and neither — nor a dry run —
//! assigns anything.
//!
//! Each table's rows differ along what the rule generalises over: the reservation's instance
//! type, zone, capacity and tenancy, which account the billing is assigned to (another
//! account, a third one, the calling account itself), how many assignments the same
//! reservation has had, the FORM of an id that resolves to nothing, and which state the
//! reservation is in — the modelled `CapacityReservationState` catalog's own members
//! (`ec2::ops::types::CAPACITY_RESERVATION_STATE_VALUES`), so no single recorded request is
//! the specification. What was written is read back through
//! `DescribeCapacityReservationBillingRequests` over the wire, so the assignment is proven by
//! the store rather than by the answer.

use std::time::SystemTime;

use ec2::ops::types::CAPACITY_RESERVATION_STATE_VALUES;
use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::{
    CALLER_ROLE_ODCR_OWNER, CALLER_ROLE_UNUSED_RESERVATION_BILLING_OWNER,
    CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_ACCEPTED,
    CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_PENDING, CAPACITY_RESERVATION_STATE_ACTIVE,
    CAPACITY_RESERVATION_STATE_CANCELLED, CapacityReservationSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, element, elements, error_code, error_message, indexed};
use super::wire::param;

/// The Region every call here is signed for. A billing assignment is between two accounts, so
/// the Region only selects the ec2Query envelope and the zones a reservation can be created
/// in.
const REGION: &str = "us-west-2";

/// Two twelve-digit account ids, the form `AccountID` publishes (`length 12..12`, pattern
/// `^[0-9]{12}$`, `smithy_query(query="shape",shape="AccountID")`), for the consumer side of
/// an assignment.
const CONSUMER: &str = "123456789012";
const ANOTHER_CONSUMER: &str = "210987654321";

/// The platform every reservation in this suite reserves capacity for — a member of the
/// modelled `CapacityReservationInstancePlatform` catalog.
const INSTANCE_PLATFORM: &str = "Linux/UNIX";

/// EC2's answer to an authorized dry run, the status it carries it on, and its whole sentence.
const DRY_RUN_CODE: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// The code this operation's validator answers for every Capacity Reservation id it cannot
/// resolve, and the code EC2 answers for a resource that exists in a state that does not admit
/// the request.
const MALFORMED_CODE: &str = "InvalidCapacityReservationId.Malformed";
const INCORRECT_STATE_CODE: &str = "IncorrectState";

/// The status every refusal in this suite is carried on.
const REFUSAL_STATUS: u16 = 400;

/// One row of the assignment table: the case, the reservation's instance type, its zone, how
/// many instances it reserves, its tenancy (`None` for the member's own default), and the
/// account the billing is assigned to (`None` for the calling account itself).
type AssignmentRow = (
    &'static str,
    &'static str,
    &'static str,
    i32,
    Option<&'static str>,
    Option<&'static str>,
);

/// The account every call in this suite is made by — the emulator's own, which is the Capacity
/// Reservation owner that initiates every request written here.
fn this_account() -> String {
    engine::emulator_account_id().to_string()
}

/// The account one row assigns the billing to: the calling account itself for `None`.
fn party(account: Option<&str>) -> String {
    account.map_or_else(this_account, str::to_string)
}

/// One `AssociateCapacityReservationBillingOwner` call, as its HTTP status and XML body.
async fn associate(
    router: &MultiServiceRouter,
    capacity_reservation_id: &str,
    billing_owner: &str,
    dry_run: bool,
) -> (u16, String) {
    let mut params = vec![
        param("CapacityReservationId", capacity_reservation_id),
        param("UnusedReservationBillingOwnerId", billing_owner),
    ];
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    super::ec2_query::call(
        router,
        REGION,
        "AssociateCapacityReservationBillingOwner",
        &params,
    )
    .await
}

/// An `active` Capacity Reservation created over the wire, as its id and the id of the
/// Availability Zone it reserves capacity in — the zone id the service resolved, so an
/// assertion about what the assignment reports compares against the reservation rather than
/// against a hand-written value.
async fn create_reservation(
    router: &MultiServiceRouter,
    case: &str,
    instance_type: &str,
    availability_zone: &str,
    instance_count: i32,
    tenancy: Option<&str>,
) -> (String, String) {
    let mut params = vec![
        param("InstanceType", instance_type),
        param("InstancePlatform", INSTANCE_PLATFORM),
        param("InstanceCount", &instance_count.to_string()),
        param("AvailabilityZone", availability_zone),
    ];
    if let Some(tenancy) = tenancy {
        params.push(param("Tenancy", tenancy));
    }
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateCapacityReservation", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reserving capacity must answer HTTP 200, got {status} with body {body}"
    );
    let id = element(&body, "capacityReservationId")
        .unwrap_or_else(|| panic!("case {case:?}: no reservation id in {body}"));
    let zone_id = element(&body, "availabilityZoneId")
        .unwrap_or_else(|| panic!("case {case:?}: no zone id in {body}"));
    (id, zone_id)
}

/// What `DescribeCapacityReservationBillingRequests` reports to `role` for the reservations
/// `ids` name, as its XML body — asserted to be served.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    ids: &[&str],
) -> String {
    let mut params = vec![param("Role", role)];
    params.extend(indexed("CapacityReservationId", ids));
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeCapacityReservationBillingRequests",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: describing must answer HTTP 200, got {status} with body {body}"
    );
    body
}

/// How many billing assignment requests the store holds, read straight out of the engine — for
/// the assertions that need to know only that nothing was written.
fn stored_requests(engine: &engine::InMemoryEngine) -> usize {
    engine
        .state::<Ec2State>()
        .lock()
        .capacity_reservation_billing_requests
        .len()
}

/// The status the store holds for one reservation's billing request, or `None` when it holds
/// no request for it at all.
fn stored_status(engine: &engine::InMemoryEngine, capacity_reservation_id: &str) -> Option<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .capacity_reservation_billing_requests
        .get(capacity_reservation_id)
        .and_then(|request| request.status.clone())
}

/// One Capacity Reservation written straight into the engine's store in `state`.
///
/// Only for the states no implemented operation leaves a reservation in — a `failed` or an
/// `expired` one. A reservation that is `active` is created over the wire
/// ([`create_reservation`]) and a `cancelled` one through `CancelCapacityReservation`, so each
/// of those rows exercises the state an operation really produces.
fn seed_reservation(engine: &engine::InMemoryEngine, state: &'static str) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_capacity_reservation(CapacityReservationSpec {
            instance_type: "m5.large".to_string(),
            instance_platform: INSTANCE_PLATFORM.to_string(),
            availability_zone: format!("{REGION}a"),
            availability_zone_id: "usw2-az1".to_string(),
            region: REGION.to_string(),
            tenancy: "default".to_string(),
            instance_count: 2,
            ebs_optimized: false,
            ephemeral_storage: false,
            end_date: None,
            instance_match_criteria: "open".to_string(),
            created_at: SystemTime::now(),
            // Capacity for immediate use: no requested delivery date, nothing committed to and
            // no delivery method — the members only a future-dated request names.
            start_date: None,
            commitment: None,
            delivery_preference: None,
            state,
            tags: Vec::new(),
            interruption_info: None,
            zero_size_preference: None,
            pending: None,
            capacity_reservation_fleet_id: None,
        })
        .expect("the store accepts a seeded Capacity Reservation")
}

// ---------------------------------------------------------------------------
// The assignment writes the request the describe reports
// ---------------------------------------------------------------------------

/// An assignment of a reservation this account holds writes the `pending` request the describe
/// reports, whatever the reservation reserves and whichever account the billing is assigned to.
#[tokio::test]
async fn an_assignment_writes_the_pending_request_the_describe_reports() {
    // The rows differ along the reservation the billing belongs to — its instance type, zone,
    // capacity and tenancy — and along the account the billing is assigned to, including the
    // calling account itself.
    let rows: [AssignmentRow; 4] = [
        (
            "one instance of a general purpose type, to another account",
            "m5.large",
            "us-west-2a",
            1,
            None,
            Some(CONSUMER),
        ),
        (
            "a dozen instances on dedicated hardware, to a third account",
            "c5.4xlarge",
            "us-west-2b",
            12,
            Some("dedicated"),
            Some(ANOTHER_CONSUMER),
        ),
        (
            "a burstable reservation on shared hardware",
            "t3.micro",
            "us-west-2c",
            3,
            Some("default"),
            Some(ANOTHER_CONSUMER),
        ),
        (
            "a reservation whose billing is assigned to the calling account itself",
            "r5.2xlarge",
            "us-west-2a",
            2,
            None,
            None,
        ),
    ];

    for (case, instance_type, zone, instance_count, tenancy, consumer) in rows {
        let (router, engine) = super::ec2_query::emulator();
        let (reservation_id, zone_id) = create_reservation(
            &router,
            case,
            instance_type,
            zone,
            instance_count,
            tenancy,
        )
        .await;
        let billing_owner = party(consumer);

        let (status, body) = associate(&router, &reservation_id, &billing_owner, false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected the assignment to report Return=true, got {body}"
        );

        // Read back through the describe, as the OWNER that initiated the request.
        let reported = described(
            &router,
            case,
            CALLER_ROLE_ODCR_OWNER,
            &[reservation_id.as_str()],
        )
        .await;
        let reported_ids = elements(&reported, "capacityReservationId");
        assert_eq!(
            reported_ids,
            vec![reservation_id.clone()],
            "case {case:?}: expected exactly the assigned reservation's request, got {reported}"
        );
        let owner_account = this_account();
        let members = [
            ("status", CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_PENDING),
            ("requestedBy", owner_account.as_str()),
            ("unusedReservationBillingOwnerId", billing_owner.as_str()),
            // "Information about the Capacity Reservation" is the reservation's own, so each
            // member is compared against what the reservation reserves rather than against the
            // request's own values.
            ("instanceType", instance_type),
            ("availabilityZone", zone),
            ("availabilityZoneId", zone_id.as_str()),
            ("tenancy", tenancy.unwrap_or("default")),
        ];
        for (member, expected) in members {
            let actual = element(&reported, member);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected {member} to be {expected:?}, got {actual:?} in \
                 {reported}"
            );
        }
        // "When the request was last updated" is a timestamp of the call, so it reads as one.
        let last_update_time = element(&reported, "lastUpdateTime").unwrap_or_default();
        assert!(
            as_timestamp(&last_update_time).is_some(),
            "case {case:?}: expected a timestamp for lastUpdateTime, got \
             {last_update_time:?} in {reported}"
        );
        // One assignment, one stored request.
        let held = stored_requests(&engine);
        assert_eq!(
            held, 1,
            "case {case:?}: expected exactly one stored billing request, got {held}"
        );
    }
}

/// A SECOND assignment of the same reservation replaces the request the first one made: one
/// reservation's unused capacity is billed to one account, so the later call is the one that
/// stands.
#[tokio::test]
async fn assigning_the_same_reservation_again_replaces_the_request() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "the same reservation assigned twice";
    let (reservation_id, _) =
        create_reservation(&router, case, "m5.large", "us-west-2a", 4, None).await;

    // The rows differ along how many assignments the reservation has already had: each one
    // leaves exactly one request outstanding, carrying the account IT named.
    let mut previous_update: Option<f64> = None;
    for (attempt, consumer) in [CONSUMER, ANOTHER_CONSUMER, CONSUMER].iter().enumerate() {
        let case = format!("assignment {} to {consumer}", attempt + 1);
        let (status, body) = associate(&router, &reservation_id, consumer, false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = described(
            &router,
            &case,
            CALLER_ROLE_ODCR_OWNER,
            &[reservation_id.as_str()],
        )
        .await;
        assert_eq!(
            elements(&reported, "capacityReservationId"),
            vec![reservation_id.clone()],
            "case {case:?}: expected one request for the reservation, got {reported}"
        );
        assert_eq!(
            element(&reported, "unusedReservationBillingOwnerId").as_deref(),
            Some(*consumer),
            "case {case:?}: expected the account this assignment named, got {reported}"
        );
        assert_eq!(
            element(&reported, "status").as_deref(),
            Some(CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_PENDING),
            "case {case:?}: expected the replaced request to be pending, got {reported}"
        );
        let held = stored_requests(&engine);
        assert_eq!(
            held, 1,
            "case {case:?}: expected the assignment to replace rather than add a request, \
             got {held} stored"
        );
        // "When the request was last updated": each assignment records its own instant, which
        // cannot predate the one before it.
        let update = as_timestamp(&element(&reported, "lastUpdateTime").unwrap_or_default())
            .unwrap_or_else(|| panic!("case {case:?}: expected a timestamp in {reported}"));
        if let Some(previous) = previous_update {
            assert!(
                update >= previous,
                "case {case:?}: expected lastUpdateTime {update} not to predate the \
                 previous {previous}"
            );
        }
        previous_update = Some(update);
    }
}

/// The request this call writes is the one the consumer account answers: assigning the billing
/// to the calling account and then accepting it moves that request to `accepted`.
#[tokio::test]
async fn the_request_an_assignment_writes_is_the_one_the_consumer_accepts() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "an assignment the consumer account accepts";
    let (reservation_id, _) =
        create_reservation(&router, case, "m5.large", "us-west-2a", 2, None).await;
    let (status, body) = associate(&router, &reservation_id, &this_account(), false).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );

    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "AcceptCapacityReservationBillingOwnership",
        &[param("CapacityReservationId", &reservation_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: accepting must answer HTTP 200, got {status} with body {body}"
    );

    let reported = described(
        &router,
        case,
        CALLER_ROLE_UNUSED_RESERVATION_BILLING_OWNER,
        &[reservation_id.as_str()],
    )
    .await;
    assert_eq!(
        element(&reported, "status").as_deref(),
        Some(CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_ACCEPTED),
        "case {case:?}: expected the accepted request, got {reported}"
    );
    assert_eq!(
        stored_status(&engine, &reservation_id).as_deref(),
        Some(CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_ACCEPTED),
        "case {case:?}: expected the store to hold the accepted request"
    );
}

// ---------------------------------------------------------------------------
// An id this validator cannot resolve, and a reservation holding no capacity
// ---------------------------------------------------------------------------

/// Every Capacity Reservation id this operation cannot resolve is refused as malformed —
/// whatever its form — and nothing is assigned.
#[tokio::test]
async fn an_id_this_validator_cannot_resolve_is_refused_malformed() {
    let (router, engine) = super::ec2_query::emulator();
    // One reservation the account DOES hold, so a refusal below is about the id the row names
    // rather than about an empty store.
    let (held, _) = create_reservation(
        &router,
        "the reservation the account holds",
        "m5.large",
        "us-west-2a",
        1,
        None,
    )
    .await;
    let reservations = engine.state::<Ec2State>().lock().capacity_reservations.len();

    // The rows differ in the FORM of the id: the long form EC2 issues but this account holds
    // nothing under, the all-zero placeholder, a leading digit above zero, upper-cased digits,
    // the short form, another resource's id, and a value that is not an id at all.
    let rows = [
        ("the long form, naming no reservation", "cr-0123456789abcdef0"),
        ("another unheld long form", "cr-0fedcba987654321f"),
        ("the all-zero placeholder", "cr-00000000000000000"),
        ("a leading digit above zero", "cr-9123456789abcdef0"),
        ("upper-cased digits", "cr-0123456789ABCDEF0"),
        ("the short id form", "cr-01234567"),
        ("a digit outside hexadecimal", "cr-0123456789abcdefg"),
        ("another resource's id", "i-0123456789abcdef0"),
        ("not an id at all", "capacity-reservation"),
    ];

    for (case, id) in rows {
        let (status, body) = associate(&router, id, CONSUMER, false).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            MALFORMED_CODE,
            "case {case:?}: expected {MALFORMED_CODE}, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Capacity Reservation ID '{id}' is malformed"),
            "case {case:?}: expected EC2's wording for a malformed id, got body {body}"
        );
        let stored = stored_requests(&engine);
        assert_eq!(
            stored, 0,
            "case {case:?}: a refused assignment must store nothing, got {stored} requests"
        );
    }

    // The reservation the account holds is untouched, and assigning ITS billing still works.
    let after = engine.state::<Ec2State>().lock().capacity_reservations.len();
    assert_eq!(
        after, reservations,
        "the refusals must leave the reservation store as it was: held {reservations}, now \
         {after}"
    );
    let (status, body) = associate(&router, &held, CONSUMER, false).await;
    assert_eq!(
        status, 200,
        "the reservation the account holds must still be assignable, got {status} with \
         body {body}"
    );
}

/// A reservation that holds no capacity has no unused capacity to assign: every state outside
/// `active` is refused for the state it is in, and nothing is assigned.
#[tokio::test]
async fn a_reservation_outside_active_is_refused_for_its_state() {
    // The rows are the modelled `CapacityReservationState` catalog's own members, minus the one
    // state the assignment is admitted in, so a member the model adds later is covered too.
    for state in CAPACITY_RESERVATION_STATE_VALUES
        .iter()
        .filter(|state| **state != CAPACITY_RESERVATION_STATE_ACTIVE)
    {
        let (router, engine) = super::ec2_query::emulator();
        // The catalog is a slice of `&'static str`, which is what the store stores.
        let reservation_id = seed_reservation(&engine, state);
        let (status, body) = associate(&router, &reservation_id, CONSUMER, false).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {state:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INCORRECT_STATE_CODE,
            "case {state:?}: expected {INCORRECT_STATE_CODE}, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Capacity Reservation '{reservation_id}' is in the '{state}' state."),
            "case {state:?}: expected EC2's wording for the state a resource is in, got \
             body {body}"
        );
        let stored = stored_requests(&engine);
        assert_eq!(
            stored, 0,
            "case {state:?}: a refused assignment must store nothing, got {stored} requests"
        );
    }
}

/// The same refusal for the state an implemented operation really leaves a reservation in: a
/// cancelled reservation's billing cannot be assigned, where the same reservation's could be
/// before it was cancelled.
#[tokio::test]
async fn a_cancelled_reservation_is_refused_for_its_state() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "a reservation cancelled over the wire";
    let (reservation_id, _) =
        create_reservation(&router, case, "m5.large", "us-west-2a", 1, None).await;

    let (status, body) = associate(&router, &reservation_id, CONSUMER, false).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the active reservation must be assignable, got {status} with \
         body {body}"
    );

    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "CancelCapacityReservation",
        &[param("CapacityReservationId", &reservation_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: cancelling must answer HTTP 200, got {status} with body {body}"
    );

    let (status, body) = associate(&router, &reservation_id, ANOTHER_CONSUMER, false).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INCORRECT_STATE_CODE,
        "case {case:?}: expected {INCORRECT_STATE_CODE}, got body {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Capacity Reservation '{reservation_id}' is in the \
             '{CAPACITY_RESERVATION_STATE_CANCELLED}' state."
        ),
        "case {case:?}: expected EC2's wording for the state a resource is in, got body {body}"
    );
    // The request the first assignment made is the one the store still holds: the refused
    // second assignment wrote nothing over it.
    assert_eq!(
        stored_status(&engine, &reservation_id).as_deref(),
        Some(CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_PENDING),
        "case {case:?}: expected the first assignment's request to stand"
    );
    let reported = described(
        &router,
        case,
        CALLER_ROLE_ODCR_OWNER,
        &[reservation_id.as_str()],
    )
    .await;
    assert_eq!(
        element(&reported, "unusedReservationBillingOwnerId").as_deref(),
        Some(CONSUMER),
        "case {case:?}: expected the first assignment's consumer account, got {reported}"
    );
}

// ---------------------------------------------------------------------------
// A dry run assigns nothing
// ---------------------------------------------------------------------------

/// `DryRun` is answered once the id's FORM has been read and before anything is resolved: a
/// reservation the account holds and one it does not are both told the request would have
/// succeeded, a value EC2 could never have issued is still refused, and no dry run assigns
/// anything.
#[tokio::test]
async fn a_dry_run_answers_the_verdict_and_assigns_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let (reservation_id, _) = create_reservation(
        &router,
        "the reservation a dry run must leave alone",
        "m5.large",
        "us-west-2a",
        1,
        None,
    )
    .await;

    // (case, the id the dry run names, the status and code it is answered with)
    let rows = [
        (
            "a reservation the account holds",
            reservation_id.clone(),
            DRY_RUN_STATUS,
            DRY_RUN_CODE,
        ),
        (
            "a well-formed id the account holds no reservation under",
            "cr-0fedcba987654321f".to_string(),
            DRY_RUN_STATUS,
            DRY_RUN_CODE,
        ),
        (
            "a value EC2 could never have issued",
            "cr-nonsense".to_string(),
            REFUSAL_STATUS,
            MALFORMED_CODE,
        ),
    ];

    // Rows differing along how many dry runs have already been answered: each asks about the
    // caller, so answering one assigns nothing and changes no later answer.
    for attempt in 1..=2 {
        for (case, id, expected_status, expected_code) in &rows {
            let case = format!("{case} (attempt {attempt})");
            let (status, body) = associate(&router, id, CONSUMER, true).await;
            assert_eq!(
                status, *expected_status,
                "case {case:?}: expected HTTP {expected_status} under the flag, got {status} \
                 with body {body}"
            );
            assert_eq!(
                error_code(&body),
                *expected_code,
                "case {case:?}: expected {expected_code} under the flag, got body {body}"
            );
            if *expected_code == DRY_RUN_CODE {
                assert_eq!(
                    error_message(&body),
                    DRY_RUN_MESSAGE,
                    "case {case:?}: expected EC2's own dry-run wording, got body {body}"
                );
            }
            let stored = stored_requests(&engine);
            assert_eq!(
                stored, 0,
                "case {case:?}: a dry run must assign nothing, got {stored} stored requests"
            );
        }
    }

    // And the same call without the flag is the one that makes the assignment.
    let (status, body) = associate(&router, &reservation_id, CONSUMER, false).await;
    assert_eq!(
        status, 200,
        "the call without the flag must answer HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        stored_status(&engine, &reservation_id).as_deref(),
        Some(CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_PENDING),
        "the call without the flag must leave the pending request in the store"
    );
}
