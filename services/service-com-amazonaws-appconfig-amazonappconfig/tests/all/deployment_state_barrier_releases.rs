// SPDX-License-Identifier: CC0-1.0

//! The completion BARRIER a client builds on `Deployment$State`, driven over a whole
//! release rather than at the single moment a table picks.
//!
//! The rule under test: each of this service's two state enums is served in the form AWS
//! serves THAT enum in, at every stage of a release and through every projection of it, so
//! a client that re-reads a deployment until its `State` is one of the members no further
//! transition follows (`COMPLETE`, `ROLLED_BACK`, `REVERTED` — `DeploymentState` carries no
//! `@enumValue` override, `smithy_query(query="shape", shape="DeploymentState")`, so each
//! member's wire token IS its name) releases as soon as the release settles. The sibling
//! `Environment$State` is NOT served that way: its token is the CamelCase spelling of the
//! member, so the two forms can never coincide on one release.
//!
//! What this suite adds to the sibling `crate::deployment_state_wire_form` and
//! `crate::deployment_and_environment_state_tokens` tables, which assert the token at the
//! stages they name: here the barrier itself is the assertion, so EVERY token the service
//! serves while a release is in flight is checked as it is read — a token outside the
//! deployment enum's members at any single stage would hang the loop instead of satisfying
//! it — and the two projections a poller has (`GetDeployment` and the environment's
//! `ListDeployments`) are checked to agree once the release has settled.
//!
//! The rows differ along what the rule generalises over: how the release is paced (nothing
//! to pace, a linear rollout, an exponential one), how it ends (the clock completes it, a
//! caller's stop rolls it back, a caller's revert undoes a completed one), and therefore
//! which member of each enum the release settles in.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::ops::types::{
    DEPLOYMENT_STATE_VALUES, ENVIRONMENT_STATE_VALUES,
};

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, router, TERMINAL_DEPLOYMENT_STATES,
};
use crate::deployment_state_wire_form::{is_a_published_deployment_state, listed_state};
use crate::deployment_wire_fidelity::{create_strategy, host_version, member, Row};
use crate::environment_state_wire_form::member_name_of;
use crate::stop_deployment::{environment_state, get_deployment, start, stop};

/// How long the barrier is given to release. Generous on purpose: the stages of a release
/// are owed by the CLOCK, and this waits for the CONDITION rather than for a span, so the
/// bound only has to be far larger than any stage the rows declare.
const BARRIER_BY: Duration = Duration::from_secs(180);

/// How often the barrier re-reads the deployment while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Everything the rule requires of one `Deployment$State` token, wherever it was read: it
/// is a member the model declares, and it is one of the values the crate's own catalog
/// publishes for that enum.
fn assert_deployment_form(case: &str, document: &str, token: &str) {
    assert!(
        DEPLOYMENT_STATE_VALUES.contains(&token),
        "case {case:?}: {document} must serve Deployment$State as a DeploymentState member, \
         which {token:?} is not among {DEPLOYMENT_STATE_VALUES:?}"
    );
    assert!(
        is_a_published_deployment_state(token),
        "case {case:?}: {document} must serve a published DeploymentState wire value, got \
         {token:?}"
    );
}

/// Everything the rule requires of one `Environment$State` token: it is the CamelCase
/// spelling of a member the model declares, and never the member name itself — the opposite
/// form from the deployment's, read off the same release.
fn assert_environment_form(case: &str, token: &str) {
    let named = member_name_of(token);
    assert!(
        ENVIRONMENT_STATE_VALUES.contains(&named.as_str()),
        "case {case:?}: GetEnvironment must serve Environment$State as the CamelCase token of \
         an EnvironmentState member, and {token:?} names {named:?}, which is not among \
         {ENVIRONMENT_STATE_VALUES:?}"
    );
    assert!(
        !ENVIRONMENT_STATE_VALUES.contains(&token),
        "case {case:?}: Environment$State is served as the CamelCase token of its member, not \
         as the member name, got {token:?}"
    );
}

/// A deployment number as the `DeploymentNumber` path member binds it, failing the case
/// rather than addressing some other deployment when it does not fit.
fn bound_number(case: &str, number: i64) -> i32 {
    i32::try_from(number)
        .unwrap_or_else(|_| panic!("case {case:?}: the deployment number {number} is not an i32"))
}

