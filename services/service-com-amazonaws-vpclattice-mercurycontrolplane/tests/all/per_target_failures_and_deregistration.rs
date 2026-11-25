// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the per-target outcome of the target slice: what a registration
//! reports for ONE target it cannot register, what a deregistration does to the stored
//! membership, and which `reasonCode` a target's status carries.
//!
//! Every case drives the service the way a client does — a signed restJson1 request
//! through the router that mounts it — and reads the membership back out of a LATER
//! `ListTargets`, so nothing here can pass on a canned body.
//!
//! The harness and the registration/listing helpers are the ones the `targets` suite
//! already builds; nothing is duplicated here.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one row:
//!
//! 1. A target this service accepts the SHAPE of but cannot register fails on its own: the
//!    call still answers HTTP 200, that target is reported under `unsuccessful` with a
//!    `failureCode` and `failureMessage`, and every other target of the same call is stored
//!    and reported under `successful`. The admission rule is ownership — a target whose id
//!    is another service's ARN belongs to the account that ARN names, and this service
//!    registers only a target of the caller's own account.
//! 2. A target's `status` carries the `reasonCode` that says which case that status is: the
//!    health-check condition of the target GROUP is read first and independently of what
//!    routes to the group, so a group whose type runs no health checks reports
//!    `UNAVAILABLE`/`HealthCheckNotSupported` and one that has health checking turned off
//!    reports `UNAVAILABLE`/`HealthCheckDisabled`, while a status with only one case
//!    carries no reason code.
//! 3. A deregistered target leaves the target group's stored membership: at once where its
//!    type's targets have no connections to drain, and after the documented draining time
//!    (reporting `DRAINING` until then) where they do — so a later `ListTargets` reports
//!    the membership the deregistration left, and the room the target held is free again.
//!    The targets the group HELD are reported under `successful`; a target it holds no
//!    registration of is reported under `unsuccessful` with the `TargetNotFound` failure
//!    code and the failure class's own fixed `failureMessage`, and leaves the stored
//!    membership exactly as it was (`DeregisterTargets` is `@idempotent`: a retry changes
//!    nothing).

use service_com_amazonaws_vpclattice_mercurycontrolplane::state;

use crate::listeners::a_service;
use crate::resource_not_found_messages::{absent_id, assert_not_found};
use crate::targets::{
    a_target_group, forward_to, list_targets, listed_pairs, pairs_of, register_targets, vpc_config,
    ANOTHER_LAMBDA, AN_ALB, A_LAMBDA,
};
use crate::vpclattice_service_slice::{message_of, Answer, Lattice};

/// A Lambda function ARN of ANOTHER account: a target this service accepts the shape of
/// (it is a Lambda function ARN) but cannot register.
const FOREIGN_LAMBDA: &str = "arn:aws:lambda:us-west-2:111111111111:function:not-mine";

/// A second Lambda function ARN of another account, of a third account again; see
/// [`FOREIGN_LAMBDA`].
const ANOTHER_FOREIGN_LAMBDA: &str = "arn:aws:lambda:us-west-2:222222222222:function:not-mine-2";

/// An Application Load Balancer ARN of another account; see [`FOREIGN_LAMBDA`].
const FOREIGN_ALB: &str =
    "arn:aws:elasticloadbalancing:us-west-2:333333333333:loadbalancer/app/theirs/1234567890abcdef";

/// How the service reports a target that names a resource of another account: the label of
/// what the target is, the submitted id, and the CALLER's own account.
fn not_owned(label: &str, id: &str) -> String {
    format!(
        "{label} {id} is not owned by account {}.",
        state::account_id()
    )
}

/// One row of the per-target admission table: the case, the target-group type, the config
/// the group is created with, the targets the registration sends, the (id, port) pairs it
/// has to STORE, and the (id, label of what the target is, port) of each target it has to
/// report as failed.
type AdmissionCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    serde_json::Value,
    Vec<(&'static str, Option<i64>)>,
    Vec<(&'static str, &'static str, Option<i64>)>,
);

/// One row of the reason-code table: the case, the target-group type, the config the group
/// is created with, the target registered with it, and the `status` and `reasonCode` that
/// target has to report.
type ReasonCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// One row of the deregistration table: the case, the target-group type, the config the
/// group is created with, the two targets it registers in turn, and whether that type's
/// targets are reached over connections that drain.
type DeregistrationCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    [&'static str; 2],
    bool,
);

