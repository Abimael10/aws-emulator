// SPDX-License-Identifier: CC0-1.0

//! The LOAD METRIC HISTORY a predictive-scaling forecast is computed from.
//!
//! `GetScalingPlanResourceForecastData` answers "predicted values, or data points, that are
//! calculated using historical data points from a specified CloudWatch load metric"
//! (`smithy_query(query="operation",operation="GetScalingPlanResourceForecastData")`), and
//! the metric is the one the scaling instruction names — its
//! `PredefinedLoadMetricSpecification` or its `CustomizedLoadMetricSpecification`. So a
//! forecast exists for a resource exactly when this emulator HOLDS history for that metric:
//! this service measures no metric itself, and a resource nothing has published load for has
//! no past to predict from, which is the empty `Datapoints` list.
//!
//! The history is the metric data a publisher gave CloudWatch, read through the directory the
//! two services share ([`::service_common::published_metric_data`]) — so what a forecast is
//! computed from is exactly what was observed, and a client that publishes its own load
//! metric sees the forecast follow it.
//!
//! What is read is the metric's own identity (namespace, name, dimension set) and the SUM of
//! the observations inside each hour — "Before AWS Auto Scaling generates the forecast, it
//! sums up all the metric data points that occurred within each hour to match the granularity
//! period that is used in the forecast (60 minutes)"
//! (`smithy_query(query="shape",shape="CustomizedLoadMetricSpecification")`) — which is also
//! why the statistic a load metric is read with is `Sum`: "The `Sum` statistic must represent
//! the total load on the resource", and `CustomizedLoadMetricSpecification$Statistic`'s "only
//! valid value is `Sum`".

use ::service_common::published_metric_data::{DimensionSet, MetricSeries};

use crate::ops::get_scaling_plan_resource_forecast_data::FORECAST_INTERVAL_SECONDS;
use crate::ops::types::ScalingInstruction;

/// Hours in a day, for the hour of day a prediction is read off.
const HOURS_PER_DAY: i64 = 24;

/// Seconds in a day, for turning the lookback in days into a span.
const SECONDS_PER_DAY: i64 = 86_400;

/// How far back the history a forecast is computed from is read, in days.
///
/// CHOSEN: the documentation states what a forecast is computed FROM — the load metric's
/// historical data points, summed per hour — and not how much of that past the forecaster
/// takes. Two weeks covers the weekly shape of a load curve while bounding the work one read
/// does, and it sits inside the span the data itself is available over
/// (`crate::ops::get_scaling_plan_resource_forecast_data::MAX_FORECAST_HISTORY_DAYS`).
pub const HISTORY_LOOKBACK_DAYS: i64 = 14;

/// The CloudWatch namespace the Amazon EC2 Auto Scaling group metrics a predefined
/// `ASGTotal…` load metric aggregates are published under.
const EC2_NAMESPACE: &str = "AWS/EC2";

/// The dimension those metrics identify an Auto Scaling group by.
const AUTO_SCALING_GROUP_DIMENSION: &str = "AutoScalingGroupName";

/// The CloudWatch namespace an Application Load Balancer's request count is published under.
const APPLICATION_ELB_NAMESPACE: &str = "AWS/ApplicationELB";

/// The two dimensions that request count identifies its target group by: the load balancer
/// and the target group, whose values are the two portions a `ResourceLabel` carries.
const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";
/// See [`LOAD_BALANCER_DIMENSION`].
const TARGET_GROUP_DIMENSION: &str = "TargetGroup";

/// The segment a `ResourceLabel` joins its load-balancer portion and its target-group portion
/// at: "the final portion of the load balancer ARN and the final portion of the target group
/// ARN into a single value, separated by a forward slash (/). The format is
/// `app/<load-balancer-name>/<load-balancer-id>/targetgroup/<target-group-name>/<target-group-id>`"
/// (`smithy_query(query="shape",shape="PredefinedLoadMetricSpecification")`).
const TARGET_GROUP_SEGMENT: &str = "/targetgroup/";

/// The prefix the target-group portion of that label opens with, which is part of the value
/// the `TargetGroup` dimension carries.
const TARGET_GROUP_PREFIX: &str = "targetgroup/";

