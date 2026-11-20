// SPDX-License-Identifier: CC0-1.0

//! The AGGREGATE `AWS/EC2` series the instances of one type and of one image publish between
//! them, end to end over the wire.
//!
//! THE RULE: a guest measurement of an instance with DETAILED monitoring is published into the
//! `AWS/EC2` series dimensioned on that instance's type and into the one dimensioned on the
//! image it was launched from, beside the series dimensioned on the instance itself —
//! "`InstanceType`: This dimension filters the data you request for all instances running with
//! this specified instance type" and "`ImageId`: This dimension filters the data you request for
//! all instances running this Amazon EC2 Amazon Machine Image (AMI)", both "available for
//! instances with detailed monitoring enabled" (Amazon EC2 User Guide, instance metric
//! dimensions; these are documented metrics and dimensions rather than Smithy shapes, so there
//! is no model member to read them from). One contribution per instance per period, so each
//! period of an aggregate series folds exactly the instances measured in it: its `SampleCount` is
//! their number, its `Sum` their total and its `Maximum` the largest reading any of them took,
//! in that metric's own unit. An instance publishing at the BASIC cadence contributes to neither
//! aggregate, and a status check — a verdict about one named instance — is published under the
//! instance's own dimension alone.
//!
//! The rows span what the rule generalises over: the aggregate dimension (type and image), the
//! metric and its unit (a percentage, bytes and a count), how many instances contribute to one
//! series (one, two, none), the monitoring state of each contributor, and the cadence class
//! (guest measurement against status check). Every aggregate is checked against the folds of the
//! contributors' OWN series read over the same window, so nothing about the expected numbers is
//! hard-coded.
//!
//! The harness is the sibling suite's ([`super::instance_metric_publication`]): Amazon CloudWatch
//! mounted BESIDE Amazon EC2 on one engine, as the assembled router mounts them, with the cadence
//! driven by advancing the sampler's own clock over a span of periods rather than by waiting for
//! those periods to pass. Every datapoint is read back through CloudWatch's own
//! `GetMetricStatistics` and `ListMetrics`.

use std::collections::BTreeMap;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::instance_metric_publication::{
    cloudwatch_ok, ec2_ok, emulator, p, poll_until_reported, sample_the_span, span_base,
    INSTANCE_ID_DIMENSION, MINUTE, NAMESPACE, SPAN_MINUTES,
};

/// The dimension the instances of one instance type are rolled up under.
const INSTANCE_TYPE_DIMENSION: &str = "InstanceType";

/// The dimension the instances launched from one image are rolled up under.
const IMAGE_ID_DIMENSION: &str = "ImageId";

/// The image most launches here name.
const AMI: &str = "ami-0123456789abcdef0";

/// A second image, so one image's aggregate is told from another's.
const OTHER_AMI: &str = "ami-00112233445566778";

/// The instance type the pair of instances share.
const SHARED_TYPE: &str = "t3.micro";

/// A second instance type, so one type's aggregate is told from another's.
const OTHER_TYPE: &str = "t3.small";

/// An instance type nothing here launches, for the read that must find nothing.
const ABSENT_TYPE: &str = "m5.large";

/// How far two folded values may differ and still be the same number.
const VALUE_EPSILON: f64 = 1e-9;

/// One metric of the guest-measurement family, with the unit its datapoints carry.
///
/// The names and units come from the metrics Amazon EC2 documents for one INSTANCE (Amazon EC2
/// User Guide, "Instance metrics"); they are spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a case pass. One metric per unit the catalog uses,
/// and `MetadataNoToken` is a metric the issue that drove this suite never cited.
const GUEST_METRICS: &[(&str, &str)] = &[
    ("CPUUtilization", "Percent"),
    ("NetworkIn", "Bytes"),
    ("DiskWriteOps", "Count"),
    ("MetadataNoToken", "Count"),
];

/// A status-check metric, whose verdict is about one instance: "Status check metrics" of the same
/// guide.
const STATUS_CHECK_METRIC: &str = "StatusCheckFailed_System";

/// One folded window of one series.
#[derive(Debug, Clone, PartialEq)]
struct Fold {
    /// How many observations the window folded.
    sample_count: f64,
    /// The largest of them.
    maximum: f64,
    /// Their total.
    sum: f64,
    /// The unit they were measured in.
    unit: String,
}

