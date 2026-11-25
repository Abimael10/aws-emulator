// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a `CancelCapacityReservation` CHARGES for: whether the
//! cancellation of a Capacity Reservation has to apply a charge decides both what the request
//! must carry and the state the reservation is reported in afterwards. Driven over the wire
//! the service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//! ec2Query is selected by the `ec2` signing name in the request's SigV4 credential scope, so
//! every call here carries one.
//!
//! The rules under test:
//!
//! 1. A cancel whose charges have to be APPLIED does not reach the terminal state in the same
//!    instant: "The Capacity Reservation transitions to `cancelling` while charges are
//!    applied" (`CancelCapacityReservation`), so the reservation is written in that state with
//!    the move to `cancelled` owed to the clock, and reaches `cancelled` from the instant that
//!    move falls due whether or not anybody calls again. Neither state admits another cancel.
//! 2. A cancel with NOTHING to charge for ends in `cancelled` straight away — "Cancels the
//!    specified Capacity Reservation, releases the reserved capacity, and changes the Capacity
//!    Reservation's state to `cancelled`" (same doc) — and leaves the clock owing the
//!    reservation nothing: a reservation that committed to nothing, one whose "commitment
//!    duration has elapsed", and one whose commitment the state it is in waives.
//! 3. Which of the two a request is, is a fact of the reservation rather than of the request:
//!    a reservation whose cancellation is charged for is refused `MissingParameter` until the
//!    request carries the quote and the charge type ("Use
//!    CreateCapacityReservationCancellationQuote to generate a quote, then pass the quote ID
//!    with ApplyCancellationCharges set to commitment-wind-down"), while one in the state that
//!    waives the commitment — "`delayed` — the commitment duration is waived, so no
//!    cancellation charge applies" — is cancelled with neither.
//! 4. A charged cancel of a reservation that has capacity allocated out of it releases that
//!    allocation too, and both ends fall due together: the reservation reaches `cancelled` and
//!    the allocation the same call released reaches its own terminal status with nothing
//!    allocated to it, with the capacity back in the reservation that reserved it.
//!
//! Every table row is a distinct input along what those rules generalise over: how much
//! capacity the reservation holds, how far ahead it starts, how long it committed for, how far
//! past its commitment it is, and which `CapacityReservationState` it is in when the cancel
//! arrives.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::{
    APPLY_CANCELLATION_CHARGES_COMMITMENT_WIND_DOWN, CAPACITY_RESERVATION_STATE_ACTIVE,
    CAPACITY_RESERVATION_STATE_ASSESSING, CAPACITY_RESERVATION_STATE_CANCELLED,
    CAPACITY_RESERVATION_STATE_CANCELLING, CAPACITY_RESERVATION_STATE_DELAYED,
    CAPACITY_RESERVATION_STATE_SCHEDULED, CapacityReservationCommitment, CapacityReservationSpec,
    INTERRUPTIBLE_ALLOCATION_STATUS_ACTIVE, INTERRUPTIBLE_ALLOCATION_STATUS_CANCELED,
    INTERRUPTIBLE_ALLOCATION_STATUS_CANCELING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::capacity_reservation_cancellation_quotes::{
    ReservationCase, create_future_dated, quote_params, reported_quote, start_date,
};
use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::param;

/// The Region every call here is signed for, and the zone in it the reserved capacity sits in.
const REGION: &str = "us-east-1";
const AVAILABILITY_ZONE: &str = "us-east-1a";
const AVAILABILITY_ZONE_ID: &str = "use1-az1";

/// The platform and tenancy every reservation here reserves capacity for: members of
/// `CapacityReservationInstancePlatform` and `CapacityReservationTenancy`
/// (`smithy_query(query="shape",shape="CapacityReservationInstancePlatform")`).
const INSTANCE_PLATFORM: &str = "Linux/UNIX";
const TENANCY: &str = "default";

/// The match criteria a future-dated reservation must carry: "If you are requesting a
/// future-dated Capacity Reservation, you must specify `targeted`"
/// (`CreateCapacityReservationRequest$InstanceMatchCriteria`).
const INSTANCE_MATCH_CRITERIA_TARGETED: &str = "targeted";

