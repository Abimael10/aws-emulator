// SPDX-License-Identifier: CC0-1.0

//! Amazon AI Operations' published default-quota catalog.

use super::*;

/// The `AWS/Usage` display name CloudWatch publishes Amazon AI Operations' resource
/// counts under, which is the `Service` dimension of every metered row of this catalog.
const AIOPS_USAGE_SERVICE: &str = "Amazon AI Operations";

/// Amazon AI Operations' published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `aiops` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code aiops` reports), and
/// the recorded default-quota listing of that code for which `QuotaCode` names which
/// plane, its published name, default, adjustability, window, usage metric and
/// `Description` — the text of the investigation-group and monthly-investigation rows
/// below is stated verbatim by the evidence behind this catalog, so each carries its
/// own.
///
/// Every row is published under the `L-` code AWS mints for it, in code order.
pub(in crate::state) fn aiops_quotas() -> Vec<QuotaRow> {
    vec![
        // The investigations a month may cover, which AWS publishes over a ONE-HOUR
        // window rather than the five-minute one its resource counts are published over.
        quota("Monthly investigations", 150.0)
            .code("L-4CD81CD5")
            .fixed()
            .period(1, "HOUR")
            .metric(usage_metric(
                AIOPS_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", "MonthlyAIAssistedInvestigations"),
                    ("Type", "Resource"),
                ],
            ))
            .describe(
                "The maximum number of AI-assisted investigations per month in this \
                 account in the current region.",
            ),
        quota("Investigation groups", 1.0)
            .code("L-5EC19707")
            .fixed()
            .describe(
                "The maximum number of investigation groups that can be created per \
                 account in a region.",
            ),
        quota("Concurrent active investigations", 2.0)
            .code("L-EF31E0C4")
            .fixed()
            .metric(usage_metric(
                AIOPS_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", "ConcurrentActiveInvestigations"),
                    ("Type", "Resource"),
                ],
            ))
            .describe(
                "The maximum number of concurrent investigations with active AI \
                 analysis per account per region.",
            ),
    ]
}
