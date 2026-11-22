// SPDX-License-Identifier: CC0-1.0

//! The forecast a predictive scaling policy publishes: what `GetPredictiveScalingForecast`
//! reports for a policy of type `PredictiveScaling`.
//!
//! # The rule this module implements
//!
//! "Retrieves the forecast data for a predictive scaling policy. Load forecasts are
//! predictions of the hourly load values using historical load data from CloudWatch and an
//! analysis of historical trends. Capacity forecasts are represented as predicted values for
//! the minimum capacity that is needed on an hourly basis, based on the hourly load forecast.
//! A minimum of 24 hours of data is required to create the initial forecasts."
//! (`smithy_query(query="operation",operation="GetPredictiveScalingForecast")`). So a
//! forecast is COMPUTED from observations the account already holds, and it is computed the
//! same way for every policy:
//!
//! * the load is read from the series the policy's own metric specification names — the
//!   CloudWatch metric a predefined type IS, or the one a customized specification's metric
//!   data query names — out of the account's own CloudWatch store, which is the store the
//!   observations live in ([`load_series`]);
//! * those observations are folded into the WHOLE HOURS they fall in, because the forecast
//!   is hourly: one hour's load is that hour's statistic of the series, aggregated by
//!   CloudWatch's OWN fold ([`cloudwatch::BasicStatistics`], the one `GetMetricStatistics`
//!   aggregates a period's window with), so a load and a metric read cannot disagree
//!   ([`hourly_history`]);
//! * the hours observed give the daily pattern the forecast is made from — the mean load of
//!   each hour of the day ([`daily_profile`]) — and each hour of the requested range is
//!   forecast as the pattern's value for the hour of the day it falls in. Fewer than
//!   [`MIN_HISTORY_PERIODS`] hours observed is less than the minimum the operation documents,
//!   so such a policy has no PATTERN to forecast from: the hours it was asked for are still
//!   the hours it reports, and every one of them carries NO VALUE (not a number) rather than
//!   a load invented from a few observations — which is what a forecast of a policy created
//!   moments ago reads as, and what an hour of the day the pattern never observed reads as
//!   too;
//! * the capacity each hour needs is the smallest that keeps every metric specification at
//!   its own `TargetValue` ([`PredictiveScalingMetricSpecification$TargetValue`]), held
//!   between the group's own `MinSize` and — unless the policy may raise it
//!   ([`super::scaling_policies::DEFAULT_MAX_CAPACITY_BREACH_BEHAVIOR`]) — its `MaxSize`; an
//!   hour no load was predicted for needs no number either, so the two series always cover
//!   the same hours.
//!
//! Nothing here writes: a forecast is a report of what the store holds, and two reads taken
//! at one instant report the same series. A specification naming a form this forecast cannot
//! compute — a metric math expression, which names no single series, or an extended
//! (percentile) statistic, which needs the individual published values a stored aggregate
//! does not carry — is answered as the unimplemented form it is ([`ForecastRefusal`]) rather
//! than with a number aggregated some other way.
//!
//! # What one read costs
//!
//! A forecast is one series per metric specification over the hours the request asked for, so
//! both factors are bounded and neither is a caller's to choose freely: the span the request
//! may ask for is taken against the rules its own two members state before anything is read
//! ([`time_range_refusal`]) and the period lattice is held to [`MAX_FORECAST_RANGE`] as the
//! backstop behind that, while how many specifications a policy may hold is bounded where the
//! policy is WRITTEN ([`super::scaling_policies::MAX_PREDICTIVE_METRIC_SPECIFICATIONS`]).
//! Specifications naming the same series are folded ONCE and share the result ([`forecast`]),
//! so no list of them multiplies the pass over a series' observations.
//!
//! LOCKING. Neither of this process's stores is locked by the caller of [`forecast`]: it is
//! handed the policy and the group's bounds, and takes CloudWatch's lock itself for the one
//! pass that READS the observations, releasing it when that pass ends — so a forecast is
//! computed with this service's own store free for every other request. That is the order
//! this crate takes the two stores in everywhere ([`super::scaling_policies`]).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use crate::ops::types::{
    CapacityForecast, LoadForecast, MetricDataQuery, MetricStat,
    PredictiveScalingMetricSpecification,
};