/// One row of the not-registered table: the case, the targets the deregistration names,
/// and the (id, port) each per-target failure has to report.
type NotRegisteredCase = (
    &'static str,
    serde_json::Value,
    Vec<(&'static str, Option<i64>)>,
);

/// `POST /targetgroups/{targetGroupIdentifier}/deregistertargets`.
async fn deregister_targets(
    lattice: &Lattice,
    identifier: &str,
    targets: serde_json::Value,
) -> Answer {
    lattice
        .call(
            "POST",
            &format!("/targetgroups/{identifier}/deregistertargets"),
            Some(serde_json::json!({ "targets": targets })),
        )
        .await
}

/// The (id, failureCode, failureMessage, port) of each entry of a failure list.
fn failures_of(case: &str, answer: &Answer) -> Vec<(String, String, String, Option<i64>)> {
    let entries = answer.body["unsuccessful"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected unsuccessful to be a list, got {}",
                answer.body
            )
        });
    entries
        .iter()
        .map(|entry| {
            let field = |member: &str| {
                entry[member]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a TargetFailure without a {member}: {entry}")
                    })
                    .to_string()
            };
            (
                field("id"),
                field("failureCode"),
                field("failureMessage"),
                entry["port"].as_i64(),
            )
        })
        .collect()
}

/// The (id, status, reasonCode) of every target `ListTargets` reports for `identifier`.
async fn summaries(
    lattice: &Lattice,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, Option<String>)> {
    let listed = list_targets(lattice, identifier, &[], None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListTargets, got {} / {}",
        listed.status, listed.body
    );
    let items = listed.body["items"].as_array().cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected items to be a list, got {}",
            listed.body
        )
    });
    let mut reported: Vec<(String, String, Option<String>)> = items
        .iter()
        .map(|item| {
            (
                item["id"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a TargetSummary without an id: {item}")
                    })
                    .to_string(),
                item["status"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a TargetSummary without a status: {item}")
                    })
                    .to_string(),
                item["reasonCode"].as_str().map(str::to_string),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// The targets the `targets` suite registers are in the caller's own account, which is
/// what makes them registrable at all — pinned here so neither constant can drift into
/// another account without this suite saying so.
#[test]
fn the_registrable_targets_of_this_crates_suites_name_the_callers_account() {
    for (case, target) in [
        ("a function", A_LAMBDA),
        ("a second function", ANOTHER_LAMBDA),
        ("a load balancer", AN_ALB),
    ] {
        let account = target.split(':').nth(4).unwrap_or_default();
        assert_eq!(
            account,
            state::account_id(),
            "case {case:?}: {target} must name the caller's own account"
        );
    }
    for (case, target) in [
        ("a function of another account", FOREIGN_LAMBDA),
        ("a third account's function", ANOTHER_FOREIGN_LAMBDA),
        ("a load balancer of another account", FOREIGN_ALB),
    ] {
        let account = target.split(':').nth(4).unwrap_or_default();
        assert_ne!(
            account,
            state::account_id(),
            "case {case:?}: {target} must name an account other than the caller's"
        );
    }
}

/// Rule 1, over a table that varies the target-group type, what a target of it is, and
/// whether the account the target names is the caller's.
#[tokio::test]
async fn a_target_this_service_cannot_register_fails_on_its_own_and_the_others_are_stored() {
    let lattice = Lattice::new();

    // (case, type, config, submitted targets, the ids that must be STORED, the per-target
    // failures the answer must report as (id, failureCode, failureMessage, port))
    let cases: Vec<AdmissionCase> = vec![
        (
            "a function of another account, alone",
            "LAMBDA",
            None,
            serde_json::json!([{ "id": FOREIGN_LAMBDA }]),
            vec![],
            // A port does not apply to a function target, so the failure reports none.
            vec![(FOREIGN_LAMBDA, "Lambda", None)],
        ),
        (
            "a function of the caller's own account",
            "LAMBDA",
            None,
            serde_json::json!([{ "id": A_LAMBDA }]),
            vec![(A_LAMBDA, None)],
            vec![],
        ),
        (
            "two load balancers, one of another account",
            "ALB",
            Some(vpc_config("HTTPS", Some(443))),
            serde_json::json!([{ "id": AN_ALB }, { "id": FOREIGN_ALB }]),
            vec![(AN_ALB, Some(443))],
            vec![(FOREIGN_ALB, "Application Load Balancer", Some(443))],
        ),
        (
            "an address names no account to own",
            "IP",
            Some(vpc_config("HTTP", Some(80))),
            serde_json::json!([{ "id": "10.0.0.7" }]),
            vec![("10.0.0.7", Some(80))],
            vec![],
        ),
        (
            "an instance id names no account to own",
            "INSTANCE",
            Some(vpc_config("HTTP", Some(9000))),
            serde_json::json!([{ "id": "i-0123456789abcdef0" }]),
            vec![("i-0123456789abcdef0", Some(9000))],
            vec![],
        ),
    ];

    for (index, (case, group_type, config, targets, stored, failed)) in
        cases.into_iter().enumerate()
    {
        let (id, _) = a_target_group(&lattice, &format!("per-target-{index}"), group_type, config)
            .await;
        let answer = register_targets(&lattice, &id, targets).await;
        // A target the service cannot register is NOT a request-level refusal: the call
        // answers 200 and reports it per target.
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );

        let mut expected_stored: Vec<(String, Option<i64>)> = stored
            .iter()
            .map(|(target, port)| (target.to_string(), *port))
            .collect();
        expected_stored.sort();
        assert_eq!(
            pairs_of(case, &answer, "successful"),
            expected_stored,
            "case {case:?}: successful must report the targets this call stored, got {}",
            answer.body
        );
        let expected_failures: Vec<(String, String, String, Option<i64>)> = failed
            .iter()
            .map(|(target, label, port)| {
                (
                    target.to_string(),
                    "InvalidTarget".to_string(),
                    not_owned(label, target),
                    *port,
                )
            })
            .collect();
        assert_eq!(
            failures_of(case, &answer),
            expected_failures,
            "case {case:?}: unsuccessful must report each target this call refused, got {}",
            answer.body
        );

        // The membership a later listing reports is exactly what was stored.
        assert_eq!(
            listed_pairs(&lattice, case, &id).await,
            expected_stored,
            "case {case:?}: ListTargets must report what the registration stored"
        );
    }
}