/// The `ResourceId` prefix of an Auto Scaling group: "a prefix (autoScalingGroup) followed by
/// the name of a specified Auto Scaling group" (`ScalingInstruction$ResourceId`).
const AUTO_SCALING_GROUP_PREFIX: &str = "autoScalingGroup/";

/// Which CloudWatch metric each `LoadMetricType` aggregates, as AWS publishes it: the metric
/// name in [`EC2_NAMESPACE`] whose total over the Auto Scaling group that type names, and the
/// Application Load Balancer request count.
///
/// The value list is the model's own
/// (`smithy_query(query="shape",shape="LoadMetricType")`): `ASGTotalCPUUtilization`,
/// `ASGTotalNetworkIn`, `ASGTotalNetworkOut` and `ALBTargetGroupRequestCount`. Each names the
/// TOTAL of one published metric over the resource — which is what a load metric measures,
/// and why it is read with the `Sum` statistic.
const PREDEFINED_LOAD_METRICS: &[(&str, &str)] = &[
    ("ASGTotalCPUUtilization", "CPUUtilization"),
    ("ASGTotalNetworkIn", "NetworkIn"),
    ("ASGTotalNetworkOut", "NetworkOut"),
];

/// The `LoadMetricType` whose metric is the Application Load Balancer request count rather
/// than an Auto Scaling group total.
const ALB_REQUEST_COUNT_TYPE: &str = "ALBTargetGroupRequestCount";

/// The metric name that type aggregates.
const ALB_REQUEST_COUNT_METRIC: &str = "RequestCount";

/// The identity of the CloudWatch metric `instruction` configures predictive scaling with,
/// or `None` where it configures none — an instruction carrying neither load metric
/// specification has no metric to predict from at all.
///
/// A `CustomizedLoadMetricSpecification` carries that identity outright (its namespace, its
/// metric name and its dimensions, "the same dimensions in your customized load metric
/// specification" the publisher used). A `PredefinedLoadMetricSpecification` names a metric
/// AWS publishes, so the identity is the one AWS publishes it under, keyed to the resource the
/// instruction applies to: the Auto Scaling group of its `ResourceId` for the `ASGTotal…`
/// types, and the load balancer and target group of its `ResourceLabel` for the Application
/// Load Balancer request count.
#[must_use]
pub fn load_metric_series(instruction: &ScalingInstruction) -> Option<MetricSeries> {
    if let Some(customized) = &instruction.customized_load_metric_specification {
        let dimensions: DimensionSet = customized
            .dimensions
            .iter()
            .flatten()
            .map(|dimension| (dimension.name.clone(), dimension.value.clone()))
            .collect();
        return Some(MetricSeries::new(
            customized.namespace.clone(),
            customized.metric_name.clone(),
            dimensions,
        ));
    }
    let predefined = instruction.predefined_load_metric_specification.as_ref()?;
    let metric_type = predefined.predefined_load_metric_type.as_str();
    if metric_type == ALB_REQUEST_COUNT_TYPE {
        let label = predefined.resource_label.as_deref()?;
        let (load_balancer, target_group) = label.split_once(TARGET_GROUP_SEGMENT)?;
        if load_balancer.is_empty() || target_group.is_empty() {
            return None;
        }
        let dimensions: DimensionSet = [
            (
                LOAD_BALANCER_DIMENSION.to_string(),
                load_balancer.to_string(),
            ),
            (
                TARGET_GROUP_DIMENSION.to_string(),
                format!("{TARGET_GROUP_PREFIX}{target_group}"),
            ),
        ]
        .into_iter()
        .collect();
        return Some(MetricSeries::new(
            APPLICATION_ELB_NAMESPACE,
            ALB_REQUEST_COUNT_METRIC,
            dimensions,
        ));
    }
    let (_, metric_name) = PREDEFINED_LOAD_METRICS
        .iter()
        .find(|(load_metric_type, _)| *load_metric_type == metric_type)?;
    let group = instruction
        .resource_id
        .strip_prefix(AUTO_SCALING_GROUP_PREFIX)
        .filter(|group| !group.is_empty())?;
    let dimensions: DimensionSet = [(
        AUTO_SCALING_GROUP_DIMENSION.to_string(),
        group.to_string(),
    )]
    .into_iter()
    .collect();
    Some(MetricSeries::new(
        EC2_NAMESPACE,
        *metric_name,
        dimensions,
    ))
}

