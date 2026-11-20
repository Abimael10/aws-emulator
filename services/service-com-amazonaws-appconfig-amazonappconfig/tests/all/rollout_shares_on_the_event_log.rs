// SPDX-License-Identifier: CC0-1.0

//! Every share of its targets a rollout REACHES is on the deployment's own event log, and it
//! is still there once the deployment is over.
//!
//! RULE: `EventLog` is the deployment's log of its lifecycle transitions, and
//! `DeploymentEvent.Description` is documented as including "The percentage of hosts that
//! received the deployment"
//! (`smithy_query(query="shape",shape="DeploymentEvent,DeploymentEventType")`), so each time
//! a rollout's growth interval makes the configuration available to a further share of the
//! targets AppConfig appends one `PERCENTAGE_UPDATED` event, `TriggeredBy` `APPCONFIG`,
//! whose `Description` is `Configuration available to <pct>% of clients` — the one variable
//! part being the share the rollout itself reached, spelled with two decimals. Ordered by
//! `OccurredAt` those events fall after the `DEPLOYMENT_STARTED` the start logged and at or
//! before the `BAKE_TIME_STARTED` a declared bake opens with, because the bake begins only
//! once the rollout has finished; and because the log is a history rather than a status,
//! every one of them is still on it when the deployment is read back `COMPLETE`.
//!
//! The rows span what the rule generalises over: how many growth intervals the rollout takes
//! to reach every target and therefore how many of these events it logs, the share each one
//! names (whole and fractional), both `GrowthType` values — `LINEAR` adds `GrowthFactor`
//! percent per interval and `EXPONENTIAL` reaches `GrowthFactor * 2^N` at interval `N`,
//! capped at every target, which is the formula the model's `GrowthType` documentation gives
//! — whether the strategy declares a rollout window at all, and whether a bake follows the
//! rollout or the deployment completes straight out of it.
//!
//! Every wait here is on the CONDITION a transition makes true, re-read until it holds; the
//! end of a declared bake window is reached by moving the instant the deployment started
//! BACK — driving the clock the completion is owed by — so nothing waits the declared
//! minutes out and no assertion is about how long anything took.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state::{
    self, ComAmazonawsAppconfigAmazonappconfigState,
};

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state, router_on,
};
use crate::deployment_phases_follow_the_strategy::{available_to, BY_APPCONFIG};
use crate::deployment_wire_fidelity::{
    create_strategy, event_log, host_version, iso_instant, member, Row,
};
use crate::stop_deployment::start;

/// The event the start of every deployment logs, the one a declared bake opens with, and the
/// one a completion writes: the three the shares are ordered against.
const DEPLOYMENT_STARTED: &str = "DEPLOYMENT_STARTED";
/// See [`DEPLOYMENT_STARTED`].
const BAKE_TIME_STARTED: &str = "BAKE_TIME_STARTED";
/// See [`DEPLOYMENT_STARTED`].
const DEPLOYMENT_COMPLETED: &str = "DEPLOYMENT_COMPLETED";
/// The event every share the rollout reached is logged as.
const PERCENTAGE_UPDATED: &str = "PERCENTAGE_UPDATED";

/// One rollout: the strategy that paces it, and the cumulative share of the targets each of
/// its growth intervals reaches.
///
/// The shares are the model's own formula applied to the row's criteria — `LINEAR` adds
/// `GrowthFactor` percent per interval, `EXPONENTIAL` reaches `GrowthFactor * 2^N` at
/// interval `N`, both capped at every target
/// (`smithy_query(query="doc",shape="DeploymentStrategy",member="GrowthType")`) — written out
/// so a row states what its log must name rather than recomputing it from the code under
/// test.
struct Rollout {
    row: Row,
    shares: &'static [f64],
}

