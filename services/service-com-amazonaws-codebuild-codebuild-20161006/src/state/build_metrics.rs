// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch metrics CodeBuild publishes about the builds it RAN.
//!
//! # The rule this module implements
//!
//! A build ends with no call of the client's — the transition out of `IN_PROGRESS` is owed by
//! the clock ([`super::build_run`]) — so the way a client watches build health over time is
//! not `BatchGetBuilds` but the series CodeBuild publishes about itself: the
//! [`METRIC_NAMESPACE`] namespace, dimensioned on the project the build belongs to
//! ([`PROJECT_NAME_DIMENSION`]), which is what an alarm on failing builds and a dashboard
//! summing builds read. One build that has ENDED contributes one observation to each series
//! its outcome belongs to:
//!
//! * [`BUILDS_METRIC`], a count of the builds triggered, stamped at the instant the build
//!   STARTED — so the period a build is counted in is the period it was triggered in,
//!   whatever its run then came to;
//! * [`SUCCEEDED_BUILDS_METRIC`], a count of the successful builds — the `SUCCEEDED` status
//!   of `StatusType`;
//! * [`FAILED_BUILDS_METRIC`], a count of the builds that failed because of a client error
//!   or because of a timeout — the `FAILED` and `TIMED_OUT` statuses of that same enum
//!   ([`FAILED_BUILD_STATUSES`]);
//! * [`DURATION_METRIC`], how long the build took, in [`SECONDS_UNIT`] rather than the
//!   counters' [`COUNT_UNIT`].
//!
//! A build that ended in neither success nor failure — one a client `STOPPED`, and one that
//! `FAULT`ed, which is the service's own fault rather than a client error or a timeout —
//! is counted in the builds that ran and in their duration, and in neither outcome counter:
//! summing the two counters over a window therefore never claims more builds than ran, and
//! a stopped build is not reported as a failing one.
//!
//! A project nothing has built publishes NO datapoint at all: a series exists because a
//! build ended, and reporting a zero for a project nobody built would claim an observation
//! nothing made.
//!
//! The datapoints are appended to the CloudWatch store directly rather than sent through
//! `PutMetricData`: CloudWatch reserves the `AWS/` namespaces for the services that own them
//! ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`), so a service's own metric never travels over that API. It goes
//! into the very store `ListMetrics`, `GetMetricStatistics` and `GetMetricData` read, through
//! CloudWatch's own bounded append, so one metric and its reader cannot disagree — and an
//! alarm watching the series is made due by the append, exactly as an ingested datapoint
//! makes it due.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::{BUILD_STATUS_FAILED, BUILD_STATUS_SUCCEEDED};

/// The CloudWatch store a build's own metrics live in.
///
/// It is the CloudWatch service's OWN state struct, reached through the SAME shared engine
/// this service's store lives in, so what a build publishes is what `ListMetrics` and
/// `GetMetricStatistics` report.
pub type CloudWatchState =
    ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ComAmazonawsCloudwatchGraniteserviceversion20100801State;

/// The namespace CodeBuild publishes its own metrics to.
pub const METRIC_NAMESPACE: &str = "AWS/CodeBuild";

/// The dimension they are reported per: the project whose build the work was.
pub const PROJECT_NAME_DIMENSION: &str = "ProjectName";

/// The metric counting the builds of one project that were triggered.
pub const BUILDS_METRIC: &str = "Builds";

/// The metric counting the builds of one project that succeeded.
pub const SUCCEEDED_BUILDS_METRIC: &str = "SucceededBuilds";

/// The metric counting the builds of one project that failed because of a client error or
/// because of a timeout.
pub const FAILED_BUILDS_METRIC: &str = "FailedBuilds";

/// The metric reporting how long one build took.
pub const DURATION_METRIC: &str = "Duration";

/// The `StatusType` value of a build that TIMED OUT, which is the second of the two
/// outcomes [`FAILED_BUILDS_METRIC`] counts (`StatusType`: SUCCEEDED, FAILED, FAULT,
/// TIMED_OUT, IN_PROGRESS, STOPPED).
pub const BUILD_STATUS_TIMED_OUT: &str = "TIMED_OUT";

