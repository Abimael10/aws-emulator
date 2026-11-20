// SPDX-License-Identifier: CC0-1.0

//! The zero end of a group's CloudWatch series: a group that collects NO metric.
//!
//! RULE: the `AWS/AutoScaling` series that exist for a group are exactly the metrics that
//! group's metrics collection names — "Enables group metrics collection for the specified Auto
//! Scaling group" (`smithy_query(query="doc",shape="EnableMetricsCollection")`) is what arms
//! them — so a group whose collection is empty has NO series at all, however many instances it
//! holds and whatever another group of the same account is publishing, and the moment that same
//! group collects one metric it has exactly that one.
//!
//! The suite that owns the published values spans the metrics a collection NAMES
//! ([`crate::group_metric_publication`]: one weight per row, the whole catalog, two different
//! subsets, a disable). Every group of every row there collects something, so the empty
//! collection — the one case in which the answer is "no series" rather than a value — is pinned
//! here: the two rows are the same group before and after its first enable, read beside a group
//! that is publishing throughout, so neither answer can be explained by nothing having been
//! published at all.
//!
//! Every wait here looks again until the condition holds, never for a span: a datapoint is owed
//! on a clock nothing sleeps for.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, json_rpc, p, query, tag_value, tag_values, REGION};

/// The launch template both groups here launch from.
const LAUNCH_TEMPLATE_NAME: &str = "lt-uncollected-metrics";

/// The instance type their mixed instances policy weights.
const INSTANCE_TYPE: &str = "t3.micro";

/// The weight one member of either group contributes, so the group's sizes are in those units.
const WEIGHT: &str = "2";

/// The namespace and the dimension the series are read under, taken from the service's own
/// statement of them rather than written out here.
const NAMESPACE: &str = autoscaling::state::METRIC_NAMESPACE;
const GROUP_DIMENSION: &str = autoscaling::state::GROUP_NAME_DIMENSION;

/// The granularity a collection is enabled at: the only value the member admits.
const GRANULARITY: &str = "1Minute";

/// The `LifecycleState` of an instance that is in service, spelled as the model's own
/// `LifecycleState` enum spells it.
const IN_SERVICE: &str = "InService";

/// The signing name a client scopes a CloudWatch request to.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `x-amz-target` prefix CloudWatch's awsJson protocol names its operations under.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The awsJson version a CloudWatch client sends.
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// How long a poll keeps looking before it gives up. Generous on purpose: the condition is what
/// is waited for, never a span.
const POLL_DEADLINE: Duration = Duration::from_secs(150);

/// How long one poll waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One emulator with Amazon EC2 Auto Scaling, Amazon EC2 and CloudWatch on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::support::seed_launch_templates(&engine, &[LAUNCH_TEMPLATE_NAME]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// The zone both groups place their instances in: the first of the Region's own catalog.
fn only_zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// Create one weighted group holding `desired` capacity units.
async fn create_group(router: &MultiServiceRouter, name: &str, desired: i32) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
            LAUNCH_TEMPLATE_NAME,
        ),
        p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
            INSTANCE_TYPE,
        ),
        p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.WeightedCapacity",
            WEIGHT,
        ),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", only_zone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body:?}"
    );
}

/// Enable the collection of `metric` for `group`.
async fn enable_metric(router: &MultiServiceRouter, group: &str, metric: &str) {
    let (status, body) = query(
        router,
        "EnableMetricsCollection",
        &[
            ("AutoScalingGroupName", group),
            ("Granularity", GRANULARITY),
            ("Metrics.member.1", metric),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "EnableMetricsCollection for {group:?} must succeed, got {status} with {body:?}"
    );
}

/// The metrics `DescribeAutoScalingGroups` reports `group` collects.
async fn enabled_metrics(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} got {status} with {body:?}"
    );
    tag_values(&tag_value(&body, "EnabledMetrics"), "Metric")
}

/// Look again until `group` reports `wanted` members, every one of them in service.
async fn settle(router: &MultiServiceRouter, group: &str, wanted: usize) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeAutoScalingGroups for {group:?} got {status} with {body:?}"
        );
        let states = tag_values(&tag_value(&body, "Instances"), "LifecycleState");
        if states.len() == wanted && states.iter().all(|state| state == IN_SERVICE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {wanted} instance(s) of {group:?} in {IN_SERVICE}, it reports {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// POST one CloudWatch operation and answer its body, asserting it succeeded.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answer) = json_rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        CLOUDWATCH_JSON_VERSION,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {answer}"
    );
    answer
}

