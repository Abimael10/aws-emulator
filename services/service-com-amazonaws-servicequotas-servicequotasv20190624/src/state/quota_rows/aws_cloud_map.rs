// SPDX-License-Identifier: CC0-1.0

//! AWS Cloud Map's published default-quota catalog.

use super::*;

/// AWS Cloud Map's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `AWSCloudMap` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The other
/// quotas that listing carries — the per-service instance plane (`L-2DA90E5C`, whose
/// published value is withheld) among them — are left to the default-quota-set gap rather
/// than published under a guessed value, the way [`super::access_analyzer_quotas`] leaves
/// the rest of that service's catalog.
///
/// The discovery rows are RATES, and the window and `AWS/Usage` dimensions AWS meters
/// them with are not among the data available here, so they publish neither a `Period`
/// nor a `UsageMetric` rather than members this catalog would be guessing at — the same
/// reason [`super::aws_mcp_quotas`] leaves its own rate rows unmetered. Their sentences
/// are withheld too, so the rows publish none.
pub(in crate::state) fn aws_cloud_map_quotas() -> Vec<QuotaRow> {
    vec![
        // The revision-aware discovery call has a rate of its own, beside the two planes
        // of the plain discovery call below.
        quota("DiscoverInstancesRevision operation per account rate", 3000.0)
            .code("L-0BA10AAE"),
        // The namespace plane of a Region: a plain count rather than a rate.
        quota("Namespaces per Region", 50.0).code("L-0FE3F50E"),
        // Discovery is published as two planes of one call: the rate an account may KEEP
        // UP, and the burst allowance of requests that may arrive at once above it, which
        // is why the burst value is the larger of the two.
        quota("DiscoverInstances operation per account steady rate", 1000.0)
            .code("L-514A639A"),
        quota("DiscoverInstances operation per account burst rate", 2000.0)
            .code("L-76CF203B"),
    ]
}
