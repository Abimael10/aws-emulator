// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT-WIDE Provisioned DPU quota, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a request for provisioned capacity is served only while the account has
//! that much capacity left — the amount asked for, added to what every reservation the account
//! holds already requests, may not pass the account's Provisioned DPU quota
//! (`MAX_ACCOUNT_PROVISIONED_DPUS`) — and a request past it is refused with the client error
//! both operations that request capacity model (`InvalidRequestException`, HTTP 400) and
//! Athena's fixed sentence for that quota, having created and changed nothing. Capacity a
//! cancellation RELEASES is capacity the next request may ask for.
//!
//! Each test asserts that rule over a TABLE rather than one request: the rows differ along the
//! amount asked for (one block, the whole quota, one block past it, far past it), along how much
//! the account already holds, along which operation asks (`CreateCapacityReservation` and
//! `UpdateCapacityReservation` state the same member) and along whether the capacity the
//! account holds was released first.
//!
//! Nothing here waits for a fixed span: a reservation an update addresses is POLLED for the
//! status an update is served from, under a deadline far above the span that transition is due
//! after.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    account_provisioned_dpus_exceeded, capacity_reservation_not_found, CAPACITY_DPUS_INCREMENT,
    CAPACITY_RESERVATION_STATUS_ACTIVE, CAPACITY_RESERVATION_STATUS_CANCELLED,
    ERROR_CODE_INVALID_INPUT, MAX_ACCOUNT_PROVISIONED_DPUS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, await_reservation_status, call, error_code, message, router};

/// One block of capacity, the unit every requested amount is a multiple of, so a row that is
/// about the QUOTA is never refused for the block rule instead.
const ONE_BLOCK: i32 = CAPACITY_DPUS_INCREMENT;

/// Ask for a reservation named `name` of `target_dpus` data processing units.
async fn create(router: &MultiServiceRouter, name: &str, target_dpus: i32) -> (u16, Value) {
    call(
        router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": target_dpus }),
    )
    .await
}

/// Ask the reservation `name` for another amount.
async fn update(router: &MultiServiceRouter, name: &str, target_dpus: i32) -> (u16, Value) {
    call(
        router,
        "UpdateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": target_dpus }),
    )
    .await
}

/// Create the reservation `name` of `target_dpus` and wait for the account to be holding that
/// capacity, which is the status an update is served from.
async fn given_active(router: &MultiServiceRouter, name: &str, target_dpus: i32) {
    let (status, body) = create(router, name, target_dpus).await;
    assert_eq!(
        status, 200,
        "the reservation {name} of {target_dpus} must be created, got {status} with {body:?}"
    );
    await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
}

/// The requested amount a read of the reservation `name` reports, or `None` where the account
/// holds no reservation under that name.
async fn requested(router: &MultiServiceRouter, name: &str) -> Option<i64> {
    let (status, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
    if status != 200 {
        return None;
    }
    body["CapacityReservation"]["TargetDpus"].as_i64()
}

/// RULE over the CREATE path: an amount that fits in what the account has left is served and an
/// amount past it is refused with Athena's sentence for the quota, with no reservation created.
#[tokio::test]
async fn a_create_past_the_accounts_provisioned_dpu_quota_is_refused() {
    // (case, the amount asked for, whether the account has that much left)
    let rows: [(&str, i32, bool); 5] = [
        ("one block of a fresh account", ONE_BLOCK, true),
        (
            "the rest of the quota after that block",
            MAX_ACCOUNT_PROVISIONED_DPUS - ONE_BLOCK,
            true,
        ),
        ("one more block once the quota is held", ONE_BLOCK, false),
        (
            "the whole quota again once it is held",
            MAX_ACCOUNT_PROVISIONED_DPUS,
            false,
        ),
        ("an amount far past the quota", 1_000_000, false),
    ];

    let router = router();
    for (case, target_dpus, inside) in rows {
        let name = format!("quota-{case}").replace(' ', "-");
        let (status, body) = create(&router, &name, target_dpus).await;
        if inside {
            assert_eq!(
                status, 200,
                "case {case:?}: {target_dpus} data processing units must be reserved, got \
                 {status} with {body:?}"
            );
            assert_eq!(
                requested(&router, &name).await,
                Some(i64::from(target_dpus)),
                "case {case:?}: the reservation must hold the amount it asked for"
            );
            continue;
        }
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &account_provisioned_dpus_exceeded(),
        );
        // The refused create registered nothing: a read of that name misses, in the sentence
        // this service states a reservation miss with.
        let (read_status, read) =
            call(&router, "GetCapacityReservation", &json!({ "Name": name })).await;
        assert_refusal(
            case,
            read_status,
            &read,
            ERROR_CODE_INVALID_INPUT,
            &capacity_reservation_not_found(&name),
        );
    }
}

