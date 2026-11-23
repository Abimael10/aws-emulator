// SPDX-License-Identifier: CC0-1.0

//! The `AWS/IoT` series the RULES ENGINE publishes about the actions it ATTEMPTED: one count
//! per attempt, on the [`SUCCESS_METRIC`] series of the rule when the action was carried out
//! and on its [`FAILURE_METRIC`] series when it was not.
//!
//! A rule action's outcome is reported to nobody on the data plane — the publisher is answered
//! the same whether every action ran or none did ([`super`]) — so these series are the ONLY
//! signal a client has about what its rules are doing with the messages they select. AWS IoT
//! publishes them in the [`NAMESPACE`] namespace dimensioned by the rule the action belongs to
//! ([`RULE_NAME_DIMENSION`]), which is the series a client's rule-health alarm is built on:
//! without them a rule dropping every message looks exactly like a rule delivering every
//! message.
//!
//! What counts is the ATTEMPT and its outcome, whatever made the attempt fail: a target
//! service that refused the call, a grant that does not admit it, a role that bars it, a
//! destination that does not admit the endpoint. The one thing that is neither a success nor a
//! failure is an action this engine did not perform at all — an action member it does not
//! implement, a statement it cannot read, a parameter whose template it cannot render, an
//! endpoint no destination admits, a hop or a delivery past one of its own ceilings. Nothing
//! was attempted there, so there is nothing to count: the engine reports it as
//! [`super::Attempt::NotAttempted`] and [`super::counted`] gives it neither series — the same
//! reason those manufacture no error report.
//!
//! The datapoints go into the CloudWatch store of the SAME engine, which is the store
//! `ListMetrics`, `GetMetricStatistics` and `GetMetricData` answer from, so a client reads them
//! where it reads every other series. They are published OFF the publish path: an engine
//! carrying no CloudWatch has nowhere to publish to and one at capacity drops the count, and
//! neither changes what the rules engine did or what the publisher was answered.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

/// The namespace AWS IoT publishes the rule engine's own metrics under.
pub const NAMESPACE: &str = "AWS/IoT";

/// The dimension that names the rule a rule-action series is about.
pub const RULE_NAME_DIMENSION: &str = "RuleName";

/// The series counting the rule actions that WERE carried out.
pub const SUCCESS_METRIC: &str = "Success";

/// The series counting the rule actions that were attempted and FAILED.
pub const FAILURE_METRIC: &str = "Failure";

/// The `StandardUnit` a bare count of things carries: `Count`.
pub const COUNT_UNIT: &str = "Count";

/// The `StorageResolution` these datapoints are published at, in seconds: regular resolution,
/// which CloudWatch stores at 1-minute resolution (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// What ONE attempted rule action came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The action was carried out.
    Succeeded,
    /// The action was attempted and failed — the outcome the rule's `errorAction` reports.
    Failed,
}

impl Outcome {
    /// The series an attempt with this outcome is counted on.
    #[must_use]
    pub fn metric_name(self) -> &'static str {
        match self {
            Self::Succeeded => SUCCESS_METRIC,
            Self::Failed => FAILURE_METRIC,
        }
    }
}

/// The series one of the rule engine's own metrics about `rule_name` lives in.
#[must_use]
pub fn series(metric: &str, rule_name: &str) -> cloudwatch::MetricKey {
    cloudwatch::MetricKey {
        namespace: NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: [(RULE_NAME_DIMENSION.to_string(), rule_name.to_string())]
            .into_iter()
            .collect(),
    }
}

/// Count ONE attempted action of the rule `rule_name`: a single observation of 1 on the series
/// `outcome` names, so a `Sum` over a window is how many of that rule's actions that window
/// came to with that outcome.
///
/// Call it with NO IoT store lock held: it takes the CloudWatch store's own lock.
pub fn publish_action_outcome<Eng: Send + Sync + 'static>(
    engine: &Eng,
    rule_name: &str,
    outcome: Outcome,
) {
    let Some(state) = cloudwatch::service_state(engine) else {
        return;
    };
    let at_secs = cloudwatch::now_secs();
    let key = series(outcome.metric_name(), rule_name);
    let watched = cloudwatch::WrittenSeries {
        key: key.clone(),
        units: vec![COUNT_UNIT.to_string()],
    };
    let datum = cloudwatch::StoredDatum {
        timestamp_secs: at_secs,
        // One attempt: the datapoint IS that one action's outcome, never a running total kept
        // here — the totals a client reads are the sum CloudWatch computes over the series.
        sample_count: 1.0,
        sum: 1.0,
        minimum: 1.0,
        maximum: 1.0,
        unit: Some(COUNT_UNIT.to_string()),
        storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
    };
    let mut store = state.lock();
    if store.append_datapoints(key, vec![datum]).is_err() {
        ::tracing::warn!(
            rule = %rule_name,
            metric = outcome.metric_name(),
            "the CloudWatch metric store is at capacity, so this rule action's outcome was not \
             published"
        );
        return;
    }
    // An alarm on a rule's health is an alarm on one of these series, so an arriving datapoint
    // is what makes its next evaluation due — exactly as a published datapoint does.
    store.make_alarms_due(&[watched], at_secs);
}
