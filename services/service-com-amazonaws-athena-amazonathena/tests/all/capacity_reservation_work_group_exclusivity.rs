// SPDX-License-Identifier: CC0-1.0

//! One workgroup's capacity is served by at most ONE capacity reservation.
//!
//! The rule under test: **a `PutCapacityAssignmentConfiguration` naming a workgroup that
//! another reservation still holding its capacity assigns is refused with this operation's
//! own `InvalidRequestException` and stores nothing; a workgroup no live reservation serves —
//! because none ever did, because the reservation that did dropped it from its own
//! configuration, or because that reservation's capacity was released — is assignable.** A
//! reservation's own configuration is not in its own way: re-putting it is accepted.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.
//!
//! The rows differ along what the rule generalises over: where the claimed name sits in the
//! configuration, how many names the two configurations carry, which workgroup is claimed
//! (including the account's default one, which no create registered), the status the holder
//! is in when the claim is made, and how a workgroup came to be free again.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    CAPACITY_RESERVATION_STATUS_ACTIVE, CAPACITY_RESERVATION_STATUS_CANCELLED, DEFAULT_WORK_GROUP,
    ERROR_CODE_INVALID_INPUT, WORK_GROUPS_ASSIGNED_TO_ANOTHER_RESERVATION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, await_reservation_status, call, create_work_group, router};

/// The amount of provisioned capacity every reservation here asks for: a positive multiple of
/// the block size, so no row says anything about the requested amount by accident.
const SOME_TARGET_DPUS: i32 = 8;

/// The reservation that holds the workgroups first, and the one that tries to claim one of
/// them.
const HOLDER: &str = "holder";
/// See [`HOLDER`].
const CLAIMANT: &str = "claimant";

/// The workgroups the rows assign: one the holder takes, two more for a configuration that
/// names several, and one the claimant holds of its own so a refused put is visible as a
/// configuration that did NOT change rather than only as a missing one.
const WORK_GROUPS: &[&str] = &["alpha", "beta", "gamma", "spare"];

/// One configuration, as a row carries it: one list of workgroup names per assignment.
type Assignments = Vec<Vec<String>>;

/// The assignments `groups` names, as a row holds them.
fn groups(groups: &[&[&str]]) -> Assignments {
    groups
        .iter()
        .map(|names| names.iter().map(|name| (*name).to_string()).collect())
        .collect()
}

/// The `CapacityAssignments` member a configuration made of `assignments` is sent as.
fn body_of(assignments: &Assignments) -> Value {
    Value::Array(
        assignments
            .iter()
            .map(|work_groups| json!({ "WorkGroupNames": work_groups }))
            .collect(),
    )
}

