// SPDX-License-Identifier: CC0-1.0

//! The two barriers a client waits on before it treats a configuration release as done:
//! the deployment reaching its terminal state, and the environment becoming ready for the
//! next deployment.
//!
//! The rule under test: a state enum member goes on the wire as the value the service
//! publishes for that member, which for `DeploymentState` and `EnvironmentState` is not
//! the member's Smithy name but its CamelCase spelling — so a client that re-reads
//! `GetDeployment` until `State` is the published terminal value and then `GetEnvironment`
//! until `State` is the published ready value gets past both barriers, and every value it
//! reads on the way there is one of the published spellings rather than a member name.
//!
//! The barriers are the only completion signal AppConfig offers for a release, so this
//! drives them exactly as a deploy script does: over the FRONT DOOR, polling for the
//! condition until it holds.
//!
//! The table spans what the rule generalises over: how many stages the deployment's
//! strategy paces before it settles (none at all, one growth interval, four linear
//! intervals, an exponential canary), the growth type, the declared growth factor and the
//! declared rollout window. Which of those a release used changes which states a barrier
//! observes on the way through and changes nothing about their spelling, which is the
//! point. The expected spellings are written out literally: a regression that renamed the
//! crate's state constants back to the model's member names would still satisfy a test
//! written in terms of those constants.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, json_call, router,
};
use crate::deployment_state_wire_form::is_a_published_deployment_state;
use crate::deployment_wire_fidelity::{create_strategy, host_version, Row};
use crate::environment_state_wire_form::is_a_published_environment_state;

/// The published value a settled deployment reports, which is what a client's settle
/// barrier compares against. Source: the model's `DeploymentState` enum, whose members
/// carry no `@enumValue` trait, so each goes on the wire as its own name.
const SETTLED_DEPLOYMENT: &str = "COMPLETE";

/// The published value an environment with no deployment in flight reports. Source: the
/// AppConfig API Reference's valid-value list for `Environment$State`
/// (`ReadyForDeployment | Deploying | RollingBack | RolledBack | Reverted`).
const READY_ENVIRONMENT: &str = "ReadyForDeployment";

/// How long a barrier waits for the state it polls for.
///
/// A generous ceiling on the WAIT rather than a statement about any stage's span: the
/// stages a strategy paces are owed by the clock, and a release that has not settled by
/// here is one nothing is moving.
const BARRIER_DEADLINE: Duration = Duration::from_secs(120);

/// How often a barrier re-reads while waiting. The barrier waits for the CONDITION; this
/// is only how often it looks.
const BARRIER_INTERVAL: Duration = Duration::from_millis(50);

/// The releases driven, each a strategy's criteria: `DeploymentDurationInMinutes`,
/// `GrowthFactor`, `GrowthType` and `FinalBakeTimeInMinutes`. The bounds are the model's
/// own — `MinutesBetween0And24Hours` is `range 0..1440` and `Percentage` is `range 1..100`
/// (`smithy_query(query="shape", shape="DeploymentStrategy")`), and `GrowthType` is
/// `EXPONENTIAL | LINEAR`.
///
/// Rows differ in how many growth intervals the rollout is split into — a strategy that
/// paces nothing settles inside the call that starts it, 100% per interval takes one,
/// 25% linear takes four and 25% exponential takes three — which is what decides the
/// states a barrier sees before the terminal one.
const ROWS: [Row; 4] = [
    ("a strategy that paces nothing", 0, 100.0, "LINEAR", 0),
    ("one growth interval", 1, 100.0, "LINEAR", 0),
    ("four linear growth intervals", 10, 25.0, "LINEAR", 0),
    ("an exponential canary", 30, 25.0, "EXPONENTIAL", 0),
];