/// Rollouts that a declared bake time follows: one interval, two, and a fractional share
/// that takes four, under both growth types.
const BAKED_ROLLOUTS: [Rollout; 4] = [
    Rollout {
        row: (
            "every target in one interval, then the shortest declared bake",
            0,
            100.0,
            "LINEAR",
            1,
        ),
        shares: &[100.0],
    },
    Rollout {
        row: ("half the fleet per interval", 0, 50.0, "LINEAR", 1),
        shares: &[50.0, 100.0],
    },
    Rollout {
        row: (
            "exponential doubling from a quarter of the fleet",
            0,
            25.0,
            "EXPONENTIAL",
            2,
        ),
        shares: &[25.0, 50.0, 100.0],
    },
    Rollout {
        row: (
            "a fractional share per interval, over a declared window",
            5,
            33.3,
            "LINEAR",
            1,
        ),
        shares: &[33.3, 66.6, 99.9, 100.0],
    },
];

/// Rollouts no bake follows, which complete straight out of the rollout: the shares are
/// logged there too, with nothing but the completion after them.
const UNBAKED_ROLLOUTS: [Rollout; 2] = [
    Rollout {
        row: ("every target in one interval, no bake", 1, 100.0, "LINEAR", 0),
        shares: &[100.0],
    },
    Rollout {
        row: ("half the fleet per interval, no bake", 5, 50.0, "LINEAR", 0),
        shares: &[50.0, 100.0],
    },
];

/// How often a share is re-read while waiting for the rollout to log the next one.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A generous ceiling on waiting for a rollout to log every share it reaches — not a
/// statement about how long an interval takes: a rollout that has not logged them by here is
/// one nothing is moving.
const EVERY_SHARE_BY: Duration = Duration::from_secs(120);

/// One entry of an event log, as this suite reads them.
#[derive(Debug, Clone)]
struct Entry {
    event_type: String,
    triggered_by: String,
    description: String,
    /// `OccurredAt` as epoch seconds, which is what the ordering is asserted on.
    occurred_at: f64,
}

/// Every entry of a document's event log, OLDEST first — the order the rule states the
/// ordering in, which is the reverse of the newest-first order the document reports.
fn entries(case: &str, document: &str, body: &Value) -> Vec<Entry> {
    let mut log: Vec<Entry> = event_log(case, document, body)
        .iter()
        .map(|event| Entry {
            event_type: member(event, "EventType")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            triggered_by: member(event, "TriggeredBy")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            description: member(event, "Description")
                .as_str()
                .unwrap_or_default()
                .to_string(),
            occurred_at: iso_instant(case, document, "OccurredAt", &member(event, "OccurredAt"))
                .as_secs_f64(),
        })
        .collect();
    log.reverse();
    log
}

/// The entries of one event type, oldest first.
fn of_type<'log>(log: &'log [Entry], event_type: &str) -> Vec<&'log Entry> {
    log.iter()
        .filter(|entry| entry.event_type == event_type)
        .collect()
}

/// The single entry of one event type, which this suite's rows all log exactly once.
fn only<'log>(case: &str, log: &'log [Entry], event_type: &str) -> &'log Entry {
    let found = of_type(log, event_type);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {event_type} on the log, got {log:?}"
    );
    found[0]
}

/// Everything the rule requires of the shares on one document's log: one
/// APPCONFIG-triggered `PERCENTAGE_UPDATED` naming each share the rollout reached, in the
/// order it reached them, after the start and never going backwards in time.
fn assert_shares(case: &str, document: &str, body: &Value, shares: &[f64]) -> Vec<Entry> {
    let log = entries(case, document, body);
    let started = only(case, &log, DEPLOYMENT_STARTED);
    let reached = of_type(&log, PERCENTAGE_UPDATED);

    assert_eq!(
        reached
            .iter()
            .map(|entry| entry.description.clone())
            .collect::<Vec<String>>(),
        shares
            .iter()
            .map(|share| available_to(*share))
            .collect::<Vec<String>>(),
        "case {case:?}: {document} must log one event per share the rollout reached, naming \
         that share, got {log:?}"
    );

    let mut previous = started.occurred_at;
    for entry in &reached {
        assert_eq!(
            entry.triggered_by, BY_APPCONFIG,
            "case {case:?}: {document} reports a share AppConfig reached on its own, got \
             {entry:?}"
        );
        assert!(
            entry.occurred_at >= previous,
            "case {case:?}: {document}'s shares follow the deployment's start in order, got \
             {entry:?} after {previous} in {log:?}"
        );
        previous = entry.occurred_at;
    }
    reached.into_iter().cloned().collect()
}

