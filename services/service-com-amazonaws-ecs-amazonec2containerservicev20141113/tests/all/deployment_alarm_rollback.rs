// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch ALARMS a rollout is watched by, driven end to end with CloudWatch mounted
//! beside Amazon ECS on the same engine — so every alarm these rows watch is moved into its
//! `ALARM` state through CloudWatch's OWN API, never by writing into a store.
//!
//! The rule under test, over the whole class rather than one request:
//!
//! > A rollout whose `deploymentConfiguration.alarms` turned the method on
//! > (`DeploymentAlarms$enable`) is WATCHED while it deploys, and Amazon ECS "considers the
//! > service deployment as failed when any of the alarms move to the `ALARM` state"
//! > (`ServiceDeploymentAlarms`): the rollout reaches the terminal `FAILED` state of the
//! > deployment state machine with the reason that names the alarms that moved, those alarms
//! > are what `ServiceDeploymentAlarms$triggeredAlarmNames` reports ("one or more CloudWatch
//! > alarm names that have been triggered during the service deployment") and the alarms
//! > report `TRIGGERED`, and `DeploymentAlarms$rollback` decides where that leaves the
//! > service: "If rollback is used, when a service deployment fails, the service is rolled
//! > back to the last deployment that completed successfully", while a rollout failed with no
//! > rollback configured ends where it stands. A rollout no watched alarm MOVED into that
//! > state during completes.
//!
//! The rows differ along what the rule generalises over: whether `rollback` is on, which is
//! the difference between a service taken back to the revision that completed and one left on
//! the revision that failed; whether the alarm moves at all; whether the method is turned on
//! AT ALL, which is the row that pins the rule to the configuration rather than to the alarm;
//! WHEN the alarm moved, since only an alarm triggered DURING the deployment is one of the
//! deployment's; and which KIND of alarm `alarmNames` names, since that member names alarms
//! without distinguishing a metric alarm from a composite one.
//!
//! Nothing here reaches into either state struct: every value asserted is read back through
//! `DescribeServices` and `DescribeServiceDeployments`, and every alarm state is written
//! through CloudWatch's own `SetAlarmState`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cloudwatch, count, create_cluster, emulator_with_cloudwatch, list, ok, poll_until,
    register_capacity, string,
};

/// The cluster every service below runs in.
const CLUSTER: &str = "alarmrollout";

/// The task-definition family every service below rolls out.
const FAMILY: &str = "alarmapp";

/// The image every container names, and a program that stays up — so a task that is running
/// is still running while the rollout it belongs to is read.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// `Deployment$rolloutState` of a rollout that reached the steady state it was rolling out,
/// and of one a failure-detection method failed. Spelled out here rather than imported, so a
/// wrong constant in the implementation cannot make a test pass.
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// The `ServiceDeploymentRollbackMonitorsStatus` members these rows read
/// (`smithy_query(query="shape", shape="ServiceDeploymentRollbackMonitorsStatus")` ->
/// TRIGGERED, MONITORING, MONITORING_COMPLETE, DISABLED).
const TRIGGERED: &str = "TRIGGERED";
const MONITORING_COMPLETE: &str = "MONITORING_COMPLETE";
const DISABLED: &str = "DISABLED";

/// The `ServiceDeploymentStatus` a rollout the alarms rolled back ends under, and the one a
/// rollout they failed with no rollback configured ends under.
const ROLLBACK_SUCCESSFUL: &str = "ROLLBACK_SUCCESSFUL";
const STOPPED: &str = "STOPPED";

/// `Deployment$rolloutStateReason` of a rollout its alarms failed, as the implementation words
/// it: fixed text stating what the method detected, with the alarms that moved named.
fn failure_reason(alarms: &[&str]) -> String {
    format!("ECS deployment alarms: {} in ALARM state.", alarms.join(", "))
}

/// The `statusReason` of a rollout the alarms ROLLED BACK.
fn rolled_back_reason(alarms: &[&str]) -> String {
    format!(
        "Service deployment rolled back because CloudWatch alarm(s) {} moved to the ALARM \
         state.",
        alarms.join(", ")
    )
}

/// The same for a rollout the alarms failed with no rollback configured.
fn stopped_reason(alarms: &[&str]) -> String {
    format!(
        "Service deployment stopped because CloudWatch alarm(s) {} moved to the ALARM state.",
        alarms.join(", ")
    )
}

