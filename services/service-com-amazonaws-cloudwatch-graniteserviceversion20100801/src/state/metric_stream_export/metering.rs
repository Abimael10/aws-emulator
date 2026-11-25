// SPDX-License-Identifier: CC0-1.0

//! The METERING a running metric stream publishes about its own export.
//!
//! A metric stream is a metered pipeline: beside the records it pushes into its destination it
//! publishes, as metrics of its OWN, how much it streamed — the series a client watches a
//! stream's volume on, and the ones its cost is read from. They live in the reserved
//! [`METRIC_STREAMS_NAMESPACE`], so no caller can publish them (`PutMetricData` refuses the
//! reserved
//! namespace prefix, [`crate::ops::put_metric_data`]) and the service itself is the only thing
//! that puts a series there — the same seam this service meters its own API through
//! ([`crate::state::usage`]).
//!
//! What one export is metered as:
//!
//! * [`METRIC_UPDATE`] counts the metric UPDATES the stream exported: one observation per record
//!   that reached the destination.
//! * [`TOTAL_METRIC_UPDATE`] counts the STATISTICS those updates carried, each as an update of
//!   its own: the four every streamed metric gets are the update itself ("a metric stream always
//!   sends the `MAX`, `MIN`, `SUM`, and `SAMPLECOUNT` statistics for each metric that is
//!   streamed", `PutMetricStream`), and each additional statistic a statistics configuration
//!   adds for the metric a record reports counts once more ("You can use the
//!   `StatisticsConfigurations` parameter to have the metric stream send additional statistics
//!   in the stream", `PutMetricStream`). So the two series are equal for a stream that
//!   configures no additional statistic, and the total is the greater exactly when a
//!   configuration added statistics to what was streamed — by one per configured statistic per
//!   record.
//! * [`PUBLISH_ERROR_RATE`] reports the SHARE of a publish attempt's metric updates that did not
//!   reach the destination: one observation per attempt the stream made, 0 for an attempt every
//!   record of which was published and up to 1 for one none of it was. So a stream that cannot
//!   publish at all — its role cannot be assumed, its role may not put records into the
//!   destination, the destination refused the put — reports a non-zero rate while a healthy one
//!   reports zero, which is what tells the two apart from the publisher's side.
//!
//! Each of the three is published TWICE, into the two series a reader asks for it by: the
//! stream's own series, under [`STREAM_NAME_DIMENSION`] naming the stream it is about
//! ([`metering_key`]), and the ACCOUNT-level aggregate of that same metric, which carries no
//! dimension at all ([`account_metering_key`]). The dimension set is part of a series' identity
//! and a read names it exactly ("If a specific combination of dimensions was not published, you
//! can't retrieve statistics for it", `GetMetricStatisticsInput$Dimensions`), so the aggregate is
//! a series of its own and every stream of the account meters into it: a `Sum` of the aggregate
//! over a window is what the account's streams updated in it, which is at least what any one of
//! them did, and an `Average` of the aggregated rate is the mean share over every attempt the
//! account's streams made.
//!
//! All three are stamped with the instant the export was MADE rather than with the period the
//! records report: the counters are a reading of the stream's own throughput, so a backdated
//! datapoint streamed now is metered now. A stream that MAKES no attempt at all — one
//! `StopMetricStreams` has taken out of service, or one whose filters selected nothing — meters
//! nothing on any of the three: there is neither an update to count nor an attempt to report a
//! rate for, which is what tells a stopped stream from a running one by its own metrics. A stream
//! that attempted a publish and placed no record of it counts no update and reports the rate.

use super::MeasuredPeriod;
use crate::state::{
    ComAmazonawsCloudwatchGraniteserviceversion20100801State as State, DimensionSet, MetricKey,
};

/// The namespace a metric stream's own metrics live in, which no caller may publish into
/// ([`crate::ops::put_metric_data`] refuses the reserved prefix).
pub const METRIC_STREAMS_NAMESPACE: &str = "AWS/CloudWatch/MetricStreams";

/// The metric counting the metric updates a stream exported: a `Sum` over a window is how many
/// updates that window's exports carried.
pub const METRIC_UPDATE: &str = "MetricUpdate";

/// The metric counting the same exports with each statistic of an update counted separately.
pub const TOTAL_METRIC_UPDATE: &str = "TotalMetricUpdate";

