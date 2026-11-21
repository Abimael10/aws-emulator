// SPDX-License-Identifier: CC0-1.0

//! Every action-point invocation AppConfig makes is ONE delivery to the target the action
//! names, and a consumer that takes those deliveries off the target queue is handed each of
//! them exactly once.
//!
//! The rule under test: an extension action bound to an Amazon SQS queue is invoked once per
//! occurrence of the action point it is bound at, each invocation carrying the
//! system-generated `InvocationId` the deployment's own event log records for it
//! (`ActionInvocations` is "the list of extensions that were invoked as part of the
//! deployment", `smithy_query(query="shape",shape="DeploymentEvent")`), and it enqueues one
//! message for that invocation and no other. So the invocations a deployment RECORDS and the
//! events its target RECEIVES are the same set, one message each — whether the consumer
//! reads them in one poll or in several, because a poll of Amazon SQS answers at most ten
//! messages and leaves the ones it answered on the queue until they are deleted.
//!
//! The rows differ along how many invocations one observation has to account for: a single
//! one, a handful, and more than one `ReceiveMessage` can hand over — and along how many
//! action points the extension binds, because an invocation is owed per action point per
//! occurrence. Nothing here waits out a span: every row's deployments reach every target
//! inside the call that asks for them, and the suite's own waiting is a poll loop that ends
//! as soon as the deliveries the deployments recorded have arrived.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{create_application, create_environment, create_profile};
use crate::extension_action_points::{
    associate, create_version, drain, emulator, get_deployment, given_extension, given_queue,
    given_strategy, start_deployment, RECEIVE_PAGE,
};
use crate::resource_tags::arn;

/// How long a row waits for the deliveries its deployments recorded.
///
/// A generous ceiling on the WAIT, not a statement about how long a delivery takes: a row
/// that is still short of them by here is one whose invocations are not reaching their
/// target, and nothing short of it is decided by how fast the host runs.
const DELIVERED_BY: Duration = Duration::from_secs(90);

/// How often the deployments and their target queue are read while waiting for that.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One row: which action points the extension binds, how many deployments it is invoked
/// for, and what that makes of the observation.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// `ActionPoint` values the extension binds an action at
    /// (`crate::state::extension_actions`' own constants, the model's `ActionPoint` enum).
    action_points: &'static [&'static str],
    /// The `Type` each delivered event may carry: the action point in PascalCase, which is
    /// the form the event names the point in.
    expected_types: &'static [&'static str],
    /// How many deployments of the environment the extension is bound to this row runs.
    deployments: usize,
    /// Whether this row's deliveries outnumber what one poll of the queue hands over.
    more_than_one_poll: bool,
}

/// The rows: one invocation, a handful, and more than one poll's worth of them.
const ROWS: &[Row] = &[
    Row {
        case: "one action point of one deployment",
        action_points: &["ON_DEPLOYMENT_COMPLETE"],
        expected_types: &["OnDeploymentComplete"],
        deployments: 1,
        more_than_one_poll: false,
    },
    Row {
        case: "two action points of three deployments",
        action_points: &["ON_DEPLOYMENT_START", "ON_DEPLOYMENT_COMPLETE"],
        expected_types: &["OnDeploymentStart", "OnDeploymentComplete"],
        deployments: 3,
        more_than_one_poll: false,
    },
    Row {
        case: "two action points of six deployments, more than one poll of deliveries",
        action_points: &["ON_DEPLOYMENT_START", "ON_DEPLOYMENT_COMPLETE"],
        expected_types: &["OnDeploymentStart", "OnDeploymentComplete"],
        deployments: 6,
        more_than_one_poll: true,
    },
];

/// The `InvocationId` of every action invocation the row's deployments record on their own
/// event logs.
///
/// Reading a deployment is also what gives the action points the clock has made due
/// somewhere to run, the way a client watching its own rollout does, so this is both the
/// expectation and the drive.
async fn recorded_invocations(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    deployments: usize,
) -> BTreeSet<String> {
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    for number in 1..=deployments {
        let number = i32::try_from(number).expect("a deployment number of this suite's size");
        let read = get_deployment(router, application, environment, number).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment({number}) answered {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let document = read.json();
        for event in document["EventLog"].as_array().cloned().unwrap_or_default() {
            for invocation in event["ActionInvocations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                let id = invocation["InvocationId"].as_str().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a recorded action invocation names the invocation it \
                         was, got {invocation}"
                    )
                });
                recorded.insert(id.to_string());
            }
        }
    }
    recorded
}

