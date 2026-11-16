// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a FINISHED canary run is reported as.
//!
//! A canary run is observable in two places, not one: the run record `GetCanaryRuns`
//! reports, and the CloudWatch metrics the run publishes when it finishes. The model itself
//! says so where it describes what happens to a run that is still open when the canary is
//! stopped — "the run that is in progress completes on its own, publishes metrics, and
//! uploads artifacts" (`smithy_query(query="doc",shape="StopCanary")`) — and the run's own
//! timeline carries the instant those metrics are generated at
//! (`CanaryRunTimeline$MetricTimestampForRunAndRetries`, "The time at which the metrics will
//! be generated for this run or retries").
//!
//! So a run that reached a terminal state publishes, into the [`NAMESPACE`] namespace
//! dimensioned by the canary it belongs to ([`CANARY_NAME_DIMENSION`]):
//!
//! * [`SUCCESS_PERCENT_METRIC`] — [`PERCENT_ALL`] for a run that passed and
//!   [`PERCENT_NONE`] for one that did not, which is what a canary-health alarm grades;
//! * [`FAILED_METRIC`] — one for a run that did not pass, zero for one that did;
//! * [`DURATION_METRIC`] — how long the run took, in milliseconds, measured from the run's
//!   own timeline.
//!
//! Nothing else is published: the metrics CloudWatch Synthetics reports per STEP, and the
//! response-code counters it reports for the requests a script made, are about work this
//! emulator's runs do not perform (no script is executed, `crate::state::canary_run`), so no
//! series is invented for them.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the very series
//! `ListMetrics`, `GetMetricStatistics` and `GetMetricData` read, and the alarm evaluation
//! grades — reached through the shared engine, so one metric has one home and a publisher
//! cannot drift from its readers. They never travel over `PutMetricData`: CloudWatch reserves
//! the namespaces the services own ("you should not specify a namespace that begins with
//! `AWS/`", `PutMetricData$Namespace`), and a service's own metric is appended rather than
//! sent.
//!
//! Nothing here fails a request: an engine carrying no CloudWatch store, or one at capacity,
//! drops the observation and the call that materialised the run is answered exactly as it
//! was.

use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

/// The namespace CloudWatch Synthetics publishes a canary's run metrics under.
pub const NAMESPACE: &str = "CloudWatchSynthetics";

/// The dimension those metrics are reported per: the canary the run belongs to.
pub const CANARY_NAME_DIMENSION: &str = "CanaryName";

/// How much of the run succeeded, as a percentage.
pub const SUCCESS_PERCENT_METRIC: &str = "SuccessPercent";

/// How many of the runs reported failed.
pub const FAILED_METRIC: &str = "Failed";

/// How long the run took.
pub const DURATION_METRIC: &str = "Duration";

/// The `StandardUnit` a percentage is published in.
pub const PERCENT_UNIT: &str = "Percent";

/// The `StandardUnit` a count of things is published in.
pub const COUNT_UNIT: &str = "Count";

/// The `StandardUnit` a span measured in milliseconds is published in.
pub const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The whole of something, as a percentage: what a run that passed succeeded for.
pub const PERCENT_ALL: f64 = 100.0;

/// None of something, as a percentage: what a run that did not pass succeeded for.
pub const PERCENT_NONE: f64 = 0.0;

/// The `StorageResolution` these datapoints are published at, in seconds: the regular
/// resolution, "which CloudWatch stores at 1-minute resolution"
/// (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// How many milliseconds a second holds, for turning a measured span into the unit
/// [`DURATION_METRIC`] is published in.
const MILLISECONDS_PER_SECOND: f64 = 1_000.0;

/// One run that has finished, and the observations that run owes CloudWatch.
///
/// Recorded by whatever moved the run out of `RUNNING` — the clock's own settling of the
/// verdict the run's work produced, or the stop that cut the run short — and published by the
/// operation that materialised it, which is the one holding an engine to publish through.
#[derive(Debug, Clone)]
pub struct FinishedRun {
    /// The canary the run belongs to, which is the value of [`CANARY_NAME_DIMENSION`].
    pub canary_name: String,
    /// The instant the datapoints are stamped at: the run's own
    /// `CanaryRunTimeline$MetricTimestampForRunAndRetries`, so the run record and the series
    /// agree about when the run's metrics were generated.
    pub at: SystemTime,
    /// Whether the run PASSED. A run in any other terminal state did not.
    pub passed: bool,
    /// How long the run took, from its own timeline.
    pub took: Duration,
}