use super::scaling_policies::{
    resource_label_dimensions, StoredScalingPolicy, TrackedSeries,
    APPLICATION_ELB_METRIC_NAMESPACE, DEFAULT_MAX_CAPACITY_BREACH_BEHAVIOR,
    EC2_METRIC_NAMESPACE, POLICY_TYPE_PREDICTIVE_SCALING,
};
use super::StoredGroup;

/// The bounds a forecast capacity is held between: the group's own two sizes, read off the
/// group under this service's store lock so the forecast itself is computed with that lock
/// released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCapacityBounds {
    /// The group's `MinSize`, which a forecast capacity is never below.
    pub min_size: i32,
    /// The group's `MaxSize`, which it is never above while the policy honours it.
    pub max_size: i32,
}

impl GroupCapacityBounds {
    /// The bounds `group` is configured with.
    #[must_use]
    pub fn of(group: &StoredGroup) -> Self {
        Self {
            min_size: group.min_size,
            max_size: group.max_size,
        }
    }
}

/// How wide one forecast period is: the hour the operation's own documentation forecasts in
/// ("predictions of the HOURLY load values", "the minimum capacity that is needed on an
/// HOURLY basis").
pub const FORECAST_PERIOD: Duration = Duration::from_secs(60 * 60);

/// How many forecast periods one day holds, which is the pattern a forecast is made from.
const PERIODS_PER_DAY: i64 = 24;

/// The fewest observed periods a forecast can be made from: "A minimum of 24 hours of data is
/// required to create the initial forecasts" (`GetPredictiveScalingForecast`).
pub const MIN_HISTORY_PERIODS: usize = 24;

/// How long the span one request may ask for is, in days: "The maximum time duration between
/// the start and end time is 30 days" (`GetPredictiveScalingForecastType$EndTime`).
pub const MAX_FORECAST_RANGE_DAYS: u64 = 30;

/// That span as a duration. A request past it is REFUSED
/// ([`time_range_refusal`]); the period lattice is held to it as well
/// ([`forecast_periods`]), as the backstop that bounds this read whatever reaches it.
pub const MAX_FORECAST_RANGE: Duration =
    Duration::from_secs(MAX_FORECAST_RANGE_DAYS * 24 * 60 * 60);

/// How far back a request's start time may reach: "At most, the date and time can be one year
/// before the current date and time." (`GetPredictiveScalingForecastType$StartTime`).
pub const MAX_FORECAST_LOOKBACK: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// The sentence a request whose span is longer than [`MAX_FORECAST_RANGE`] is refused with:
/// the rule as `GetPredictiveScalingForecastType$EndTime` states it, with the maximum in
/// force as its one variable part.
#[must_use]
pub fn too_long_a_range_message() -> String {
    format!(
        "The maximum time duration between the start and end time is \
         {MAX_FORECAST_RANGE_DAYS} days."
    )
}

/// The sentence a request whose start time reaches further back than
/// [`MAX_FORECAST_LOOKBACK`] is refused with: the rule as
/// `GetPredictiveScalingForecastType$StartTime` states it. Fixed text — the defect is how far
/// back the instant is rather than which instant it is.
pub const START_TIME_TOO_OLD_MESSAGE: &str =
    "At most, the date and time can be one year before the current date and time.";

/// The sentence a request whose range the rules of its two members leave no room in is
/// refused with, or `None` for a range the forecast is computed over.
///
/// Every rule is the operation's own members' ([`MAX_FORECAST_RANGE`],
/// [`MAX_FORECAST_LOOKBACK`]), and the range is a fact of the REQUEST, so it is taken before
/// anything is read: the span the two instants bound is what the forecast's size is, so no
/// caller decides how much work one read costs. A window that does not open is refused with
/// the sentence this service states for that class wherever it states it
/// ([`super::END_TIME_MESSAGE`]).
#[must_use]
pub fn time_range_refusal(
    start: SystemTime,
    end: SystemTime,
    now: SystemTime,
) -> Option<String> {
    let Some(span) = end.duration_since(start).ok().filter(|span| !span.is_zero()) else {
        return Some(super::END_TIME_MESSAGE.to_string());
    };
    if span > MAX_FORECAST_RANGE {
        return Some(too_long_a_range_message());
    }
    if now
        .duration_since(start)
        .is_ok_and(|behind| behind > MAX_FORECAST_LOOKBACK)
    {
        return Some(START_TIME_TOO_OLD_MESSAGE.to_string());
    }
    None
}

