// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch alarms a rollout is watched by, moved by the METRIC DATA a client publishes
//! rather than by a caller forcing a state — with CloudWatch mounted beside Amazon ECS on one
//! engine, so every alarm here is created, fed and read through CloudWatch's own API.
//!
//! The rule under test, over the whole class rather than one request:
//!
//! > A rollout whose `deploymentConfiguration.alarms` turned the method on is WATCHED past the
//! > instant its tasks are in place: Amazon ECS considers the deployment failed "when any of the
//! > alarms move to the `ALARM` state" (`ServiceDeploymentAlarms`), and an alarm reports what the
//! > revision now running is doing only once it has graded the series it watches — so the watch
//! > lasts until those alarms could have reached a verdict about that revision, the span their own
//! > `Period` and `EvaluationPeriods` take, and the rollout completes only after it ("After this
//! > percentage of tasks is healthy and the bake time elapses, Amazon ECS completes the
//! > deployment", `DeploymentEarlySuccessCriteria$healthyPercent`). An alarm that moves inside
//! > that window fails the rollout — `rolloutState` `FAILED` with a reason naming the alarms, not
//! > the circuit breaker — and `DeploymentAlarms$rollback` decides where the failure leaves the
//! > service. A rollout none of its own alarms moved during completes.
//!
//! The rows differ along what the rule generalises over: the EVALUATION CONFIGURATION of the
//! alarm that is watching, which is what decides how long the watch lasts (a one-period alarm, a
//! two-period one, and a wider period), whether the series the alarm watches is ever published
//! into at all, and whether `rollback` is on. Every row's metric data is published AFTER that
//! row's new revision is already running its task — which is the point: an alarm fed only once
//! the tasks are up is exactly the alarm a watch that ended with the task set could never catch.
//!
//! Nothing here reaches into either state struct: every ECS value asserted is read back through
//! `DescribeServices` and `DescribeServiceDeployments`, every alarm state through
//! `DescribeAlarms`, and the alarms move only because `PutMetricData` published into the series
//! they grade.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cloudwatch, count, create_cluster, emulator_with_cloudwatch, list, ok, poll_until,
    register_capacity, string,
};

/// The cluster every service below runs in, and the task-definition family they roll out.
const CLUSTER: &str = "alarmmetric";
const FAMILY: &str = "alarmmetricapp";

/// The image every container names, and a program that stays up — so a task that is running is
/// still running while the rollout it belongs to is watched.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// The custom series every row's alarm grades: one namespace, one metric name, and one dimension
/// whose VALUE is the row's own service — so publishing into one row's series publishes into no
/// other's.
const NAMESPACE: &str = "Emulator/AlarmRollout";
const METRIC: &str = "Faults";
const DIMENSION: &str = "Leg";

/// How each period of that series is folded and compared, and the value a breach carries: a
/// count at or past the threshold breaches, and a period with no datapoint at all does not
/// ("Valid Values: breaching | notBreaching | ignore | missing",
/// `PutMetricAlarmInput$TreatMissingData`).
const STATISTIC: &str = "Sum";
const COMPARISON: &str = "GreaterThanOrEqualToThreshold";
const THRESHOLD: f64 = 1.0;
const TREAT_MISSING: &str = "notBreaching";
const BREACHING_VALUE: f64 = 5.0;

/// The `StorageResolution` the datapoints below are published at: "Setting this to 1 specifies
/// this metric as a high-resolution metric, so that CloudWatch stores the metric with sub-minute
/// resolution down to one second" (`MetricDatum$StorageResolution`) — the resolution the
/// sub-minute alarm periods of these rows can be graded at.
const HIGH_RESOLUTION: i64 = 1;

/// The `StateValue` values this suite reads, of the modelled catalog
/// (`StateValue`: `ALARM | INSUFFICIENT_DATA | OK`).
const OK_STATE: &str = "OK";
const ALARM_STATE: &str = "ALARM";

/// `Deployment$rolloutState` of a rollout that reached the steady state it was rolling out, and
/// of one a failure-detection method failed. Spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a test pass.
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";
const IN_PROGRESS: &str = "IN_PROGRESS";

