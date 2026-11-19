// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for Athena's capacity assignment configuration:
//! `PutCapacityAssignmentConfiguration` assigns workgroups to a capacity reservation and
//! `GetCapacityAssignmentConfiguration` reads that assignment back — both driven over the
//! wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the same router the
//! emulator boots, so every assertion is about what an SDK client observes.
//!
//! The rule each test states is the documented one: a capacity reservation holds at most ONE
//! capacity assignment configuration, made up of the individual assignments the last accepted
//! put carried, and a put that is refused — for a reservation the account does not hold, or
//! for an assignment naming a workgroup it does not hold — stores nothing. Every test asserts
//! it over a TABLE of inputs whose rows differ along what the rule generalises over: how many
//! assignments a configuration is made of, how many workgroups each one names, which
//! workgroup (including the account's default one, which no create registered), the shape of
//! the name, whether a put follows an earlier one, and how many callers write at once.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    capacity_assignment_configuration_too_large, CAPACITY_RESERVATION_STATUS_ACTIVE,
    CAPACITY_RESERVATION_STATUS_CANCELLED, DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
    MAX_CAPACITY_ASSIGNMENT_ENTRIES, WORK_GROUPS_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, await_reservation_status, call, create_work_group, router};

/// The amount of provisioned capacity every reservation here asks for: a positive multiple of
/// the block size, so no row says anything about the requested amount by accident.
const SOME_TARGET_DPUS: i32 = 8;

/// The longest name the modelled `WorkGroupName` pattern admits
/// (`^[a-zA-Z0-9._-]{1,128}$`), over its whole character set.
fn longest_work_group_name() -> String {
    "aA0._-".chars().cycle().take(128).collect()
}

/// One configuration, as a table row carries it: one list of workgroup names per assignment.
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

/// The workgroups a reported configuration assigns, one list per assignment — the value a
/// round-trip is asserted on, read out of the answer rather than compared as raw JSON so the
/// assertion is about the assignments themselves.
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

/// Create every workgroup `assignments` names, except the account's default workgroup, which
/// exists without any create of its own.
async fn create_named_work_groups(router: &MultiServiceRouter, assignments: &Assignments) {
    for work_group in assignments.iter().flatten() {
        if work_group != DEFAULT_WORK_GROUP {
            create_work_group(router, work_group).await;
        }
    }
}

/// Put `assignments` as the capacity assignment configuration of `reservation`.
async fn put(router: &MultiServiceRouter, reservation: &str, assignments: &Value) -> (u16, Value) {
    call(
        router,
        "PutCapacityAssignmentConfiguration",
        &json!({
            "CapacityReservationName": reservation,
            "CapacityAssignments": assignments,
        }),
    )
    .await
}

/// Read the capacity assignment configuration of `reservation`: the status and the
/// `CapacityAssignmentConfiguration` or the refusal body.
async fn get(router: &MultiServiceRouter, reservation: &str) -> (u16, Value) {
    let (status, body) = call(
        router,
        "GetCapacityAssignmentConfiguration",
        &json!({ "CapacityReservationName": reservation }),
    )
    .await;
    if status != 200 {
        return (status, body);
    }
    (status, body["CapacityAssignmentConfiguration"].clone())
}

/// The round-trip table: a configuration made of one assignment and of several, each naming
/// one workgroup and several, including the account's default workgroup and an assignment
/// naming none at all, with workgroup names at both ends of the modelled `WorkGroupName`
/// pattern.
fn configuration_cases() -> Vec<(&'static str, Assignments)> {
    vec![
        (
            "one assignment naming one workgroup",
            groups(&[&["analytics"]]),
        ),
        (
            "one assignment naming several workgroups",
            groups(&[&["analytics", "reporting", "ad-hoc"]]),
        ),
        (
            "several assignments, one workgroup each",
            groups(&[&["analytics"], &["reporting"], &["ad-hoc"]]),
        ),
        (
            "an assignment naming the account's default workgroup",
            groups(&[&[DEFAULT_WORK_GROUP]]),
        ),
        (
            "names at both ends of the modelled workgroup pattern",
            vec![vec!["a".to_string(), longest_work_group_name()]],
        ),
        ("an assignment naming no workgroup", groups(&[&[]])),
        ("a configuration made of no assignment", groups(&[])),
    ]
}

