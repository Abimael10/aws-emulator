// SPDX-License-Identifier: CC0-1.0

//! The metric series this service reports for a resource it holds: how a
//! requested window is cut into the datapoints a metric read answers with, and
//! what one sample of a metric is.
//!
//! This module OWNS this service's telemetry. Every metric reader —
//! `GetInstanceMetricData`, `GetRelationalDatabaseMetricData`,
//! `GetLoadBalancerMetricData`, `GetDistributionMetricData` and
//! `GetContainerServiceMetricData` — asks here rather than assembling a series
//! of its own, so the cutting rule and the per-metric sample live in one place
//! and anything else that has to read the same series asks the same question and
//! gets the same answer.
//!
//! Two parts, because they are two different rules:
//!
//! * [`MetricSeries::datapoints`] is the CUTTING rule, and is about time alone:
//!   `period` divides the time line into windows, each window the request
//!   overlaps that also holds a published sample of the resource carries one
//!   datapoint, and a window holding no sample carries none. It knows nothing
//!   about which metric it is cutting, and nothing about what was measured.
//! * what one sample IS belongs to the metric: [`UNPERFORMED_WORK_SAMPLE`] for
//!   an amount of work this emulator performed none of, and the resource's own
//!   provisioned capacity for a metric that reports one. Nothing here reports a
//!   measurement of work nothing performed.

use super::*;

use crate::ops::types::MetricDatapoint;

// ---------------------------------------------------------------------------
// The statistics a request selects
// ---------------------------------------------------------------------------

/// The members of the modelled `MetricStatistic` enum, as a request names them
/// in `statistics` — each selecting one member OF a datapoint.
///
/// Source: `smithy_query(query="shape",shape="MetricStatistic")`.
pub const STATISTIC_MINIMUM: &str = "Minimum";
/// See [`STATISTIC_MINIMUM`].
pub const STATISTIC_MAXIMUM: &str = "Maximum";
/// See [`STATISTIC_MINIMUM`].
pub const STATISTIC_SUM: &str = "Sum";
/// See [`STATISTIC_MINIMUM`].
pub const STATISTIC_AVERAGE: &str = "Average";
/// See [`STATISTIC_MINIMUM`].
pub const STATISTIC_SAMPLE_COUNT: &str = "SampleCount";

/// How many statistics one read can ask for: the members of the modelled
/// `MetricStatistic` enum, since each entry of `statistics` selects one of them
/// and selecting it twice selects nothing more.
///
/// The model puts no length on `MetricStatisticList`, so this is where a read's
/// own list is bounded.
pub const MAX_METRIC_STATISTICS: usize = 5;

// ---------------------------------------------------------------------------
// When a sample lands
// ---------------------------------------------------------------------------

/// The granularity an instance's status checks are published at, in seconds.
///
/// From the modelled `GetInstanceMetricDataRequest$period` documentation: "The
/// StatusCheckFailed, StatusCheckFailed_Instance, and StatusCheckFailed_System
/// instance metric data is available in 1-minute (60 seconds) granularity."
pub const STATUS_CHECK_GRANULARITY_SECONDS: i64 = 60;

/// The granularity every other instance metric is published at, in seconds.
///
/// From the same sentence: "All other instance metric data is available in
/// 5-minute (300 seconds) granularity."
pub const INSTANCE_METRIC_GRANULARITY_SECONDS: i64 = 300;

/// How long after a resource starts publishing its FIRST sample of a metric
/// lands.
///
/// CHOSEN, and the one span here that is: every later sample follows the
/// granularity the metric is published at, but a caller that polls for a metric
/// to appear cannot be made to wait a whole granularity for the first one. It is
/// deliberately NON-ZERO, so a caller reading immediately after the create still
/// sees the window it is in carry no sample yet, exactly as it does against AWS.
/// It is the same span this store's clock-owed transitions settle after
/// ([`STATE_TRANSITION_MILLIS`]), for the same reason.
pub const FIRST_SAMPLE_MILLIS: i64 = STATE_TRANSITION_MILLIS;

