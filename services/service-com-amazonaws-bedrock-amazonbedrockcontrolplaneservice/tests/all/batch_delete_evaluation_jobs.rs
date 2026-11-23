// SPDX-License-Identifier: CC0-1.0

//! `BatchDeleteEvaluationJob` end-to-end, over the same `MultiServiceRouter` the emulator
//! boots (`crate::support`): the delete is driven through HTTP and every claim about it is
//! read back through `GetEvaluationJob`.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one recorded
//! request:
//!
//! 1. **A job in a terminal status is accepted for deletion**: the call answers the
//!    modelled 202, reports every job it accepted under `evaluationJobs` with the
//!    `Deleting` status, reports nothing under `errors`, and the stored job is `Deleting`
//!    from that call onwards — over batches differing in size (one, three, the same ARN
//!    twice) and in the region they address. Each accepted job is reported by its BARE job
//!    id — the resource-id segment of its ARN, the `[a-z0-9]{12}` id the ARN the request
//!    named ends in — so a caller can correlate the item with the job it submitted and
//!    rebuild that job's ARN from it, while the sibling `errors[].jobIdentifier` carries
//!    the ARN, as its own member documents ("The ARN of the evaluation job being deleted").
//! 2. **The `Deleting` status is owed the job's REMOVAL by the clock**: once
//!    `EVALUATION_JOB_DELETED_AFTER` has passed the job is gone, so a read of its ARN is
//!    refused with 404 `ResourceNotFoundException` and its `jobName` is free for a new job.
//! 3. **A job that is not in a terminal status cannot be deleted**: "An evaluation job can
//!    only be deleted if it has following status FAILED, COMPLETED, and STOPPED"
//!    (`smithy_query(query="operation",operation="BatchDeleteEvaluationJob")`), so a batch
//!    naming a running, a winding-down or an already-deleting job is refused with 400
//!    `ValidationException` (the refusal the corpus records for a batch delete of a job
//!    that is still in progress) and deletes NONE of the jobs the batch names.
//! 4. **An identifier no stored job answers to is reported per job**: it deletes nothing,
//!    so the call still answers 202 and carries that ARN under `errors` with the HTTP
//!    status code a read of the same identifier answers, while the deletable jobs in the
//!    same batch are accepted.

use serde_json::{json, Value};

use crate::support::{
    age_evaluation_job, emulator, job_id_of as job_id, label, Emulator, Reply, ACCOUNT, REGION,
};

// ---------------------------------------------------------------------------------------
// Values the model admits, and the spans the handlers hold.
// ---------------------------------------------------------------------------------------

/// A `RoleArn` the model admits (pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/bedrock-evaluation";

/// `EvaluationJobStatus` is `[InProgress, Completed, Failed, Stopping, Stopped, Deleting]`
/// (`smithy_query(query="shape",shape="EvaluationJobStatus")`).
const IN_PROGRESS: &str = "InProgress";
const STOPPING: &str = "Stopping";
const STOPPED: &str = "Stopped";
const DELETING: &str = "Deleting";

/// The statuses a job may be deleted in, as the handler holds the doc sentence that fixes
/// them.
const DELETABLE: [&str; 3] =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_DELETABLE_STATUSES;

/// How long after a stop the job's move to [`STOPPED`] is owed, as the handler holds it.
const STOP_SPAN: std::time::Duration =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_STOPPED_AFTER;

/// How long after a batch delete the accepted job's removal is owed, as the handler holds
/// it.
const DELETE_SPAN: std::time::Duration =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_DELETED_AFTER;

/// How much past a span a record is aged, so a row that asserts what the clock owes reads
/// it strictly after the instant it was owed at.
const PAST_THE_DUE_INSTANT: std::time::Duration = std::time::Duration::from_millis(150);

// ---------------------------------------------------------------------------------------
// Request helpers.
// ---------------------------------------------------------------------------------------