/// The metric reporting the share of a publish attempt's metric updates the destination did not
/// take: a `Maximum` over a window is the worst share that window saw, and a window holding only
/// zeros is a stream that published everything it attempted.
pub const PUBLISH_ERROR_RATE: &str = "PublishErrorRate";

/// The dimension naming WHICH stream a metering series is about.
pub const STREAM_NAME_DIMENSION: &str = "MetricStreamName";

/// What ONE record of an export counts as in [`TOTAL_METRIC_UPDATE`] before its additional
/// statistics are added: the update itself, which is the four statistics every streamed metric
/// carries.
const UPDATE_ITSELF: f64 = 1.0;

/// The series one of a stream's own metrics lives in: the metric name, under the dimension
/// naming the stream.
///
/// Derived from the stream's name alone, so the series the export metered and the one a reader
/// asks for by that stream name are the same series.
#[must_use]
pub fn metering_key(metric_name: &str, stream_name: &str) -> MetricKey {
    let mut dimensions = DimensionSet::new();
    dimensions.insert(STREAM_NAME_DIMENSION.to_string(), stream_name.to_string());
    MetricKey {
        namespace: METRIC_STREAMS_NAMESPACE.to_string(),
        metric_name: metric_name.to_string(),
        dimensions,
    }
}

/// The series the ACCOUNT-level aggregate of one of a stream's own metrics lives in: the same
/// metric name in the same namespace, with NO dimension at all.
///
/// Derived from the metric name alone, so every stream of the account meters into the one
/// aggregate series, and the series a reader asks for without naming a stream is that same
/// series.
#[must_use]
pub fn account_metering_key(metric_name: &str) -> MetricKey {
    MetricKey {
        namespace: METRIC_STREAMS_NAMESPACE.to_string(),
        metric_name: metric_name.to_string(),
        dimensions: DimensionSet::new(),
    }
}

/// BOTH series one of a stream's own metrics is published into: the stream's own, and the
/// account-level aggregate of that metric.
///
/// Every reading below goes through this, so neither series can be written without the other:
/// the aggregate is the same observation under a dimension set of its own, which is what makes a
/// read of it report the account while a read under the stream's dimension reports the stream.
fn metering_keys(metric_name: &str, stream_name: &str) -> [MetricKey; 2] {
    [
        metering_key(metric_name, stream_name),
        account_metering_key(metric_name),
    ]
}

/// The share of ONE publish attempt's metric updates that did not reach the destination, as
/// [`PUBLISH_ERROR_RATE`] reports it, or `None` for a stream that attempted nothing (which has no
/// share to report).
///
/// The attempt is the records the stream pushed and the ones the destination took, so the share
/// is the records left over divided by the records pushed: 0 when every one of them was
/// published, 1 when none was, and the fraction in between for a push the destination broke off
/// partway.
#[must_use]
pub(super) fn publish_error_rate(attempted: usize, taken: usize) -> Option<f64> {
    if attempted == 0 {
        return None;
    }
    let failed = attempted.saturating_sub(taken);
    Some(failed as f64 / attempted as f64)
}

/// What one export counts as: the updates it carried, and those updates' statistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportedUpdates {
    /// The metric updates exported, one per record.
    pub updates: f64,
    /// The statistics those updates carried, each counted as an update of its own.
    pub statistic_updates: f64,
}

/// What the first `taken` of one export's measurements count as: one update per record, and per
/// record the update itself plus each additional statistic it carries.
///
/// The measurements and the records rendered from them are one for one and in the same order, so
/// `taken` is how many of them the destination took.
///
/// A record's additional statistics are the ones it CARRIES, which is what the stream streamed:
/// a configured statistic a period's distribution does not define is left out of the record
/// ([`super::measured_period`]) and so is not counted as an update either, which is why the
/// total is at most the updates times one plus the configured statistics rather than always
/// exactly that.
#[must_use]
pub(super) fn exported(measured: &[MeasuredPeriod], taken: usize) -> ExportedUpdates {
    let delivered = measured.iter().take(taken);
    ExportedUpdates {
        updates: delivered.clone().count() as f64,
        statistic_updates: delivered
            .map(|measured| UPDATE_ITSELF + measured.additional.len() as f64)
            .sum(),
    }
}