/// The `Stat` a load series is read at when the specification names none: the load of an hour
/// is the TOTAL observed in it, which is what each predefined load metric is a total of ("the
/// Auto Scaling group's total CPU metric is used as the load metric",
/// `PredictiveScalingPredefinedMetricPair$PredefinedMetricType`).
const LOAD_STATISTIC: &str = "Sum";

/// The refusal of a policy name that resolves to a policy of another type.
///
/// This operation reports the forecast OF a predictive scaling policy, so a name that
/// resolves to a policy of any other type names nothing this operation can answer for. The
/// policy name, taken verbatim from the request, is the sentence's only variable part.
#[must_use]
pub fn not_a_predictive_scaling_policy_message(policy_name: &str) -> String {
    format!("Policy {policy_name} is not a predictive scaling policy.")
}

/// The refusal of a policy NAME the group a request names holds no policy under, as this
/// operation states it.
///
/// The same subject as this service's other sentence about a policy a request names
/// ([`super::policy_execution::absent_policy_message`]) — the resource word, the name asked
/// for verbatim, and that nothing answers to it — closed as a SENTENCE, the way this
/// operation states its refusals about a policy
/// ([`not_a_predictive_scaling_policy_message`]). The name is its only variable part.
#[must_use]
pub fn absent_policy_message(policy_name: &str) -> String {
    format!(
        "{}.",
        super::policy_execution::absent_policy_message(policy_name)
    )
}

/// Whether `policy` is the type this operation reports a forecast for.
#[must_use]
pub fn is_predictive_scaling(policy: &StoredScalingPolicy) -> bool {
    policy.policy_type == POLICY_TYPE_PREDICTIVE_SCALING
}

/// One policy's forecast: the load forecast of each of its metric specifications, and the one
/// capacity forecast they together imply.
#[derive(Debug, Clone, Default)]
pub struct Forecast {
    /// One entry per metric specification of the policy, in the order the policy states them
    /// — "The load forecast" the answer reports.
    pub load: Vec<LoadForecast>,
    /// "The capacity forecast": the minimum capacity each forecast hour needs.
    pub capacity: CapacityForecast,
}

/// Why a forecast could not be computed for a metric specification: the form it names is one
/// this forecast does not implement, rather than anything about the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForecastRefusal {
    /// The specification names no single CloudWatch series: a customized metric whose queries
    /// drive the load from an expression or from no one result
    /// ([`driving_metric_stat`]), or a predefined metric type outside the sets the model's own
    /// enumerations hold.
    NoSingleSeries,
    /// The series is aggregated by a statistic the stored aggregates do not define \u2014 an
    /// extended (percentile) statistic needs the individual published values
    /// ([`cloudwatch::BasicStatistics`] holds the basic five only); carries the statistic the
    /// specification named.
    UnsupportedStatistic(String),
}

impl ForecastRefusal {
    /// What of this operation is not implemented for such a policy, as the clause that
    /// follows the operation's own name in the refusal.
    #[must_use]
    pub fn unimplemented_form(&self) -> String {
        match self {
            Self::NoSingleSeries => {
                "for a metric specification that names no single CloudWatch series".to_string()
            }
            Self::UnsupportedStatistic(statistic) => format!(
                "for a load metric aggregated with the statistic {statistic}"
            ),
        }
    }
}