/// Rule 1 again, for the shape the recorded fixture has: a target group already holding
/// the one target its type allows, and a registration of a target of another account. The
/// per-target failure is reported ahead of the group's own maximum, which counts only the
/// targets a call would really store, and the held target is untouched.
#[tokio::test]
async fn a_per_target_failure_does_not_consume_the_room_the_group_has_left() {
    let lattice = Lattice::new();
    let case = "a foreign function against a full LAMBDA target group";
    let (id, _) = a_target_group(&lattice, "lambda-full", "LAMBDA", None).await;
    let accepted = register_targets(&lattice, &id, serde_json::json!([{ "id": A_LAMBDA }])).await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: the first registration must be accepted, got {} / {}",
        accepted.status, accepted.body
    );

    let answer = register_targets(&lattice, &id, serde_json::json!([{ "id": FOREIGN_LAMBDA }]))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        pairs_of(case, &answer, "successful"),
        Vec::new(),
        "case {case:?}: nothing was stored, so successful is empty, got {}",
        answer.body
    );
    assert_eq!(
        failures_of(case, &answer),
        vec![(
            FOREIGN_LAMBDA.to_string(),
            "InvalidTarget".to_string(),
            not_owned("Lambda", FOREIGN_LAMBDA),
            None,
        )],
        "case {case:?}: the refused target is reported per target, got {}",
        answer.body
    );
    assert_eq!(
        listed_pairs(&lattice, case, &id).await,
        vec![(A_LAMBDA.to_string(), None)],
        "case {case:?}: the target already registered must still be registered"
    );

    // A batch of refused targets is still one 200 with an entry each, in submission order.
    let case = "two foreign functions in one call";
    let (empty, _) = a_target_group(&lattice, "lambda-foreign", "LAMBDA", None).await;
    let answer = register_targets(
        &lattice,
        &empty,
        serde_json::json!([{ "id": FOREIGN_LAMBDA }, { "id": ANOTHER_FOREIGN_LAMBDA }]),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        failures_of(case, &answer)
            .into_iter()
            .map(|(target, code, _, _)| (target, code))
            .collect::<Vec<(String, String)>>(),
        vec![
            (FOREIGN_LAMBDA.to_string(), "InvalidTarget".to_string()),
            (
                ANOTHER_FOREIGN_LAMBDA.to_string(),
                "InvalidTarget".to_string()
            ),
        ],
        "case {case:?}: one entry per refused target, got {}",
        answer.body
    );
    assert_eq!(
        listed_pairs(&lattice, case, &empty).await,
        Vec::new(),
        "case {case:?}: a call that stored nothing leaves the group empty"
    );
}

