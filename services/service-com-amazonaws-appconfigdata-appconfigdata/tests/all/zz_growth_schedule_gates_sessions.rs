// SPDX-License-Identifier: CC0-1.0

//! A rollout the CLOCK carries through its growth schedule is partial in what the data
//! plane serves at every step of that schedule, and the fleet converges only at the end.
//!
//! RULE: a deployment strategy's `GrowthType` "define[s] how percentage grows over time" —
//! `LINEAR` "deploys the configuration to 10 percent of the hosts. After those deployments
//! are complete, the system deploys the configuration to the next 10 percent … until 100%
//! of the targets have successfully received the configuration", and `EXPONENTIAL`
//! "processes the deployment exponentially using the following formula: `G*(2^N)` … until
//! the configuration has been deployed to all targets"
//! (`smithy_query(query="doc",shape="CreateDeploymentStrategyRequest",member="GrowthType")`),
//! each cumulative percentage capped at the 100 that bounds the `Percentage` shape
//! `GrowthFactor` and `PercentageComplete` both target. The percentage a rollout has
//! reached is a SERVING decision rather than a label: `PercentageComplete` is "the
//! percentage of targets for which the deployment is available"
//! (`smithy_query(query="doc",shape="Deployment",member="PercentageComplete")`), so while a
//! deployment is in flight at a percentage strictly between 0 and 100, independently opened
//! configuration sessions over the same application/environment/configuration-profile
//! triple are SPLIT — each is served one deployed version's exact bytes, some the previous
//! version's and some the new one's, none a blend of the two — and every session converges
//! on the new version only once the rollout has reached every target. A session the rollout
//! has already reached keeps the new version for as long as that rollout stands, because
//! the share a rollout has reached only grows.
//!
//! Asserted with NOTHING written into either store: the deployment advances because the
//! clock advances it, each reading of `PercentageComplete` is taken from `GetDeployment`
//! over HTTP, and the sessions sampled at each reading are opened and polled through
//! AppConfigData's own operations. The rows span what the rule generalises over — both
//! `GrowthType` values and growth factors that take a different number of steps to reach
//! every target — and each row's grid is written out from the model's formula above rather
//! than read back from the emulator, so a row also pins the schedule the split follows.
//!
//! The companion suites assert the two halves separately:
//! `rollout_percentage_splits_sessions` pins the gate itself over a table of percentages
//! written into the store, and `zz_timing` pins that the clock is what moves a rollout
//! between its ends. This one is the whole canary: a paced rollout, watched from the data
//! plane while it grows.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;

use super::configuration_session_poll::{Emulator, Hierarchy};
use super::rollout_percentage_splits_sessions::{host_version, start_deployment, NEW, PREVIOUS};

/// How long a rollout is given to show itself part-way through and then to settle before
/// the suite fails. A ceiling on the WAIT, not a statement about the span a growth
/// interval takes: a rollout that has not moved by here is one nothing is advancing.
const SETTLE_BY: Duration = Duration::from_secs(180);

/// How often the deployment under observation is re-read.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How many fresh sessions are sampled at each reading taken while the rollout is part-way
/// through. Each session's position in the rollout is independent of every other's, so a
/// handful per reading accumulated over the steps of a schedule splits with certainty.
const SAMPLE_PER_READING: usize = 4;

/// How many fresh sessions are sampled once the rollout has reached every target, where
/// every one of them must be served the new version.
const SAMPLE_AFTER: usize = 8;

/// How far two percentages may differ and still be the same point of a grid: the member is
/// a float on the wire, so readings are compared with slack rather than for bit equality.
const PERCENTAGE_SLACK: f64 = 0.01;

/// The `DeploymentState` a rollout nobody stopped ends in, in the CamelCase form the
/// enum's members go on the wire as.
const COMPLETE: &str = "Complete";

/// The `DeploymentState` values a deployment stops at, after which it advances no further.
const TERMINAL_STATES: &[&str] = &["Complete", "RolledBack", "Reverted"];

/// One rollout to watch: the strategy pacing it and the cumulative percentage of targets
/// each of its growth steps reaches.
struct Row {
    case: &'static str,
    /// The modelled `GrowthType` (`smithy_query(query="shape",shape="GrowthType")`).
    growth_type: &'static str,
    growth_factor: f32,
    /// The schedule the model's own formula gives for that growth type and factor.
    grid: &'static [f64],
}

