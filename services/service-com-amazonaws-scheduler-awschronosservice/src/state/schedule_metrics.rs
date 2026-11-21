// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series EventBridge Scheduler publishes about the invocations it
//! MAKES.
//!
//! An occurrence is performed with no client action at all ([`super::invocation`]), and
//! the control plane keeps reporting the schedule exactly as its owner wrote it however
//! its occurrences go — so the aggregate view of whether a fleet of schedules is firing,
//! failing, being given up on and dead-lettered is these series and nothing else. They
//! live in the [`METRIC_NAMESPACE`] namespace, dimensioned on the SCHEDULE GROUP the
//! schedule belongs to ([`SCHEDULE_GROUP_DIMENSION`]), which is the dimension EventBridge
//! Scheduler reports them per.
//!
//! One observation is published per EVENT — a delivery attempt made, a target call that
//! failed, an invocation given up on, a dead-letter write — so a `Sum` over a window is
//! what happened in it, and a window in which nothing of a kind happened holds no
//! datapoint of it rather than a zero this service never observed.
//!
//! The datapoints are appended to the CloudWatch store directly rather than sent through
//! `PutMetricData`: CloudWatch reserves the `AWS/` namespaces for the services that own
//! them ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`), so a service's own metric never travels over that API. It
//! goes into the very store `ListMetrics` and `GetMetricStatistics` read, through
//! CloudWatch's own bounded append for a self-metered count, so one metric and its reader
//! cannot disagree.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

/// The CloudWatch store these series live in.
///
/// It is the CloudWatch service's OWN state struct, reached through the SAME shared engine
/// this service's store lives in, so what an invocation publishes is what `ListMetrics`
/// and `GetMetricStatistics` report.
pub type CloudWatchState =
    ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ComAmazonawsCloudwatchGraniteserviceversion20100801State;

/// The namespace EventBridge Scheduler publishes its own metrics to.
pub const METRIC_NAMESPACE: &str = "AWS/Scheduler";

/// The dimension they are reported per: the schedule group the schedule belongs to.
pub const SCHEDULE_GROUP_DIMENSION: &str = "ScheduleGroup";

/// One delivery ATTEMPT of an invocation, counted for every attempt actually made — the
/// first and each retry of it alike.
pub const INVOCATION_ATTEMPT_COUNT: &str = "InvocationAttemptCount";

/// One attempt whose call the TARGET did not accept, whatever answered it: the target's
/// own service refusing or failing the call, or the permissions the invocation is made
/// under withholding it.
pub const TARGET_ERROR_COUNT: &str = "TargetErrorCount";

/// One invocation EventBridge Scheduler GAVE UP on: no further attempt of it will be
/// made, so the occurrence it belongs to delivered nothing.
pub const INVOCATION_DROPPED_COUNT: &str = "InvocationDroppedCount";

/// One invocation whose failed event was written to the queue `Target.DeadLetterConfig`
/// names.
pub const INVOCATIONS_SENT_TO_DEAD_LETTER_COUNT: &str = "InvocationsSentToDeadLetterCount";

/// One invocation whose failed event could NOT be written there — the queue refused the
/// write, or the execution role may not write to it — so the failure was reported nowhere
/// a client can read it.
pub const INVOCATIONS_FAILED_TO_BE_SENT_TO_DEAD_LETTER_COUNT: &str =
    "InvocationsFailedToBeSentToDeadLetterCount";

/// The publisher of one store's invocation metrics, held by the clock that performs them.
///
/// It holds nothing but the store it appends to: the counts themselves are datapoints in
/// the CloudWatch store, never a second copy kept here.
#[derive(Clone)]
pub struct ScheduleMetrics {
    /// The store the datapoints are appended to, or `None` when the engine carries no
    /// CloudWatch — a deployment with nowhere to publish, which is evidence of nothing.
    store: Option<::engine::ServiceState<CloudWatchState>>,
}

impl ::std::fmt::Debug for ScheduleMetrics {
    /// Whether there is a store to publish to, which is all of this publisher that has a
    /// rendering (a store handle has none of its own).
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ScheduleMetrics")
            .field("published", &self.store.is_some())
            .finish()
    }
}

impl ScheduleMetrics {
    /// The publisher appending to the CloudWatch store of the same shared engine.
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            store: cloudwatch::service_state(engine),
        }
    }

    /// Count ONE event of `metric` against the schedule group `group`.
    ///
    /// The append is CloudWatch's own seam for a count a service publishes about itself,
    /// so a datapoint of this series is aggregated — and makes an alarm over it due —
    /// exactly as a published one is. A store with no room left for a new series drops
    /// the count rather than failing the invocation it is about: what is metered is
    /// published off the outcome and never decides it.
    pub fn count(&self, group: &str, metric: &str) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let key = cloudwatch::MetricKey {
            namespace: METRIC_NAMESPACE.to_string(),
            metric_name: metric.to_string(),
            dimensions: [(SCHEDULE_GROUP_DIMENSION.to_string(), group.to_string())]
                .into_iter()
                .collect(),
        };
        store
            .lock()
            .append_self_metered_count(key, 1.0, cloudwatch::now_secs());
    }
}
