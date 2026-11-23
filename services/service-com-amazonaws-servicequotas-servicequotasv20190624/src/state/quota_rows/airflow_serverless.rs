// SPDX-License-Identifier: CC0-1.0

//! Amazon MWAA Serverless's published default-quota catalog.

use super::*;

/// Amazon MWAA Serverless's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `airflow-serverless` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// Both rows are published under the `L-` code AWS mints for them, in code order. The
/// other quotas that listing carries — `L-2C1A393B`, `L-430BC7B5`, `L-643FB251`,
/// `L-B3B5D226` and `L-D905802C` — are left to the default-quota-set gap rather than
/// published under a guessed fact: no source available here states the quota name or the
/// default value published under any of those codes, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn airflow_serverless_quotas() -> Vec<QuotaRow> {
    vec![
        // The XCom plane of one task instance: a SIZE quota, measured in kilobytes
        // rather than counted, whose published sentence states that measure in words.
        quota("Maximum task instance XCom data size", 100.0)
            .code("L-053986BB")
            .unit("Kilobytes")
            .describe("Maximum task instance XCom data size in kilobytes"),
        // The account's workflow-code storage, measured in megabytes. The sentence this
        // service publishes for the row is not among the data available here — the
        // recorded listing shows the form the XCom row's own sentence takes is not the
        // text it carries for this one, and withholds the text — so the row publishes no
        // description rather than wording of this catalog's own.
        quota("Maximum code storage per account", 76800.0)
            .code("L-E488E4B4")
            .unit("Megabytes"),
    ]
}
