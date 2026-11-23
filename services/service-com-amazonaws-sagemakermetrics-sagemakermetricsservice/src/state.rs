// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-sagemakermetrics-sagemakermetricsservice service.
//!
//! The service is a metric store: `BatchPutMetrics` appends raw data points to a trial
//! component's named series, and `BatchGetMetrics` aggregates those same points. Both
//! operations reach this one struct through [`service_state`], so a point written by one
//! request is read by every later one.

use std::collections::HashMap;

/// How many trial components this store holds metrics for.
///
/// The map is keyed on a caller-chosen `TrialComponentName`, and the store lives as long
/// as the process, so WITHOUT this cap a client looping `BatchPutMetrics` with fresh
/// names grows the emulator's memory until the host is exhausted. AWS publishes no
/// number for it, so the cap is chosen by this emulator; a name past it is refused with
/// the modelled `PutMetricsErrorCode::METRIC_LIMIT_EXCEEDED` for that datum's index.
pub const MAX_TRIAL_COMPONENTS: usize = 200;

/// How many distinct metric names one trial component may hold.
///
/// AWS publishes no number for this, so the cap is chosen by this emulator; it exists
/// because the modelled `PutMetricsErrorCode::METRIC_LIMIT_EXCEEDED` has to be reachable
/// at all — a store with no cap can never report it.
pub const MAX_METRIC_NAMES_PER_TRIAL_COMPONENT: usize = 40;

/// How many data points one `(trial component, metric name)` series may hold. Chosen by
/// this emulator for the same reason as [`MAX_METRIC_NAMES_PER_TRIAL_COMPONENT`].
///
/// The three caps multiply to the store's whole-process ceiling —
/// 200 x 40 x 2 000 = 16 000 000 points, a few hundred megabytes of
/// [`MetricDatum`] at worst — which is the size this emulator is willing to hold for a
/// test workload, not a number AWS states.
pub const MAX_DATA_POINTS_PER_SERIES: usize = 2_000;

/// How long a caller-chosen name this store will hold, in bytes — a `TrialComponentName`
/// or a `MetricName`, each of which becomes a map key held for the life of the process.
///
/// The caps above bound how MANY names the store holds, and this one bounds their SIZE:
/// without it a caller can grow the emulator's memory by the length of its names rather
/// than by their number, and the length of a name is not what the counting caps bound.
/// AWS publishes no number for it, so it is chosen by this emulator, an order of magnitude
/// above the longest name either modelled member admits (`ExperimentEntityName` is
/// `@length 1..120` and `MetricName` `@length 1..255`) — this service does not refuse a
/// name for those bounds (see `crate::ops::batch_put_metrics`), so the resource bound has to
/// be its own. A point naming something longer is refused for its own index with the
/// modelled `PutMetricsErrorCode::METRIC_LIMIT_EXCEEDED`, and stores nothing.
pub const MAX_NAME_BYTES: usize = 4_096;

/// How many ITEMS one request of either operation may carry: `MetricQueries` for a read,
/// `MetricData` for a write.
///
/// The model bounds a request at 100 queries and 10 data points, and this service does not
/// refuse a request for those bounds (see `crate::service::envelope`), so this cap is the
/// resource bound that takes their place: it is what keeps the WORK and the RESPONSE of one
/// call bounded, since a read evaluates every query it carries and each query may return up
/// to [`crate::ops::batch_get_metrics::MAX_RESULT_POINTS_PER_QUERY`] aggregated points. The
/// product is what it bounds — 200 x 1 000 = 200 000 points at worst for one response — and
/// it is chosen by this emulator, twice the modelled query maximum and twenty times the
/// modelled data-point maximum, so no request either recorded operation serves comes near
/// it.
pub const MAX_ITEMS_PER_REQUEST: usize = 200;

/// One raw metric data point, as `BatchPutMetrics` wrote it.
///
/// The instant is kept as the epoch SECOND plus the millisecond within it, which is the
/// `aws_smithy_types::DateTime` the request carried, quantised to the millisecond: every
/// instant that type can hold is storable (an epoch-millisecond count would overflow `i64`
/// for the extremes of it), the modelled `Timestamp` accepts instants before 1970 (which
/// `SystemTime` cannot hold on every platform without signed arithmetic), and integers make
/// the `BatchGetMetrics` period bucketing exact instead of float-rounded. Two points of one
/// series that differ only BELOW the millisecond therefore share a coordinate, so the
/// second is a re-write of the first (identical value) or a conflict (different value).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricDatum {
    /// `RawMetricData.Timestamp`'s epoch second (negative before 1970).
    pub epoch_second: i64,
    /// The millisecond within that second, `0..1000`.
    pub subsec_millis: u32,
    /// `RawMetricData.Step` — `None` when the caller omitted it.
    pub step: Option<i32>,
    /// `RawMetricData.Value`. Never `NaN`: a value with no position in any ordering is
    /// not stored, so every stored value is comparable and aggregatable. An UNBOUNDED
    /// value is a number and is stored as one, so aggregating a series has to be total
    /// over those two as well (`crate::ops::batch_get_metrics`).
    pub value: f64,
}

impl MetricDatum {
    /// The point's position on the timestamp axis, in whole epoch seconds.
    ///
    /// The stored second IS that position: a sub-second remainder is held separately, so
    /// nothing has to be floored here and a pre-1970 point keeps the second it was
    /// recorded in.
    #[must_use]
    pub fn epoch_seconds(&self) -> i64 {
        self.epoch_second
    }

