// SPDX-License-Identifier: CC0-1.0

//! The METRIC DATAPOINTS a publisher gave Amazon CloudWatch, for the services that read a
//! metric's HISTORY rather than measure it themselves.
//!
//! A service that forecasts or scales from a metric cannot see that metric's past unless
//! the past was recorded as it happened: the observations belong to the service they were
//! PUBLISHED to, and a reader of them holds none of its own. This is that record — ONE
//! directory in the shared engine, written by CloudWatch as a publish is stored and read by
//! the services whose own answers are computed from a metric's history, so a reader and the
//! publisher can no more disagree about what was observed than two readers of one store can.
//!
//! The engine keys state by TYPE, which is why the directory is declared here rather than
//! inside either crate: a copy of its own inside one service's state struct would be
//! invisible to the other.
//!
//! What a series is keyed by is the metric's own identity — "a metric is uniquely defined by
//! a name, a namespace, and zero or more dimensions" — and nothing of the request that
//! published it. Each observation carries the instant it was observed at and the SUM of the
//! values it stands for, which is the quantity a publish contributes to a span of the series
//! however it was worded (one value, a statistic set, or a values/counts pair): a reader
//! aggregates those sums over whatever span its own answer is reported at.
//!
//! The directory is a record of what this emulator was GIVEN, so it is partial by
//! construction — a series it holds nothing for is a series nothing published here, which is
//! a reader's "no history" and not an error — and it is BOUNDED
//! ([`MAX_PUBLISHED_SERIES`], [`MAX_OBSERVATIONS_PER_SERIES`]), so a publisher looping
//! cannot grow it without end. Past either bound the OLDEST entry is dropped, never a newer
//! one, which keeps the recent history a live workflow just published readable.

use std::collections::{BTreeMap, VecDeque};

/// A metric's dimension set: dimension name to value, ordered by name, so the same set keys
/// the same series whatever order a request listed it in.
pub type DimensionSet = BTreeMap<String, String>;

/// The identity of one metric time series, as the metric itself defines it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MetricSeries {
    /// The metric's namespace.
    pub namespace: String,
    /// The metric's name.
    pub metric_name: String,
    /// The metric's dimension set.
    pub dimensions: DimensionSet,
}

impl MetricSeries {
    /// The series `metric_name` in `namespace` carrying `dimensions`.
    #[must_use]
    pub fn new(
        namespace: impl Into<String>,
        metric_name: impl Into<String>,
        dimensions: DimensionSet,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            metric_name: metric_name.into(),
            dimensions,
        }
    }
}

/// One observation of a series: when it was observed, and the sum of the values it stands
/// for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    /// The instant the publisher stamped the observation with, as epoch seconds.
    pub at_secs: i64,
    /// The sum of the values this observation stands for.
    pub sum: f64,
}

/// How many series the directory holds at once.
///
/// An EMULATOR ceiling, not a service quota: the directory lives in memory for the lifetime
/// of the process, so without a bound a publisher writing new series in a loop would grow it
/// without end.
pub const MAX_PUBLISHED_SERIES: usize = 2_000;

/// How many observations one series holds at once, bounded for the same reason.
pub const MAX_OBSERVATIONS_PER_SERIES: usize = 2_000;

/// The observations published for each series, oldest publish first.
#[derive(Debug, Default)]
pub struct PublishedMetricData {
    /// The observations held per series.
    series: BTreeMap<MetricSeries, VecDeque<Observation>>,
    /// The series in the order they were first published, so the oldest is the one dropped
    /// at [`MAX_PUBLISHED_SERIES`].
    order: VecDeque<MetricSeries>,
}

impl PublishedMetricData {
    /// Record that `series` was observed at `at_secs` with values summing to `sum`.
    ///
    /// Past either bound the oldest entry goes: the oldest observation of the series for
    /// [`MAX_OBSERVATIONS_PER_SERIES`], and the series published longest ago for
    /// [`MAX_PUBLISHED_SERIES`].
    pub fn publish(&mut self, series: MetricSeries, at_secs: i64, sum: f64) {
        if !self.series.contains_key(&series) {
            if self.order.len() >= MAX_PUBLISHED_SERIES {
                if let Some(oldest) = self.order.pop_front() {
                    self.series.remove(&oldest);
                }
            }
            self.order.push_back(series.clone());
        }
        let observations = self.series.entry(series).or_default();
        observations.push_back(Observation { at_secs, sum });
        while observations.len() > MAX_OBSERVATIONS_PER_SERIES {
            observations.pop_front();
        }
    }

