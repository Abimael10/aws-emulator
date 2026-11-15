// SPDX-License-Identifier: CC0-1.0

//! Every deployment-lifecycle action point an AWS-authored notification extension binds
//! DELIVERS, and nothing else does.
//!
//! The rule under test: an extension AppConfig itself authors publishes no `Uri` of its own
//! (`smithy_query(query="doc",shape="Action",member="Uri")` makes the member optional), so
//! each of its actions delivers to the destination the ASSOCIATION that bound the extension
//! supplies for the extension's published parameter — an association bound at ANY of the
//! three levels an association can bind (`smithy_query(query="doc",shape="Parameter")`:
//! "parameter values are specified in an extension association") — and it delivers at every
//! action point the extension binds and at no other stage of the deployment. For the
//! deployment-event notification extensions those points are the three of a deployment's own
//! lifecycle: its start, its completion and its rollback, each reported as the action point
//! in PascalCase.
//!
//! What the rows below span is therefore the two things the rule generalises over: the
//! TERMINAL stage the deployment reaches (one that completes, one that completes through an
//! intermediate growth step, one its caller rolls back), and the resource LEVEL the
//! association binds (the application, the environment, the configuration profile). The
//! stages a deployment passes through that the extension binds no action at — its growth
//! steps — are pinned by the same rows: a case asserts exactly the notifications its stages
//! owe and then that no further one arrives.
//!
//! Delivery is asserted where a client sees it, on the Amazon SQS queue the association
//! named, so the suite mounts SQS beside AppConfig on one engine
//! (`crate::extension_action_points::emulator`). Which ARN each row of the published
//! catalogue resolves its destination from is pinned over the whole catalogue by
//! `crate::state::extension_actions`' own unit table; the queue is the destination a test
//! can read the delivered event back from.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state,
};
use crate::deployment_event_notifications_to_sqs::{discover_sqs_extension, QUEUE_PARAMETER};
use crate::extension_action_points::{
    associate, create_version, drain, emulator, get_deployment, given_growing_strategy,
    given_queue, start_deployment,
};
use crate::resource_tags::arn;
use crate::stop_deployment::stop;

/// How long a case waits for the notifications the stages of its deployment owe.
///
/// A generous ceiling on the WAIT rather than a statement about how long a stage takes: a
/// deployment whose notifications have not arrived by here is one nothing is delivering.
const DELIVERED_BY: Duration = Duration::from_secs(60);

/// How often a case re-reads the deployment while waiting for that — which is also what
/// gives the action points the clock has made due somewhere to run, the way a client
/// watching its own rollout does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many further reads of a settled deployment a case looks for an extra notification
/// over, having collected the ones its stages owe.
///
/// Every stage of a deployment that has reached a terminal state has already happened, so
/// this is a bound on the LOOKING rather than a wait: each round re-reads the deployment,
/// which is what would dispatch an action point that was still owed, and drains the queue.
const SILENT_ROUNDS: usize = 5;

/// The configuration version every case deploys, and the bytes it holds.
const VERSION: &str = "1";

/// The level an association binds, as the resource path of the ARN it names.
#[derive(Debug, Clone, Copy)]
enum Level {
    /// `application/<id>`.
    Application,
    /// `application/<id>/environment/<id>`.
    Environment,
    /// `application/<id>/configurationprofile/<id>`.
    ConfigurationProfile,
}

impl Level {
    /// The ARN an association of this level names, for one application tree.
    fn resource_arn(self, application: &str, environment: &str, profile: &str) -> String {
        match self {
            Self::Application => arn(&format!("application/{application}")),
            Self::Environment => arn(&format!(
                "application/{application}/environment/{environment}"
            )),
            Self::ConfigurationProfile => arn(&format!(
                "application/{application}/configurationprofile/{profile}"
            )),
        }
    }
}