    /// The point's identity within its series: the instant it was recorded at, to the
    /// millisecond, plus the iteration it belongs to. Two puts at the same coordinate
    /// describe the same point.
    fn coordinate(&self) -> (i64, u32, Option<i32>) {
        (self.epoch_second, self.subsec_millis, self.step)
    }
}

/// Every metric series one trial component holds.
#[derive(Debug, Default)]
pub struct TrialComponentMetrics {
    /// `BatchPutMetrics.TrialComponentName` as the first write spelled it; the map key in
    /// [`ComAmazonawsSagemakermetricsSagemakermetricsserviceState::components`] is the
    /// same string.
    pub name: String,
    /// `RawMetricData.MetricName` -> that series, sorted ascending by
    /// `(timestamp, step)` so bucketing and the `Last` statistic are well defined.
    pub series: HashMap<String, Vec<MetricDatum>>,
}

/// Per-service state for com-amazonaws-sagemakermetrics-sagemakermetricsservice. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsSagemakermetricsSagemakermetricsserviceState {
    /// Trial component name (the `BatchPutMetrics.TrialComponentName`, which is also the
    /// final path segment of a `MetricQuery.ResourceArn`) -> that component's metrics.
    pub components: HashMap<String, TrialComponentMetrics>,
}

/// Why one data point of a batch was not stored. The rest of the batch is unaffected:
/// `BatchPutMetrics` reports per-index failures and still persists every other point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRejection {
    /// Storing the point would take the store past [`MAX_TRIAL_COMPONENTS`], the trial
    /// component past [`MAX_METRIC_NAMES_PER_TRIAL_COMPONENT`] or the series past
    /// [`MAX_DATA_POINTS_PER_SERIES`], or it names something longer than
    /// [`MAX_NAME_BYTES`].
    MetricLimitExceeded,
    /// The series already holds a DIFFERENT value at this point's
    /// `(timestamp, step)` coordinate. Re-writing the same value there is accepted and
    /// changes nothing, so a retried batch is idempotent.
    Conflict,
}

impl ComAmazonawsSagemakermetricsSagemakermetricsserviceState {
    /// Append one data point to `trial_component_name`'s `metric_name` series, keeping
    /// the series sorted by `(timestamp, step)`.
    ///
    /// `Ok(())` means the series holds this point afterwards — either because it was
    /// inserted, or because the identical point was already there.
    ///
    /// EVERY cap is checked against the current store before any entry is created, so a
    /// rejected point leaves no trace: it can create neither an empty trial component
    /// nor an empty series.
    pub fn record(
        &mut self,
        trial_component_name: &str,
        metric_name: &str,
        datum: MetricDatum,
    ) -> Result<(), RecordRejection> {
        // A name longer than this store holds is refused before any entry is created: the
        // counting caps below bound how many names are held, not how long one may be.
        if trial_component_name.len() > MAX_NAME_BYTES || metric_name.len() > MAX_NAME_BYTES {
            return Err(RecordRejection::MetricLimitExceeded);
        }
        let component = self.components.get(trial_component_name);
        if component.is_none() && self.components.len() >= MAX_TRIAL_COMPONENTS {
            return Err(RecordRejection::MetricLimitExceeded);
        }
        if let Some(component) = component {
            if !component.series.contains_key(metric_name)
                && component.series.len() >= MAX_METRIC_NAMES_PER_TRIAL_COMPONENT
            {
                return Err(RecordRejection::MetricLimitExceeded);
            }
            // A series at its point cap still accepts a write to a coordinate it already
            // holds (that stores nothing new); anything else would grow it.
            if component.series.get(metric_name).is_some_and(|series| {
                series.len() >= MAX_DATA_POINTS_PER_SERIES
                    && series
                        .binary_search_by_key(&datum.coordinate(), MetricDatum::coordinate)
                        .is_err()
            }) {
                return Err(RecordRejection::MetricLimitExceeded);
            }
        }

        let component = self
            .components
            .entry(trial_component_name.to_string())
            .or_insert_with(|| TrialComponentMetrics {
                name: trial_component_name.to_string(),
                series: HashMap::new(),
            });
        let series = component.series.entry(metric_name.to_string()).or_default();
        match series.binary_search_by_key(&datum.coordinate(), MetricDatum::coordinate) {
            // Same coordinate: identical value is a no-op, a different value conflicts.
            Ok(at) if series[at].value == datum.value => Ok(()),
            Ok(_) => Err(RecordRejection::Conflict),
            Err(at) => {
                series.insert(at, datum);
                Ok(())
            }
        }
    }

    /// The stored points for one `(trial component, metric name)` pair, ascending by
    /// `(timestamp, step)`. Empty when nothing was ever written for that pair — an
    /// unknown resource or metric is a legitimate empty result, not a failure.
    #[must_use]
    pub fn series(&self, trial_component_name: &str, metric_name: &str) -> &[MetricDatum] {
        self.components
            .get(trial_component_name)
            .and_then(|component| component.series.get(metric_name))
            .map_or(&[], Vec::as_slice)
    }
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsSagemakermetricsSagemakermetricsserviceState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsSagemakermetricsSagemakermetricsserviceState>)
}