/// The forecast `policy` publishes over `[start, end)`, computed from the observations the
/// account's CloudWatch store holds for the metrics the policy names.
///
/// Reports only: nothing is written, here or in CloudWatch. Call it with this service's own
/// store lock RELEASED — it needs nothing of it but `bounds` and the policy. Answers
/// [`ForecastRefusal`] for a specification naming a form this forecast does not implement, so
/// no such policy is answered with a number aggregated some other way.
pub fn forecast<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group_name: &str,
    bounds: GroupCapacityBounds,
    policy: &StoredScalingPolicy,
    start: SystemTime,
    end: SystemTime,
) -> Result<Forecast, ForecastRefusal> {
    let specifications: Vec<PredictiveScalingMetricSpecification> = policy
        .predictive_scaling_configuration
        .iter()
        .flat_map(|configuration| configuration.metric_specifications.clone())
        .collect();
    // The series each specification names, resolved BEFORE anything is read: a form this
    // forecast does not implement is answered as such rather than forecast from another
    // aggregate.
    let series: Vec<TrackedSeries> = specifications
        .iter()
        .map(|specification| load_series(group_name, specification))
        .collect::<Result<_, _>>()?;
    let periods = forecast_periods(start, end);
    // The daily pattern each DISTINCT series gives, read in ONE pass over the CloudWatch
    // store: two specifications naming one series fold it once and share the pattern, and a
    // series the account holds too little of has no pattern, and so no forecast.
    let patterns: BTreeMap<SeriesIdentity, Option<BTreeMap<i64, f64>>> = {
        let history = cloudwatch::service_state(engine);
        let store = history.as_ref().map(|state| state.lock());
        series
            .iter()
            .map(|series| {
                let pattern = store
                    .as_ref()
                    .and_then(|store| daily_profile(&hourly_history(store, series)));
                (series_identity(series), pattern)
            })
            .collect()
    };
    // Per specification, the pattern of the series it names: the lookup is by the series'
    // identity, so the order the specifications are stated in is the order they are reported
    // in whichever of them share a series.
    let per_specification: Vec<Option<&BTreeMap<i64, f64>>> = series
        .iter()
        .map(|series| patterns.get(&series_identity(series)).and_then(Option::as_ref))
        .collect();

    let load: Vec<LoadForecast> = specifications
        .iter()
        .zip(&per_specification)
        .map(|(specification, pattern)| {
            let mut forecast = LoadForecast {
                metric_specification: specification.clone(),
                ..LoadForecast::default()
            };
            for period in &periods {
                let predicted = pattern
                    .and_then(|pattern| pattern.get(&period_of_day(*period)).copied());
                // Every forecast period is reported, whether or not the pattern states a
                // load for it: a forecast made from too little data — or from none — covers
                // the hours it was asked for and carries NO VALUE for them rather than
                // dropping them, so a reader sees which hours were forecast and that nothing
                // was predicted for them.
                forecast.timestamps.push(super::to_date_time(*period));
                forecast.values.push(predicted.unwrap_or(f64::NAN));
            }
            forecast
        })
        .collect();

    let mut capacity = CapacityForecast::default();
    for period in &periods {
        let needed =
            needed_capacity(bounds, policy, &specifications, &per_specification, *period);
        // The capacity of a period no load was predicted for is as unpredicted as that
        // load, so the two series cover the same hours.
        capacity.timestamps.push(super::to_date_time(*period));
        capacity.values.push(needed.unwrap_or(f64::NAN));
    }
    Ok(Forecast { load, capacity })
}

/// What makes two [`TrackedSeries`] the SAME series read the same way: the metric it names,
/// the dimensions it is published under and the statistic it is read at.
type SeriesIdentity = (String, String, Vec<(String, String)>, String);

/// The identity of one series, for folding the specifications that share it once.
fn series_identity(series: &TrackedSeries) -> SeriesIdentity {
    (
        series.namespace.clone(),
        series.metric_name.clone(),
        series.dimensions.clone(),
        series.statistic.clone(),
    )
}

/// The whole hours of `[start, end)` a forecast covers, in order.
///
/// The hours are the lattice of whole hours from the epoch, so the periods of two reads of
/// one range are the same instants — and the first of them is the whole hour at or AFTER the
/// range's start, because the range is read from its "inclusive start time" to its "exclusive
/// end time" (`GetPredictiveScalingForecastType$StartTime`, `$EndTime`): an hour that began
/// before the start is not an hour the range holds. The lattice is held to
/// [`MAX_FORECAST_RANGE`] from its own start: a request past that span is REFUSED before this
/// is reached
/// ([`time_range_refusal`]), so this is the backstop that bounds what one read computes
/// whatever reaches it.
fn forecast_periods(start: SystemTime, end: SystemTime) -> Vec<SystemTime> {
    let last = end.min(start.checked_add(MAX_FORECAST_RANGE).unwrap_or(end));
    let mut periods = Vec::new();
    let mut period = first_period_from(start);
    while period < last {
        periods.push(period);
        period = match period.checked_add(FORECAST_PERIOD) {
            Some(next) => next,
            None => break,
        };
    }
    periods
}