/// The `InvocationId` each event delivered to `queue_url` belongs to, in delivery order and
/// WITH any repeat, appended to `delivered`; the action point each one names is collected
/// into `types`, and the row's own `Type` check is made here, where the event is in hand.
async fn collect_deliveries(
    router: &MultiServiceRouter,
    row: &Row,
    queue_url: &str,
    delivered: &mut Vec<String>,
    types: &mut BTreeSet<String>,
) {
    let case = row.case;
    for event in drain(router, queue_url).await {
        let event_type = event["Type"].as_str().unwrap_or_default().to_string();
        assert!(
            row.expected_types.contains(&event_type.as_str()),
            "case {case:?}: an action point hands its target an event naming the point it \
             fired at, expected one of {:?}, got {event_type:?} in {event}",
            row.expected_types
        );
        types.insert(event_type);
        let id = event["InvocationId"].as_str().unwrap_or_else(|| {
            panic!(
                "case {case:?}: an action-point event names the invocation it belongs to, got \
                 {event}"
            )
        });
        delivered.push(id.to_string());
    }
}

/// RULE: the action invocations a deployment records and the events its target is delivered
/// are the same set, one delivery each — read in as many polls as the queue needs.
#[tokio::test]
async fn every_recorded_action_invocation_is_delivered_to_its_target_once() {
    let router = emulator();
    // A strategy that paces no stage: every row's deployment reaches every target inside the
    // call that asked for it, so no row waits out a rollout to be invoked for one.
    let instant = given_strategy(&router, "delivery-instant", 0, 0).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = create_application(&router, &format!("delivery-app-{suffix}")).await;
        let environment =
            create_environment(&router, &application, &format!("delivery-env-{suffix}")).await;
        let profile =
            create_profile(&router, &application, &format!("delivery-profile-{suffix}")).await;
        let (queue_url, queue_arn) =
            given_queue(&router, &format!("delivery-observations-{suffix}")).await;
        let (extension, _) = given_extension(
            &router,
            &format!("delivery-ext-{suffix}"),
            row.action_points,
            &queue_arn,
            &[],
        )
        .await;
        let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
        associate(&router, case, &extension, &environment_arn, &Value::Null).await;

        // One hosted version per deployment, each deployed in turn: a deployment rolls out a
        // version of its own, and the row's count is how many occurrences of its action
        // points the environment goes through.
        for number in 1..=row.deployments {
            let written = create_version(
                &router,
                &application,
                &profile,
                "application/json",
                format!(r#"{{"release":{number}}}"#).as_bytes(),
            )
            .await;
            assert_eq!(
                written.status,
                201,
                "case {case:?}: CreateHostedConfigurationVersion answered {} / {}",
                written.status,
                String::from_utf8_lossy(&written.body)
            );
            let started = start_deployment(
                &router,
                &application,
                &environment,
                &json!({
                    "DeploymentStrategyId": instant,
                    "ConfigurationProfileId": profile,
                    "ConfigurationVersion": number.to_string(),
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
                json!("COMPLETE"),
                "case {case:?}: a deployment whose strategy paces no stage is complete when \
                 StartDeployment answers, got {}",
                started.json()
            );
        }

        // The wait is on the CONDITION: look again until every invocation the deployments
        // recorded has been delivered, which is also how a repeat delivery shows up — it is
        // counted in `delivered` and in nothing else.
        let deadline = Instant::now() + DELIVERED_BY;
        let mut delivered: Vec<String> = Vec::new();
        let mut types: BTreeSet<String> = BTreeSet::new();
        let recorded = loop {
            let recorded =
                recorded_invocations(&router, case, &application, &environment, row.deployments)
                    .await;
            collect_deliveries(&router, row, &queue_url, &mut delivered, &mut types).await;
            let observed: BTreeSet<String> = delivered.iter().cloned().collect();
            if !recorded.is_empty() && observed == recorded {
                break recorded;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the invocations {recorded:?} the deployments recorded \
                 to be delivered, got {delivered:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        // ONE delivery per recorded invocation: the set matched above, so a longer list of
        // deliveries than there are invocations is a delivery of the same event twice.
        assert_eq!(
            delivered.len(),
            recorded.len(),
            "case {case:?}: expected one delivery of each of the {} recorded invocations, got \
             {delivered:?}",
            recorded.len()
        );
        // Every action point the extension binds was invoked, not just one of them: the
        // points the deliveries name are exactly the ones bound.
        let expected_types: BTreeSet<String> = row
            .expected_types
            .iter()
            .map(|event_type| (*event_type).to_string())
            .collect();
        assert_eq!(
            types, expected_types,
            "case {case:?}: every action point the extension binds is invoked, expected \
             deliveries naming {expected_types:?}, got {types:?}"
        );
        // The row that needs more than one poll to be read really does: its deliveries
        // outnumber the ten messages one `ReceiveMessage` hands over.
        assert_eq!(
            delivered.len() > RECEIVE_PAGE,
            row.more_than_one_poll,
            "case {case:?}: expected {} than the {RECEIVE_PAGE} deliveries one poll hands \
             over, got {}",
            if row.more_than_one_poll { "more" } else { "no more" },
            delivered.len()
        );
        // And nothing is handed over a second time: a consumer that has taken the events off
        // the queue, with no further occurrence of any bound point since, is delivered none.
        let again = drain(&router, &queue_url).await;
        assert!(
            again.is_empty(),
            "case {case:?}: an event a consumer has already been handed is not delivered \
             again, got {again:?}"
        );
    }
}