/// The one-minute windows of `metric` under the single dimension `(name, value)` inside
/// `[start, end)`, by the period each is stamped with.
async fn folds(
    router: &MultiServiceRouter,
    case: &str,
    dimension: (&str, &str),
    metric: &str,
    start: i64,
    end: i64,
) -> BTreeMap<i64, Fold> {
    let answer = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": dimension.0, "Value": dimension.1}],
            "StartTime": start,
            "EndTime": end,
            "Period": MINUTE,
            "Statistics": ["SampleCount", "Maximum", "Sum"],
        }),
    )
    .await;
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Timestamp"].as_f64().unwrap_or_default().round() as i64,
                Fold {
                    sample_count: point["SampleCount"].as_f64().unwrap_or_default(),
                    maximum: point["Maximum"].as_f64().unwrap_or_default(),
                    sum: point["Sum"].as_f64().unwrap_or_default(),
                    unit: point["Unit"].as_str().unwrap_or_default().to_string(),
                },
            )
        })
        .collect()
}

/// The windows the contributors' own series hold, folded the way one aggregate period folds
/// them: the count summed, the maximum taken and the total summed.
fn rolled_up(contributions: &[BTreeMap<i64, Fold>]) -> BTreeMap<i64, Fold> {
    let mut rolled: BTreeMap<i64, Fold> = BTreeMap::new();
    for windows in contributions {
        for (at, fold) in windows {
            let entry = rolled.entry(*at).or_insert_with(|| Fold {
                sample_count: 0.0,
                maximum: f64::NEG_INFINITY,
                sum: 0.0,
                unit: fold.unit.clone(),
            });
            entry.sample_count += fold.sample_count;
            entry.maximum = entry.maximum.max(fold.maximum);
            entry.sum += fold.sum;
        }
    }
    rolled
}

/// Whether two folds are the same reading of the same window.
fn same(left: &Fold, right: &Fold) -> bool {
    (left.sample_count - right.sample_count).abs() < VALUE_EPSILON
        && (left.maximum - right.maximum).abs() < VALUE_EPSILON
        && (left.sum - right.sum).abs() < VALUE_EPSILON
        && left.unit == right.unit
}

/// Launch one instance of `instance_type` from `image_id`, with detailed monitoring on or off,
/// and answer the id this account issued it.
async fn launch_one(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    instance_type: &str,
    detailed: bool,
) -> String {
    let mut params = vec![
        p("ImageId", image_id),
        p("MinCount", "1"),
        p("MaxCount", "1"),
        p("InstanceType", instance_type),
    ];
    if detailed {
        params.push(p("Monitoring.Enabled", "true"));
    }
    let body = ec2_ok(router, case, "RunInstances", &params).await;
    let instance_id = ec2_query::element(&body, "instanceId").unwrap_or_else(|| {
        panic!("case {case:?}: RunInstances must report the instance it minted: {body}")
    });
    let monitoring = if detailed { "enabled" } else { "disabled" };
    poll_until_reported(router, case, &instance_id, "running", monitoring).await;
    instance_id
}

/// One row of the aggregate table: the dimension one series is read under, and which of the
/// launched instances must contribute to it.
struct AggregateRow {
    case: &'static str,
    dimension: &'static str,
    /// The dimension value, as the launch spelled it.
    value: &'static str,
    /// The indices, into the launched instances, of the contributors to this series.
    contributors: &'static [usize],
}