/// The canary the `EXPONENTIAL` documentation describes: a growth factor of 10 reaches
/// `10*(2^N)` percent of the targets at step `N`, so 10%, then 20%, 40%, 80% and finally
/// every target.
const EXPONENTIAL_CANARY: Row = Row {
    case: "exponential, 10% canary",
    growth_type: "EXPONENTIAL",
    growth_factor: 10.0,
    grid: &[10.0, 20.0, 40.0, 80.0, 100.0],
};

/// An `EXPONENTIAL` rollout whose factor takes a different number of steps: `25*(2^N)`
/// reaches 25%, 50% and then every target.
const EXPONENTIAL_QUARTER: Row = Row {
    case: "exponential, 25% canary",
    growth_type: "EXPONENTIAL",
    growth_factor: 25.0,
    grid: &[25.0, 50.0, 100.0],
};

/// A `LINEAR` rollout, which adds its growth factor per step: 20%, 40%, 60%, 80% and then
/// every target.
const LINEAR_FIFTH: Row = Row {
    case: "linear, 20% per step",
    growth_type: "LINEAR",
    growth_factor: 20.0,
    grid: &[20.0, 40.0, 60.0, 80.0, 100.0],
};

/// RULE for the `EXPONENTIAL` growth type at the factor its documentation uses as the
/// example of a canary.
#[tokio::test]
async fn an_exponential_rollout_is_partial_in_what_sessions_are_served() {
    watch_a_rollout(&EXPONENTIAL_CANARY).await;
}

/// RULE for an `EXPONENTIAL` rollout that takes a different number of steps to reach every
/// target, so the split is not a property of one schedule length.
#[tokio::test]
async fn an_exponential_rollout_of_fewer_steps_is_partial_too() {
    watch_a_rollout(&EXPONENTIAL_QUARTER).await;
}

/// RULE for the other `GrowthType`: a `LINEAR` rollout is gated by the share it has reached
/// in exactly the same way.
#[tokio::test]
async fn a_linear_rollout_is_partial_in_what_sessions_are_served() {
    watch_a_rollout(&LINEAR_FIFTH).await;
}

