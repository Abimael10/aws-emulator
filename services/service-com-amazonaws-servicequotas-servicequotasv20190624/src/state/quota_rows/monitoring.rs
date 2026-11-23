// SPDX-License-Identifier: CC0-1.0

//! Amazon CloudWatch's published default-quota catalog, under the `ServiceCode` its API
//! endpoint uses.

use super::*;

/// The `AWS/Usage` display name Amazon CloudWatch publishes its call counts under,
/// which is the `Service` dimension of every metered CloudWatch quota.
const CLOUDWATCH_USAGE_SERVICE: &str = "CloudWatch";

/// One Amazon CloudWatch API REQUEST-RATE quota: `Rate of <api> requests`, the most
/// requests to that one operation CloudWatch serves per second.
///
/// CloudWatch publishes a rate quota for each of its API operations, and the members of
/// such a row that the CLASS fixes are the same across it: the `QuotaName` names the
/// operation the rate is of, the window is one second, the usage metric is that
/// operation's `AWS/Usage` `CallCount` ([`call_count_metric`]) under the `CloudWatch`
/// service dimension, and the `Description` is the sentence the class is published with,
/// with the throttled operation named in it. Those are what one constructor fills, so two
/// rows of the class cannot drift apart.
///
/// The requests per second and the adjustability are NOT class members: AWS publishes
/// them per quota, so every call site states the rate of the row it builds and the row
/// says so where AWS publishes it as fixed. A rate shared by two rows is a coincidence of
/// those two quotas, never a default for a third.
///
/// SOURCE: the recorded default-quota listing of `monitoring` for the class's name form,
/// its one-second window, the call-count metric each row carries and the sentence above.
fn cloudwatch_request_rate(api_name: &str, requests_per_second: f64) -> QuotaRow {
    quota(
        format!("Rate of {api_name} requests"),
        requests_per_second,
    )
    .period(1, "SECOND")
    .metric(call_count_metric(CLOUDWATCH_USAGE_SERVICE, api_name))
    .describe(&format!(
        "The maximum number of {api_name} requests that you can make, per second, in this \
         account in the current region."
    ))
}

/// Amazon CloudWatch's published quota catalog.
///
/// CloudWatch publishes most of its quotas as per-API REQUEST RATES, each over a
/// one-second window and measured with the `AWS/Usage` `CallCount` metric of the API it
/// throttles ([`cloudwatch_request_rate`]), beside the rates it puts on the metric-math
/// functions a `GetMetricData` request may use.
///
/// SOURCE: AWS's published Service Quotas data for the `monitoring` service code (what
/// `aws service-quotas list-aws-default-service-quotas --service-code monitoring`
/// reports), and the recorded default-quota listing of that code for which `QuotaCode`
/// names which quota and for the published rate, adjustability and sentence of each row
/// below — row by row, never a rate carried over from a neighbour.
///
/// The rows below are the ones whose AWS code this catalog can cite TOGETHER WITH the
/// quota that code names and the facts published under it. A row is an IDENTITY — the
/// code a client resolves a limit by — so the rest of the service's published rows are
/// left to the default-quota-set gap rather than served under a code AWS does not mint or
/// a code whose quota this catalog would be guessing at (see
/// [`super::descriptive_quota_code`] for the rule about a row's code).
pub(in crate::state) fn monitoring_quotas() -> Vec<QuotaRow> {
    vec![
        // This API states its own rate: "There is a limit of 20 transactions per second
        // for this API" (the `GetMetricWidgetImage` documentation of the CloudWatch
        // model), which is the limit AWS publishes the quota at.
        cloudwatch_request_rate("GetMetricWidgetImage", 20.0).code("L-6FCAAA2E"),
        // The rates whose code, throttled operation, requests per second and
        // adjustability the recorded listing of this service code states. Two of them
        // AWS publishes as FIXED, so no client is told it may ask for more.
        cloudwatch_request_rate("ListDashboards", 10.0).code("L-69C44FFD"),
        cloudwatch_request_rate("DescribeAnomalyDetectors", 10.0).code("L-6300B446"),
        cloudwatch_request_rate("DescribeAlarmsForMetric", 9.0)
            .code("L-E2310E7A")
            .fixed(),
        cloudwatch_request_rate("EnableAlarmActions", 3.0)
            .code("L-CA907765")
            .fixed(),
        // A rate CloudWatch puts on ONE METRIC-MATH FUNCTION rather than on an API:
        // the quota counts how often the anomaly-detection band may be computed across
        // all `GetMetricData` requests in a second, and AWS publishes it as not
        // adjustable. It carries no `UsageMetric`, because `AWS/Usage` publishes a
        // `CallCount` series per API operation and no series counts one function's use
        // inside a call of `GetMetricData`.
        quota("Rate of ANOMALY_DETECTION_BAND usage in GetMetricData", 1000.0)
            .code("L-722EAE58")
            .fixed()
            .period(1, "SECOND")
            .describe(
                "The maximum number of times the ANOMALY_DETECTION_BAND function can be \
                 used in all GetMetricData requests, per second, in this account in the \
                 current region.",
            ),
    ]
}
