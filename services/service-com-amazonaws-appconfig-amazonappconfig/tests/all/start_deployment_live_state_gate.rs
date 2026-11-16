// SPDX-License-Identifier: CC0-1.0

//! `StartDeployment`: the live state of the environment the call ADDRESSES is settled
//! before anything about what would be deployed is resolved.
//!
//! RULE: `StartDeployment` is `POST
//! /applications/{ApplicationId}/environments/{EnvironmentId}/deployments`
//! (`smithy_query(query="operation", operation="StartDeployment")`), so the environment is
//! the resource it addresses: while that environment is still working on a deployment, a
//! second one into it is refused with the modelled `ConflictException` (HTTP 409) naming the
//! ENVIRONMENT's own state — whatever the request's body names — and the body's references
//! (the configuration profile, the version of it, the deployment strategy) are resolved only
//! once the environment is free to take a deployment, where a reference that resolves to
//! nothing is that level's own `ResourceNotFoundException` (HTTP 404).
//!
//! The rows span what the rule generalises over rather than one recorded call: the body
//! reference that would otherwise resolve to nothing (a profile no application holds, a
//! profile holding no such version, a version the profile does not hold, a strategy nothing
//! holds, and a body in which everything resolves), plus the same gate for a profile DELETED
//! while the deployment it was deploying is still in flight.
//!
//! The other dimension the rule generalises over is WHICH PHASE of its flight the deployment
//! already holding the environment is in: a rollout and a final bake are both work AppConfig
//! has not finished, so each of them is the same conflict, and the state the refusal names is
//! the ENVIRONMENT's own (`DEPLOYING`, the only member `EnvironmentState` has for a deployment
//! in flight) rather than the `DeploymentState` of the deployment holding it — which is
//! `BAKING` for one monitoring its final bake time.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state, host_version,
    json_call, router, Answer,
};
use crate::deployment_wire_fidelity::{create_strategy, Row};
use crate::environment_state_wire_form::{is_a_published_environment_state, member_name_of};

/// The criteria of a deployment strategy whose rollout is long enough that the deployment it
/// paces is still in flight for the rest of a case: the longest rollout window the model
/// allows (`MinutesBetween0And24Hours` is `range 0..1440`) grown one percent at a time (the
/// lower bound of `Percentage`, `range 1..100`).
const SLOW_ROLLOUT: Row = ("still rolling out", 1440, 1.0, "LINEAR", 0);

/// `StartDeployment` of `version` of `profile` into `environment`, paced by `strategy`.
async fn start_deployment(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: &str,
) -> Answer {
    json_call(
        router,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
        }),
    )
    .await
}

/// The refusal AppConfig states for a deployment into an environment it is still working on:
/// the environment's id and the MEMBER NAME of the state that environment is in.
fn expected_conflict(environment: &str, state_member: &str) -> String {
    format!(
        "Environment {environment} currently has a state of {state_member}. The current \
         deployment must finish before a new deployment can be started."
    )
}

