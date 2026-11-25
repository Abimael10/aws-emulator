// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TEXT of the per-target failure `DeregisterTargets` reports.
//!
//! The rule under test: the `failureMessage` of a target a deregistration could not
//! deregister is that failure class's own FIXED text — it interpolates nothing from the
//! request and nothing from service state, so no target id, ARN, port, target-group id or
//! target-group type appears in it, and every target of every target-group type that the
//! operation cannot deregister carries exactly the same sentence beside its own `id`,
//! `port` and `failureCode`.
//!
//! The table varies what the rule generalises over: the target-group type, the FORM of the
//! target id (function ARN, load-balancer ARN, IPv4, IPv6, instance id), the port (none,
//! the group's configured one, one nothing is reached on), how many targets one call names,
//! and whether the same call also deregisters a target the group DOES hold.
//!
//! Every case drives the service the way a client does — a signed restJson1 request through
//! the router that mounts it — so nothing here can pass on a canned body. The harness and
//! the target helpers are the ones the `targets` suite already builds.

use service_com_amazonaws_vpclattice_mercurycontrolplane::state;

use crate::targets::{
    a_target_group, register_targets, vpc_config, ANOTHER_LAMBDA, AN_ALB, A_LAMBDA,
};
use crate::vpclattice_service_slice::{as_label, Answer, Lattice};

/// One row of the failure-message table: the case, the target-group type, the config the
/// group is created with, the targets registered with it first, the targets the
/// deregistration names, and the (id, port) of each entry it has to report under
/// `unsuccessful` and under `successful`.
type MessageCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    serde_json::Value,
    serde_json::Value,
    Vec<(&'static str, Option<i64>)>,
    Vec<(&'static str, Option<i64>)>,
);

/// `POST /targetgroups/{targetGroupIdentifier}/deregistertargets`.
async fn deregister(lattice: &Lattice, identifier: &str, targets: serde_json::Value) -> Answer {
    lattice
        .call(
            "POST",
            &format!("/targetgroups/{identifier}/deregistertargets"),
            Some(serde_json::json!({ "targets": targets })),
        )
        .await
}

/// The (id, port) pairs a member of the answer carries, sorted so two answers compare by
/// content rather than by order.
fn pairs(case: &str, answer: &Answer, member: &str) -> Vec<(String, Option<i64>)> {
    let entries = answer.body[member].as_array().cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the member {member} to be a list, got {}",
            answer.body
        )
    });
    let mut listed: Vec<(String, Option<i64>)> = entries
        .iter()
        .map(|entry| {
            (
                entry["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: an entry without an id: {entry}"))
                    .to_string(),
                entry["port"].as_i64(),
            )
        })
        .collect();
    listed.sort();
    listed
}

/// The expected (id, port) pairs of one row, in the order `pairs` answers them in.
fn sorted(expected: Vec<(&'static str, Option<i64>)>) -> Vec<(String, Option<i64>)> {
    let mut expected: Vec<(String, Option<i64>)> = expected
        .into_iter()
        .map(|(target, port)| (target.to_string(), port))
        .collect();
    expected.sort();
    expected
}

/// Assert the per-target failure text of one answer: every entry of `unsuccessful` carries
/// the failure class's fixed message and nothing taken from the request or from state.
fn assert_fixed_message(case: &str, answer: &Answer, group_type: &str, target_group_id: &str) {
    let entries = answer.body["unsuccessful"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected unsuccessful to be a list, got {}",
                answer.body
            )
        });
    for entry in &entries {
        let message = entry["failureMessage"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: a failure without a failureMessage: {entry}")
        });
        assert_eq!(
            message,
            state::TARGET_FAILURE_DEREGISTER_MESSAGE,
            "case {case:?}: the failure message is this class's fixed text, got {message:?}"
        );
        assert_eq!(
            entry["failureCode"].as_str(),
            Some(state::TARGET_FAILURE_TARGET_NOT_FOUND),
            "case {case:?}: unexpected failureCode, got {entry}"
        );
        // Nothing of the request or of service state is interpolated into the text: the
        // entry's own `id` and `port` are the only per-target values it carries.
        let id = entry["id"].as_str().unwrap_or_default();
        for absent in [id, target_group_id, group_type] {
            assert!(
                !absent.is_empty() && !message.contains(absent),
                "case {case:?}: the message must not carry {absent:?}, got {message:?}"
            );
        }
        if let Some(port) = entry["port"].as_i64() {
            assert!(
                !message.contains(&port.to_string()),
                "case {case:?}: the message must not carry the port {port}, got {message:?}"
            );
        }
    }
}