impl State {
    /// Meter `exported` against the stream named `stream_name`, stamped `now_secs`.
    ///
    /// Each series takes as many observations of 1 as the count it reports, through the seam
    /// every count this service publishes about itself goes through
    /// ([`State::append_self_metered_count`]): a `Sum` over a window is how much that window
    /// streamed, a `SampleCount` is the same number and an `Average` is 1 — the aggregate of
    /// that many single updates.
    ///
    /// Counted into both series the metric is published in ([`metering_keys`]): the stream's own
    /// and the account-level aggregate, so a `Sum` of the aggregate holds this stream's updates
    /// beside every other stream's.
    pub fn meter_exported_updates(
        &mut self,
        stream_name: &str,
        exported: ExportedUpdates,
        now_secs: f64,
    ) {
        if exported.updates <= 0.0 {
            return;
        }
        for (metric_name, count) in [
            (METRIC_UPDATE, exported.updates),
            (TOTAL_METRIC_UPDATE, exported.statistic_updates),
        ] {
            for key in metering_keys(metric_name, stream_name) {
                self.append_self_metered_count(key, count, now_secs);
            }
        }
    }

    /// Report `rate` as the share of one publish attempt by the stream named `stream_name` that
    /// did not reach the destination, stamped `now_secs`.
    ///
    /// One observation per attempt, through the seam a self-published SHARE goes through
    /// ([`State::append_self_metered_ratio`]): a `Maximum` over a window is the worst share that
    /// window saw, and an `Average` is the mean share over the attempts it held. An attempt that
    /// published everything observes 0, which is a measurement and not an absence.
    ///
    /// Reported into both series the metric is published in ([`metering_keys`]), so the
    /// account-level aggregate holds one observation per attempt of every stream of the account
    /// while each stream's own series holds only its own.
    pub fn meter_publish_error_rate(&mut self, stream_name: &str, rate: f64, now_secs: f64) {
        for key in metering_keys(PUBLISH_ERROR_RATE, stream_name) {
            self.append_self_metered_ratio(key, rate, now_secs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{MetricKey, UNIT_NONE};

    /// A measured period carrying `additional` additional statistics, for the rows below.
    fn measured(additional: usize) -> MeasuredPeriod {
        MeasuredPeriod {
            key: MetricKey {
                namespace: "Custom/One".to_string(),
                metric_name: "Latency".to_string(),
                dimensions: DimensionSet::new(),
            },
            start: 60.0,
            unit: UNIT_NONE.to_string(),
            statistics: crate::state::BasicStatistics::default(),
            additional: (0..additional)
                .map(|index| (format!("p{index}0"), index as f64))
                .collect(),
        }
    }

    /// RULE: one of a stream's own metrics is published into TWO series — the stream's own,
    /// identified by the dimension naming it, and the account-level aggregate of the same metric
    /// name with no dimension at all — both in the reserved namespace.
    #[test]
    fn each_reading_is_published_as_the_stream_s_own_and_as_the_account_aggregate() {
        // The rows differ along what the rule generalises over: which of the namespace's three
        // metrics is metered, and which stream it is about.
        let rows: [(&str, &str, &str); 5] = [
            ("the update counter", METRIC_UPDATE, "orders"),
            ("the statistic counter", TOTAL_METRIC_UPDATE, "orders"),
            ("the error rate", PUBLISH_ERROR_RATE, "orders"),
            ("another stream's update counter", METRIC_UPDATE, "audit-7"),
            ("another stream's error rate", PUBLISH_ERROR_RATE, "audit-7"),
        ];
        for (case, metric_name, stream_name) in rows {
            let [own, aggregate] = metering_keys(metric_name, stream_name);
            let mut expected = DimensionSet::new();
            expected.insert(
                STREAM_NAME_DIMENSION.to_string(),
                stream_name.to_string(),
            );
            assert_eq!(
                (
                    own.namespace.as_str(),
                    own.metric_name.as_str(),
                    &own.dimensions
                ),
                (METRIC_STREAMS_NAMESPACE, metric_name, &expected),
                "case {case:?}: the stream's own series must be the metric under the dimension \
                 naming it, got {own:?}"
            );
            assert_eq!(
                (
                    aggregate.namespace.as_str(),
                    aggregate.metric_name.as_str(),
                    &aggregate.dimensions
                ),
                (METRIC_STREAMS_NAMESPACE, metric_name, &DimensionSet::new()),
                "case {case:?}: the account aggregate must be the same metric with no dimension, \
                 got {aggregate:?}"
            );
            assert_eq!(
                aggregate,
                account_metering_key(metric_name),
                "case {case:?}: every stream must meter into the one aggregate series, got \
                 {aggregate:?}"
            );
        }
    }

    /// One counting case: the records one export carried, with how many additional statistics
    /// each of them did, and what the counts are for every record and for one record fewer.
    struct CountingRow {
        case: &'static str,
        /// The additional statistics each record carries, one entry per record.
        records: Vec<usize>,
        /// The counts when the destination took every record.
        all: ExportedUpdates,
        /// The counts when it took one fewer, which is an export whose last put it refused.
        short_of_one: ExportedUpdates,
    }

    /// The counts as `(updates, statistic updates)`, for the rows below.
    fn counts(updates: f64, statistic_updates: f64) -> ExportedUpdates {
        ExportedUpdates {
            updates,
            statistic_updates,
        }
    }

    /// RULE: one record is one metric update, and counts in the total as itself plus each
    /// additional statistic it carries — over the records the destination TOOK.
    #[test]
    fn the_total_counts_each_additional_statistic_as_one_more_update() {
        // The rows differ along how many records one export carried and how many additional
        // statistics each of them did, and each is counted twice: for every record, and for
        // one record fewer.
        let cases: Vec<CountingRow> = vec![
            CountingRow {
                case: "nothing exported",
                records: vec![],
                all: counts(0.0, 0.0),
                short_of_one: counts(0.0, 0.0),
            },
            CountingRow {
                case: "one record with no additional statistic",
                records: vec![0],
                all: counts(1.0, 1.0),
                short_of_one: counts(0.0, 0.0),
            },
            CountingRow {
                case: "one record with three of them",
                records: vec![3],
                all: counts(1.0, 4.0),
                short_of_one: counts(0.0, 0.0),
            },
            CountingRow {
                case: "two records with six of them each",
                records: vec![6, 6],
                all: counts(2.0, 14.0),
                short_of_one: counts(1.0, 7.0),
            },
            CountingRow {
                case: "three records, two of them configured",
                records: vec![0, 2, 6],
                all: counts(3.0, 11.0),
                short_of_one: counts(2.0, 4.0),
            },
        ];
        for row in cases {
            let case = row.case;
            let delivered: Vec<MeasuredPeriod> = row.records.into_iter().map(measured).collect();
            for (taken, expected) in [
                (delivered.len(), row.all),
                (delivered.len().saturating_sub(1), row.short_of_one),
            ] {
                let counted = exported(&delivered, taken);
                assert_eq!(
                    counted, expected,
                    "case {case:?} with {taken} record(s) taken: expected {expected:?}, got \
                     {counted:?}"
                );
            }
        }
    }

    /// RULE: the error rate of one publish attempt is the records the destination did not take
    /// over the records the attempt pushed; an attempt that pushed nothing has no rate at all.
    #[test]
    fn the_error_rate_is_the_share_of_the_attempt_that_did_not_reach_the_destination() {
        // The rows differ along what the rule generalises over: how many records one attempt
        // pushed, how many of them the destination took (all, none, some), and the size of the
        // attempt — so the rate is asserted as a SHARE rather than for one batch size.
        let rows: [(&str, usize, usize, Option<f64>); 8] = [
            ("nothing attempted", 0, 0, None),
            ("one record, published", 1, 1, Some(0.0)),
            ("one record, refused", 1, 0, Some(1.0)),
            ("four records, all published", 4, 4, Some(0.0)),
            ("four records, none published", 4, 0, Some(1.0)),
            ("four records, one published", 4, 1, Some(0.75)),
            ("four records, three published", 4, 3, Some(0.25)),
            ("twenty records, ten published", 20, 10, Some(0.5)),
        ];
        for (case, attempted, taken, expected) in rows {
            let rate = publish_error_rate(attempted, taken);
            assert_eq!(
                rate, expected,
                "case {case:?}: expected {expected:?}, got {rate:?}"
            );
        }
    }
}