/// `GetDeployment`, asserted to answer 200, as the JSON document it served.
async fn get_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Value {
    let read = json_call(
        router,
        "GET",
        &format!(
            "/applications/{application}/environments/{environment}/deployments/{number}"
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment must answer 200, got {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()
}

/// `GetEnvironment`, asserted to answer 200, as the JSON document it served.
async fn get_environment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> Value {
    let read = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetEnvironment must answer 200, got {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()
}

/// The `State` a served document reports, which every operation serving that document
/// must carry.
fn served_state(case: &str, document_name: &str, document: &Value) -> String {
    document
        .get("State")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!("case {case:?}: {document_name} must report a State, got {document}")
        })
}

/// Re-read `GetDeployment` until `State` is `expected`, answering the document that
/// reported it.
///
/// Every value read on the way is asserted to be one of the published `DeploymentState`
/// spellings, so a barrier that releases on a value nothing publishes fails here rather
/// than passing by accident.
async fn deployment_settle_barrier(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
    expected: &str,
) -> Value {
    let deadline = Instant::now() + BARRIER_DEADLINE;
    let mut observed: Vec<String> = Vec::new();
    loop {
        let document = get_deployment(router, case, application, environment, number).await;
        let state = served_state(case, "GetDeployment", &document);
        assert!(
            is_a_published_deployment_state(&state),
            "case {case:?}: GetDeployment must report a published DeploymentState spelling, got \
             {state:?} (states read so far: {observed:?})"
        );
        if observed.last().map(String::as_str) != Some(state.as_str()) {
            observed.push(state.clone());
        }
        if state == expected {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a client waiting for GetDeployment to report {expected:?} never saw \
             it within {}s; it read {observed:?}",
            BARRIER_DEADLINE.as_secs()
        );
        tokio::time::sleep(BARRIER_INTERVAL).await;
    }
}

/// Re-read `GetEnvironment` until `State` is `expected`, with the same assertion on every
/// value read on the way.
async fn environment_settle_barrier(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    expected: &str,
) {
    let deadline = Instant::now() + BARRIER_DEADLINE;
    let mut observed: Vec<String> = Vec::new();
    loop {
        let document = get_environment(router, case, application, environment).await;
        let state = served_state(case, "GetEnvironment", &document);
        assert!(
            is_a_published_environment_state(&state),
            "case {case:?}: GetEnvironment must report a published EnvironmentState spelling, got \
             {state:?} (states read so far: {observed:?})"
        );
        if observed.last().map(String::as_str) != Some(state.as_str()) {
            observed.push(state.clone());
        }
        if state == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a client waiting for GetEnvironment to report {expected:?} never saw \
             it within {}s; it read {observed:?}",
            BARRIER_DEADLINE.as_secs()
        );
        tokio::time::sleep(BARRIER_INTERVAL).await;
    }
}

/// What `ListDeployments` reports as the `State` of the entry for `number`.
async fn listed_state(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Option<String> {
    let listing = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listing.status,
        200,
        "case {case:?}: ListDeployments must answer 200, got {} / {}",
        listing.status,
        String::from_utf8_lossy(&listing.body)
    );
    listing
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("DeploymentNumber").and_then(Value::as_i64) == Some(number))
                .cloned()
        })
        .and_then(|item| {
            item.get("State")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// A client that releases a configuration and waits for it to settle gets past both
/// barriers: `GetDeployment` reaches the published terminal value and `GetEnvironment`
/// then reports the published ready value — whatever the strategy paced on the way, and
/// with `StartDeployment` and `ListDeployments` spelling the state the same way.
#[tokio::test]
async fn both_release_settle_barriers_release_on_the_published_state_values() {
    let router = router();

    for (index, row) in ROWS.iter().enumerate() {
        let (case, _, _, _, _) = *row;
        let application = create_application(&router, &format!("barrier-app-{index}")).await;
        let environment = create_environment(&router, &application, &format!("barrier-{index}")).await;
        let profile = create_profile(&router, &application, &format!("barrier-config-{index}")).await;
        host_version(&router, &application, &profile).await;
        let strategy = create_strategy(&router, index, row).await;

        let started = json_call(
            &router,
            "POST",
            &format!("/applications/{application}/environments/{environment}/deployments"),
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let accepted = started.json();
        let accepted_state = served_state(case, "StartDeployment", &accepted);
        assert!(
            is_a_published_deployment_state(&accepted_state),
            "case {case:?}: StartDeployment must report a published DeploymentState spelling, got \
             {accepted_state:?}"
        );
        let number = accepted
            .get("DeploymentNumber")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartDeployment answered no DeploymentNumber: {accepted}")
            });

        // The environment the release went into is serving the same catalog from the
        // first read on, whichever state the strategy's pacing leaves it in.
        let in_flight = get_environment(&router, case, &application, &environment).await;
        let in_flight_state = served_state(case, "GetEnvironment", &in_flight);
        assert!(
            is_a_published_environment_state(&in_flight_state),
            "case {case:?}: GetEnvironment must report a published EnvironmentState spelling, got \
             {in_flight_state:?}"
        );

        // Barrier one: the deployment settles.
        let settled = deployment_settle_barrier(
            &router,
            case,
            &application,
            &environment,
            number,
            SETTLED_DEPLOYMENT,
        )
        .await;
        assert_eq!(
            settled.get("PercentageComplete").and_then(Value::as_f64),
            Some(100.0),
            "case {case:?}: a deployment reporting {SETTLED_DEPLOYMENT:?} has reached every \
             target, got {:?}",
            settled.get("PercentageComplete")
        );

        // The listing of the same environment's history spells it the same way.
        let listed = listed_state(&router, case, &application, &environment, number).await;
        assert_eq!(
            listed.as_deref(),
            Some(SETTLED_DEPLOYMENT),
            "case {case:?}: ListDeployments must report State {SETTLED_DEPLOYMENT:?} for \
             deployment {number}, got {listed:?}"
        );

        // Barrier two: the environment is ready for the next deployment.
        environment_settle_barrier(&router, case, &application, &environment, READY_ENVIRONMENT)
            .await;
    }
}