#[tokio::test]
async fn a_get_reports_the_assignments_the_put_carried() {
    // The rule: the configuration a reservation holds is the assignments the last accepted
    // put carried, so a read reports those assignments and nothing else.
    for (case, assignments) in configuration_cases() {
        let router = router();
        let reservation = "round-trip";
        create_reservation(&router, reservation).await;
        create_named_work_groups(&router, &assignments).await;

        let (status, answer) = put(&router, reservation, &body_of(&assignments)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be accepted, got {status} with {answer:?}"
        );

        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            configuration["CapacityReservationName"].as_str(),
            Some(reservation),
            "case {case:?}: the configuration must name {reservation:?}, got {configuration:?}"
        );
        assert_eq!(
            assigned(&configuration),
            assignments,
            "case {case:?}: expected the assignments {assignments:?}, got {configuration:?}"
        );
    }
}

#[tokio::test]
async fn a_put_replaces_the_configuration_the_reservation_held() {
    // The rule: a reservation holds only ONE capacity assignment configuration, so a put
    // replaces the one it held rather than adding to it. The rows walk a sequence of
    // configurations differing in how many assignments they are made of, which workgroups
    // they name and in what order, each read back before the next put.
    let router = router();
    let reservation = "replaced";
    create_reservation(&router, reservation).await;
    for work_group in ["first", "second", "third"] {
        create_work_group(&router, work_group).await;
    }

    let sequence = vec![
        ("the first configuration", groups(&[&["first"]])),
        ("another workgroup entirely", groups(&[&["second"]])),
        ("two assignments", groups(&[&["first"], &["second"]])),
        (
            "the same two, reordered",
            groups(&[&["second"], &["first"]]),
        ),
        (
            "one assignment naming all three",
            groups(&[&["first", "second", "third"]]),
        ),
        ("no assignment at all", groups(&[])),
    ];

    for (case, assignments) in sequence {
        let (status, answer) = put(&router, reservation, &body_of(&assignments)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be accepted, got {status} with {answer:?}"
        );
        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            assigned(&configuration),
            assignments,
            "case {case:?}: the reservation must hold only the configuration just put, \
             expected {assignments:?}, got {configuration:?}"
        );
    }
}

#[tokio::test]
async fn a_get_of_a_reservation_no_put_configured_reports_no_assignment() {
    // The rule: the configuration belongs to the reservation, so every reservation the
    // account holds answers the read — with no assignment until a put makes one. The rows
    // differ along the status the reservation is in when it is read, a reservation being
    // readable while its first allocation is still in flight and once it is allocated.
    let router = router();
    create_reservation(&router, "fresh").await;

    for case in ["straight after the create", "once the reservation is allocated"] {
        if case == "once the reservation is allocated" {
            await_reservation_status(&router, "fresh", CAPACITY_RESERVATION_STATUS_ACTIVE).await;
        }
        let (status, configuration) = get(&router, "fresh").await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            configuration["CapacityReservationName"].as_str(),
            Some("fresh"),
            "case {case:?}: the configuration must name the reservation, got {configuration:?}"
        );
        assert!(
            assigned(&configuration).is_empty(),
            "case {case:?}: a reservation no put configured must assign no workgroup, got \
             {configuration:?}"
        );
    }
}

