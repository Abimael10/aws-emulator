// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for SPENDING a Capacity Reservation cancellation quote: the quote
//! `CreateCapacityReservationCancellationQuote` mints is one `CancelCapacityReservation`
//! accepts, and the reservation it was minted for is cancelled. Driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//! ec2Query is selected by the `ec2` signing name in the request's SigV4 credential scope, so
//! every call here carries one.
//!
//! The rules under test:
//!
//! 1. A quote a MINT answers with is usable: "The quote includes the cancellation terms and a
//!    quote ID that you can pass to the CancelCapacityReservation action"
//!    (`CreateCapacityReservationCancellationQuote`), and that action takes a quote "in an
//!    active state" (`CancelCapacityReservationRequest$QuoteId`) — so a minted quote is
//!    reported `active` and the cancel that passes it with `ApplyCancellationCharges` set to
//!    `commitment-wind-down` answers `Return=true` and leaves the reservation cancelling or
//!    cancelled, with no state a caller has to wait through first.
//! 2. That state is the quote's own, not the cancel's: a quote whose documented validity has
//!    lapsed ("Cancellation quotes are valid for 24 hours", same doc) is `expired` and the
//!    cancel that passes it is refused `IncorrectState`, naming the state the quote is in.
//! 3. A quote belongs to the reservation it was minted for: passing it for ANOTHER reservation
//!    is refused, and so is a quote id no quote of the account answers to.
//! 4. An id of the Capacity Reservation id form that no reservation of the account answers to
//!    is reported `InvalidCapacityReservationId.NotFound` with AWS's sentence for it, by every
//!    operation that resolves such an id — the cancel and the interruptible-allocation update
//!    among them — while `InvalidCapacityReservationId.Malformed` is kept for a value outside
//!    that form.
//!
//! Every table row is a distinct input along what those rules generalise over — how much
//! capacity the reservation holds, how far ahead it starts, how long it committed for, which
//! id form the refusal rows send, and which operation resolves it.

use std::time::{Duration, SystemTime};

use ec2::ops::types::CapacityReservationConfiguration;
use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::{
    APPLY_CANCELLATION_CHARGES_COMMITMENT_WIND_DOWN,
    CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_ACTIVE,
    CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_EXPIRED,
    CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_PENDING,
    CAPACITY_RESERVATION_CANCELLATION_QUOTE_VALIDITY, CAPACITY_RESERVATION_STATE_CANCELLED,
    CAPACITY_RESERVATION_STATE_CANCELLING, CAPACITY_RESERVATION_STATE_SCHEDULED,
    CapacityReservationCancellationQuoteSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::capacity_reservation_cancellation_quotes::{
    ReservationCase, create_future_dated, quote_params, reported_quote, start_date,
};
use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The rows the quote-then-cancel rule is asserted over. They differ along what the rule
/// generalises over: how much capacity the reservation holds, how far ahead it starts and how
/// long it committed for.
fn reservation_rows() -> Vec<ReservationCase> {
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
            name: "sixteen instances, sixty-five days out, a fortnight's commitment",
            instance_type: "t3.micro",
            instance_count: 16,
            lead_days: 65,
            commitment_seconds: 14 * 24 * 60 * 60,
            quote_tags: &[("Team", "platform")],
        },
        ReservationCase {
            name: "the furthest-ahead start and a year's commitment",
            instance_type: "r5.large",
            instance_count: 64,
            lead_days: 119,
            commitment_seconds: 365 * 24 * 60 * 60,
            quote_tags: &[],
        },
    ]
}

/// One `CreateCapacityReservationCancellationQuote` call carrying `params`.
async fn generate(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    super::ec2_query::call(
        router,
        REGION,
        "CreateCapacityReservationCancellationQuote",
        params,
    )
    .await
}

/// One `CancelCapacityReservation` call for `reservation_id`, with `quote_id` and the one
/// cancellation charge the model carries when a quote is passed.
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

