// SPDX-License-Identifier: CC0-1.0

//! An `AT_DEPLOYMENT_TICK` event states the rollout position as of the INVOCATION it is
//! handed to.
//!
//! The rule under test: the event an action point hands a target describes the deployment as
//! of the instant that invocation is made — for a tick, the share of targets the
//! configuration is available to then ("the percentage of targets for which the deployment
//! is available", `smithy_query(query="shape",shape="Deployment")` for
//! `PercentageComplete`), which is the position the target rules on when it answers a
//! directive. The actions bound at one point are invoked one after another, and an action
//! can take longer to answer than the rollout takes to grow, so what each invocation is
//! told is the position the clock owes the deployment when that invocation is made — never
//! the one the deployment's record held when the dispatch began.
//!
//! Each row binds TWO actions at the point: one on a Lambda function that HOLDS its
//! invocation for longer than a growth interval (a function is the only target kind that
//! runs code, so the only one that can hold a dispatch), and one on an Amazon SQS queue,
//! where the event is observable as a client sees it. Every tick of the rollout is therefore
//! dispatched across at least one growth interval, and the positions the queue reports are
//! the shares the rollout grew into rather than the share it started from. The rows differ
//! along how the rollout is paced — its `GrowthType` and its `GrowthFactor` — because the
//! shares a tick can report are the ones that strategy's own growth reaches.
//!
//! Nothing here waits out a span: the hold is the TARGET's own work, the slow action the
//! dispatch is waiting on, and the suite's own waiting is a poll loop on the queue that
//! ends as soon as the invocations it is looking for have arrived.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_appconfig_amazonappconfig::state::STAGE_DURATION;
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{create_application, create_environment, create_profile};
use crate::deployment_tick_directive::{emulator, given_function};
use crate::extension_action_points::{
    associate, create_version, drain, get_deployment, given_extension,
    given_growing_strategy_of_type, given_queue, start_deployment, stop_deployment,
};
use crate::resource_tags::arn;

/// How long the holding action keeps each tick dispatch: longer than ONE growth interval of
/// a paced rollout (`state::STAGE_DURATION`, the span this emulator paces an owed
/// transition by), so the clock owes the rollout a further share of its targets by the time
/// the dispatch reaches the action invoked after it.
///
/// A span of the TARGET's, not a wait of this suite's: it is how long the function this row
/// bound at the action point takes to answer.
const HOLD_IN_SECONDS: u64 = STAGE_DURATION.as_secs() + 1;

/// How many tick invocations of the observation action one row reads before it rules.
///
/// Two, because the actions bound at one point are invoked in an order the service picks:
/// whichever of the two the observation action is, the SECOND tick of the rollout is
/// dispatched after a hold that outlasts a growth interval.
const TICKS_OBSERVED: usize = 2;

/// How long a row waits for those invocations.
///
/// A generous ceiling on the WAIT, not a statement about how often a tick falls due: a row
/// that has seen fewer by here is one whose action point is not being invoked.
const TICKS_BY: Duration = Duration::from_secs(180);

/// How often the observation queue is read while waiting for them.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `CreateDeploymentStrategy$DeploymentDurationInMinutes` for every row's strategy
/// (`MinutesBetween0And24Hours`, range 0..1440): a rollout window, so the deployment is
/// paced through its growth intervals rather than reaching every target inside the call that
/// started it.
const ROLLOUT_IN_MINUTES: i32 = 1;

/// `CreateDeploymentStrategy$FinalBakeTimeInMinutes`, the same range: a window AppConfig
/// monitors the deployment over after the rollout, so the deployment stays in flight — and
/// therefore ticking — for far longer than the ticks a row observes take, whatever the pace
/// of the host. No row ever waits it out.
const BAKE_IN_MINUTES: i32 = 1;

/// The handler of the holding action: it answers nothing that steers the rollout, and takes
/// [`HOLD_IN_SECONDS`] to answer it.
fn holding_source() -> String {
    format!(
        "import time\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   time.sleep({HOLD_IN_SECONDS})\n\
         \x20   return {{}}\n"
    )
}

/// One row: how the rollout under tick is paced, and the shares its growth reaches.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// A `GrowthType` the model declares
    /// (`smithy_query(query="shape",shape="GrowthType")`: `EXPONENTIAL | LINEAR`).
    growth_type: &'static str,
    /// `CreateDeploymentStrategy$GrowthFactor` (`Percentage`, range 1..100).
    growth_factor: f64,
    /// The cumulative shares of its targets this strategy's growth reaches, in order: for
    /// `LINEAR` "AppConfig deploys the configuration to the number of targets specified by
    /// the growth factor per interval", and for `EXPONENTIAL` the share at interval `N` is
    /// `GrowthFactor * 2^N`, each capped at every target
    /// (`smithy_query(query="shape",shape="GrowthType")`).
    shares: &'static [f64],
}

/// The rows: a rollout that takes several intervals to reach every target, one that takes
/// two, and one whose share doubles each interval.
const ROWS: &[Row] = &[
    Row {
        case: "a rollout growing by a quarter of its targets per interval",
        growth_type: "LINEAR",
        growth_factor: 25.0,
        shares: &[25.0, 50.0, 75.0, 100.0],
    },
    Row {
        case: "a rollout growing by half of its targets per interval",
        growth_type: "LINEAR",
        growth_factor: 50.0,
        shares: &[50.0, 100.0],
    },
    Row {
        case: "a rollout doubling the share it grows by each interval",
        growth_type: "EXPONENTIAL",
        growth_factor: 25.0,
        shares: &[25.0, 50.0, 100.0],
    },
];

