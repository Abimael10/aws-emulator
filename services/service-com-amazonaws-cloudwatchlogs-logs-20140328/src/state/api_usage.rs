// SPDX-License-Identifier: CC0-1.0

//! The usage telemetry CloudWatch Logs publishes about its OWN API: one call count per call it
//! serves, and one error count beside it for a call it answered as a failure.
//!
//! An account's API usage is published in the reserved `AWS/Usage` namespace, where every
//! service meters its own calls under its own `Service` dimension beside the operation called
//! ([`cloudwatch::state::usage`], which owns that series' identity and the store it lives in).
//! CloudWatch Logs publishes its own calls there, and publishes the counters of its API in its
//! own `AWS/Logs` namespace ([`super::emf::LOGS_NAMESPACE`], where the rest of what it reports
//! about itself lives): the call count per operation under that same usage dimension set, how
//! many of those calls were answered as a FAILURE beside it, and both for the ACCOUNT as a
//! whole under no dimensions at all — the granularity this service publishes an account-wide
//! figure at. How a call was answered is reported in the service's own namespace rather than in
//! the usage one, which publishes the call count alone. So a client reads how many CloudWatch
//! Logs calls its account made and how many of them failed, per operation or for the whole
//! account, and the API-usage report, request-rate alarm and error-rate alarm each read it from
//! the series they are built on.
//!
//! This is the service metering itself, not a caller publishing a metric: the datapoints are
//! published off the ANSWER, by the seam every request of this service passes through
//! ([`crate::service::api_usage`]), rather than by any one handler, so a call is counted
//! wherever it was answered from — the handler, a modelled error, or a refusal raised before
//! any handler ran. A call the service REFUSED was still served, so it is counted as a call
//! too, and counted as an error beside it: the call count is of requests this API answered,
//! whatever it answered, and the error count is of the ones it answered as failures.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use super::emf::{publish, ExtractedDatapoint, DEFAULT_STORAGE_RESOLUTION, LOGS_NAMESPACE};

/// The name an account's usage metrics publish this service's calls under: the `Service`
/// dimension of every usage series of it, which is what a reader has to name to find them.
///
/// The service as the model names it for a consumer outside its own API (`cloudFormationName`
/// `Logs` on the service shape), not the `sdkId` an SDK is generated from.
pub const USAGE_SERVICE: &str = "Logs";

/// The metric counting the calls an account made of one of this service's operations, which is
/// published in both namespaces.
pub const CALL_COUNT: &str = cloudwatch::state::usage::CALL_COUNT;

/// The metric counting the ones of those calls that were answered as a failure, which this
/// service publishes in its OWN namespace: the usage namespace publishes how many calls were
/// made and not how they were answered.
pub const ERROR_COUNT: &str = "ErrorCount";

/// The lowest HTTP status that is a failure, which is what makes a served call an ERROR for
/// the usage telemetry: every answer below it is a success this service produced, and every
/// answer at or above it is one a client reads as a failure.
const FIRST_FAILURE_STATUS: u16 = 400;

/// Whether an answer of `status` is a failure, and so counts an error beside its call.
#[must_use]
pub fn failed(status: u16) -> bool {
    status >= FIRST_FAILURE_STATUS
}

/// The metrics one served call is counted into: its call count, and the error count beside it
/// for a call answered as a failure.
fn counted_metrics(status: u16) -> &'static [&'static str] {
    if failed(status) {
        &[CALL_COUNT, ERROR_COUNT]
    } else {
        &[CALL_COUNT]
    }
}

/// The `AWS/Logs` datapoints one call of `operation` answered with `status` publishes, stamped
/// `at_ms`: one observation of 1 per metric counted, for the OPERATION called and for the
/// ACCOUNT as a whole.
///
/// Both series are this service's own figures about its API rather than measurements of one
/// resource, so neither carries a unit: a count of calls is a bare number, which is what the
/// `StandardUnit` value for "no unit" states and what a publisher declaring none is reported
/// under.
///
/// The per-operation series carries the usage dimension set, read from the one derivation of it
/// ([`cloudwatch::state::usage::api_usage_key`]) so the call count this namespace publishes and
/// the one the usage namespace does are the same series under two namespaces. The account-wide
/// series carries NO dimensions: it is the figure for every operation of this service together,
/// which is not a property of any one of them.
#[must_use]
pub fn datapoints(operation: &str, status: u16, at_ms: i64) -> Vec<ExtractedDatapoint> {
    let mut datapoints = Vec::new();
    for metric in counted_metrics(status) {
        let per_operation =
            cloudwatch::state::usage::api_usage_key(USAGE_SERVICE, metric, operation).dimensions;
        for dimensions in [per_operation, ::std::collections::BTreeMap::new()] {
            datapoints.push(ExtractedDatapoint {
                namespace: LOGS_NAMESPACE.to_string(),
                metric_name: (*metric).to_string(),
                dimensions,
                timestamp_ms: at_ms,
                value: 1.0,
                unit: None,
                storage_resolution: DEFAULT_STORAGE_RESOLUTION,
            });
        }
    }
    datapoints
}

/// Count one call of `operation` this service answered with `status` into the usage telemetry
/// of this account, held in the CloudWatch metric store the shared engine carries.
///
/// The counts have one home — the store `PutMetricData` writes and `GetMetricData` /
/// `GetMetricStatistics` read — so the `AWS/Usage` call count is stored through CloudWatch's own
/// self-metering seam ([`cloudwatch::state::usage::count_served_call`]) and the `AWS/Logs`
/// counters through the seam every metric this service publishes goes through ([`publish`]),
/// rather than through a path of their own; a store that is not mounted at all is counted into
/// nothing. Nothing is answered: a call is answered whether or not its own usage datapoint could
/// be stored.
///
/// What one metered call costs is its datapoints and nothing else: both seams take the metric
/// store's lock for their appends alone, with no log store held and nothing delivered, so a
/// counted call cannot drive work of its own.
pub fn count_api_call<Eng: Send + Sync + 'static>(engine: &Eng, operation: &str, status: u16) {
    publish(engine, &datapoints(operation, status, super::now_ms()));
    cloudwatch::state::usage::count_served_call(engine, USAGE_SERVICE, operation);
}