/// One case: a deployment paced one way, bound one way, and the lifecycle it reports.
struct Row {
    /// The case's name, which every assertion of it reports.
    case: &'static str,
    /// The level the association binds the notification extension at.
    level: Level,
    /// The strategy's growth factor, which is how many growth steps the rollout takes:
    /// 100 reaches every target at once, a smaller one passes THROUGH a step stage the
    /// extension binds no action at.
    growth_factor: f64,
    /// Whether the case stops its own deployment instead of letting the rollout finish.
    stopped: bool,
    /// The `DeploymentState` the deployment settles in.
    expected_state: &'static str,
    /// The `Type` of each notification the deployment's stages owe, in the order the
    /// stages happen.
    expected_types: &'static [&'static str],
}

/// The cases, spanning the terminal stage and the level the association binds.
const ROWS: &[Row] = &[
    Row {
        case: "reaches every target at once, bound to the environment",
        level: Level::Environment,
        growth_factor: 100.0,
        stopped: false,
        expected_state: "Complete",
        expected_types: &["OnDeploymentStart", "OnDeploymentComplete"],
    },
    Row {
        case: "completes through a growth step, bound to the configuration profile",
        level: Level::ConfigurationProfile,
        growth_factor: 50.0,
        stopped: false,
        expected_state: "Complete",
        expected_types: &["OnDeploymentStart", "OnDeploymentComplete"],
    },
    Row {
        case: "rolled back by its own caller, bound to the application",
        level: Level::Application,
        growth_factor: 10.0,
        stopped: true,
        expected_state: "RolledBack",
        expected_types: &["OnDeploymentStart", "OnDeploymentRolledBack"],
    },
];

/// Re-read the deployment, which is also what gives the action points the clock has made
/// due somewhere to run — through the suite's shared `GetDeployment` caller.
async fn read_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i32,
) {
    let read = get_deployment(router, application, environment, number).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment expected 200, got {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
}

/// The notifications on `queue_url`, collected by re-reading the deployment until as many
/// as the case expects have arrived.
async fn delivered(
    router: &MultiServiceRouter,
    row: &Row,
    application: &str,
    environment: &str,
    number: i32,
    queue_url: &str,
) -> Vec<Value> {
    let case = row.case;
    let expected = row.expected_types.len();
    let deadline = Instant::now() + DELIVERED_BY;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, queue_url).await);
        if collected.len() >= expected {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} notifications within {}s, got {collected:?}",
            DELIVERED_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
        read_deployment(router, case, application, environment, number).await;
    }
}

/// Assert the deployment delivers nothing more: a stage the extension binds no action at
/// produces no notification, and a stage that already delivered does not deliver twice.
async fn nothing_further(
    router: &MultiServiceRouter,
    row: &Row,
    application: &str,
    environment: &str,
    number: i32,
    queue_url: &str,
) {
    let case = row.case;
    for round in 0..SILENT_ROUNDS {
        read_deployment(router, case, application, environment, number).await;
        let extra = drain(router, queue_url).await;
        assert!(
            extra.is_empty(),
            "case {case:?}: round {round}: the deployment's remaining stages bind no action, \
             so nothing more is delivered, got {extra:?}"
        );
    }
}