/// Assert that `answer` is the 409 refusal of `environment` reporting `state_member`, for the
/// row `case`.
fn assert_refused_with_the_environments_state(
    case: &str,
    answer: &Answer,
    environment: &str,
    state_member: &str,
) {
    assert_eq!(
        answer.status,
        409,
        "case {case:?}: a deployment into a busy environment is refused 409, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let code = answer.error_code();
    assert!(
        code.contains("ConflictException"),
        "case {case:?}: expected the modelled ConflictException, got {code:?}"
    );
    let expected = expected_conflict(environment, state_member);
    assert_eq!(
        answer.string("Message").unwrap_or_default(),
        expected,
        "case {case:?}: expected the refusal {expected:?}, got {:?}",
        answer.string("Message")
    );
}

/// Every deployment of `environment`, newest first.
async fn list_deployments(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
) -> Vec<Value> {
    let answer = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "ListDeployments must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// What the suite sends as the body of the second deployment: the case, the configuration
/// profile it names, the version of it, and the strategy that would pace it.
type Body<'a> = (&'a str, &'a str, &'a str, &'a str);

/// The rows the rule is asserted over: each one is a body whose references AppConfig would
/// otherwise have to resolve, including one in which every reference does resolve.
fn rows<'a>(profile: &'a str, spare: &'a str, strategy: &'a str) -> [Body<'a>; 5] {
    [
        (
            "a body in which every reference resolves",
            profile,
            "1",
            strategy,
        ),
        (
            "a configuration profile no application holds",
            "nosuchprofile",
            "1",
            strategy,
        ),
        (
            "a configuration profile that holds no version at all",
            spare,
            "1",
            strategy,
        ),
        (
            "a configuration version the profile does not hold",
            profile,
            "7",
            strategy,
        ),
        // A strategy id that satisfies `DeploymentStrategyId`'s pattern
        // (`^(^[a-z0-9]{4,7}$|^AppConfig\.[A-Za-z0-9]{9,40}$)$`, from
        // `smithy_query(query="operation", operation="StartDeployment")`) and that no
        // created or predefined strategy carries.
        (
            "a deployment strategy nothing holds",
            profile,
            "1",
            "zzzzzz",
        ),
    ]
}

/// RULE: while the environment is working on a deployment, every one of those bodies is
/// refused with the environment's own state — the conflict is decided on the resource the
/// call addresses, not on the references the body carries — and nothing is deployed.
#[tokio::test]
async fn a_busy_environment_refuses_every_body_with_its_own_state() {
    let router = router();
    let application = create_application(&router, "live-state-gate").await;
    let environment = create_environment(&router, &application, "production").await;
    let profile = create_profile(&router, &application, "config").await;
    let spare = create_profile(&router, &application, "spare-config").await;
    let strategy = create_strategy(&router, 0, &SLOW_ROLLOUT).await;
    host_version(
        &router,
        &application,
        &profile,
        "the version the first deployment rolls out",
        br#"{"enabled":true}"#,
        None,
    )
    .await;

    let started = start_deployment(
        &router,
        &application,
        &environment,
        &profile,
        &strategy,
        "1",
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "the first deployment must answer 201, got {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    assert_eq!(
        started.string("State").as_deref(),
        Some("Deploying"),
        "the first deployment must still be rolling out, got {}",
        started.json()
    );

    for (case, named_profile, version, named_strategy) in rows(&profile, &spare, &strategy) {
        let answer = start_deployment(
            &router,
            &application,
            &environment,
            named_profile,
            named_strategy,
            version,
        )
        .await;
        assert_refused_with_the_environments_state(case, &answer, &environment, "DEPLOYING");
        let numbers: Vec<i64> = list_deployments(&router, &application, &environment)
            .await
            .iter()
            .filter_map(|item| item.get("DeploymentNumber").and_then(Value::as_i64))
            .collect();
        assert_eq!(
            numbers,
            vec![1],
            "case {case:?}: a refused deployment deploys nothing, got {numbers:?}"
        );
    }
}

/// RULE: the references in the body ARE resolved once the environment can take a deployment
/// — the gate above decides the busy case, it does not swallow the lookups — so each of the
/// same bodies is that level's own 404 on an environment with nothing in flight, and the
/// body whose references all resolve is accepted.
#[tokio::test]
async fn an_environment_with_nothing_in_flight_resolves_the_body() {
    let router = router();
    let application = create_application(&router, "idle-environment").await;
    let environment = create_environment(&router, &application, "production").await;
    let profile = create_profile(&router, &application, "config").await;
    let spare = create_profile(&router, &application, "spare-config").await;
    let strategy = create_strategy(&router, 0, &SLOW_ROLLOUT).await;
    host_version(
        &router,
        &application,
        &profile,
        "the version the resolvable row deploys",
        br#"{"enabled":true}"#,
        None,
    )
    .await;

    let all = rows(&profile, &spare, &strategy);
    let (resolvable, unresolvable) = all
        .split_first()
        .expect("the row table carries the resolvable body first");
    for (case, named_profile, version, named_strategy) in unresolvable {
        let answer = start_deployment(
            &router,
            &application,
            &environment,
            named_profile,
            named_strategy,
            version,
        )
        .await;
        assert_eq!(
            answer.status,
            404,
            "case {case:?}: a reference that resolves to nothing is 404, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let code = answer.error_code();
        assert!(
            code.contains("ResourceNotFoundException"),
            "case {case:?}: expected the modelled ResourceNotFoundException, got {code:?}"
        );
        assert!(
            list_deployments(&router, &application, &environment)
                .await
                .is_empty(),
            "case {case:?}: a refused deployment deploys nothing"
        );
    }

    let (case, named_profile, version, named_strategy) = resolvable;
    let answer = start_deployment(
        &router,
        &application,
        &environment,
        named_profile,
        named_strategy,
        version,
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: the body every reference resolves for deploys, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// RULE: the gate reads the ENVIRONMENT, so it holds for a body naming a configuration
/// profile that no longer exists — deleting the leaves of a deployment that is in flight is
/// accepted, and a second deployment into that environment is still the environment's own
/// conflict rather than a lookup of what the body named.
#[tokio::test]
async fn a_deleted_configuration_profile_is_still_the_environments_conflict() {
    let router = router();
    let application = create_application(&router, "deleted-leaves").await;
    let environment = create_environment(&router, &application, "production").await;
    let profile = create_profile(&router, &application, "config").await;
    let strategy = create_strategy(&router, 0, &SLOW_ROLLOUT).await;
    let version = host_version(
        &router,
        &application,
        &profile,
        "the version the deployment rolls out",
        br#"{"enabled":true}"#,
        None,
    )
    .await;

    let started = start_deployment(
        &router,
        &application,
        &environment,
        &profile,
        &strategy,
        "1",
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "the first deployment must answer 201, got {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );

    // The leaves of the deployment in flight, deleted bottom-up: the hosted version being
    // deployed, then the configuration profile it belonged to.
    for (case, path) in [
        (
            "the hosted configuration version being deployed",
            format!(
                "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions/{version}"
            ),
        ),
        (
            "the configuration profile being deployed",
            format!("/applications/{application}/configurationprofiles/{profile}"),
        ),
    ] {
        let deleted = json_call(&router, "DELETE", &path, &Value::Null).await;
        assert_eq!(
            deleted.status,
            204,
            "case {case:?}: deleting a leaf of a deployment in flight is accepted, got {} / {}",
            deleted.status,
            String::from_utf8_lossy(&deleted.body)
        );
    }

    let answer = start_deployment(
        &router,
        &application,
        &environment,
        &profile,
        &strategy,
        "1",
    )
    .await;
    assert_refused_with_the_environments_state(
        "a configuration profile deleted while its deployment is in flight",
        &answer,
        &environment,
        "DEPLOYING",
    );
}

/// One phase of a deployment's flight: the criteria of the strategy that puts the first
/// deployment there (`deployment_wire_fidelity::Row` — the case, then
/// `DeploymentDurationInMinutes`, `GrowthFactor`, `GrowthType` and
/// `FinalBakeTimeInMinutes`), the `DeploymentState` that deployment reports in the phase, and
/// the `Environment$State` its environment reports while it does. Each state is written once,
/// as the value it goes on the wire as; the member name a refusal spells is derived from it
/// through the one wire-form mapping.
struct Phase {
    row: Row,
    deployment_state: &'static str,
    environment_state: &'static str,
}

/// The phases a deployment holds its environment through. The criteria stay inside the
/// model's own bounds: `MinutesBetween0And24Hours` is `range 0..1440` and `GrowthFactor`
/// targets `Percentage`, `range 1..100`
/// (`smithy_query(query="shape", shape="DeploymentStrategy")`).
const PHASES: &[Phase] = &[
    Phase {
        // The longest rollout the model allows, grown one percent at a time, so the
        // deployment is still rolling out for the whole case.
        row: SLOW_ROLLOUT,
        deployment_state: "Deploying",
        environment_state: "Deploying",
    },
    Phase {
        // No rollout window and the longest final bake: the rollout is over after its one
        // growth interval and the bake then holds the deployment, which is the phase whose
        // own state differs from the environment's.
        row: ("monitoring its final bake time", 0, 100.0, "LINEAR", 1440),
        deployment_state: "Baking",
        environment_state: "Deploying",
    },
    Phase {
        // A rollout in ten-percent steps with a bake window still to come: a strategy that
        // paces both stages rather than only one of them.
        row: ("rolling out with a bake window to come", 15, 10.0, "LINEAR", 10),
        deployment_state: "Deploying",
        environment_state: "Deploying",
    },
];

/// The `State` `GetEnvironment` reports for `environment`, checked to be one of the values
/// `Environment$State` goes on the wire as.
async fn environment_state(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> String {
    let answer = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: GetEnvironment must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let state = answer.string("State").unwrap_or_default();
    assert!(
        is_a_published_environment_state(&state),
        "case {case:?}: GetEnvironment must report a published EnvironmentState value, got \
         {state:?}"
    );
    state
}

/// RULE: whichever phase of its flight the deployment holding the environment is in, a second
/// deployment into it is the environment's own conflict — decided before the body's
/// references are resolved — and the state the refusal names is the one the environment
/// itself reports, never the `DeploymentState` of the deployment holding it.
#[tokio::test]
async fn the_conflict_reports_the_environments_state_in_every_phase_of_a_deployments_flight() {
    let router = router();
    let application = create_application(&router, "flight-phases").await;
    let profile = create_profile(&router, &application, "config").await;
    let spare = create_profile(&router, &application, "spare-config").await;
    host_version(
        &router,
        &application,
        &profile,
        "the version every phase deploys",
        br#"{"enabled":true}"#,
        None,
    )
    .await;

    for (index, phase) in PHASES.iter().enumerate() {
        let phase_case = phase.row.0;
        let strategy = create_strategy(&router, 300 + index, &phase.row).await;
        // Its own environment per phase: an environment takes one deployment at a time.
        let environment =
            create_environment(&router, &application, &format!("phase-{index}")).await;
        let started = start_deployment(
            &router,
            &application,
            &environment,
            &profile,
            &strategy,
            "1",
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {phase_case:?}: the first deployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let number = started
            .json()
            .get("DeploymentNumber")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                panic!("case {phase_case:?}: StartDeployment answered no DeploymentNumber")
            });

        // Every phase after the one `StartDeployment` answers is owed by the CLOCK, so the
        // case waits for the CONDITION rather than for a span.
        deployment_in_state(
            &router,
            phase_case,
            &application,
            &environment,
            number,
            &[phase.deployment_state],
        )
        .await;
        let observed = environment_state(&router, phase_case, &application, &environment).await;
        assert_eq!(
            observed, phase.environment_state,
            "case {phase_case:?}: GetEnvironment must report {:?} while its deployment is {:?}, \
             got {observed:?}",
            phase.environment_state, phase.deployment_state
        );
        let environment_member = member_name_of(&observed);
        // The member name of the state the deployment holding the environment is in, derived
        // the same way: the refusal must report the environment's rather than this one.
        let deployment_member = member_name_of(phase.deployment_state);

        for (body_case, named_profile, version, named_strategy) in rows(&profile, &spare, &strategy)
        {
            let case = format!("{phase_case} / {body_case}");
            let answer = start_deployment(
                &router,
                &application,
                &environment,
                named_profile,
                named_strategy,
                version,
            )
            .await;
            assert_refused_with_the_environments_state(
                &case,
                &answer,
                &environment,
                &environment_member,
            );
            // The deployment holding the environment can be in a state of its own that the
            // environment has no member for: the refusal reports the environment's.
            if deployment_member != environment_member {
                let message = answer.string("Message").unwrap_or_default();
                assert!(
                    !message.contains(&deployment_member),
                    "case {case:?}: the refusal names the ENVIRONMENT's state, not the state of \
                     the deployment holding it ({deployment_member:?}), got {message:?}"
                );
            }
            let numbers: Vec<i64> = list_deployments(&router, &application, &environment)
                .await
                .iter()
                .filter_map(|item| item.get("DeploymentNumber").and_then(Value::as_i64))
                .collect();
            assert_eq!(
                numbers,
                vec![number],
                "case {case:?}: a refused deployment deploys nothing, got {numbers:?}"
            );
        }
    }
}
