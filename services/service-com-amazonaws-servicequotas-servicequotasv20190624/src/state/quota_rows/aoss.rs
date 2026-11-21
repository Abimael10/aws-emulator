// SPDX-License-Identifier: CC0-1.0

//! Amazon OpenSearch Serverless's published default-quota catalog.

use super::*;

/// Amazon OpenSearch Serverless's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `aoss` service code, which states
/// which quota each `QuotaCode` names and the facts published under it.
///
/// The row is published under the `L-` code AWS mints for it. The other quotas that
/// listing carries — the second policy-size plane (`L-877395DB`, whose unit it names
/// while its value stays withheld), the collection plane of a collection group
/// (`L-1A85D401`) and the indexing OCU settings (`L-50FA809B`, `L-FBAA6E20`) — are left
/// to the default-quota-set gap rather than published under a guessed fact, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn aoss_quotas() -> Vec<QuotaRow> {
    // A policy-DOCUMENT size quota, measured in bytes rather than counted, which AWS
    // publishes as not adjustable. The sentence this service publishes for the row is
    // not among the data available here — the recorded listing shows the forms this
    // catalog words such a sentence in are not the text it carries, and withholds the
    // text — so the row publishes no description rather than wording of this catalog's
    // own.
    vec![quota("Encryption policy size per policy", 10240.0)
        .code("L-2005AF7E")
        .unit("Bytes")
        .fixed()]
}
