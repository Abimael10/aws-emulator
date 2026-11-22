// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT a `Glue Job State Change` event reports about a run that came to
//! rest having been REFUSED — by the admission of the job's role, or by another service's
//! answer to the script's own AWS call.
//!
//! The rule under test: a job run that reaches a terminal `JobRunState` is published once onto
//! the account's default EventBridge bus as `source` `aws.glue` / `detail-type` `Glue Job State
//! Change`, whose `detail` reports that run's own `jobName`, `jobRunId`, the `state` it came to
//! rest in, how severe that state is for the job, and — where the run carries one — its
//! `JobRun$ErrorMessage` VERBATIM, whatever decided the state; a rule narrowed on
//! `detail.jobName` delivers only the events of the job it names. The two reasons a refused run
//! carries are themselves the rule's subject:
//!
//! * a run whose role Glue may not ASSUME — the role's `AssumeRolePolicyDocument` does not
//!   admit the Glue service principal — is refused before anything of the job is fetched or
//!   executed, and reports the job executor's own failure line for it;
//! * a run whose SCRIPT was answered a refusal by another service reports that refusal as the
//!   script's own SDK raised it: the Python exception class the SDK raises for a service's
//!   refusal, then the sentence that SDK renders — the code, the operation and the service's
//!   message — in which the principal blamed is the SESSION Glue opens of the job's role for a
//!   job runner, under one fixed session name whatever the job is called.
//!
//! Neither refused run writes what its script would have written.
//!
//! What this suite pins beside the other job-run event suites — [`crate::job_run_state_change_events`]
//! (the terminal states of runs whose roles no IAM record backs) and
//! [`crate::job_run_event_target_permission`] (which target a delivery reaches) — is the
//! JUNCTION of the publication with the two admission paths: the reason a refused run reports
//! is what the event hands a client that reacts to the run through EventBridge and never calls
//! Glue again.
//!
//! The rows differ along what the rule generalises over: what the role's trust document says
//! (another service, Glue explicitly denied, Glue allowed), what the policies in force on it
//! grant (the script read, the output write, the write at another prefix only), which key the
//! script's refused call addressed, and the terminal state and severity each one produces.
//!
//! Glue, IAM, Amazon S3, EventBridge and SQS are mounted on ONE engine, so every answer is the
//! decision those services really took: the IAM records are the ones `CreateRole` and
//! `PutRolePolicy` write, the refusal is Amazon S3's own about the run's session, and the
//! delivery travels the real bus to a real queue. The due instant a run's terminal state falls
//! at is moved THROUGH THE STORE rather than slept for, each run is read back until it HAS come
//! to rest ([`crate::job_runs::run_at_rest`]), and each delivery is polled for until it is
//! there against a generous deadline, so nothing here depends on how fast the host runs.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    GLUE_SERVICE_PRINCIPAL, JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role, trust};
use crate::job_run_execution_role::{
    every_object, grant_role, statement, JOB_EXECUTOR, JOB_EXECUTOR_CODE, JOB_EXECUTOR_STATUS,
};
use crate::job_run_state_change_events::{
    delivered_event, drain, emulator_with_a_bus, given_queue, given_rule, grant_delivery,
};
use crate::job_script_execution::{create_bucket, object_body, stage_script, BUCKET, SCRIPTS};
use crate::job_runs::run_at_rest;
use crate::wire::{ok, ACCOUNT_ID};

/// The `source` and `detail-type` Glue publishes a job run's terminal state under, and the two
/// severities such an event carries — spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a test pass.
const EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";
const SEVERITY_INFO: &str = "INFO";
const SEVERITY_ERROR: &str = "ERROR";

/// The role-session name Glue opens a job run's session under — ONE name, whatever the job is
/// called — written out here rather than read from the crate's own constant so this suite pins
/// it.
const RUNNER_SESSION: &str = "GlueJobRunnerSession";

/// The script every row runs: it writes `ok` at the key its `--output_key` argument names,
/// which is the call the row's role either may or may not make.
const WRITES_ITS_OUTPUT: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"], Key=named["--output_key"], Body=b"ok"
)
"#;

