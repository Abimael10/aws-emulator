// SPDX-License-Identifier: CC0-1.0

//! AWS MCP's published default-quota catalog.

use super::*;

/// One per-TOOL request-rate quota of this service: the requests per second it serves
/// calls of one of its tools at.
///
/// Every member of such a row but the tool's own name and the value is the same across
/// the class: AWS publishes the quota as `Rate of <tool> tool requests`, measured in
/// calls per second rather than counted. One constructor fills that so two rows of the
/// class cannot drift apart. The window, the usage metric and the sentence this service
/// words a rate quota with are not among the data available here, so these rows publish
/// none of them rather than members of this catalog's own.
fn mcp_tool_request_rate(code: &str, tool_name: &str, per_second: f64) -> QuotaRow {
    quota(format!("Rate of {tool_name} tool requests"), per_second)
        .code(code)
        .unit("Count/Second")
}

/// AWS MCP's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `aws-mcp` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// quotas that listing carries under a code whose published VALUE no source available
/// here states — the rate rows of the `call_aws` (`L-24E93970`) and
/// `search_documentation` (`L-A2044E93`) tools among them — are left to the
/// default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn aws_mcp_quotas() -> Vec<QuotaRow> {
    vec![
        mcp_tool_request_rate("L-7A518ECF", "retrieve_skill", 10.0),
        // The SESSION plane of the account, published over the window a resource count
        // is reported on. The `AWS/Usage` dimension values that meter it are not among
        // the data available here, so the row publishes the window without a usage
        // metric rather than one whose dimensions this catalog would be guessing at —
        // the same reason the `resource-groups` row is left unpublished.
        quota("Concurrent sessions per account", 180.0)
            .code("L-E5DA7401")
            .period(crate::state::RESOURCE_COUNT_PERIOD_MINUTES, "MINUTE"),
    ]
}