/// An emulator with Amazon ECS and CloudWatch — which owns the alarms a rollout is watched by
/// — on ONE shared engine ([`crate::ecs_wire::emulator_with_cloudwatch`]).
fn emulator() -> MultiServiceRouter {
    emulator_with_cloudwatch()
}

/// WHICH kind of alarm one row's `alarmNames` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A metric alarm, created by `PutMetricAlarm`.
    Metric,
    /// A composite alarm, created by `PutCompositeAlarm` over a metric alarm of its own —
    /// "the alarms specified in a composite alarm's rule expression can include metric alarms
    /// and other composite alarms".
    Composite,
}

/// WHEN one row's alarm moves into its `ALARM` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moves {
    /// While the second rollout is deploying, which is what makes it an alarm "triggered
    /// during the service deployment".
    DuringRollout,
    /// BEFORE that rollout started — after the first one had already completed — so the
    /// deployment it is watching was not deploying when it moved.
    BeforeRollout,
    /// Never: the alarm stays in the state a created alarm starts in.
    Never,
}

/// Where the alarms leave the service, for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The alarms failed the rollout and the service went back to the revision that
    /// completed.
    RolledBack,
    /// The alarms failed the rollout and the service stayed on the revision that failed.
    Failed,
    /// No alarm of this rollout's own moved while it was deploying, so it completed.
    Completed,
}

/// One row: a service, the alarms it is created with, when its alarm moves, and what that
/// owes the rollout.
struct Row {
    case: &'static str,
    service: &'static str,
    /// The alarm `DeploymentAlarms$alarmNames` names.
    alarm: &'static str,
    kind: Kind,
    /// `DeploymentAlarms$enable`.
    enable: bool,
    /// `DeploymentAlarms$rollback`.
    rollback: bool,
    moves: Moves,
    outcome: Outcome,
}

/// The rows, each its own service in the one cluster with an alarm of its own.
const ROWS: &[Row] = &[
    Row {
        case: "an alarm that moves during the rollout, with rollback on",
        service: "alarmrollback",
        alarm: "rollback-alarm",
        kind: Kind::Metric,
        enable: true,
        rollback: true,
        moves: Moves::DuringRollout,
        outcome: Outcome::RolledBack,
    },
    Row {
        case: "an alarm that moves during the rollout, with no rollback",
        service: "alarmnorollback",
        alarm: "norollback-alarm",
        kind: Kind::Metric,
        enable: true,
        rollback: false,
        moves: Moves::DuringRollout,
        outcome: Outcome::Failed,
    },
    Row {
        case: "a watched alarm that stays quiet",
        service: "alarmquiet",
        alarm: "quiet-alarm",
        kind: Kind::Metric,
        enable: true,
        rollback: true,
        moves: Moves::Never,
        outcome: Outcome::Completed,
    },
    Row {
        case: "an alarm that moves while the method is turned off",
        service: "alarmdisabled",
        alarm: "disabled-alarm",
        kind: Kind::Metric,
        enable: false,
        rollback: true,
        moves: Moves::DuringRollout,
        outcome: Outcome::Completed,
    },
    Row {
        case: "an alarm already in ALARM before the rollout started",
        service: "alarmearly",
        alarm: "early-alarm",
        kind: Kind::Metric,
        enable: true,
        rollback: true,
        moves: Moves::BeforeRollout,
        outcome: Outcome::Completed,
    },
    Row {
        case: "a composite alarm that moves during the rollout, with rollback on",
        service: "alarmcomposite",
        alarm: "composite-alarm",
        kind: Kind::Composite,
        enable: true,
        rollback: true,
        moves: Moves::DuringRollout,
        outcome: Outcome::RolledBack,
    },
];

/// Create the alarm one row watches, through CloudWatch's own API.
///
/// The smallest alarm that API accepts: a name, the series it grades, the period and
/// evaluation count it grades over, and the threshold it compares against. Nothing publishes
/// into that series, so the alarm's state only ever moves because this suite moves it.
async fn create_alarm(router: &MultiServiceRouter, row: &Row) {
    let child = format!("{}-child", row.alarm);
    let metric_alarm = match row.kind {
        Kind::Metric => row.alarm.to_string(),
        Kind::Composite => child.clone(),
    };
    cloudwatch(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": metric_alarm,
            "Namespace": "AlarmRollout/Test",
            "MetricName": "Errors",
            "Statistic": "Sum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
        }),
    )
    .await;
    if row.kind == Kind::Composite {
        cloudwatch(
            router,
            "PutCompositeAlarm",
            json!({
                "AlarmName": row.alarm,
                "AlarmRule": format!("ALARM(\"{child}\")"),
            }),
        )
        .await;
    }
}