/// Rule 2, over a table of the states the statuses describe, with NOTHING routing to the
/// target group in every row — so a row that reports `UNAVAILABLE` proves the health-check
/// condition is read ahead of and independently of the `UNUSED` one.
#[tokio::test]
async fn a_targets_status_carries_the_reason_code_for_the_case_it_is_in() {
    let lattice = Lattice::new();

    // (case, type, config, the target, the status, the reasonCode)
    let cases: Vec<ReasonCase> = vec![
        (
            "a LAMBDA target group's type supports no health check at all",
            "LAMBDA",
            None,
            A_LAMBDA,
            state::TARGET_STATUS_UNAVAILABLE,
            Some(state::TARGET_REASON_HEALTH_CHECK_NOT_SUPPORTED),
        ),
        (
            "an ALB target group's type supports no health check either",
            "ALB",
            Some(vpc_config("HTTPS", Some(443))),
            AN_ALB,
            state::TARGET_STATUS_UNAVAILABLE,
            Some(state::TARGET_REASON_HEALTH_CHECK_NOT_SUPPORTED),
        ),
        (
            "health checking turned off on a type that supports it",
            "IP",
            Some(serde_json::json!({
                "protocol": "HTTP",
                "port": 80,
                "vpcIdentifier": crate::target_groups::A_VPC,
                "healthCheck": { "enabled": false },
            })),
            "10.0.0.11",
            state::TARGET_STATUS_UNAVAILABLE,
            Some(state::TARGET_REASON_HEALTH_CHECK_DISABLED),
        ),
        (
            "health checks apply and nothing routes to the group",
            "IP",
            Some(vpc_config("HTTP", Some(80))),
            "10.0.0.12",
            state::TARGET_STATUS_UNUSED,
            None,
        ),
        (
            "an INSTANCE group that health-checks its targets",
            "INSTANCE",
            Some(vpc_config("HTTP", Some(80))),
            "i-0a1b2c3d",
            state::TARGET_STATUS_UNUSED,
            None,
        ),
    ];

    for (index, (case, group_type, config, target, status, reason)) in
        cases.into_iter().enumerate()
    {
        let (id, _) = a_target_group(&lattice, &format!("reason-{index}"), group_type, config)
            .await;
        let registered = register_targets(&lattice, &id, serde_json::json!([{ "id": target }]))
            .await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            registered.status, registered.body
        );
        assert_eq!(
            summaries(&lattice, case, &id).await,
            vec![(
                target.to_string(),
                status.to_string(),
                reason.map(str::to_string),
            )],
            "case {case:?}: expected status {status} with reasonCode {reason:?}"
        );
    }

    // A target of a group that health-checks it, with a listener routing to the group, is
    // in one of the two health-checking states and carries no reason code: the clock
    // decides which of them, so the assertion names both rather than racing it.
    let case = "a health-checked target of a group something routes to";
    let (service_id, _) = a_service(&lattice, "reason-code-host").await;
    let (id, _) = a_target_group(
        &lattice,
        "reason-routed",
        "IP",
        Some(vpc_config("HTTP", Some(80))),
    )
    .await;
    forward_to(&lattice, &service_id, "reason-listener", 8080, &id).await;
    let registered = register_targets(&lattice, &id, serde_json::json!([{ "id": "10.0.0.13" }]))
        .await;
    assert_eq!(
        registered.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        registered.status, registered.body
    );
    let reported = summaries(&lattice, case, &id).await;
    let (_, status, reason) = reported
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected one target, got {reported:?}"));
    assert!(
        [state::TARGET_STATUS_INITIAL, state::TARGET_STATUS_HEALTHY].contains(&status.as_str()),
        "case {case:?}: expected one of the health-checking statuses, got {status}"
    );
    assert_eq!(
        reason, &None,
        "case {case:?}: a status with one case carries no reasonCode, got {reason:?}"
    );
}

