// SPDX-License-Identifier: CC0-1.0

//! Amazon Bedrock Web Search's published default-quota catalog.

use super::*;

/// Amazon Bedrock Web Search's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `bedrock-websearch` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// The row is published under the `L-` code AWS mints for it. The other quota that
/// listing carries — the fetch tool's own query rate (`L-C4E07D49`, whose quota name it
/// states while its default value stays withheld) — is left to the default-quota-set gap
/// rather than published under a guessed value, the way [`super::aws_mcp_quotas`] leaves
/// the rate rows of that service whose values no source states.
///
/// The window and the usage series AWS meters a query rate of this service over are not
/// among the data available here, so the row publishes neither a `Period` nor a
/// `UsageMetric`, and its sentence is withheld too, so it publishes none.
pub(in crate::state) fn bedrock_websearch_quotas() -> Vec<QuotaRow> {
    vec![
        // The QUERY rate of the web search tool: measured in queries per second rather
        // than counted, and published as adjustable.
        quota("Rate of Bedrock Web Search Tool queries", 10.0)
            .code("L-39158EC4")
            .unit("Count/Second"),
    ]
}