/// The first whole period at or after `at`: the one a range beginning at `at` opens with.
fn first_period_from(at: SystemTime) -> SystemTime {
    let whole = whole_period(at);
    if whole < at {
        whole.checked_add(FORECAST_PERIOD).unwrap_or(at)
    } else {
        whole
    }
}

/// `at` rounded DOWN to the whole forecast period it falls in.
fn whole_period(at: SystemTime) -> SystemTime {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let width = FORECAST_PERIOD.as_secs();
    UNIX_EPOCH
        .checked_add(Duration::from_secs(since - since % width))
        .unwrap_or(at)
}

/// Which period of the DAY `at` falls in, which is what one period of the forecast is
/// predicted from.
fn period_of_day(at: SystemTime) -> i64 {
    period_index(at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64())
        .rem_euclid(PERIODS_PER_DAY)
}

/// The index of the forecast period the epoch instant `secs` falls in.
#[allow(clippy::cast_possible_truncation)]
fn period_index(secs: f64) -> i64 {
    #[allow(clippy::cast_precision_loss)]
    let width = FORECAST_PERIOD.as_secs() as f64;
    (secs / width).floor() as i64
}

/// The CloudWatch series the LOAD of one metric specification is observed in.
///
/// A predefined type IS a documented series of one service's resources, and a metric PAIR
/// names the load metric of that pair: "There are two different types of metrics for each
/// metric type: one is a load metric and one is a scaling metric. For example, if the metric
/// type is `ASGCPUUtilization`, the Auto Scaling group's total CPU metric is used as the load
/// metric" (`PredictiveScalingPredefinedMetricPair$PredefinedMetricType`) — so both forms
/// resolve to the same Amazon EC2 or Application Load Balancer series, read as the TOTAL over
/// the hour. A customized load metric states its series itself, as a metric data query whose
/// `MetricStat` names the metric and the statistic it is graded by.
///
/// Answers [`ForecastRefusal`] for a specification this forecast cannot read a series for:
/// one that is a metric math expression rather than a named metric, a predefined type outside
/// the model's own enumerations, or a series aggregated by a statistic the stored aggregates
/// do not define.
pub fn load_series(
    group_name: &str,
    specification: &PredictiveScalingMetricSpecification,
) -> Result<TrackedSeries, ForecastRefusal> {
    let series = named_load_series(group_name, specification)
        .ok_or(ForecastRefusal::NoSingleSeries)?;
    // The hourly load is CloudWatch's own aggregate of that hour's observations
    // ([`cloudwatch::BasicStatistics`]), which is defined for the basic statistics only: an
    // extended (percentile) statistic needs the individual published values, which a stored
    // aggregate does not carry. AWS accepts such a `Stat`, so it is answered as the
    // unimplemented form it is rather than aggregated some other way.
    if !::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::STATISTIC_VALUES
        .contains(&series.statistic.as_str())
    {
        return Err(ForecastRefusal::UnsupportedStatistic(
            series.statistic.clone(),
        ));
    }
    Ok(series)
}

/// The series one specification NAMES, before it is weighed against what this forecast can
/// read: `None` for a specification that names no single series.
fn named_load_series(
    group_name: &str,
    specification: &PredictiveScalingMetricSpecification,
) -> Option<TrackedSeries> {
    if let Some(customized) = specification.customized_load_metric_specification.as_ref() {
        let stat = driving_metric_stat(&customized.metric_data_queries)?;
        return Some(TrackedSeries {
            namespace: stat.metric.namespace.clone(),
            metric_name: stat.metric.metric_name.clone(),
            statistic: stat.stat.clone(),
            unit: stat.unit.clone(),
            period: period_seconds(),
            dimensions: stat
                .metric
                .dimensions
                .iter()
                .flatten()
                .map(|dimension| (dimension.name.clone(), dimension.value.clone()))
                .collect(),
        });
    }
    let (metric_type, resource_label) = specification
        .predefined_load_metric_specification
        .as_ref()
        .map(|predefined| {
            (
                predefined.predefined_metric_type.as_str(),
                predefined.resource_label.as_deref(),
            )
        })
        .or_else(|| {
            specification
                .predefined_metric_pair_specification
                .as_ref()
                .map(|pair| {
                    (
                        pair.predefined_metric_type.as_str(),
                        pair.resource_label.as_deref(),
                    )
                })
        })?;
    predefined_load_series(group_name, metric_type, resource_label)
}

