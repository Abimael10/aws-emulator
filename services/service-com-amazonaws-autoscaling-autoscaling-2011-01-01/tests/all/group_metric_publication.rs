// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a group's metrics collection publishes.
//!
//! The rule under test: a group that collects a group metric AGGREGATES it into the
//! CloudWatch namespace Amazon EC2 Auto Scaling owns (`AWS/AutoScaling`), dimensioned by the
//! group's own `AutoScalingGroupName`, as a one-minute series carrying no unit, whose value is
//! computed from the group's own state — a `*Capacity` metric counting the CAPACITY UNITS its
//! instances contribute ("you can specify the number of capacity units provided by each
//! instance type … the capacity units count toward the desired capacity",
//! `LaunchTemplateOverrides$WeightedCapacity`) while its `*Instances` twin counts the
//! INSTANCES themselves — and exactly the metrics the group collects are published, so
//! `ListMetrics` over that namespace indexes the collected subset and nothing else.
//!
//! Three subjects, each a table:
//!
//! 1. The two families against the WEIGHT the group states for its instance type: the rows
//!    span an unweighted group, three weights, and a capacity the weights overshoot, so what
//!    is pinned is the ratio the families stand in rather than one recorded pair of numbers.
//! 2. The WHOLE catalog, read from the service's own `METRIC_COLLECTION_TYPES` rather than
//!    written out here: a group collecting every metric publishes one series per name and each
//!    reads the value its own family implies — including the warm pool and retained-capacity
//!    names no part of subject 1 touches.
//! 3. The collected SUBSET: two groups collecting different subsets each index exactly their
//!    own, and a metric a later `DisableMetricsCollection` takes out of a collection stops
//!    being published while the group keeps publishing the metrics it still collects.
//! 4. The TERMINATING family and the TOTAL it is part of, on a group whose termination
//!    lifecycle hook holds the instance a scale-in takes: a state the group really stays in,
//!    rather than one the clock is about to retire.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, json_rpc, p, query, tag_value, tag_values, REGION};

/// The launch template every group in this suite launches from.
const LAUNCH_TEMPLATE_NAME: &str = "lt-group-metrics";

/// The instance type the groups here weight: one type, so a row's weight is the only thing
/// that decides what an instance contributes.
const INSTANCE_TYPE: &str = "t3.micro";

/// The `LifecycleState` of an instance that is in service, spelled as the model's own
/// `LifecycleState` enum spells it.
const IN_SERVICE: &str = "InService";

/// The signing name a client scopes a CloudWatch request to.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `x-amz-target` prefix CloudWatch's awsJson protocol names its operations under.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The awsJson version a CloudWatch client sends.
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// The namespace and dimension the series are read under, taken from the service's own
/// statement of them rather than written out here.
const NAMESPACE: &str = autoscaling::state::METRIC_NAMESPACE;
const GROUP_DIMENSION: &str = autoscaling::state::GROUP_NAME_DIMENSION;

/// The granularity the collection is enabled at: the only value the member admits.
const GRANULARITY: &str = "1Minute";

/// The `StandardUnit` value a datapoint of a bare count carries.
const UNIT_NONE: &str = "None";

/// How long a poll keeps looking before it gives up. Generous on purpose: every wait here is
/// for a CONDITION — an instance reaching service, a minute falling due — that nothing inside
/// the emulator sleeps for, so a loaded host may need many passes to reach it.
const POLL_DEADLINE: Duration = Duration::from_secs(150);

/// How long one poll waits before looking again. Unhurried on purpose: the condition is what
/// is waited for, so a slower look costs nothing and leaves the host to the work it is
/// waiting on.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How far back a read of the series looks, in seconds: well past the run-up a series opens
/// with, so a read finds every minute it has published.
const READ_BACK_SECS: f64 = 30.0 * 60.0;

/// One emulator with Amazon EC2 Auto Scaling, Amazon EC2 and CloudWatch on ONE engine: the
/// instances a group launches are the account's own EC2 instances and the series it publishes
/// are CloudWatch's own metrics, so one engine is what makes the three views one account.
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

/// The zone every group here places its instances in: the first of the Region's own catalog,
/// never written out.
fn only_zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// Create one group whose mixed instances policy overrides [`INSTANCE_TYPE`], carrying
/// `weight` when the row states one.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    weight: Option<&str>,
    min_size: i32,
    max_size: i32,
    desired: i32,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
            LAUNCH_TEMPLATE_NAME,
        ),
        p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
            INSTANCE_TYPE,
        ),
        p("MinSize", min_size.to_string()),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", only_zone()),
    ];
    if let Some(weight) = weight {
        params.push(p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.WeightedCapacity",
            weight,
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body:?}"
    );
}