/// The prefix a row's role may write under, for the row that is granted the write SOMEWHERE
/// but not where its script writes.
const PERMITTED_PREFIX: &str = "results/good/";

/// An emulator with Glue, Amazon S3, EventBridge, SQS AND IAM on one shared engine, plus that
/// engine.
///
/// The bus mounting is the one the other job-run event suites stand up
/// ([`emulator_with_a_bus`]); IAM is mounted beside it because the admission of each run is
/// decided against this account's own role records.
fn emulator_with_a_bus_and_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = emulator_with_a_bus();
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// What the run of a row's job must report as its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The script ran to its end: the run carries no reason at all.
    None,
    /// Glue could not assume the job's role, so nothing of the job ran.
    RoleNotAssumable,
    /// The script's own output write was refused by the object store.
    OutputWriteRefused,
}

/// One row: the role a job runs as, where its script writes, and what the run — and the event
/// that reports it — must come to.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The role's `AssumeRolePolicyDocument`.
    trust_document: String,
    /// The identity policy documents in force on the role.
    grants: Vec<Value>,
    /// The key the row's script writes, under this suite's bucket.
    output_key: &'static str,
    /// The `JobRunState` the run must come to rest in, which is the `state` the event carries.
    rests_in: &'static str,
    /// The `severity` the event must carry.
    severity: &'static str,
    /// The reason the run — and so the event — must carry.
    reason: Reason,
}

/// The rows: every form of trust document and of grant, against what the run reports and what
/// its event therefore says.
fn rows() -> Vec<Row> {
    let reads_the_script = statement("Allow", &["s3:GetObject"], &every_object());
    let reads_and_writes = statement("Allow", &["s3:GetObject", "s3:PutObject"], &every_object());
    vec![
        Row {
            case: "a role Glue can assume, allowed to read the script and write its output",
            trust_document: glue_trust(),
            grants: vec![reads_and_writes],
            output_key: "results/good/ran.txt",
            rests_in: JOB_RUN_STATE_SUCCEEDED,
            severity: SEVERITY_INFO,
            reason: Reason::None,
        },
        Row {
            case: "a role whose trust document names another service",
            trust_document: trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole"),
            grants: vec![statement("Allow", &["s3:*"], "*")],
            output_key: "results/good/untrusted.txt",
            rests_in: JOB_RUN_STATE_FAILED,
            severity: SEVERITY_ERROR,
            reason: Reason::RoleNotAssumable,
        },
        Row {
            case: "a role whose trust document denies Glue the assume",
            trust_document: trust("Deny", GLUE_SERVICE_PRINCIPAL, "sts:AssumeRole"),
            grants: vec![statement("Allow", &["s3:*"], "*")],
            output_key: "results/good/denied-trust.txt",
            rests_in: JOB_RUN_STATE_FAILED,
            severity: SEVERITY_ERROR,
            reason: Reason::RoleNotAssumable,
        },
        Row {
            case: "a role allowed to read the script but not to write anything",
            trust_document: glue_trust(),
            grants: vec![reads_the_script.clone()],
            output_key: "results/bad/output.txt",
            rests_in: JOB_RUN_STATE_FAILED,
            severity: SEVERITY_ERROR,
            reason: Reason::OutputWriteRefused,
        },
        Row {
            // The write is granted SOMEWHERE, so what the refusal names is the key the script
            // really addressed and not the only key the role was ever refused.
            case: "a role allowed to write another prefix than the one its script writes",
            trust_document: glue_trust(),
            grants: vec![
                reads_the_script,
                statement(
                    "Allow",
                    &["s3:PutObject"],
                    &format!("arn:aws:s3:::{BUCKET}/{PERMITTED_PREFIX}*"),
                ),
            ],
            output_key: "results/elsewhere/output.bin",
            rests_in: JOB_RUN_STATE_FAILED,
            severity: SEVERITY_ERROR,
            reason: Reason::OutputWriteRefused,
        },
    ]
}