/// The granularity the instance metric `metric_name` is published at, in
/// seconds: see [`STATUS_CHECK_GRANULARITY_SECONDS`] and
/// [`INSTANCE_METRIC_GRANULARITY_SECONDS`].
pub fn instance_metric_granularity_seconds(metric_name: &str) -> i64 {
    match metric_name {
        "StatusCheckFailed" | "StatusCheckFailed_Instance" | "StatusCheckFailed_System" => {
            STATUS_CHECK_GRANULARITY_SECONDS
        }
        _ => INSTANCE_METRIC_GRANULARITY_SECONDS,
    }
}

/// The most windows one metric read walks, and so the most datapoints it
/// answers with.
///
/// A window and a granularity are both caller-sized, so their quotient is too: a
/// year of minutes is half a million windows. The walk takes the most RECENT
/// windows of the requested span, the ones a caller watching a metric is reading
/// for. CHOSEN — the model puts no length on `MetricDatapointList` — and matched
/// to the 1,440 datapoints one CloudWatch statistics call returns.
pub const MAX_METRIC_DATAPOINTS: usize = 1_440;

// ---------------------------------------------------------------------------
// Cutting a window into datapoints
// ---------------------------------------------------------------------------

/// What a metric read ASKS for, as the request carried it: which metric, in
/// which unit, over what window, at what granularity, as of when, and which
/// members of a datapoint the caller wants.
///
/// One struct because they travel together: every metric reader of this service
/// takes exactly these members, and the series owner needs all of them to answer.
#[derive(Debug, Clone, Copy)]
pub struct MetricRead<'a> {
    /// The metric being read, as the request named it.
    pub metric_name: &'a str,
    /// The unit the metric is published in; every datapoint carries it.
    pub unit: &'a str,
    /// `period`: "The granularity, in seconds, of the returned data points".
    pub period_seconds: i64,
    /// The requested window, in epoch milliseconds — the unit this store's
    /// clock reads.
    pub start_ms: i64,
    /// See [`MetricRead::start_ms`].
    pub end_ms: i64,
    /// Now. No window after it holds a sample: nothing has been observed in a
    /// window that has not happened.
    pub now_ms: i64,
    /// The `MetricStatistic` members the request asked for.
    pub statistics: &'a [String],
}

/// One metric read's question: from when the resource has been publishing, how
/// often it publishes, and what the request asked for.
#[derive(Debug, Clone, Copy)]
pub struct MetricSeries<'a> {
    /// The instant the resource started publishing this metric: no window
    /// before it holds a sample, because there was nothing to sample.
    pub published_from_ms: i64,
    /// How often the metric is published, in milliseconds — its own published
    /// granularity, not the `period` the request asked for.
    pub publish_interval_ms: i64,
    /// What the request asked for.
    pub read: MetricRead<'a>,
}

