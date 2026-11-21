// SPDX-License-Identifier: CC0-1.0

//! The fleet a WEIGHTED Auto Scaling group converges on — at the capacity it is created with
//! and at every capacity it is given afterwards — and the `AWS/AutoScaling` series that fleet
//! publishes.
//!
//! THE RULE: a group whose launch overrides state a weight has its three size parameters in
//! those capacity units ("if you configure a mixed instances policy that defines weights for
//! the instance types, you must specify these sizes with the same units that you use for
//! weighting instances",
//! `smithy_query(query="doc",shape="LaunchTemplateOverrides",member="WeightedCapacity")`), so
//! EVERY capacity it holds — the one it was configured with and the one each later capacity
//! change asks for — is fulfilled by the FEWEST instances whose weights reach it: "Amazon EC2
//! Auto Scaling launches instances until the desired capacity is totally fulfilled, even if
//! this results in an overage" (same member), and a scale-in never takes the capacity back
//! below what is asked for. Those instances reach `InService` and each reports the weight its
//! type is counted at, while the series the group publishes count the two families apart — a
//! `*Capacity` metric in capacity units, its `*Instances` twin in instances.
//!
//! The rows span what the rule generalises over: the WEIGHT an override states (an override
//! that states none, and three weights), the capacity asked for (a capacity the weight
//! divides, one it overshoots, and one below a single instance's weight), and the DIRECTION of
//! each change (the capacity the group is created with, a scale-out and a scale-in, in the
//! group's own units rather than in instances). Each row runs on its own emulator and asserts
//! after every step, so a group that converged once but not after a change fails the row.
//!
//! The sibling suite `group_metric_publication.rs` owns the catalog and the collected subset;
//! what is under test here is the CONVERGENCE behind those series — a weighted fleet that
//! reaches service, and reaches it again at each capacity it is moved to.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;

use crate::support::{
    borrowed, create_launch_template, json_rpc, p, query, seed_subnets, tag_value, tag_values,
};

/// The instance type every override of this suite names: one type, so a row's weight is the
/// only thing that decides what one instance contributes.
const INSTANCE_TYPE: &str = "t3.micro";

/// The `LifecycleState` of an instance that is in service, spelled as the model's
/// `LifecycleState` enum spells it.
const IN_SERVICE: &str = "InService";

/// The ceiling every group here is created with: far above every capacity the rows ask for, so
/// the only bounds a row exercises are its own capacities and the floor of zero.
const MAX_SIZE: i32 = 12;

/// The granularity a collection is enabled at: the only value the member admits
/// (`EnableMetricsCollectionQuery$Granularity`).
const GRANULARITY: &str = "1Minute";

/// The namespace and the dimension the series are read under, taken from the service's own
/// statement of them rather than written out here.
const NAMESPACE: &str = autoscaling::state::METRIC_NAMESPACE;
const GROUP_DIMENSION: &str = autoscaling::state::GROUP_NAME_DIMENSION;

/// The `StandardUnit` a datapoint of a bare count carries.
const UNIT_NONE: &str = "None";

/// The signing name, target prefix and awsJson version a CloudWatch client sends.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// How far back a read of a series looks, in seconds: well past the run-up a series opens
/// with, so a read finds every minute it has published.
const READ_BACK_SECS: f64 = 30.0 * 60.0;

/// How long a poll keeps looking before it gives up, and how long it waits between looks.
///
/// Generous on purpose: every wait here is for a CONDITION — an instance reaching service, a
/// minute of a series falling due — so machine load decides when a row finishes and never
/// whether it passes.
const POLL_DEADLINE: Duration = Duration::from_secs(150);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// One row of the weighting table.
struct Row {
    case: &'static str,
    /// The `WeightedCapacity` the group's single override carries, or `None` for an override
    /// that states none — an unweighted group, whose instances are worth one unit each.
    weight: Option<&'static str>,
    /// The capacities the group is given, in the units its size parameters are stated in: the
    /// one it is created with, then the one each later `SetDesiredCapacity` asks for.
    capacities: &'static [i32],
}

/// ROW 1: an override that states NO weight — the capacity is an instance count, which is what
/// the rule reads as a weight of one.
#[tokio::test]
async fn an_unweighted_group_fulfils_each_capacity_in_instances() {
    weighted_capacity_row(
        1,
        Row {
            case: "an override that states no weight",
            weight: None,
            capacities: &[2, 3, 1],
        },
    )
    .await;
}

/// ROW 2: a weight of two, with every capacity a multiple of it — the fleet is half the
/// capacity, before and after each change.
#[tokio::test]
async fn a_weight_of_two_fulfils_each_capacity_with_half_as_many_instances() {
    weighted_capacity_row(
        2,
        Row {
            case: "a weight of two, every capacity a multiple of it",
            weight: Some("2"),
            capacities: &[4, 8, 2],
        },
    )
    .await;
}