/// Poll `GetDeployment` until the deployment's log carries one event per share the rollout
/// reaches, and answer the document that reported it.
///
/// A growth interval is owed by the CLOCK, so this waits for the log to hold the shares
/// rather than for a span: each read reports what the clock has already made true.
async fn deployment_with_every_share(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i32,
    shares: &[f64],
) -> Value {
    let deadline = std::time::Instant::now() + EVERY_SHARE_BY;
    loop {
        let document = deployment_in_state(
            router,
            case,
            application,
            environment,
            i64::from(number),
            &["DEPLOYING", "BAKING", "COMPLETE"],
        )
        .await;
        let log = entries(case, "GetDeployment", &document);
        if of_type(&log, PERCENTAGE_UPDATED).len() >= shares.len() {
            return document;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the rollout did not log its {} share(s) within {}s, got {log:?}",
            shares.len(),
            EVERY_SHARE_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `DeploymentNumber` a start minted, asserting the start itself answered 201.
async fn started_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    strategy: &str,
    profile: &str,
) -> i32 {
    let answer = start(router, application, environment, strategy, profile).await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: StartDeployment must answer 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let body = answer.json();
    let number = member(&body, "DeploymentNumber").as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: StartDeployment answered no DeploymentNumber, got {body}")
    });
    i32::try_from(number).unwrap_or_else(|_| {
        panic!("case {case:?}: a minted DeploymentNumber is an Integer, got {number}")
    })
}

/// An application with one hosted configuration version to deploy, answering
/// `(application_id, profile_id)`.
async fn given_a_configuration(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let application = create_application(router, name).await;
    let profile = create_profile(router, &application, &format!("{name}-profile")).await;
    host_version(router, &application, &profile).await;
    (application, profile)
}

/// Bring the end of a deployment's declared bake window `by` closer, by moving the instant it
/// started back.
///
/// The declared minutes are the window a caller asked AppConfig to monitor the deployment
/// for, so a row about what is on the log AFTER that window drives the clock the completion
/// is owed by instead of waiting the minutes out. Nothing here moves a state, writes an event
/// or touches any other record: the transitions themselves stay the service's.
fn bring_the_completion_forward(
    shared: &Arc<engine::InMemoryEngine>,
    case: &str,
    application_id: &str,
    environment_id: &str,
    number: i32,
    by: Duration,
) {
    let held = shared.state::<ComAmazonawsAppconfigAmazonappconfigState>();
    let mut store = held.lock();
    let deployment = store
        .application_mut(application_id)
        .and_then(|application| application.environments.get_mut(environment_id))
        .and_then(|environment| environment.deployments.get_mut(&number))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the environment must hold the deployment {number} that was \
                 started into it"
            )
        });
    deployment.started_at -= by;
}