impl MetricSeries<'_> {
    /// The datapoints this read answers with, oldest first, each window's
    /// samples being `sample`.
    pub fn datapoints(&self, sample: f64) -> Vec<MetricDatapoint> {
        let period_ms = self.read.period_seconds.saturating_mul(1_000).max(1);

        // The instants that can carry a sample at all: inside the requested
        // window, after the resource began publishing, and not after now. The
        // requested window is half-open — `startTime` is the earliest instant
        // asked for and `endTime` the first one past it — so its last instant is
        // the millisecond before `endTime`.
        let from_ms = self
            .read
            .start_ms
            .max(self.published_from_ms.saturating_add(1));
        let to_ms = self.read.end_ms.saturating_sub(1).min(self.read.now_ms);
        if from_ms > to_ms {
            return Vec::new();
        }

        // `period` divides the time line into windows, and a datapoint's
        // `timestamp` is the start of its own window, so the windows are the
        // multiples of the granularity — not offsets from the request's own
        // `startTime`, which would move every timestamp with the request.
        let first_window = from_ms.div_euclid(period_ms).saturating_mul(period_ms);
        let last_window = to_ms.div_euclid(period_ms).saturating_mul(period_ms);
        let windows = (last_window - first_window) / period_ms + 1;

        // Walked newest first, so the cap keeps the most recent windows, and
        // bounded by the cap itself: a caller-sized span over a caller-sized
        // granularity cannot drive more work than that.
        //
        // Which members the request asked for is resolved ONCE, before the walk:
        // `statistics` is a caller-sized list, so asking it per window would
        // multiply its length by the window count.
        let asked = RequestedStatistics::of(self.read.statistics);
        let walk = windows.min(MAX_METRIC_DATAPOINTS as i64);
        let mut series = Vec::new();
        for step in 0..walk {
            let window_start = last_window - step * period_ms;
            let samples = self.samples_in(
                window_start.max(from_ms),
                (window_start + period_ms - 1).min(to_ms),
            );
            if samples == 0 {
                // A window the resource published no sample in holds no
                // datapoint — what the service itself answers for a window it
                // has no data for.
                continue;
            }
            series.push(self.datapoint(window_start, samples, sample, asked));
        }
        series.reverse();
        series
    }

    /// How many instants this resource published a sample at fall in the
    /// inclusive span `from_ms..=to_ms`.
    ///
    /// Those instants are [`FIRST_SAMPLE_MILLIS`] after it started publishing,
    /// and then one per granularity interval from that same start — computed,
    /// not walked, because a day of them is a four-figure number.
    ///
    /// The first instant is counted apart from the interval-aligned ones, which
    /// is exact because [`FIRST_SAMPLE_MILLIS`] is shorter than any granularity
    /// a metric is published at — the assertion below pins that, since a first
    /// span that reached a whole interval would be counted twice.
    fn samples_in(&self, from_ms: i64, to_ms: i64) -> i64 {
        if from_ms > to_ms {
            return 0;
        }
        let interval = self.publish_interval_ms.max(1);
        debug_assert!(
            FIRST_SAMPLE_MILLIS < interval,
            "the first sample lands before the first whole granularity interval"
        );
        let first = self.published_from_ms.saturating_add(FIRST_SAMPLE_MILLIS);
        let mut samples = i64::from(first >= from_ms && first <= to_ms);
        let lowest = (from_ms.saturating_sub(self.published_from_ms) + interval - 1)
            .div_euclid(interval)
            .max(1);
        let highest = to_ms
            .saturating_sub(self.published_from_ms)
            .div_euclid(interval);
        if highest >= lowest {
            samples += highest - lowest + 1;
        }
        samples
    }

    /// The datapoint for the window starting at `window_start_ms`, built from the
    /// `samples` it holds and what each of them measured.
    ///
    /// Each `MetricStatistic` the request named selects one member OF the
    /// datapoint, so only those are filled: a statistic nobody asked for is left
    /// out rather than reported. The values are the ones the modelled statistics
    /// define over the window's samples — "Minimum - The lowest value observed
    /// during the specified period", "Maximum - The highest", "Sum - All values
    /// submitted for the matching metric added together" and "Average - The
    /// value of Sum / SampleCount" — so they hold together: with every sample of
    /// the window the same measurement, the extremes and the mean are that
    /// measurement and the total is it over the count.
    fn datapoint(
        &self,
        window_start_ms: i64,
        samples: i64,
        sample: f64,
        asked: RequestedStatistics,
    ) -> MetricDatapoint {
        let sample_count = samples as f64;
        MetricDatapoint {
            minimum: asked.minimum.then_some(sample),
            maximum: asked.maximum.then_some(sample),
            sum: asked.sum.then_some(sample * sample_count),
            average: asked.average.then_some(sample),
            sample_count: asked.sample_count.then_some(sample_count),
            // "The timestamp": the start of the window this datapoint is the
            // aggregate of.
            timestamp: Some(timestamp(window_start_ms)),
            // "The unit": the one the metric is published in, which the request
            // had to name to be served at all.
            unit: Some(self.read.unit.to_string()),
        }
    }
}