/// Rules: the aggregate series of a type and of an image fold exactly the detailed-monitoring
/// instances measured in each period, in the metric's own unit — and an instance publishing at
/// the basic cadence is in neither fold.
#[tokio::test]
async fn an_aggregate_period_folds_the_detailed_instances_measured_in_it() {
    let case = "two instances of one type, one of another, and one under basic monitoring";
    let (router, engine) = emulator();

    // 0 and 1: detailed, the shared type, the shared image — two contributors to one series.
    // 2: detailed, another type, the shared image — one contributor of its own.
    // 3: detailed, the shared type, another image.
    // 4: BASIC monitoring, the shared type and image — a contributor to neither aggregate.
    let instances = [
        launch_one(&router, case, AMI, SHARED_TYPE, true).await,
        launch_one(&router, case, AMI, SHARED_TYPE, true).await,
        launch_one(&router, case, AMI, OTHER_TYPE, true).await,
        launch_one(&router, case, OTHER_AMI, SHARED_TYPE, true).await,
        launch_one(&router, case, AMI, SHARED_TYPE, false).await,
    ];

    let rows = [
        AggregateRow {
            case: "the shared type, three detailed instances of it",
            dimension: INSTANCE_TYPE_DIMENSION,
            value: SHARED_TYPE,
            contributors: &[0, 1, 3],
        },
        AggregateRow {
            case: "another type, one detailed instance of it",
            dimension: INSTANCE_TYPE_DIMENSION,
            value: OTHER_TYPE,
            contributors: &[2],
        },
        AggregateRow {
            case: "the shared image, three detailed instances from it",
            dimension: IMAGE_ID_DIMENSION,
            value: AMI,
            contributors: &[0, 1, 2],
        },
        AggregateRow {
            case: "another image, one detailed instance from it",
            dimension: IMAGE_ID_DIMENSION,
            value: OTHER_AMI,
            contributors: &[3],
        },
        AggregateRow {
            case: "a type nothing launched",
            dimension: INSTANCE_TYPE_DIMENSION,
            value: ABSENT_TYPE,
            contributors: &[],
        },
    ];

    let base = span_base(0);
    sample_the_span(&engine, base);
    let end = base + SPAN_MINUTES * MINUTE;

    for (metric, unit) in GUEST_METRICS {
        for row in &rows {
            let case = format!("{} of {}={} ({})", metric, row.dimension, row.value, row.case);
            let mut contributions = Vec::new();
            for index in row.contributors {
                contributions.push(
                    folds(
                        &router,
                        &case,
                        (INSTANCE_ID_DIMENSION, &instances[*index]),
                        metric,
                        base,
                        end,
                    )
                    .await,
                );
            }
            let expected = rolled_up(&contributions);
            let aggregate = folds(&router, &case, (row.dimension, row.value), metric, base, end).await;

            assert_eq!(
                aggregate.keys().copied().collect::<Vec<i64>>(),
                expected.keys().copied().collect::<Vec<i64>>(),
                "case {case:?}: the aggregate must carry exactly the periods its contributors \
                 were measured in, got {aggregate:?} against {expected:?}"
            );
            for (at, want) in &expected {
                let got = aggregate
                    .get(at)
                    .unwrap_or_else(|| panic!("case {case:?}: no aggregate window at {at}"));
                assert!(
                    same(got, want),
                    "case {case:?}: the window at {at} must fold its contributors to {want:?}, \
                     got {got:?}"
                );
                assert_eq!(
                    &got.unit, unit,
                    "case {case:?}: the window at {at} must be measured in {unit}, got {:?}",
                    got.unit
                );
                assert_eq!(
                    got.sample_count,
                    row.contributors.len() as f64,
                    "case {case:?}: the window at {at} must fold one observation per \
                     contributing instance, got {}",
                    got.sample_count
                );
            }
            // A series nothing contributes to is read as empty rather than as a series of zeros.
            if row.contributors.is_empty() {
                assert!(
                    aggregate.is_empty(),
                    "case {case:?}: nothing publishes this series, got {aggregate:?}"
                );
            }
        }
        // The positive control for the row above: the instance under BASIC monitoring IS being
        // measured in this window, it is only outside the aggregates.
        let case = format!("{metric} of the basic-monitoring instance");
        let own = folds(
            &router,
            &case,
            (INSTANCE_ID_DIMENSION, &instances[4]),
            metric,
            base,
            end,
        )
        .await;
        assert!(
            !own.is_empty(),
            "case {case:?}: the instance must publish its own series, got {own:?}"
        );
    }
}

/// Rule: a status check's verdict is published under the instance's own dimension alone — it is
/// a verdict about that instance, not a quantity to roll up.
#[tokio::test]
async fn a_status_check_verdict_is_published_under_no_aggregate_dimension() {
    let case = "two detailed instances of one type and image";
    let (router, engine) = emulator();
    let instance_ids = [
        launch_one(&router, case, AMI, SHARED_TYPE, true).await,
        launch_one(&router, case, AMI, SHARED_TYPE, true).await,
    ];

    let base = span_base(0);
    sample_the_span(&engine, base);
    let end = base + SPAN_MINUTES * MINUTE;

    // The positive control: each instance's own verdict series is published.
    for instance_id in &instance_ids {
        let own = folds(
            &router,
            case,
            (INSTANCE_ID_DIMENSION, instance_id),
            STATUS_CHECK_METRIC,
            base,
            end,
        )
        .await;
        assert!(
            !own.is_empty(),
            "case {case:?}: {instance_id} must publish its own {STATUS_CHECK_METRIC} series, \
             got {own:?}"
        );
    }

    for dimension in [
        (INSTANCE_TYPE_DIMENSION, SHARED_TYPE),
        (IMAGE_ID_DIMENSION, AMI),
    ] {
        let aggregate = folds(&router, case, dimension, STATUS_CHECK_METRIC, base, end).await;
        assert!(
            aggregate.is_empty(),
            "case {case:?}: {STATUS_CHECK_METRIC} must not be published under {}={}, got \
             {aggregate:?}",
            dimension.0,
            dimension.1
        );
    }
}

