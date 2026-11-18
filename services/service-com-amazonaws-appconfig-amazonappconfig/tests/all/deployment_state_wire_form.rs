// SPDX-License-Identifier: CC0-1.0

//! `Deployment$State` on the wire, pinned against the value AWS serves for each
//! `DeploymentState` member.
//!
//! The rule under test: a modelled enum member goes on the wire as the value the model
//! gives it, which — for an enum whose members carry no `@enumValue` trait — is the member's
//! own name. `DeploymentState`'s members are `BAKING | VALIDATING | DEPLOYING | COMPLETE |
//! ROLLING_BACK | ROLLED_BACK | REVERTED`
//! (`smithy_query(query="shape",shape="DeploymentState")`), none of them overridden, so a
//! finished deployment serves `COMPLETE`, one still rolling out `DEPLOYING` and one a stop
//! undid `ROLLED_BACK` — underscore and all for the two-word members. Every operation that
//! serves a Deployment document or summary serves that same token, byte for byte, out of
//! the one catalog the crate spells it in. A client waiting for a deployment to finish
//! compares `State` against that value, so a document carrying some other spelling of the
//! member never satisfies the wait.
//!
//! A refusal is the other side of the same rule: a sentence that spells a state out names
//! the enum's MEMBER, which here is the very string the document serves, so the two are
//! asserted against each other rather than against two different spellings.
//!
//! This is NOT a blanket rule for the service: the sibling `Environment$State` IS served in
//! a form its members do not carry (`ReadyForDeployment`, `RollingBack`, …), which
//! `crate::environment_state_wire_form` pins. Each enum is asserted against the form that
//! enum is served in.
//!
//! The table also pins the state `StartDeployment` reports on its OWN 201, which is a
//! projection of its own: a client that branches on the answer to the call it just made,
//! before any poll, reads the token from there.
//!
//! The tables below span what the rule generalises over: WHICH member of the enum (all
//! seven, including the ones no operation reaches), which OPERATION serves the document
//! (`StartDeployment`, `GetDeployment`, `ListDeployments`, `StopDeployment`), and WHAT put
//! the deployment in that state (a strategy that paces nothing, a long rollout, a bake
//! window, a caller's stop, a caller's revert). The literal strings here are deliberately
//! literal: a regression that re-spelled the crate's constants would still satisfy a test
//! written in terms of those constants.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::ops::types::DEPLOYMENT_STATE_VALUES;
use service_com_amazonaws_appconfig_amazonappconfig::state;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state, router,
};
use crate::deployment_wire_fidelity::{create_strategy, host_version, member, Row};
use crate::stop_deployment::{get_deployment, start, stop};
use crate::stop_deployment_without_its_environment::list_deployments;

/// Every `DeploymentState` member paired with the value AppConfig puts on the wire for it.
///
/// Source of the member names: the model's own enum, as
/// `smithy_query(query="shape", shape="DeploymentState")` reports it and as the generated
/// `DEPLOYMENT_STATE_VALUES` carries it (`BAKING`, `COMPLETE`, `DEPLOYING`, `REVERTED`,
/// `ROLLED_BACK`, `ROLLING_BACK`, `VALIDATING`). Source of the wire values: the same enum —
/// the model JSON in `smithy-model/` ships no `@enumValue` trait on these members, so each
/// member's value IS its name, and the pairing below is written out so the catalog is
/// checkable rather than circular.
const PUBLISHED_DEPLOYMENT_STATES: &[(&str, &str)] = &[
    ("DEPLOYING", "DEPLOYING"),
    ("BAKING", "BAKING"),
    ("VALIDATING", "VALIDATING"),
    ("COMPLETE", "COMPLETE"),
    ("ROLLING_BACK", "ROLLING_BACK"),
    ("ROLLED_BACK", "ROLLED_BACK"),
    ("REVERTED", "REVERTED"),
];

/// Whether `value` is one of the values `Deployment$State` goes on the wire as.
///
/// `pub(crate)` because every suite that asserts a served state checks it against this one
/// catalog rather than against a spelling of its own.
pub(crate) fn is_a_published_deployment_state(value: &str) -> bool {
    PUBLISHED_DEPLOYMENT_STATES
        .iter()
        .any(|(_, wire_value)| *wire_value == value)
}