/// The workgroups a reported configuration assigns, one list per assignment.
fn assigned(configuration: &Value) -> Assignments {
    configuration["CapacityAssignments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|assignment| {
            assignment["WorkGroupNames"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .collect()
}

/// Create the capacity reservation `name` and assert it was accepted.
async fn create_reservation(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "CreateCapacityReservation",
        &json!({ "Name": name, "TargetDpus": SOME_TARGET_DPUS }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation {name} must be created, got {status} with {body:?}"
    );
}

/// Put `assignments` as the capacity assignment configuration of `reservation`.
async fn put(
    router: &MultiServiceRouter,
    reservation: &str,
    assignments: &Assignments,
) -> (u16, Value) {
    call(
        router,
        "PutCapacityAssignmentConfiguration",
        &json!({
            "CapacityReservationName": reservation,
            "CapacityAssignments": body_of(assignments),
        }),
    )
    .await
}

/// Put `assignments` on `reservation` and assert the put was accepted.
async fn put_accepted(
    router: &MultiServiceRouter,
    case: &str,
    reservation: &str,
    assignments: &Assignments,
) {
    let (status, body) = put(router, reservation, assignments).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put of {assignments:?} on {reservation} must be accepted, got \
         {status} with {body:?}"
    );
}

/// The capacity assignment configuration `reservation` holds, read back through the operation
/// that projects it.
async fn held(router: &MultiServiceRouter, case: &str, reservation: &str) -> Assignments {
    let (status, body) = call(
        router,
        "GetCapacityAssignmentConfiguration",
        &json!({ "CapacityReservationName": reservation }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read of {reservation}'s configuration must answer 200, got {status} \
         with {body:?}"
    );
    assigned(&body["CapacityAssignmentConfiguration"])
}

/// A router holding the workgroups the rows name, the two reservations, and the claimant's own
/// configuration — the state every row starts from.
async fn two_reservations() -> MultiServiceRouter {
    let router = router();
    for work_group in WORK_GROUPS {
        create_work_group(&router, work_group).await;
    }
    create_reservation(&router, HOLDER).await;
    create_reservation(&router, CLAIMANT).await;
    router
}

/// The configuration the claimant holds before it tries to claim anything, made of a workgroup
/// no row claims — so a refused put that stored what it carried would be visible as a CHANGED
/// configuration.
fn claimants_own() -> Assignments {
    groups(&[&["spare"]])
}

#[tokio::test]
async fn a_put_claiming_a_work_group_another_live_reservation_serves_is_refused() {
    // The rule: a workgroup is served by at most one reservation that still holds its
    // capacity, so a put naming one another such reservation assigns is refused and stores
    // nothing — neither configuration changes. The rows differ along where the claimed name
    // sits, how many names each configuration carries, which workgroup is claimed, and the
    // status the holder is in when the claim is made.
    //
    // (case, what the holder assigns, what the claimant tries to claim, whether the holder's
    // capacity is allocated first)
    let rows: Vec<(&str, Assignments, Assignments, bool)> = vec![
        (
            "the claim names the one workgroup the holder serves",
            groups(&[&["alpha"]]),
            groups(&[&["alpha"]]),
            true,
        ),
        (
            "a claimed name beside a free one",
            groups(&[&["alpha"]]),
            groups(&[&["beta", "alpha"]]),
            true,
        ),
        (
            "the claimed name in a later assignment",
            groups(&[&["alpha"]]),
            groups(&[&["beta"], &["alpha"]]),
            true,
        ),
        (
            "the holder serves several workgroups, one of them claimed",
            groups(&[&["alpha", "beta", "gamma"]]),
            groups(&[&["beta"]]),
            true,
        ),
        (
            "the holder serves them through several assignments",
            groups(&[&["alpha"], &["beta"], &["gamma"]]),
            groups(&[&["gamma"]]),
            true,
        ),
        (
            "the claim names it twice",
            groups(&[&["alpha"]]),
            groups(&[&["alpha", "alpha"]]),
            true,
        ),
        (
            "the account's default workgroup",
            groups(&[&[DEFAULT_WORK_GROUP]]),
            groups(&[&[DEFAULT_WORK_GROUP]]),
            true,
        ),
        (
            // A reservation whose first allocation is still in flight holds the capacity it
            // asked for just as an allocated one does, so the workgroups it serves are not
            // free for another reservation to claim.
            "the holder is still allocating its capacity",
            groups(&[&["alpha"]]),
            groups(&[&["alpha"]]),
            false,
        ),
    ];

    for (case, holders, claim, allocated) in rows {
        let router = two_reservations().await;
        put_accepted(&router, case, CLAIMANT, &claimants_own()).await;
        put_accepted(&router, case, HOLDER, &holders).await;
        if allocated {
            await_reservation_status(&router, HOLDER, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        }

        let (status, body) = put(&router, CLAIMANT, &claim).await;
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            WORK_GROUPS_ASSIGNED_TO_ANOTHER_RESERVATION,
        );

        assert_eq!(
            held(&router, case, CLAIMANT).await,
            claimants_own(),
            "case {case:?}: a refused put must store nothing"
        );
        assert_eq!(
            held(&router, case, HOLDER).await,
            holders,
            "case {case:?}: a refused put must leave the holder's own configuration alone"
        );
    }
}

#[tokio::test]
async fn a_work_group_no_live_reservation_serves_is_assignable() {
    // The rule's other side: only a workgroup another LIVE reservation serves is in the way.
    // The rows differ along why the claimed workgroup is free — nobody ever served it, the
    // reservation that served it dropped it, or that reservation's capacity was released, by
    // the cancellation alone and by the delete that follows it — and one row claims through
    // the SAME reservation that already serves it, which is not another reservation at all.
    for case in [
        "a workgroup no reservation ever served",
        "the reservation that served it dropped it",
        "the reservation that served it was re-pointed at another workgroup",
        "the holder's cancellation was asked for",
        "the holder was cancelled and deleted",
        "the reservation that serves it re-puts its own configuration",
    ] {
        let router = two_reservations().await;
        let claim = groups(&[&["alpha"]]);

        // Every row but the first starts with the holder really serving the workgroup the
        // claim names.
        if case != "a workgroup no reservation ever served" {
            put_accepted(&router, case, HOLDER, &claim).await;
            await_reservation_status(&router, HOLDER, CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        }

        let claiming = match case {
            "the reservation that served it dropped it" => {
                put_accepted(&router, case, HOLDER, &groups(&[])).await;
                CLAIMANT
            }
            "the reservation that served it was re-pointed at another workgroup" => {
                put_accepted(&router, case, HOLDER, &groups(&[&["beta"]])).await;
                CLAIMANT
            }
            // A cancellation releases the capacity the reservation held, so the workgroups it
            // served are free from the moment it is asked for — whether the cancellation has
            // completed by the time the claim is made or not.
            "the holder's cancellation was asked for" => {
                let (status, body) =
                    call(&router, "CancelCapacityReservation", &json!({ "Name": HOLDER })).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the cancellation must be accepted, got {status} with {body:?}"
                );
                CLAIMANT
            }
            "the holder was cancelled and deleted" => {
                let (status, body) =
                    call(&router, "CancelCapacityReservation", &json!({ "Name": HOLDER })).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the cancellation must be accepted, got {status} with {body:?}"
                );
                await_reservation_status(&router, HOLDER, CAPACITY_RESERVATION_STATUS_CANCELLED)
                    .await;
                let (status, body) =
                    call(&router, "DeleteCapacityReservation", &json!({ "Name": HOLDER })).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the delete must be accepted, got {status} with {body:?}"
                );
                CLAIMANT
            }
            "the reservation that serves it re-puts its own configuration" => HOLDER,
            _ => CLAIMANT,
        };

        put_accepted(&router, case, claiming, &claim).await;
        assert_eq!(
            held(&router, case, claiming).await,
            claim,
            "case {case:?}: the accepted put's configuration must be the one {claiming} holds"
        );
    }
}
