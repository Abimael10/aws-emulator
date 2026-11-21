// SPDX-License-Identifier: CC0-1.0

//! AWS Marketplace's published default-quota catalog.

use super::*;

/// One THROTTLE quota of the AWS Marketplace Discovery plane: how many calls of one of
/// its operations the account is served.
///
/// Every member of such a row but the operation's own name and the value is the same
/// across the class: AWS publishes the quota under the plane it throttles, named in
/// brackets before the operation's own name. One constructor fills that so two rows of
/// the class cannot drift apart. The window, the usage metric and the sentence this
/// service words a throttle quota with are not among the data available here, so these
/// rows publish none of them rather than members of this catalog's own.
fn marketplace_discovery_throttle(code: &str, api_name: &str, calls: f64) -> QuotaRow {
    quota(
        format!("[AWS Marketplace Discovery] {api_name} throttle quota"),
        calls,
    )
    .code(code)
}

/// AWS Marketplace's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `aws-marketplace` service code,
/// which states which operation each `QuotaCode` throttles, the plane the quota is
/// published under and the calls published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// quotas that listing carries under a code whose published VALUE no source available
/// here states — the offer-terms throttle among them — are left to the default-quota-set
/// gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn aws_marketplace_quotas() -> Vec<QuotaRow> {
    vec![
        marketplace_discovery_throttle("L-68F80CEA", "GetProduct", 5.0),
        marketplace_discovery_throttle("L-96311249", "SearchListings", 10.0),
        marketplace_discovery_throttle("L-CD4DC1E4", "GetOffer", 5.0),
        marketplace_discovery_throttle("L-DF8B8977", "ListFulfillmentOptions", 5.0),
    ]
}
