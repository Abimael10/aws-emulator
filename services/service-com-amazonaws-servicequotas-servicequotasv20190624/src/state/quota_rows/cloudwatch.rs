// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Omni plane's published default-quota catalog, which the `cloudwatch`
//! service code publishes (Amazon CloudWatch itself is integrated under `monitoring`).

use super::*;

/// The `AWS/Usage` display name this plane publishes its usage under, which is the
/// `Service` dimension of its metered quota.
const CLOUDWATCH_OMNI_USAGE_SERVICE: &str = "CloudWatch Omni";

/// The CloudWatch Omni plane's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `cloudwatch` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// The row is published under the `L-` code AWS mints for it.
pub(in crate::state) fn cloudwatch_quotas() -> Vec<QuotaRow> {
    vec![
        // The agent CREDITS an account may spend in a billing month: AWS publishes it as
        // not adjustable, metered by the credits-used series CloudWatch publishes for
        // this plane, over the window that series is published on — an hour, which this
        // row states rather than the window a resource count is reported on. The sentence
        // this service publishes for the row is not among the data available here, so it
        // publishes none rather than wording of this catalog's own.
        quota("Monthly Omni agent credits", 100000.0)
            .code("L-111E0C2D")
            .fixed()
            .period(1, "HOUR")
            .metric(usage_metric(
                CLOUDWATCH_OMNI_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", "OmniAgentCreditsUsed"),
                    ("Type", "Resource"),
                ],
            )),
    ]
}
