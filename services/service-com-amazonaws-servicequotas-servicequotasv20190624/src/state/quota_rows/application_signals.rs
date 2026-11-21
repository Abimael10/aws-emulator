// SPDX-License-Identifier: CC0-1.0

//! Amazon CloudWatch Application Signals' published default-quota catalog.

use super::*;

/// The name `AWS/Usage` publishes this service's API call counts under, which is the
/// `Service` dimension of every metered row below.
///
/// SOURCE: the recorded default-quota listing of `application-signals`, whose rate rows
/// carry this dimension value. It is NOT the display name the service catalog reports for
/// the code (see [`aws_usage_metric`]): the dimension is the key a client puts in a
/// `GetMetricData` query, so it is held once here rather than derived from the
/// `ServiceName`.
const APPLICATION_SIGNALS_USAGE_SERVICE: &str = "CloudWatch Application Signals";

/// One per-API REQUEST-RATE quota of this service: the requests per second it serves
/// calls of one operation at.
///
/// Every member of such a row but the operation's own name and the value is the same
/// across the class: the name is `Rate of <Api> requests`, the window is one second and
/// the usage metric is that operation's `AWS/Usage` `CallCount` ([`call_count_metric`]).
/// One constructor fills them so two rows of the class cannot drift apart, the way
/// [`super::request_rate`] does for the services whose rate rows also publish a
/// `Description`; the sentence THIS service words its rate quotas with is not among the
/// data available here, so these rows publish none rather than wording of this catalog's
/// own.
fn application_signals_request_rate(code: &str, api_name: &str, per_second: f64) -> QuotaRow {
    quota(format!("Rate of {api_name} requests"), per_second)
        .code(code)
        .period(1, "SECOND")
        .metric(call_count_metric(
            APPLICATION_SIGNALS_USAGE_SERVICE,
            api_name,
        ))
}

/// Amazon CloudWatch Application Signals' published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `application-signals` service code,
/// which states which operation each `QuotaCode` throttles, the rate published under it
/// and the `AWS/Usage` metric it is metered by.
///
/// Every row is published under the `L-` code AWS mints for it. The quotas that listing
/// carries under a code whose published RATE no source available here states — the
/// service-dependency listing (`L-A3B40D72`) among them — are left to the
/// default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn application_signals_quotas() -> Vec<QuotaRow> {
    vec![
        application_signals_request_rate("L-D898488F", "ListAuditFindings", 1.0),
        application_signals_request_rate("L-EB837E0B", "StartDiscovery", 5.0),
    ]
}
