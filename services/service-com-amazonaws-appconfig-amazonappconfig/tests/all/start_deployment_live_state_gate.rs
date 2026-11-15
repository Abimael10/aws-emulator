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

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, host_version, json_call, router,
    Answer,
};

/// A deployment strategy whose rollout is long enough that the deployment it paces is still
/// in flight for the rest of the case: a whole day of LINEAR growth in one-percent steps.
async fn create_slow_strategy(router: &MultiServiceRouter, case: &str) -> String {
    json_call(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": format!("slow-{case}"),
            "DeploymentDurationInMinutes": 1440,
            "GrowthFactor": 1.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": 0,
            "ReplicateTo": "NONE",
        }),
    )
    .await
    .string("Id")
    .unwrap_or_else(|| panic!("case {case:?}: CreateDeploymentStrategy answered no Id"))
}

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

/// The refusal AppConfig states for a deployment into an environment it is still working on.
fn expected_conflict(environment: &str) -> String {
    format!(
        "Environment {environment} currently has a state of DEPLOYING. The current deployment \
         must finish before a new deployment can be started."
    )
}

/// Assert that `answer` is the 409 refusal of `environment`, for the row `case`.
fn assert_refused_with_the_environments_state(case: &str, answer: &Answer, environment: &str) {
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
    let expected = expected_conflict(environment);
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
type Row<'a> = (&'a str, &'a str, &'a str, &'a str);

/// The rows the rule is asserted over: each one is a body whose references AppConfig would
/// otherwise have to resolve, including one in which every reference does resolve.
fn rows<'a>(profile: &'a str, spare: &'a str, strategy: &'a str) -> [Row<'a>; 5] {
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
    let strategy = create_slow_strategy(&router, "live-state-gate").await;
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
        assert_refused_with_the_environments_state(case, &answer, &environment);
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
    let strategy = create_slow_strategy(&router, "idle-environment").await;
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
    let strategy = create_slow_strategy(&router, "deleted-leaves").await;
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
    );
}