/// The build statuses [`FAILED_BUILDS_METRIC`] counts: the build failed because of a client
/// error, or because it timed out.
pub const FAILED_BUILD_STATUSES: &[&str] = &[BUILD_STATUS_FAILED, BUILD_STATUS_TIMED_OUT];

/// The unit a count of builds carries: `Count`, the `StandardUnit` member for a plain tally
/// of occurrences — a number of builds is neither a rate nor a size.
pub const COUNT_UNIT: &str = "Count";

/// The unit a build's duration carries: `Seconds`, the `StandardUnit` member for a span of
/// time.
pub const SECONDS_UNIT: &str = "Seconds";

/// The `StorageResolution` these datapoints are published at, in seconds: regular
/// resolution, "which CloudWatch stores at 1-minute resolution"
/// (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// How much ONE observation of a build counter stands for: one build.
const ONE_BUILD: f64 = 1.0;

/// Milliseconds in one second, for the duration a build is published in seconds.
const MILLIS_PER_SECOND: f64 = 1_000.0;

/// One build whose run has ENDED, as the metrics it publishes are read off it.
///
/// It holds no measurement of its own beyond the build's own record: the quantities are the
/// datapoints in the CloudWatch store, never a running total kept here, so a client's
/// `Sum` over a window is what that window held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedBuild {
    /// The project the build belongs to, which is the dimension its series are reported
    /// per.
    pub project_name: String,
    /// The `StatusType` the build ended in, which decides which outcome counter it
    /// contributes to.
    pub status: String,
    /// Epoch millis the build started at — when the build was TRIGGERED, which is the
    /// instant [`BUILDS_METRIC`] counts it at.
    pub start_millis: i64,
    /// Epoch millis the build ended at.
    pub end_millis: i64,
}

/// ONE datapoint a completed build publishes: the series it belongs to, what it measured,
/// the unit of that measurement and the instant it was taken at.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// The metric name of the series this datapoint goes into.
    pub metric_name: &'static str,
    /// What the datapoint measured.
    pub value: f64,
    /// The `StandardUnit` the value is expressed in.
    pub unit: &'static str,
    /// Epoch millis the measurement was taken at.
    pub at_millis: i64,
}

impl CompletedBuild {
    /// How long the build took, in seconds: the span between the instants it started and
    /// ended at. Never negative — a build cannot end before it started.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        let millis = self.end_millis.saturating_sub(self.start_millis).max(0);
        millis as f64 / MILLIS_PER_SECOND
    }

    /// The outcome counter this build's status contributes to, or `None` for a status that
    /// is neither a success nor a failure ([`FAILED_BUILD_STATUSES`]).
    #[must_use]
    pub fn outcome_metric(&self) -> Option<&'static str> {
        if self.status == BUILD_STATUS_SUCCEEDED {
            return Some(SUCCEEDED_BUILDS_METRIC);
        }
        FAILED_BUILD_STATUSES
            .contains(&self.status.as_str())
            .then_some(FAILED_BUILDS_METRIC)
    }

    /// Every datapoint this build publishes: that it ran, how long it took, and the
    /// outcome counter its status belongs to.
    #[must_use]
    pub fn observations(&self) -> Vec<Observation> {
        let mut observations = vec![
            Observation {
                metric_name: BUILDS_METRIC,
                value: ONE_BUILD,
                unit: COUNT_UNIT,
                // The build is counted in the period it was TRIGGERED in.
                at_millis: self.start_millis,
            },
            Observation {
                metric_name: DURATION_METRIC,
                value: self.duration_seconds(),
                unit: SECONDS_UNIT,
                at_millis: self.end_millis,
            },
        ];
        if let Some(metric_name) = self.outcome_metric() {
            observations.push(Observation {
                metric_name,
                value: ONE_BUILD,
                unit: COUNT_UNIT,
                at_millis: self.end_millis,
            });
        }
        observations
    }
}