/// Which members of a datapoint one read asks for: the modelled
/// `MetricStatistic` list, resolved to the five members it can select.
///
/// Resolved ONCE per read rather than per datapoint. `MetricStatisticList`
/// carries no length bound, so a request may name the same statistic any number
/// of times; walking it once here keeps the cost of a read the length of that
/// list PLUS the window count, never their product.
#[derive(Debug, Clone, Copy)]
struct RequestedStatistics {
    minimum: bool,
    maximum: bool,
    sum: bool,
    average: bool,
    sample_count: bool,
}

impl RequestedStatistics {
    /// The members `statistics` selects, in one pass over it.
    fn of(statistics: &[String]) -> Self {
        let mut asked = Self {
            minimum: false,
            maximum: false,
            sum: false,
            average: false,
            sample_count: false,
        };
        for requested in statistics {
            match requested.as_str() {
                STATISTIC_MINIMUM => asked.minimum = true,
                STATISTIC_MAXIMUM => asked.maximum = true,
                STATISTIC_SUM => asked.sum = true,
                STATISTIC_AVERAGE => asked.average = true,
                STATISTIC_SAMPLE_COUNT => asked.sample_count = true,
                // Unreachable: the boundary walk admits only the modelled
                // enum's members, and those are the five above.
                _ => {}
            }
        }
        asked
    }
}

// ---------------------------------------------------------------------------
// What one sample measured
// ---------------------------------------------------------------------------

/// What a sample of a metric that measures WORK measured, for work nothing
/// performed: zero.
///
/// Every metric of this service is either an amount of work a resource did in
/// the window — compute units used, bytes carried, requests served, responses
/// generated, operations completed, health checks failed — or an amount of the
/// capacity it was given. This emulator runs no guest, no container and no
/// health check and serves no request through a load balancer or a
/// distribution, so each metric of the FIRST kind measured nothing in every
/// window, which is the value each metric's own documentation gives a window in
/// which nothing happened. Any other number would be a measurement of work
/// nothing performed, which a client could not tell from a measured one.
///
/// A metric of the SECOND kind is not this: a capacity the store PROVISIONED is
/// a quantity the store knows, and is reported from the resource's own record
/// (see [`relational_database_metric_series`]).
pub const UNPERFORMED_WORK_SAMPLE: f64 = 0.0;

/// How many bytes one gibibyte is, for a metric published in `Bytes` over a
/// provisioned size a bundle states in GB.
pub const BYTES_PER_GIB: f64 = 1_073_741_824.0;

/// What every sample of every instance metric this store publishes measured:
/// nothing, so zero.
///
/// Each instance metric is a measurement taken ON a running guest, and this
/// emulator runs none — so in every window the quantity each of them measures is
/// zero, and each metric's own documentation (the modelled
/// `GetInstanceMetricDataRequest$metricName`) says so for exactly that window:
///
/// * each status check "can be either 0 (passed) or 1 (failed)", and nothing in
///   this store impairs an instance or its host;
/// * `MetadataNoToken` "reports the number of times that the instance metadata
///   service was successfully accessed without a token ... If all requests use
///   token-backed sessions ... then the value is 0", and no process runs here to
///   reach that service at all;
/// * `NetworkIn` and `NetworkOut` are "the number of bytes received on" / "sent
///   out on all network interfaces ... during the period", and no byte crossed
///   an interface of this guest in any period;
/// * the disk metrics are the read and write operations "completed on the system
///   disk of the instance during the period", the bytes they moved, the seconds
///   they spent and the requests "waiting to access the system disk", and no
///   operation completed and none is outstanding;
/// * `CPUUtilization` is "the percentage of allocated compute units that are
///   currently in use on the instance", and nothing is using any of them, as
///   `BurstCapacityPercentage` and `BurstCapacityTime` are the burst capacity a
///   running guest accrues and consumes, of which this store has observed
///   neither.
///
/// Any other number would be a measurement of work nothing performed, which a
/// client could not tell from a measured one. What this series does carry is
/// what the store DOES know: which windows the instance has been publishing in,
/// how many samples each of them holds, the metric's published unit, and the
/// statistics the request asked for.
pub const INSTANCE_METRIC_SAMPLE: f64 = UNPERFORMED_WORK_SAMPLE;

