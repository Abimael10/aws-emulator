// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two rules both Athena operations that request provisioned
//! capacity enforce on the amount of data processing units a request states.
//!
//! The first is the BLOCK rule: capacity is allocated in whole blocks, so an amount of at
//! least one data processing unit that is not a whole multiple of the block size cannot be
//! allocated and is refused with the one sentence that states that rule. The block size is
//! the minimum of the modelled `@range` on `TargetDpusInteger`, the shape
//! `CreateCapacityReservationInput$TargetDpus` and `UpdateCapacityReservationInput$TargetDpus`
//! both target
//! (`smithy_query(query="operation",operation="CreateCapacityReservation,UpdateCapacityReservation")`),
//! and the rule holds whether the FRAMEWORK measured the amount against that modelled bound
//! (an amount below it never reaches a handler) or the HANDLER measured it against the rule
//! (an amount above it that is not a whole number of blocks).
//!
//! The second is the service's own lower BOUND: an amount of capacity is a positive number of
//! data processing units, so an amount below one states no capacity at all and is refused for
//! that bound — in the validation wording any member outside its range is refused in, naming
//! the member and the bound the service applies, which is one unit and not one block. That is
//! a different refusal from the block rule and is covered by
//! `tests/all/capacity_dpus_minimum_bound.rs`.
//!
//! Either way the refusal is the same otherwise: HTTP 400, `InvalidRequestException`, and the
//! class `INVALID_INPUT` under both of the names the wire gives a refusal's code.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots, so each assertion is about what an SDK client
//! observes. Each test asserts the rule over a TABLE of amounts rather than one recorded
//! request: the rows differ along where the amount sits relative to a block boundary and
//! along which operation asked for it.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    capacity_dpus_not_in_increments, CAPACITY_DPUS_INCREMENT,
    CAPACITY_RESERVATION_STATUS_ACTIVE, ERROR_CODE_INVALID_INPUT,
};

use crate::harness::{await_reservation_status, call, error_code, message, router};

/// The error shape every refusal of these two operations carries: the client error both
/// model (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The sentence AWS states the capacity-request rule in, pinned verbatim here so that a
/// change to the wording the service renders cannot pass unnoticed: it is the `Message` a
/// recorded refusal of an amount that cannot be allocated carries, fixed text with no part
/// taken from the request.
const CAPACITY_SENTENCE: &str = "Capacity must be requested in increments of 4.";

/// An allocatable amount every row that is not about the amount itself asks for: a whole
/// number of blocks, well above one.
const ALLOCATABLE_DPUS: i32 = CAPACITY_DPUS_INCREMENT * 6;

/// The amounts that state capacity but no WHOLE NUMBER of blocks: every position strictly
/// between two block boundaries, low and high.
///
/// An amount below the smallest a request may state is not here: it broke the other rule, and
/// `tests/all/capacity_dpus_minimum_bound.rs` is where that class is asserted.
fn unallocatable_amounts() -> Vec<(String, i32)> {
    let mut rows: Vec<(String, i32)> = Vec::new();
    // Each position inside the first block, then each inside a block much further up, so
    // the rule is seen to hold away from the modelled floor as well as at it.
    for offset in 1..CAPACITY_DPUS_INCREMENT {
        rows.push((format!("{offset} past no blocks"), offset));
        rows.push((
            format!("{offset} past {ALLOCATABLE_DPUS} data processing units"),
            ALLOCATABLE_DPUS + offset,
        ));
    }
    rows
}

/// The amounts a reservation CAN be allocated: one block, and whole multiples of it.
fn allocatable_amounts() -> Vec<(String, i32)> {
    [1, 2, 3, 6, 60, 2500]
        .into_iter()
        .map(|blocks| {
            (
                format!("{blocks} whole block(s)"),
                CAPACITY_DPUS_INCREMENT * blocks,
            )
        })
        .collect()
}

/// Assert one refusal: HTTP 400, the modelled refusal shape, the class under both of the
/// names the wire gives it, and the sentence that states the rule word for word.
fn assert_refused_for_the_rule(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        INVALID_REQUEST,
        "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
    );
    for member in ["AthenaErrorCode", "ErrorCode"] {
        assert_eq!(
            body[member].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected {member} {ERROR_CODE_INVALID_INPUT:?}, got {body:?}"
        );
    }
    assert_eq!(
        message(body),
        CAPACITY_SENTENCE,
        "case {case:?}: expected the sentence that states the rule, got {:?}",
        message(body)
    );
    // The service renders that sentence from the rule it enforces rather than from a literal,
    // so the two must say the same thing.
    assert_eq!(
        capacity_dpus_not_in_increments(),
        CAPACITY_SENTENCE,
        "case {case:?}: expected the rule's own rendering to be AWS's sentence"
    );
}

