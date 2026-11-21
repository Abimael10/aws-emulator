// SPDX-License-Identifier: CC0-1.0

//! Amplify UI Builder's published default-quota catalog.

use super::*;

/// Amplify UI Builder's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `amplifyuibuilder` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// quotas that listing carries under a code whose published default VALUE no source
/// available here states — `L-4FE3AB63` and `L-A25B72F2` (the component- and form-size
/// planes, whose unit it names while their values stay withheld) and `L-429DD3BA` (the
/// per-app theme count) — are left to the default-quota-set gap rather than published
/// under a guessed fact, the way [`super::access_analyzer_quotas`] leaves the rest of
/// that service's catalog.
pub(in crate::state) fn amplifyuibuilder_quotas() -> Vec<QuotaRow> {
    vec![
        // A document-SIZE quota, measured in kilobytes rather than counted, which AWS
        // publishes as not adjustable.
        quota("Theme size", 350.0)
            .code("L-D500C0BA")
            .unit("Kilobytes")
            .fixed()
            .describe("The maximum size (in KB) of the theme."),
        // The view plane of one app, which AWS publishes as not adjustable. The
        // sentence this service publishes for the row is not among the data available
        // here — the recorded listing shows the forms this catalog words such a
        // sentence in are not the text it carries, and withholds the text — so the row
        // publishes no description rather than wording of this catalog's own.
        quota("Views per app", 1000.0).code("L-E4AD9560").fixed(),
    ]
}