/// The CloudWatch store in the SAME shared engine. `None` means the engine carries no such
/// store — a deployment with no CloudWatch to publish to.
pub fn cloudwatch_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<CloudWatchState>> {
    ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state::service_state(engine)
}

/// Publish what each build of `completed` is metered by, into the CloudWatch store of the
/// same shared engine.
///
/// Best effort, the way every cross-service publication in this tree is: an emulator with no
/// CloudWatch mounted has nowhere to publish, and a CloudWatch store at capacity keeps what
/// it holds — the build ran and reports its own outcome either way, because a metric is
/// published off the answer and so can never be what withholds one. A build belonging to no
/// project names no series to publish into, so nothing is published for it.
///
/// Call it with no CodeBuild store lock held: it takes the CloudWatch store's own.
pub fn publish_completed_builds<Eng: Send + Sync + 'static>(
    engine: &Eng,
    completed: &[CompletedBuild],
) {
    if completed.is_empty() {
        return;
    }
    let Some(store) = cloudwatch_state(engine) else {
        return;
    };
    for build in completed {
        if build.project_name.is_empty() {
            continue;
        }
        for observation in build.observations() {
            append(&store, &build.project_name, &observation);
        }
    }
}

/// Store one observation of the series `metric_name` names for `project_name`, and make
/// every alarm watching that series due for evaluation — exactly as an ingested
/// `PutMetricData` datapoint does, so an alarm cannot tell CodeBuild's own metric from a
/// client's.
fn append(
    store: &::engine::ServiceState<CloudWatchState>,
    project_name: &str,
    observation: &Observation,
) {
    let key = cloudwatch::MetricKey {
        namespace: METRIC_NAMESPACE.to_string(),
        metric_name: observation.metric_name.to_string(),
        dimensions: [(
            PROJECT_NAME_DIMENSION.to_string(),
            project_name.to_string(),
        )]
        .into_iter()
        .collect(),
    };
    let value = observation.value;
    let datum = cloudwatch::StoredDatum {
        timestamp_secs: observation.at_millis as f64 / MILLIS_PER_SECOND,
        sample_count: 1.0,
        sum: value,
        minimum: value,
        maximum: value,
        unit: Some(observation.unit.to_string()),
        storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
    };
    let watched = cloudwatch::WrittenSeries {
        key: key.clone(),
        units: vec![observation.unit.to_string()],
    };
    let at_secs = datum.timestamp_secs;
    let mut store = store.lock();
    if store.append_datapoints(key, vec![datum]).is_err() {
        ::tracing::warn!(
            project = %project_name,
            metric = observation.metric_name,
            "the CloudWatch store is at capacity; a build's own metric could not be published"
        );
        return;
    }
    store.make_alarms_due(&[watched], at_secs);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the status the build ended in, and the
    /// `(metric, value, unit)` of every datapoint it publishes, in the order they are
    /// published.
    type Row = (
        &'static str,
        &'static str,
        Vec<(&'static str, f64, &'static str)>,
    );

    /// A build of one project that started at `start_millis` and ended at `end_millis` in
    /// `status`.
    fn ended(status: &str, start_millis: i64, end_millis: i64) -> CompletedBuild {
        CompletedBuild {
            project_name: "a-project".to_string(),
            status: status.to_string(),
            start_millis,
            end_millis,
        }
    }

    /// RULE: a build that ended publishes that it ran and how long it took, plus the one
    /// outcome counter its status belongs to — the success counter for a build that
    /// succeeded, the failure counter for one that failed because of a client error or a
    /// timeout, and no counter at all for a build that ended in neither.
    ///
    /// The rows span every value of `StatusType`
    /// (`smithy_query(query="shape",shape="StatusType")`: SUCCEEDED, FAILED, FAULT,
    /// TIMED_OUT, IN_PROGRESS, STOPPED), including the two the issue that motivated this
    /// module never cited, plus a status no enum member spells.
    #[test]
    fn a_build_publishes_the_outcome_counter_its_status_belongs_to() {
        let seconds = 1.5;
        let start = 1_700_000_000_000;
        let end = start + 1_500;
        // (case, status, the datapoints)
        let rows: Vec<Row> = vec![
            (
                "a build that succeeded",
                "SUCCEEDED",
                vec![
                    ("Builds", 1.0, "Count"),
                    ("Duration", seconds, "Seconds"),
                    ("SucceededBuilds", 1.0, "Count"),
                ],
            ),
            (
                "a build that failed",
                "FAILED",
                vec![
                    ("Builds", 1.0, "Count"),
                    ("Duration", seconds, "Seconds"),
                    ("FailedBuilds", 1.0, "Count"),
                ],
            ),
            (
                "a build that timed out, which is the other way a build fails",
                "TIMED_OUT",
                vec![
                    ("Builds", 1.0, "Count"),
                    ("Duration", seconds, "Seconds"),
                    ("FailedBuilds", 1.0, "Count"),
                ],
            ),
            (
                "a build that faulted, which is neither a client error nor a timeout",
                "FAULT",
                vec![("Builds", 1.0, "Count"), ("Duration", seconds, "Seconds")],
            ),
            (
                "a build a client stopped",
                "STOPPED",
                vec![("Builds", 1.0, "Count"), ("Duration", seconds, "Seconds")],
            ),
            (
                "a status that is no outcome at all",
                "IN_PROGRESS",
                vec![("Builds", 1.0, "Count"), ("Duration", seconds, "Seconds")],
            ),
            (
                "a status no enum member spells",
                "",
                vec![("Builds", 1.0, "Count"), ("Duration", seconds, "Seconds")],
            ),
        ];

        for (case, status, expected) in rows {
            let published: Vec<(&str, f64, &str)> = ended(status, start, end)
                .observations()
                .iter()
                .map(|observation| {
                    (observation.metric_name, observation.value, observation.unit)
                })
                .collect();
            assert_eq!(
                published, expected,
                "case {case:?}: expected the datapoints {expected:?}, got {published:?}"
            );
        }
    }

    /// RULE: the count of a build is stamped at the instant it was TRIGGERED and its
    /// outcome at the instant it ENDED, so a window holds the builds it started and the
    /// outcomes it reached — and the duration is the span between the two, in seconds.
    ///
    /// The rows differ along what the rule generalises over: how long the build took (under
    /// a second, over a minute, and the degenerate span of a build whose record ends before
    /// it starts, which measures nothing rather than a negative duration).
    #[test]
    fn a_builds_datapoints_are_stamped_where_its_work_happened() {
        // (case, start millis, end millis, the duration in seconds)
        let rows: Vec<(&str, i64, i64, f64)> = vec![
            ("a build of three quarters of a second", 1_000, 1_750, 0.75),
            ("a build of one second", 10_000, 11_000, 1.0),
            ("a build of ninety seconds", 10_000, 100_000, 90.0),
            ("a record that ends when it starts", 5_000, 5_000, 0.0),
            ("a record that ends before it starts", 5_000, 4_000, 0.0),
        ];

        for (case, start_millis, end_millis, expected) in rows {
            let build = ended("SUCCEEDED", start_millis, end_millis);
            assert!(
                (build.duration_seconds() - expected).abs() < f64::EPSILON,
                "case {case:?}: expected a duration of {expected} s, got {}",
                build.duration_seconds()
            );
            let observations = build.observations();
            let stamps: Vec<(&str, i64)> = observations
                .iter()
                .map(|observation| (observation.metric_name, observation.at_millis))
                .collect();
            assert_eq!(
                stamps,
                vec![
                    (BUILDS_METRIC, start_millis),
                    (DURATION_METRIC, end_millis),
                    (SUCCEEDED_BUILDS_METRIC, end_millis),
                ],
                "case {case:?}: expected the count at the start and the outcome at the end, \
                 got {stamps:?}"
            );
        }
    }
}