/// [`FORECAST_PERIOD`] as the seconds a CloudWatch series states a period in.
fn period_seconds() -> i32 {
    i32::try_from(FORECAST_PERIOD.as_secs()).unwrap_or(i32::MAX)
}

/// The `MetricStat` the load a list of metric data queries states is DRIVEN by, or `None`
/// when the list drives it from anything but one named series.
///
/// "The metric data queries to use with this scaling policy" is a list of up to 10 structures
/// (`PredictiveScalingCustomizedLoadMetric$MetricDataQueries`), each of which is either a
/// named metric (`MetricDataQuery$MetricStat`) or an EXPRESSION over the others
/// (`$Expression`, "A math expression"), with `$ReturnData` saying which of them the result
/// is. A load computed by an expression is metric math — one of its sub-metrics is NOT the
/// load the policy is driven by, so a list carrying any expression names no single series
/// here, and neither does a list whose queries do not settle on ONE result:
///
/// * one query: that query is the result, and it has to be a named metric;
/// * several: exactly one of them may say it is the result (`ReturnData` true), and that one
///   has to be a named metric.
fn driving_metric_stat(queries: &[MetricDataQuery]) -> Option<&MetricStat> {
    if queries.iter().any(|query| query.expression.is_some()) {
        return None;
    }
    let driving = if queries.len() == 1 {
        queries.first()
    } else {
        let mut returned = queries
            .iter()
            .filter(|query| query.return_data == Some(true));
        let first = returned.next();
        // Two queries that both state they are the result state no one result.
        if returned.next().is_some() {
            return None;
        }
        first
    };
    driving?.metric_stat.as_ref()
}

/// The series one predefined metric type names, for both the load-metric and the metric-pair
/// spelling of it.
fn predefined_load_series(
    group_name: &str,
    metric_type: &str,
    resource_label: Option<&str>,
) -> Option<TrackedSeries> {
    let group_dimension = || {
        vec![(
            super::GROUP_NAME_DIMENSION.to_string(),
            group_name.to_string(),
        )]
    };
    let series = |namespace: &str, metric_name: &str, dimensions: Vec<(String, String)>| {
        Some(TrackedSeries {
            namespace: namespace.to_string(),
            metric_name: metric_name.to_string(),
            statistic: LOAD_STATISTIC.to_string(),
            unit: None,
            period: period_seconds(),
            dimensions,
        })
    };
    match metric_type {
        // The two spellings of the Auto Scaling group's own CPU load: the load metric of the
        // `ASGCPUUtilization` pair is the group's total CPU.
        "ASGTotalCPUUtilization" | "ASGCPUUtilization" => {
            series(EC2_METRIC_NAMESPACE, "CPUUtilization", group_dimension())
        }
        "ASGTotalNetworkIn" | "ASGNetworkIn" => {
            series(EC2_METRIC_NAMESPACE, "NetworkIn", group_dimension())
        }
        "ASGTotalNetworkOut" | "ASGNetworkOut" => {
            series(EC2_METRIC_NAMESPACE, "NetworkOut", group_dimension())
        }
        // The request count is counted per target GROUP, which is the resource the
        // specification's own `ResourceLabel` names.
        "ALBTargetGroupRequestCount" | "ALBRequestCount" => series(
            APPLICATION_ELB_METRIC_NAMESPACE,
            "RequestCount",
            resource_label_dimensions(resource_label.unwrap_or_default()),
        ),
        _ => None,
    }
}