#[tokio::test]
async fn an_assignment_naming_a_work_group_this_account_does_not_hold_is_refused() {
    // The rule: a configuration assigns WORKGROUPS, so a put naming one the account does not
    // hold is refused and nothing is stored — the reservation keeps the configuration the
    // last accepted put left it. The rows differ along where the unknown name sits, how it is
    // spelled, and whether the account ever held it.
    const KNOWN: &str = "known-work-group";
    const DELETED: &str = "deleted-work-group";
    let baseline = groups(&[&[KNOWN]]);
    let cases = vec![
        ("the only name is unknown", groups(&[&["absent"]])),
        (
            "an unknown name beside a known one",
            groups(&[&[KNOWN, "absent"]]),
        ),
        (
            "the unknown name is in a later assignment",
            groups(&[&[KNOWN], &["absent"]]),
        ),
        (
            "an unknown name as long as the model admits",
            vec![vec![longest_work_group_name()]],
        ),
        (
            "a workgroup the account held and deleted",
            groups(&[&[DELETED]]),
        ),
    ];

    for (case, assignments) in cases {
        let router = router();
        let reservation = "refusing";
        create_reservation(&router, reservation).await;
        create_work_group(&router, KNOWN).await;
        // The reservation starts out holding a configuration, so a refused put that stored
        // the assignments it carried would be visible as a CHANGED configuration rather than
        // only as a missing one.
        let (status, answer) = put(&router, reservation, &body_of(&baseline)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the baseline put must be accepted, got {status} with {answer:?}"
        );
        if case == "a workgroup the account held and deleted" {
            create_work_group(&router, DELETED).await;
            let (status, answer) =
                call(&router, "DeleteWorkGroup", &json!({ "WorkGroup": DELETED })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the workgroup must be deleted, got {status} with {answer:?}"
            );
        }

        let (status, body) = put(&router, reservation, &body_of(&assignments)).await;
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            WORK_GROUPS_NOT_FOUND,
        );

        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            assigned(&configuration),
            baseline,
            "case {case:?}: a refused put must store nothing, expected the configuration to \
             still be {baseline:?}, got {configuration:?}"
        );
    }
}

#[tokio::test]
async fn a_configuration_larger_than_this_service_stores_is_refused() {
    // The rule: a configuration this service stores holds at most
    // MAX_CAPACITY_ASSIGNMENT_ENTRIES assignments and workgroup names together, so one at the
    // bound is stored and one past it is refused with nothing stored. The rows are the bound
    // itself, reached two ways, and one entry past it, also reached two ways — many names in
    // one assignment, and many assignments naming one workgroup each.
    let router = router();
    let reservation = "bounded";
    create_reservation(&router, reservation).await;
    // The account's default workgroup needs no create, so a configuration of any size can be
    // built out of names this account holds.
    let names = |count: usize| -> Assignments { vec![vec![DEFAULT_WORK_GROUP.to_string(); count]] };
    let spread = |count: usize| -> Assignments {
        (0..count)
            .map(|_| vec![DEFAULT_WORK_GROUP.to_string()])
            .collect()
    };

    let accepted = vec![
        (
            "one assignment, names up to the bound",
            names(MAX_CAPACITY_ASSIGNMENT_ENTRIES - 1),
        ),
        (
            "assignments up to the bound",
            spread(MAX_CAPACITY_ASSIGNMENT_ENTRIES / 2),
        ),
    ];
    for (case, assignments) in accepted {
        let (status, answer) = put(&router, reservation, &body_of(&assignments)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a configuration at the bound must be accepted, got {status} with \
             {answer:?}"
        );
        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            assigned(&configuration),
            assignments,
            "case {case:?}: the configuration at the bound must be stored whole"
        );
    }

    // The last accepted configuration, which a refused put must leave exactly as it is.
    let held = spread(MAX_CAPACITY_ASSIGNMENT_ENTRIES / 2);
    let refused = vec![
        (
            "one assignment, one name past the bound",
            names(MAX_CAPACITY_ASSIGNMENT_ENTRIES),
        ),
        (
            "one assignment past the bound",
            spread(MAX_CAPACITY_ASSIGNMENT_ENTRIES / 2 + 1),
        ),
    ];
    for (case, assignments) in refused {
        let (status, body) = put(&router, reservation, &body_of(&assignments)).await;
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &capacity_assignment_configuration_too_large(),
        );
        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must answer 200, got {status} with {configuration:?}"
        );
        assert_eq!(
            assigned(&configuration),
            held,
            "case {case:?}: a refused put must store nothing"
        );
    }
}

