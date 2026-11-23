// SPDX-License-Identifier: CC0-1.0

//! The CONTAINER INSIGHTS metrics a cluster publishes, and the cluster setting that decides
//! whether it publishes any at all.
//!
//! The rule under test:
//!
//! > `Cluster$settings` "indicates whether CloudWatch Container Insights is on or off for a
//! > cluster" (`ClusterSetting$name` `containerInsights`), and a cluster it is ON for
//! > publishes its Container Insights metrics into CloudWatch under the namespace Amazon ECS
//! > publishes them in, dimensioned by the cluster — and by the service, for a service-level
//! > metric — while a cluster it is OFF for publishes none at all. Every value published is
//! > counted off the records the cluster holds, so it is the number `DescribeClusters` and
//! > `DescribeServices` report for the same instant.
//!
//! The rows span what the rule generalises over: the three values the setting takes
//! (`enabled`, `enhanced` and `disabled`) and a cluster that sets nothing, which inherits the
//! account setting in force — `disabled` by default, so such a cluster publishes nothing
//! either.
//!
//! Nothing here reaches into a state struct: the metrics are read back through CloudWatch's
//! OWN `GetMetricStatistics` and `ListMetrics`, which is the same store a client's
//! `PutMetricData` writes into, and the counts they are compared against are read back
//! through `DescribeClusters`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cloudwatch, count, emulator_with_cloudwatch, list, ok, register_capacity, string,
};

/// The namespace Amazon ECS publishes Container Insights metrics in, and the two metric
/// names this suite reads — spelled out here rather than imported, so a wrong constant in
/// the implementation cannot make a test pass.
const NAMESPACE: &str = "ECS/ContainerInsights";
const TASK_COUNT: &str = "TaskCount";
const SERVICE_COUNT: &str = "ServiceCount";

/// The dimension every Container Insights metric of a cluster carries.
const CLUSTER_DIMENSION: &str = "ClusterName";

/// The image every container below names, and the program it runs: one that stays up, so a
/// task that is running is running while the series is read.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// How often a poll re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: the series is published as the
/// clock moves the task it counts, this suite runs beside every other test of the workspace,
/// and every poll stops the instant its own condition holds.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// How many further looks a row that expects NO series takes at the namespace. Together with
/// [`POLL_INTERVAL`] a window far longer than the spacing a published series takes, so an
/// emptiness a row asserts is one that HELD rather than one read too early.
const QUIET_LOOKS: usize = 30;

/// One row: the `containerInsights` value the cluster is created with, and whether that
/// cluster publishes.
struct Row {
    case: &'static str,
    cluster: &'static str,
    /// The setting the create names, and `None` for a cluster that sets nothing.
    setting: Option<&'static str>,
    /// Whether Container Insights is ON for it.
    publishes: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cluster Container Insights is enabled for",
        cluster: "insights-enabled",
        setting: Some("enabled"),
        publishes: true,
    },
    Row {
        case: "a cluster Container Insights is enabled for with enhanced observability",
        cluster: "insights-enhanced",
        setting: Some("enhanced"),
        publishes: true,
    },
    Row {
        case: "a cluster Container Insights is disabled for",
        cluster: "insights-disabled",
        setting: Some("disabled"),
        publishes: false,
    },
    Row {
        case: "a cluster that sets nothing, so the account setting decides",
        cluster: "insights-default",
        setting: None,
        publishes: false,
    },
];

/// An emulator with Amazon ECS and CloudWatch — which owns the metric store the series is
/// published into — on ONE shared engine ([`crate::ecs_wire::emulator_with_cloudwatch`]).
fn emulator() -> MultiServiceRouter {
    emulator_with_cloudwatch()
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for one cluster's `metric`.
async fn datapoints(router: &MultiServiceRouter, cluster: &str, metric: &str) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{ "Name": CLUSTER_DIMENSION, "Value": cluster }],
            "StartTime": now - 600.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Maximum", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The largest value any datapoint of one cluster's `metric` reports, and `None` when the