/// Assert `events` are the notifications `row` expects, each naming the deployment, the
/// version it rolled out, the resources it ran on and the association's own parameters.
fn assert_notifications(
    row: &Row,
    number: i32,
    application: &str,
    environment: &str,
    profile: &str,
    queue_arn: &str,
    events: &[Value],
) {
    let case = row.case;
    let types: Vec<String> = events
        .iter()
        .map(|event| {
            event
                .get("Type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        types,
        row.expected_types
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<String>>(),
        "case {case:?}: expected the action points the extension binds, in the order the \
         stages happened, each in PascalCase, got {types:?}"
    );
    for event in events {
        let stage = event.get("Type").and_then(Value::as_str).unwrap_or_default();
        assert_eq!(
            event.get("DeploymentNumber").and_then(Value::as_i64),
            Some(i64::from(number)),
            "case {case:?} / {stage}: expected deployment number {number}, got {event}"
        );
        assert_eq!(
            event.get("ConfigurationVersion").and_then(Value::as_str),
            Some(VERSION),
            "case {case:?} / {stage}: expected configuration version {VERSION:?}, got {event}"
        );
        for (member, id) in [
            ("Application", application),
            ("Environment", environment),
            ("ConfigurationProfile", profile),
        ] {
            assert_eq!(
                event
                    .get(member)
                    .and_then(|resource| resource.get("Id"))
                    .and_then(Value::as_str),
                Some(id),
                "case {case:?} / {stage}: expected {member}.Id {id:?}, got {event}"
            );
        }
        assert_eq!(
            event
                .get("Parameters")
                .and_then(|parameters| parameters.get(QUEUE_PARAMETER))
                .and_then(Value::as_str),
            Some(queue_arn),
            "case {case:?} / {stage}: expected the association's {QUEUE_PARAMETER}, got {event}"
        );
    }
}

/// Run one row: its own application tree, its own queue and its own association, so the
/// level a row binds at reaches nothing another row deployed.
async fn run(router: &MultiServiceRouter, index: usize, row: &Row) {
    let case = row.case;
    let (queue_url, queue_arn) =
        given_queue(router, &format!("authored-notifications-{index}")).await;
    let (extension, _) = discover_sqs_extension(router).await;

    let application = create_application(router, &format!("authored-app-{index}")).await;
    let environment =
        create_environment(router, &application, &format!("authored-env-{index}")).await;
    let profile =
        create_profile(router, &application, &format!("authored-profile-{index}")).await;
    let written = create_version(
        router,
        &application,
        &profile,
        "application/json",
        format!(r#"{{"case":{index}}}"#).as_bytes(),
    )
    .await;
    assert_eq!(
        written.status,
        201,
        "case {case:?}: CreateHostedConfigurationVersion answered {} / {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
    // A rollout window, so the deployment is in flight when the start answers and the
    // stages after it are reached by the clock rather than by the call.
    let strategy = given_growing_strategy(
        router,
        &format!("authored-strategy-{index}"),
        1,
        0,
        row.growth_factor,
    )
    .await;

    let resource_arn = row.level.resource_arn(&application, &environment, &profile);
    associate(
        router,
        case,
        &extension,
        &resource_arn,
        &json!({ QUEUE_PARAMETER: queue_arn }),
    )
    .await;

    let started = start_deployment(
        router,
        &application,
        &environment,
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": VERSION,
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment expected 201, got {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let issued = started
        .json()
        .get("DeploymentNumber")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartDeployment answered no DeploymentNumber, got {}",
                started.json()
            )
        });
    let number = i32::try_from(issued).unwrap_or_else(|error| {
        panic!("case {case:?}: DeploymentNumber {issued} is not a deployment number ({error})")
    });

    if row.stopped {
        let stopped = stop(router, &application, &environment, number, None).await;
        assert_eq!(
            stopped.status,
            202,
            "case {case:?}: StopDeployment expected 202, got {} / {}",
            stopped.status,
            String::from_utf8_lossy(&stopped.body)
        );
    }
    let settled = deployment_in_state(
        router,
        case,
        &application,
        &environment,
        i64::from(number),
        &[row.expected_state],
    )
    .await;
    assert_eq!(
        settled.get("State").and_then(Value::as_str),
        Some(row.expected_state),
        "case {case:?}: expected the deployment to settle in {:?}, got {settled}",
        row.expected_state
    );

    let events = delivered(router, row, &application, &environment, number, &queue_url).await;
    assert_notifications(
        row,
        number,
        &application,
        &environment,
        &profile,
        &queue_arn,
        &events,
    );
    nothing_further(router, row, &application, &environment, number, &queue_url).await;
}

#[tokio::test]
async fn an_authored_notification_extension_delivers_at_every_point_it_binds_and_no_other() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        run(&router, index, row).await;
    }
}
