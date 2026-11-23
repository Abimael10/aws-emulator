// SPDX-License-Identifier: CC0-1.0

//! AWS DevOps Agent's published default-quota catalog.

use super::*;

/// AWS DevOps Agent's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `aidevops` service code (what
/// `aws service-quotas list-aws-default-service-quotas --service-code aidevops`
/// reports), and the recorded default-quota listing of that code for which `QuotaCode`
/// names which plane, its published name, unit, default and adjustability.
///
/// THE SHAPE OF THIS SERVICE'S ROWS: it names a quota with the hyphenated SLUG of what
/// the quota counts. The evidence behind this catalog states the published `Description`
/// of two of them — the agent-space plane (`L-510AF4A9`, "Agent spaces per account per
/// Region") and the evaluation plane (`L-4FF187E6`) — each that row's own slug read as
/// prose; it shows that same reading is NOT the text of the memory, memory-store and
/// on-demand-invocation planes and withholds theirs, so those publish no description
/// rather than wording of this catalog's own.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The one
/// further code that listing carries, `L-1553F789` (the investigations an agent space
/// may run at once), states no default value here, so it is left to the
/// default-quota-set gap rather than published at a value borrowed from a neighbouring
/// plane.
pub(in crate::state) fn aidevops_quotas() -> Vec<QuotaRow> {
    vec![
        quota("concurrent-on-demand-invocations-per-agent-space", 10.0)
            .code("L-3D8A06B8")
            .unit("Count"),
        quota("concurrent-evaluations-per-agent-space", 1.0)
            .code("L-4FF187E6")
            .unit("Count")
            .fixed()
            .describe("Concurrent evaluations per agent space"),
        // The agent-space plane of the account, the one row of this service published
        // under the countable `None` rather than under `Count`.
        quota("agent-spaces-per-account-per-region", 100.0)
            .code("L-510AF4A9")
            .describe("Agent spaces per account per Region"),
        // The memory planes, which AWS publishes as NOT adjustable — against the
        // on-demand-invocation plane above, whose increase it takes.
        quota("memory-stores-per-agent-space", 50.0)
            .code("L-D774F842")
            .unit("Count")
            .fixed(),
        quota("memories-per-memory-store", 200.0)
            .code("L-F1DF5F94")
            .unit("Count")
            .fixed(),
    ]
}