/// Rule: the aggregates follow the MONITORING STATE rather than the launch — an instance whose
/// detailed monitoring is taken away drops out of the folds of the periods after it.
#[tokio::test]
async fn an_unmonitored_instance_drops_out_of_the_aggregates() {
    let case = "one of two detailed instances unmonitored";
    let (router, engine) = emulator();
    let stays = launch_one(&router, case, AMI, SHARED_TYPE, true).await;
    let leaves = launch_one(&router, case, AMI, SHARED_TYPE, true).await;

    // While both are monitored in detail, both are folded.
    let first = span_base(0);
    sample_the_span(&engine, first);
    let aggregate = folds(
        &router,
        case,
        (INSTANCE_TYPE_DIMENSION, SHARED_TYPE),
        "CPUUtilization",
        first,
        first + SPAN_MINUTES * MINUTE,
    )
    .await;
    assert!(
        !aggregate.is_empty()
            && aggregate
                .values()
                .all(|fold| (fold.sample_count - 2.0).abs() < VALUE_EPSILON),
        "case {case:?}: every period must fold both instances, got {aggregate:?}"
    );

    ec2_ok(
        &router,
        case,
        "UnmonitorInstances",
        &[p("InstanceId.1", leaves.clone())],
    )
    .await;
    poll_until_reported(&router, case, &leaves, "running", "disabled").await;

    // And after it, only the instance that is still monitored in detail.
    let second = span_base(1);
    sample_the_span(&engine, second);
    let aggregate = folds(
        &router,
        case,
        (INSTANCE_TYPE_DIMENSION, SHARED_TYPE),
        "CPUUtilization",
        second,
        second + SPAN_MINUTES * MINUTE,
    )
    .await;
    assert!(
        !aggregate.is_empty()
            && aggregate
                .values()
                .all(|fold| (fold.sample_count - 1.0).abs() < VALUE_EPSILON),
        "case {case:?}: every period must fold the one instance still monitored in detail, got \
         {aggregate:?}"
    );
    // The instance that was unmonitored is still being measured on its own.
    for instance_id in [&stays, &leaves] {
        let own = folds(
            &router,
            case,
            (INSTANCE_ID_DIMENSION, instance_id),
            "CPUUtilization",
            second,
            second + SPAN_MINUTES * MINUTE,
        )
        .await;
        assert!(
            !own.is_empty(),
            "case {case:?}: {instance_id} must still publish its own series, got {own:?}"
        );
    }
}

/// Rule: an aggregate series that has been published into is a metric of the account, so
/// `ListMetrics` reports it under that one dimension and no other.
#[tokio::test]
async fn list_metrics_reports_each_aggregate_series() {
    let case = "one detailed instance";
    let (router, engine) = emulator();
    let instance_id = launch_one(&router, case, AMI, SHARED_TYPE, true).await;

    let base = span_base(0);
    sample_the_span(&engine, base);

    let answer = cloudwatch_ok(
        &router,
        case,
        "ListMetrics",
        &json!({"Namespace": NAMESPACE, "MetricName": "CPUUtilization"}),
    )
    .await;
    let listed: Vec<Vec<(String, String)>> = answer["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            metric["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .collect();

    for expected in [
        vec![(INSTANCE_ID_DIMENSION.to_string(), instance_id.clone())],
        vec![(INSTANCE_TYPE_DIMENSION.to_string(), SHARED_TYPE.to_string())],
        vec![(IMAGE_ID_DIMENSION.to_string(), AMI.to_string())],
    ] {
        assert!(
            listed.contains(&expected),
            "case {case:?}: ListMetrics must report the series {expected:?}, got {listed:?}"
        );
    }
}