/// The load a resource's own past shows, one sum per forecast interval it was observed in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadHistory {
    /// `(the hour of day an interval starts in, the sum observed in it, how many such
    /// intervals)`, folded into the running total and count of each hour of day — which is
    /// all a prediction for an hour of day reads, and bounds the struct at one entry per
    /// hour of the day whatever the history's size.
    by_hour_of_day: [(f64, u32); HOURS_PER_DAY as usize],
}

impl LoadHistory {
    /// Whether this emulator holds no load-metric history at all for the metric, which is a
    /// resource there is nothing to forecast for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_hour_of_day.iter().all(|(_, count)| *count == 0)
    }

    /// Fold one observed interval into the history.
    ///
    /// The hour of day is what indexes the entries, and [`hour_of_day`] answers one of the
    /// day's hours for every instant — the lookup still goes through `get_mut`, so an instant
    /// no hour of the day could be read off is dropped rather than reaching an index.
    fn observe(&mut self, interval_start_secs: i64, sum: f64) {
        let entry = usize::try_from(hour_of_day(interval_start_secs))
            .ok()
            .and_then(|hour| self.by_hour_of_day.get_mut(hour));
        if let Some((total, count)) = entry {
            *total += sum;
            *count = count.saturating_add(1);
        }
    }

    /// The mean load the history holds for `hour` of the day, when it holds any.
    fn mean_at(&self, hour: i64) -> Option<f64> {
        let (total, count) = usize::try_from(hour)
            .ok()
            .and_then(|hour| self.by_hour_of_day.get(hour))
            .copied()?;
        (count > 0).then(|| total / f64::from(count))
    }

    /// The mean load over every hour the history covers.
    fn mean(&self) -> Option<f64> {
        let (total, count) = self
            .by_hour_of_day
            .iter()
            .fold((0.0, 0_u32), |(total, count), (hour_total, hour_count)| {
                (total + hour_total, count.saturating_add(*hour_count))
            });
        (count > 0).then(|| total / f64::from(count))
    }

    /// The load forecast for the forecast interval starting at `interval_start_secs`.
    ///
    /// AWS does not publish the model it predicts WITH; what its documentation states is what
    /// the prediction is computed FROM — the load metric's historical data points, summed per
    /// hour. So the prediction for an hour is the load this history holds for the SAME HOUR OF
    /// DAY, averaged over the days it covers: the daily cycle of the resource's own past
    /// rather than an invented one. An hour of day the history says nothing about takes the
    /// mean over the whole history, and a history that holds nothing predicts nothing
    /// ([`LoadHistory::is_empty`], which is what empties the answer instead).
    ///
    /// Deterministic in the history and the hour, so a second read of one window never
    /// contradicts the first.
    #[must_use]
    pub fn predicted_load(&self, interval_start_secs: i64) -> f64 {
        self.mean_at(hour_of_day(interval_start_secs))
            .or_else(|| self.mean())
            .unwrap_or_default()
    }
}

/// The hour of day, in UTC, of an instant given as whole seconds since the epoch.
fn hour_of_day(at_secs: i64) -> i64 {
    at_secs
        .div_euclid(FORECAST_INTERVAL_SECONDS)
        .rem_euclid(HOURS_PER_DAY)
}