/// Rule 3, over a table of target-group types: one whose target has no connections to
/// drain, and two whose targets are reached over connections.
#[tokio::test]
async fn a_deregistered_target_leaves_the_groups_membership() {
    let lattice = Lattice::new();

    // (case, type, config, the two targets, whether its targets drain)
    let cases: Vec<DeregistrationCase> = vec![
        (
            "a LAMBDA target has no connections to drain",
            "LAMBDA",
            None,
            [A_LAMBDA, ANOTHER_LAMBDA],
            false,
        ),
        (
            "an IP target is reached over a connection",
            "IP",
            Some(vpc_config("HTTP", Some(80))),
            ["10.0.0.21", "10.0.0.22"],
            true,
        ),
        (
            "an INSTANCE target is reached over a connection",
            "INSTANCE",
            Some(vpc_config("HTTP", Some(80))),
            ["i-0123456789abcdef0", "i-0a1b2c3d"],
            true,
        ),
    ];

    for (index, (case, group_type, config, targets, drains)) in cases.into_iter().enumerate() {
        let (id, _) = a_target_group(
            &lattice,
            &format!("deregister-{index}"),
            group_type,
            config.clone(),
        )
        .await;
        assert_eq!(
            state::connections_drain(group_type),
            drains,
            "case {case:?}: the {group_type} type's draining is what this row asserts"
        );
        // A LAMBDA group holds one target at a time, so each row registers the first
        // target, deregisters it, and only then reaches for the second.
        let registered =
            register_targets(&lattice, &id, serde_json::json!([{ "id": targets[0] }])).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            registered.status, registered.body
        );

        let deregistered =
            deregister_targets(&lattice, &id, serde_json::json!([{ "id": targets[0] }])).await;
        assert_eq!(
            deregistered.status, 200,
            "case {case:?}: expected 200 from the deregistration, got {} / {}",
            deregistered.status, deregistered.body
        );
        assert_eq!(
            pairs_of(case, &deregistered, "successful")
                .into_iter()
                .map(|(target, _)| target)
                .collect::<Vec<String>>(),
            vec![targets[0].to_string()],
            "case {case:?}: successful must report the deregistered target, got {}",
            deregistered.body
        );
        assert_eq!(
            failures_of(case, &deregistered),
            Vec::new(),
            "case {case:?}: no target of this deregistration failed, got {}",
            deregistered.body
        );

        let reported = summaries(&lattice, case, &id).await;
        if drains {
            // "The target is being deregistered. No new connections are sent to this
            // target while current connections are being drained."
            assert_eq!(
                reported,
                vec![(
                    targets[0].to_string(),
                    state::TARGET_STATUS_DRAINING.to_string(),
                    None,
                )],
                "case {case:?}: a draining target is still reported, as DRAINING"
            );
            // …and registering it again puts it back in service: the drain is over and the
            // status is a health-checking one once more.
            let again =
                register_targets(&lattice, &id, serde_json::json!([{ "id": targets[0] }])).await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: expected 200 from the re-registration, got {} / {}",
                again.status, again.body
            );
            let reported = summaries(&lattice, case, &id).await;
            let (_, status, _) = reported
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: expected one target, got {reported:?}"));
            assert_ne!(
                status,
                state::TARGET_STATUS_DRAINING,
                "case {case:?}: a re-registered target is no longer draining"
            );
        } else {
            assert_eq!(
                reported,
                Vec::new(),
                "case {case:?}: a target with nothing to drain is gone from the group"
            );
            // …so deregistering it AGAIN has nothing to deregister: the target the group
            // holds no registration of any more is reported per target.
            let retry =
                deregister_targets(&lattice, &id, serde_json::json!([{ "id": targets[0] }])).await;
            assert_eq!(
                retry.status, 200,
                "case {case:?}: expected 200 from the retry, got {} / {}",
                retry.status, retry.body
            );
            assert_eq!(
                pairs_of(case, &retry, "successful"),
                Vec::new(),
                "case {case:?}: the retry deregistered nothing, got {}",
                retry.body
            );
            assert_eq!(
                failures_of(case, &retry),
                vec![(
                    targets[0].to_string(),
                    state::TARGET_FAILURE_TARGET_NOT_FOUND.to_string(),
                    state::TARGET_FAILURE_DEREGISTER_MESSAGE.to_string(),
                    None,
                )],
                "case {case:?}: the retry reports the target it could not deregister, got {}",
                retry.body
            );
            // …and the room it held is free again: the next target of a type that allows
            // one at a time is accepted.
            let next =
                register_targets(&lattice, &id, serde_json::json!([{ "id": targets[1] }])).await;
            assert_eq!(
                next.status, 200,
                "case {case:?}: the room the deregistered target held must be free, got {} / {}",
                next.status, next.body
            );
            assert_eq!(
                listed_pairs(&lattice, case, &id).await,
                vec![(targets[1].to_string(), None)],
                "case {case:?}: the group now holds the target that replaced it"
            );
        }
    }
}

