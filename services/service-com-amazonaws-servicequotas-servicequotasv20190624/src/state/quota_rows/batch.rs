// SPDX-License-Identifier: CC0-1.0

//! AWS Batch's published default-quota catalog.

use super::*;

/// One AWS Batch API THROTTLE quota: the transactions per second the service accepts for
/// one operation of the account.
///
/// Batch publishes a row of this class for each throttled operation, and every member but
/// the operation's own name and the value is the same across the class — the `QuotaName` is
/// the operation's name in "Transactions per second for <Api> limit", the rate is published
/// as NOT adjustable, and the `Description` is the sentence Batch publishes for the class
/// with the operation named in it — so one constructor fills them and two rows of the class
/// cannot drift apart.
fn transactions_per_second(code: &str, api_name: &str, per_second: f64) -> QuotaRow {
    quota(
        format!("Transactions per second for {api_name} limit"),
        per_second,
    )
    .code(code)
    .fixed()
    .describe(&format!(
        "Maximum number of transactions per second (TPS) for each account for {api_name} \
         operations."
    ))
}

/// AWS Batch's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `batch` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code batch` reports, which is
/// the "AWS Batch service quotas" table of the AWS Batch User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota and for the
/// text each row is published with. Every row carries the `L-` code AWS publishes the quota
/// under, so a client that read a code out of AWS's catalog resolves the same quota here.
///
/// Batch states its limits in the "Maximum …" voice of its own quota table rather than in a
/// sentence about the account, and the account's planes are counted "per account, per
/// region", so that is the wording a row carries.
///
/// Batch publishes its whole default set as NOT adjustable — the recorded listing reports
/// it so for the account's compute-environment and job-queue limits as much as for its
/// per-API throttles — so every row here is fixed.
///
/// Batch publishes a throttle for only SOME of its operations: the per-job-queue
/// compute-environment, share-identifier, service-environment and service-job-payload
/// quotas are published under codes a hand-keyed table had given to throttles of other
/// operations, and a plane left with no code of its own is published under no row at all.
pub(in crate::state) fn batch_quotas() -> Vec<QuotaRow> {
    vec![
        // The newer service-job plane's submission throttle, which AWS publishes without
        // the trailing "limit" the rest of the class carries.
        quota("Transactions per second for SubmitServiceJob", 5.0)
            .code("L-0194C9AB")
            .fixed()
            .describe(
                "Maximum number of transactions per second (TPS) for each account for \
                 SubmitServiceJob operations.",
            ),
        quota("Compute environment limit", 50.0)
            .code("L-144F0CA5")
            .fixed()
            .describe("Maximum number of compute environments per account, per region."),
        quota("Job queue limit", 50.0)
            .code("L-4CEA37AD")
            .fixed()
            .describe("Maximum number of job queues per account, per region."),
        // The account's service-environment plane, which Batch publishes beside the
        // per-job-queue one (`L-80D92D24`) under the singular name below.
        quota("Service environment", 50.0)
            .code("L-61E3E54E")
            .fixed()
            .describe("Maximum number of service environments per account, per region."),
        quota("Submitted state jobs limit", 1_000_000.0)
            .code("L-65F8BA2C")
            .fixed()
            .describe("Maximum number of jobs in SUBMITTED state."),
        quota("Job payload size limit", 30.0)
            .code("L-6B86CF1E")
            .fixed()
            .describe(
                "Maximum job payload size (for SubmitJob API operations), measured in KiB.",
            ),
        quota("Service environments per job queue", 1.0)
            .code("L-80D92D24")
            .fixed()
            .describe("Maximum number of service environments per job queue."),
        // UNSOURCED BINDING: no source names the quota Batch publishes under
        // `L-A2F30F35`, so the code goes on answering the row it has answered rather than
        // resolving to nothing. It is replaced the moment that quota can be sourced.
        quota("Scheduling policy limit", 100.0)
            .code("L-A2F30F35")
            .fixed()
            .describe("Maximum number of scheduling policies per account, per region."),
        quota("Job dependencies limit", 20.0)
            .code("L-AD075A76")
            .fixed()
            .describe("Maximum number of job dependencies per job."),
        // The service-job plane's payload ceiling, which Batch publishes beside the
        // SubmitJob one (`L-6B86CF1E`) in the same measured-size voice and at the same KiB.
        quota("SubmitServiceJob total payload size limit", 30.0)
            .code("L-B15D0E72")
            .fixed()
            .describe(
                "Maximum total payload size (for SubmitServiceJob API operations), measured \
                 in KiB.",
            ),
        quota("Share identifiers per job queue limit.", 500.0)
            .code("L-C997A649")
            .fixed()
            .describe("Maximum number of share identifiers per job queue."),
        quota("Job definition size limit", 24.0)
            .code("L-E6A2743D")
            .fixed()
            .describe(
                "Maximum job definition size (for RegisterJobDefinition API operations), \
                 measured in KiB.",
            ),
        transactions_per_second("L-E985971C", "SubmitJob", 50.0),
        quota("Compute environments per job queue limit.", 3.0)
            .code("L-F8102809")
            .fixed()
            .describe("Maximum number of compute environments per job queue."),
        quota("Maximum array size limit", 10_000.0)
            .code("L-F9381E33")
            .fixed()
            .describe("Maximum array size for array jobs."),
    ]
}
