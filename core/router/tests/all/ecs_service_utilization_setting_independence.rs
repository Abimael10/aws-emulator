// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ECS` utilization series of a SERVICE is published whatever the cluster's
//! `containerInsights` setting says — and never under the cluster's name alone.
//!
//! RULE: the `AWS/ECS` `CPUUtilization` and `MemoryUtilization` of a service are published, in
//! `Percent`, on EXACTLY the dimension pair `ClusterName` + `ServiceName` for as long as that
//! service holds a running task, independently of the `containerInsights` cluster setting: that
//! setting decides only whether the cluster ALSO publishes the separate Container Insights
//! namespace (`ClusterSetting$name` `containerInsights` "indicates whether CloudWatch Container
//! Insights is on or off for a cluster"), so it changes which namespaces carry series and never
//! the dimension set of a service's own utilization. The base namespace carries no series on
//! `ClusterName` alone for such a service — a cluster-wide utilization measures the container
//! instances registered to a cluster, of which a Fargate-only cluster has none — so an alarm
//! over the pair reaches the state its datapoints imply while its `ClusterName`-only twin, over
//! a dimension set nothing publishes, stays `INSUFFICIENT_DATA`.
//!
//! The rows differ along what the rule generalises over: the `containerInsights` value the
//! cluster carries — `disabled` (the recorded shape), `enabled`, `enhanced`, and a cluster that
//! sets nothing and so inherits the account setting in force — and WHICH of the two metric
//! names the row's pair of alarms watches. Each row runs its service in a cluster of its own, so
//! its series are read on their own. That the setting really took effect is itself asserted,
//! against the namespace it governs: a row the setting is ON for carries the Container Insights
//! cluster-level series, and a row it is OFF for carries none — while both publish the same
//! per-service utilization. Without that, the emptiness the `AWS/ECS` namespace is asserted to
//! hold on `ClusterName` alone would be the emptiness of a cluster nothing published for.
//!
//! Two crates have to agree for this (ECS meters its services and decides what its cluster
//! setting turns on, CloudWatch stores every series, lists it and grades the alarms over it) and
//! the `awsvpc` placements resolve a subnet in a third, so the suite lives here, beside the one
//! that spans how many tasks a service runs and which capacity runs them
//! ([`crate::ecs_service_utilization_metrics`]), whose harness it drives the emulator through.
//!
//! Nothing here reaches into a store: the clusters and services are created through the ECS
//! front door, and every datapoint, metric listing and alarm state is read back through
//! CloudWatch's own `GetMetricStatistics`, `ListMetrics`, `PutMetricAlarm` and `DescribeAlarms`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// One harness for one rule: the two services mounted on one engine, the two calls a client
// makes, the alarm reader, and the names and units the series carries.
use crate::ecs_service_utilization_metrics::{
    alarm_state, cloudwatch, ecs, emulator, CLUSTER_DIMENSION, CPU_UTILIZATION, IMAGE,
    INSUFFICIENT_DATA, MEMORY_UTILIZATION, NAMESPACE, PERCENT, SERVICE_DIMENSION, STAYS_UP, SUBNET,
};

/// The namespace the `containerInsights` setting governs, and one cluster-level metric of it —
/// spelled out here rather than imported, so a wrong constant in the implementation cannot make
/// a test pass. (CloudWatch User Guide, "Amazon ECS Container Insights metrics".)
const CONTAINER_INSIGHTS_NAMESPACE: &str = "ECS/ContainerInsights";
const TASK_COUNT: &str = "TaskCount";

/// The `StateValue` an alarm over a series of zeroes settles in, for the condition every row's
/// alarms carry: a utilization is a percentage, so none of its datapoints is above 100.
const OK: &str = "OK";

/// The condition both alarms of a row are created with, and the threshold it compares against.
/// What decides whether an alarm leaves `INSUFFICIENT_DATA` is therefore whether the dimension
/// set it names receives data at all, which is the contrast this suite is about.
const ABOVE_FULL_UTILIZATION: &str = "GreaterThanThreshold";
const FULL_UTILIZATION: f64 = 100.0;

/// How often a poll re-reads what it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one wait keeps trying before it gives up. Each wait gets this budget of its OWN,
/// never one shared between two: Amazon ECS creates and attaches the interface an `awsvpc` task
/// is allocated one documented step at a time and only then starts it, so a correct run is far
/// inside this and a broken one is reported promptly. Nothing here asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How many further looks an assertion of EMPTINESS, or of a state that must HOLD, takes.
/// Together with [`POLL_INTERVAL`] a window far longer than the spacing a series that is
/// catching up publishes at, so what a row asserts is what held rather than what was read too
/// early.
const QUIET_LOOKS: usize = 20;

