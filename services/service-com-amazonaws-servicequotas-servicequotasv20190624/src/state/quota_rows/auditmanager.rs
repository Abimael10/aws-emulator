// SPDX-License-Identifier: CC0-1.0

//! AWS Audit Manager's published default-quota catalog.

use super::*;

/// AWS Audit Manager's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `auditmanager` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// quotas that listing carries under a code whose published default VALUE no source
/// available here states — the running-assessment plane (`L-92B50F18`) among them — are
/// left to the default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn auditmanager_quotas() -> Vec<QuotaRow> {
    vec![
        // The CONTROL planes: the account's own controls, and how many of them one
        // framework may hold.
        quota("Custom controls", 500.0).code("L-0255B75F"),
        quota("Controls per framework", 1400.0).code("L-724DD74D"),
        // The FRAMEWORK plane of the account.
        quota("Custom frameworks", 100.0).code("L-8935A6F1"),
        // The assessment plane, counted across every assessment of the account rather
        // than per assessment.
        quota("Accounts in scope across all assessments", 250.0).code("L-BEA222D4"),
    ]
}