#[tokio::test]
async fn both_operations_report_a_reservation_this_account_does_not_hold_as_not_found() {
    // The rule: both operations address ONE capacity reservation by name, so a name the
    // account holds no reservation under is reported as not found by each of them. The rows
    // differ along the shape of the name and along how it came to name no reservation.
    let router = router();
    // A name that names something else entirely: a workgroup is not a reservation.
    create_work_group(&router, "a-work-group").await;
    // One reservation the account held and no longer does: only an allocated reservation can
    // be cancelled, and only a cancelled one can be deleted.
    create_reservation(&router, "gone").await;
    await_reservation_status(&router, "gone", CAPACITY_RESERVATION_STATUS_ACTIVE).await;
    let (status, answer) = call(
        &router,
        "CancelCapacityReservation",
        &json!({ "Name": "gone" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation must be cancelled, got {status} with {answer:?}"
    );
    await_reservation_status(&router, "gone", CAPACITY_RESERVATION_STATUS_CANCELLED).await;
    let (status, answer) = call(
        &router,
        "DeleteCapacityReservation",
        &json!({ "Name": "gone" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the reservation must be deleted, got {status} with {answer:?}"
    );

    let missing = vec![
        ("never created", "no-such-reservation".to_string()),
        ("the shortest name the model admits", "a".to_string()),
        (
            "the longest name the model admits",
            "aA0._-".chars().cycle().take(128).collect(),
        ),
        ("a name that names a workgroup", "a-work-group".to_string()),
        ("a reservation that was deleted", "gone".to_string()),
    ];

    for (case, name) in missing {
        let not_found = format!("Reservation with name {name} not found.");

        let (status, body) = put(&router, &name, &body_of(&groups(&[&["a-work-group"]]))).await;
        assert_refusal(
            &format!("put, {case}"),
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &not_found,
        );

        let (status, body) = get(&router, &name).await;
        assert_refusal(
            &format!("get, {case}"),
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &not_found,
        );
    }
}

#[tokio::test]
async fn concurrent_puts_are_each_accepted_and_the_reservation_holds_one_of_them() {
    // The rule: a reservation holds one WHOLE configuration, so concurrent puts are
    // serialised — every one is answered, and the configuration left behind is one caller's
    // own rather than a mixture of several. The rows differ along how many callers race.
    for racers in [2_usize, 8] {
        let router = Arc::new(router());
        let reservation = "raced";
        create_reservation(&router, reservation).await;
        let work_groups: Vec<String> = (0..racers).map(|racer| format!("racer-{racer}")).collect();
        for work_group in &work_groups {
            create_work_group(&router, work_group).await;
        }

        let mut racing = Vec::new();
        for work_group in work_groups.clone() {
            let router = Arc::clone(&router);
            racing.push(tokio::spawn(async move {
                put(
                    &router,
                    "raced",
                    &json!([{ "WorkGroupNames": [work_group] }]),
                )
                .await
            }));
        }

        for (racer, task) in racing.into_iter().enumerate() {
            let (status, body) = task.await.expect("the put task runs to completion");
            assert_eq!(
                status, 200,
                "case {racers} racers: racer {racer} must be accepted, got {status} with \
                 {body:?}"
            );
        }

        let (status, configuration) = get(&router, reservation).await;
        assert_eq!(
            status, 200,
            "case {racers} racers: the read must answer 200, got {status} with \
             {configuration:?}"
        );
        let held = assigned(&configuration);
        let whole: Vec<Assignments> = work_groups
            .iter()
            .map(|work_group| vec![vec![work_group.clone()]])
            .collect();
        assert!(
            whole.contains(&held),
            "case {racers} racers: the reservation must hold exactly one racer's whole \
             configuration, one of {whole:?}, got {configuration:?}"
        );
    }
}