/// The whole `ErrorMessage` a run whose output write the object store refused reports: the
/// Python exception class that store's refusal is raised as inside the script, then the sentence
/// the SDK renders it with — the code, the operation and the store's own message, in which the
/// principal is the session Glue opened of `role_name` for the job runner.
fn refused_output_write(role_name: &str, key: &str) -> String {
    format!(
        "ClientError: An error occurred (AccessDenied) when calling the PutObject operation: \
         User: arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{RUNNER_SESSION} is not \
         authorized to perform: s3:PutObject on resource: \"arn:aws:s3:::{BUCKET}/{key}\" \
         because no identity-based policy allows the s3:PutObject action"
    )
}

/// The `ErrorMessage` a run Glue could not assume the role of reports, as the job executor
/// words it: the run it is about, the assume-role requirement for that role, and — stripped and
/// asserted by shape, because its request id is its one per-run field — the clause naming the
/// component that raised it.
fn assert_role_not_assumable(
    case: &str,
    message: &str,
    job_name: &str,
    run_id: &str,
    role: &str,
) {
    let sentence = crate::error_classification::without_diagnostic(
        case,
        message,
        JOB_EXECUTOR,
        JOB_EXECUTOR_STATUS,
        JOB_EXECUTOR_CODE,
    );
    let expected = format!(
        "AccountId:{ACCOUNT_ID} and JobName:{job_name} and JobRunId:{run_id} failed to execute \
         with exception Role {role} should be given assume role permissions for Glue Service.\n"
    );
    assert_eq!(
        sentence, expected,
        "case {case:?}: expected ErrorMessage {expected:?}, got {message:?}"
    );
}

