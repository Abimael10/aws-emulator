// SPDX-License-Identifier: CC0-1.0

//! AWS X-Ray's published default-quota catalog.

use super::*;

/// AWS X-Ray's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `xray` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code xray` reports, which is
/// the "AWS X-Ray endpoints and quotas" page of the AWS General Reference), and the
/// `QuotaCode`s that catalog is keyed on.
///
/// X-Ray publishes the sampling and grouping planes of the account beside the request
/// rates of the APIs its agents and consoles call, which is what the rows built by
/// [`request_rate`] are: a rate, so each carries the one-second window AWS publishes it
/// over and the `AWS/Usage` `CallCount` of the API it throttles.
///
/// The recorded listing states the quota published under `L-8C0C998A` (the account's
/// custom sampling rules in the Region, 25), under `L-DB51D338` (the tags of one custom
/// sampling rule, 50, NOT adjustable), under `L-428FB8E0` (the dynamic upper limit on a
/// trace document, 500 Kilobytes, NOT adjustable) and under `L-C6B6F05D` (the annotations
/// indexed within one trace). The `BatchGetTraces`, `PutTraceSegments` and
/// `GetTraceSummaries` rates this catalog used to key on three of those codes are NOT
/// published here, and neither is the code it used to key the per-trace annotations on
/// (`L-3B8EFF88`): no source available here states AWS's code for the three rates or AWS's
/// quota for `L-3B8EFF88`, so they are left to the default-quota-set gap rather than served
/// under another quota's code. A row whose published `Description` no source available here
/// states carries none, rather than a sentence this emulator words itself.
pub(in crate::state) fn xray_quotas() -> Vec<QuotaRow> {
    let rate = |code: &str, api: &str, per_second: f64| {
        request_rate(code, XRAY_USAGE_SERVICE, api, per_second)
    };
    vec![
        quota("Trace document size (dynamic upper limit)", 500.0)
            .code("L-428FB8E0")
            .unit("Kilobytes")
            .fixed(),
        quota("Groups", 25.0)
            .code("L-5DB17969")
            .describe("The maximum number of groups in this account in the current Region."),
        rate("L-7F259013", "GetServiceGraph", 10.0),
        quota("Custom sampling rules per region", 25.0).code("L-8C0C998A"),
        rate("L-998BFF16", "GetSamplingRules", 10.0),
        quota("Annotations per trace", 50.0)
            .code("L-C6B6F05D")
            .fixed()
            .describe("The maximum number of annotations on one trace."),
        quota("Trace document size", 64.0)
            .code("L-D781C0FD")
            .unit("Kilobytes")
            .fixed()
            .describe("The maximum size, in KB, of one trace segment document."),
        quota("Tags per custom sampling rule", 50.0)
            .code("L-DB51D338")
            .fixed(),
        rate("L-E2DD2778", "PutTelemetryRecords", 10_000.0),
        quota("Trace retention", 30.0)
            .code("L-E75A2BBF")
            .unit("Days")
            .fixed()
            .describe("The time, in days, that X-Ray retains trace data."),
    ]
}

/// The name `AWS/Usage` publishes AWS X-Ray's API call counts under.
///
/// It is the `Service` dimension value a `GetMetricData` query has to name for one of
/// these counts, so it is held once here rather than spelled at each row.
const XRAY_USAGE_SERVICE: &str = "X-Ray";