/// Move one row's alarm into its `ALARM` state, through CloudWatch's own API — the one write
/// these rows make to an alarm.
async fn fire_alarm(router: &MultiServiceRouter, row: &Row) {
    cloudwatch(
        router,
        "SetAlarmState",
        json!({
            "AlarmName": row.alarm,
            "StateValue": "ALARM",
            "StateReason": "the rollout under test is being failed by this alarm",
        }),
    )
    .await;
}

/// Register one revision of [`FAMILY`] whose container carries `revision` in its environment,
/// so two revisions of the family differ, and answer its ARN.
async fn register(router: &MultiServiceRouter, revision: &str, case: &str) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": STAYS_UP,
                "environment": [{ "name": "REVISION", "value": revision }],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Read one service back through `DescribeServices`.
async fn describe(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// The service's current (`PRIMARY`) deployment, which is the first of the deployments it
/// reports.
fn primary(service: &Value, case: &str) -> Value {
    list(service, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {service}"))
}

/// Read one service deployment back through `DescribeServiceDeployments`.
async fn describe_deployment(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [arn] }),
        case,
    )
    .await;
    list(&body, "serviceDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout back, got {body}"))
}

/// The names `triggeredAlarmNames` reports for one rollout.
fn triggered_names(deployment: &Value, case: &str) -> Vec<String> {
    list(&deployment["alarms"], "triggeredAlarmNames", case)
        .into_iter()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect()
}

