// SPDX-License-Identifier: CC0-1.0

//! AWS CloudShell's published default-quota catalog.

use super::*;

/// AWS CloudShell's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `cloudshell` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// sentences this service publishes for its rows are not among the data available here,
/// so the rows publish none rather than wording of this catalog's own, and the unit of a
/// row the listing states none for stays the unitless `None` every row starts from.
pub(in crate::state) fn cloudshell_quotas() -> Vec<QuotaRow> {
    vec![
        // The HOME DIRECTORY of a shell user: a size quota, measured in gigabytes rather
        // than counted, which AWS publishes as not adjustable.
        quota("Home directory size", 1.0)
            .code("L-91D31FC1")
            .unit("Gigabytes")
            .fixed(),
        // How much of the service an account may use in a billing month, which AWS
        // publishes as adjustable.
        quota("Monthly usage", 200.0).code("L-937D704D"),
        // How long the service keeps a shell's home directory after its last use: a
        // retention window AWS publishes as not adjustable.
        quota("Data retention", 120.0).code("L-AFB6CE78").fixed(),
        // How many shells one account may have open at once.
        quota("Concurrent shells", 10.0).code("L-C2A56602"),
    ]
}