/// Enable metrics collection for `group`, naming `metrics` — or every metric of the catalog
/// when the list is empty, which is what a request naming none asks for.
async fn enable_metrics(router: &MultiServiceRouter, group: &str, metrics: &[&str]) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("Granularity", GRANULARITY),
    ];
    for (index, metric) in metrics.iter().enumerate() {
        params.push(p(&format!("Metrics.member.{}", index + 1), *metric));
    }
    let (status, body) = query(router, "EnableMetricsCollection", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "EnableMetricsCollection for {group:?} must succeed, got {status} with {body:?}"
    );
}

/// Stop collecting `metrics` for `group`.
async fn disable_metrics(router: &MultiServiceRouter, group: &str, metrics: &[&str]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, metric) in metrics.iter().enumerate() {
        params.push(p(&format!("Metrics.member.{}", index + 1), *metric));
    }
    let (status, body) = query(router, "DisableMetricsCollection", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "DisableMetricsCollection for {group:?} must succeed, got {status} with {body:?}"
    );
}

/// The metrics `DescribeAutoScalingGroups` reports the group collects, in the order it reports
/// them.
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
    let enabled = tag_value(&body, "EnabledMetrics");
    tag_values(&enabled, "Metric")
}

/// Look again until the group reports `wanted` members and every one of them is in service.
///
/// The CONDITION, never a fixed wait: a launch is owed on a clock nothing sleeps for.
async fn settle(router: &MultiServiceRouter, group: &str, wanted: usize, case: &str) {
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
            "case {case:?}: DescribeAutoScalingGroups got {status} with {body:?}"
        );
        let instances = tag_value(&body, "Instances");
        let states = tag_values(&instances, "LifecycleState");
        if states.len() == wanted && states.iter().all(|state| state == IN_SERVICE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} instances in {IN_SERVICE}, the group reports \
             {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the group reports exactly the lifecycle states `wanted` names, in any
/// order.
///
/// The CONDITION, never a fixed wait: every lifecycle move this service makes is owed on a
/// clock nothing sleeps for.
async fn settle_states(router: &MultiServiceRouter, group: &str, wanted: &[&str], case: &str) {
    let mut expected: Vec<String> = wanted.iter().map(|state| (*state).to_string()).collect();
    expected.sort();
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
            "case {case:?}: DescribeAutoScalingGroups got {status} with {body:?}"
        );
        let instances = tag_value(&body, "Instances");
        let mut states = tag_values(&instances, "LifecycleState");
        states.sort();
        if states == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the states {expected:?}, the group reports {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Put one lifecycle hook on the group, which must be stored.
async fn put_lifecycle_hook(
    router: &MultiServiceRouter,
    group: &str,
    hook: &str,
    transition: &str,
    case: &str,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleTransition", transition),
        // The shortest timeout the member admits is far longer than this suite runs, so the
        // hold the hook takes is what the group is waiting on throughout.
        p("HeartbeatTimeout", "300"),
    ];
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the hook {hook:?} must be stored, got {status} with {body:?}"
    );
}

/// Ask the group for `desired` capacity, which must be accepted.
async fn set_desired_capacity(router: &MultiServiceRouter, group: &str, desired: i32, case: &str) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity {desired} must be accepted, got {status} with \
         {body:?}"
    );
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

/// Now, as the epoch seconds a CloudWatch request states an instant in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// One datapoint of one series, as `GetMetricStatistics` reports it.
#[derive(Debug, Clone, PartialEq)]
struct Point {
    maximum: f64,
    minimum: f64,
    average: f64,
    sample_count: f64,
    unit: String,
}