/// RULE: an alarm that moves into `ALARM` while a rollout it watches is deploying fails that
/// rollout, and `rollback` decides where the failure leaves the service.
#[tokio::test]
async fn a_watched_alarm_that_moves_during_a_rollout_fails_it_and_rolls_the_service_back() {
    let router = emulator();
    create_cluster(&router, CLUSTER, "alarms").await;
    // The capacity every service's task is placed on.
    register_capacity(&router, CLUSTER, "alarms").await;
    let first = register(&router, "1", "revisions").await;
    let second = register(&router, "2", "revisions").await;

    // Every row's alarm, and a service on the revision every row CAN run — so each has a
    // deployment that completed successfully, the one a rollback puts back.
    for row in ROWS {
        create_alarm(&router, row).await;
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service,
                "taskDefinition": first,
                "desiredCount": 1,
                "launchType": "EC2",
                "deploymentController": { "type": "ECS" },
                "deploymentConfiguration": {
                    "alarms": {
                        "alarmNames": [row.alarm],
                        "enable": row.enable,
                        "rollback": row.rollback,
                    },
                },
            }),
            row.case,
        )
        .await;
    }

    // The steady state the first rollout reaches, which is what makes it "the last deployment
    // that completed successfully".
    for row in ROWS {
        poll_until(
            row.case,
            "the first rollout to complete with its task running",
            || describe(&router, row.service, row.case),
            |service| {
                let deployment = primary(service, "steady");
                string(&deployment, "rolloutState", "steady") == COMPLETED
                    && count(service, "runningCount", "steady") == 1
            },
        )
        .await;
    }

    // The alarm of a row whose alarm was ALREADY firing when its second rollout began: it
    // moves here, between the two rollouts.
    for row in ROWS.iter().filter(|row| row.moves == Moves::BeforeRollout) {
        fire_alarm(&router, row).await;
    }

    // The second rollout, which every row's alarms are watching.
    let mut rollouts = Vec::new();
    for row in ROWS {
        let body = ok(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service,
                "taskDefinition": second,
            }),
            row.case,
        )
        .await;
        rollouts.push(string(&body["service"], "currentServiceDeployment", row.case));
    }

    // ...and the alarms that move WHILE it deploys.
    for row in ROWS.iter().filter(|row| row.moves == Moves::DuringRollout) {
        fire_alarm(&router, row).await;
    }

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let rollout = rollouts[index].clone();
        match row.outcome {
            Outcome::RolledBack => {
                let wanted = first.clone();
                let service = poll_until(
                    case,
                    "the service to be rolled back to the revision that completed, running the \
                     task count it wants",
                    || describe(&router, row.service, case),
                    move |service| {
                        let deployment = primary(service, "rolled back");
                        string(service, "taskDefinition", "rolled back") == wanted
                            && string(&deployment, "rolloutState", "rolled back") == COMPLETED
                            && count(service, "runningCount", "rolled back") == 1
                    },
                )
                .await;
                let deployment = primary(&service, case);
                assert_eq!(
                    string(&deployment, "taskDefinition", case),
                    first,
                    "case {case:?}: expected the PRIMARY deployment to run the revision that \
                     completed, got {deployment}"
                );
                // The rollout the alarms failed, as a resource of its own.
                let record = describe_deployment(&router, &rollout, case).await;
                assert_eq!(
                    string(&record, "status", case),
                    ROLLBACK_SUCCESSFUL,
                    "case {case:?}: expected the failed rollout to end rolled back, got {record}"
                );
                assert_eq!(
                    string(&record, "statusReason", case),
                    rolled_back_reason(&[row.alarm]),
                    "case {case:?}: expected the rollback to name the alarms that moved, got \
                     {record}"
                );
                assert_eq!(
                    string(&record["alarms"], "status", case),
                    TRIGGERED,
                    "case {case:?}: expected the alarms to report that they fired, got {record}"
                );
                assert_eq!(
                    triggered_names(&record, case),
                    vec![row.alarm.to_string()],
                    "case {case:?}: expected the alarm that moved to be the one reported as \
                     triggered, got {record}"
                );
                // The CIRCUIT BREAKER is the other failure-detection method, and this service
                // turned none on: what failed the rollout was the alarms.
                assert_eq!(
                    string(&record["deploymentCircuitBreaker"], "status", case),
                    DISABLED,
                    "case {case:?}: expected no circuit breaker to be watching, got {record}"
                );
            }
            Outcome::Failed => {
                let service = poll_until(
                    case,
                    "the rollout to reach the terminal FAILED state",
                    || describe(&router, row.service, case),
                    |service| {
                        string(&primary(service, "failed"), "rolloutState", "failed") == FAILED
                    },
                )
                .await;
                let deployment = primary(&service, case);
                assert_eq!(
                    string(&deployment, "rolloutStateReason", case),
                    failure_reason(&[row.alarm]),
                    "case {case:?}: expected the reason to name the alarms that moved, got \
                     {deployment}"
                );
                assert_eq!(
                    string(&service, "taskDefinition", case),
                    second,
                    "case {case:?}: with no rollback configured the service stays on the \
                     revision that failed, got {service}"
                );
                let record = describe_deployment(&router, &rollout, case).await;
                assert_eq!(
                    string(&record, "status", case),
                    STOPPED,
                    "case {case:?}: expected the rollout to end where it stood, got {record}"
                );
                assert_eq!(
                    string(&record, "statusReason", case),
                    stopped_reason(&[row.alarm]),
                    "case {case:?}: expected the end to name the alarms that moved, got {record}"
                );
                assert_eq!(
                    triggered_names(&record, case),
                    vec![row.alarm.to_string()],
                    "case {case:?}: expected the alarm that moved to be reported as triggered, \
                     got {record}"
                );
            }
            Outcome::Completed => {
                let wanted = second.clone();
                let service = poll_until(
                    case,
                    "the rollout to complete, since no alarm of its own moved while it was \
                     deploying",
                    || describe(&router, row.service, case),
                    move |service| {
                        let deployment = primary(service, "completed");
                        string(service, "taskDefinition", "completed") == wanted
                            && string(&deployment, "rolloutState", "completed") == COMPLETED
                            && count(service, "runningCount", "completed") == 1
                    },
                )
                .await;
                let deployment = primary(&service, case);
                assert_ne!(
                    string(&deployment, "rolloutState", case),
                    FAILED,
                    "case {case:?}: expected the rollout to reach its steady state, got \
                     {deployment}"
                );
                let record = describe_deployment(&router, &rollout, case).await;
                assert_eq!(
                    string(&record["alarms"], "status", case),
                    if row.enable {
                        MONITORING_COMPLETE
                    } else {
                        DISABLED
                    },
                    "case {case:?}: expected the alarms of a rollout nothing failed to report \
                     the end of their watch, got {record}"
                );
                assert!(
                    triggered_names(&record, case).is_empty(),
                    "case {case:?}: expected no alarm to be reported as triggered during this \
                     rollout, got {record}"
                );
            }
        }
    }
}
