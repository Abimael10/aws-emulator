// SPDX-License-Identifier: CC0-1.0

//! The hours a predictive-scaling policy's forecast reports no VALUE for, over the awsQuery
//! wire.
//!
//! The rule under test: a forecast covers the whole hours of the range it was asked for
//! whatever the account has observed of the load metric, and an hour the daily pattern states
//! no load for carries NO VALUE — not a number — in the load forecast and in the capacity
//! forecast alike, rather than being dropped from either. "Load forecasts are predictions of
//! the hourly load values using historical load data from CloudWatch … A minimum of 24 hours
//! of data is required to create the initial forecasts"
//! (`smithy_query(query="operation",operation="GetPredictiveScalingForecast")`): the data
//! decides what each hour's value IS, not which hours a forecast is reported over, so a
//! policy created moments ago — whose series nothing has published into — is answered with its
//! metric specification and an unpredicted value for every hour of the range.
//!
//! The rows span what the rule generalises over: each form a metric specification names its
//! load metric in (a predefined metric pair, a predefined load metric and a customized load
//! metric), ranges of three and of twelve hours, and a history that observed SOME hours of
//! the day and not others — where the observed hours carry the mean of what was published
//! for them in the same answer as the unobserved ones carry nothing.
//!
//! Beside `predictive_scaling_forecast.rs`, which pins what a forecast computed from a full
//! history IS: this suite is about the hours such a pattern says nothing about.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;

use crate::support::{
    borrowed, json_rpc, list_members, new_engine, p, query, seed_launch_templates, tag_value,
};

/// How wide one forecast period is: the hour the operation forecasts in.
const PERIOD: Duration = Duration::from_secs(60 * 60);

/// How many periods one day holds.
const PERIODS_PER_DAY: i64 = 24;

/// The fewest hours of data a daily pattern is made from: "A minimum of 24 hours of data is
/// required to create the initial forecasts" (`GetPredictiveScalingForecast`). Written out
/// here rather than taken from the crate, so this suite states the requirement itself.
const MIN_HISTORY_PERIODS: i64 = 24;

/// The launch template every group here is created from.
const LAUNCH_TEMPLATE: &str = "lt-unobserved";

/// The `TargetValue` every policy here states.
const TARGET_VALUE: f64 = 25.0;

/// The namespace and metric a CUSTOMIZED load metric names here — a client's own, which is
/// why a client can publish into it.
const CUSTOM_NAMESPACE: &str = "Emulator/Unobserved";
const CUSTOM_METRIC: &str = "RequestsPerHour";

/// The dimension the custom series is published under.
const CUSTOM_DIMENSION: (&str, &str) = ("Service", "checkout");

/// The signing name, target prefix and awsJson version a CloudWatch client sends.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// The wire prefix of the one metric specification each policy here carries.
const SPEC: &str = "PredictiveScalingConfiguration.MetricSpecifications.member.1";

/// The resource label a per-target-group metric names its target group by: "the final portion
/// of the load balancer ARN and the final portion of the target group ARN into a single value,
/// separated by a forward slash (/)" (`PredefinedMetricSpecification$ResourceLabel`).
const RESOURCE_LABEL: &str = "app/web-alb/0123456789abcdef/targetgroup/web-tg/fedcba9876543210";

