// SPDX-License-Identifier: CC0-1.0

//! What an alarm that is ALREADY sounding when a Lambda deployment is created
//! does to that deployment — end to end over the wire, with Lambda and CloudWatch
//! mounted beside CodeDeploy on one engine.
//!
//! `tests/all/lambda_traffic_shifting.rs` covers an alarm that starts sounding
//! WHILE a shift's window is open, so some traffic has already moved and the
//! rollback has something to put back. This suite is about the other end of the
//! same gate: the alarm is sounding BEFORE the deployment starts, so the gate
//! stops it before its first increment falls due and the deployment moves no
//! traffic at all.
//!
//! The rule under test: a deployment is gated on the alarms its effective
//! `alarmConfiguration` names from the instant it STARTS and before any traffic
//! moves, so a deployment created while one of those alarms is in `ALARM` reaches
//! a terminal `Stopped` with `errorInformation.code` `ALARM_ACTIVE` naming that
//! alarm, having left the alias its AppSpec names on the version and the routing
//! it already held — no increment of the declared shift ever reaches it — and the
//! rollback its `DEPLOYMENT_STOP_ON_ALARM` configuration asks for is a second
//! deployment of the revision the group was on, attributed to
//! `codeDeployRollback`; the same deployment created while no named alarm is
//! sounding succeeds and moves the alias to the AppSpec's `TargetVersion`.
//!
//! So the tables drive that over a SET of inputs rather than the one case: the
//! traffic routing configuration the shift would have run under (a single
//! cutover, a canary, a linear ladder — the stop has to come before the FIRST
//! increment of each), where the configuration naming the alarm lives (the
//! group's, or the deployment's own override of it), and the state the alarm is
//! in when the deployment is created (`ALARM`, `OK`, `INSUFFICIENT_DATA`).
//!
//! The function, its versions and its alias are created through Lambda's own API
//! and the alias read back with `GetAlias`; the alarm is created with
//! `PutMetricAlarm` and forced with `SetAlarmState`. Nothing here reaches into
//! any service's store.

use serde_json::{json, Value};

use crate::alarm_stopped_deployments::{
    alarm_at, create_deployment, listed_deployments, STATE_ALARM, STATE_INSUFFICIENT_DATA,
    STATE_OK,
};
use crate::codedeploy_lifecycle::{terminal_deployment_within, Emulator};
use crate::lambda_traffic_shifting::{
    alias_reading, appspec_revision, function_with_versions, lambda_group, readings_until_terminal,
    traffic_routing, AliasReading, ALARM_ACTIVE, ALIAS, ALL_AT_ONCE, CREATOR_ROLLBACK,
    SHIFT_WINDOW_DEADLINE, WINDOW_DEADLINE,
};

/// The version the alias invokes before anything is deployed.
const ORIGINAL_VERSION: &str = "1";
/// The version the group's first (control) deployment moves the alias to, which
/// is therefore the version a rollback of the next deployment goes back to.
const SETTLED_VERSION: &str = "2";
/// The version the deployment under test tries to move the alias to.
const BLOCKED_VERSION: &str = "3";

/// The `autoRollbackConfiguration` that asks for a rollback of a deployment an
/// alarm stopped (`smithy_query(query="shape",shape="AutoRollbackEvent")`:
/// DEPLOYMENT_FAILURE | DEPLOYMENT_STOP_ON_ALARM | DEPLOYMENT_STOP_ON_REQUEST).
fn rollback_on_alarm() -> Value {
    json!({"enabled": true, "events": ["DEPLOYMENT_STOP_ON_ALARM"]})
}

/// An `alarmConfiguration` polling `alarm`.
fn alarm_configuration(alarm: &str) -> Value {
    json!({"enabled": true, "alarms": [{"name": alarm}]})
}

// The create a row's deployments go through, and the read of the ids
// `ListDeployments` answers for a group, are the sibling alarm suite's
// (`crate::alarm_stopped_deployments`): one place says how a deployment of this
// service is created and listed.

/// One row of both tables below.
struct Case {
    case: &'static str,
    /// The `trafficRoutingConfig` the deployment group's configuration carries,
    /// `None` for the built-in Lambda all-at-once configuration.
    routing: Option<Value>,
    /// Whether the configuration naming the alarm is the DEPLOYMENT's own
    /// `overrideAlarmConfiguration` rather than the deployment group's.
    alarm_on_deployment: bool,
    /// The state the alarm is forced into before the deployment under test is
    /// created.
    alarm_state: &'static str,
}

