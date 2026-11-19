// SPDX-License-Identifier: CC0-1.0

//! What a `LambdaInvoke` job does when the function it names cannot be invoked at
//! all — the half of the rule this crate can pin on its own, with no Lambda
//! service mounted beside it.
//!
//! The rule under test: a job whose `Operation` is `LambdaInvoke` RUNS, one
//! invocation per manifest entry, and the verdict each invocation came to is what
//! `ProgressSummary` accounts that task by — so an invocation nothing answered is
//! a task that FAILED, the job still reaches the terminal `Complete` its tasks
//! having run, and it carries NO `FailureReasons`: `JobDescriptor$FailureReasons`
//! is "the reasons for the failure of the specified job", which a job that
//! completed had none of. A failed task is accounted for in the job's completion
//! report, under the error code the invocation was refused with, never as a
//! statement about the implementation serving the request.
//!
//! The rows span what the rule generalises over: how many entries the manifest
//! carries, and whether the job's `LambdaInvoke` names a function at all.
//!
//! The other half — what a job whose function DOES answer a verdict per task does
//! to the objects of its list — needs Lambda mounted beside this service, so it
//! is pinned on the assembled router
//! (`core/router/tests/all/s3control_batch_lambda_invoke_job.rs`).

use service_com_amazonaws_s3control_awss3controlservicev20180820::state::{
    JOB_STATUS_COMPLETE, JOB_STATUS_FAILED,
};

use crate::access_point_lifecycle::{xml_escape, REGION, XMLNS};
use crate::batch_job_execution::{csv_manifest, poll_terminal, progress};
use crate::object_store::ObjectStore;

/// The IAM role every job below runs its action with — the modelled `IAMRoleArn`
/// pattern (`^arn:[^:]+:iam::\d{12}:role/`).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/S3BatchOperations";

/// The `TaskStatus` a completion report's row carries for a task that was not
/// carried out.
const TASK_FAILED: &str = "failed";

/// One row of the table.
struct Row {
    case: &'static str,
    /// The keys the manifest lists, in manifest order.
    listed: &'static [&'static str],
    /// The body of `<LambdaInvoke>`.
    invoke: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one entry, a function nothing answers for",
            listed: &["one.txt"],
            invoke: "<FunctionArn>arn:aws:lambda:us-west-2:123456789012:function:tagger\
                     </FunctionArn>",
        },
        Row {
            case: "three entries, a function nothing answers for",
            listed: &["one.txt", "two/nested.txt", "three.txt"],
            invoke: "<FunctionArn>arn:aws:lambda:us-west-2:123456789012:function:tagger\
                     </FunctionArn><InvocationSchemaVersion>2.0</InvocationSchemaVersion>",
        },
        Row {
            // `LambdaInvokeOperation$FunctionArn` is not modelled `@required`
            // (`smithy_query(query="shape", shape="LambdaInvokeOperation")` lists no required
            // member), so `CreateJob`'s own validation accepts the job: naming one action is
            // all it asks of `Operation`. The invocation that names no function is therefore
            // a failure of the TASK, at run time, which is what this row pins.
            case: "an invocation naming no function",
            listed: &["one.txt", "two.txt"],
            invoke: "",
        },
    ]
}

/// RULE: a `LambdaInvoke` job whose invocations came to no `Succeeded` verdict
/// settles `Complete` with every entry of its object list accounted for as a
/// failed task, and carries no `FailureReasons` — never a failure the service
/// authored about itself.
#[tokio::test]
async fn a_lambda_invoke_job_accounts_for_every_entry_and_authors_no_failure() {
    for (row_at, row) in rows().iter().enumerate() {
        let case = row.case;
        let store = ObjectStore::start();
        let bucket = format!("batch-lambda-invoke-{row_at}");
        store.create_bucket(case, REGION, &bucket).await;
        let mut manifest = String::new();
        for key in row.listed {
            store
                .put_object(case, REGION, &bucket, key, b"the object's bytes")
                .await;
            manifest.push_str(&format!("{bucket},{key}\n"));
        }
        let e_tag = store
            .put_object(
                case,
                REGION,
                &bucket,
                "manifest.csv",
                manifest.as_bytes(),
            )
            .await;

        let submitted = store
            .s3control(
                REGION,
                "POST",
                "/v20180820/jobs",
                &format!(
                    "<CreateJobRequest xmlns=\"{XMLNS}\">\
                     <ClientRequestToken>{}</ClientRequestToken>\
                     <ConfirmationRequired>false</ConfirmationRequired>\
                     <Operation><LambdaInvoke>{}</LambdaInvoke></Operation>\
                     <Report><Bucket>arn:aws:s3:::{bucket}</Bucket>\
                     <Format>Report_CSV_20180820</Format><Enabled>true</Enabled>\
                     <Prefix>reports</Prefix><ReportScope>AllTasks</ReportScope></Report>\
                     {}<Priority>1</Priority><RoleArn>{ROLE_ARN}</RoleArn>\
                     </CreateJobRequest>",
                    xml_escape(&format!("lambda-invoke-{row_at}")),
                    row.invoke,
                    csv_manifest(&bucket, "manifest.csv", &e_tag)
                ),
            )
            .await;
        assert_eq!(
            submitted.status, 200,
            "case {case:?}: expected CreateJob to answer 200, got {} with body {}",
            submitted.status, submitted.body
        );
        let job_id = submitted
            .text("JobId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateJob answers a JobId: {}", submitted.body));

        let settled = poll_terminal(&store, case, &job_id).await;
        assert_eq!(
            settled.text("Status").as_deref(),
            Some(JOB_STATUS_COMPLETE),
            "case {case:?}: a job whose invocations ran is Complete however many tasks \
             failed, not {JOB_STATUS_FAILED}, got body {}",
            settled.body
        );
        assert_eq!(
            settled.text("FailureReason"),
            None,
            "case {case:?}: a job that completed carries no FailureReasons, got body {}",
            settled.body
        );
        assert_eq!(
            settled.text("FailureCode"),
            None,
            "case {case:?}: a job that completed carries no FailureReasons, got body {}",
            settled.body
        );

        let total = i64::try_from(row.listed.len()).expect("the row count fits");
        assert_eq!(
            progress(case, &settled),
            (total, 0, total),
            "case {case:?}: expected one failed task per manifest entry, got body {}",
            settled.body
        );

        // The completion report accounts for each task under the code the
        // invocation was refused with, and names no task as carried out.
        let results = store
            .keys_under(REGION, &bucket, "reports")
            .await
            .into_iter()
            .find(|key| key.ends_with("report.csv"))
            .unwrap_or_else(|| panic!("case {case:?}: the job writes its completion report"));
        let (status, bytes) = store.object(REGION, &bucket, &results).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the report at {results} to be readable, got {status}"
        );
        let report = String::from_utf8_lossy(&bytes).into_owned();
        let rows_written = report.lines().filter(|line| !line.is_empty()).count();
        assert_eq!(
            rows_written,
            row.listed.len(),
            "case {case:?}: expected one report row per manifest entry, got {report}"
        );
        for key in row.listed {
            assert!(
                report.contains(key),
                "case {case:?}: expected the report to account for {key}, got {report}"
            );
        }
        assert_eq!(
            report.matches(TASK_FAILED).count(),
            row.listed.len(),
            "case {case:?}: expected every report row to record a failed task, got {report}"
        );
    }
}