/// The datapoints `GetInstanceMetricData` answers with for `instance`, or `None`
/// when this store publishes no such metric of an instance.
///
/// `read` carries everything about the request; the instance's own record is
/// what decides which windows carry a datapoint.
pub fn instance_metric_series(
    instance: &StoredInstance,
    read: MetricRead<'_>,
) -> Option<Vec<MetricDatapoint>> {
    // A metric no instance publishes has no series at all, and answering one
    // with a window of samples would claim this store publishes it.
    instance_metric_unit(read.metric_name)?;
    let series = MetricSeries {
        // An instance starts publishing when the service creates it: the guest
        // comes up and begins being measured, which is why a window before its
        // creation carries nothing.
        published_from_ms: instance.created_at_ms,
        publish_interval_ms: instance_metric_granularity_seconds(read.metric_name)
            .saturating_mul(1_000),
        read,
    };
    Some(series.datapoints(INSTANCE_METRIC_SAMPLE))
}

// ---------------------------------------------------------------------------
// The series the other monitored resources publish
// ---------------------------------------------------------------------------

/// The granularity a managed relational database's metrics are published at, in
/// seconds.
///
/// From the modelled `GetRelationalDatabaseMetricDataRequest$period`
/// documentation: "All relational database metric data is available in 1-minute
/// (60 seconds) granularity."
pub const RELATIONAL_DATABASE_METRIC_GRANULARITY_SECONDS: i64 = 60;

/// The granularity a container service's metrics are published at, in seconds.
///
/// From the modelled `GetContainerServiceMetricDataRequest$period`
/// documentation: "All container service metric data is available in 5-minute
/// (300 seconds) granularity."
pub const CONTAINER_SERVICE_METRIC_GRANULARITY_SECONDS: i64 = 300;

/// The granularity a load balancer's metrics are published at, in seconds.
///
/// CHOSEN: neither that operation's `period` member nor its `metricName` entries
/// state a granularity, so this is the finest one a read can ask for — the lower
/// bound of the modelled `MetricPeriod` range (60..=86400) — which is what lets
/// a read at any admitted `period` find whole published windows inside its own.
pub const LOAD_BALANCER_METRIC_GRANULARITY_SECONDS: i64 = 60;

/// The granularity a distribution's metrics are published at, in seconds.
/// CHOSEN, for the reason [`LOAD_BALANCER_METRIC_GRANULARITY_SECONDS`] is.
pub const DISTRIBUTION_METRIC_GRANULARITY_SECONDS: i64 = 60;

/// The `RelationalDatabaseMetricName` member reporting "the amount of available
/// storage space", from the modelled
/// `GetRelationalDatabaseMetricDataRequest$metricName` documentation.
pub const METRIC_FREE_STORAGE_SPACE: &str = "FreeStorageSpace";

/// The `MetricName` member reporting the amount of available random access
/// memory, from the modelled `MetricName` enum.
pub const METRIC_FREEABLE_MEMORY: &str = "FreeableMemory";