/// "The only supported value is `incremental`"
/// (`CreateCapacityReservationRequest$DeliveryPreference`).
const DELIVERY_PREFERENCE: &str = "incremental";

/// How long a poll for a clock-owed move is given before the test gives up: far longer than
/// any span this emulator owes one, so a loaded host cannot fail a test that a fast one
/// passes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The rows the wind-down rule is asserted over. They differ along what it generalises over:
/// how much capacity the reservation holds, how far ahead it starts and how long it committed
/// for — the shared future-dated row of this crate's cancellation-quote suites.
fn charged_rows() -> Vec<ReservationCase> {
    vec![
        ReservationCase {
            name: "one instance, five days out, a one-day commitment",
            instance_type: "c5.xlarge",
            instance_count: 1,
            lead_days: 5,
            commitment_seconds: 24 * 60 * 60,
            quote_tags: &[],
        },
        ReservationCase {
            name: "thirty-two instances, forty days out, a fortnight's commitment",
            instance_type: "m5.large",
            instance_count: 32,
            lead_days: 40,
            commitment_seconds: 14 * 24 * 60 * 60,
            quote_tags: &[("Team", "research")],
        },
        ReservationCase {
            name: "the furthest-ahead start and a year's commitment",
            instance_type: "r5.large",
            instance_count: 7,
            lead_days: 119,
            commitment_seconds: 365 * 24 * 60 * 60,
            quote_tags: &[],
        },
    ]
}

/// One `CancelCapacityReservation` call for `reservation_id`, carrying the quote and the one
/// charge type the model declares when `quote_id` is given and neither when it is not.
async fn cancel(
    router: &MultiServiceRouter,
    reservation_id: &str,
    quote_id: Option<&str>,
) -> (u16, String) {
    let mut params = vec![param("CapacityReservationId", reservation_id)];
    if let Some(quote_id) = quote_id {
        params.push(param("QuoteId", quote_id));
        params.push(param(
            "ApplyCancellationCharges",
            APPLY_CANCELLATION_CHARGES_COMMITMENT_WIND_DOWN,
        ));
    }
    super::ec2_query::call(router, REGION, "CancelCapacityReservation", &params).await
}

/// One `CreateCapacityReservationCancellationQuote` for `reservation_id`, answering the id of
/// the quote it minted.
async fn mint_quote(router: &MultiServiceRouter, case: &str, reservation_id: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "CreateCapacityReservationCancellationQuote",
        &quote_params(reservation_id),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 minting the quote, got {status} with body {body}"
    );
    reported_quote(case, &body).id
}

