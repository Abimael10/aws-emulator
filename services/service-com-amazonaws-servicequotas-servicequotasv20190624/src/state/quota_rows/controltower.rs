// SPDX-License-Identifier: CC0-1.0

//! AWS Control Tower's published default-quota catalog.

use super::*;

/// AWS Control Tower's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `controltower` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order.
pub(in crate::state) fn controltower_quotas() -> Vec<QuotaRow> {
    vec![
        // The CONCURRENCY planes of this service's two provisioning operations: how many
        // account operations may run at once, which AWS publishes as adjustable, and how
        // many organizational-unit operations may, which it publishes as not adjustable
        // and bounded to one at a time.
        quota("Concurrent account operations quota", 5.0).code("L-04D09B14"),
        quota("Concurrent organization units (OUs) operations quota", 1.0)
            .code("L-6579BCE4")
            .fixed(),
        // The account plane of ONE organizational unit, published as not adjustable. The
        // sentence this service publishes for the row is not among the data available
        // here, so the row publishes none rather than wording of this catalog's own.
        quota("Number of accounts in a single OU quota", 1000.0)
            .code("L-E9464183")
            .fixed(),
    ]
}
