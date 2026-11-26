// SPDX-License-Identifier: CC0-1.0

//! A rollout advances ONLY at the instants its strategy's schedule makes due, and reading one
//! never advances it.
//!
//! RULE: a deployment strategy's `GrowthFactor` is "the percentage of targets that received a
//! deployed configuration during each interval" (`smithy_query(query="shape",
//! shape="DeploymentStrategy")`) and a deployment's `PercentageComplete` is "the percentage of
//! targets for which the deployment is available" (`smithy_query(query="doc",
//! shape="Deployment", member="PercentageComplete")`), so the share a rollout has reached
//! grows at the intervals of its rollout window and at NO other instant: at every instant
//! between two of them the share the deployment reports, the `DeploymentState` it is in and
//! the targets its configuration is in force for are exactly what the earlier one left, and
//! the share only ever grows. Reading the deployment is not one of those instants, so the
//! share a reader is answered is the same share however many readers ask and however long
//! after.
//!
//! That is what lets a consumer observe a rollout part-way through at all: a share a reader
//! could overtake by being slow, or move by asking twice, would be a share nothing could be
//! served at. It is also what lets a suite hold a rollout at one share — by putting the
//! instants of its schedule out of reach, never by racing them — which is how every suite
//! here reads a fleet mid-rollout.
//!
//! The rows differ along what the rule generalises over: the growth TYPE, the growth FACTOR
//! and therefore how many intervals the rollout takes, and within each row every interval of
//! the window is walked and probed just after it opens, at its midpoint and just before the
//! next one falls due. Nothing here waits for any span: the schedule is walked at instants
//! this file names, and the leg that goes over the wire holds the schedule out of reach.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state::{
    self, Deployment, DeploymentStrategy, RolloutBucket,
};

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state, host_version,
    instant_deployment_strategy, json_call, new_deployment, router_on,
};
use crate::configuration_profile_validators::start_deployment;
use crate::rollout_percentage_gates_serving::{create_strategy, pin_percentage};

/// The rollout window every row declares, in minutes: the longest the model's
/// `DeploymentDurationInMinutes` admits (`smithy_query(query="shape",
/// shape="DeploymentStrategy")` gives the range `0..=1440`), so every row's strategy paces a
/// stage of its own rather than being performed inside the call that starts it.
const WINDOW_IN_MINUTES: i32 = 1_440;

/// The percentage points every reading probes the rollout's targets at: below, at and above
/// each share the rows pass through, and both ends of the range a target's position can hold.
const PROBES: &[f32] = &[0.0, 0.5, 9.9, 10.0, 24.9, 25.0, 49.9, 50.0, 75.0, 80.0, 99.9];

/// One rollout to walk: the growth type and factor pacing it.
struct Row {
    case: &'static str,
    growth_type: &'static str,
    growth_factor: f32,
}

/// The rows span both growth types the model names (`smithy_query(query="shape",
/// shape="GrowthType")`) and, within each, factors that take a different number of intervals
/// to reach every target: the smallest the member admits (`GrowthFactor` ranges `1..=100`),
/// which takes a hundred of them, factors that take a handful, and the one that takes a single
/// interval and is therefore never partial.
const ROWS: &[Row] = &[
    Row {
        case: "linear, 1% of the targets per interval",
        growth_type: state::GROWTH_LINEAR,
        growth_factor: 1.0,
    },
    Row {
        case: "linear, 20% of the targets per interval",
        growth_type: state::GROWTH_LINEAR,
        growth_factor: 20.0,
    },
    Row {
        case: "linear, 25% of the targets per interval",
        growth_type: state::GROWTH_LINEAR,
        growth_factor: 25.0,
    },
    Row {
        case: "linear, every target in one interval",
        growth_type: state::GROWTH_LINEAR,
        growth_factor: 100.0,
    },
    Row {
        case: "exponential, a 10% canary",
        growth_type: state::GROWTH_EXPONENTIAL,
        growth_factor: 10.0,
    },
    Row {
        case: "exponential, a 25% canary",
        growth_type: state::GROWTH_EXPONENTIAL,
        growth_factor: 25.0,
    },
];

/// What one read of a deployment sees of where its rollout stands.
///
/// The share is kept as its exact BITS: what is asserted between two due instants is that the
/// record did not move at all, so two readings are compared for identity rather than with a
/// tolerance.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reading {
    state: String,
    share_bits: u32,
    /// Whether the deployment's configuration is in force for the target at each of
    /// [`PROBES`].
    served: Vec<bool>,
}

impl Reading {
    /// The share this reading saw, for the assertions that report it.
    fn share(&self) -> f32 {
        f32::from_bits(self.share_bits)
    }
}