/// A `CreateEvaluationJob` body carrying the four `@required` members
/// (`smithy_query(query="operation",operation="CreateEvaluationJob")`).
fn create_body(job_name: &str) -> Value {
    json!({
        "jobName": job_name,
        "roleArn": ROLE_ARN,
        "outputDataConfig": { "s3Uri": "s3://evaluation-results/batch-delete/" },
        "evaluationConfig": {
            "automated": {
                "datasetMetricConfigs": [{
                    "taskType": "Summarization",
                    "dataset": { "name": "Builtin.Gigaword" },
                    "metricNames": ["Builtin.Accuracy"],
                }],
            },
        },
        "inferenceConfig": {
            "models": [{
                "bedrockModel": {
                    "modelIdentifier": "amazon.nova-lite-v1:0",
                    "inferenceParams": "{\"temperature\":0.2}",
                },
            }],
        },
    })
}

async fn create_in_region(emu: &Emulator, region: &str, job_name: &str) -> Reply {
    emu.call_in_region(region, "POST", "/evaluation-jobs", Some(create_body(job_name)))
        .await
}

/// Create an evaluation job in `region` and hand back the `jobArn` the create minted.
async fn given_job(emu: &Emulator, region: &str, job_name: &str) -> String {
    let created = create_in_region(emu, region, job_name).await;
    assert_eq!(
        created.status, 202,
        "case {job_name:?}: the create that seeds this row must be accepted, got {} {:?}",
        created.status, created.body
    );
    created.text("jobArn")
}

async fn get_in_region(emu: &Emulator, region: &str, identifier: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/evaluation-jobs/{}", label(identifier)),
        None,
    )
    .await
}

/// `StopEvaluationJob` is bound to `POST /evaluation-job/{jobIdentifier}/stop` and carries
/// no member beside that label, so the call sends no body.
async fn stop_in_region(emu: &Emulator, region: &str, identifier: &str) -> Reply {
    emu.call_in_region(
        region,
        "POST",
        &format!("/evaluation-job/{}/stop", label(identifier)),
        None,
    )
    .await
}

/// `BatchDeleteEvaluationJob` is bound to `POST /evaluation-jobs/batch-delete` and carries
/// its `jobIdentifiers` list in the body
/// (`smithy_query(query="operation",operation="BatchDeleteEvaluationJob")`).
async fn batch_delete_in_region(emu: &Emulator, region: &str, identifiers: &[String]) -> Reply {
    emu.call_in_region(
        region,
        "POST",
        "/evaluation-jobs/batch-delete",
        Some(json!({ "jobIdentifiers": identifiers })),
    )
    .await
}

async fn batch_delete(emu: &Emulator, identifiers: &[String]) -> Reply {
    batch_delete_in_region(emu, REGION, identifiers).await
}

/// Walk a job to the terminal [`STOPPED`] status: a caller stops it and the clock owes the
/// move. The only terminal status this emulator mints — reaching `Completed` or `Failed`
/// would mean the inference and the metric computation ran.
///
/// The move the clock owes is reached by AGEING the stored record past the instant it was
/// owed at ([`age_evaluation_job`]) rather than by waiting for that instant: the job's own
/// timeline is untouched and only how much of it has elapsed changes, so what the read
/// reports does not depend on how fast the host runs.
async fn given_stopped_job(emu: &Emulator, region: &str, job_name: &str) -> String {
    let arn = given_job(emu, region, job_name).await;
    let stopped = stop_in_region(emu, region, &arn).await;
    assert_eq!(
        stopped.status, 200,
        "case {job_name:?}: the stop that seeds this row must be accepted, got {} {:?}",
        stopped.status, stopped.body
    );
    age_evaluation_job(emu, &arn, STOP_SPAN + PAST_THE_DUE_INSTANT);
    let read = get_in_region(emu, region, &arn).await;
    assert_eq!(
        read.get("status"),
        json!(STOPPED),
        "case {job_name:?}: the seeded job must be {STOPPED} once the stop span has passed, \
         got {:?}",
        read.get("status")
    );
    arn
}

