// SPDX-License-Identifier: CC0-1.0

//! An extension action point of a DEPLOYMENT's workflow tells its target WHO triggered it.
//!
//! The rule under test, which the tables below are slices of: the event an action point of
//! a deployment's workflow hands its target carries `TriggeredBy`, "the entity that
//! triggered the deployment event"
//! (`smithy_query(query="doc",shape="DeploymentEvent",member="TriggeredBy")`), as one of
//! the values the model's `TriggeredBy` enum admits — the caller for a point reached inside
//! a caller's own request, and whatever drove the stage for a point of the deployment's own
//! lifecycle, which is the attribution that stage's own event log entry carries.
//!
//! An extension's target is handed that event where a client can read it, so the suite
//! mounts Amazon SQS beside AppConfig ([`emulator`]) and asserts on the documents that
//! reach the queue an action's `Uri` names, and against the deployment's own `EventLog`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_appconfig_amazonappconfig::ops::types::TRIGGERED_BY_VALUES;
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{create_application, create_environment, create_profile};
use crate::extension_action_points::{
    associate, create_version, drain, emulator, get_deployment, given_extension, given_queue,
    given_strategy, start_deployment,
};
use crate::resource_tags::arn;
use crate::stop_deployment::stop;

/// How long a case waits for an invocation the deployment it is watching owes.
///
/// A generous ceiling on the WAIT, not a statement about any interval: a deployment that
/// has produced nothing by here is one nothing is driving.
const INVOKED_BY: Duration = Duration::from_secs(60);

/// How often the deployment under observation is re-read while waiting for that.
///
/// Reading it is also what gives the invocations the clock has made due somewhere to run,
/// the way a client watching its own rollout does.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The event of type `event_type` among `events`, as the target was handed it.
fn delivered<'event>(case: &str, events: &'event [Value], event_type: &str) -> &'event Value {
    events
        .iter()
        .find(|event| event["Type"] == json!(event_type))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {event_type} event, got {events:?}")
        })
}

/// Assert that `event` names `expected` as what triggered the point, and that the value is
/// one the model admits at all.
///
/// The catalog is the crate's own `TRIGGERED_BY_VALUES`, so a model that gains a value is
/// not pinned here twice. A rollback an extension's directive drove is attributed to the
/// EXTENSION, which that catalog does not hold (recorded behaviour, stated in
/// `crate::state`), so such a row belongs with the suites that drive a directive rather
/// than in the tables below.
fn assert_triggered_by(case: &str, event: &Value, expected: &str) {
    let actual = &event["TriggeredBy"];
    assert_eq!(
        actual,
        &json!(expected),
        "case {case:?}: expected TriggeredBy {expected:?}, got {actual:?} in {event}"
    );
    let named = actual.as_str().unwrap_or_default();
    assert!(
        TRIGGERED_BY_VALUES.contains(&named),
        "case {case:?}: expected one of the modelled TriggeredBy values \
         {TRIGGERED_BY_VALUES:?}, got {named:?}"
    );
}

/// The `TriggeredBy` the deployment's own event log reports for its `event_type` entry:
/// what the action point of that stage must agree with.
async fn logged_trigger(
    case: &str,
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    number: i32,
    event_type: &str,
) -> String {
    let read = get_deployment(router, application, environment, number).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetDeployment answered {}",
        read.status
    );
    let document = read.json();
    let entry = document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|entry| entry["EventType"] == json!(event_type))
        .unwrap_or_else(|| panic!("case {case:?}: no {event_type} entry in {document}"));
    entry["TriggeredBy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {event_type} reports no TriggeredBy: {entry}"))
        .to_string()
}