/// The load each OBSERVED period of `series` held, keyed by the period's index from the
/// epoch.
///
/// The observations are the ones the account's CloudWatch store holds for the series, folded
/// into their own periods by CloudWatch's OWN fold ([`cloudwatch::BasicStatistics`] — the one
/// `GetMetricStatistics` aggregates a period's window with) and read at the statistic the
/// series is graded by, so what a forecast is made from is what a CloudWatch read of the same
/// series reports. A period whose observations do not define that statistic contributes no
/// load.
fn hourly_history(
    store: &cloudwatch::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    series: &TrackedSeries,
) -> BTreeMap<i64, f64> {
    let key = cloudwatch::MetricKey {
        namespace: series.namespace.clone(),
        metric_name: series.metric_name.clone(),
        dimensions: series.dimensions.iter().cloned().collect(),
    };
    let Some(data) = store.metrics.get(&key) else {
        return BTreeMap::new();
    };
    let mut periods: BTreeMap<i64, cloudwatch::BasicStatistics> = BTreeMap::new();
    for datum in data {
        periods
            .entry(period_index(datum.timestamp_secs))
            .or_default()
            .add(datum);
    }
    periods
        .into_iter()
        .filter_map(|(index, folded)| Some((index, folded.named(&series.statistic)?)))
        .collect()
}

/// The daily pattern `history` gives: the mean load of each period of the day.
///
/// `None` for a history shorter than [`MIN_HISTORY_PERIODS`] — the minimum the operation
/// documents a forecast can be made from — so a policy whose metric the account holds too
/// little of has no pattern at all, and every hour it is asked about carries no value instead
/// of a load made up from a few observations ([`forecast`]).
fn daily_profile(history: &BTreeMap<i64, f64>) -> Option<BTreeMap<i64, f64>> {
    if history.len() < MIN_HISTORY_PERIODS {
        return None;
    }
    let mut observed: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    for (index, value) in history {
        let slot = observed
            .entry(index.rem_euclid(PERIODS_PER_DAY))
            .or_insert((0.0, 0.0));
        slot.0 += value;
        slot.1 += 1.0;
    }
    Some(
        observed
            .into_iter()
            .filter(|(_, (_, count))| *count > 0.0)
            .map(|(slot, (total, count))| (slot, total / count))
            .collect(),
    )
}

/// The smallest capacity the group needs in the period `at`, or `None` when no specification
/// forecasts a load for it.
///
/// One specification's load at its own `TargetValue` is the capacity that specification asks
/// for, and the group needs the largest of them to hold every target at once. The capacity is
/// then held to the group's own bounds: never below its `MinSize`, and never above its
/// `MaxSize` unless the policy may raise that ceiling
/// (`PredictiveScalingConfiguration$MaxCapacityBreachBehavior`: "HonorMaxCapacity - Amazon
/// EC2 Auto Scaling can't increase the maximum capacity of the group when the forecast
/// capacity is close to or exceeds the maximum capacity").
fn needed_capacity(
    bounds: GroupCapacityBounds,
    policy: &StoredScalingPolicy,
    specifications: &[PredictiveScalingMetricSpecification],
    patterns: &[Option<&BTreeMap<i64, f64>>],
    at: SystemTime,
) -> Option<f64> {
    let slot = period_of_day(at);
    let mut needed: Option<f64> = None;
    for (specification, pattern) in specifications.iter().zip(patterns) {
        let Some(load) = pattern.and_then(|pattern| pattern.get(&slot)) else {
            continue;
        };
        // A target of zero — or one no arithmetic can read — states no capacity the load can
        // be divided into, so the specification asks for none.
        if specification.target_value <= 0.0 || !specification.target_value.is_finite() {
            continue;
        }
        let asked = (load / specification.target_value).ceil();
        needed = Some(needed.map_or(asked, |held: f64| held.max(asked)));
    }
    let honors_max_capacity = policy
        .predictive_scaling_configuration
        .as_ref()
        .and_then(|configuration| configuration.max_capacity_breach_behavior.as_deref())
        .unwrap_or(DEFAULT_MAX_CAPACITY_BREACH_BEHAVIOR)
        == DEFAULT_MAX_CAPACITY_BREACH_BEHAVIOR;
    needed.map(|needed| {
        let floor = f64::from(bounds.min_size);
        let ceiling = if honors_max_capacity {
            f64::from(bounds.max_size)
        } else {
            f64::INFINITY
        };
        needed.max(floor).min(ceiling)
    })
}