/// Rule 3's remaining rows: a deregistration of a target the group holds no registration
/// of, the same call twice, and an identifier naming no target group.
#[tokio::test]
async fn a_deregistration_reports_what_it_holds_no_registration_of_and_changes_nothing() {
    let lattice = Lattice::new();
    let (id, _) = a_target_group(
        &lattice,
        "deregister-idempotent",
        "IP",
        Some(vpc_config("HTTP", Some(80))),
    )
    .await;
    let registered = register_targets(
        &lattice,
        &id,
        serde_json::json!([{ "id": "10.0.0.31" }, { "id": "10.0.0.32" }]),
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "case fixture registration: expected 200, got {} / {}",
        registered.status, registered.body
    );

    // (case, the targets the deregistration names, the (id, port) each failure reports)
    let cases: Vec<NotRegisteredCase> = vec![
        (
            "a target the group never held",
            serde_json::json!([{ "id": "10.9.9.9" }]),
            vec![("10.9.9.9", Some(80))],
        ),
        (
            "the same target twice in one call",
            serde_json::json!([{ "id": "10.9.9.9" }, { "id": "10.9.9.9" }]),
            vec![("10.9.9.9", Some(80))],
        ),
        (
            "a target on a port nothing is reached on",
            serde_json::json!([{ "id": "10.0.0.31", "port": 8080 }]),
            vec![("10.0.0.31", Some(8080))],
        ),
    ];
    for (case, targets, failed) in cases {
        for attempt in ["the first call", "the retry"] {
            let answer = deregister_targets(&lattice, &id, targets.clone()).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?} ({attempt}): expected 200, got {} / {}",
                answer.status, answer.body
            );
            assert_eq!(
                pairs_of(case, &answer, "successful"),
                Vec::new(),
                "case {case:?} ({attempt}): this call deregistered nothing, got {}",
                answer.body
            );
            assert_eq!(
                failures_of(case, &answer),
                failed
                    .iter()
                    .map(|(target, port)| (
                        target.to_string(),
                        state::TARGET_FAILURE_TARGET_NOT_FOUND.to_string(),
                        state::TARGET_FAILURE_DEREGISTER_MESSAGE.to_string(),
                        *port,
                    ))
                    .collect::<Vec<(String, String, String, Option<i64>)>>(),
                "case {case:?} ({attempt}): unexpected unsuccessful list, got {}",
                answer.body
            );
            // Nothing the group holds was touched by any of these calls.
            assert_eq!(
                listed_pairs(&lattice, case, &id).await,
                vec![
                    ("10.0.0.31".to_string(), Some(80)),
                    ("10.0.0.32".to_string(), Some(80)),
                ],
                "case {case:?} ({attempt}): the registered targets must be untouched"
            );
        }
    }

    // A target this service would never have registered is one the group holds no
    // registration of either, reported with the same per-target failure as any other.
    let case = "a function of another account";
    let (lambda_group, _) = a_target_group(&lattice, "deregister-foreign", "LAMBDA", None).await;
    let answer = deregister_targets(
        &lattice,
        &lambda_group,
        serde_json::json!([{ "id": FOREIGN_LAMBDA }]),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        failures_of(case, &answer),
        vec![(
            FOREIGN_LAMBDA.to_string(),
            state::TARGET_FAILURE_TARGET_NOT_FOUND.to_string(),
            state::TARGET_FAILURE_DEREGISTER_MESSAGE.to_string(),
            None,
        )],
        "case {case:?}: the target is reported per target, got {}",
        answer.body
    );

    // A target of a form the group's type does not admit at all is input the whole
    // request is malformed in, the same 400 a registration of it is.
    let case = "an id of another form than the type documents";
    let refused = deregister_targets(
        &lattice,
        &lambda_group,
        serde_json::json!([{ "id": "i-0123456789abcdef0" }]),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} / {}",
        refused.status, refused.body
    );
    assert!(
        message_of(case, &refused).contains("LAMBDA"),
        "case {case:?}: the refusal must name the target group's type, got {}",
        refused.body
    );

    let case = "an identifier nothing created";
    let absent = absent_id(state::PREFIX_TARGET_GROUP, "9");
    let answer = deregister_targets(&lattice, &absent, serde_json::json!([{ "id": A_LAMBDA }]))
        .await;
    assert_not_found(
        &answer,
        case,
        state::ResourceKind::TargetGroup.message_name(),
        &absent,
    );
}
