// SPDX-License-Identifier: CC0-1.0

//! AWS App Studio's published default-quota catalog.

use super::*;

/// AWS App Studio's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `appstudio` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Both rows are published under the `L-` code AWS mints for them, in code order.
pub(in crate::state) fn appstudio_quotas() -> Vec<QuotaRow> {
    vec![
        // How many of the account's applications may be PUBLISHED, beside how many it
        // may hold at all — AWS publishes both as not adjustable. The sentences this
        // service publishes for them are not among the data available here — the
        // recorded listing shows the forms this catalog words such a sentence in are not
        // the text it carries, and withholds the text — so the rows publish no
        // description rather than wording of this catalog's own.
        quota("Published applications", 6.0).code("L-45B0D852").fixed(),
        quota("Applications", 20.0).code("L-B07DC829").fixed(),
    ]
}