impl Case {
    /// The reading the alias holds before the deployment under test is created,
    /// which is what a stopped deployment has to leave it on.
    fn settled_alias(&self) -> AliasReading {
        (SETTLED_VERSION.to_string(), Vec::new())
    }
}

/// Stand the row up: a function with three versions and a `live` alias on the
/// first, an alarm at `OK`, a Lambda deployment group under the row's routing
/// configuration that rolls back on an alarm, and one control deployment that
/// moves the alias to [`SETTLED_VERSION`] while the alarm is quiet.
///
/// Answers the function, the application, the group and the alarm.
async fn given_group_on_a_settled_revision(
    emulator: &Emulator,
    case: &Case,
    slug: &str,
) -> (String, String, String, String) {
    let function = format!("{slug}-fn");
    let application = format!("{slug}-app");
    let alarm = format!("{slug}-alarm");
    function_with_versions(emulator, &function, 3).await;
    alarm_at(emulator, &alarm, STATE_OK).await;
    let mut extras = json!({"autoRollbackConfiguration": rollback_on_alarm()});
    if !case.alarm_on_deployment {
        extras["alarmConfiguration"] = alarm_configuration(&alarm);
    }
    let group = lambda_group(emulator, &application, case.routing.clone(), &extras).await;

    let control = create_deployment(
        emulator,
        &application,
        &group,
        &appspec_revision(&function, ALIAS, ORIGINAL_VERSION, SETTLED_VERSION),
        // The control leg is the SETUP: it only has to leave the alias on a known
        // version, so it shifts all of its traffic in one step rather than walking
        // the window the row's own configuration states.
        &json!({"deploymentConfigName": ALL_AT_ONCE}),
    )
    .await;
    let info = terminal_deployment_within(emulator, &control, case.case, WINDOW_DEADLINE).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {:?}: the control deployment under a quiet alarm must succeed, got {info:?}",
        case.case
    );
    assert_eq!(
        alias_reading(emulator, &function).await,
        case.settled_alias(),
        "case {:?}: the control deployment must leave the alias on version {SETTLED_VERSION}",
        case.case
    );
    (function, application, group, alarm)
}

/// The deployment under test: the alarm is forced into the row's state FIRST, so
/// the deployment is created with it already there.
async fn deployment_under_a_sounding_alarm(
    emulator: &Emulator,
    case: &Case,
    function: &str,
    application: &str,
    group: &str,
    alarm: &str,
) -> String {
    alarm_at(emulator, alarm, case.alarm_state).await;
    let extras = if case.alarm_on_deployment {
        json!({"overrideAlarmConfiguration": alarm_configuration(alarm)})
    } else {
        json!({})
    };
    create_deployment(
        emulator,
        application,
        group,
        &appspec_revision(function, ALIAS, SETTLED_VERSION, BLOCKED_VERSION),
        &extras,
    )
    .await
}