/// A create that asks for an amount no reservation can be allocated is refused with that one
/// sentence, and registers nothing: the name it asked for is still free afterwards.
#[tokio::test]
async fn a_create_of_an_unallocatable_amount_is_refused_and_registers_nothing() {
    let router = router();

    for (case, target_dpus) in unallocatable_amounts() {
        let name = format!("create-{}", target_dpus.unsigned_abs());
        let (status, body) = call(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refused_for_the_rule(&case, status, &body);

        // Nothing was registered under that name: a read of it is the miss of a
        // reservation this account does not hold.
        let (read_status, read) = call(
            &router,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            read_status, 400,
            "case {case:?}: a refused create must register no reservation, got {read_status} \
             with {read:?}"
        );
    }
}

/// An update that asks for such an amount is refused with the same sentence, and leaves the
/// amount the reservation already holds exactly as it was.
#[tokio::test]
async fn an_update_to_an_unallocatable_amount_is_refused_and_changes_nothing() {
    let router = router();

    let name = "updated";
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
    // Only an allocated reservation takes an update at all, so the row would otherwise be
    // refused for the status rather than for its amount.
    await_reservation_status(&router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;

    for (case, target_dpus) in unallocatable_amounts() {
        let (status, body) = call(
            &router,
            "UpdateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_refused_for_the_rule(&case, status, &body);

        let (read_status, read) = call(
            &router,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: the reservation must still be readable, got {read_status} with \
             {read:?}"
        );
        assert_eq!(
            read["CapacityReservation"]["TargetDpus"].as_i64(),
            Some(i64::from(ALLOCATABLE_DPUS)),
            "case {case:?}: a refused update must change no requested amount, got {read:?}"
        );
    }
}

/// A whole number of blocks is allocatable: the create is served and the amount round-trips
/// through a read, and an update to another whole number of blocks is served too.
#[tokio::test]
async fn a_whole_number_of_blocks_is_allocated() {
    let router = router();

    for (case, target_dpus) in allocatable_amounts() {
        let name = format!("blocks-{target_dpus}");
        let (status, created) = call(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {target_dpus} data processing units must be accepted, got \
             {status} with {created:?}"
        );

        let (read_status, read) = call(
            &router,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: the created reservation must be readable, got {read_status} with \
             {read:?}"
        );
        assert_eq!(
            read["CapacityReservation"]["TargetDpus"].as_i64(),
            Some(i64::from(target_dpus)),
            "case {case:?}: the read must report the amount the create asked for, got {read:?}"
        );

        // The same rule on the update path, which carries the same member: another whole
        // number of blocks is asked for and becomes the requested amount.
        await_reservation_status(&router, &name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        let updated_to = target_dpus + CAPACITY_DPUS_INCREMENT;
        let (status, updated) = call(
            &router,
            "UpdateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": updated_to }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an update to {updated_to} must be accepted, got {status} with \
             {updated:?}"
        );
        let (_, read) = call(
            &router,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            read["CapacityReservation"]["TargetDpus"].as_i64(),
            Some(i64::from(updated_to)),
            "case {case:?}: the update's amount must be the requested one, got {read:?}"
        );
    }
}

/// A request whose amount is inside a block AND whose name breaks another input constraint is
/// stated in AWS's validation envelope over the constraint it really broke: the amount meets
/// the lower bound the service states, so no clause about that bound is raised, and the block
/// rule's one sentence is what a request breaking that rule ALONE is answered with.
#[tokio::test]
async fn a_request_that_also_breaks_another_constraint_states_that_constraint() {
    let router = router();
    let (status, body) = call(
        &router,
        "CreateCapacityReservation",
        // The name is outside the modelled `CapacityReservationName` pattern
        // (`^[a-zA-Z0-9._-]+$`) and the amount is inside a block.
        &json!({ "Name": "not a name!", "TargetDpus": 1 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_REQUEST,
        "expected {INVALID_REQUEST}, got {body:?}"
    );
    let stated = message(&body);
    assert_eq!(
        stated,
        "1 validation error detected: Value at 'name' failed to satisfy constraint: Member \
         must satisfy regular expression pattern: [a-zA-Z0-9._-]+",
        "expected the clause for the constraint the request broke, got {stated:?}"
    );

    // And a request that breaks the DPU rule beside a constraint the service has no sentence
    // of its own for is still stated over every clause: a request stating no capacity at all
    // broke the lower bound, which IS a numeric bound and is named as one.
    let (status, body) = call(
        &router,
        "CreateCapacityReservation",
        &json!({ "Name": "not a name!", "TargetDpus": 0 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected HTTP 400, got {status} with {body:?}"
    );
    let stated = message(&body);
    assert!(
        stated.starts_with("2 validation errors detected: "),
        "expected the envelope to count both clauses, got {stated:?}"
    );
    for member in ["'name'", "'targetDpus'"] {
        assert!(
            stated.contains(member),
            "expected {member} among the clauses, got {stated:?}"
        );
    }
}