/// Deploy one version, then roll a second one out under `row`'s strategy and watch the
/// rollout from the data plane: every reading of `PercentageComplete` must be a point of
/// `row`'s grid, the sessions sampled while the reading is strictly between 0 and 100 must
/// be served BOTH versions' exact bytes, and once the deployment completes every session
/// must be served the new version.
async fn watch_a_rollout(row: &Row) {
    let case = row.case;
    let emulator = Emulator::new();
    let hierarchy = emulator
        .create_hierarchy("canary-app", "production", "flags")
        .await;

    // The previous version goes out with a strategy that has nothing to pace and is left to
    // settle, so it is the version in force for every session when the rollout starts.
    emulator
        .deploy(
            &hierarchy,
            &hierarchy.profile_id,
            PREVIOUS,
            "application/json",
            None,
        )
        .await;
    emulator.settle_deployments(&hierarchy);

    // The rollout of the new version declares a window to grow over and no final bake
    // time, so the deployment completes as soon as its last growth step has been reached.
    let version = host_version(&emulator, &hierarchy, NEW).await;
    let strategy = paced_strategy(&emulator, row).await;
    let number = start_deployment(&emulator, &hierarchy, &strategy, &version).await;

    // One session that polls right through the rollout, beside the fresh ones sampled at
    // each reading: what it is served must only ever move FORWARD, because the share a
    // rollout has reached only grows.
    let mut standing = StandingSession::opened(&emulator, &hierarchy).await;

    let mut on_new = 0usize;
    let mut on_previous = 0usize;
    let mut readings: Vec<f64> = Vec::new();
    let mut seen_part_way = false;
    let deadline = Instant::now() + SETTLE_BY;
    loop {
        let document = get_deployment(case, &emulator, &hierarchy, number).await;
        let reached = percentage_complete(case, &document);
        let state = deployment_state(case, &document);
        if readings.last().is_none_or(|last| *last != reached) {
            readings.push(reached);
        }
        standing.poll(case, &emulator).await;

        if reached > 0.0 && reached < 100.0 {
            seen_part_way = true;
            for index in 0..SAMPLE_PER_READING {
                let served = poll_a_fresh_session(&emulator, &hierarchy).await;
                match served.as_slice() {
                    bytes if bytes == NEW => on_new += 1,
                    bytes if bytes == PREVIOUS => on_previous += 1,
                    other => panic!(
                        "case {case:?}: a session opened at {reached}% of the rollout must be \
                         served one of the two deployed versions' exact bytes, session \
                         {index} was served {:?}",
                        String::from_utf8_lossy(other)
                    ),
                }
            }
        }

        // Enough has been observed once both versions have been served at the same point of
        // one rollout; and nothing more will be observed once the deployment has stopped
        // advancing, or once the wait's ceiling is reached — both of which the assertions
        // below report on.
        if (on_new > 0 && on_previous > 0)
            || TERMINAL_STATES.contains(&state.as_str())
            || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    assert!(
        seen_part_way,
        "case {case:?}: a rollout growing in {} steps must be observable part-way through, \
         but every reading of PercentageComplete was 0 or 100 ({readings:?})",
        row.grid.len()
    );
    assert!(
        on_new > 0 && on_previous > 0,
        "case {case:?}: the rollout was not partial — across {} mid-rollout sessions only \
         {} was ever served, so the new version reached either none or all of the fleet \
         instead of a percentage of it (readings {readings:?})",
        on_new + on_previous,
        if on_new > 0 { "the new version" } else { "the previous version" }
    );
    assert_on_the_grid(case, row.grid, &readings);

    // And the fleet converges: once the rollout has reached every target, every session
    // opened afterwards is served the new version.
    let state = wait_for_completion(case, &emulator, &hierarchy, number).await;
    assert_eq!(
        state, COMPLETE,
        "case {case:?}: a rollout nobody stopped must reach {COMPLETE}, got {state}"
    );
    for index in 0..SAMPLE_AFTER {
        let served = poll_a_fresh_session(&emulator, &hierarchy).await;
        assert_eq!(
            served.as_slice(),
            NEW,
            "case {case:?}: once the rollout has reached every target every session must be \
             served the new version's bytes, session {index} was served {:?}",
            String::from_utf8_lossy(&served)
        );
    }

    // Including the session that was already open while the rollout grew: the version it
    // holds is the new one, whether the rollout reached it part-way through or at the end.
    standing.poll(case, &emulator).await;
    assert_eq!(
        standing.served.as_slice(),
        NEW,
        "case {case:?}: a session polling right through the rollout must hold the new \
         version once the rollout has reached every target, it holds {:?}",
        String::from_utf8_lossy(&standing.served)
    );
}

/// Every reading taken while the rollout was under way must be a point of the schedule the
/// growth type prescribes — 0 for a rollout that has reached nobody yet — and the readings
/// must never go backwards: the share a rollout has reached only grows.
fn assert_on_the_grid(case: &str, grid: &[f64], readings: &[f64]) {
    let mut highest = 0.0f64;
    for reading in readings {
        assert!(
            *reading >= highest - PERCENTAGE_SLACK,
            "case {case:?}: PercentageComplete must never go backwards, read {reading} after \
             {highest} (readings {readings:?})"
        );
        highest = highest.max(*reading);
        let on_grid = *reading == 0.0
            || grid
                .iter()
                .any(|point| (point - reading).abs() < PERCENTAGE_SLACK);
        assert!(
            on_grid,
            "case {case:?}: PercentageComplete must take the percentages the growth type \
             prescribes ({grid:?}), read {reading} (readings {readings:?})"
        );
    }
}

/// Re-read the deployment until it stops advancing, answering the `DeploymentState` it
/// stopped at. The transitions are the clock's, so this waits for the CONDITION rather than
/// for a span.
async fn wait_for_completion(
    case: &str,
    emulator: &Emulator,
    hierarchy: &Hierarchy,
    number: i64,
) -> String {
    let deadline = Instant::now() + SETTLE_BY;
    loop {
        let document = get_deployment(case, emulator, hierarchy, number).await;
        let state = deployment_state(case, &document);
        if TERMINAL_STATES.contains(&state.as_str()) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the rollout never settled — it is still {state} at \
             {}% of its targets",
            percentage_complete(case, &document)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One session that stays open across the whole rollout, and the version it currently
/// holds.
///
/// A poll's payload "may be empty if the client already has the latest version of
/// configuration"
/// (`smithy_query(query="doc",shape="GetLatestConfigurationResponse",member="Configuration")`),
/// so the version a session holds is the last non-empty payload it was served, carried
/// forward across the polls that answer nothing new.
struct StandingSession {
    token: String,
    /// The bytes this session currently holds.
    served: Vec<u8>,
}

impl StandingSession {
    /// Open a session over the triple, before anything is read from it.
    async fn opened(emulator: &Emulator, hierarchy: &Hierarchy) -> Self {
        let token = emulator
            .session_token(
                &hierarchy.application_id,
                &hierarchy.environment_id,
                &hierarchy.profile_id,
            )
            .await;
        Self {
            token,
            served: Vec::new(),
        }
    }

    /// Poll this session once, carrying its token forward: what it is served must be one of
    /// the two deployed versions' exact bytes or nothing new, and a session the rollout has
    /// already reached is never taken BACK to the previous version while that rollout
    /// stands.
    async fn poll(&mut self, case: &str, emulator: &Emulator) {
        let reply = emulator.get_latest_configuration(&self.token).await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: a standing session's poll must answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        self.token = reply.next_token();
        if reply.body.is_empty() {
            return;
        }
        let served = reply.body.to_vec();
        assert!(
            served == NEW || served == PREVIOUS,
            "case {case:?}: a standing session must be served one of the two deployed \
             versions' exact bytes, got {:?}",
            String::from_utf8_lossy(&served)
        );
        assert!(
            self.served != NEW || served == NEW,
            "case {case:?}: a session the rollout has already reached must not be taken back \
             to the previous version, it was served {:?}",
            String::from_utf8_lossy(&served)
        );
        self.served = served;
    }
}

/// Open one session over the triple, poll it once, and answer the bytes it was served.
async fn poll_a_fresh_session(emulator: &Emulator, hierarchy: &Hierarchy) -> Vec<u8> {
    let token = emulator
        .session_token(
            &hierarchy.application_id,
            &hierarchy.environment_id,
            &hierarchy.profile_id,
        )
        .await;
    let reply = emulator.get_latest_configuration(&token).await;
    assert_eq!(
        reply.status,
        200,
        "GetLatestConfiguration must answer 200, got {} body {}",
        reply.status,
        reply.text()
    );
    reply.body.to_vec()
}

/// The `Deployment` document `GetDeployment` answers.
async fn get_deployment(
    case: &str,
    emulator: &Emulator,
    hierarchy: &Hierarchy,
    number: i64,
) -> Value {
    let application_id = &hierarchy.application_id;
    let environment_id = &hierarchy.environment_id;
    let reply = emulator
        .send(
            "GET",
            &format!(
                "/applications/{application_id}/environments/{environment_id}\
                 /deployments/{number}"
            ),
            "application/json",
            &[],
            Bytes::new(),
        )
        .await;
    assert_eq!(
        reply.status,
        200,
        "case {case:?}: GetDeployment({number}) must answer 200, got {} body {}",
        reply.status,
        reply.text()
    );
    reply.json()
}

/// The `PercentageComplete` a `Deployment` document reports.
fn percentage_complete(case: &str, document: &Value) -> f64 {
    document["PercentageComplete"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a Deployment document reports a PercentageComplete, got {document}")
    })
}

/// The `State` a `Deployment` document reports.
fn deployment_state(case: &str, document: &Value) -> String {
    document["State"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a Deployment document reports a State, got {document}")
        })
        .to_string()
}

/// Create the deployment strategy a row's rollout is paced by: a declared rollout window to
/// grow over, the row's growth type and factor, and no final bake time, so nothing but the
/// growth schedule stands between the start and the completion.
async fn paced_strategy(emulator: &Emulator, row: &Row) -> String {
    let Row {
        case,
        growth_type,
        growth_factor,
        ..
    } = row;
    let created = emulator
        .post_json(
            "/deploymentstrategies",
            &format!(
                r#"{{"Name":"paced-{growth_type}-{growth_factor}",
                      "DeploymentDurationInMinutes":4,
                      "GrowthFactor":{growth_factor},"GrowthType":"{growth_type}",
                      "FinalBakeTimeInMinutes":0,"ReplicateTo":"NONE"}}"#
            ),
        )
        .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDeploymentStrategy must answer 201, got {} body {}",
        created.status,
        created.text()
    );
    created.string("Id")
}