/// RULE: a deployment whose effective alarm configuration names an alarm that is
/// already in `ALARM` when it is created is stopped before any of its traffic
/// increments falls due — terminal `Stopped`, `errorInformation.code`
/// `ALARM_ACTIVE` naming that alarm — and the alias its AppSpec names is left on
/// the version and routing it held, whatever increments the deployment's routing
/// configuration would have shifted it in.
#[tokio::test]
async fn an_alarm_already_sounding_stops_the_deployment_before_any_traffic_moves() {
    // Rows differ along the shift the deployment would have performed (a single
    // cutover, a two-step canary, a linear ladder) and along where the
    // configuration naming the alarm lives.
    let cases: &[Case] = &[
        Case {
            case: "a single cutover",
            routing: None,
            alarm_on_deployment: false,
            alarm_state: STATE_ALARM,
        },
        Case {
            case: "a canary shift",
            routing: Some(traffic_routing("TimeBasedCanary", 10, 1)),
            alarm_on_deployment: false,
            alarm_state: STATE_ALARM,
        },
        Case {
            case: "a linear shift",
            routing: Some(traffic_routing("TimeBasedLinear", 50, 1)),
            alarm_on_deployment: false,
            alarm_state: STATE_ALARM,
        },
        Case {
            case: "the deployment's own override names the sounding alarm",
            routing: None,
            alarm_on_deployment: true,
            alarm_state: STATE_ALARM,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.case;
        let emulator = Emulator::with_lambda_and_cloudwatch();
        let slug = format!("blocked-{index}");
        let (function, application, group, alarm) =
            given_group_on_a_settled_revision(&emulator, case, &slug).await;
        let blocked = deployment_under_a_sounding_alarm(
            &emulator,
            case,
            &function,
            &application,
            &group,
            &alarm,
        )
        .await;

        // The alias is sampled for the whole run of the stopped deployment: no
        // increment of the declared shift may ever show up on it.
        let (observed, info) =
            readings_until_terminal(&emulator, &function, &blocked, name).await;
        assert_eq!(
            observed,
            vec![case.settled_alias()],
            "case {name:?}: expected the alias to stay at {:?} for the whole run, got {observed:?}",
            case.settled_alias()
        );
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str()
            ),
            (Some("Stopped"), Some(ALARM_ACTIVE)),
            "case {name:?}: expected the sounding alarm to stop the deployment, got {info:?}"
        );
        let message = info["errorInformation"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(&alarm),
            "case {name:?}: expected the stop to name the activated alarm {alarm:?}, got \
             {message:?}"
        );

        // The rollback the configuration asked for: a deployment of the revision
        // the group was on, CodeDeploy's own, listed with the group's deployments
        // — and it leaves the alias where the stopped deployment found it.
        let rollback_id = info["rollbackInfo"]["rollbackDeploymentId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !rollback_id.is_empty(),
            "case {name:?}: expected the stop to record its rollback deployment, got {info:?}"
        );
        let listed = listed_deployments(&emulator, &application, &group).await;
        assert!(
            listed.contains(&rollback_id),
            "case {name:?}: expected {rollback_id:?} among the group's deployments, got {listed:?}"
        );
        let rolled_back =
            terminal_deployment_within(&emulator, &rollback_id, name, WINDOW_DEADLINE).await;
        assert_eq!(
            (
                rolled_back["creator"].as_str(),
                rolled_back["rollbackInfo"]["rollbackTriggeringDeploymentId"].as_str()
            ),
            (Some(CREATOR_ROLLBACK), Some(blocked.as_str())),
            "case {name:?}: expected CodeDeploy's own rollback of the stopped deployment, got \
             {rolled_back:?}"
        );
        let after = alias_reading(&emulator, &function).await;
        assert_eq!(
            after,
            case.settled_alias(),
            "case {name:?}: expected the rollback to leave the alias at {:?}, got {after:?}",
            case.settled_alias()
        );
    }
}

/// RULE: the same deployment created while no alarm its effective configuration
/// names is sounding runs to `Succeeded` with no error information and moves the
/// alias to its AppSpec's `TargetVersion` — only the `ALARM` state stops a
/// deployment.
#[tokio::test]
async fn a_deployment_created_under_an_alarm_that_is_not_sounding_moves_its_alias() {
    // Rows differ along the state the alarm is in — every `StateValue` other
    // than `ALARM` — and along the shift the deployment performs.
    let cases: &[Case] = &[
        Case {
            case: "a single cutover with the alarm at OK",
            routing: None,
            alarm_on_deployment: false,
            alarm_state: STATE_OK,
        },
        Case {
            case: "a canary shift with the alarm on insufficient data",
            routing: Some(traffic_routing("TimeBasedCanary", 10, 1)),
            alarm_on_deployment: false,
            alarm_state: STATE_INSUFFICIENT_DATA,
        },
        Case {
            case: "the deployment's own override, with the alarm at OK",
            routing: None,
            alarm_on_deployment: true,
            alarm_state: STATE_OK,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.case;
        let emulator = Emulator::with_lambda_and_cloudwatch();
        let slug = format!("released-{index}");
        let (function, application, group, alarm) =
            given_group_on_a_settled_revision(&emulator, case, &slug).await;
        let released = deployment_under_a_sounding_alarm(
            &emulator,
            case,
            &function,
            &application,
            &group,
            &alarm,
        )
        .await;

        let info =
            terminal_deployment_within(&emulator, &released, name, SHIFT_WINDOW_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {name:?}: expected an alarm that is not sounding to stop nothing, got {info:?}"
        );
        assert!(
            info["errorInformation"].is_null(),
            "case {name:?}: expected no errorInformation on a deployment nothing stopped, got {:?}",
            info["errorInformation"]
        );
        assert!(
            info["rollbackInfo"]["rollbackDeploymentId"].is_null(),
            "case {name:?}: expected no rollback deployment, got {:?}",
            info["rollbackInfo"]
        );
        let after = alias_reading(&emulator, &function).await;
        assert_eq!(
            after,
            (BLOCKED_VERSION.to_string(), Vec::new()),
            "case {name:?}: expected the alias moved to version {BLOCKED_VERSION}, got {after:?}"
        );
    }
}