/// The `ServiceDeploymentRollbackMonitorsStatus` members these rows read
/// (`smithy_query(query="shape", shape="ServiceDeploymentRollbackMonitorsStatus")` -> TRIGGERED,
/// MONITORING, MONITORING_COMPLETE, DISABLED).
const TRIGGERED: &str = "TRIGGERED";
const MONITORING: &str = "MONITORING";
const MONITORING_COMPLETE: &str = "MONITORING_COMPLETE";

/// The `ServiceDeploymentStatus` a rollout the alarms rolled back ends under, and the one a
/// rollout they failed with no rollback configured ends under.
const ROLLBACK_SUCCESSFUL: &str = "ROLLBACK_SUCCESSFUL";
const STOPPED: &str = "STOPPED";

/// What the circuit breaker's own failure reason says, so a row can assert the reason its alarms
/// earned names them and NOT the other failure-detection method
/// (`crate::deployment_circuit_breaker` asserts this sentence for the breaker itself).
const CIRCUIT_BREAKER_WORD: &str = "circuit breaker";

/// `Deployment$rolloutStateReason` of a rollout its alarms failed, as the implementation words
/// it: fixed text stating what the method detected, with the alarms that moved named.
fn failure_reason(alarms: &[&str]) -> String {
    format!("ECS deployment alarms: {} in ALARM state.", alarms.join(", "))
}

/// The `statusReason` of a rollout the alarms ROLLED BACK, and of one they failed with no
/// rollback configured.
fn rolled_back_reason(alarms: &[&str]) -> String {
    format!(
        "Service deployment rolled back because CloudWatch alarm(s) {} moved to the ALARM \
         state.",
        alarms.join(", ")
    )
}

fn stopped_reason(alarms: &[&str]) -> String {
    format!(
        "Service deployment stopped because CloudWatch alarm(s) {} moved to the ALARM state.",
        alarms.join(", ")
    )
}

/// Where the alarms leave the service, for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The alarms failed the rollout and the service went back to the revision that completed.
    RolledBack,
    /// They failed it and the service stayed on the revision that failed.
    Failed,
    /// Nothing published into the series this row's alarm grades, so the rollout completed.
    Completed,
}

/// One row: a service, the alarm watching its rollouts, the evaluation configuration that alarm
/// grades its series with — which is what sizes the watch — whether its series is fed, and what
/// that owes the rollout.
struct Row {
    case: &'static str,
    service: &'static str,
    alarm: &'static str,
    /// `MetricAlarm$Period`, in seconds: one of "10, 20, 30, and any multiple of 60"
    /// (`PutMetricAlarmInput$Period`).
    period: i32,
    /// `MetricAlarm$EvaluationPeriods`: "the number of periods over which data is compared to
    /// the specified threshold".
    evaluation_periods: i32,
    /// `DeploymentAlarms$rollback`.
    rollback: bool,
    /// Whether this row publishes breaching datapoints into the series its alarm grades.
    fed: bool,
    outcome: Outcome,
}

/// The table: one row per shape the rule generalises over, each its own service with an alarm
/// and a series of its own in the one cluster.
const ROWS: &[Row] = &[
    Row {
        // The recorded shape: one period, one evaluation, rollback asked for.
        case: "a one-period alarm fed a breach after the new revision came up, with rollback on",
        service: "metricrollback",
        alarm: "metric-rollback-alarm",
        period: 10,
        evaluation_periods: 1,
        rollback: true,
        fed: true,
        outcome: Outcome::RolledBack,
    },
    Row {
        case: "an alarm that grades TWO periods before it speaks, with rollback on",
        service: "metrictwoperiods",
        alarm: "metric-two-period-alarm",
        period: 10,
        evaluation_periods: 2,
        rollback: true,
        fed: true,
        outcome: Outcome::RolledBack,
    },
    Row {
        case: "a wider-period alarm fed a breach, with no rollback configured",
        service: "metricnorollback",
        alarm: "metric-norollback-alarm",
        period: 20,
        evaluation_periods: 1,
        rollback: false,
        fed: true,
        outcome: Outcome::Failed,
    },
    Row {
        // The positive control in the same cluster: an alarm whose series nothing publishes
        // into stays out of `ALARM`, so its rollout is watched and then completes.
        case: "a watched alarm whose series is never published into",
        service: "metricquiet",
        alarm: "metric-quiet-alarm",
        period: 10,
        evaluation_periods: 1,
        rollback: true,
        fed: false,
        outcome: Outcome::Completed,
    },
];