/// The rule over the table: whatever the target-group type, the target's form, the port,
/// the count, or whether the same call deregisters something too, the per-target failure
/// text is the class's one fixed sentence.
#[tokio::test]
async fn a_deregistration_reports_one_fixed_failure_message_for_every_target_it_cannot_deregister()
{
    let lattice = Lattice::new();

    // (case, type, config, registered first, deregistered, expected unsuccessful, successful)
    let cases: Vec<MessageCase> = vec![
        (
            "LAMBDA: a function the group never held",
            "LAMBDA",
            None,
            serde_json::json!([]),
            serde_json::json!([{ "id": A_LAMBDA }]),
            vec![(A_LAMBDA, None)],
            vec![],
        ),
        (
            "LAMBDA: a function of another account, which no registration would admit",
            "LAMBDA",
            None,
            serde_json::json!([]),
            serde_json::json!([{
                "id": "arn:aws:lambda:us-west-2:111111111111:function:not-mine",
            }]),
            vec![(
                "arn:aws:lambda:us-west-2:111111111111:function:not-mine",
                None,
            )],
            vec![],
        ),
        (
            "ALB: a load balancer ARN on a port nothing was registered on",
            "ALB",
            Some(vpc_config("HTTPS", Some(443))),
            serde_json::json!([{ "id": AN_ALB, "port": 8443 }]),
            serde_json::json!([{ "id": AN_ALB, "port": 9443 }]),
            vec![(AN_ALB, Some(9443))],
            vec![],
        ),
        (
            "IP over HTTP: three addresses at once, IPv4 and IPv6, on the default port",
            "IP",
            Some(vpc_config("HTTP", None)),
            serde_json::json!([]),
            serde_json::json!([
                { "id": "10.9.9.1" },
                { "id": "10.9.9.2", "port": 8080 },
                { "id": "2001:db8::9" },
            ]),
            vec![
                ("10.9.9.1", Some(80)),
                ("10.9.9.2", Some(8080)),
                ("2001:db8::9", Some(80)),
            ],
            vec![],
        ),
        (
            "IP: one held target and one the group never held, in the same call",
            "IP",
            Some(vpc_config("HTTP", Some(80))),
            serde_json::json!([{ "id": "10.0.1.7" }]),
            serde_json::json!([{ "id": "10.0.1.7" }, { "id": "10.0.1.8" }]),
            vec![("10.0.1.8", Some(80))],
            vec![("10.0.1.7", Some(80))],
        ),
        (
            "INSTANCE: an instance id of each documented length",
            "INSTANCE",
            Some(vpc_config("HTTP", Some(9000))),
            serde_json::json!([]),
            serde_json::json!([{ "id": "i-0fedcba9876543210" }, { "id": "i-0a1b2c3e" }]),
            vec![
                ("i-0a1b2c3e", Some(9000)),
                ("i-0fedcba9876543210", Some(9000)),
            ],
            vec![],
        ),
    ];

    for (index, (case, group_type, config, held, named, failed, succeeded)) in
        cases.into_iter().enumerate()
    {
        let (id, _) = a_target_group(
            &lattice,
            &format!("deregister-message-{index}"),
            group_type,
            config,
        )
        .await;
        if !held.as_array().is_some_and(|targets| targets.is_empty()) {
            let registered = register_targets(&lattice, &id, held).await;
            assert_eq!(
                registered.status, 200,
                "case {case:?}: expected 200 from the registration, got {} / {}",
                registered.status, registered.body
            );
        }

        let answer = deregister(&lattice, &id, named).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            pairs(case, &answer, "unsuccessful"),
            sorted(failed),
            "case {case:?}: unexpected unsuccessful list, got {}",
            answer.body
        );
        assert_eq!(
            pairs(case, &answer, "successful"),
            sorted(succeeded),
            "case {case:?}: unexpected successful list, got {}",
            answer.body
        );
        assert_fixed_message(case, &answer, group_type, &id);
    }
}

/// The text does not depend on the identifier form the call named the target group by
/// either: a deregistration against the group's ARN reports the same sentence as one
/// against its bare id.
#[tokio::test]
async fn the_failure_message_is_the_same_through_both_identifier_forms() {
    let lattice = Lattice::new();
    let (id, arn) = a_target_group(&lattice, "deregister-message-arn", "LAMBDA", None).await;

    for (case, identifier) in [("the bare id", id.clone()), ("the target group ARN", as_label(&arn))]
    {
        let answer =
            deregister(&lattice, &identifier, serde_json::json!([{ "id": ANOTHER_LAMBDA }])).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            pairs(case, &answer, "unsuccessful"),
            vec![(ANOTHER_LAMBDA.to_string(), None)],
            "case {case:?}: the target it could not deregister is reported, got {}",
            answer.body
        );
        assert_fixed_message(case, &answer, "LAMBDA", &id);
    }
}