/// ROW 3: a weight of three against capacities it does NOT divide — the fleet overshoots the
/// capacity asked for rather than falling short of it, and a scale-in that cannot be paid for
/// by a whole instance takes none.
#[tokio::test]
async fn a_weight_of_three_overshoots_a_capacity_it_does_not_divide() {
    weighted_capacity_row(
        3,
        Row {
            case: "a weight of three against capacities it does not divide",
            weight: Some("3"),
            capacities: &[6, 7, 4],
        },
    )
    .await;
}

/// ROW 4: a weight LARGER than the capacity asked for — one instance covers the whole of it,
/// and the capacity the group then provides exceeds what it was asked for by the rest of that
/// instance's weight.
#[tokio::test]
async fn a_weight_larger_than_the_capacity_asked_for_is_fulfilled_by_one_instance() {
    weighted_capacity_row(
        4,
        Row {
            case: "a weight of five, ending below one instance's weight",
            weight: Some("5"),
            capacities: &[5, 9, 1],
        },
    )
    .await;
}

/// EVERY ROW: the group reaches, at each capacity it is given, the fewest instances whose
/// weights fulfil that capacity, every one of them in service and each reporting the weight
/// its type is counted at; and the series it publishes read the capacity family in capacity
/// units and the instance family in instances at one shared minute.
async fn weighted_capacity_row(index: usize, row: Row) {
    let case = row.case;
    let (router, engine) = emulator();
    let group = format!("asg-weighted-change-{index}");
    let template = format!("lt-weighted-change-{index}");
    create_launch_template(&router, &template).await;
    // Placed by subnet, the way a client that built its own VPC places a group: the zones come
    // from Amazon EC2's own record of the subnets.
    let subnets = seed_subnets(&engine, 2);
    let placement: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();

    let weight = row_weight(&row);
    let (first, later) = row
        .capacities
        .split_first()
        .expect("a row states the capacity its group is created with");
    create_group(
        &router,
        &group,
        &template,
        row.weight,
        *first,
        &placement.join(","),
    )
    .await;
    assert_fulfilled(&router, &group, case, *first, weight).await;

    for capacity in later {
        set_desired_capacity(&router, &group, *capacity, case).await;
        assert_fulfilled(&router, &group, case, *capacity, weight).await;
    }

    // The series, read after the fleet has settled at the LAST capacity of the row: the
    // capacity family counts the units those instances contribute while the instance family
    // counts the instances, and the group is converging on nothing, so its pending family is
    // empty.
    let settled = *row
        .capacities
        .last()
        .expect("a row states at least one capacity");
    let instances = instances_fulfilling(settled, weight);
    let provided = instances * weight;
    let metrics = [
        "GroupDesiredCapacity",
        "GroupInServiceCapacity",
        "GroupInServiceInstances",
        "GroupPendingCapacity",
        "GroupPendingInstances",
        "GroupTotalCapacity",
        "GroupTotalInstances",
    ];
    enable_metrics(&router, &group, &metrics).await;
    let expected = [
        ("GroupDesiredCapacity", f64::from(settled)),
        ("GroupInServiceCapacity", f64::from(provided)),
        ("GroupInServiceInstances", f64::from(instances)),
        ("GroupPendingCapacity", 0.0),
        ("GroupPendingInstances", 0.0),
        ("GroupTotalCapacity", f64::from(provided)),
        ("GroupTotalInstances", f64::from(instances)),
    ];
    assert_eq!(
        expected.len(),
        metrics.len(),
        "case {case:?}: every collected metric must carry an expected value"
    );
    let stamp = shared_minute(&router, &group, case, &expected).await;
    assert_eq!(
        stamp % 60,
        0,
        "case {case:?}: a one-minute datapoint is stamped with the start of its minute, got \
         {stamp}"
    );
}

/// The group holds the fewest instances whose weights fulfil `capacity`, every one of them in
/// service and each reporting the weight its type is counted at.
///
/// The count is asserted BOTH ways: against the rounding the rule states, and against the
/// bounds that make it the fewest — the fleet covers the capacity asked for, and one instance
/// fewer would not.
async fn assert_fulfilled(
    router: &MultiServiceRouter,
    group: &str,
    case: &str,
    capacity: i32,
    weight: i32,
) {
    let wanted = instances_fulfilling(capacity, weight);
    let provided = wanted * weight;
    assert!(
        provided >= capacity && provided - weight < capacity,
        "case {case:?}: {wanted} instances of {weight} units provide {provided}, which must \
         fulfil {capacity} with no whole instance to spare"
    );
    let members = settle(
        router,
        group,
        usize::try_from(wanted).expect("an instance count is not negative"),
        case,
    )
    .await;
    assert_eq!(
        members.weights,
        expected_weights(members.ids.len(), weight),
        "case {case:?}: at {capacity} units each member must report the weight its type is \
         counted at, got {:?}",
        members.weights
    );
    // The capacity the group reports is the capacity it was asked for, in its own units: the
    // overage the weights produce is in the fleet, not in the parameter.
    assert_eq!(
        members.desired_capacity,
        capacity.to_string(),
        "case {case:?}: the group must report the capacity it was asked for, got {:?}",
        members.desired_capacity
    );
}