/// The value the crate serves for one modelled member — the single place it spells the wire
/// form, which is what every handler reads — or `None` for a member no state of this
/// service's deployment lifecycle corresponds to.
fn served_for(member_name: &str) -> Option<&'static str> {
    match member_name {
        "DEPLOYING" => Some(state::DEPLOYMENT_DEPLOYING),
        "BAKING" => Some(state::DEPLOYMENT_BAKING),
        "COMPLETE" => Some(state::DEPLOYMENT_COMPLETE),
        "ROLLING_BACK" => Some(state::DEPLOYMENT_ROLLING_BACK),
        "ROLLED_BACK" => Some(state::DEPLOYMENT_ROLLED_BACK),
        "REVERTED" => Some(state::DEPLOYMENT_REVERTED),
        _ => None,
    }
}

/// The one member the service holds no deployment in: AppConfig reports it while a
/// deployment's configuration is being checked, which this service does inside the call
/// that starts the deployment rather than as a stage of it.
const MEMBER_NO_DEPLOYMENT_SITS_IN: &str = "VALIDATING";

/// The catalog the crate serves covers every member the model declares, each paired with
/// the value the model gives it, which for this enum is the member name itself.
#[test]
fn every_modelled_deployment_state_member_is_served_as_its_published_wire_value() {
    assert_eq!(
        PUBLISHED_DEPLOYMENT_STATES.len(),
        DEPLOYMENT_STATE_VALUES.len(),
        "the published catalog must cover every modelled DeploymentState member, got {:?} \
         against {:?}",
        PUBLISHED_DEPLOYMENT_STATES,
        DEPLOYMENT_STATE_VALUES
    );

    for (member_name, wire_value) in PUBLISHED_DEPLOYMENT_STATES {
        assert!(
            DEPLOYMENT_STATE_VALUES.contains(member_name),
            "case {member_name:?}: expected a member the model declares, which is not among {:?}",
            DEPLOYMENT_STATE_VALUES
        );
        // No `@enumValue` override on any member of this enum, so the value and the member
        // name are one string — including the underscore of a two-word member.
        assert_eq!(
            wire_value, member_name,
            "case {member_name:?}: the wire value of a member with no @enumValue override is \
             the member name, got {wire_value:?}"
        );
        let expected = if *member_name == MEMBER_NO_DEPLOYMENT_SITS_IN {
            None
        } else {
            Some(*wire_value)
        };
        assert_eq!(
            served_for(member_name),
            expected,
            "case {member_name:?}: the crate must serve {expected:?}, got {:?}",
            served_for(member_name)
        );
    }

    // And nothing the model declares is left without a published wire value.
    for member_name in DEPLOYMENT_STATE_VALUES {
        assert!(
            PUBLISHED_DEPLOYMENT_STATES
                .iter()
                .any(|(known, _)| known == member_name),
            "case {member_name:?}: a modelled member with no published wire value in {:?}",
            PUBLISHED_DEPLOYMENT_STATES
        );
    }
}

/// What a row does to its deployment after starting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settle {
    /// Nothing: the row reads the deployment where the clock leaves it.
    Nothing,
    /// `StopDeployment` with no `Allow-Revert`, which rolls a deployment in flight back.
    Stop,
    /// `StopDeployment` with `Allow-Revert: true`, which reverts a completed one.
    Revert,
}

/// One row: the case, the strategy that paces the deployment, what the row does to it, and
/// the value `Deployment$State` must then carry on every path that serves it.
struct Case {
    /// The strategy's criteria — `DeploymentDurationInMinutes`, `GrowthFactor`,
    /// `GrowthType`, `FinalBakeTimeInMinutes` — as `deployment_wire_fidelity`'s row type.
    row: Row,
    settle: Settle,
    /// The published wire value `StartDeployment`'s own 201 must carry: the first stage of a
    /// strategy that paces one, and the terminal value of a strategy that paces none.
    at_start: &'static str,
    /// The published wire value, written out rather than read from the crate under test.
    expected: &'static str,
}

