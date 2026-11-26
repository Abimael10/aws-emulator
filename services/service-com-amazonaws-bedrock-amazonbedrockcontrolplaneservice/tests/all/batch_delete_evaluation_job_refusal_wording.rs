// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusal `BatchDeleteEvaluationJob` answers a batch whose jobs are
//! not all deletable with, end to end over the same `MultiServiceRouter` the emulator boots
//! (`crate::support`).
//!
//! The rule under test: **a modelled error's message is the fixed text the service words
//! that error class with, and a refusal the service states about the REQUEST carries no
//! value of the request or of the service's state inside it.** Amazon Bedrock refuses this
//! batch with one request-level statement — "All provided evaluation jobs in the request are
//! not supported by batch deletion. Please re-submit with evaluation jobs that exists and
//! under FAILED COMPLETED STOPPED status." — which names neither the job the batch
//! addressed nor the status that job holds, so a client that keys on the refusal reads the
//! same bytes whatever it submitted.
//!
//! The rows differ along everything the refusal could have interpolated and does not: the
//! non-terminal status the addressed job holds (every one this emulator can reach), how many
//! identifiers the batch names, whether an identifier no job answers to and a deletable job
//! ride along, the position of the offending job in the list, and the region the request
//! addresses (which is what decides the ARNs in play). Every row asserts the SAME bytes, and
//! that none of the values of its own request appears in them.
//!
//! The sibling suite `batch_delete_evaluation_jobs` holds the rest of the operation's
//! contract; this one is about the refusal's text alone.

use serde_json::{json, Value};

use crate::support::{
    age_evaluation_job, emulator, job_id_of as job_id, label, Emulator, Reply, ACCOUNT, REGION,
};

/// The message Amazon Bedrock refuses a batch whose jobs are not all deletable with: fixed
/// text end to end, as the behavioral corpus `simparity-behavioral-ga-prod-v1` records it
/// for a batch delete naming a job that is still running. Written here as the literal the
/// wire must carry, so this suite fails if the handler words the refusal its own way.
const NOT_DELETABLE_MESSAGE: &str =
    "All provided evaluation jobs in the request are not supported by batch deletion. \
     Please re-submit with evaluation jobs that exists and under FAILED COMPLETED STOPPED \
     status.";

/// A `RoleArn` the model admits (pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/bedrock-evaluation";

/// `EvaluationJobStatus` is `[InProgress, Completed, Failed, Stopping, Stopped, Deleting]`
/// (`smithy_query(query="shape",shape="EvaluationJobStatus")`); these three are the
/// non-terminal ones a batch delete is refused for.
const IN_PROGRESS: &str = "InProgress";
const STOPPING: &str = "Stopping";
const DELETING: &str = "Deleting";
const STOPPED: &str = "Stopped";

/// How long after a stop the job's move to [`STOPPED`] is owed, as the handler holds it.
const STOP_SPAN: std::time::Duration =
    service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::EVALUATION_JOB_STOPPED_AFTER;

/// How much past a span a record is aged, so a row that needs a move the clock owes reads it
/// strictly after the instant it was owed at — no wall-clock wait, so the row's outcome does
/// not depend on how fast the host runs.
const PAST_THE_DUE_INSTANT: std::time::Duration = std::time::Duration::from_millis(150);

/// A second region, so a row's ARNs — and the region inside them — differ from the first's.
const OTHER_REGION: &str = "eu-central-1";

/// A well-formed `EvaluationJobIdentifier` no stored job answers to: an ARN of this
/// service's namespace naming a job id nothing minted.
fn unknown_arn(region: &str) -> String {
    format!("arn:aws:bedrock:{region}:{ACCOUNT}:evaluation-job/000000nojob0")
}