/// A run that comes to rest having been refused — by its role's trust policy, or by the object
/// store's answer to its script's own call — publishes a `Glue Job State Change` carrying that
/// run's terminal state, its severity and its own reason verbatim; a rule narrowed on
/// `detail.jobName` delivers only that job's event; and neither refused run writes anything.
#[tokio::test]
async fn a_refused_job_runs_event_carries_the_reason_the_run_reports() {
    let (router, engine) = emulator_with_a_bus_and_iam();
    create_bucket(&router).await;

    // Every queue and every rule exists BEFORE any run starts: a run's event is published when
    // it comes to rest, so a rule put afterwards would have nothing to match.
    let table = rows();
    let mut queues: Vec<(String, String)> = Vec::with_capacity(table.len());
    for (index, row) in table.iter().enumerate() {
        let job_name = format!("job_run_reason_{index}");
        let (url, arn) = given_queue(&router, &format!("glue-job-reason-{index}")).await;
        grant_delivery(&router, &url, &arn).await;
        given_rule(
            &router,
            &format!("glue-job-reason-{index}"),
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [JOB_STATE_CHANGE],
                "detail": { "jobName": [job_name] },
            }),
            std::slice::from_ref(&arn),
        )
        .await;
        queues.push((url, arn));
        assert_eq!(
            queues.len(),
            index + 1,
            "case {:?}: every row must have its own target queue",
            row.case
        );
    }
    // The sibling rule narrows on a job name NO job here carries, so its queue is what shows
    // that the narrowing on `detail.jobName` is read at all.
    let (absent_url, absent_arn) = given_queue(&router, "glue-job-reason-absent").await;
    grant_delivery(&router, &absent_url, &absent_arn).await;
    given_rule(
        &router,
        "glue-job-reason-absent",
        json!({
            "source": [EVENT_SOURCE],
            "detail-type": [JOB_STATE_CHANGE],
            "detail": { "jobName": ["job_run_reason_no_such_job"] },
        }),
        std::slice::from_ref(&absent_arn),
    )
    .await;

    for (index, row) in table.into_iter().enumerate() {
        let case = row.case;
        let job_name = format!("job_run_reason_{index}");
        let role_name = format!("glue-job-reason-role-{index}");
        let role = seed_role(&engine, &role_name, Some(row.trust_document.clone()));
        grant_role(&engine, &role_name, &row.grants, &[]);

        let location = stage_script(
            &router,
            &format!("{SCRIPTS}{job_name}.py"),
            WRITES_ITS_OUTPUT,
        )
        .await;
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": job_name,
                "Role": role,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": location,
                },
                "DefaultArguments": { "--bucket": BUCKET, "--output_key": row.output_key },
                "MaxCapacity": 0.0625,
            }),
        )
        .await;

        let started = ok(&router, "StartJobRun", &json!({ "JobName": job_name })).await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
            .to_string();
        let run = run_at_rest(&router, &engine, case, &job_name, &run_id).await;
        assert_eq!(
            run["JobRunState"].as_str().unwrap_or_default(),
            row.rests_in,
            "case {case:?}: expected the run to come to rest in {:?}, got {run}",
            row.rests_in
        );
        let message = run
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let written = object_body(&router, row.output_key).await;

        match row.reason {
            Reason::None => {
                assert!(
                    run.get("ErrorMessage").is_none(),
                    "case {case:?}: a run that failed at nothing carries no ErrorMessage, got \
                     {run}"
                );
                assert_eq!(
                    written.as_deref(),
                    Some("ok"),
                    "case {case:?}: expected the script's own object, got {written:?}"
                );
            }
            Reason::RoleNotAssumable => {
                assert_role_not_assumable(case, &message, &job_name, &run_id, &role);
                assert!(
                    written.is_none(),
                    "case {case:?}: nothing of the job must have run, but its object is \
                     {written:?}"
                );
            }
            Reason::OutputWriteRefused => {
                let expected = refused_output_write(&role_name, row.output_key);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected ErrorMessage {expected:?}, got {message:?}"
                );
                assert!(
                    written.is_none(),
                    "case {case:?}: the refused write must leave no object, got {written:?}"
                );
            }
        }

        // The event is the service's own clock pump's work, so it is waited for rather than
        // expected at an instant.
        let (url, _) = &queues[index];
        let event = delivered_event(&router, case, url).await;
        assert_eq!(
            event["source"],
            json!(EVENT_SOURCE),
            "case {case:?}: expected source {EVENT_SOURCE:?}, got {event}"
        );
        assert_eq!(
            event["detail-type"],
            json!(JOB_STATE_CHANGE),
            "case {case:?}: expected detail-type {JOB_STATE_CHANGE:?}, got {event}"
        );
        let detail = &event["detail"];
        assert_eq!(
            detail["jobName"],
            json!(job_name),
            "case {case:?}: the detail must name the job, got {detail}"
        );
        assert_eq!(
            detail["jobRunId"],
            json!(run_id),
            "case {case:?}: the detail must name the run, got {detail}"
        );
        assert_eq!(
            detail["state"],
            json!(row.rests_in),
            "case {case:?}: expected state {:?}, got {detail}",
            row.rests_in
        );
        assert_eq!(
            detail["severity"],
            json!(row.severity),
            "case {case:?}: expected severity {:?}, got {detail}",
            row.severity
        );
        // The reason the event reports is the run's OWN: the same text `GetJobRun` answers
        // with, whichever refusal produced it, and none at all for a run that failed at
        // nothing.
        assert_eq!(
            detail.get("message").and_then(Value::as_str),
            run.get("ErrorMessage").and_then(Value::as_str),
            "case {case:?}: the event's message must be the run's own, got {detail} for {run}"
        );
        assert_eq!(
            detail.get("message").is_some(),
            row.reason != Reason::None,
            "case {case:?}: a run that carries a reason publishes it, got {detail}"
        );
    }

    // The narrowing on `detail.jobName` is read: the rule naming a job nothing here carries
    // received none of these events, and no row's rule received another row's event — each
    // queue held exactly the one its own row drained.
    let absent = drain(&router, &absent_url).await;
    assert!(
        absent.is_empty(),
        "a rule narrowed to a job name no job carries receives nothing, got {absent:?}"
    );
    for (index, (url, _)) in queues.iter().enumerate() {
        let left = drain(&router, url).await;
        assert!(
            left.is_empty(),
            "the queue of job_run_reason_{index} must hold only its own job's event, got {left:?}"
        );
    }
}