/// series holds no datapoint at all.
async fn highest(router: &MultiServiceRouter, cluster: &str, metric: &str) -> Option<f64> {
    datapoints(router, cluster, metric)
        .await
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Create the cluster one row asks for, through this service's own API.
async fn create_cluster(router: &MultiServiceRouter, row: &Row) {
    let mut request = json!({ "clusterName": row.cluster });
    if let Some(value) = row.setting {
        request["settings"] = json!([{ "name": "containerInsights", "value": value }]);
    }
    ok(router, "CreateCluster", request, row.case).await;
}

/// Create one SERVICE of `family` in one row's cluster, from a revision that stays up.
///
/// A service rather than a standalone task: "the service scheduler ... replaces tasks that
/// fail or stop" (`CreateService`), so the task it places keeps running for as long as the
/// row reads, and the count the series carries is a count that holds still while it is
/// compared with the cluster's own ([`describe_cluster`]) — where a standalone task would
/// have ended when the work it was placed to do was over.
async fn create_service(router: &MultiServiceRouter, row: &Row, family: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
        row.case,
    )
    .await;
    ok(
        router,
        "CreateService",
        json!({
            "cluster": row.cluster,
            "serviceName": family,
            "taskDefinition": family,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        row.case,
    )
    .await;
}

/// The cluster as `DescribeClusters` reports it.
async fn describe_cluster(router: &MultiServiceRouter, row: &Row) -> Value {
    let body = ok(
        router,
        "DescribeClusters",
        json!({ "clusters": [row.cluster] }),
        row.case,
    )
    .await;
    list(&body, "clusters", row.case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {:?}: expected the cluster back", row.case))
}

/// RULE: a cluster Container Insights is on for publishes the counts it holds; one it is off
/// for publishes nothing.
#[tokio::test]
async fn container_insights_publishes_a_clusters_counts_only_when_it_is_on() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        create_cluster(&router, row).await;
        // The capacity the service's task is placed on, and a service that keeps one task
        // running on it, so the counts the series carries are counts of something.
        register_capacity(&router, row.cluster, row.case).await;
        create_service(&router, row, &format!("insights{index}")).await;
    }

    for row in ROWS {
        let case = row.case;
        if !row.publishes {
            // A cluster the setting is OFF for publishes nothing at all — asserted over a
            // WINDOW, because a datapoint the clock owes would arrive a moment later.
            for _ in 0..QUIET_LOOKS {
                tokio::time::sleep(POLL_INTERVAL).await;
                for metric in [TASK_COUNT, SERVICE_COUNT] {
                    let points = datapoints(&router, row.cluster, metric).await;
                    assert!(
                        points.is_empty(),
                        "case {case:?}: expected no {metric} datapoint, got {points:?}"
                    );
                }
            }
            continue;
        }
        // A cluster the setting is ON for publishes the count of its own running tasks: the
        // series reaches the task the row's service placed, and that count then HOLDS — the
        // service's scheduler keeps the task in place — so the comparison below is against a
        // count that is not moving under it.
        let deadline = Instant::now() + POLL_BUDGET;
        let reported = loop {
            let reported = highest(&router, row.cluster, TASK_COUNT).await;
            if reported == Some(1.0) {
                break reported;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the {TASK_COUNT} series to reach 1, got {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        // ...and it is the very count the cluster itself reports for the same records.
        let cluster = describe_cluster(&router, row).await;
        assert_eq!(
            reported,
            Some(count(&cluster, "runningTasksCount", case) as f64),
            "case {case:?}: the published count is the cluster's own, got {cluster}"
        );
        assert_eq!(
            string(&cluster, "clusterName", case),
            row.cluster,
            "case {case:?}: expected the cluster back, got {cluster}"
        );
        // Every metric of the namespace is dimensioned by the cluster it counts, which is
        // what a client selects a series by.
        let listed = cloudwatch(
            &router,
            "ListMetrics",
            json!({
                "Namespace": NAMESPACE,
                "Dimensions": [{ "Name": CLUSTER_DIMENSION, "Value": row.cluster }],
            }),
        )
        .await;
        let names: Vec<String> = listed["Metrics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|metric| metric["MetricName"].as_str().map(str::to_string))
            .collect();
        for metric in [TASK_COUNT, SERVICE_COUNT] {
            assert!(
                names.iter().any(|held| held == metric),
                "case {case:?}: expected {metric} among the cluster's metrics, got {names:?}"
            );
        }
    }
}