/// The `state` the one reservation `reservation_id` names reports.
async fn reservation_state(
    router: &MultiServiceRouter,
    case: &str,
    reservation_id: &str,
) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeCapacityReservations",
        &indexed("CapacityReservationId", &[reservation_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the reservation back, got {status} with \
         body {body}"
    );
    element(&body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: expected a state in {body}"))
}

/// A minted quote is `active` and the cancel that passes it cancels the reservation.
#[tokio::test]
async fn a_minted_quote_cancels_the_reservation_it_was_minted_for() {
    let (router, _engine) = super::ec2_query::emulator();

    for case in reservation_rows() {
        let name = case.name;
        let reservation_id = create_future_dated(&router, &case, start_date(case.lead_days)).await;

        let (status, body) = generate(&router, &quote_params(&reservation_id)).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 minting the quote, got {status} with body {body}"
        );
        let quote = reported_quote(name, &body);
        assert_eq!(
            quote.capacity_reservation_id, reservation_id,
            "case {name:?}: expected the quote to name the reservation it quotes, got {:?}",
            quote.capacity_reservation_id
        );
        assert_eq!(
            quote.quote_state, CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_ACTIVE,
            "case {name:?}: expected a minted quote to be \
             {CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_ACTIVE} rather than \
             {CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_PENDING}, got {:?}",
            quote.quote_state
        );
        // The configuration the quote reports is the reservation's own as of the mint, which
        // for a future-dated reservation whose assessment has fallen due is `scheduled`.
        assert_eq!(
            quote.configuration_reservation_state, CAPACITY_RESERVATION_STATE_SCHEDULED,
            "case {name:?}: expected the quoted configuration to report the reservation's \
             state, got {:?}",
            quote.configuration_reservation_state
        );

        // The cancel SPENDS it, with no wait in between.
        let (status, body) = cancel(&router, &reservation_id, Some(&quote.id)).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the cancel carrying the minted quote to be answered \
             HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "return").as_deref(),
            Some("true"),
            "case {name:?}: expected Return=true, got {body}"
        );

        // "The Capacity Reservation transitions to `cancelling` while charges are applied"
        // and its state ends `cancelled` (`CancelCapacityReservation`): either is a
        // reservation that has left the states it was billing in.
        let state = reservation_state(&router, name, &reservation_id).await;
        assert!(
            [
                CAPACITY_RESERVATION_STATE_CANCELLING,
                CAPACITY_RESERVATION_STATE_CANCELLED,
            ]
            .contains(&state.as_str()),
            "case {name:?}: expected the reservation to be \
             {CAPACITY_RESERVATION_STATE_CANCELLING} or \
             {CAPACITY_RESERVATION_STATE_CANCELLED}, got {state:?}"
        );
    }
}

/// A quote that is not usable, and one that is not this reservation's, cannot pay for a
/// cancellation: the refusal names the state the quote is in, or the quote the account does
/// not hold.
#[tokio::test]
async fn a_quote_that_cannot_be_spent_is_refused() {
    let (router, engine) = super::ec2_query::emulator();

    // Two future-dated reservations under live commitments: one the quotes below are minted
    // for, and one to pass another reservation's quote for.
    let rows = reservation_rows();
    let first = &rows[0];
    let second = &rows[1];
    let first_id = create_future_dated(&router, first, start_date(first.lead_days)).await;
    let second_id = create_future_dated(&router, second, start_date(second.lead_days)).await;

    let (status, body) = generate(&router, &quote_params(&first_id)).await;
    assert_eq!(
        status, 200,
        "minting the quote: expected HTTP 200, got {status} with body {body}"
    );
    let minted = reported_quote("the minted quote", &body);

    // A quote minted for ANOTHER reservation.
    let (status, body) = cancel(&router, &second_id, Some(&minted.id)).await;
    assert_eq!(
        status, 400,
        "another reservation's quote: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "another reservation's quote: expected InvalidParameterValue, got {body}"
    );

    // A quote id of the issued form the account holds no quote under.
    let (status, body) = cancel(&router, &first_id, Some("crcq-0ffffffffffffffff")).await;
    assert_eq!(
        status, 400,
        "an unknown quote id: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidCapacityReservationCancellationQuoteId.NotFound",
        "an unknown quote id: expected the quote id not-found code, got {body}"
    );

    // Quotes whose validity has LAPSED: seeded, because the only way to reach one over the
    // wire is to wait a day. The rows differ in how far past the validity they are.
    for (row, ago) in [
        (
            "a quote minted exactly its validity ago",
            CAPACITY_RESERVATION_CANCELLATION_QUOTE_VALIDITY,
        ),
        (
            "a quote minted a week ago",
            Duration::from_secs(7 * 24 * 60 * 60),
        ),
    ] {
        let expired_id = engine
            .state::<Ec2State>()
            .lock()
            .insert_capacity_reservation_cancellation_quote(
                CapacityReservationCancellationQuoteSpec {
                    capacity_reservation_id: first_id.clone(),
                    created_at: SystemTime::now()
                        .checked_sub(ago)
                        .expect("a representable past instant"),
                    current_configuration: CapacityReservationConfiguration {
                        instance_count: Some(first.instance_count),
                        reservation_state: Some(
                            CAPACITY_RESERVATION_STATE_SCHEDULED.to_string(),
                        ),
                    },
                    cancellation_terms: Vec::new(),
                    tags: Vec::new(),
                },
            )
            .expect("the store accepts a seeded quote");
        let (status, body) = cancel(&router, &first_id, Some(&expired_id)).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "IncorrectState",
            "case {row:?}: expected IncorrectState, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "The cancellation quote '{expired_id}' is in state \
                 '{CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_EXPIRED}', not \
                 '{CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_ACTIVE}', so it cannot be \
                 used for a cancellation"
            ),
            "case {row:?}: expected the refusal to name the state the quote is in, got {body}"
        );
        // The reservation it names was not cancelled by the refused call.
        let state = reservation_state(&router, row, &first_id).await;
        assert_eq!(
            state, CAPACITY_RESERVATION_STATE_SCHEDULED,
            "case {row:?}: expected the refused cancel to leave the reservation \
             {CAPACITY_RESERVATION_STATE_SCHEDULED}, got {state:?}"
        );
    }

    // And the quote minted for the first reservation still spends, after every refusal above.
    let (status, body) = cancel(&router, &first_id, Some(&minted.id)).await;
    assert_eq!(
        status, 200,
        "the minted quote after the refusals: expected HTTP 200, got {status} with body {body}"
    );
}