/// The `state` `DescribeCapacityReservations` reports for the one reservation
/// `reservation_id` names.
async fn reported_state(router: &MultiServiceRouter, case: &str, reservation_id: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeCapacityReservations",
        &indexed("CapacityReservationId", &[reservation_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading {reservation_id} back, got {status} with \
         body {body}"
    );
    element(&body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: expected a state in {body}"))
}

/// The state the reservation is reported in once it has left `cancelling`, waited for as a
/// CONDITION rather than for a span of the wall clock: the loop looks again until the move the
/// clock owes the reservation has fallen due.
async fn state_once_the_charges_are_applied(
    router: &MultiServiceRouter,
    case: &str,
    reservation_id: &str,
) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let state = reported_state(router, case, reservation_id).await;
        if state != CAPACITY_RESERVATION_STATE_CANCELLING {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the reservation never left \
             {CAPACITY_RESERVATION_STATE_CANCELLING}, last read {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The state word the store HOLDS for `reservation_id`, and whether the clock still owes it a
/// move.
///
/// Read out of the store rather than off the wire because that is what a write is: a reader
/// reports the state the clock has reached, which is the same word once the owed move has
/// fallen due, so only the record says whether the cancel wrote the state the charges are
/// applied in or went straight to the terminal one.
fn written(engine: &Arc<engine::InMemoryEngine>, case: &str, reservation_id: &str) -> (String, bool) {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    let record = store
        .capacity_reservations
        .get(reservation_id)
        .unwrap_or_else(|| panic!("case {case:?}: expected the store to hold {reservation_id}"));
    (record.state().to_string(), record.pending.is_some())
}

/// The status and allocated count the store HOLDS for the interruptible allocation
/// `reservation_id` carries.
fn written_allocation(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    reservation_id: &str,
) -> (String, i32) {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    let allocation = store
        .capacity_reservations
        .get(reservation_id)
        .and_then(|record| record.view.interruptible_capacity_allocation.as_ref())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {reservation_id} to carry an allocation")
        });
    (
        allocation.status.clone().unwrap_or_default(),
        allocation.instance_count.unwrap_or_default(),
    )
}

/// Seed one Capacity Reservation in `state`, with `commitment` as its commitment, and answer
/// its id.
///
/// Seeded rather than created over the wire because the states and commitment windows the
/// rules generalise over are not all reachable through a create: a commitment that has
/// ELAPSED needs a reservation older than its own commitment duration, and `delayed` is where
/// "Amazon EC2 encountered a delay in provisioning the requested future-dated Capacity
/// Reservation" (`CapacityReservation$State`) leaves one, which no request asks for.
fn seed(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    state: &'static str,
    instance_count: i32,
    start_date: SystemTime,
    commitment: Option<CapacityReservationCommitment>,
) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_capacity_reservation(CapacityReservationSpec {
            instance_type: "c5.xlarge".to_string(),
            instance_platform: INSTANCE_PLATFORM.to_string(),
            availability_zone: AVAILABILITY_ZONE.to_string(),
            availability_zone_id: AVAILABILITY_ZONE_ID.to_string(),
            region: REGION.to_string(),
            tenancy: TENANCY.to_string(),
            instance_count,
            ebs_optimized: false,
            ephemeral_storage: false,
            end_date: None,
            instance_match_criteria: INSTANCE_MATCH_CRITERIA_TARGETED.to_string(),
            created_at: SystemTime::now(),
            start_date: Some(start_date),
            commitment,
            delivery_preference: Some(DELIVERY_PREFERENCE.to_string()),
            state,
            tags: Vec::new(),
            interruption_info: None,
            zero_size_preference: None,
            pending: None,
            capacity_reservation_fleet_id: None,
        })
        .unwrap_or_else(|| panic!("case {case:?}: expected the store to accept the reservation"))
}

/// A commitment that is still in force, ending `ahead` from now.
fn live_commitment(ahead: Duration) -> CapacityReservationCommitment {
    CapacityReservationCommitment {
        duration_seconds: i64::try_from(ahead.as_secs()).unwrap_or(i64::MAX),
        end_date: SystemTime::now() + ahead,
    }
}

/// A commitment the clock has passed, which ended `ago` before now.
fn elapsed_commitment(ago: Duration) -> CapacityReservationCommitment {
    CapacityReservationCommitment {
        duration_seconds: 60 * 60,
        end_date: SystemTime::now()
            .checked_sub(ago)
            .expect("a representable past instant"),
    }
}

/// An instant `ago` before now, for a reservation that has already been delivered.
fn delivered_at(ago: Duration) -> SystemTime {
    SystemTime::now()
        .checked_sub(ago)
        .expect("a representable past instant")
}

/// A reservation created for immediate use, with no commitment at all, created OVER THE WIRE.
async fn create_immediate(router: &MultiServiceRouter, case: &str, instance_count: i32) -> String {
    let params = [
        param("InstanceType", "c5.xlarge"),
        param("InstancePlatform", INSTANCE_PLATFORM),
        param("InstanceCount", &instance_count.to_string()),
        param("AvailabilityZone", AVAILABILITY_ZONE),
    ];
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateCapacityReservation", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating the reservation, got {status} with body \
         {body}"
    );
    element(&body, "capacityReservationId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a reservation id in {body}"))
}

// ---------------------------------------------------------------------------
// Rule 1: a charged cancel winds the commitment down
// ---------------------------------------------------------------------------

