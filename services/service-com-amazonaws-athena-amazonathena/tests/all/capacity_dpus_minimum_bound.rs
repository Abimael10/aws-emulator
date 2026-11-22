// SPDX-License-Identifier: CC0-1.0

//! The lower BOUND on the amount of capacity a request may state, driven end to end over the
//! same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a request for provisioned capacity states a POSITIVE number of data
//! processing units, so an amount below the smallest a request may state
//! (`MIN_REQUESTED_CAPACITY_DPUS`) is refused for that bound — the client error both
//! operations that request capacity model (`InvalidRequestException`, HTTP 400), with the
//! message in which a member outside its range is refused: AWS's validation envelope over one
//! clause naming the member as the wire names it and the bound the SERVICE applies, with no
//! echo of the amount the request carried. That is a DIFFERENT refusal from the block rule
//! (`tests/all/capacity_dpu_increments.rs`), which states the service's own sentence for an
//! amount of at least one unit that is not a whole number of blocks, and the bound it names is
//! not the modelled `@range` minimum of `TargetDpusInteger` either — that one is a whole block
//! and is what a generated client measures a request against before it sends one.
//!
//! The rule is asserted over a TABLE whose rows differ along how far below the bound the
//! amount sits — no capacity at all, one short of the bound, a whole negative block, and the
//! smallest number the member carries — and along which operation stated it, since both
//! `CreateCapacityReservation` and `UpdateCapacityReservation` target the same shape. A
//! refused request changes nothing: the create registers no reservation and the update leaves
//! the amount its reservation already holds.
//!
//! Nothing here waits for a fixed span: the reservation the update rows address is polled for
//! the state an update is served from, under a deadline far above the span that transition is
//! due after.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    capacity_dpus_not_in_increments, CAPACITY_DPUS_INCREMENT, CAPACITY_RESERVATION_STATUS_ACTIVE,
    ERROR_CODE_INVALID_INPUT, MIN_REQUESTED_CAPACITY_DPUS,
};

use crate::harness::{assert_refusal, await_reservation_status, call, router};

/// An allocatable amount the reservation the update rows address asks for: a whole number of
/// blocks, well above one.
const ALLOCATABLE_DPUS: i32 = CAPACITY_DPUS_INCREMENT * 5;

/// The member the amount of requested capacity travels under, as a refusal's clause names it:
/// the lowerCamel of `TargetDpus`, which both operations declare
/// (`smithy_query(query="operation",operation="CreateCapacityReservation,UpdateCapacityReservation")`).
const TARGET_DPUS_MEMBER: &str = "targetDpus";

/// The amounts that state no capacity at all: every row is below
/// [`MIN_REQUESTED_CAPACITY_DPUS`], from one short of it down to the smallest number the
/// member carries.
fn amounts_below_the_bound() -> Vec<(String, i32)> {
    vec![
        (
            "one short of the smallest amount a request may state".to_string(),
            MIN_REQUESTED_CAPACITY_DPUS - 1,
        ),
        (
            "a negative amount inside a block".to_string(),
            -(CAPACITY_DPUS_INCREMENT - 1),
        ),
        (
            "a negative whole number of blocks".to_string(),
            -CAPACITY_DPUS_INCREMENT,
        ),
        (
            "the smallest amount the member carries".to_string(),
            i32::MIN,
        ),
    ]
}

/// The message such a request is refused with: AWS's validation envelope over the one clause
/// that names the member and the bound the service applies, built from that bound rather than
/// from a literal so the sentence cannot state a bound the service does not apply.
fn below_the_bound_message() -> String {
    format!(
        "1 validation error detected: Value at '{TARGET_DPUS_MEMBER}' failed to satisfy \
         constraint: Member must have value greater than or equal to \
         {MIN_REQUESTED_CAPACITY_DPUS}"
    )
}

/// The `CapacityReservation` the account holds under `name`, and the HTTP status the read
/// answered with.
async fn read_reservation(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> (u16, Value) {
    let (status, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
    (status, body["CapacityReservation"].clone())
}

/// A create that states no capacity at all is refused for the bound, and registers nothing:
/// the name it asked for is still free afterwards.
#[tokio::test]
async fn a_create_below_the_bound_is_refused_for_the_bound_and_registers_nothing() {
    let router = router();

    for (case, target_dpus) in amounts_below_the_bound() {
        let name = format!("create-{}", target_dpus.unsigned_abs());
        let (status, body) = call(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refusal(
            &case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &below_the_bound_message(),
        );
        // The amount the request carried is not echoed back: the clause states the member and
        // the requirement alone.
        let stated = crate::harness::message(&body);
        assert!(
            !stated.contains(&target_dpus.to_string()),
            "case {case:?}: the clause must echo no submitted amount, got {stated:?}"
        );

        let (read_status, read) = read_reservation(&router, &name).await;
        assert_eq!(
            read_status, 400,
            "case {case:?}: a refused create must register no reservation, got {read_status} \
             with {read:?}"
        );
    }
}

/// An update that states no capacity at all is refused the same way, and leaves the amount the
/// reservation already holds exactly as it was.
#[tokio::test]
async fn an_update_below_the_bound_is_refused_for_the_bound_and_changes_nothing() {
    let router = router();

    let name = "held";
    let (status, created) = call(
        &router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": ALLOCATABLE_DPUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation an update addresses must be created, got {status} with {created:?}"
    );
    // Only an allocated reservation takes an update at all, so a row would otherwise be
    // refused for the status rather than for its amount. The CONDITION is waited on, not a
    // span.
    await_reservation_status(&router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;

    for (case, target_dpus) in amounts_below_the_bound() {
        let (status, body) = call(
            &router,
            "UpdateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refusal(
            &case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &below_the_bound_message(),
        );

        let (read_status, read) = read_reservation(&router, name).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: the reservation must still be readable, got {read_status} with \
             {read:?}"
        );
        assert_eq!(
            read["TargetDpus"].as_i64(),
            Some(i64::from(ALLOCATABLE_DPUS)),
            "case {case:?}: a refused update must change no requested amount, got {read:?}"
        );
    }
}

/// The two rules are told apart by the amount itself: at the bound and above it, an amount
/// that is not a whole number of blocks is refused with the service's own sentence for the
/// block rule, and only an amount below the bound is refused for the bound — on both
/// operations.
#[tokio::test]
async fn the_bound_and_the_block_rule_are_two_refusals() {
    let router = router();

    let name = "boundary";
    let (status, created) = call(
        &router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": ALLOCATABLE_DPUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation an update addresses must be created, got {status} with {created:?}"
    );
    await_reservation_status(&router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;

    // (case, the amount, the message it is refused with) — the bound itself and every amount
    // inside the first block are the block rule's; one short of the bound is the bound's.
    let block_rule = capacity_dpus_not_in_increments();
    let bound = below_the_bound_message();
    let mut rows: Vec<(String, i32, &str)> = vec![(
        "one short of the bound".to_string(),
        MIN_REQUESTED_CAPACITY_DPUS - 1,
        &bound,
    )];
    for amount in MIN_REQUESTED_CAPACITY_DPUS..CAPACITY_DPUS_INCREMENT {
        rows.push((format!("{amount} of a whole block"), amount, &block_rule));
    }

    for (case, target_dpus, expected) in rows {
        let (status, body) = call(
            &router,
            "UpdateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refusal(&case, status, &body, ERROR_CODE_INVALID_INPUT, expected);

        let created_under = format!("other-{}", target_dpus.unsigned_abs());
        let (status, body) = call(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": created_under, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refusal(
            &format!("{case} on CreateCapacityReservation"),
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            expected,
        );
    }
}