/// A `CreateEvaluationJob` body carrying the four `@required` members
/// (`smithy_query(query="operation",operation="CreateEvaluationJob")`).
fn create_body(job_name: &str) -> Value {
    json!({
        "jobName": job_name,
        "roleArn": ROLE_ARN,
        "outputDataConfig": { "s3Uri": "s3://evaluation-results/refusal-wording/" },
        "evaluationConfig": {
            "automated": {
                "datasetMetricConfigs": [{
                    "taskType": "QuestionAndAnswer",
                    "dataset": { "name": "Builtin.BoolQ" },
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

/// Create an evaluation job in `region` and hand back the `jobArn` the create minted.
async fn given_job(emu: &Emulator, case: &str, region: &str, job_name: &str) -> String {
    let created = emu
        .call_in_region(region, "POST", "/evaluation-jobs", Some(create_body(job_name)))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: the create that seeds this row must be accepted, got {} {:?}",
        created.status, created.body
    );
    created.text("jobArn")
}

async fn get_job(emu: &Emulator, region: &str, identifier: &str) -> Reply {
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
async fn stop_job(emu: &Emulator, region: &str, identifier: &str) -> Reply {
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
async fn batch_delete(emu: &Emulator, region: &str, identifiers: &[String]) -> Reply {
    emu.call_in_region(
        region,
        "POST",
        "/evaluation-jobs/batch-delete",
        Some(json!({ "jobIdentifiers": identifiers })),
    )
    .await
}

/// Walk a job to the terminal `Stopped` status: a caller stops it and the clock owes the
/// move, which is reached by AGEING the stored record past the instant it was owed at rather
/// than by waiting for it.
async fn given_stopped_job(emu: &Emulator, case: &str, region: &str, job_name: &str) -> String {
    let arn = given_job(emu, case, region, job_name).await;
    let stopped = stop_job(emu, region, &arn).await;
    assert_eq!(
        stopped.status, 200,
        "case {case:?}: the stop that seeds this row must be accepted, got {} {:?}",
        stopped.status, stopped.body
    );
    age_evaluation_job(emu, &arn, STOP_SPAN + PAST_THE_DUE_INSTANT);
    let read = get_job(emu, region, &arn).await;
    assert_eq!(
        read.get("status"),
        json!(STOPPED),
        "case {case:?}: the seeded job must be {STOPPED} once the stop span has passed, got {:?}",
        read.get("status")
    );
    arn
}

/// How a row's offending job is walked to the non-terminal status it is refused for.
#[derive(Debug, Clone, Copy)]
enum Walk {
    /// Created and left alone: the job is running.
    Created,
    /// Stopped, and read before the stop span has passed: the job is winding down.
    Stopping,
    /// Stopped, settled and then deleted: the job is already being deleted.
    Deleting,
}

/// What rides in the batch beside the offending job.
#[derive(Debug, Clone, Copy)]
enum Batch {
    /// The offending job alone, which is the shape the corpus records.
    Alone,
    /// A deletable job FIRST and the offending one after it, so the offending identifier is
    /// not the first the request names.
    AfterADeletableJob,
    /// A deletable job, an identifier no job answers to, and the offending one.
    WithAnUnknownIdentifier,
}

/// One row: the status the offending job holds, what rides with it, and the region the whole
/// request addresses.
struct Row {
    case: &'static str,
    /// The `jobName` this row's jobs are created under, within `JobName`'s own constraint
    /// (`[a-z0-9](-*[a-z0-9]){0,62}`, at most 63 characters).
    name: &'static str,
    walk: Walk,
    status: &'static str,
    batch: Batch,
    region: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a running job, named on its own",
            name: "running-alone",
            walk: Walk::Created,
            status: IN_PROGRESS,
            batch: Batch::Alone,
            region: REGION,
        },
        Row {
            case: "a winding-down job, behind a deletable one",
            name: "stopping-second",
            walk: Walk::Stopping,
            status: STOPPING,
            batch: Batch::AfterADeletableJob,
            region: REGION,
        },
        Row {
            case: "a job already being deleted, with an unknown identifier in the batch",
            name: "deleting-with-unknown",
            walk: Walk::Deleting,
            status: DELETING,
            batch: Batch::WithAnUnknownIdentifier,
            region: REGION,
        },
        Row {
            case: "a running job of another region, with an unknown identifier in the batch",
            name: "running-other-region",
            walk: Walk::Created,
            status: IN_PROGRESS,
            batch: Batch::WithAnUnknownIdentifier,
            region: OTHER_REGION,
        },
        Row {
            case: "a winding-down job of another region, named on its own",
            name: "stopping-other-region",
            walk: Walk::Stopping,
            status: STOPPING,
            batch: Batch::Alone,
            region: OTHER_REGION,
        },
    ]
}

#[tokio::test]
async fn the_refusal_of_a_non_deletable_batch_is_one_fixed_request_level_sentence_pair() {
    let emu = emulator();

    for Row {
        case,
        name: job_name,
        walk,
        status,
        batch,
        region,
    } in rows()
    {
        // The deletable job is seeded FIRST: walking one to a terminal status ages its own
        // record, which a job this row must read in a non-terminal status cannot sit
        // through.
        let deletable =
            given_stopped_job(&emu, case, region, &format!("bystander-{job_name}")).await;
        let offending = match walk {
            Walk::Created => given_job(&emu, case, region, job_name).await,
            Walk::Stopping => {
                let arn = given_job(&emu, case, region, job_name).await;
                let stopped = stop_job(&emu, region, &arn).await;
                assert_eq!(
                    stopped.status, 200,
                    "case {case:?}: the stop that seeds this row must be accepted, got {} {:?}",
                    stopped.status, stopped.body
                );
                arn
            }
            Walk::Deleting => {
                let arn = given_stopped_job(&emu, case, region, job_name).await;
                let accepted = batch_delete(&emu, region, std::slice::from_ref(&arn)).await;
                assert_eq!(
                    accepted.status, 202,
                    "case {case:?}: the delete that seeds this row must be accepted, got {} {:?}",
                    accepted.status, accepted.body
                );
                arn
            }
        };

        let identifiers = match batch {
            Batch::Alone => vec![offending.clone()],
            Batch::AfterADeletableJob => vec![deletable.clone(), offending.clone()],
            Batch::WithAnUnknownIdentifier => vec![
                deletable.clone(),
                unknown_arn(region),
                offending.clone(),
            ],
        };
        let refused = batch_delete(&emu, region, &identifiers).await;

        assert_eq!(
            refused.status, 400,
            "case {case:?}: ValidationException is modelled @httpError(400), got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: a batch whose jobs are not all deletable is refused \
             ValidationException, got {:?} {:?}",
            refused.error,
            refused.body
        );
        let message = refused.text("message");
        assert_eq!(
            message, NOT_DELETABLE_MESSAGE,
            "case {case:?}: expected the fixed request-level refusal \
             {NOT_DELETABLE_MESSAGE:?}, got {message:?}"
        );

        // And the refusal interpolated NOTHING: none of the values this request or the
        // store could have put in it appears in the message.
        for (what, value) in [
            ("the offending job's ARN", offending.clone()),
            ("the offending job's id", job_id(&offending)),
            ("the status that job holds", status.to_string()),
            ("the deletable job's ARN", deletable.clone()),
            ("the unknown identifier", unknown_arn(region)),
            ("the region the request addresses", region.to_string()),
            ("the account the jobs belong to", ACCOUNT.to_string()),
            ("the number of identifiers named", identifiers.len().to_string()),
        ] {
            assert!(
                !message.contains(&value),
                "case {case:?}: the refusal must not carry {what} ({value:?}), got {message:?}"
            );
        }

        // The refusal deleted none of the jobs the batch named: the deletable one is still
        // there, in the status it held, and is accepted once it is named on its own.
        let untouched = get_job(&emu, region, &deletable).await;
        assert_eq!(
            untouched.get("status"),
            json!(STOPPED),
            "case {case:?}: a refused batch leaves its deletable job {STOPPED}, got {:?}",
            untouched.body
        );
        let alone = batch_delete(&emu, region, std::slice::from_ref(&deletable)).await;
        assert_eq!(
            alone.status, 202,
            "case {case:?}: the deletable job alone is accepted, got {} {:?}",
            alone.status, alone.body
        );
    }
}