/// Read where `deployment` stands, without changing it.
fn reading(deployment: &Deployment) -> Reading {
    Reading {
        state: deployment.state.clone(),
        share_bits: deployment.percentage_complete.to_bits(),
        served: PROBES
            .iter()
            .map(|probe| {
                deployment
                    .serves_the_configuration_to(RolloutBucket::at_percentile(f64::from(*probe)))
            })
            .collect(),
    }
}

/// A strategy record, as `CreateDeploymentStrategy` would have stored one.
fn strategy(growth_type: &str, growth_factor: f32) -> DeploymentStrategy {
    DeploymentStrategy {
        id: "held001".to_string(),
        name: "held".to_string(),
        description: None,
        deployment_duration_in_minutes: WINDOW_IN_MINUTES,
        growth_type: growth_type.to_string(),
        growth_factor,
        final_bake_time_in_minutes: 0,
        replicate_to: state::REPLICATE_TO_NONE.to_string(),
        tags: HashMap::new(),
    }
}

/// RULE, against the state records: between the instant one growth interval falls due and the
/// instant the next one does, a deployment reports the same share, the same state and the same
/// targets in force however often it is read — and at the instant the next one falls due the
/// share it has reached grows, until the rollout has reached every target.
///
/// Walked at instants this test names, so the schedule this asserts is the strategy's and no
/// part of the outcome is the host's speed.
#[test]
fn a_rollout_stands_still_between_the_instants_its_schedule_makes_due() {
    let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    for row in ROWS {
        let case = row.case;
        let paced_by = strategy(row.growth_type, row.growth_factor);
        let mut deployment = Deployment::start(new_deployment(), &paced_by, started_at);
        let interval = deployment.growth_interval();
        assert!(
            !interval.is_zero(),
            "case {case:?}: a strategy with a rollout window paces its intervals, so one is \
             not instant"
        );
        let intervals = deployment.rollout_duration().as_nanos() / interval.as_nanos();

        for step in 0..intervals {
            let step = u32::try_from(step).expect("the interval count fits the span arithmetic");
            let opened_at = started_at + interval * step;
            deployment.materialize(opened_at);
            let opened = reading(&deployment);

            // Every instant strictly inside the interval: the record is owed nothing until the
            // next one falls due, so each of these reads sees exactly what the interval opened
            // with.
            for inside in [
                Duration::from_nanos(1),
                Duration::from_millis(1),
                interval / 2,
                interval - Duration::from_nanos(1),
            ] {
                deployment.materialize(opened_at + inside);
                let probed = reading(&deployment);
                assert_eq!(
                    probed,
                    opened,
                    "case {case:?}: interval {step} read {inside:?} in must report what the \
                     interval opened with, {:?} at {}%, got {:?} at {}%",
                    opened.state,
                    opened.share(),
                    probed.state,
                    probed.share()
                );
            }

            // And at the instant the next interval falls due the rollout has reached a further
            // share of its targets — unless it had already reached them all.
            deployment.materialize(opened_at + interval);
            let grown = reading(&deployment);
            assert!(
                grown.share() >= opened.share(),
                "case {case:?}: interval {step} must not take the rollout back, it went from \
                 {}% to {}%",
                opened.share(),
                grown.share()
            );
            if opened.share() < 100.0 {
                assert!(
                    grown.share() > opened.share(),
                    "case {case:?}: the instant after interval {step} must reach a further \
                     share of the targets, the rollout is still at {}%",
                    grown.share()
                );
            }
        }

        // The walk covered the whole window, so the rollout has reached every target and the
        // deployment is no longer one the clock owes anything.
        let ended = reading(&deployment);
        assert_eq!(
            ended.share(),
            100.0,
            "case {case:?}: a rollout whose window has run out has reached every target, got \
             {}%",
            ended.share()
        );
        assert_eq!(
            ended.state,
            state::DEPLOYMENT_COMPLETE,
            "case {case:?}: a rollout that reached every target and bakes for no time is \
             {:?}, got {:?}",
            state::DEPLOYMENT_COMPLETE,
            ended.state
        );
    }
}

/// The two versions the wire leg deploys, told apart by their bytes.
const PREVIOUS: &[u8] = br#"{"tier":"one"}"#;
/// See [`PREVIOUS`].
const NEW: &[u8] = br#"{"tier":"two"}"#;

/// The share the wire leg holds its rollout at: one strictly between the two ends, so both
/// versions are in service and a reader that moved the rollout would be visible in what the
/// fleet is served.
const HELD_AT: f32 = 50.0;

/// How many clients read the configuration in each round.
const CLIENTS: usize = 24;