// ---------------------------------------------------------------------------------------
// Expectations the model and the doc sentence fix.
// ---------------------------------------------------------------------------------------

/// The refusal a batch naming a job in `status` is answered with: it names the job the
/// delete addressed and the status that job holds, and then the requirement as the model
/// words it ("An evaluation job can only be deleted if it has following status FAILED,
/// COMPLETED, and STOPPED").
fn not_deletable_message(identifier: &str, status: &str) -> String {
    format!(
        "Evaluation job: {identifier} is in status {status} and cannot be deleted. An \
         evaluation job can only be deleted if it has following status FAILED, COMPLETED, \
         and STOPPED."
    )
}

/// The `evaluationJobs` list of a reply, as `(jobIdentifier, jobStatus)` pairs in the order
/// the response carries them.
fn accepted(reply: &Reply) -> Vec<(String, String)> {
    reply
        .get("evaluationJobs")
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|item| {
            (
                item.get("jobIdentifier")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                item.get("jobStatus")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The `errors` list of a reply, as `(jobIdentifier, code)` pairs.
fn reported_errors(reply: &Reply) -> Vec<(String, String)> {
    reply
        .get("errors")
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|item| {
            (
                item.get("jobIdentifier")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                item.get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// 1-2. A terminal job is accepted, reported Deleting, and removed on the clock.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_batch_delete_accepts_every_job_in_a_terminal_status() {
    let emu = emulator();

    // The rows differ along what the rule generalises over: how many jobs one batch names
    // (the modelled `@length(1..25)` admits any of these), whether the same ARN is named
    // twice, and the region the batch addresses.
    let rows: Vec<(&str, &str, usize, bool)> = vec![
        ("one job", REGION, 1, false),
        ("three jobs in one batch", REGION, 3, false),
        ("the same job named twice", REGION, 1, true),
        ("two jobs in another region", "eu-central-1", 2, false),
    ];

    // What the batches deleted, carried past the loop so the removal the clock owes is
    // asserted once the span has passed.
    let mut deleted: Vec<(&str, &str, String)> = Vec::new();

    for (case, region, count, repeat) in rows {
        let mut arns: Vec<String> = Vec::new();
        for index in 0..count {
            let job_name = format!("{}-{index}", case.replace(' ', "-"));
            arns.push(given_stopped_job(&emu, region, &job_name).await);
        }

        let sent: Vec<String> = if repeat {
            vec![arns[0].clone(), arns[0].clone()]
        } else {
            arns.clone()
        };
        let reply = batch_delete_in_region(&emu, region, &sent).await;

        assert_eq!(
            reply.status, 202,
            "case {case:?}: BatchDeleteEvaluationJob is modelled @http(code: 202), got {} {:?}",
            reply.status, reply.body
        );
        // The same ARN twice names one job, so the batch reports it ONCE — by its bare id
        // (rule 1a, asserted on its own below).
        let expected: Vec<(String, String)> = arns
            .iter()
            .map(|arn| (job_id(arn), DELETING.to_string()))
            .collect();
        assert_eq!(
            accepted(&reply),
            expected,
            "case {case:?}: every accepted job is reported with the {DELETING} status, got {:?}",
            reply.body
        );
        assert_eq!(
            reported_errors(&reply),
            Vec::new(),
            "case {case:?}: a batch of deletable jobs reports no per-job error, got {:?}",
            reply.body
        );

        // The response is what the store holds: a read reports the transient status, so the
        // removal span is not zero.
        for arn in &arns {
            let read = get_in_region(&emu, region, arn).await;
            assert_eq!(
                read.get("status"),
                json!(DELETING),
                "case {case:?}: the stored job is {DELETING} from the delete onwards, got {:?}",
                read.body
            );
            deleted.push((case, region, arn.clone()));
        }
    }

    // 2. The removal is owed by the CLOCK: once the span has passed the job is gone, so its
    // ARN names nothing. The record is aged past the instant the removal was owed at rather
    // than waited on, so the row does not depend on how fast the host runs.
    for (_, _, arn) in &deleted {
        age_evaluation_job(&emu, arn, DELETE_SPAN + PAST_THE_DUE_INSTANT);
    }
    for (case, region, arn) in &deleted {
        let read = get_in_region(&emu, region, arn).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a removed job's ARN names nothing, and ResourceNotFoundException \
             is modelled @httpError(404), got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: a removed job's ARN names nothing, got {:?} {:?}",
            read.error,
            read.body
        );
    }

    // And the removal freed the job's NAME: the name of a deleted job mints a new job.
    let reused = create_in_region(&emu, REGION, "one-job-0").await;
    assert_eq!(
        reused.status, 202,
        "a deleted job's name is free for a new job, got {} {:?}",
        reused.status, reused.body
    );
}

// ---------------------------------------------------------------------------------------
// 3. A job that is not in a terminal status cannot be deleted.
// ---------------------------------------------------------------------------------------

/// How a row's job is walked to the status the batch is refused for.
#[derive(Debug, Clone, Copy)]
enum Walk {
    /// Created and left alone: the job is running.
    Created,
    /// Stopped, and read before the stop span has passed: the job is winding down.
    Stopping,
    /// Stopped, settled and then deleted: the job is already being deleted.
    Deleting,
}

#[tokio::test]
async fn a_job_that_is_not_in_a_terminal_status_cannot_be_deleted() {
    let emu = emulator();

    // The rows differ along the NON-terminal status the job holds — every one this emulator
    // can reach, since `Completed` and `Failed` are terminal and deletable.
    let rows: Vec<(&str, Walk, &str)> = vec![
        ("a job that is still running", Walk::Created, IN_PROGRESS),
        ("a job that is winding down", Walk::Stopping, STOPPING),
        ("a job already being deleted", Walk::Deleting, DELETING),
    ];

    for (case, walk, status) in rows {
        let job_name = case.replace(' ', "-");
        // A second, deletable job rides in the same batch, so the refusal is about the
        // STATUS of the job that is not deletable and not about the batch carrying one.
        // Seeded FIRST because walking a job to a terminal status waits out the stop span,
        // which a job this row leaves in a non-terminal status must not sit through.
        let bystander = given_stopped_job(&emu, REGION, &format!("bystander-{job_name}")).await;
        let arn = match walk {
            Walk::Created => given_job(&emu, REGION, &job_name).await,
            Walk::Stopping => {
                let arn = given_job(&emu, REGION, &job_name).await;
                let stopped = stop_in_region(&emu, REGION, &arn).await;
                assert_eq!(
                    stopped.status, 200,
                    "case {case:?}: the stop that seeds this row must be accepted, got {} {:?}",
                    stopped.status, stopped.body
                );
                arn
            }
            Walk::Deleting => {
                let arn = given_stopped_job(&emu, REGION, &job_name).await;
                let first = batch_delete(&emu, std::slice::from_ref(&arn)).await;
                assert_eq!(
                    first.status, 202,
                    "case {case:?}: the delete that seeds this row must be accepted, got {} {:?}",
                    first.status, first.body
                );
                arn
            }
        };

        let refused = batch_delete(&emu, &[arn.clone(), bystander.clone()]).await;

        assert_eq!(
            refused.status, 400,
            "case {case:?}: ValidationException is modelled @httpError(400), got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: a job that is not in a terminal status cannot be deleted, got {:?} \
             {:?}",
            refused.error,
            refused.body
        );
        assert_eq!(
            refused.get("message"),
            json!(not_deletable_message(&arn, status)),
            "case {case:?}: the refusal names the job and the status it holds, got {:?}",
            refused.get("message")
        );
        // And it names every status a deletion DOES admit, so a caller is told what to wait
        // for.
        let message = refused.text("message");
        for deletable in DELETABLE {
            assert!(
                message.contains(&deletable.to_uppercase()),
                "case {case:?}: the refusal names the {deletable} status a deletion admits, \
                 got {message:?}"
            );
        }

        // The refusal deleted NONE of the jobs the batch named: the deletable one is still
        // there, in the status it held.
        let untouched = get_in_region(&emu, REGION, &bystander).await;
        assert_eq!(
            untouched.get("status"),
            json!(STOPPED),
            "case {case:?}: a refused batch leaves its deletable job {STOPPED}, got {:?}",
            untouched.body
        );
        // And once it is named on its own it is accepted, so the refusal was the other
        // job's status alone.
        let alone = batch_delete(&emu, std::slice::from_ref(&bystander)).await;
        assert_eq!(
            alone.status, 202,
            "case {case:?}: the deletable job alone is accepted, got {} {:?}",
            alone.status, alone.body
        );
        assert_eq!(
            accepted(&alone),
            vec![(job_id(&bystander), DELETING.to_string())],
            "case {case:?}: the deletable job alone is reported {DELETING}, got {:?}",
            alone.body
        );
    }
}

// ---------------------------------------------------------------------------------------
// 4. An identifier no stored job answers to is reported per job.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_identifier_no_stored_job_answers_to_is_reported_per_job() {
    let emu = emulator();

    let live = given_stopped_job(&emu, REGION, "deletable-job").await;
    let live_id = job_id(&live);

    // The rows a well-formed `EvaluationJobIdentifier` can name no stored job through: an
    // id that was never minted, the live id under another account, and the live id under
    // another partition.
    let unknown: Vec<(&str, String)> = vec![
        (
            "an id that was never minted",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:evaluation-job/bbbbbbbbbbbb"),
        ),
        (
            "the live id under another account",
            format!("arn:aws:bedrock:{REGION}:999999999999:evaluation-job/{live_id}"),
        ),
        (
            "the live id under another partition",
            format!("arn:aws-us-gov:bedrock:{REGION}:{ACCOUNT}:evaluation-job/{live_id}"),
        ),
    ];

    for (case, identifier) in &unknown {
        let reply = batch_delete(&emu, std::slice::from_ref(identifier)).await;
        assert_eq!(
            reply.status, 202,
            "case {case:?}: an unknown ARN is reported per job, not refused, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reported_errors(&reply),
            vec![(identifier.clone(), "404".to_string())],
            "case {case:?}: the per-job error names the ARN and the status code a read of it \
             answers, got {:?}",
            reply.body
        );
        assert_eq!(
            accepted(&reply),
            Vec::new(),
            "case {case:?}: an unknown ARN deletes nothing, got {:?}",
            reply.body
        );
    }

    // A batch mixing the unknown ARNs with the live one: the live job is accepted and each
    // unknown ARN is reported, so a per-job failure fails only its own job.
    let mut sent: Vec<String> = unknown.iter().map(|(_, arn)| arn.clone()).collect();
    sent.push(live.clone());
    let mixed = batch_delete(&emu, &sent).await;
    assert_eq!(
        mixed.status, 202,
        "case a mixed batch: it is accepted, got {} {:?}",
        mixed.status, mixed.body
    );
    assert_eq!(
        accepted(&mixed),
        vec![(job_id(&live), DELETING.to_string())],
        "case a mixed batch: the live job is accepted for deletion, got {:?}",
        mixed.body
    );
    assert_eq!(
        reported_errors(&mixed),
        unknown
            .iter()
            .map(|(_, arn)| (arn.clone(), "404".to_string()))
            .collect::<Vec<_>>(),
        "case a mixed batch: every unknown ARN is reported under errors, got {:?}",
        mixed.body
    );
}

// ---------------------------------------------------------------------------------------
// 1a. An accepted job is reported by its bare job id.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_accepted_job_is_reported_by_its_bare_job_id() {
    let emu = emulator();

    // The rows differ along what the rule generalises over: how many jobs one batch names,
    // whether the same job is named twice, the region the batch addresses, and whether the
    // batch also names an identifier no stored job answers to.
    let rows: Vec<(&str, &str, usize, bool, bool)> = vec![
        ("one job", REGION, 1, false, false),
        ("three jobs in one batch", REGION, 3, false, false),
        ("the same job named twice", REGION, 1, true, false),
        ("two jobs in another region", "eu-central-1", 2, false, false),
        ("a job beside an unknown ARN", REGION, 1, false, true),
    ];

    // A well-formed `EvaluationJobIdentifier` that names no stored job, so the row that
    // carries it exercises the sibling `errors` member beside the accepted items.
    let unknown = format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:evaluation-job/cccccccccccc");

    for (case, region, count, repeat, with_unknown) in rows {
        let mut arns: Vec<String> = Vec::new();
        for index in 0..count {
            let job_name = format!("bare-id-{}-{index}", case.replace(' ', "-").to_lowercase());
            arns.push(given_stopped_job(&emu, region, &job_name).await);
        }

        let mut sent: Vec<String> = if repeat {
            vec![arns[0].clone(), arns[0].clone()]
        } else {
            arns.clone()
        };
        if with_unknown {
            sent.push(unknown.clone());
        }
        let reply = batch_delete_in_region(&emu, region, &sent).await;

        assert_eq!(
            reply.status, 202,
            "case {case:?}: BatchDeleteEvaluationJob is modelled @http(code: 202), got {} {:?}",
            reply.status, reply.body
        );

        // Every accepted job is reported by the resource-id segment of its ARN, in request
        // order, with the status the delete moved it to.
        let expected: Vec<(String, String)> = arns
            .iter()
            .map(|arn| (job_id(arn), DELETING.to_string()))
            .collect();
        assert_eq!(
            accepted(&reply),
            expected,
            "case {case:?}: every accepted job is reported by its bare job id, got {:?}",
            reply.body
        );

        for (arn, (reported, _)) in arns.iter().zip(accepted(&reply)) {
            // The value is an id, not an identifier echoed back.
            assert_ne!(
                reported, *arn,
                "case {case:?}: the accepted item carries the job's id, not the ARN the \
                 request named, got {reported:?}"
            );
            // `EvaluationJobArn` admits `[a-z0-9]{12}` for that segment, so the id the
            // emulator mints and reports is 12 of those characters.
            assert_eq!(
                reported.len(),
                12,
                "case {case:?}: a job id is the 12-character segment of its ARN, got \
                 {reported:?}"
            );
            assert!(
                reported
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "case {case:?}: a job id is lower-case alphanumeric, got {reported:?}"
            );
            // And the value correlates back to the job: the ARN rebuilt from it is the ARN
            // the request named, and a read of that ARN finds the job the batch accepted.
            let rebuilt = format!("arn:aws:bedrock:{region}:{ACCOUNT}:evaluation-job/{reported}");
            assert_eq!(
                rebuilt, *arn,
                "case {case:?}: an ARN rebuilt from the reported id addresses the same job, \
                 got {rebuilt:?}"
            );
            let read = get_in_region(&emu, region, &rebuilt).await;
            assert_eq!(
                read.get("status"),
                json!(DELETING),
                "case {case:?}: the job the reported id names is the one the batch accepted, \
                 got {:?}",
                read.body
            );
        }

        // The sibling `errors` member documents the ARN ("The ARN of the evaluation job
        // being deleted"), so an identifier no job answers to is reported in full.
        let expected_errors: Vec<(String, String)> = if with_unknown {
            vec![(unknown.clone(), "404".to_string())]
        } else {
            Vec::new()
        };
        assert_eq!(
            reported_errors(&reply),
            expected_errors,
            "case {case:?}: a per-job error names the ARN the request supplied, got {:?}",
            reply.body
        );
    }
}