/// The dimension list one row's series is identified by.
fn dimensions(row: &Row) -> Value {
    json!([{ "Name": DIMENSION, "Value": row.service }])
}

/// Create one row's alarm through CloudWatch's own API: the series it grades, the periods and
/// evaluation count it grades over, the comparison it is decided by, and the treatment a period
/// holding no datapoint gets — which is what settles a fed-nothing alarm in `OK`.
async fn create_alarm(router: &MultiServiceRouter, row: &Row) {
    cloudwatch(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": row.alarm,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": dimensions(row),
            "Statistic": STATISTIC,
            "Period": row.period,
            "EvaluationPeriods": row.evaluation_periods,
            "DatapointsToAlarm": row.evaluation_periods,
            "Threshold": THRESHOLD,
            "ComparisonOperator": COMPARISON,
            "TreatMissingData": TREAT_MISSING,
        }),
    )
    .await;
}

/// Publish ONE breaching datapoint per period this row's alarm grades, into the series it
/// grades, stamped on the periods that have already closed — so the next evaluation of that
/// alarm grades real data rather than waiting for the period the clock is in to end.
///
/// The CALL happens while the row's new revision is already running, which is what makes the
/// transition it earns one that happened during the watch rather than during the rollout's task
/// work.
async fn publish_breach(router: &MultiServiceRouter, row: &Row) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let period = f64::from(row.period);
    let data: Vec<Value> = (1..=row.evaluation_periods)
        .map(|back| {
            let back = f64::from(back);
            json!({
                "MetricName": METRIC,
                "Dimensions": dimensions(row),
                "Value": BREACHING_VALUE,
                "Unit": "Count",
                "StorageResolution": HIGH_RESOLUTION,
                "Timestamp": now_secs - back * period,
            })
        })
        .collect();
    cloudwatch(
        router,
        "PutMetricData",
        json!({ "Namespace": NAMESPACE, "MetricData": data }),
    )
    .await;
}