/// Every series CloudWatch indexes in this service's namespace, as
/// `(metric name, the value of its group dimension)` pairs, sorted.
///
/// The whole namespace rather than one group's slice, so a group with no series is read from the
/// same answer the publishing group's series are in: that is what makes "nothing for this group"
/// an assertion about the group rather than about the read.
async fn indexed_series(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let answer = cloudwatch(router, "ListMetrics", json!({"Namespace": NAMESPACE})).await;
    let mut series: Vec<(String, String)> = answer
        .get("Metrics")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics
                .iter()
                .map(|metric| {
                    let name = metric
                        .get("MetricName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let group = metric
                        .get("Dimensions")
                        .and_then(Value::as_array)
                        .map(|dimensions| {
                            dimensions
                                .iter()
                                .filter(|dimension| {
                                    dimension.get("Name").and_then(Value::as_str)
                                        == Some(GROUP_DIMENSION)
                                })
                                .filter_map(|dimension| {
                                    dimension.get("Value").and_then(Value::as_str)
                                })
                                .map(str::to_string)
                                .collect::<Vec<String>>()
                                .join(",")
                        })
                        .unwrap_or_default();
                    (name, group)
                })
                .collect()
        })
        .unwrap_or_default();
    series.sort();
    series
}

/// How many datapoints CloudWatch holds for one series of `group`.
async fn datapoint_count(router: &MultiServiceRouter, group: &str, metric: &str) -> usize {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_secs_f64();
    let answer = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": GROUP_DIMENSION, "Value": group}],
            // Wide enough at both ends to hold every minute either group can have published.
            "StartTime": now - 30.0 * 60.0,
            "EndTime": now + 10.0 * 60.0,
            "Period": 60,
            "Statistics": ["Average", "SampleCount"],
        }),
    )
    .await;
    answer
        .get("Datapoints")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// Look again until `group` has published at least one datapoint of `metric`.
async fn published(router: &MultiServiceRouter, group: &str, metric: &str, case: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if datapoint_count(router, group, metric).await > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the series {metric:?} of {group:?} published no datapoint"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a group that collects nothing publishes nothing — and the same group publishes exactly
/// the one metric it is then asked to collect.
#[tokio::test]
async fn a_group_that_collects_no_metric_has_no_series_at_all() {
    let router = emulator();
    let publishing = "asg-collecting";
    let quiet = "asg-not-collecting";
    let metric = "GroupInServiceCapacity";

    // Two groups of one account, each holding its own instances; only one of them collects.
    for group in [publishing, quiet] {
        create_group(&router, group, 2).await;
        settle(&router, group, 1).await;
    }
    enable_metric(&router, publishing, metric).await;
    published(&router, publishing, metric, "one group collects").await;

    // ROW 1: the group that collects nothing reports no collection, and the namespace holds no
    // series about it — while the other group's series are in the very same answer.
    let enabled = enabled_metrics(&router, quiet).await;
    assert!(
        enabled.is_empty(),
        "a group nothing enabled a metric on must report no collection, got {enabled:?}"
    );
    let indexed = indexed_series(&router).await;
    assert_eq!(
        indexed,
        vec![(metric.to_string(), publishing.to_string())],
        "only the collecting group's one metric may be indexed, got {indexed:?}"
    );
    assert_eq!(
        datapoint_count(&router, quiet, metric).await,
        0,
        "a read of the uncollected series of {quiet:?} must find no datapoint, got {indexed:?}"
    );

    // ROW 2: the same group, now collecting ONE metric — so row 1 is the empty collection and
    // not a publisher that never runs for this group.
    let joined = "GroupTotalCapacity";
    enable_metric(&router, quiet, joined).await;
    published(&router, quiet, joined, "the quiet group collects one metric").await;
    assert_eq!(
        enabled_metrics(&router, quiet).await,
        vec![joined.to_string()],
        "the group must collect exactly the metric it was enabled with"
    );
    let indexed = indexed_series(&router).await;
    let mut expected = vec![
        (metric.to_string(), publishing.to_string()),
        (joined.to_string(), quiet.to_string()),
    ];
    expected.sort();
    assert_eq!(
        indexed, expected,
        "each group must be indexed for exactly the metrics it collects, got {indexed:?}"
    );
}