/// The rows, spanning the states a deployment of this service reaches and what puts it
/// there. The durations are the model's own bounds: `MinutesBetween0And24Hours` is
/// `range 0..1440` and `Percentage` is `range 1..100`.
const CASES: &[Case] = &[
    Case {
        // A strategy that paces no stage leaves AppConfig nothing to roll out and nothing
        // to monitor, so the start itself answers a completed deployment.
        row: ("no window and no bake", 0, 100.0, "LINEAR", 0),
        settle: Settle::Nothing,
        at_start: "COMPLETE",
        expected: "COMPLETE",
    },
    Case {
        // A growth factor of one percent per interval, so the rollout window is still far
        // from over when the row's reads happen: each interval of a rollout is a
        // transition of its own.
        row: ("the longest rollout the model allows", 1440, 1.0, "LINEAR", 0),
        settle: Settle::Nothing,
        at_start: "DEPLOYING",
        expected: "DEPLOYING",
    },
    Case {
        // No rollout window but the longest bake: the rollout is over after one interval
        // and the bake then holds the deployment.
        // The rollout of such a strategy is a stage AppConfig performs after the call, so
        // the call itself answers the deployment rolling out rather than baking.
        row: ("no window, the longest bake", 0, 100.0, "EXPONENTIAL", 1440),
        settle: Settle::Nothing,
        at_start: "DEPLOYING",
        expected: "BAKING",
    },
    Case {
        row: ("stopped while still rolling out", 1440, 1.0, "LINEAR", 0),
        settle: Settle::Stop,
        at_start: "DEPLOYING",
        expected: "ROLLED_BACK",
    },
    Case {
        row: ("reverted after completing", 0, 100.0, "LINEAR", 0),
        settle: Settle::Revert,
        at_start: "COMPLETE",
        expected: "REVERTED",
    },
];

/// The `State` the listing reports for `number`, read through `ListDeployments`.
async fn listed_state(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> String {
    let listing = list_deployments(router, application, environment).await;
    assert_eq!(
        listing.status,
        200,
        "case {case:?}: ListDeployments must answer 200, got {} / {}",
        listing.status,
        String::from_utf8_lossy(&listing.body)
    );
    let body = listing.json();
    let items = body
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: ListDeployments must carry Items, got {body}"));
    items
        .iter()
        .find(|item| member(item, "DeploymentNumber").as_i64() == Some(number))
        .map(|item| member(item, "State").as_str().unwrap_or_default().to_string())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report deployment {number}, got {body}")
        })
}

/// Everything the rule requires of one served state: it is the published wire value, it is
/// one of the catalog's values, and it is a member the model declares.
fn assert_published(case: &str, document: &str, expected: &str, actual: &str) {
    assert_eq!(
        actual, expected,
        "case {case:?}: {document} must report State {expected:?}, got {actual:?}"
    );
    assert!(
        is_a_published_deployment_state(actual),
        "case {case:?}: {document}'s State must be a published DeploymentState wire value, got \
         {actual:?}"
    );
    assert!(
        DEPLOYMENT_STATE_VALUES.contains(&actual),
        "case {case:?}: {document} must serve a member the model declares, got {actual:?}"
    );
}

/// Every operation that serves a Deployment document serves the published wire value of
/// the state the deployment is in, whatever put it there.
#[tokio::test]
async fn every_operation_serves_the_published_wire_value_of_a_deployments_state() {
    let router = router();
    let application = create_application(&router, "deployment-state-wire").await;
    let profile = create_profile(&router, &application, "wire-form").await;
    host_version(&router, &application, &profile).await;

    for (index, expected_case) in CASES.iter().enumerate() {
        let case = expected_case.row.0;
        let expected = expected_case.expected;
        let strategy = create_strategy(&router, index, &expected_case.row).await;
        // Its own environment per row: an environment holds one deployment at a time.
        let environment =
            create_environment(&router, &application, &format!("wire-{index}")).await;

        let started = start(&router, &application, &environment, &strategy, &profile).await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        // The call's OWN answer, before anything polls: a client that branches on the
        // deployment it just created reads the state from here, so this projection serves
        // the published value too.
        let accepted = started.json();
        let at_start = member(&accepted, "State")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_published(
            case,
            "StartDeployment",
            expected_case.at_start,
            at_start.as_str(),
        );
        // And which KIND of member that is follows from the strategy the request named
        // rather than from a spelling: a strategy declaring a rollout window or a final
        // bake time leaves AppConfig a stage to perform after the call, so the call answers
        // the deployment at its first stage; one declaring neither leaves nothing to pace,
        // and the deployment is over by the time the call answers.
        let (_, duration, _, _, bake) = expected_case.row;
        let paces_a_stage = duration > 0 || bake > 0;
        assert_eq!(
            matches!(at_start.as_str(), "DEPLOYING" | "BAKING"),
            paces_a_stage,
            "case {case:?}: a strategy that paces a stage ({paces_a_stage}) is answered at an \
             in-flight member and one that paces none at a terminal member, got {at_start:?}"
        );
        let number = member(&accepted, "DeploymentNumber")
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: StartDeployment answered no number"));

        match expected_case.settle {
            Settle::Nothing => {
                // Whatever the row's strategy owes, the state it owes is the published
                // value: the wait is for the CONDITION, spelled the way AWS spells it, so a
                // document carrying another spelling would never satisfy it.
                let reached = deployment_in_state(
                    &router,
                    case,
                    &application,
                    &environment,
                    number,
                    &[expected],
                )
                .await;
                assert_published(
                    case,
                    "GetDeployment",
                    expected,
                    member(&reached, "State").as_str().unwrap_or_default(),
                );
            }
            Settle::Stop | Settle::Revert => {
                let allow_revert = if expected_case.settle == Settle::Revert {
                    // Only a deployment that COMPLETED can be reverted, and completing is
                    // owed by the clock, so the row waits for the condition first.
                    deployment_in_state(
                        &router,
                        case,
                        &application,
                        &environment,
                        number,
                        &["COMPLETE"],
                    )
                    .await;
                    Some(true)
                } else {
                    None
                };
                let number = i32::try_from(number).unwrap_or_default();
                let settled = stop(
                    &router,
                    &application,
                    &environment,
                    number,
                    allow_revert,
                )
                .await;
                assert_eq!(
                    settled.status,
                    202,
                    "case {case:?}: StopDeployment must answer 202, got {} / {}",
                    settled.status,
                    String::from_utf8_lossy(&settled.body)
                );
                assert_published(
                    case,
                    "StopDeployment",
                    expected,
                    member(&settled.json(), "State").as_str().unwrap_or_default(),
                );
            }
        }

        // The read path and the listing report the same spelling as the call that left the
        // deployment there.
        let fetched = get_deployment(
            &router,
            &application,
            &environment,
            i32::try_from(number).unwrap_or_default(),
        )
        .await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: GetDeployment must answer 200, got {} / {}",
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        assert_published(
            case,
            "GetDeployment",
            expected,
            member(&fetched.json(), "State").as_str().unwrap_or_default(),
        );
        let listed = listed_state(&router, case, &application, &environment, number).await;
        assert_published(case, "ListDeployments", expected, listed.as_str());
    }
}