/// A cancel that applies the commitment wind-down charges leaves the reservation in the state
/// those charges are applied in, and the clock moves it on to the terminal state of a cancel.
#[tokio::test]
async fn a_charged_cancel_winds_the_commitment_down_before_the_reservation_is_cancelled() {
    for row in charged_rows() {
        let case = row.name;
        let (router, engine) = super::ec2_query::emulator();
        let id = create_future_dated(&router, &row, start_date(row.lead_days)).await;
        let quote_id = mint_quote(&router, case, &id).await;

        let (status, body) = cancel(&router, &id, Some(&quote_id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 cancelling {id}, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected the cancel to answer true, got {body}"
        );

        // The write: the state the charges are applied in, with the move to the terminal
        // state owed to the clock rather than taken in the same instant.
        let (state, owed) = written(&engine, case, &id);
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_CANCELLING,
            "case {case:?}: expected a charged cancel to write \
             {CAPACITY_RESERVATION_STATE_CANCELLING}, got {state:?}"
        );
        assert!(
            owed,
            "case {case:?}: expected the move out of {CAPACITY_RESERVATION_STATE_CANCELLING} \
             to be owed to the clock, got none"
        );

        // Neither the state the charges are applied in nor the terminal state admits another
        // cancel, so the second call is refused whichever of the two the clock has reached.
        let (status, refusal) = cancel(&router, &id, Some(&quote_id)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a second cancel: expected HTTP 400, got {status} with body \
             {refusal}"
        );
        assert_eq!(
            error_code(&refusal),
            "IncorrectState",
            "case {case:?}: a second cancel: expected IncorrectState, got {refusal}"
        );

        // The clock's own move, observed through a READ: the charges have been applied, so the
        // reservation is in the terminal state of a cancel.
        let state = state_once_the_charges_are_applied(&router, case, &id).await;
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the reservation to reach \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {state:?}"
        );
        // A read does not change what a later caller sees.
        let again = reported_state(&router, case, &id).await;
        assert_eq!(
            again, state,
            "case {case:?}: a second read must report the same state, got {again:?}"
        );
        // And the refusal of a cancel now names the state the reservation ended in, EC2's own
        // wording for a request a resource's state forbids.
        let (status, refusal) = cancel(&router, &id, Some(&quote_id)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a cancel of the cancelled reservation: expected HTTP 400, got \
             {status} with body {refusal}"
        );
        assert_eq!(
            error_message(&refusal),
            format!(
                "Capacity Reservation '{id}' is in the \
                 '{CAPACITY_RESERVATION_STATE_CANCELLED}' state."
            ),
            "case {case:?}: expected the refusal to name the state the reservation is in, got \
             {refusal}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a cancel with nothing to charge for is terminal at once
// ---------------------------------------------------------------------------

/// One row of the uncharged table: what it is called, and the reservation the cancel names.
struct UnchargedCase {
    case: &'static str,
    /// `None` for a reservation created over the wire for immediate use, which commits to
    /// nothing; `Some` for one seeded in a state and commitment a create cannot reach.
    seeded: Option<(&'static str, Option<Duration>)>,
    instance_count: i32,
}

/// The rows: a reservation that committed to nothing, ones whose commitment the clock has
/// passed, and one in the state that waives the commitment.
fn uncharged_rows() -> Vec<UnchargedCase> {
    vec![
        UnchargedCase {
            case: "a single instance that committed to nothing",
            seeded: None,
            instance_count: 1,
        },
        UnchargedCase {
            case: "nine instances that committed to nothing",
            seeded: None,
            instance_count: 9,
        },
        UnchargedCase {
            case: "a commitment that elapsed a minute ago",
            seeded: Some((
                CAPACITY_RESERVATION_STATE_ACTIVE,
                Some(Duration::from_secs(60)),
            )),
            instance_count: 4,
        },
        UnchargedCase {
            case: "a commitment that elapsed a week ago",
            seeded: Some((
                CAPACITY_RESERVATION_STATE_ACTIVE,
                Some(Duration::from_secs(7 * 24 * 60 * 60)),
            )),
            instance_count: 64,
        },
        UnchargedCase {
            case: "a delayed reservation, whose commitment is waived",
            seeded: Some((CAPACITY_RESERVATION_STATE_DELAYED, None)),
            instance_count: 3,
        },
    ]
}

/// A cancel with nothing to charge for needs no quote, ends in the terminal state straight
/// away and leaves the clock owing the reservation nothing.
#[tokio::test]
async fn an_uncharged_cancel_is_cancelled_at_once() {
    for row in uncharged_rows() {
        let UnchargedCase {
            case,
            seeded,
            instance_count,
        } = row;
        let (router, engine) = super::ec2_query::emulator();
        let id = match seeded {
            None => create_immediate(&router, case, instance_count).await,
            // A reservation whose commitment has elapsed was delivered before it ended; one
            // whose commitment is waived is still waiting for its start date.
            Some((state, Some(ago))) => seed(
                &engine,
                case,
                state,
                instance_count,
                delivered_at(ago + Duration::from_secs(60 * 60)),
                Some(elapsed_commitment(ago)),
            ),
            Some((state, None)) => seed(
                &engine,
                case,
                state,
                instance_count,
                SystemTime::now() + Duration::from_secs(5 * 24 * 60 * 60),
                Some(live_commitment(Duration::from_secs(30 * 24 * 60 * 60))),
            ),
        };

        let (status, body) = cancel(&router, &id, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 cancelling {id} with no quote, got {status} \
             with body {body}"
        );
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected the cancel to answer true, got {body}"
        );

        // Terminal at once: nothing is owed to the clock, so no caller ever sees the state the
        // charges of a cancellation are applied in.
        let (state, owed) = written(&engine, case, &id);
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected an uncharged cancel to write \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {state:?}"
        );
        assert!(
            !owed,
            "case {case:?}: expected the clock to owe a cancelled reservation nothing"
        );
        let reported = reported_state(&router, case, &id).await;
        assert_eq!(
            reported, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the reader to report \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {reported:?}"
        );
        // The capacity the reservation reserved is still what it reserved, whatever state it
        // ended in.
        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "DescribeCapacityReservations",
            &indexed("CapacityReservationId", &[id.as_str()]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading the capacity back, got {status} with \
             body {body}"
        );
        assert_eq!(
            element(&body, "totalInstanceCount").as_deref(),
            Some(instance_count.to_string().as_str()),
            "case {case:?}: expected the reserved count to stand, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: which it is, is a fact of the reservation
// ---------------------------------------------------------------------------

/// A reservation under a LIVE commitment is refused until the request carries the quote and the
/// charge type, in every state that commitment is charged for — and in the one state that
/// waives it, neither is needed.
#[tokio::test]
async fn the_quote_a_cancel_needs_follows_the_state_the_reservation_is_in() {
    // The rows differ in the `CapacityReservationState` the reservation is in when the cancel
    // arrives — every state a cancel is accepted from
    // (`smithy_query(query="shape",shape="CapacityReservationState")`) — and in whether that
    // state waives the commitment.
    let rows: [(&str, &'static str, bool); 4] = [
        ("a reservation being assessed", CAPACITY_RESERVATION_STATE_ASSESSING, true),
        ("a scheduled reservation", CAPACITY_RESERVATION_STATE_SCHEDULED, true),
        ("a delivered reservation", CAPACITY_RESERVATION_STATE_ACTIVE, true),
        ("a delayed reservation", CAPACITY_RESERVATION_STATE_DELAYED, false),
    ];
    for (case, state, charged) in rows {
        let (router, engine) = super::ec2_query::emulator();
        // A live commitment, with the start date the state it is in implies: a delivered
        // reservation started already, and one still being assessed or scheduled has not.
        let start = if state == CAPACITY_RESERVATION_STATE_ACTIVE {
            delivered_at(Duration::from_secs(60 * 60))
        } else {
            SystemTime::now() + Duration::from_secs(5 * 24 * 60 * 60)
        };
        let id = seed(
            &engine,
            case,
            state,
            2,
            start,
            Some(live_commitment(Duration::from_secs(30 * 24 * 60 * 60))),
        );

        if !charged {
            let (status, body) = cancel(&router, &id, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the waived commitment to be cancelled with no quote, \
                 got {status} with body {body}"
            );
            let (written_state, _) = written(&engine, case, &id);
            assert_eq!(
                written_state, CAPACITY_RESERVATION_STATE_CANCELLED,
                "case {case:?}: expected no charge to be applied, got {written_state:?}"
            );
            continue;
        }

        // No quote at all: the parameter the charged cancel needs is missing.
        let (status, body) = cancel(&router, &id, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a cancel with no quote, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "MissingParameter",
            "case {case:?}: expected MissingParameter, got {body}"
        );

        // The quote alone, with no charge type beside it: still missing a parameter.
        let quote_id = mint_quote(&router, case, &id).await;
        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "CancelCapacityReservation",
            &[
                param("CapacityReservationId", &id),
                param("QuoteId", &quote_id),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a cancel with no charge type, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "MissingParameter",
            "case {case:?}: expected MissingParameter, got {body}"
        );

        // Neither refusal wrote anything: the reservation is still in the state it was in.
        let (written_state, _) = written(&engine, case, &id);
        assert_eq!(
            written_state, state,
            "case {case:?}: expected the refused cancels to leave the reservation in \
             {state:?}, got {written_state:?}"
        );

        // Both of them, and the cancel winds the commitment down.
        let (status, body) = cancel(&router, &id, Some(&quote_id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a cancel carrying both, got {status} with \
             body {body}"
        );
        let (written_state, owed) = written(&engine, case, &id);
        assert_eq!(
            written_state, CAPACITY_RESERVATION_STATE_CANCELLING,
            "case {case:?}: expected the charged cancel to write \
             {CAPACITY_RESERVATION_STATE_CANCELLING}, got {written_state:?}"
        );
        assert!(
            owed,
            "case {case:?}: expected the move to \
             {CAPACITY_RESERVATION_STATE_CANCELLED} to be owed to the clock"
        );
        let reached = state_once_the_charges_are_applied(&router, case, &id).await;
        assert_eq!(
            reached, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the reservation to reach \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {reached:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: a charged cancel also finishes the allocation it released
// ---------------------------------------------------------------------------

/// The `status` of the interruptible allocation the source reservation carries, as
/// `DescribeCapacityReservations` reports it — the first `status` the reservation's element
/// carries.
async fn reported_allocation(
    router: &MultiServiceRouter,
    case: &str,
    source_id: &str,
    member: &str,
) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeCapacityReservations",
        &indexed("CapacityReservationId", &[source_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading {source_id} back, got {status} with body \
         {body}"
    );
    element(&body, member)
        .unwrap_or_else(|| panic!("case {case:?}: expected <{member}> in {body}"))
}

/// A reservation that has capacity allocated out of it is owed BOTH ends of a charged cancel:
/// its own terminal state and the terminal status of the allocation that cancel released.
///
/// The one case where the two moves the clock owes a reservation fall due together — a source
/// under a live commitment, which is a delivered future-dated reservation, so it is seeded with
/// the start date and commitment such a reservation has.
#[tokio::test]
async fn a_charged_cancel_also_finishes_the_allocation_it_released() {
    // The rows differ in how much of the source's capacity is allocated out of it: part of it,
    // and all of it.
    for (case, capacity, allocated) in [
        ("part of the source's capacity", 8, 3),
        ("all of the source's capacity", 4, 4),
    ] {
        let (router, engine) = super::ec2_query::emulator();
        let source = seed(
            &engine,
            case,
            CAPACITY_RESERVATION_STATE_ACTIVE,
            capacity,
            delivered_at(Duration::from_secs(60 * 60)),
            Some(live_commitment(Duration::from_secs(30 * 24 * 60 * 60))),
        );

        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "CreateInterruptibleCapacityReservationAllocation",
            &[
                param("CapacityReservationId", &source),
                param("InstanceCount", &allocated.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 allocating, got {status} with body {body}"
        );
        // The allocation completes on the clock; waited for as a CONDITION so the cancel below
        // releases an allocation that is in place rather than racing it.
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let status = reported_allocation(&router, case, &source, "status").await;
            if status == INTERRUPTIBLE_ALLOCATION_STATUS_ACTIVE {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the allocation never reached \
                 {INTERRUPTIBLE_ALLOCATION_STATUS_ACTIVE}, last read {status:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let interruptible =
            reported_allocation(&router, case, &source, "interruptibleCapacityReservationId").await;

        let quote_id = mint_quote(&router, case, &source).await;
        let (status, body) = cancel(&router, &source, Some(&quote_id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 cancelling the source, got {status} with body \
             {body}"
        );

        // The write: the state the charges are applied in, and the allocation on its way out.
        let (state, owed) = written(&engine, case, &source);
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_CANCELLING,
            "case {case:?}: expected the charged cancel to write \
             {CAPACITY_RESERVATION_STATE_CANCELLING}, got {state:?}"
        );
        assert!(
            owed,
            "case {case:?}: expected both ends of the cancel to be owed to the clock"
        );
        let (allocation_status, _) = written_allocation(&engine, case, &source);
        assert_eq!(
            allocation_status, INTERRUPTIBLE_ALLOCATION_STATUS_CANCELING,
            "case {case:?}: expected the released allocation to be \
             {INTERRUPTIBLE_ALLOCATION_STATUS_CANCELING}, got {allocation_status:?}"
        );
        // The capacity is the source's again straight away, whatever state it ends in.
        let total = reported_allocation(&router, case, &source, "totalInstanceCount").await;
        assert_eq!(
            total,
            capacity.to_string(),
            "case {case:?}: expected the allocated capacity back in the reserved count, got \
             {total:?}"
        );

        // Both ends, once the move has fallen due: the reservation is cancelled and the
        // allocation is in its terminal status with nothing allocated to it.
        let reached = state_once_the_charges_are_applied(&router, case, &source).await;
        assert_eq!(
            reached, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the source to reach \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {reached:?}"
        );
        let status = reported_allocation(&router, case, &source, "status").await;
        assert_eq!(
            status, INTERRUPTIBLE_ALLOCATION_STATUS_CANCELED,
            "case {case:?}: expected the allocation to reach \
             {INTERRUPTIBLE_ALLOCATION_STATUS_CANCELED}, got {status:?}"
        );
        let allocated_now = reported_allocation(&router, case, &source, "instanceCount").await;
        assert_eq!(
            allocated_now, "0",
            "case {case:?}: expected nothing allocated to a released allocation, got \
             {allocated_now:?}"
        );
        // The interruptible reservation the allocation created is cancelled with it.
        let child_state = reported_state(&router, case, &interruptible).await;
        assert_eq!(
            child_state, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the interruptible reservation to be \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {child_state:?}"
        );

        // And a WRITE materialises both ends in the store, so the record a later writer reads
        // holds what every reader above reported.
        let (status, refusal) = cancel(&router, &source, Some(&quote_id)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a cancel of the cancelled source: expected HTTP 400, got {status} \
             with body {refusal}"
        );
        let (state, owed) = written(&engine, case, &source);
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: expected the store to hold \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {state:?}"
        );
        assert!(
            !owed,
            "case {case:?}: expected the clock to owe the cancelled reservation nothing"
        );
        let (allocation_status, allocated_count) = written_allocation(&engine, case, &source);
        assert_eq!(
            allocation_status, INTERRUPTIBLE_ALLOCATION_STATUS_CANCELED,
            "case {case:?}: expected the store to hold \
             {INTERRUPTIBLE_ALLOCATION_STATUS_CANCELED}, got {allocation_status:?}"
        );
        assert_eq!(
            allocated_count, 0,
            "case {case:?}: expected nothing allocated in the store, got {allocated_count}"
        );
    }
}