/// The `StateValue` CloudWatch reports for one alarm.
async fn alarm_state(router: &MultiServiceRouter, name: &str) -> String {
    let described = cloudwatch(router, "DescribeAlarms", json!({ "AlarmNames": [name] })).await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Wait for one alarm to report `wanted`, publishing a fresh breach before each look for a row
/// whose series has to carry one: the data a client keeps publishing while it watches for the
/// state that data implies.
async fn poll_alarm_state(router: &MultiServiceRouter, row: &Row, wanted: &str, feed: bool) {
    poll_until(
        row.case,
        &format!("the alarm {} to report {wanted}", row.alarm),
        || async {
            if feed {
                publish_breach(router, row).await;
            }
            json!(alarm_state(router, row.alarm).await)
        },
        |state| state == &json!(wanted),
    )
    .await;
}

/// Register one revision of [`FAMILY`] carrying `revision` in its container's environment, so
/// two revisions of the family differ, and answer its ARN.
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

/// Whether ANY deployment this service reports is in `FAILED`, carrying `reason`: the rollout a
/// monitor failed keeps that terminal state while the rollback that replaces it takes the
/// service over, so a caller looking for the failure finds it as the primary deployment or
/// beside the one that superseded it.
fn failed_with(service: &Value, reason: &str, case: &str) -> bool {
    list(service, "deployments", case).iter().any(|deployment| {
        deployment["rolloutState"] == json!(FAILED)
            && deployment["rolloutStateReason"] == json!(reason)
    })
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

/// Wait for one service to report a SINGLE deployment of `revision` running the one task it
/// wants: the rollout has done all its task work — the new task is up and the one it replaced is
/// gone — whatever its own state machine says about it.
async fn poll_tasks_in_place(
    router: &MultiServiceRouter,
    row: &Row,
    revision: &str,
    case: &str,
) -> Value {
    let wanted = revision.to_string();
    poll_until(
        case,
        "the rollout's task set to be in place, with the task it replaced gone",
        || describe(router, row.service, case),
        move |service| {
            let deployments = list(service, "deployments", "tasks in place");
            deployments.len() == 1
                && string(service, "taskDefinition", "tasks in place") == wanted
                && count(service, "runningCount", "tasks in place") == 1
        },
    )
    .await
}

/// RULE: an alarm that moves into `ALARM` on the metric data a client publishes AFTER a
/// rollout's tasks are up is still an alarm of that rollout's watch: it fails the rollout, and
/// `rollback` decides where the failure leaves the service.
#[tokio::test]
async fn a_metric_driven_alarm_fails_the_rollout_its_tasks_already_came_up_for() {
    let router = emulator_with_cloudwatch();
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
                        "enable": true,
                        "rollback": row.rollback,
                    },
                },
            }),
            row.case,
        )
        .await;
    }

    // The steady state the first rollout reaches — which it only reaches once its alarms have
    // had their say about it — and the `OK` those alarms settle in over a series nothing has
    // published into.
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
        poll_alarm_state(&router, row, OK_STATE, false).await;
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

    // ...and the breach each FED row publishes once that rollout's task set is already in
    // place. A watch that ended with the task set would have nothing left to catch here.
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let service = poll_tasks_in_place(&router, row, &second, case).await;
        let deployment = primary(&service, case);
        assert_eq!(
            string(&deployment, "rolloutState", case),
            IN_PROGRESS,
            "case {case:?}: a rollout whose alarms are still watching it has not completed, \
             however settled its task set is, got {deployment}"
        );
        let watching = describe_deployment(&router, &rollouts[index], case).await;
        assert_eq!(
            string(&watching["alarms"], "status", case),
            MONITORING,
            "case {case:?}: expected the alarms of a rollout they are still watching to report \
             that watch, got {watching}"
        );
        if !row.fed {
            continue;
        }
        poll_alarm_state(&router, row, ALARM_STATE, true).await;
        // The terminal `FAILED` state the alarm that just moved takes the rollout to, read
        // here — beside the move that earned it — rather than after every other row has been
        // driven: where the configuration asked for a rollback, that rollback replaces the
        // failed rollout a span later, so the state is looked for while it is the rollout's.
        // The reason names the alarms that moved and NOT the other failure-detection method,
        // which this service turned none of on.
        let reason = failure_reason(&[row.alarm]);
        poll_until(
            case,
            "the rollout the alarms failed to report the terminal FAILED state",
            || describe(&router, row.service, case),
            |service| failed_with(service, &reason, case),
        )
        .await;
        assert!(
            !reason.contains(CIRCUIT_BREAKER_WORD),
            "case {case:?}: an alarm-failed rollout names the alarms that moved and not the \
             circuit breaker, got {reason:?}"
        );
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
                // The rollback record: the revision that was deployed in the failed one's
                // place, which is one of the revisions this rollout started FROM, and when the
                // rollback began.
                let restored = string(&record["rollback"], "serviceRevisionArn", case);
                let sources: Vec<String> = list(&record, "sourceServiceRevisions", case)
                    .iter()
                    .filter_map(|source| source["arn"].as_str().map(str::to_string))
                    .collect();
                assert!(
                    sources.contains(&restored),
                    "case {case:?}: expected the rollback to name a revision this rollout \
                     replaced, got {restored:?} against {sources:?}"
                );
                assert!(
                    record["rollback"]["startedAt"].is_number(),
                    "case {case:?}: expected the rollback to carry the instant it started at, \
                     got {record}"
                );
            }
            Outcome::Failed => {
                // With no rollback configured the failure is where the rollout ENDS, so the
                // service is still on the revision it failed on.
                let service = describe(&router, row.service, case).await;
                assert!(
                    failed_with(&service, &failure_reason(&[row.alarm]), case),
                    "case {case:?}: a rollout the alarms failed keeps the terminal FAILED \
                     state, got {service}"
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
                    "the rollout to complete, since nothing published into the series its alarm \
                     grades",
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
                assert_eq!(
                    alarm_state(&router, row.alarm).await,
                    OK_STATE,
                    "case {case:?}: expected the alarm of a series nothing published into to \
                     stay out of ALARM"
                );
                let record = describe_deployment(&router, &rollout, case).await;
                assert_eq!(
                    string(&record["alarms"], "status", case),
                    MONITORING_COMPLETE,
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