/// The other side of the rule: the refusal that names a state spells the enum's MEMBER,
/// which for this enum is the same token the document serves.
#[tokio::test]
async fn a_refusal_names_the_member_of_the_state_the_document_serves() {
    let router = router();
    let application = create_application(&router, "state-spelling").await;
    let profile = create_profile(&router, &application, "spelling").await;
    host_version(&router, &application, &profile).await;

    // Rows differ along the terminal state the second stop is refused for, and along the
    // verb that stop asks for: a plain stop of a completed deployment is refused, and a
    // deployment in flight is rolled back by one.
    let rows: [(&str, Row, Settle, &str, &str, bool); 2] = [
        (
            "a completed deployment",
            ("completed", 0, 100.0, "LINEAR", 0),
            Settle::Nothing,
            "COMPLETE",
            "COMPLETE",
            false,
        ),
        (
            "a rolled-back deployment",
            ("rolled back", 1440, 1.0, "LINEAR", 0),
            Settle::Stop,
            "ROLLED_BACK",
            "ROLLED_BACK",
            true,
        ),
    ];

    for (index, (case, row, settle, served, member_name, allow_revert)) in
        rows.into_iter().enumerate()
    {
        let strategy = create_strategy(&router, 100 + index, &row).await;
        let environment =
            create_environment(&router, &application, &format!("spelling-{index}")).await;
        let started = start(&router, &application, &environment, &strategy, &profile).await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let number = member(&started.json(), "DeploymentNumber")
            .as_i64()
            .unwrap_or_default();
        let settled_number = i32::try_from(number).unwrap_or_default();
        if settle == Settle::Stop {
            let settled = stop(&router, &application, &environment, settled_number, None).await;
            assert_eq!(
                settled.status,
                202,
                "case {case:?}: the first stop must answer 202, got {} / {}",
                settled.status,
                String::from_utf8_lossy(&settled.body)
            );
        }
        let document = deployment_in_state(
            &router,
            case,
            &application,
            &environment,
            number,
            &[served],
        )
        .await;
        assert_published(
            case,
            "GetDeployment",
            served,
            member(&document, "State").as_str().unwrap_or_default(),
        );

        let refused = stop(
            &router,
            &application,
            &environment,
            settled_number,
            allow_revert.then_some(true),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: a stop of a terminal deployment is refused with the modelled \
             BadRequestException's HTTP 400, got {} / {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        let verb = if allow_revert { "reverted" } else { "stopped" };
        let expected = format!(
            "Deployment {number} for App {application} and Environment {environment} cannot be \
             {verb}, it has a status of {member_name}."
        );
        let message = refused.string("Message").unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
        assert!(
            message.contains(served),
            "case {case:?}: the refusal names the enum's member, which is also the value the \
             document serves ({served:?}), got {message:?}"
        );
    }
}