/// One emulator with Amazon EC2 Auto Scaling and CloudWatch on ONE engine: the observations a
/// forecast is computed from are the account's own CloudWatch metrics.
fn emulator() -> MultiServiceRouter {
    let engine = new_engine();
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Create one group and assert the create succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "10"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// The members one specification form carries, beside the target value every policy states.
fn specification_forms() -> Vec<(&'static str, Vec<(String, String)>)> {
    let predefined = |member: &str, metric_type: &str, label: bool| {
        let prefix = format!("{SPEC}.{member}");
        let mut params = vec![p(&format!("{prefix}.PredefinedMetricType"), metric_type)];
        if label {
            params.push(p(&format!("{prefix}.ResourceLabel"), RESOURCE_LABEL));
        }
        params
    };
    vec![
        (
            "a predefined metric pair over the group's own CPU",
            predefined(
                "PredefinedMetricPairSpecification",
                "ASGCPUUtilization",
                false,
            ),
        ),
        (
            "a predefined metric pair counted per load balancer target group",
            predefined("PredefinedMetricPairSpecification", "ALBRequestCount", true),
        ),
        (
            "a predefined load metric over the group's own network traffic",
            predefined(
                "PredefinedLoadMetricSpecification",
                "ASGTotalNetworkIn",
                false,
            ),
        ),
        (
            "a customized load metric over a client's own series",
            customized_load_metric(),
        ),
    ]
}

/// The customized-load-metric form: a metric data query naming the client's own series,
/// totalled over the hour.
fn customized_load_metric() -> Vec<(String, String)> {
    let prefix = format!("{SPEC}.CustomizedLoadMetricSpecification.MetricDataQueries.member.1");
    vec![
        p(&format!("{prefix}.Id"), "load"),
        p(
            &format!("{prefix}.MetricStat.Metric.Namespace"),
            CUSTOM_NAMESPACE,
        ),
        p(
            &format!("{prefix}.MetricStat.Metric.MetricName"),
            CUSTOM_METRIC,
        ),
        p(
            &format!("{prefix}.MetricStat.Metric.Dimensions.member.1.Name"),
            CUSTOM_DIMENSION.0,
        ),
        p(
            &format!("{prefix}.MetricStat.Metric.Dimensions.member.1.Value"),
            CUSTOM_DIMENSION.1,
        ),
        p(&format!("{prefix}.MetricStat.Stat"), "Sum"),
    ]
}

/// Put one predictive scaling policy carrying `specification` and assert the put succeeded.
async fn put_policy(router: &MultiServiceRouter, group: &str, policy: &str, specification: &[(String, String)]) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", policy),
        p("PolicyType", "PredictiveScaling"),
        p(&format!("{SPEC}.TargetValue"), TARGET_VALUE.to_string()),
    ];
    params.extend_from_slice(specification);
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "putting {policy:?} must succeed, got {status} with {body:?}"
    );
}

/// Publish one observation of the client's own series per instant in `hours`, each carrying
/// `load`, through CloudWatch's own `PutMetricData`.
async fn publish(router: &MultiServiceRouter, hours: &[SystemTime], load: f64) {
    let data: Vec<serde_json::Value> = hours
        .iter()
        .map(|at| {
            json!({
                "MetricName": CUSTOM_METRIC,
                "Dimensions": [{"Name": CUSTOM_DIMENSION.0, "Value": CUSTOM_DIMENSION.1}],
                "Timestamp": epoch_secs(*at),
                "Value": load,
                "Unit": "Count",
            })
        })
        .collect();
    let (status, answer) = json_rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        CLOUDWATCH_JSON_VERSION,
        "PutMetricData",
        json!({"Namespace": CUSTOM_NAMESPACE, "MetricData": data}),
    )
    .await;
    assert_eq!(
        status, 200,
        "publishing the history must succeed, got {status} with {answer}"
    );
}

/// Post one `GetPredictiveScalingForecast` and answer its status and body.
async fn get_forecast(
    router: &MultiServiceRouter,
    group: &str,
    policy: &str,
    start: SystemTime,
    end: SystemTime,
) -> (u16, String) {
    query(
        router,
        "GetPredictiveScalingForecast",
        &[
            ("AutoScalingGroupName", group),
            ("PolicyName", policy),
            ("StartTime", &rfc3339(start)),
            ("EndTime", &rfc3339(end)),
        ],
    )
    .await
}

/// `at` as the RFC 3339 instant a request states a timestamp in.
fn rfc3339(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from_secs(whole_seconds(at))
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the instant renders")
}