impl FinishedRun {
    /// The datapoints this run owes: the metric, the value, and the unit it is published in.
    fn datapoints(&self) -> [(&'static str, f64, &'static str); 3] {
        [
            (
                SUCCESS_PERCENT_METRIC,
                if self.passed {
                    PERCENT_ALL
                } else {
                    PERCENT_NONE
                },
                PERCENT_UNIT,
            ),
            (
                FAILED_METRIC,
                if self.passed { 0.0 } else { 1.0 },
                COUNT_UNIT,
            ),
            (
                DURATION_METRIC,
                self.took.as_secs_f64() * MILLISECONDS_PER_SECOND,
                MILLISECONDS_UNIT,
            ),
        ]
    }
}

/// Publish everything the runs this store has finished owe CloudWatch, and take them off the
/// store's owed list so a datapoint is published exactly once.
///
/// Called by every operation that settles the store, right after it settles it: the runs
/// whose datapoints this writes were finished by the clock at that same instant, so what is
/// published does not depend on who called.
///
/// The CloudWatch store's lock is taken while this service's is held, which is the only
/// order the two are ever taken in — CloudWatch reaches nothing of this service's.
pub fn publish_finished_runs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &mut super::ComAmazonawsSyntheticsSyntheticsState,
) {
    let finished = std::mem::take(&mut store.finished_runs);
    if finished.is_empty() {
        return;
    }
    // A deployment with no CloudWatch mounted has nowhere to publish to. The runs are taken
    // off the owed list either way, so the list cannot grow without bound.
    let Some(metrics) = cloudwatch::state::service_state(engine) else {
        return;
    };
    let mut metrics = metrics.lock();
    let mut written: Vec<cloudwatch::state::WrittenSeries> = Vec::new();
    for run in &finished {
        let at = epoch_secs(run.at);
        for (metric, value, unit) in run.datapoints() {
            // The series identity is the namespace, the metric and the canary's NAME: that
            // dimension is what a reader selects one canary's series by.
            let key = cloudwatch::state::MetricKey {
                namespace: NAMESPACE.to_string(),
                metric_name: metric.to_string(),
                dimensions: [(
                    CANARY_NAME_DIMENSION.to_string(),
                    run.canary_name.clone(),
                )]
                .into_iter()
                .collect(),
            };
            let datum = cloudwatch::state::StoredDatum {
                timestamp_secs: at,
                // One run is one observation of each of its metrics, so an `Average` over a
                // window is what the runs in it came to and a `Sum` of the failures is how
                // many of them failed.
                sample_count: 1.0,
                sum: value,
                minimum: value,
                maximum: value,
                unit: Some(unit.to_string()),
                storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
            };
            if metrics.append_datapoints(key.clone(), vec![datum]).is_err() {
                // The metric store holds no more series. The run itself is recorded either
                // way; the dropped datapoint is reported where a developer can see it.
                ::tracing::warn!(
                    canary = %run.canary_name,
                    metric = %metric,
                    "the CloudWatch metric store is at capacity, so this run's datapoint was \
                     not published"
                );
                continue;
            }
            written.push(cloudwatch::state::WrittenSeries {
                key,
                units: vec![unit.to_string()],
            });
        }
    }
    // A canary-health alarm is an alarm on one of these series, so the arriving datapoints
    // are what makes its next evaluation due — exactly as a published datapoint does.
    metrics.make_alarms_due(&written, cloudwatch::state::now_secs());
}

/// `at` as the epoch seconds a stored datapoint carries, which is the unit CloudWatch's own
/// readers window by.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finished run of `canary`, with the verdict and the span a row below states.
    fn run(canary: &str, passed: bool, took: Duration) -> FinishedRun {
        FinishedRun {
            canary_name: canary.to_string(),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            passed,
            took,
        }
    }

    /// RULE: a finished run's datapoints are its verdict as a percentage of success, its
    /// failure as a count, and its own duration in milliseconds — so the value domain of
    /// `SuccessPercent` is exactly {0, 100} and `Failed` exactly {0, 1}.
    #[test]
    fn a_finished_run_reports_its_verdict_and_its_duration() {
        let rows = [
            ("a run that passed", true, Duration::from_millis(500), 100.0, 0.0, 500.0),
            ("a run that failed", false, Duration::from_millis(500), 0.0, 1.0, 500.0),
            ("a longer passing run", true, Duration::from_secs(3), 100.0, 0.0, 3_000.0),
            (
                "a sub-millisecond failing run",
                false,
                Duration::from_micros(250),
                0.0,
                1.0,
                0.25,
            ),
        ];
        for (case, passed, took, success, failed, millis) in rows {
            let datapoints = run("probe", passed, took).datapoints();
            let expected = [
                (SUCCESS_PERCENT_METRIC, success, PERCENT_UNIT),
                (FAILED_METRIC, failed, COUNT_UNIT),
                (DURATION_METRIC, millis, MILLISECONDS_UNIT),
            ];
            assert_eq!(
                datapoints, expected,
                "case {case:?}: expected {expected:?}, got {datapoints:?}"
            );
        }
    }
}
