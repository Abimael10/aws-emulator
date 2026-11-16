// SPDX-License-Identifier: CC0-1.0

//! The two `State` members of ONE AppConfig release, read off the same lifecycle: a
//! deployment's and its environment's.
//!
//! The rule under test: each of this service's two state enums is served in the form AWS
//! serves THAT enum in, and the two forms are not the same one.
//!
//! * `Deployment$State` targets `DeploymentState`, whose members carry no `@enumValue`
//!   override (`smithy_query(query="shape",shape="DeploymentState")`), so each member's
//!   wire value is the member's own name — `COMPLETE`, `DEPLOYING`, `ROLLED_BACK`,
//!   underscore included.
//! * `Environment$State` targets `EnvironmentState`, whose members carry no override
//!   either, and yet AWS serves it as the CamelCase token the AppConfig API Reference
//!   lists for that member — `ReadyForDeployment`, `Deploying`, `RolledBack`.
//!
//! So a change that "harmonises" the two — serving both as member names, or both as
//! CamelCase — breaks one of them, and the barrier a client waits on for a release to
//! settle (`State == "COMPLETE"` for the deployment, `State == "ReadyForDeployment"` for
//! the environment) stops releasing. That is why the two are asserted TOGETHER here, on
//! one request sequence, rather than only enum by enum in the sibling
//! `crate::deployment_state_wire_form` / `crate::environment_state_wire_form` suites.
//!
//! The rows span what the rule generalises over: how the release ends (the clock completes
//! it, a caller's stop rolls it back, a caller's revert undoes a completed one) and
//! therefore which member of each enum the pair lands in.

use service_com_amazonaws_appconfig_amazonappconfig::ops::types::{
    DEPLOYMENT_STATE_VALUES, ENVIRONMENT_STATE_VALUES,
};

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state, router,
};
use crate::deployment_wire_fidelity::{create_strategy, host_version, member, Row};
use crate::stop_deployment::{environment_state, get_deployment, start, stop};

/// What a row does to its deployment once it has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Nothing: the clock finishes the release.
    Settles,
    /// `StopDeployment` with no `Allow-Revert`, which withdraws a release still in flight.
    Stopped,
    /// `StopDeployment` with `Allow-Revert: true` on a release that already completed.
    Reverted,
}

/// One row: the case, the strategy pacing the release, how it ends, and the token EACH of
/// the two enums must then carry — written out literally, so a regression that re-spelled
/// the crate's own constants would not satisfy the row.
struct Pair {
    /// The strategy's criteria, as `deployment_wire_fidelity`'s row type spells them.
    row: Row,
    ending: Ending,
    /// The `DeploymentState` member `Deployment$State` must serve.
    deployment: &'static str,
    /// The `EnvironmentState` token `Environment$State` must serve.
    environment: &'static str,
}

/// The rows. `MinutesBetween0And24Hours` is `range 0..1440` and `Percentage` is
/// `range 1..100`, so the paced row below is the longest rollout the model can express —
/// still in flight when the row stops it.
const PAIRS: &[Pair] = &[
    Pair {
        row: ("the clock completes it", 0, 100.0, "LINEAR", 0),
        ending: Ending::Settles,
        deployment: "COMPLETE",
        environment: "ReadyForDeployment",
    },
    Pair {
        row: ("a stop withdraws it", 1440, 1.0, "LINEAR", 0),
        ending: Ending::Stopped,
        deployment: "ROLLED_BACK",
        environment: "RolledBack",
    },
    Pair {
        row: ("a revert undoes it", 0, 100.0, "LINEAR", 0),
        ending: Ending::Reverted,
        deployment: "REVERTED",
        environment: "Reverted",
    },
];

/// RULE: the deployment's state is served as the `DeploymentState` member it sits in, and
/// the environment's as the CamelCase token of the `EnvironmentState` member it sits in —
/// so one is a modelled member name and the other is not, on the very same release.
#[tokio::test]
async fn a_deployment_and_its_environment_report_their_states_in_their_own_forms() {
    let router = router();
    let application = create_application(&router, "state-token-pair").await;
    let profile = create_profile(&router, &application, "pair").await;
    host_version(&router, &application, &profile).await;

    for (index, pair) in PAIRS.iter().enumerate() {
        let case = pair.row.0;
        let strategy = create_strategy(&router, 200 + index, &pair.row).await;
        // One environment per row: an environment holds one deployment at a time.
        let environment = create_environment(&router, &application, &format!("pair-{index}")).await;
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
            .unwrap_or_else(|| panic!("case {case:?}: StartDeployment answered no number"));
        let settled_number = i32::try_from(number).unwrap_or_else(|_| {
            panic!("case {case:?}: the deployment number {number} is not an i32")
        });

        match pair.ending {
            Ending::Settles => {
                // Waits for the CONDITION, spelled the way AWS spells it: the barrier is
                // the assertion.
                deployment_in_state(
                    &router,
                    case,
                    &application,
                    &environment,
                    number,
                    &[pair.deployment],
                )
                .await;
            }
            Ending::Stopped | Ending::Reverted => {
                let allow_revert = if pair.ending == Ending::Reverted {
                    // Only a release that COMPLETED can be reverted, and completing is
                    // owed by the clock.
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
                let settled =
                    stop(&router, &application, &environment, settled_number, allow_revert).await;
                assert_eq!(
                    settled.status,
                    202,
                    "case {case:?}: StopDeployment must answer 202, got {} / {}",
                    settled.status,
                    String::from_utf8_lossy(&settled.body)
                );
                assert_eq!(
                    member(&settled.json(), "State").as_str(),
                    Some(pair.deployment),
                    "case {case:?}: the stop's own answer must report State {:?}, got {}",
                    pair.deployment,
                    settled.json()
                );
            }
        }

        let read_back = get_deployment(&router, &application, &environment, settled_number).await;
        assert_eq!(
            read_back.status,
            200,
            "case {case:?}: GetDeployment must answer 200, got {} / {}",
            read_back.status,
            String::from_utf8_lossy(&read_back.body)
        );
        let deployment_now = member(&read_back.json(), "State")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            deployment_now, pair.deployment,
            "case {case:?}: GetDeployment must report State {:?}, got {deployment_now:?}",
            pair.deployment
        );
        // The discriminating half of the rule, stated against the MODEL rather than
        // against another spelling: a deployment's state IS one of its enum's members.
        assert!(
            DEPLOYMENT_STATE_VALUES.contains(&deployment_now.as_str()),
            "case {case:?}: Deployment$State must be a DeploymentState member, which \
             {deployment_now:?} is not among {DEPLOYMENT_STATE_VALUES:?}"
        );

        let environment_now = environment_state(&router, &application, &environment)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no State"));
        assert_eq!(
            environment_now, pair.environment,
            "case {case:?}: GetEnvironment must report State {:?}, got {environment_now:?}",
            pair.environment
        );
        // And an environment's state is NOT: it is the CamelCase token of its member, so
        // serving the two enums the same way would fail here.
        assert!(
            !ENVIRONMENT_STATE_VALUES.contains(&environment_now.as_str()),
            "case {case:?}: Environment$State is served as the CamelCase token of its \
             member, not as the member name, got {environment_now:?} which is among \
             {ENVIRONMENT_STATE_VALUES:?}"
        );
        assert_ne!(
            environment_now, deployment_now,
            "case {case:?}: the two enums are served in different forms, so one release's \
             two states cannot be the same token, got {environment_now:?} for both"
        );
    }
}