/// `at` as whole epoch seconds.
fn whole_seconds(at: SystemTime) -> i64 {
    i64::try_from(at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
        .expect("an instant inside the epoch's range")
}

/// `at` as epoch seconds, the form a published observation states its instant in.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// `at` rounded DOWN to the whole hour it falls in.
fn whole_hour(at: SystemTime) -> SystemTime {
    let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    UNIX_EPOCH + Duration::from_secs(secs - secs % PERIOD.as_secs())
}

/// Which hour of the day `at` falls in, counted off the epoch exactly as a forecast period is.
fn period_of_day(at: SystemTime) -> i64 {
    (whole_seconds(at) / i64::try_from(PERIOD.as_secs()).expect("an hour fits")) 
        .rem_euclid(PERIODS_PER_DAY)
}

/// One series the answer reports: its instants, as the epoch seconds a client reads them in,
/// beside its values.
type ReportedSeries = (Vec<i64>, Vec<f64>);

/// The `(timestamps, values)` of one reported series, as a client reads them.
fn series_of(block: &str) -> ReportedSeries {
    (
        list_members(block, "Timestamps")
            .into_iter()
            .map(|member| {
                ::aws_smithy_types::DateTime::from_str(
                    member,
                    ::aws_smithy_types::date_time::Format::DateTime,
                )
                .map(|at| at.secs())
                .unwrap_or_else(|error| panic!("{member:?} must be an instant: {error}"))
            })
            .collect(),
        list_members(block, "Values")
            .into_iter()
            .map(|member| {
                member
                    .parse::<f64>()
                    .unwrap_or_else(|error| panic!("{member:?} must be a number: {error}"))
            })
            .collect(),
    )
}

/// The load forecast and the capacity forecast one answer reports.
fn forecasts(body: &str) -> (ReportedSeries, ReportedSeries) {
    (
        series_of(&tag_value(body, "LoadForecast")),
        series_of(&tag_value(body, "CapacityForecast")),
    )
}

/// The whole hours of `[start, end)`, as the epoch seconds the answer reports them in.
fn hours_of(start: SystemTime, end: SystemTime) -> Vec<i64> {
    let mut at = start;
    let mut hours = Vec::new();
    while at < end {
        hours.push(whole_seconds(at));
        at += PERIOD;
    }
    hours
}

/// RULE: a policy whose load series the account holds NO observation of is answered over every
/// hour of the range, with no value in either series — whichever form its specification names
/// its load metric in, and however wide the range.
#[tokio::test]
async fn a_series_nobody_published_into_predicts_no_value_for_any_hour() {
    for (index, (case, specification)) in specification_forms().into_iter().enumerate() {
        for hours_asked in [3_u64, 12_u64] {
            let router = emulator();
            let group = format!("asg-unobserved-{index}-{hours_asked}");
            create_group(&router, &group).await;
            put_policy(&router, &group, "forecast-policy", &specification).await;

            let start = whole_hour(SystemTime::now()) + PERIOD;
            let end = start + Duration::from_secs(hours_asked * PERIOD.as_secs());
            let (status, body) = get_forecast(&router, &group, "forecast-policy", start, end).await;
            assert_eq!(
                status, 200,
                "case {case:?} over {hours_asked} hours: the forecast must be served, got \
                 {status} with {body}"
            );

            // The policy's one metric specification is reported, whatever was observed.
            let reported = tag_value(&body, "MetricSpecification");
            assert!(
                !reported.is_empty(),
                "case {case:?} over {hours_asked} hours: the forecast must report the \
                 specification it was read for, got {body}"
            );

            let expected = hours_of(start, end);
            let ((load_at, loads), (capacity_at, capacities)) = forecasts(&body);
            assert_eq!(
                (load_at.clone(), capacity_at),
                (expected.clone(), expected.clone()),
                "case {case:?} over {hours_asked} hours: both series must cover every whole \
                 hour of the range, got {load_at:?} in {body}"
            );
            assert_eq!(
                (loads.len(), capacities.len()),
                (expected.len(), expected.len()),
                "case {case:?} over {hours_asked} hours: expected a value per hour, got \
                 {loads:?} and {capacities:?}"
            );
            for (slot, (load, capacity)) in loads.iter().zip(&capacities).enumerate() {
                assert!(
                    load.is_nan() && capacity.is_nan(),
                    "case {case:?} over {hours_asked} hours: hour {slot} of a series nobody \
                     published into must predict no value, got the load {load} needing \
                     {capacity}"
                );
            }
        }
    }
}

/// RULE: every instant a forecast reports lies inside the range asked for — the whole hours at
/// or after its "inclusive start time" and before its "exclusive end time"
/// (`GetPredictiveScalingForecastType$StartTime`, `$EndTime`) — so a range that begins part
/// way through an hour opens at the NEXT whole hour rather than at the one already under way.
#[tokio::test]
async fn a_forecast_reports_only_whole_hours_inside_the_range() {
    let router = emulator();
    let group = "asg-unobserved-range";
    create_group(&router, group).await;
    put_policy(&router, group, "forecast-policy", &customized_load_metric()).await;

    let hour = whole_hour(SystemTime::now());
    // Rows differing in how far into an hour the range begins and how far into one it ends:
    // the whole hour the start falls in, one offset by a minute, by half an hour and by all
    // but a minute of it.
    let offsets = [0_u64, 60, 30 * 60, 59 * 60];
    for offset in offsets {
        let start = hour + PERIOD + Duration::from_secs(offset);
        let end = start + Duration::from_secs(4 * PERIOD.as_secs());
        let (status, body) = get_forecast(&router, group, "forecast-policy", start, end).await;
        assert_eq!(
            status, 200,
            "case {offset}s into the hour: the forecast must be served, got {status} with {body}"
        );
        let ((load_at, _), (capacity_at, _)) = forecasts(&body);
        assert_eq!(
            load_at, capacity_at,
            "case {offset}s into the hour: both series must cover the same hours, got \
             {load_at:?} and {capacity_at:?}"
        );
        assert!(
            !load_at.is_empty(),
            "case {offset}s into the hour: the range holds whole hours, so the forecast must \
             report them, got {body}"
        );
        let first_whole_hour = if offset == 0 {
            whole_seconds(start)
        } else {
            whole_seconds(start) + i64::try_from(PERIOD.as_secs()).expect("an hour fits")
                - i64::try_from(offset).expect("the offset fits")
        };
        assert_eq!(
            load_at.first().copied(),
            Some(first_whole_hour),
            "case {offset}s into the hour: expected the first whole hour at or after the \
             start, got {load_at:?}"
        );
        for at in &load_at {
            assert!(
                *at >= whole_seconds(start) && *at < whole_seconds(end),
                "case {offset}s into the hour: every instant must lie inside \
                 [{}, {}), got {at}",
                whole_seconds(start),
                whole_seconds(end)
            );
        }
    }
}

/// RULE: with a pattern that observed SOME hours of the day, the observed hours carry the mean
/// of what was published for them and the hours it never observed carry no value — in one
/// answer, over the same hours.
#[tokio::test]
async fn an_hour_of_day_the_pattern_never_observed_predicts_no_value() {
    let router = emulator();
    let group = "asg-unobserved-pattern";
    create_group(&router, group).await;
    put_policy(&router, group, "forecast-policy", &customized_load_metric()).await;

    // The minimum number of hours of data, published every OTHER hour, so exactly half the
    // hours of the day are observed and the other half never are.
    let load = 600.0;
    let now = whole_hour(SystemTime::now());
    let published: Vec<SystemTime> = (1..=MIN_HISTORY_PERIODS)
        .map(|k| now - Duration::from_secs(2 * u64::try_from(k).expect("a positive hour") * PERIOD.as_secs()))
        .collect();
    publish(&router, &published, load).await;
    let observed: Vec<i64> = published.iter().map(|at| period_of_day(*at)).collect();
    assert_eq!(
        observed.len(),
        usize::try_from(MIN_HISTORY_PERIODS).expect("the minimum fits"),
        "the history must hold the documented minimum of hours, got {observed:?}"
    );

    // A whole day of hours, so the answer covers both the observed hours of the day and the
    // unobserved ones.
    let start = now + PERIOD;
    let end = start + Duration::from_secs(24 * PERIOD.as_secs());
    let (status, body) = get_forecast(&router, group, "forecast-policy", start, end).await;
    assert_eq!(
        status, 200,
        "the forecast must be served, got {status} with {body}"
    );

    let expected = hours_of(start, end);
    let ((load_at, loads), (capacity_at, capacities)) = forecasts(&body);
    assert_eq!(
        (load_at.clone(), capacity_at),
        (expected.clone(), expected.clone()),
        "both series must cover every whole hour of the day asked for, got {load_at:?} in \
         {body}"
    );

    let mut predicted = 0usize;
    let mut unpredicted = 0usize;
    for (at, (reported, capacity)) in expected.iter().zip(loads.iter().zip(&capacities)) {
        let hour = SystemTime::UNIX_EPOCH
            + Duration::from_secs(u64::try_from(*at).expect("an instant after the epoch"));
        if observed.contains(&period_of_day(hour)) {
            predicted += 1;
            assert!(
                (reported - load).abs() <= 1e-6,
                "the hour {at} was observed, so it must carry the load {load} published for \
                 it, got {reported} in {body}"
            );
            assert!(
                !capacity.is_nan(),
                "and the capacity that load needs, got {capacity} in {body}"
            );
        } else {
            unpredicted += 1;
            assert!(
                reported.is_nan() && capacity.is_nan(),
                "the hour {at} was never observed, so it must carry no value, got the load \
                 {reported} needing {capacity} in {body}"
            );
        }
    }
    // Half the hours of the day were observed and half were not, so the answer exercises both
    // sides of the rule rather than one of them.
    assert_eq!(
        (predicted, unpredicted),
        (12, 12),
        "expected half the day's hours predicted and half not, got {predicted} and \
         {unpredicted} in {body}"
    );
}