    /// Every observation of `series` stamped inside `[from_secs, until_secs]`, in the order
    /// they were published.
    ///
    /// Empty where nothing published that series, and where nothing it published falls in
    /// the span — the two cases a reader treats alike, since both are "no history here".
    #[must_use]
    pub fn observations(
        &self,
        series: &MetricSeries,
        from_secs: i64,
        until_secs: i64,
    ) -> Vec<Observation> {
        self.series
            .get(series)
            .map(|observations| {
                observations
                    .iter()
                    .copied()
                    .filter(|observation| {
                        observation.at_secs >= from_secs && observation.at_secs <= until_secs
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many series the directory holds.
    #[must_use]
    pub fn series_count(&self) -> usize {
        self.series.len()
    }
}

/// The ONE directory of published metric data in the shared engine.
///
/// `None` for an engine this emulator does not hold its state in, which leaves a publisher
/// with nothing to record and a reader with no history — never an error of either one's own
/// call.
///
/// LOCK ORDER: a caller takes its OWN service state first and this directory second, and
/// nothing holds this one while taking another store, so the order cannot invert.
#[must_use]
pub fn published_metric_data<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<PublishedMetricData>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<PublishedMetricData>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One series, named `metric` in `namespace`, over the dimension set `dimensions`.
    fn series(namespace: &str, metric: &str, dimensions: &[(&str, &str)]) -> MetricSeries {
        MetricSeries::new(
            namespace,
            metric,
            dimensions
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        )
    }

    /// RULE: a series is identified by its namespace, its name and its dimension set, so an
    /// observation is readable under exactly that identity and under no other.
    #[test]
    fn an_observation_is_held_under_the_metrics_own_identity() {
        let mut directory = PublishedMetricData::default();
        let published = series("Workload", "Requests", &[("Group", "one")]);
        directory.publish(published.clone(), 100, 7.0);
        // (case, the identity a reader asks under, how many observations it finds)
        let rows: &[(&str, MetricSeries, usize)] = &[
            ("the identity it was published under", published.clone(), 1),
            (
                "the same name in another namespace",
                series("Other", "Requests", &[("Group", "one")]),
                0,
            ),
            (
                "another name in the same namespace",
                series("Workload", "Latency", &[("Group", "one")]),
                0,
            ),
            (
                "another dimension value",
                series("Workload", "Requests", &[("Group", "two")]),
                0,
            ),
            (
                "no dimensions at all",
                series("Workload", "Requests", &[]),
                0,
            ),
        ];
        for (case, asked, expected) in rows {
            let found = directory.observations(asked, 0, 1_000);
            assert_eq!(
                found.len(),
                *expected,
                "case {case:?}: expected {expected} observations, got {found:?}"
            );
        }
    }

    /// RULE: a read reports the observations stamped inside the span it asks for, and
    /// nothing outside it.
    #[test]
    fn a_read_selects_the_observations_inside_its_own_span() {
        let mut directory = PublishedMetricData::default();
        let published = series("Workload", "Requests", &[]);
        for at in [10_i64, 20, 30, 40] {
            directory.publish(published.clone(), at, at as f64);
        }
        // (case, span, the instants inside it)
        let rows: &[(&str, (i64, i64), &[i64])] = &[
            ("the whole history", (0, 100), &[10, 20, 30, 40]),
            ("a span closed on both observations", (20, 30), &[20, 30]),
            ("a span of one instant", (30, 30), &[30]),
            ("a span before everything published", (0, 9), &[]),
            ("a span after everything published", (41, 100), &[]),
        ];
        for (case, (from, until), expected) in rows {
            let instants: Vec<i64> = directory
                .observations(&published, *from, *until)
                .into_iter()
                .map(|observation| observation.at_secs)
                .collect();
            assert_eq!(
                instants, *expected,
                "case {case:?}: expected the observations at {expected:?}, got {instants:?}"
            );
        }
    }

    /// RULE: both bounds drop the OLDEST entry, so the most recent history stays readable.
    #[test]
    fn the_oldest_entry_is_the_one_dropped_at_each_bound() {
        let mut directory = PublishedMetricData::default();
        let published = series("Workload", "Requests", &[]);
        let total = MAX_OBSERVATIONS_PER_SERIES + 5;
        for index in 0..total {
            directory.publish(published.clone(), index as i64, 1.0);
        }
        let held = directory.observations(&published, 0, total as i64);
        assert_eq!(
            held.len(),
            MAX_OBSERVATIONS_PER_SERIES,
            "case observations past the bound: expected {MAX_OBSERVATIONS_PER_SERIES} held, \
             got {}",
            held.len()
        );
        assert_eq!(
            held.first().map(|observation| observation.at_secs),
            Some(5),
            "case observations past the bound: expected the oldest five dropped, got {:?}",
            held.first()
        );

        let mut directory = PublishedMetricData::default();
        for index in 0..MAX_PUBLISHED_SERIES + 1 {
            directory.publish(series("Workload", &format!("m{index}"), &[]), 1, 1.0);
        }
        assert_eq!(
            directory.series_count(),
            MAX_PUBLISHED_SERIES,
            "case series past the bound: expected {MAX_PUBLISHED_SERIES} series held, got {}",
            directory.series_count()
        );
        assert!(
            directory
                .observations(&series("Workload", "m0", &[]), 0, 10)
                .is_empty(),
            "case series past the bound: expected the series published first to be the one \
             dropped"
        );
        assert_eq!(
            directory
                .observations(&series("Workload", &format!("m{MAX_PUBLISHED_SERIES}"), &[]), 0, 10)
                .len(),
            1,
            "case series past the bound: expected the newest series to be held"
        );
    }
}