/// How many rounds the whole fleet, and the deployment itself, are read for.
const ROUNDS: usize = 4;

/// RULE over the wire: while a rollout's schedule is out of reach, every round of reads is
/// answered identically — `GetDeployment` reports one share and `GetConfiguration` serves each
/// client the version that share puts it on — because reading a deployment is not one of the
/// instants that advance it.
#[tokio::test]
async fn reading_a_held_rollout_over_and_over_changes_nothing_it_answers() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let application = create_application(&router, "held-app").await;
    let environment = create_environment(&router, &application, "production").await;
    let profile = create_profile(&router, &application, "flags").await;
    let case = "a rollout held at half its targets";

    let previous_version = host_version(&router, &application, &profile, case, PREVIOUS, None).await;
    let new_version = host_version(&router, &application, &profile, case, NEW, None).await;

    // The previous version goes out with a strategy that paces nothing, so it is in force for
    // every client before the rollout starts.
    let instant = instant_deployment_strategy(&router, "instant").await;
    let seeded = deployment_number(
        &router,
        case,
        &application,
        &environment,
        &profile,
        &instant,
        &previous_version.to_string(),
    )
    .await;
    deployment_in_state(
        &router,
        case,
        &application,
        &environment,
        seeded,
        &["COMPLETE"],
    )
    .await;

    // The rollout under test grows by one percentage point of its targets per growth interval,
    // and an interval is a span of its own rather than a share of the declared window, so the
    // rollout runs over a hundred of them: the window only makes the strategy pace a stage at
    // all.
    let gradual = create_strategy(&router, "gradual", 1.0, WINDOW_IN_MINUTES, 0).await;
    let number = deployment_number(
        &router,
        case,
        &application,
        &environment,
        &profile,
        &gradual,
        &new_version.to_string(),
    )
    .await;
    pin_percentage(&engine, &application, &environment, HELD_AT);

    let mut first: Option<Vec<Vec<u8>>> = None;
    for round in 1..=ROUNDS {
        let document = json_call(
            &router,
            "GET",
            &format!(
                "/applications/{application}/environments/{environment}/deployments/{number}"
            ),
            &serde_json::Value::Null,
        )
        .await;
        assert_eq!(
            document.status,
            200,
            "case {case:?}: round {round}: GetDeployment must answer 200, got {} / {}",
            document.status,
            String::from_utf8_lossy(&document.body)
        );
        let reported = document.json()["PercentageComplete"].as_f64();
        assert_eq!(
            reported,
            Some(f64::from(HELD_AT)),
            "case {case:?}: round {round}: GetDeployment must report the share the rollout \
             stands at, got {reported:?}"
        );

        let mut fleet = Vec::with_capacity(CLIENTS);
        for client in 0..CLIENTS {
            let read = json_call(
                &router,
                "GET",
                &format!(
                    "/applications/{application}/environments/{environment}/configurations/\
                     {profile}?client_id=client-{client:04}"
                ),
                &serde_json::Value::Null,
            )
            .await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: round {round}: GetConfiguration for client-{client:04} must \
                 answer 200, got {} / {}",
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            let served = read.body.to_vec();
            assert!(
                served == PREVIOUS || served == NEW,
                "case {case:?}: round {round}: client-{client:04} must be served one of the two \
                 deployed versions' exact bytes, got {:?}",
                String::from_utf8_lossy(&served)
            );
            fleet.push(served);
        }

        match &first {
            // The precondition the rounds are judged against: the held share gates what the
            // fleet receives, so both versions are in service while it is read.
            None => {
                let on_new = fleet.iter().filter(|served| *served == NEW).count();
                assert!(
                    on_new > 0 && on_new < CLIENTS,
                    "case {case:?}: a rollout held at {HELD_AT}% must leave BOTH versions in \
                     service, got {on_new} of {CLIENTS} clients on the new version"
                );
                first = Some(fleet);
            }
            Some(first) => assert_eq!(
                &fleet,
                first,
                "case {case:?}: round {round}: every client must be served what the first round \
                 served it, {} of {CLIENTS} were not",
                fleet
                    .iter()
                    .zip(first.iter())
                    .filter(|(now, before)| now != before)
                    .count()
            ),
        }
    }
}

/// `StartDeployment` of `version` through the suite-shared helper, answering the deployment
/// number it minted.
async fn deployment_number(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    strategy_id: &str,
    version: &str,
) -> i64 {
    let answer =
        start_deployment(router, application, environment, profile, strategy_id, version).await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: StartDeployment({version:?}) must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json()["DeploymentNumber"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: StartDeployment answered no DeploymentNumber"))
}