/// The history this emulator holds for the load metric `instruction` names, over the
/// [`HISTORY_LOOKBACK_DAYS`] before `now_secs`.
///
/// Empty — so nothing is forecast — for an instruction that configures no predictive scaling,
/// for a metric nothing has published, and for an engine this emulator holds no shared
/// directory in.
#[must_use]
pub fn load_history<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instruction: &ScalingInstruction,
    now_secs: i64,
) -> LoadHistory {
    let mut history = LoadHistory::default();
    let Some(series) = load_metric_series(instruction) else {
        return history;
    };
    let Some(directory) = ::service_common::published_metric_data::published_metric_data(engine)
    else {
        return history;
    };
    let earliest = now_secs.saturating_sub(HISTORY_LOOKBACK_DAYS * SECONDS_PER_DAY);
    // One sum per forecast interval, the granularity the forecast is reported at, folded into
    // the hour of day that interval belongs to.
    let mut intervals: ::std::collections::BTreeMap<i64, f64> = ::std::collections::BTreeMap::new();
    for observation in directory.lock().observations(&series, earliest, now_secs) {
        let interval = observation
            .at_secs
            .div_euclid(FORECAST_INTERVAL_SECONDS)
            .saturating_mul(FORECAST_INTERVAL_SECONDS);
        *intervals.entry(interval).or_default() += observation.sum;
    }
    for (interval_start, sum) in intervals {
        history.observe(interval_start, sum);
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{
        CustomizedLoadMetricSpecification, MetricDimension, PredefinedLoadMetricSpecification,
    };

    /// One instruction over the Auto Scaling group `resource_id`.
    fn instruction(resource_id: &str) -> ScalingInstruction {
        ScalingInstruction {
            service_namespace: "autoscaling".to_string(),
            resource_id: resource_id.to_string(),
            scalable_dimension: "autoscaling:autoScalingGroup:DesiredCapacity".to_string(),
            ..ScalingInstruction::default()
        }
    }

    /// The same instruction carrying the predefined load metric `metric_type`.
    fn with_predefined(
        resource_id: &str,
        metric_type: &str,
        resource_label: Option<&str>,
    ) -> ScalingInstruction {
        ScalingInstruction {
            predefined_load_metric_specification: Some(PredefinedLoadMetricSpecification {
                predefined_load_metric_type: metric_type.to_string(),
                resource_label: resource_label.map(str::to_string),
            }),
            ..instruction(resource_id)
        }
    }

    /// The metric identity a row expects: `(namespace, metric name, dimension set)`.
    type NamedMetric<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

    /// RULE: a predefined load metric names the metric AWS publishes it as, keyed to the
    /// resource the instruction applies to; a customized one carries that identity itself;
    /// an instruction carrying neither names no metric.
    #[test]
    fn a_load_metric_specification_names_the_published_metric() {
        let customized = ScalingInstruction {
            customized_load_metric_specification: Some(CustomizedLoadMetricSpecification {
                metric_name: "Requests".to_string(),
                namespace: "Workload".to_string(),
                dimensions: Some(vec![MetricDimension {
                    name: "Group".to_string(),
                    value: "one".to_string(),
                }]),
                statistic: "Sum".to_string(),
                unit: None,
            }),
            ..instruction("autoScalingGroup/my-asg")
        };
        // (case, instruction, the (namespace, metric name, dimensions) it names)
        let rows: &[(&str, ScalingInstruction, Option<NamedMetric<'_>>)] = &[
            (
                "the total CPU utilization of a group",
                with_predefined("autoScalingGroup/my-asg", "ASGTotalCPUUtilization", None),
                Some((
                    "AWS/EC2",
                    "CPUUtilization",
                    &[("AutoScalingGroupName", "my-asg")],
                )),
            ),
            (
                "the total network in of another group",
                with_predefined("autoScalingGroup/other", "ASGTotalNetworkIn", None),
                Some((
                    "AWS/EC2",
                    "NetworkIn",
                    &[("AutoScalingGroupName", "other")],
                )),
            ),
            (
                "the total network out",
                with_predefined("autoScalingGroup/my-asg", "ASGTotalNetworkOut", None),
                Some((
                    "AWS/EC2",
                    "NetworkOut",
                    &[("AutoScalingGroupName", "my-asg")],
                )),
            ),
            (
                "an Application Load Balancer target group's request count",
                with_predefined(
                    "autoScalingGroup/my-asg",
                    "ALBTargetGroupRequestCount",
                    Some("app/my-lb/1234567890abcdef/targetgroup/my-tg/abcdef1234567890"),
                ),
                Some((
                    "AWS/ApplicationELB",
                    "RequestCount",
                    &[
                        ("LoadBalancer", "app/my-lb/1234567890abcdef"),
                        ("TargetGroup", "targetgroup/my-tg/abcdef1234567890"),
                    ],
                )),
            ),
            (
                "that request count with no resource label to key it to",
                with_predefined("autoScalingGroup/my-asg", "ALBTargetGroupRequestCount", None),
                None,
            ),
            (
                "a customized specification",
                customized,
                Some(("Workload", "Requests", &[("Group", "one")])),
            ),
            (
                "an instruction configuring no predictive scaling",
                instruction("autoScalingGroup/my-asg"),
                None,
            ),
        ];
        for (case, instruction, expected) in rows {
            let named = load_metric_series(instruction);
            let wanted = expected.map(|(namespace, metric_name, dimensions)| {
                MetricSeries::new(
                    namespace,
                    metric_name,
                    dimensions
                        .iter()
                        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                        .collect(),
                )
            });
            assert_eq!(
                named, wanted,
                "case {case:?}: expected the metric {wanted:?}, got {named:?}"
            );
        }
    }

    /// RULE: the prediction for an hour is the mean of the load observed in the same hour of
    /// day, and an hour of day the history says nothing about takes the mean of the whole
    /// history.
    #[test]
    fn a_prediction_is_the_load_the_same_hour_of_day_held() {
        let mut history = LoadHistory::default();
        // Two days of history at two hours of the day: 10 and 30 at hour 0, 20 at hour 5.
        for (hour_of_day, day, sum) in [(0_i64, 0_i64, 10.0), (0, 1, 30.0), (5, 0, 20.0)] {
            history.observe(
                day * HOURS_PER_DAY * FORECAST_INTERVAL_SECONDS
                    + hour_of_day * FORECAST_INTERVAL_SECONDS,
                sum,
            );
        }
        // (case, the hour of day predicted for, the load expected)
        let rows: &[(&str, i64, f64)] = &[
            ("an hour of day observed on two days", 0, 20.0),
            ("an hour of day observed once", 5, 20.0),
            ("an hour of day the history never covered", 13, 20.0),
        ];
        for (case, hour, expected) in rows {
            let predicted = history.predicted_load(hour * FORECAST_INTERVAL_SECONDS);
            assert!(
                (predicted - expected).abs() < 1e-9,
                "case {case:?}: expected a predicted load of {expected}, got {predicted}"
            );
        }
        assert!(
            !history.is_empty(),
            "case a history holding observations: expected it not to read as empty"
        );
        assert!(
            LoadHistory::default().is_empty(),
            "case a history holding nothing: expected it to read as empty"
        );
    }

    /// RULE: a history holding nothing predicts nothing, and the mean of an unobserved hour
    /// of day is undefined rather than zero, so a forecast is issued only where history is
    /// held.
    #[test]
    fn an_unpublished_metric_holds_no_history() {
        let engine = ::engine::InMemoryEngine::new();
        let instruction = with_predefined("autoScalingGroup/my-asg", "ASGTotalCPUUtilization", None);
        let history = load_history(&engine, &instruction, 1_000_000);
        assert!(
            history.is_empty(),
            "case a metric nothing published: expected no history, got {history:?}"
        );
    }

    /// RULE: the history read is the published observations summed per forecast interval,
    /// taken from inside the lookback span and nowhere else.
    #[test]
    fn the_history_read_is_the_published_observations_of_the_lookback_span() {
        let engine = ::engine::InMemoryEngine::new();
        let instruction = with_predefined("autoScalingGroup/my-asg", "ASGTotalCPUUtilization", None);
        let series = load_metric_series(&instruction).expect("the instruction names a metric");
        let now_secs = 100 * HOURS_PER_DAY * FORECAST_INTERVAL_SECONDS;
        let directory = ::service_common::published_metric_data::published_metric_data(&engine)
            .expect("the shared directory");
        // Two observations inside one hour of the span, and one before the span opens.
        let inside = now_secs - FORECAST_INTERVAL_SECONDS;
        directory.lock().publish(series.clone(), inside, 4.0);
        directory.lock().publish(series.clone(), inside + 60, 6.0);
        directory.lock().publish(
            series,
            now_secs - (HISTORY_LOOKBACK_DAYS + 1) * SECONDS_PER_DAY,
            1_000.0,
        );
        let history = load_history(&engine, &instruction, now_secs);
        let predicted = history.predicted_load(inside);
        assert!(
            (predicted - 10.0).abs() < 1e-9,
            "case two observations in one interval: expected their sum of 10, got {predicted}"
        );
    }
}