/// RULE: a rollout a declared bake follows logs one `PERCENTAGE_UPDATED` per share it
/// reached, all of them at or before the bake's own event, and they are all still on the log
/// the completed deployment answers.
#[tokio::test]
async fn the_shares_a_rollout_reached_precede_the_bake_and_survive_the_completion() {
    // The engine behind the router, because reaching the end of a declared bake window means
    // driving the clock that window is measured on rather than waiting it out.
    let shared: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&shared);
    let (application, profile) = given_a_configuration(&router, "rollout-log").await;

    for (index, rollout) in BAKED_ROLLOUTS.iter().enumerate() {
        let (case, _duration, _growth_factor, _growth_type, bake) = rollout.row;
        let strategy = create_strategy(&router, index, &rollout.row).await;
        // One environment per row: an environment holds one deployment at a time.
        let environment =
            create_environment(&router, &application, &format!("rollout-log-{index}")).await;
        let number = started_deployment(
            &router,
            case,
            &application,
            &environment,
            &strategy,
            &profile,
        )
        .await;

        // The bake begins once the rollout has finished, so a deployment reported BAKING has
        // logged every share it was going to reach.
        let baking = deployment_in_state(
            &router,
            case,
            &application,
            &environment,
            i64::from(number),
            &["BAKING"],
        )
        .await;
        let shares = assert_shares(case, "GetDeployment", &baking, rollout.shares);
        let log = entries(case, "GetDeployment", &baking);
        let bake_started = only(case, &log, BAKE_TIME_STARTED);
        for share in &shares {
            assert!(
                share.occurred_at <= bake_started.occurred_at,
                "case {case:?}: the bake opens only once the rollout has finished, so every \
                 share is at or before it, got {share:?} against {bake_started:?}"
            );
        }
        assert_eq!(
            member(&baking, "PercentageComplete").as_f64(),
            Some(100.0),
            "case {case:?}: the share the newest event named is the share the deployment \
             reports, got {baking}"
        );

        // Once the declared window has elapsed the deployment completes on its own, and the
        // history it answers still holds every share it passed through.
        bring_the_completion_forward(
            &shared,
            case,
            &application,
            &environment,
            number,
            state::bake_duration(bake),
        );
        let completed = deployment_in_state(
            &router,
            case,
            &application,
            &environment,
            i64::from(number),
            &["COMPLETE"],
        )
        .await;
        let after = assert_shares(case, "GetDeployment", &completed, rollout.shares);
        assert_eq!(
            after
                .iter()
                .map(|entry| entry.occurred_at)
                .collect::<Vec<f64>>(),
            shares
                .iter()
                .map(|entry| entry.occurred_at)
                .collect::<Vec<f64>>(),
            "case {case:?}: a completion rewrites no history, so the shares keep the instants \
             they were reached at, got {after:?} against {shares:?}"
        );
        let final_log = entries(case, "GetDeployment", &completed);
        let completion = only(case, &final_log, DEPLOYMENT_COMPLETED);
        let bake_started = only(case, &final_log, BAKE_TIME_STARTED);
        assert!(
            completion.occurred_at >= bake_started.occurred_at,
            "case {case:?}: the completion follows the bake it waited out, got {completion:?} \
             against {bake_started:?}"
        );
        assert_eq!(
            final_log.last().map(|entry| entry.event_type.clone()),
            Some(DEPLOYMENT_COMPLETED.to_string()),
            "case {case:?}: the newest event of a completed deployment is its completion, got \
             {final_log:?}"
        );
    }
}

/// RULE without a bake: the shares are logged by the rollout itself, so a deployment that
/// completes straight out of its rollout carries them too — with nothing between the last of
/// them and the completion.
#[tokio::test]
async fn a_rollout_with_no_bake_logs_its_shares_before_completing() {
    let router = router_on(&Arc::new(engine::InMemoryEngine::new()));
    let (application, profile) = given_a_configuration(&router, "rollout-log-unbaked").await;

    for (index, rollout) in UNBAKED_ROLLOUTS.iter().enumerate() {
        let (case, ..) = rollout.row;
        let strategy = create_strategy(&router, index, &rollout.row).await;
        let environment =
            create_environment(&router, &application, &format!("unbaked-log-{index}")).await;
        let number = started_deployment(
            &router,
            case,
            &application,
            &environment,
            &strategy,
            &profile,
        )
        .await;

        // Every share is logged while the rollout runs, before the completion it leads to.
        let rolling = deployment_with_every_share(
            &router,
            case,
            &application,
            &environment,
            number,
            rollout.shares,
        )
        .await;
        assert_shares(case, "GetDeployment", &rolling, rollout.shares);

        let completed = deployment_in_state(
            &router,
            case,
            &application,
            &environment,
            i64::from(number),
            &["COMPLETE"],
        )
        .await;
        let shares = assert_shares(case, "GetDeployment", &completed, rollout.shares);
        let log = entries(case, "GetDeployment", &completed);
        assert!(
            of_type(&log, BAKE_TIME_STARTED).is_empty(),
            "case {case:?}: a strategy with no declared bake opens no bake window, got {log:?}"
        );
        let completion = only(case, &log, DEPLOYMENT_COMPLETED);
        let last_share = shares
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: the rollout logged no share at all"));
        assert!(
            completion.occurred_at >= last_share.occurred_at,
            "case {case:?}: the completion follows the last share the rollout reached, got \
             {completion:?} against {last_share:?}"
        );
        assert_eq!(
            log.last().map(|entry| entry.event_type.clone()),
            Some(DEPLOYMENT_COMPLETED.to_string()),
            "case {case:?}: the newest event is the completion, got {log:?}"
        );
    }
}