/// The series `metric` of `group` holds, keyed by the whole minute each datapoint is
/// aggregated at — which is what makes "these two metrics agree at ONE time stamp" readable.
async fn series(router: &MultiServiceRouter, group: &str, metric: &str) -> BTreeMap<i64, Point> {
    let end = now_secs();
    let answer = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": GROUP_DIMENSION, "Value": group}],
            "StartTime": end - READ_BACK_SECS,
            "EndTime": end,
            "Period": 60,
            "Statistics": ["Maximum", "Minimum", "Average", "SampleCount"],
        }),
    )
    .await;
    answer
        .get("Datapoints")
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .filter_map(|point| {
                    let number = |member: &str| point.get(member).and_then(Value::as_f64);
                    #[allow(clippy::cast_possible_truncation)]
                    let stamp = number("Timestamp")? as i64;
                    Some((
                        stamp,
                        Point {
                            maximum: number("Maximum")?,
                            minimum: number("Minimum")?,
                            average: number("Average")?,
                            sample_count: number("SampleCount")?,
                            unit: point
                                .get("Unit")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The metric names CloudWatch indexes for `group` in this service's namespace, sorted.
async fn indexed_metrics(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let answer = cloudwatch(
        router,
        "ListMetrics",
        json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{"Name": GROUP_DIMENSION, "Value": group}],
        }),
    )
    .await;
    let mut names: Vec<String> = answer
        .get("Metrics")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics
                .iter()
                .filter_map(|metric| metric.get("MetricName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether two readings of a count are the same number.
///
/// The values are whole counts of instances and of capacity units carried as doubles, so they
/// are compared with a tolerance rather than bit for bit.
fn same(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

/// Look again until ONE whole minute carries every `expected` metric at the value it names,
/// and answer that minute's stamp.
///
/// Each metric is read on its own, exactly as a client reads them, and the minute is one the
/// readings SHARE: that is what makes "the capacity is the weight times the instance count at
/// a single time stamp" an assertion about one observation of the group rather than about two
/// readings taken at different instants.
async fn shared_minute(
    router: &MultiServiceRouter,
    group: &str,
    case: &str,
    expected: &[(&str, f64)],
) -> i64 {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let mut read: Vec<(&str, f64, BTreeMap<i64, Point>)> = Vec::new();
        for (metric, value) in expected {
            read.push((metric, *value, series(router, group, metric).await));
        }
        let candidates: Vec<i64> = read
            .first()
            .map(|(_, _, points)| points.keys().copied().collect())
            .unwrap_or_default();
        let satisfying = candidates.into_iter().find(|stamp| {
            read.iter().all(|(_, value, points)| {
                points.get(stamp).is_some_and(|point| {
                    same(point.maximum, *value)
                        && same(point.minimum, *value)
                        && same(point.average, *value)
                        // One observation per minute: a group metric is a reading of the
                        // group, not an aggregate of several, so a blended minute would be a
                        // different answer from any of them.
                        && same(point.sample_count, 1.0)
                        && point.unit == UNIT_NONE
                })
            })
        });
        if let Some(stamp) = satisfying {
            return stamp;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no single minute carried {expected:?}; the series read {read:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the series `metric` of `group` holds at least one datapoint, and answer
/// how many it holds.
async fn published_count(
    router: &MultiServiceRouter,
    group: &str,
    metric: &str,
    case: &str,
) -> usize {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let points = series(router, group, metric).await;
        if !points.is_empty() {
            return points.len();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the series {metric:?} of {group:?} published no datapoint"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the weighting table: the weight the group's single override carries, the
/// capacity it is asked for in those units, and how many instances fulfil it.
struct WeightRow {
    case: &'static str,
    /// `LaunchTemplateOverrides$WeightedCapacity`, or `None` for an override that states none
    /// — an unweighted group, whose instances are worth one unit each.
    weight: Option<&'static str>,
    /// The capacity asked for, in the units the group's size parameters are stated in.
    desired: i32,
    /// The instances that fulfil it: the fewest whose weights reach the capacity asked for.
    instances: i32,
    /// The capacity those instances provide, which overshoots the capacity asked for when the
    /// weight does not divide it ("if there are two units remaining to fulfill capacity, and
    /// Amazon EC2 Auto Scaling can only launch an instance with a WeightedCapacity of five
    /// units, the instance is launched, and the desired capacity is exceeded by three units",
    /// `LaunchTemplateOverrides$WeightedCapacity`).
    capacity: i32,
}

/// The six metrics the weighting table reads: three capacity series and two instance series,
/// plus the pending capacity a settled group has none of.
fn weight_table() -> Vec<WeightRow> {
    vec![
        WeightRow {
            case: "an unweighted group: the two families read alike",
            weight: None,
            desired: 2,
            instances: 2,
            capacity: 2,
        },
        WeightRow {
            case: "a weight of two: the capacity is twice the instance count",
            weight: Some("2"),
            desired: 4,
            instances: 2,
            capacity: 4,
        },
        WeightRow {
            case: "a weight of three",
            weight: Some("3"),
            desired: 6,
            instances: 2,
            capacity: 6,
        },
        WeightRow {
            case: "a weight that covers the whole capacity with one instance",
            weight: Some("4"),
            desired: 4,
            instances: 1,
            capacity: 4,
        },
        WeightRow {
            case: "a weight that does not divide the capacity asked for",
            weight: Some("2"),
            desired: 3,
            instances: 2,
            capacity: 4,
        },
    ]
}

/// SUBJECT 1: at one minute of the series, the capacity family counts the weighted units the
/// group's instances contribute while the instance family counts the instances.
#[tokio::test]
async fn the_capacity_family_counts_weighted_units_and_the_instance_family_counts_instances() {
    let router = emulator();

    for (index, row) in weight_table().into_iter().enumerate() {
        let case = row.case;
        let group = format!("asg-weighted-{index}");
        create_group(
            &router,
            &group,
            row.weight,
            0,
            row.capacity.max(row.desired),
            row.desired,
        )
        .await;
        let wanted = usize::try_from(row.instances).expect("a row's instance count is positive");
        settle(&router, &group, wanted, case).await;

        let metrics = [
            "GroupDesiredCapacity",
            "GroupInServiceCapacity",
            "GroupInServiceInstances",
            "GroupPendingCapacity",
            "GroupTotalCapacity",
            "GroupTotalInstances",
        ];
        enable_metrics(&router, &group, &metrics).await;

        let expected = [
            ("GroupDesiredCapacity", f64::from(row.desired)),
            ("GroupInServiceCapacity", f64::from(row.capacity)),
            ("GroupInServiceInstances", f64::from(row.instances)),
            // A settled group is launching nothing, so its pending family is empty.
            ("GroupPendingCapacity", 0.0),
            ("GroupTotalCapacity", f64::from(row.capacity)),
            ("GroupTotalInstances", f64::from(row.instances)),
        ];
        let stamp = shared_minute(&router, &group, case, &expected).await;
        assert_eq!(
            stamp % 60,
            0,
            "case {case:?}: a one-minute datapoint is stamped with the start of its minute, \
             got {stamp}"
        );

        // The RATIO, read off the same minute: the weight the group states for its instance
        // type is exactly what separates the two families.
        let in_service_capacity = series(&router, &group, "GroupInServiceCapacity").await;
        let in_service_instances = series(&router, &group, "GroupInServiceInstances").await;
        let capacity = in_service_capacity
            .get(&stamp)
            .map(|point| point.maximum)
            .unwrap_or_default();
        let instances = in_service_instances
            .get(&stamp)
            .map(|point| point.maximum)
            .unwrap_or_default();
        let weight = f64::from(row.capacity) / f64::from(row.instances);
        assert!(
            same(capacity, instances * weight),
            "case {case:?}: at {stamp} the in-service capacity must be {weight} times the \
             in-service instance count, got {capacity} against {instances}"
        );

        // And exactly the collected subset is indexed: no series outside it exists for this
        // group.
        let mut wanted_names: Vec<String> = metrics.iter().map(|name| (*name).to_string()).collect();
        wanted_names.sort();
        assert_eq!(
            indexed_metrics(&router, &group).await,
            wanted_names,
            "case {case:?}: ListMetrics must index exactly the collected metrics"
        );
    }
}

/// SUBJECT 2: the whole catalog on one group — one series per collected metric, each reading
/// the value its own family implies.
///
/// The group is created with a capacity of one weighted-at-three instance and no warm pool, so
/// every name of the catalog has a value that follows from the group alone: the three size
/// parameters as configured, the in-service family and the total family at that one instance,
/// every other lifecycle family empty, and the warm pool's own names at the nothing a group
/// with no pool holds. The `GroupAndWarmPool*` pair is the two halves added together.
#[tokio::test]
async fn every_metric_of_the_catalog_reads_the_value_its_family_implies() {
    let router = emulator();
    let group = "asg-whole-catalog";
    let case = "every metric of the catalog";
    let (min_size, max_size, desired, weight, instances) = (3, 9, 3, 3.0, 1.0);
    create_group(&router, group, Some("3"), min_size, max_size, desired).await;
    settle(&router, group, 1, case).await;

    // A request naming no metric collects every one of them: "If you specify Granularity and
    // don't specify any metrics, all metrics are enabled" (`EnableMetricsCollection$Metrics`).
    enable_metrics(&router, group, &[]).await;
    let collected = enabled_metrics(&router, group).await;
    let mut catalog: Vec<String> = autoscaling::state::METRIC_COLLECTION_TYPES
        .iter()
        .map(|metric| (*metric).to_string())
        .collect();
    catalog.sort();
    let mut sorted_collected = collected.clone();
    sorted_collected.sort();
    assert_eq!(
        sorted_collected, catalog,
        "case {case:?}: a request naming no metric must collect the whole catalog"
    );

    let capacity = instances * weight;
    // One expectation per catalog name, each the value its family implies for this group.
    let expected: Vec<(&str, f64)> = vec![
        ("GroupMinSize", f64::from(min_size)),
        ("GroupMaxSize", f64::from(max_size)),
        ("GroupDesiredCapacity", f64::from(desired)),
        ("GroupInServiceInstances", instances),
        ("GroupInServiceCapacity", capacity),
        ("GroupPendingInstances", 0.0),
        ("GroupPendingCapacity", 0.0),
        ("GroupStandbyInstances", 0.0),
        ("GroupStandbyCapacity", 0.0),
        ("GroupTerminatingInstances", 0.0),
        ("GroupTerminatingCapacity", 0.0),
        ("GroupTerminatingRetainedInstances", 0.0),
        ("GroupTerminatingRetainedCapacity", 0.0),
        ("GroupTotalInstances", instances),
        ("GroupTotalCapacity", capacity),
        // A group with no warm pool holds no warmed instance and maintains none.
        ("WarmPoolMinSize", 0.0),
        ("WarmPoolDesiredCapacity", 0.0),
        ("WarmPoolWarmedCapacity", 0.0),
        ("WarmPoolPendingCapacity", 0.0),
        ("WarmPoolPendingRetainedCapacity", 0.0),
        ("WarmPoolTerminatingCapacity", 0.0),
        ("WarmPoolTerminatingRetainedCapacity", 0.0),
        ("WarmPoolTotalCapacity", 0.0),
        ("GroupAndWarmPoolDesiredCapacity", f64::from(desired)),
        ("GroupAndWarmPoolTotalCapacity", capacity),
    ];
    assert_eq!(
        expected.len(),
        autoscaling::state::METRIC_COLLECTION_TYPES.len(),
        "case {case:?}: the table must carry one row per metric of the catalog"
    );
    shared_minute(&router, group, case, &expected).await;

    assert_eq!(
        indexed_metrics(&router, group).await,
        catalog,
        "case {case:?}: ListMetrics must index exactly the catalog the group collects"
    );
}

/// SUBJECT 3: the series that exist are the collected subset — per group, and after a disable.
#[tokio::test]
async fn only_the_collected_subset_is_published() {
    let router = emulator();
    // Two groups collecting DIFFERENT subsets of the catalog, so neither subset can be
    // explained by the other group's collection.
    let first = ("asg-subset-desired", "GroupDesiredCapacity");
    let second = ("asg-subset-total", "GroupTotalInstances");
    for (group, metric) in [first, second] {
        create_group(&router, group, Some("2"), 0, 4, 2).await;
        settle(&router, group, 1, metric).await;
        enable_metrics(&router, group, &[metric]).await;
    }

    for (group, metric) in [first, second] {
        let case = format!("{group} collects only {metric}");
        published_count(&router, group, metric, &case).await;
        assert_eq!(
            indexed_metrics(&router, group).await,
            vec![metric.to_string()],
            "case {case:?}: ListMetrics must index exactly the one collected metric"
        );
    }

    // A second metric joins the first group's collection, and is published into its own series
    // while the metric already collected keeps its own.
    let (group, kept) = first;
    let dropped = "GroupInServiceInstances";
    enable_metrics(&router, group, &[dropped]).await;
    let case = format!("{group} collects {kept} and {dropped}");
    published_count(&router, group, dropped, &case).await;
    let mut both = vec![dropped.to_string(), kept.to_string()];
    both.sort();
    assert_eq!(
        indexed_metrics(&router, group).await,
        both,
        "case {case:?}: ListMetrics must index both collected metrics"
    );

    // The disable takes ONE metric out of the collection. What the group then publishes is the
    // metric it still collects: a THIRD metric enabled after the disable gets a series of its
    // own — which is the publisher running again — and the dropped metric's series does not
    // grow across it.
    disable_metrics(&router, group, &[dropped]).await;
    let after_disable = series(&router, group, dropped).await.len();
    let added = "GroupTotalCapacity";
    enable_metrics(&router, group, &[added]).await;
    let case = format!("{group} stopped collecting {dropped}");
    published_count(&router, group, added, &case).await;
    // And a metric enabled later lands on the SAME minute lattice as the one the group was
    // already publishing, so a reader finds the two at one time stamp.
    let kept_minutes = series(&router, group, kept).await;
    let added_minutes = series(&router, group, added).await;
    assert!(
        added_minutes
            .keys()
            .any(|stamp| kept_minutes.contains_key(stamp)),
        "case {case:?}: a metric enabled later must share a minute with the metric already \
         collected, got {added_minutes:?} against {kept_minutes:?}"
    );
    assert_eq!(
        enabled_metrics(&router, group).await,
        vec![kept.to_string(), added.to_string()],
        "case {case:?}: the group must collect exactly the metrics it was left with"
    );
    assert_eq!(
        series(&router, group, dropped).await.len(),
        after_disable,
        "case {case:?}: a metric the group no longer collects must receive no further datapoint"
    );
    let mut remaining = vec![kept.to_string(), added.to_string(), dropped.to_string()];
    remaining.sort();
    assert_eq!(
        indexed_metrics(&router, group).await,
        remaining,
        "case {case:?}: the series a disable stops keeps the datapoints it was published with, \
         so it stays indexed beside the collected ones"
    );
}

/// The `LifecycleTransition` a hook that holds an instance on its way OUT of the group names,
/// from the model's own documentation for `PutLifecycleHookType$LifecycleTransition`.
const TERMINATING_TRANSITION: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The `LifecycleState` of an instance such a hook is holding, as the model's own
/// `LifecycleState` enum spells it.
const TERMINATING_WAIT: &str = "Terminating:Wait";

/// SUBJECT 4: the TERMINATING family, and the TOTAL family it is part of.
///
/// A scale-in of a group whose termination lifecycle hook holds the instance it takes leaves
/// that instance a member of the group, waiting: so the terminating family counts it, the
/// in-service family no longer does, and the total still holds it — "the total number of
/// instances in the Auto Scaling group". Each family's capacity half counts the weighted units
/// the same instances contribute, which is what separates this row from a group whose weights
/// are one.
///
/// The hold is what makes the row an assertion rather than a race: the instance stays in
/// `Terminating:Wait` until something resolves the hook, so the minute the series publishes is
/// a state the group is REALLY in rather than one the clock is about to retire.
#[tokio::test]
async fn a_held_terminating_member_counts_in_the_terminating_family_and_in_the_total() {
    let router = emulator();
    let group = "asg-terminating";
    let case = "a member held at a termination lifecycle hook";
    // Weight two and four units asked for: two instances. Scaled in to two units, one of them
    // is taken out of service — and the hook holds it there.
    create_group(&router, group, Some("2"), 0, 4, 4).await;
    settle(&router, group, 2, case).await;
    put_lifecycle_hook(&router, group, "hold-terminations", TERMINATING_TRANSITION, case).await;
    set_desired_capacity(&router, group, 2, case).await;
    settle_states(&router, group, &[IN_SERVICE, TERMINATING_WAIT], case).await;

    let expected = [
        // The capacity asked for, in the units the group's size parameters are stated in.
        ("GroupDesiredCapacity", 2.0),
        // One instance in service, worth its own two units.
        ("GroupInServiceInstances", 1.0),
        ("GroupInServiceCapacity", 2.0),
        // One on its way out, worth two units as well.
        ("GroupTerminatingInstances", 1.0),
        ("GroupTerminatingCapacity", 2.0),
        // The group holds BOTH, so the total is the two families added together.
        ("GroupTotalInstances", 2.0),
        ("GroupTotalCapacity", 4.0),
        // And nothing is being launched or parked, nor retained instead of terminated.
        ("GroupPendingInstances", 0.0),
        ("GroupStandbyInstances", 0.0),
        ("GroupTerminatingRetainedInstances", 0.0),
    ];
    let metrics: Vec<&str> = expected.iter().map(|(metric, _)| *metric).collect();
    // Collected AFTER the scale-in, so the minutes the series opens with report the state the
    // group is in rather than the one it was in before.
    enable_metrics(&router, group, &metrics).await;
    shared_minute(&router, group, case, &expected).await;
}