/// The positions the ticks delivered to `queue_url` report, oldest first, once
/// [`TICKS_OBSERVED`] of them have arrived.
///
/// The DEPLOYMENT is deliberately not read while this waits: reading it is itself what
/// brings it forward, so a suite that polled it would be materialising the very positions it
/// is checking the ticks against. Only the queue is read, which is all a client watching its
/// extension's target does.
async fn observed_positions(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
) -> Vec<f64> {
    let deadline = Instant::now() + TICKS_BY;
    let mut positions: Vec<f64> = Vec::new();
    loop {
        for event in drain(router, queue_url).await {
            if event["Type"] != json!("AtDeploymentTick") {
                continue;
            }
            let position = event["PercentageComplete"].as_f64().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a tick must report the rollout position it is asking \
                     about, got {event}"
                )
            });
            positions.push(position);
        }
        if positions.len() >= TICKS_OBSERVED {
            return positions;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {TICKS_OBSERVED} tick invocations of the observation \
             action, got {positions:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the position a tick states is the one the rollout stands at when that invocation is
/// made, so the ticks of a dispatch held past a growth interval report the shares the
/// rollout grew into — and never a share it has not reached.
#[tokio::test]
async fn a_tick_states_the_rollout_position_as_of_the_invocation_it_is_handed_to() {
    let (router, engine) = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = create_application(&router, &format!("tick-position-app-{suffix}")).await;
        let environment =
            create_environment(&router, &application, &format!("tick-position-env-{suffix}")).await;
        let profile =
            create_profile(&router, &application, &format!("tick-position-profile-{suffix}")).await;
        let written = create_version(
            &router,
            &application,
            &profile,
            "application/json",
            format!(r#"{{"release":"under tick","row":{suffix}}}"#).as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: CreateHostedConfigurationVersion answered {} / {}",
            written.status,
            String::from_utf8_lossy(&written.body)
        );

        // The holding action and, beside it, the observation action on a queue: a queue
        // cannot answer, so it reports the event it was handed without steering anything.
        let holding = given_function(
            &router,
            &engine,
            case,
            &format!("tick-hold-{suffix}"),
            &holding_source(),
        )
        .await;
        let (queue_url, queue_arn) = given_queue(&router, &format!("tick-positions-{suffix}")).await;
        let (holder, _) = given_extension(
            &router,
            &format!("tick-hold-ext-{suffix}"),
            &["AT_DEPLOYMENT_TICK"],
            &holding,
            &[],
        )
        .await;
        let (watcher, _) = given_extension(
            &router,
            &format!("tick-watch-ext-{suffix}"),
            &["AT_DEPLOYMENT_TICK"],
            &queue_arn,
            &[],
        )
        .await;
        let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
        for extension in [&holder, &watcher] {
            associate(&router, case, extension, &environment_arn, &Value::Null).await;
        }

        let strategy = given_growing_strategy_of_type(
            &router,
            &format!("tick-position-paced-{suffix}"),
            row.growth_type,
            ROLLOUT_IN_MINUTES,
            BAKE_IN_MINUTES,
            row.growth_factor,
        )
        .await;
        let started = start_deployment(
            &router,
            &application,
            &environment,
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
            "case {case:?}: StartDeployment answered {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        assert_eq!(
            started.json()["State"],
            json!("DEPLOYING"),
            "case {case:?}: a paced deployment is still rolling out when StartDeployment \
             answers, got {}",
            started.json()
        );

        let positions = observed_positions(&router, case, &queue_url).await;

        // Every position is a share this strategy's growth reaches — or the share the
        // rollout starts from, which the first tick of a deployment nothing has grown yet
        // reports — and a rollout only ever grows, so consecutive ticks never go backwards.
        let mut reached = 0.0_f64;
        for position in &positions {
            assert!(
                (*position == 0.0 || row.shares.contains(position)) && *position >= reached,
                "case {case:?}: expected a rollout position among {:?} that does not go back \
                 from {reached}, got {position} in {positions:?}",
                row.shares
            );
            reached = *position;
        }
        // What the hold turns on: every dispatch spends longer than a growth interval in the
        // action invoked before the observed one, so by the last tick observed the clock owes
        // the rollout a share beyond the one it started from. A tick still reporting that
        // start share is stating a position the clock has already moved past.
        assert!(
            reached >= row.shares[0],
            "case {case:?}: a tick invoked after a growth interval had elapsed reports the \
             share the rollout grew into, expected at least {}, got {reached} in {positions:?}",
            row.shares[0]
        );
        // And never a share the rollout has not reached: a read taken after those ticks
        // reports at least the position the last of them stated. The read itself drives
        // whatever the dispatch owes, so it can spend several of the holding action's holds
        // before it answers — a row that takes tens of seconds here is waiting on that
        // action, not hanging.
        let read = get_deployment(&router, &application, &environment, 1).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment answered {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let document = read.json();
        let reported = document["PercentageComplete"].as_f64().unwrap_or_default();
        assert!(
            reported >= reached,
            "case {case:?}: a tick cannot report a share the rollout has not reached, the \
             last reported {reached} where the read after it reported {reported} for a \
             deployment in state {}",
            document["State"]
        );

        // The row's rollout is stopped before the next one starts: a deployment the service
        // is still working on goes on being ticked, and the ticks of a rollout nothing is
        // observing any more would pace the next row's dispatches rather than its own.
        let stopped = stop_deployment(&router, &application, &environment, 1).await;
        assert_eq!(
            stopped.status,
            202,
            "case {case:?}: StopDeployment answered {} / {}",
            stopped.status,
            String::from_utf8_lossy(&stopped.body)
        );
    }
}