/// RULE over the UPDATE path: the amount an update asks for REPLACES what its own reservation
/// requests rather than adding to it, so an update inside the quota is served and one past it is
/// refused with the same sentence, leaving the amount the reservation already holds.
#[tokio::test]
async fn an_update_is_weighed_against_the_quota_beside_the_reservations_own_amount() {
    let router = router();
    // Half the quota is held by another reservation, so what this one may ask for is the other
    // half however much it itself already holds.
    given_active(&router, "held-elsewhere", MAX_ACCOUNT_PROVISIONED_DPUS / 2).await;
    given_active(&router, "updated", ONE_BLOCK).await;

    // (case, the amount the update asks for, whether the account has that much left)
    let rows: [(&str, i32, bool); 5] = [
        ("the same amount again", ONE_BLOCK, true),
        (
            "the whole half the account has left",
            MAX_ACCOUNT_PROVISIONED_DPUS / 2,
            true,
        ),
        (
            "one block past that half",
            MAX_ACCOUNT_PROVISIONED_DPUS / 2 + ONE_BLOCK,
            false,
        ),
        (
            "the whole quota",
            MAX_ACCOUNT_PROVISIONED_DPUS,
            false,
        ),
        ("an amount far past the quota", 1_000_000, false),
    ];

    let mut held = ONE_BLOCK;
    for (case, target_dpus, inside) in rows {
        // Only an allocated reservation takes an update at all, so the row would otherwise be
        // refused for the status rather than for the quota.
        await_reservation_status(&router, "updated", CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        let (status, body) = update(&router, "updated", target_dpus).await;
        if inside {
            assert_eq!(
                status, 200,
                "case {case:?}: an update to {target_dpus} must be served, got {status} with \
                 {body:?}"
            );
            held = target_dpus;
        } else {
            assert_refusal(
                case,
                status,
                &body,
                ERROR_CODE_INVALID_INPUT,
                &account_provisioned_dpus_exceeded(),
            );
        }
        assert_eq!(
            requested(&router, "updated").await,
            Some(i64::from(held)),
            "case {case:?}: expected the reservation to request {held}"
        );
    }
}

/// RULE: a cancellation RELEASES the capacity its reservation requested, so the account may ask
/// for it again — the quota is about the capacity the account HOLDS rather than about every
/// reservation it ever made.
#[tokio::test]
async fn capacity_a_cancellation_releases_can_be_requested_again() {
    let router = router();
    given_active(&router, "the-whole-quota", MAX_ACCOUNT_PROVISIONED_DPUS).await;

    // While it is held, nothing more is granted.
    let (status, body) = create(&router, "while-held", ONE_BLOCK).await;
    assert_refusal(
        "one block while the whole quota is held",
        status,
        &body,
        ERROR_CODE_INVALID_INPUT,
        &account_provisioned_dpus_exceeded(),
    );

    let (status, cancelled) = call(
        &router,
        "CancelCapacityReservation",
        &json!({ "Name": "the-whole-quota" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation must be cancellable, got {status} with {cancelled:?}"
    );

    // The release is complete when the clock makes it due, and the capacity is the account's
    // again from then on — polled for rather than slept through.
    await_reservation_status(
        &router,
        "the-whole-quota",
        CAPACITY_RESERVATION_STATUS_CANCELLED,
    )
    .await;

    // (case, the amount asked for after the release)
    for (case, target_dpus) in [
        ("one block", ONE_BLOCK),
        (
            "the rest of the quota",
            MAX_ACCOUNT_PROVISIONED_DPUS - ONE_BLOCK,
        ),
    ] {
        let name = format!("released-{target_dpus}");
        let (status, body) = create(&router, &name, target_dpus).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the released capacity must be grantable again, got {status} with \
             {body:?}"
        );
        assert_eq!(
            requested(&router, &name).await,
            Some(i64::from(target_dpus)),
            "case {case:?}: the new reservation must hold what it asked for"
        );
    }
}

/// RULE: the quota is stated in ONE sentence whatever the amount asked for — fixed text that
/// quotes neither the amount, the account's remaining capacity nor the quota itself.
#[tokio::test]
async fn the_quota_refusal_states_one_sentence_for_every_amount() {
    let router = router();
    given_active(&router, "holder", MAX_ACCOUNT_PROVISIONED_DPUS).await;

    // (case, the operation, the reservation addressed, the amount asked for) — every row is
    // past what the account has left, the whole quota being held: a create asks for capacity
    // beside the quota the holder already requests, and an update asks the HOLDER itself for
    // more than the quota.
    let rows: [(&str, &str, &str, i32); 5] = [
        (
            "one block beside the whole quota",
            "CreateCapacityReservation",
            "another-block",
            ONE_BLOCK,
        ),
        (
            "a second whole quota",
            "CreateCapacityReservation",
            "another-quota",
            MAX_ACCOUNT_PROVISIONED_DPUS,
        ),
        (
            "far past the quota",
            "CreateCapacityReservation",
            "another-far-past",
            4_000_000,
        ),
        (
            "one block past the quota on the reservation that holds it",
            "UpdateCapacityReservation",
            "holder",
            MAX_ACCOUNT_PROVISIONED_DPUS + ONE_BLOCK,
        ),
        (
            "far past the quota on the reservation that holds it",
            "UpdateCapacityReservation",
            "holder",
            4_000_000,
        ),
    ];

    for (case, operation, addressed, target_dpus) in rows {
        let (status, body) = call(
            &router,
            operation,
            &json!({ "Name": addressed, "TargetDpus": target_dpus }),
        )
        .await;
        assert_eq!(
            error_code(&body),
            "InvalidRequestException",
            "case {case:?} on {operation}: expected InvalidRequestException, got {status} with \
             {body:?}"
        );
        assert_eq!(
            message(&body),
            account_provisioned_dpus_exceeded(),
            "case {case:?} on {operation}: expected the quota sentence, got {status} with \
             {body:?}"
        );
    }

    // Nothing above changed what the account holds.
    assert_eq!(
        requested(&router, "holder").await,
        Some(i64::from(MAX_ACCOUNT_PROVISIONED_DPUS)),
        "the refused requests must leave the reservation requesting the whole quota"
    );
}
