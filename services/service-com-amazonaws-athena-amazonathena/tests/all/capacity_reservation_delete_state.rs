// SPDX-License-Identifier: CC0-1.0

//! The STATE a refused `DeleteCapacityReservation` names, driven end to end over the same
//! router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: `DeleteCapacityReservation` serves a reservation only from the one
//! state a deletion is allowed from ("A reservation must be cancelled before it can be
//! deleted"), and a delete that meets any other state is refused with
//! `InvalidRequestException` (HTTP 400) whose message states THAT state beside the one it
//! needs — so a client can tell a state it should wait out from one it must act on. The
//! refused delete takes no effect: the reservation stays readable in the state that refused
//! it.
//!
//! The rule is asserted over a TABLE whose rows differ along the state the delete meets:
//! every non-terminal state of the lifecycle (an allocation in flight, an allocated
//! reservation, a re-allocation in flight, a cancellation in flight) plus the state the
//! delete IS served from.
//!
//! Nothing here waits for a fixed span: the one row that needs a transition the clock owes
//! POLLS the service for the state it is waiting on, under a deadline far above the span
//! that transition is due after.

use serde_json::json;
use service_com_amazonaws_athena_amazonathena::state::{
    self, CAPACITY_RESERVATION_STATUS_ACTIVE, CAPACITY_RESERVATION_STATUS_CANCELLED,
    CAPACITY_RESERVATION_STATUS_CANCELLING, CAPACITY_RESERVATION_STATUS_PENDING,
    CAPACITY_RESERVATION_STATUS_UPDATE_PENDING, ERROR_CODE_INVALID_INPUT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, await_reservation_status, call, message, router};

/// The amount of capacity every reservation below asks for: a multiple of the block size
/// Athena allocates in, above the modelled floor.
const TARGET_DPUS: i32 = 8;

/// Create the reservation `name`, which leaves it with its first allocation in flight.
async fn given_reservation(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": TARGET_DPUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation {name} must be created, got {status} with {created:?}"
    );
}

/// The `Status` the reservation `name` reports, or the refusal's HTTP status.
async fn status_of(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
    if status != 200 {
        return format!("<{status}>");
    }
    body["CapacityReservation"]["Status"]
        .as_str()
        .unwrap_or("<no Status>")
        .to_string()
}

/// Drive the reservation `name` into `state` and hand it back once it reports it.
///
/// A state the clock owes is waited for by POLLING for it; a state the previous call leaves
/// behind is already there when its call returns.
async fn given_reservation_in(router: &MultiServiceRouter, name: &str, wanted: &str) {
    given_reservation(router, name).await;
    match wanted {
        CAPACITY_RESERVATION_STATUS_PENDING => {}
        CAPACITY_RESERVATION_STATUS_ACTIVE => {
            await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        }
        CAPACITY_RESERVATION_STATUS_UPDATE_PENDING => {
            await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
            let (status, updated) = call(
                router,
                "UpdateCapacityReservation",
                &json!({ "Name": name, "TargetDpus": TARGET_DPUS * 2 }),
            )
            .await;
            assert_eq!(
                status, 200,
                "the update of {name} must be accepted, got {status} with {updated:?}"
            );
        }
        CAPACITY_RESERVATION_STATUS_CANCELLING => {
            await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
            let (status, cancelled) = call(
                router,
                "CancelCapacityReservation",
                &json!({ "Name": name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "the cancel of {name} must be accepted, got {status} with {cancelled:?}"
            );
        }
        CAPACITY_RESERVATION_STATUS_CANCELLED => {
            await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
            let (status, cancelled) = call(
                router,
                "CancelCapacityReservation",
                &json!({ "Name": name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "the cancel of {name} must be accepted, got {status} with {cancelled:?}"
            );
            await_reservation_status(router, name, CAPACITY_RESERVATION_STATUS_CANCELLED).await;
        }
        other => panic!("no row drives a reservation into {other}"),
    }
    assert_eq!(
        status_of(router, name).await,
        wanted,
        "the reservation {name} must be in {wanted} before the delete"
    );
}

/// A delete refused by the reservation's state names that state, leaves the reservation as
/// it was, and the one state a delete IS served from deletes it.
#[tokio::test]
async fn a_refused_delete_names_the_state_it_met_and_changes_nothing() {
    let router = router();

    // Rows differ along the state the delete meets: each non-terminal state of the
    // lifecycle, every one of which refuses the delete and names ITSELF.
    let refusing = [
        CAPACITY_RESERVATION_STATUS_PENDING,
        CAPACITY_RESERVATION_STATUS_ACTIVE,
        CAPACITY_RESERVATION_STATUS_UPDATE_PENDING,
        CAPACITY_RESERVATION_STATUS_CANCELLING,
    ];

    let mut sentences: Vec<String> = Vec::new();
    for state_met in refusing {
        let name = format!("delete-from-{}", state_met.to_lowercase());
        given_reservation_in(&router, &name, state_met).await;

        let (status, refused) = call(
            &router,
            "DeleteCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_refusal(
            state_met,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &state::capacity_reservation_not_cancelled(state_met),
        );
        // The state the delete met is IN the sentence, and the one state a delete is served
        // from is named beside it.
        assert!(
            message(&refused).contains(state_met),
            "case {state_met:?}: the refusal must name the state it met, got {:?}",
            message(&refused)
        );
        assert!(
            message(&refused).contains(CAPACITY_RESERVATION_STATUS_CANCELLED),
            "case {state_met:?}: the refusal must name the state a delete needs, got {:?}",
            message(&refused)
        );
        // The reservation's own name is not part of the sentence: the request already
        // carries it, and the state is the one value that varies.
        assert!(
            !message(&refused).contains(name.as_str()),
            "case {state_met:?}: the refusal must not quote the reservation's name, got {:?}",
            message(&refused)
        );
        // The refused delete took no effect: the reservation is still readable, still in the
        // state that refused it.
        assert_eq!(
            status_of(&router, &name).await,
            state_met,
            "case {state_met:?}: a refused delete must leave the reservation as it was"
        );
        sentences.push(message(&refused));
    }

    // Each state reports ITSELF, so no two of those refusals read alike.
    for (left, right) in sentences.iter().zip(refusing.iter()) {
        assert!(
            left.contains(right),
            "the refusal {left:?} must name {right:?}"
        );
    }
    assert_eq!(
        sentences
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        refusing.len(),
        "every state must read differently, got {sentences:?}"
    );

    // And the one state a delete IS served from: the reservation goes, so every later read
    // of the name reports it absent.
    let cancelled = "delete-from-cancelled";
    given_reservation_in(&router, cancelled, CAPACITY_RESERVATION_STATUS_CANCELLED).await;
    let (status, deleted) = call(
        &router,
        "DeleteCapacityReservation",
        &json!({ "Name": cancelled }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a cancelled reservation must be deletable, got {status} with {deleted:?}"
    );
    let (status, refused) = call(
        &router,
        "GetCapacityReservation",
        &json!({ "Name": cancelled }),
    )
    .await;
    assert_refusal(
        "after the delete",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        &state::capacity_reservation_not_found(cancelled),
    );
}