/// One row: the `containerInsights` value its cluster carries, and the metric its two alarms
/// watch.
struct Row {
    case: &'static str,
    cluster: &'static str,
    service: &'static str,
    /// The setting the create names, and `None` for a cluster that sets nothing and so inherits
    /// the account setting in force.
    setting: Option<&'static str>,
    /// Whether that setting turns Container Insights ON for the cluster — asserted against the
    /// namespace it governs, so a row's setting is known to have taken effect.
    insights_on: bool,
    /// Which of the two utilization metrics this row's pair of alarms is created over.
    alarm_metric: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: a cluster Container Insights is disabled for",
        cluster: "setting-disabled",
        service: "orders",
        setting: Some("disabled"),
        insights_on: false,
        alarm_metric: CPU_UTILIZATION,
    },
    Row {
        case: "a cluster Container Insights is enabled for",
        cluster: "setting-enabled",
        service: "checkout",
        setting: Some("enabled"),
        insights_on: true,
        alarm_metric: MEMORY_UTILIZATION,
    },
    Row {
        case: "a cluster Container Insights is enabled for with enhanced observability",
        cluster: "setting-enhanced",
        service: "billing",
        setting: Some("enhanced"),
        insights_on: true,
        alarm_metric: CPU_UTILIZATION,
    },
    Row {
        case: "a cluster that sets nothing, so the account setting decides",
        cluster: "setting-default",
        service: "shipping",
        setting: None,
        insights_on: false,
        alarm_metric: MEMORY_UTILIZATION,
    },
];

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for `metric` of `namespace` on exactly `dimensions`.
async fn series(
    router: &MultiServiceRouter,
    namespace: &str,
    metric: &str,
    dimensions: Value,
) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": namespace,
            "MetricName": metric,
            "Dimensions": dimensions,
            // Wide enough to hold the whole run-up a series opens with.
            "StartTime": now - 1_800.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The dimension pair that names one row's service.
fn service_dimensions(row: &Row) -> Value {
    json!([
        { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
        { "Name": SERVICE_DIMENSION, "Value": row.service },
    ])
}

/// The cluster-only dimension set — the one no service-level series carries.
fn cluster_dimensions(row: &Row) -> Value {
    json!([{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }])
}

/// Create one row's cluster with the `containerInsights` value it carries, the Fargate revision
/// its service runs, and the service itself.
async fn create_service(router: &MultiServiceRouter, row: &Row) {
    let mut cluster = json!({ "clusterName": row.cluster });
    if let Some(value) = row.setting {
        cluster["settings"] = json!([{ "name": "containerInsights", "value": value }]);
    }
    ecs(router, "CreateCluster", cluster).await;
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.service,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
    )
    .await;
    ecs(
        router,
        "CreateService",
        json!({
            "cluster": row.cluster,
            "serviceName": row.service,
            "taskDefinition": row.service,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [SUBNET],
                    "securityGroups": [],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
}

/// Wait until `DescribeServices` reports the row's service holding its one task in the `RUNNING`
/// state — the state its utilization is measured in.
async fn converged(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            "DescribeServices",
            json!({ "cluster": row.cluster, "services": [row.service] }),
        )
        .await;
        let service = described["services"][0].clone();
        if service["runningCount"].as_i64() == Some(1) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected runningCount 1, got {service}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the row's service has published at least one datapoint of `metric` into the base
/// namespace, and answer every datapoint the series then holds.
async fn published(router: &MultiServiceRouter, row: &Row, metric: &str) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = series(router, NAMESPACE, metric, service_dimensions(row)).await;
        if !points.is_empty() {
            return points;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric} datapoints on {}, got none",
            service_dimensions(row)
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Put one alarm over `metric` on `dimensions`, named `name`, with the condition every alarm of
/// this suite carries.
async fn put_alarm(router: &MultiServiceRouter, name: &str, metric: &str, dimensions: Value) {
    cloudwatch(
        router,
        "PutMetricAlarm",
        json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": FULL_UTILIZATION,
            "ComparisonOperator": ABOVE_FULL_UTILIZATION,
        }),
    )
    .await;
}

/// `(metric name, the dimensions it carries)` for every `AWS/ECS` series CloudWatch lists for
/// one row's cluster — the selection a client discovering the cluster's metrics makes.
async fn listed_series(router: &MultiServiceRouter, row: &Row) -> BTreeSet<(String, Vec<String>)> {
    let listed = cloudwatch(
        router,
        "ListMetrics",
        json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }],
        }),
    )
    .await;
    listed["Metrics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|metric| {
            let dimensions: Vec<String> = metric["Dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|dimension| {
                    format!(
                        "{}={}",
                        dimension["Name"].as_str().unwrap_or_default(),
                        dimension["Value"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            (
                metric["MetricName"].as_str().unwrap_or_default().to_string(),
                dimensions,
            )
        })
        .collect()
}

/// RULE: the service's own utilization is published on the cluster-and-service pair in `Percent`
/// whatever the cluster's `containerInsights` setting says, and under no other dimension set of
/// that namespace.
#[tokio::test]
async fn a_services_utilization_is_published_per_service_whatever_the_insights_setting() {
    let router = emulator();
    // Every row's service is created up front, so the clock-owed placements of all of them fall
    // due at once rather than one row's wait after another's.
    for row in ROWS {
        create_service(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        converged(&router, row).await;

        for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
            let points = published(&router, row, metric).await;
            for point in &points {
                assert_eq!(
                    point["Unit"].as_str(),
                    Some(PERCENT),
                    "case {case:?}: expected {metric} in {PERCENT}, got {point}"
                );
                // A utilization is a percentage of what the task definition reserves, so no
                // datapoint of either metric is negative or past 100.
                let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Minimum for {metric}, got {point}")
                });
                let maximum = point["Maximum"].as_f64().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Maximum for {metric}, got {point}")
                });
                assert!(
                    (0.0..=FULL_UTILIZATION).contains(&minimum)
                        && (0.0..=FULL_UTILIZATION).contains(&maximum)
                        && minimum <= maximum,
                    "case {case:?}: expected {metric} within 0..{FULL_UTILIZATION}, got {point}"
                );
                assert!(
                    point["SampleCount"].as_f64().unwrap_or_default() >= 1.0,
                    "case {case:?}: expected {metric} to report its observations, got {point}"
                );
            }
        }

        // The cluster setting decides the OTHER namespace, and it really took effect: a cluster
        // Container Insights is on for carries its cluster-level series, and one it is off for
        // carries none at all. So the emptiness asserted below is the emptiness of the base
        // namespace's cluster-only dimension set, not of a cluster nothing published for.
        let insights = {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let points = series(
                    &router,
                    CONTAINER_INSIGHTS_NAMESPACE,
                    TASK_COUNT,
                    cluster_dimensions(row),
                )
                .await;
                if !row.insights_on || !points.is_empty() || Instant::now() >= deadline {
                    break points;
                }
                ::tokio::time::sleep(POLL_INTERVAL).await;
            }
        };
        assert_eq!(
            !insights.is_empty(),
            row.insights_on,
            "case {case:?}: expected the {CONTAINER_INSIGHTS_NAMESPACE} {TASK_COUNT} series to \
             be present == {}, got {insights:?}",
            row.insights_on
        );

        // The base namespace carries no series on the CLUSTER alone for this service — asserted
        // over a WINDOW, because a datapoint the clock owed would arrive a moment later.
        for _ in 0..QUIET_LOOKS {
            ::tokio::time::sleep(POLL_INTERVAL).await;
            for metric in [CPU_UTILIZATION, MEMORY_UTILIZATION] {
                let points = series(&router, NAMESPACE, metric, cluster_dimensions(row)).await;
                assert!(
                    points.is_empty(),
                    "case {case:?}: expected no {metric} datapoint on {CLUSTER_DIMENSION} alone, \
                     got {points:?}"
                );
            }
        }

        // ...and that is exactly what a client selecting the cluster's metrics of the namespace
        // finds: the two metric names, each under the one dimension set naming this service, and
        // nothing else.
        let expected: BTreeSet<(String, Vec<String>)> = [CPU_UTILIZATION, MEMORY_UTILIZATION]
            .into_iter()
            .map(|metric| {
                (
                    metric.to_string(),
                    vec![
                        format!("{CLUSTER_DIMENSION}={}", row.cluster),
                        format!("{SERVICE_DIMENSION}={}", row.service),
                    ],
                )
            })
            .collect();
        let held = listed_series(&router, row).await;
        assert_eq!(
            held, expected,
            "case {case:?}: expected the cluster's {NAMESPACE} series to be exactly the two \
             per-service ones, got {held:?}"
        );
    }
}

