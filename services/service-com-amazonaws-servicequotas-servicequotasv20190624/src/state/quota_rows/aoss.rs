// SPDX-License-Identifier: CC0-1.0

//! Amazon OpenSearch Serverless's published default-quota catalog.

use super::*;

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
        // A policy-DOCUMENT size quota, measured in bytes rather than counted, which AWS
        // publishes as not adjustable, with the sentence this service publishes for it.
        quota("Encryption policy size per policy", 10240.0)
            .code("L-2005AF7E")
            .unit("Bytes")
            .fixed()
            .describe("The maximum size of an encryption policy in bytes."),
        // The DEFAULT ceiling of the indexing capacity an account's collections may scale
        // to, counted in OpenSearch Compute Units: AWS publishes the account-wide setting
        // as fixed and the per-collection-group one as adjustable. Neither sentence is
        // among the data available here, so neither row publishes one rather than wording
        // of this catalog's own.
        quota("Default indexing MAX OCU setting", 10.0)
            .code("L-50FA809B")
            .fixed(),
        // The second policy-document size plane, measured in bytes like the first and
        // published as not adjustable.
        quota("Network policy size per policy", 10240.0)
            .code("L-877395DB")
            .unit("Bytes")
            .fixed(),
        quota("Default indexing MAX Collection Group OCU setting", 96.0).code("L-FBAA6E20"),
    ]
}