/// Re-read the deployment until an event of `event_type` has reached the queue, and answer
/// it.
///
/// The wait is on the CONDITION — reading the deployment is what gives the invocations the
/// clock has made due somewhere to run — and gives up only at a deadline far past any
/// interval this service paces.
async fn invoked_event(
    case: &str,
    router: &MultiServiceRouter,
    queue_url: &str,
    application: &str,
    environment: &str,
    number: i32,
    event_type: &str,
) -> Value {
    let deadline = Instant::now() + INVOKED_BY;
    loop {
        let read = get_deployment(router, application, environment, number).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetDeployment answered {}",
            read.status
        );
        let delivered = drain(router, queue_url).await;
        if let Some(found) = delivered
            .into_iter()
            .find(|event| event["Type"] == json!(event_type))
        {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no {event_type} event was invoked within {}s",
            INVOKED_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The points a deployment a caller started reaches inside that caller's own call: the
/// gate consulted before anything is written, the stage the call itself reached, and the
/// stage the strategy's own clock made due. Each event names what triggered it, and every
/// point of the deployment's lifecycle agrees with the attribution its stage's event log
/// entry carries.
///
/// The rows differ along WHICH point of the workflow the event belongs to and, with it,
/// which entity the rule attributes it to.
#[tokio::test]
async fn a_deployment_action_point_names_the_entity_that_triggered_its_stage() {
    let router = emulator();
    let (queue_url, queue_arn) = given_queue(&router, "trigger-observations").await;
    let application = create_application(&router, "trigger-app").await;
    let environment = create_environment(&router, &application, "trigger-env").await;
    let profile = create_profile(&router, &application, "trigger-profile").await;
    let seeded = create_version(
        &router,
        &application,
        &profile,
        "application/json",
        br#"{"v":1}"#,
    )
    .await;
    assert_eq!(
        seeded.status, 201,
        "seeding a version answered {}",
        seeded.status
    );
    let (extension, _) = given_extension(
        &router,
        "trigger-watcher",
        &[
            "PRE_START_DEPLOYMENT",
            "ON_DEPLOYMENT_START",
            "ON_DEPLOYMENT_COMPLETE",
        ],
        &queue_arn,
        &[],
    )
    .await;
    associate(
        &router,
        "the environment",
        &extension,
        &arn(&format!("application/{application}/environment/{environment}")),
        &Value::Null,
    )
    .await;

    // A strategy that paces no stage: the deployment is started and completed inside the
    // call, so all three points are reached by the time it answers.
    let strategy = given_strategy(&router, "trigger-instant", 0, 0).await;
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
        "expected 201, got {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let events = drain(&router, &queue_url).await;

    // (case, the event's Type, the entity the rule attributes it to, the deployment event
    // of the stage it belongs to — none for a point consulted before the deployment
    // exists).
    let rows: [(&str, &str, &str, Option<&str>); 3] = [
        (
            "the gate consulted inside the caller's own request",
            "PreStartDeployment",
            "USER",
            None,
        ),
        (
            "the stage that request itself reached",
            "OnDeploymentStart",
            "USER",
            Some("DEPLOYMENT_STARTED"),
        ),
        (
            "the stage the strategy's clock made due",
            "OnDeploymentComplete",
            "APPCONFIG",
            Some("DEPLOYMENT_COMPLETED"),
        ),
    ];
    for (case, event_type, expected, stage) in rows {
        let event = delivered(case, &events, event_type);
        assert_triggered_by(case, event, expected);
        if let Some(stage) = stage {
            let logged = logged_trigger(case, &router, &application, &environment, 1, stage).await;
            assert_eq!(
                logged, expected,
                "case {case:?}: the {stage} entry reports {logged:?}, so the action point \
                 must hand its target the same attribution"
            );
        }
    }
}

/// The points of a deployment that is still in flight: a tick, which no event of the
/// deployment owes because it is the interval AppConfig paces the rollout with, and the
/// rolled-back point of a deployment a CALLER stopped — the same action point, attributed
/// differently because a different entity triggered the stage.
#[tokio::test]
async fn an_in_flight_deployments_points_name_appconfigs_pacing_and_a_callers_stop() {
    let router = emulator();
    let (queue_url, queue_arn) = given_queue(&router, "in-flight-observations").await;
    let application = create_application(&router, "in-flight-app").await;
    let environment = create_environment(&router, &application, "in-flight-env").await;
    let profile = create_profile(&router, &application, "in-flight-profile").await;
    let seeded = create_version(
        &router,
        &application,
        &profile,
        "application/json",
        br#"{"v":1}"#,
    )
    .await;
    assert_eq!(
        seeded.status, 201,
        "seeding a version answered {}",
        seeded.status
    );
    let (extension, _) = given_extension(
        &router,
        "in-flight-watcher",
        &["AT_DEPLOYMENT_TICK", "ON_DEPLOYMENT_ROLLED_BACK"],
        &queue_arn,
        &[],
    )
    .await;
    associate(
        &router,
        "the environment",
        &extension,
        &arn(&format!("application/{application}/environment/{environment}")),
        &Value::Null,
    )
    .await;

    // A rollout window keeps the deployment in flight, so its ticks fall due and it is
    // still stoppable.
    let strategy = given_strategy(&router, "in-flight-paced", 1, 0).await;
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
        "expected 201, got {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );

    let case = "a tick AppConfig's own pacing owes";
    let tick = invoked_event(
        case,
        &router,
        &queue_url,
        &application,
        &environment,
        1,
        "AtDeploymentTick",
    )
    .await;
    assert_triggered_by(case, &tick, "APPCONFIG");

    let case = "the rollback a caller's stop asked for";
    let stopped = stop(&router, &application, &environment, 1, None).await;
    assert_eq!(
        stopped.status,
        202,
        "case {case:?}: StopDeployment answered {} / {}",
        stopped.status,
        String::from_utf8_lossy(&stopped.body)
    );
    let rolled_back = invoked_event(
        case,
        &router,
        &queue_url,
        &application,
        &environment,
        1,
        "OnDeploymentRolledBack",
    )
    .await;
    let logged = logged_trigger(
        case,
        &router,
        &application,
        &environment,
        1,
        "ROLLBACK_COMPLETED",
    )
    .await;
    assert_triggered_by(case, &rolled_back, &logged);
    assert_eq!(
        logged, "USER",
        "case {case:?}: a stop the caller asked for is the caller's, got {logged:?}"
    );
}