/// RULE: over the same published data, the alarm on the cluster-and-service pair reaches the
/// state its datapoints imply while the `ClusterName`-only twin stays `INSUFFICIENT_DATA` —
/// whatever the cluster's `containerInsights` setting says, and whichever of the two metrics the
/// pair of alarms watches.
#[tokio::test]
async fn only_the_per_service_alarm_leaves_insufficient_data_for_any_insights_setting() {
    let router = emulator();
    for row in ROWS {
        create_service(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        let metric = row.alarm_metric;
        converged(&router, row).await;
        published(&router, row, metric).await;

        let per_service = format!("{}-per-service", row.cluster);
        let per_cluster = format!("{}-per-cluster", row.cluster);
        put_alarm(&router, &per_service, metric, service_dimensions(row)).await;
        put_alarm(&router, &per_cluster, metric, cluster_dimensions(row)).await;

        // "the alarm is then evaluated and its state is set appropriately" (`PutMetricAlarm`):
        // the alarm over the series that receives data leaves INSUFFICIENT_DATA for the state
        // its datapoints imply — no percentage is above full utilization, so OK.
        let deadline = Instant::now() + DEADLINE;
        loop {
            let state = alarm_state(&router, &per_service).await;
            if state == OK {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the {metric} alarm on the service pair to reach {OK}, \
                 got {state:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
        // ...and the one over the dimension set nothing publishes cannot leave it at all.
        for _ in 0..QUIET_LOOKS {
            ::tokio::time::sleep(POLL_INTERVAL).await;
            let state = alarm_state(&router, &per_cluster).await;
            assert_eq!(
                state, INSUFFICIENT_DATA,
                "case {case:?}: expected the {metric} {CLUSTER_DIMENSION}-only twin to stay \
                 {INSUFFICIENT_DATA}, got {state:?}"
            );
        }
    }
}
