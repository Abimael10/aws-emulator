// SPDX-License-Identifier: CC0-1.0

//! Amazon OpenSearch Serverless's published default-quota catalog.

use super::*;

/// One policy-DOCUMENT size quota of this service: how large one policy of a given kind
/// may be.
///
/// Every member of such a row but the policy kind's own name is the same across the
/// class — measured in bytes rather than counted, published as NOT adjustable, and worded
/// with the one sentence this service publishes for the class, which names the kind of
/// policy and states the measure — so one constructor fills it and two rows of the class
/// cannot drift apart. The sentence template is the one the listing states for this
/// service's encryption-policy row; only the policy kind differs between the rows.
fn policy_size(code: &str, policy_kind: &str, bytes: f64) -> QuotaRow {
    let kind = policy_kind.to_lowercase();
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    quota(format!("{policy_kind} policy size per policy"), bytes)
        .code(code)
        .unit("Bytes")
        .fixed()
        .describe(&format!(
            "The maximum size of {article} {kind} policy in bytes."
        ))
}

/// Amazon OpenSearch Serverless's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `aoss` service code, which states
/// which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The other
/// quotas that listing carries — the SAML-provider plane (`L-1D4405ED`, whose quota name
/// it states while its value stays withheld) among them — are left to the
/// default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn aoss_quotas() -> Vec<QuotaRow> {
    vec![
        // The collection plane of ONE collection group, on the capacity model this
        // service publishes as the classic one, with the sentence this service publishes
        // for it. AWS publishes it as NOT adjustable: the capacity model fixes it, so a
        // client that reads adjustability to decide whether an increase can be asked for
        // is told the truth about this one.
        quota("Collections per Collection Group (Classic)", 1500.0)
            .code("L-1A85D401")
            .fixed()
            .describe("The maximum number of collections per collection group."),
        // The two policy-document size planes of this service, one per kind of policy.
        policy_size("L-2005AF7E", "Encryption", 10240.0),
        // The DEFAULT ceiling of the indexing capacity an account's collections may scale
        // to, counted in OpenSearch Compute Units: AWS publishes the account-wide setting
        // as fixed and the per-collection-group one as adjustable. No sentence of this
        // class is among the data available here — neither row of it is sourced — so
        // neither publishes one rather than wording of this catalog's own.
        quota("Default indexing MAX OCU setting", 10.0)
            .code("L-50FA809B")
            .fixed(),
        policy_size("L-877395DB", "Network", 10240.0),
        quota("Default indexing MAX Collection Group OCU setting", 96.0).code("L-FBAA6E20"),
    ]
}