/// The `State` `GetDeployment` reports for one deployment, checked against the rule as it
/// is read.
async fn polled_state(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> String {
    let read = get_deployment(router, application, environment, bound_number(case, number)).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment must answer 200, got {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let token = read.string("State").unwrap_or_default();
    assert_deployment_form(case, "GetDeployment", token.as_str());
    token
}

/// Re-read the deployment until its `State` is one of the members no further transition
/// leads out of — the barrier a client waits on, spelled the way AWS spells those members —
/// answering the token it released on.
///
/// Every token read on the way is held to the rule too, and the environment's own token to
/// the opposite form, so a release that served one wrong spelling at one stage fails here
/// rather than at the end.
async fn wait_for_the_barrier(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> String {
    let deadline = Instant::now() + BARRIER_BY;
    loop {
        let token = polled_state(router, case, application, environment, number).await;
        if TERMINAL_DEPLOYMENT_STATES.contains(&token.as_str()) {
            return token;
        }
        let environment_token = environment_state(router, application, environment)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no State"));
        assert_environment_form(case, environment_token.as_str());
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the barrier did not release within {}s; deployment {number} last \
             reported {token:?}, and a client waits for one of {TERMINAL_DEPLOYMENT_STATES:?}",
            BARRIER_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How a row's release ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Nothing: the clock finishes it.
    Settles,
    /// `StopDeployment` with no `Allow-Revert`, which withdraws a release still in flight.
    Stopped,
    /// `StopDeployment` with `Allow-Revert: true` on a release that already completed.
    Reverted,
}

/// One row: how the release is paced, how it ends, and the token each of the two enums
/// carries once it has settled — written out literally, so a regression that re-spelled the
/// crate's own constants would not satisfy the row.
struct Release {
    /// The strategy's criteria, as `deployment_wire_fidelity`'s row type spells them.
    row: Row,
    ending: Ending,
    /// The `DeploymentState` member the settled release reports.
    deployment: &'static str,
    /// The `EnvironmentState` token its environment reports.
    environment: &'static str,
}

/// The rows. The durations are the model's own bounds: `MinutesBetween0And24Hours` is
/// `range 0..1440` and `Percentage` is `range 1..100`, so the 1% row is the longest rollout
/// the model can express — still in flight when the row stops it. No row declares a final
/// bake time, which is a window the caller asked for and which keeps the length they asked
/// for.
const RELEASES: &[Release] = &[
    Release {
        row: ("nothing to pace", 0, 100.0, "LINEAR", 0),
        ending: Ending::Settles,
        deployment: "COMPLETE",
        environment: "ReadyForDeployment",
    },
    Release {
        row: ("an exponential rollout", 1440, 50.0, "EXPONENTIAL", 0),
        ending: Ending::Settles,
        deployment: "COMPLETE",
        environment: "ReadyForDeployment",
    },
    Release {
        row: ("a linear rollout a stop withdraws", 1440, 1.0, "LINEAR", 0),
        ending: Ending::Stopped,
        deployment: "ROLLED_BACK",
        environment: "RolledBack",
    },
    Release {
        row: ("a revert of a completed release", 0, 100.0, "LINEAR", 0),
        ending: Ending::Reverted,
        deployment: "REVERTED",
        environment: "Reverted",
    },
];

/// RULE: a client that polls `GetDeployment` until `State` is one of the terminal
/// `DeploymentState` members releases when the release settles, because every token the
/// service serves for that member — at every stage, through every projection — is the member
/// itself, while the environment holding it reports the CamelCase token of ITS enum.
#[tokio::test]
async fn a_poll_for_a_terminal_deployment_state_releases_when_the_release_settles() {
    let router = router();
    let application = create_application(&router, "state-barrier").await;
    let profile = create_profile(&router, &application, "barrier").await;
    host_version(&router, &application, &profile).await;

    for (index, release) in RELEASES.iter().enumerate() {
        let case = release.row.0;
        let strategy = create_strategy(&router, 300 + index, &release.row).await;
        // One environment per row: an environment holds one release at a time.
        let environment =
            create_environment(&router, &application, &format!("barrier-{index}")).await;

        let started = start(&router, &application, &environment, &strategy, &profile).await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let accepted = started.json();
        assert_deployment_form(
            case,
            "StartDeployment",
            member(&accepted, "State").as_str().unwrap_or_default(),
        );
        let number = member(&accepted, "DeploymentNumber")
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: StartDeployment answered no number"));

        // A strategy that declares a rollout window leaves AppConfig a stage to perform
        // after the call, so a read taken straight away is of a release still in flight:
        // the token then is a member too, and not one the barrier accepts.
        let (_, duration, _, _, _) = release.row;
        if duration > 0 {
            let in_flight = polled_state(&router, case, &application, &environment, number).await;
            assert!(
                !TERMINAL_DEPLOYMENT_STATES.contains(&in_flight.as_str()),
                "case {case:?}: a release whose strategy declares a rollout window is still in \
                 flight right after the call that started it, got {in_flight:?}"
            );
        }

        if release.ending != Ending::Settles {
            let allow_revert = if release.ending == Ending::Reverted {
                // Only a release that COMPLETED can be reverted, and completing is owed by
                // the clock — so the row waits on the barrier first.
                let settled =
                    wait_for_the_barrier(&router, case, &application, &environment, number).await;
                assert_eq!(
                    settled, "COMPLETE",
                    "case {case:?}: the release must complete before a revert can undo it, got \
                     {settled:?}"
                );
                Some(true)
            } else {
                None
            };
            let stopped = stop(
                &router,
                &application,
                &environment,
                bound_number(case, number),
                allow_revert,
            )
            .await;
            assert_eq!(
                stopped.status,
                202,
                "case {case:?}: StopDeployment must answer 202, got {} / {}",
                stopped.status,
                String::from_utf8_lossy(&stopped.body)
            );
            assert_deployment_form(
                case,
                "StopDeployment",
                member(&stopped.json(), "State").as_str().unwrap_or_default(),
            );
        }

        let settled =
            wait_for_the_barrier(&router, case, &application, &environment, number).await;
        assert_eq!(
            settled, release.deployment,
            "case {case:?}: the barrier must release on State {:?}, got {settled:?}",
            release.deployment
        );

        // Both projections a poller has report the same settled token: a settled release no
        // longer moves, so the listing and the reader cannot disagree about it.
        let listed = listed_state(&router, case, &application, &environment, number).await;
        assert_eq!(
            listed, release.deployment,
            "case {case:?}: ListDeployments must report State {:?} for deployment {number}, got \
             {listed:?}",
            release.deployment
        );

        let environment_token = environment_state(&router, &application, &environment)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: GetEnvironment answered no State"));
        assert_environment_form(case, environment_token.as_str());
        assert_eq!(
            environment_token, release.environment,
            "case {case:?}: GetEnvironment must report State {:?}, got {environment_token:?}",
            release.environment
        );
        assert_ne!(
            environment_token, settled,
            "case {case:?}: the two enums are served in different forms, so one release's two \
             states cannot be the same token, got {environment_token:?} for both"
        );
    }
}
