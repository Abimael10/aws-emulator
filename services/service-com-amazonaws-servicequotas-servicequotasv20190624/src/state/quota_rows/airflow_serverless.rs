// SPDX-License-Identifier: CC0-1.0

//! Amazon MWAA Serverless's published default-quota catalog.

use super::*;

/// Amazon MWAA Serverless's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `airflow-serverless` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The one
/// other quota that listing carries — the account's workflow count (`L-D905802C`, whose
/// quota name it states while its default value stays withheld) — is left to the
/// default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
///
/// Where the listing states a row's name and value but withholds whether an increase may
/// be asked for, the row carries the adjustability of its class — this service's sourced
/// rows are published as adjustable — marked at the row rather than guessed at a second
/// time per row.
pub(in crate::state) fn airflow_serverless_quotas() -> Vec<QuotaRow> {
    vec![
        // The XCom plane of one task instance: a SIZE quota, measured in kilobytes
        // rather than counted, whose published sentence states that measure in words.
        quota("Maximum task instance XCom data size", 100.0)
            .code("L-053986BB")
            .unit("Kilobytes")
            .describe("Maximum task instance XCom data size in kilobytes"),
        // How long ONE task of a workflow may run before the service times it out. The
        // `Unit` the listing publishes the timeout under is the unitless `None` every row
        // of this catalog starts from; the measure is stated by the row's own sentence,
        // which words the timeout in minutes.
        quota("Maximum task execution timeout", 60.0)
            .code("L-2C1A393B")
            .describe("Maximum task execution timeout in minutes"),
        // The VERSION plane of one workflow, beside the RUN plane of the whole account:
        // the first is counted per workflow, the second account-wide in the Region, and
        // each is a plain count, with the sentence this service publishes for it.
        quota("Maximum workflow versions per workflow", 50.0)
            .code("L-430BC7B5")
            .describe("Maximum number of versions that can be created per workflow"),
        quota("Maximum concurrent workflow runs per account", 100.0)
            .code("L-643FB251")
            .describe("Maximum number of concurrent workflow runs per account"),
        // The definition DOCUMENT of a workflow: a size quota measured in kilobytes,
        // published as adjustable, whose sentence states that measure in words.
        quota("Maximum workflow definition size", 50.0)
            .code("L-B3B5D226")
            .unit("Kilobytes")
            .describe("Maximum workflow definition size in kilobytes"),
        // The account's workflow-code storage, measured in megabytes, with the sentence
        // this service publishes for it — which states the measure in mebibytes, the way
        // the service words it.
        quota("Maximum code storage per account", 76800.0)
            .code("L-E488E4B4")
            .unit("Megabytes")
            .describe("Maximum total size (MiB) of workflow code stored per account."),
    ]
}