/// The `WeightedCapacity` every member of a fleet of `held` instances reports.
///
/// A weighted group's members each report the weight their type is counted at ("the number of
/// capacity units contributed by the instance based on its instance type",
/// `smithy_query(query="doc",shape="Instance",member="WeightedCapacity")`); an unweighted
/// group states no weight for its type, so its members report none at all and a read of them
/// carries the member for none of them.
fn expected_weights(held: usize, weight: i32) -> Vec<String> {
    if weight == UNWEIGHTED {
        return Vec::new();
    }
    vec![weight.to_string(); held]
}

/// The single unit an instance of an override that states no weight contributes.
const UNWEIGHTED: i32 = 1;

/// The capacity units one instance of the row's group contributes, as a number.
fn row_weight(row: &Row) -> i32 {
    row.weight.map_or(UNWEIGHTED, |weight| {
        weight
            .parse()
            .expect("a row states its weight as a whole number of capacity units")
    })
}

/// How many instances of `weight` units fulfil `capacity`: the capacity divided by the weight,
/// ROUNDED UP, because a launch that overshoots the capacity asked for still happens while a
/// shortfall must not stand.
fn instances_fulfilling(capacity: i32, weight: i32) -> i32 {
    let capacity = u32::try_from(capacity).expect("a capacity asked for is not negative");
    let weight = u32::try_from(weight).expect("a weight is a positive number of units");
    i32::try_from(capacity.div_ceil(weight)).expect("an instance count fits in an i32")
}

/// One emulator with Amazon EC2 Auto Scaling, Amazon EC2 and CloudWatch on ONE engine: the
/// instances a group launches are the account's own Amazon EC2 instances, its subnets and
/// launch templates are Amazon EC2's records, and the series it publishes are CloudWatch's own
/// metrics.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Create one group whose mixed instances policy overrides [`INSTANCE_TYPE`], carrying `weight`
/// when the row states one, placed in `subnets`.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    weight: Option<&str>,
    desired: i32,
    subnets: &str,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
            template,
        ),
        p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
            INSTANCE_TYPE,
        ),
        p("MinSize", "0"),
        p("MaxSize", MAX_SIZE.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", subnets),
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

/// Ask the group for `desired` capacity units, which must be served.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    group: &str,
    desired: i32,
    case: &str,
) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
            // The capacity change is what is under test, never the cooldown that follows one:
            // the row makes each change the moment the fleet before it settled.
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity {desired} must be served, got {status} with {body:?}"
    );
}

/// What one read of the group reports about its fleet.
#[derive(Debug, Default)]
struct Members {
    ids: Vec<String>,
    states: Vec<String>,
    weights: Vec<String>,
    desired_capacity: String,
}

/// The fleet the group reports, read inside its own `Instances` list.
async fn members(router: &MultiServiceRouter, group: &str, case: &str) -> Members {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be readable, got {status} with {body:?}"
    );
    let instances = tag_value(&body, "Instances");
    Members {
        ids: tag_values(&instances, "InstanceId"),
        states: tag_values(&instances, "LifecycleState"),
        weights: tag_values(&instances, "WeightedCapacity"),
        desired_capacity: tag_value(&body, "DesiredCapacity"),
    }
}

/// Look again until the group reports `wanted` members and every one of them is in service.
///
/// The CONDITION, never a fixed wait: a launch and a termination are owed on a clock nothing
/// sleeps for.
async fn settle(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
    case: &str,
) -> Members {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = members(router, group, case).await;
        if read.ids.len() == wanted && read.states.iter().all(|state| state == IN_SERVICE) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} member(s) in {IN_SERVICE}, the group reports {:?} \
             in {:?}",
            read.ids,
            read.states
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Enable metrics collection for `group`, naming exactly `metrics`.
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

/// One datapoint of one series, as `GetMetricStatistics` reports it.
#[derive(Debug, Clone, PartialEq)]
struct Point {
    maximum: f64,
    minimum: f64,
    unit: String,
}

/// The series `metric` of `group` holds, keyed by the whole minute each datapoint is aggregated
/// at — which is what makes "these metrics agree at ONE time stamp" readable.
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
            "Statistics": ["Maximum", "Minimum"],
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

/// Now, as the epoch seconds a CloudWatch request states an instant in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Whether two readings of a count are the same number: whole counts carried as doubles, so
/// they are compared with a tolerance rather than bit for bit.
fn same(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

/// Look again until ONE whole minute carries every `expected` metric at the value it names, and
/// answer that minute's stamp.
///
/// Each metric is read on its own, exactly as a client reads it, and the minute is one the
/// readings SHARE: that is what makes "the capacity is the weight times the instance count" an
/// assertion about one observation of the group rather than about readings taken at different
/// instants.
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