/// The datapoints `GetRelationalDatabaseMetricData` answers with for `database`.
///
/// `read` carries everything about the request; the database's own record is
/// what decides which windows carry a datapoint and what each of their samples
/// measured.
pub fn relational_database_metric_series(
    database: &StoredRelationalDatabase,
    read: MetricRead<'_>,
) -> Vec<MetricDatapoint> {
    // Of this resource type's metrics, two are an amount of the capacity the
    // bundle PROVISIONED rather than work the engine did, and the store knows
    // that capacity: it is the database's own hardware
    // (`RelationalDatabaseHardware$diskSizeInGb` and `$ramSizeInGb`, which
    // `GetRelationalDatabase` reports). `FreeStorageSpace` is "the amount of
    // available storage space" and this store has written nothing to that disk;
    // `FreeableMemory` is the memory available to the engine and no engine
    // process of this emulator holds any of it. Reporting zero instead would
    // state the opposite of what the store holds — a full disk and no memory on
    // a database this service reports as `available`.
    let sample = match read.metric_name {
        METRIC_FREE_STORAGE_SPACE => f64::from(database.bundle.disk_size_in_gb) * BYTES_PER_GIB,
        METRIC_FREEABLE_MEMORY => f64::from(database.bundle.ram_size_in_gb) * BYTES_PER_GIB,
        // The rest measure the engine's work — the share of its compute units in
        // use, the connections open to it, the requests queued against its disk,
        // the swap it is using and the bytes its interfaces carried — of which
        // this store observes none.
        _ => UNPERFORMED_WORK_SAMPLE,
    };
    MetricSeries {
        // A database starts publishing when the service creates it, which is why
        // a window before its creation carries nothing.
        published_from_ms: database.created_at_ms,
        publish_interval_ms: RELATIONAL_DATABASE_METRIC_GRANULARITY_SECONDS.saturating_mul(1_000),
        read,
    }
    .datapoints(sample)
}

/// The datapoints `GetLoadBalancerMetricData` answers with for `load_balancer`.
///
/// Every load balancer metric is an amount of the traffic the load balancer
/// carried or of the health checks it ran against its targets, and this emulator
/// serves no request through one and runs no health check — which is why every
/// registration stays `initial` or `draining`, neither healthy nor unhealthy
/// ([`INSTANCE_HEALTH_INITIAL`]) — so every sample measured
/// [`UNPERFORMED_WORK_SAMPLE`].
pub fn load_balancer_metric_series(
    load_balancer: &StoredLoadBalancer,
    read: MetricRead<'_>,
) -> Vec<MetricDatapoint> {
    MetricSeries {
        published_from_ms: load_balancer.created_at_ms,
        publish_interval_ms: LOAD_BALANCER_METRIC_GRANULARITY_SECONDS.saturating_mul(1_000),
        read,
    }
    .datapoints(UNPERFORMED_WORK_SAMPLE)
}

/// The datapoints `GetDistributionMetricData` answers with for `distribution`.
///
/// Every distribution metric is an amount of the requests the distribution
/// served, the bytes they moved or the share of them that ended in an error, and
/// this emulator serves no request through a distribution — so every sample
/// measured [`UNPERFORMED_WORK_SAMPLE`], which for an error RATE over no request
/// is also the rate itself.
pub fn distribution_metric_series(
    distribution: &StoredDistribution,
    read: MetricRead<'_>,
) -> Vec<MetricDatapoint> {
    MetricSeries {
        published_from_ms: distribution.created_at_ms,
        publish_interval_ms: DISTRIBUTION_METRIC_GRANULARITY_SECONDS.saturating_mul(1_000),
        read,
    }
    .datapoints(UNPERFORMED_WORK_SAMPLE)
}

/// The datapoints `GetContainerServiceMetricData` answers with for `service`.
///
/// Both container service metrics are the average share of the compute units and
/// of the memory in use across the service's nodes, and this emulator runs no
/// container on any node — so every sample measured
/// [`UNPERFORMED_WORK_SAMPLE`].
pub fn container_service_metric_series(
    service: &StoredContainerService,
    read: MetricRead<'_>,
) -> Vec<MetricDatapoint> {
    MetricSeries {
        published_from_ms: service.created_at_ms,
        publish_interval_ms: CONTAINER_SERVICE_METRIC_GRANULARITY_SECONDS.saturating_mul(1_000),
        read,
    }
    .datapoints(UNPERFORMED_WORK_SAMPLE)
}