/// One row of the id table: what it is called, the id the request sends and the half of the
/// `InvalidCapacityReservationId` family it must be refused with.
type IdRow = (&'static str, &'static str, &'static str);

/// The rows every operation that resolves a Capacity Reservation id is asserted over: ids of
/// the issued form the account holds no reservation under, and values outside the id form.
fn id_rows() -> Vec<IdRow> {
    vec![
        (
            "an id of the issued form nothing was reserved under",
            "cr-0ffffffffffffffff",
            "NotFound",
        ),
        (
            "another id of that form, with digits across the alphabet",
            "cr-0123456789abcdef0",
            "NotFound",
        ),
        (
            "the lowest id of that form the mint could have issued",
            "cr-00000000000000001",
            "NotFound",
        ),
        (
            "another resource's id",
            "cb-00000000000000001",
            "Malformed",
        ),
        (
            "an id of the right form with one digit too many",
            "cr-00000000000000001a",
            "Malformed",
        ),
        (
            "digits outside the hexadecimal alphabet",
            "cr-0zzzzzzzzzzzzzzzz",
            "Malformed",
        ),
        ("a value that is not an id at all", "not-an-id", "Malformed"),
    ]
}

/// A Capacity Reservation id of the issued form the account holds no reservation under is
/// reported NOT FOUND, with AWS's sentence for it, and only a value outside the id form is
/// refused as malformed — by the cancel and by the interruptible-allocation update alike.
#[tokio::test]
async fn an_unknown_reservation_id_is_reported_not_found_by_every_operation_that_resolves_one() {
    let (router, _engine) = super::ec2_query::emulator();

    // One stored reservation, so "not found" is about the id the request sent rather than
    // about an empty store.
    let case = &reservation_rows()[0];
    let known = create_future_dated(&router, case, start_date(case.lead_days)).await;
    assert!(
        !known.is_empty(),
        "expected the suite's own reservation to be created"
    );

    for (row, id, half) in id_rows() {
        let expected_code = format!("InvalidCapacityReservationId.{half}");
        let expected_message = if half == "NotFound" {
            format!("The capacity-reservation ID '{id}' does not exist")
        } else {
            format!("Capacity Reservation ID '{id}' is malformed")
        };

        // Every operation that has to resolve such an id, sent the SAME value.
        let calls: Vec<(&str, Vec<(String, String)>)> = vec![
            (
                "CancelCapacityReservation",
                vec![param("CapacityReservationId", id)],
            ),
            (
                "UpdateInterruptibleCapacityReservationAllocation",
                vec![
                    param("CapacityReservationId", id),
                    param("TargetInstanceCount", "1"),
                ],
            ),
        ];
        for (action, params) in calls {
            let (status, body) = super::ec2_query::call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {row:?} through {action}: expected HTTP 400, got {status} with body \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                expected_code,
                "case {row:?} through {action}: expected {expected_code}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_message,
                "case {row:?} through {action}: expected {expected_message:?}, got {body}"
            );
        }
    }

    // The all-zero placeholder, which the two id readers of this crate deliberately disagree
    // about: `ec2::state::require_capacity_reservation_by_id_shape` — the reading the
    // interruptible-allocation update applies, on the recorded contract its doc comment cites
    // for that operation — reads it as an id and reports it not found, while
    // `ec2::state::require_known_capacity_reservation` — the reading the cancel applies, on
    // the recorded refusals its doc comment cites — refuses it as an id EC2 could never have
    // issued, the mint counting from one. Sent per operation, so each operation's own reading
    // is asserted rather than left out of the table above.
    let placeholder = "cr-00000000000000000";
    for (action, params, half) in [
        (
            "UpdateInterruptibleCapacityReservationAllocation",
            vec![
                param("CapacityReservationId", placeholder),
                param("TargetInstanceCount", "1"),
            ],
            "NotFound",
        ),
        (
            "CancelCapacityReservation",
            vec![param("CapacityReservationId", placeholder)],
            "Malformed",
        ),
    ] {
        let expected_code = format!("InvalidCapacityReservationId.{half}");
        let expected_message = if half == "NotFound" {
            format!("The capacity-reservation ID '{placeholder}' does not exist")
        } else {
            format!("Capacity Reservation ID '{placeholder}' is malformed")
        };
        let (status, body) = super::ec2_query::call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 400,
            "the all-zero placeholder through {action}: expected HTTP 400, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "the all-zero placeholder through {action}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "the all-zero placeholder through {action}: expected {expected_message:?}, got \
             {body}"
        );
    }
}
