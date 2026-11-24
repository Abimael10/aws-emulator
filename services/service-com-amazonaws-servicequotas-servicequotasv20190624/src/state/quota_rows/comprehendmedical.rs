// SPDX-License-Identifier: CC0-1.0

//! Amazon Comprehend Medical's published default-quota catalog.

use super::*;

/// One per-operation THROUGHPUT quota of this service, published as so many units of
/// work per second for ONE operation.
///
/// This service publishes its throughput quotas as `<measure> for the <Api> operation`,
/// so the window is stated in the quota's own name and the rows carry no `Period` or
/// `UsageMetric` block. Every member of such a row but the measure, the operation's own
/// name and the value is the same across the class, so one constructor fills them and two
/// rows of the class cannot drift apart. The sentences this service publishes for the
/// class are not among the data available here, so the rows publish none rather than
/// wording of this catalog's own.
fn per_operation_throughput(code: &str, measure: &str, api_name: &str, per_second: f64) -> QuotaRow {
    quota(format!("{measure} for the {api_name} operation"), per_second).code(code)
}

/// One per-operation REQUEST-rate quota: the transactions per second this service serves
/// calls of that one operation at.
fn transactions_per_second(code: &str, api_name: &str, per_second: f64) -> QuotaRow {
    per_operation_throughput(code, "Transactions per second (TPS)", api_name, per_second)
}

/// One per-operation TEXT-rate quota: the characters per second this service accepts for
/// that one operation, which is how it publishes the throughput of its synchronous
/// inference operations.
fn characters_per_second(code: &str, api_name: &str, per_second: f64) -> QuotaRow {
    per_operation_throughput(code, "Characters per second (CPS)", api_name, per_second)
}

/// Amazon Comprehend Medical's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `comprehendmedical` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The rest
/// of that listing's codes name operations and measures no source available here states,
/// and a code identifies ONE quota, so those rows are left to the default-quota-set gap
/// rather than published under a guessed binding, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn comprehendmedical_quotas() -> Vec<QuotaRow> {
    vec![
        // The two SNOMED CT inference-job operations are published at the same rate, the
        // one that STARTS a job as not adjustable.
        transactions_per_second("L-23625ADE", "StartSNOMEDCTInferenceJob", 5.0).fixed(),
        // The synchronous ICD-10-CM inference operation is published as a TEXT rate
        // instead: characters per second rather than calls per second.
        characters_per_second("L-2D8DD360", "InferICD10CM", 40000.0),
        transactions_per_second("L-3645AB89", "StopSNOMEDCTInferenceJob", 5.0),
        // The PHI detection job plane, published as adjustable.
        transactions_per_second("L-63264B74", "StartPHIDetectionJob", 5.0),
    ]
}
