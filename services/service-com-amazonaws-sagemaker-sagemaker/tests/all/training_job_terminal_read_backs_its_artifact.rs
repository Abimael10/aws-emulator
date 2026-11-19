// SPDX-License-Identifier: CC0-1.0

//! WHAT a read that reports a stage of a training job's work is BACKED by in Amazon S3: the
//! checkpoints the stage that TRAINS writes, and the model artifact the job's finished run
//! leaves behind.
//!
//! RULE — every object a stage of a job's work leaves behind is readable from the FIRST read
//! that reports that stage: what the clock owes a job is materialised as of the one instant the
//! read reports as of, so the publication owed at a stage and the move into that stage are
//! never observable apart. `CheckpointConfig`'s objects belong to the stage that TRAINS, which
//! is the stage a container writes them during, and the model artifact to the stage that
//! uploads it, which is over once the job reports a terminal status. So a client that polls
//! `SecondaryStatus` to decide when one of those objects exists — a resumable-training chain
//! reading the checkpoint prefix, a deploy chain reading the artifact — never reads the stage
//! with nothing there.
//!
//! [`crate::training_artifact_publication`] weighs the artifact leg of that rule across clients
//! that look at DIFFERENT cadences; the rows here vary the job's own shape instead: the form
//! the checkpoint location is given in, and whether the job's OWN declared program wrote the
//! directory whose files become those objects.
//!
//! The harness, the request bodies and the channel data are the training-run suite's
//! ([`crate::training_job_execution`]) rather than a second copy of them. Every wait is a poll
//! that looks again until the condition it waits for holds, under a deadline far longer than
//! any span this state gives a job's work, so no claim rests on how fast the host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_key, artifact_uri, channel, csv_records, read_and_write, str_field, training_body,
    Emulator, BUCKET,
};

/// How long a job is polled for the stage it has to reach before a case fails: far longer than
/// the span this state gives a job's whole work, so a loaded host still reaches it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's CONDITION decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The file name the default train instructions check a model's state out under, read from the
/// crate's own state rather than restated here.
const MODEL_ENTRY: &str = service_com_amazonaws_sagemaker_sagemaker::state::MODEL_ARCHIVE_ENTRY;

/// The `SecondaryStatus` members that are AT or PAST the stage a container writes its
/// checkpoints during (`DescribeTrainingJob$SecondaryStatus`): the stage that trains, the one
/// that uploads what it produced, and the status of a job whose work is over.
const TRAINING_OR_PAST: [&str; 3] = ["Training", "Uploading", "Completed"];

/// The directory a container of these cases writes its checkpoints to, which is
/// `CheckpointConfig$LocalPath`'s own default.
const CHECKPOINT_DIRECTORY: &str =
    service_com_amazonaws_sagemaker_sagemaker::state::DEFAULT_CHECKPOINT_DIRECTORY;

/// Hold the bucket and the execution role every job below runs under, and answer the role's
/// ARN.
async fn given_role(emulator: &Emulator) -> String {
    emulator.given_bucket(BUCKET).await;
    emulator
        .given_role("sagemaker-stage-backing-role", Some(&read_and_write()))
        .await
}

/// Stage one channel of records for `job` and answer the `InputDataConfig` naming it.
async fn given_channel(emulator: &Emulator, job: &str) -> Value {
    let key = format!("{job}/train/data.csv");
    emulator
        .given_object(BUCKET, &key, &csv_records(16, 2))
        .await;
    json!([channel("train", &format!("s3://{BUCKET}/{key}"), "S3Prefix")])
}

/// Replace the job's algorithm specification with one whose container runs `program`, keeping
/// the image the harness's body already names.
fn with_program(body: &mut Value, program: &str) {
    let image = body["AlgorithmSpecification"]["TrainingImage"].clone();
    body["AlgorithmSpecification"] = json!({
        "TrainingImage": image,
        "TrainingInputMode": "File",
        "ContainerEntrypoint": ["/bin/sh", "-c", program],
    });
}

/// Poll `DescribeTrainingJob` until the read reports a `SecondaryStatus` in `wanted`, answering
/// that read. A job that fails instead ends the case where it failed, with the reason it gives.
async fn poll_until_reported(
    emulator: &Emulator,
    case: &str,
    job: &str,
    wanted: &[&str],
) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        assert_ne!(
            str_field(&read, "TrainingJobStatus"),
            "Failed",
            "case {case:?}: the job must run, got {read}"
        );
        if wanted.contains(&str_field(&read, "SecondaryStatus").as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job must reach one of {wanted:?}, last read {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE, the checkpoint leg: the objects `CheckpointConfig` asked for are readable from the
/// first read that reports the stage they are written during.
///
/// The rows differ in the form the checkpoint location is given in — a prefix ending in the
/// separator, one that does not, a nested one — and in whether the job's own declared program
/// wrote the checkpoint directory, which decides WHICH files become those objects.
#[tokio::test]
async fn the_checkpoints_are_readable_from_the_read_that_reports_the_stage_that_writes_them() {
    /// One row: the case, the job name, the checkpoint location as the request writes it, the
    /// program the container runs (`None` for the default train instructions), and the object
    /// the prefix has to hold with what it has to carry.
    struct Row {
        case: &'static str,
        job: &'static str,
        location: String,
        program: Option<String>,
        object: String,
        holds: Option<&'static str>,
    }
    let rows = vec![
        Row {
            case: "a checkpoint prefix ending in the separator, the default train instructions",
            job: "checkpoint-separator",
            location: format!("s3://{BUCKET}/checkpoint-separator/checkpoints/"),
            program: None,
            object: format!("checkpoint-separator/checkpoints/{MODEL_ENTRY}"),
            holds: None,
        },
        Row {
            case: "a nested checkpoint prefix with no trailing separator",
            job: "checkpoint-nested",
            location: format!("s3://{BUCKET}/checkpoint-nested/runs/saved"),
            program: None,
            object: format!("checkpoint-nested/runs/saved/{MODEL_ENTRY}"),
            holds: None,
        },
        Row {
            case: "a declared program that writes the checkpoint directory itself",
            job: "checkpoint-program",
            location: format!("s3://{BUCKET}/checkpoint-program/checkpoints/"),
            program: Some(format!(
                "mkdir -p /opt/ml/model {CHECKPOINT_DIRECTORY} && \
                 echo fitted > /opt/ml/model/artifact.txt && \
                 echo step-3 > {CHECKPOINT_DIRECTORY}/state.txt"
            )),
            object: "checkpoint-program/checkpoints/state.txt".to_string(),
            holds: Some("step-3"),
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_role(&emulator).await;
        let channels = given_channel(&emulator, row.job).await;
        let mut body = training_body(
            row.job,
            &role,
            channels,
            &format!("s3://{BUCKET}/{}/out/", row.job),
        );
        body["CheckpointConfig"] = json!({
            "S3Uri": row.location,
            "LocalPath": CHECKPOINT_DIRECTORY,
        });
        if let Some(program) = row.program.as_ref() {
            with_program(&mut body, program);
        }
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;

        // The first read that reports the stage the checkpoints are written during is the read
        // the rule is about: whatever that stage leaves behind has to be there already.
        let read = poll_until_reported(&emulator, case, row.job, &TRAINING_OR_PAST).await;
        let held = emulator.object_bytes(BUCKET, &row.object).await;
        let bytes = held.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the checkpoint {:?} must be readable from the read that reports \
                 {:?}, got {read}",
                row.object,
                str_field(&read, "SecondaryStatus")
            )
        });
        assert!(
            !bytes.is_empty(),
            "case {case:?}: the checkpoint {:?} must hold what the run wrote, got no bytes",
            row.object
        );
        if let Some(expected) = row.holds {
            let text = String::from_utf8_lossy(&bytes).trim().to_string();
            assert_eq!(
                text, expected,
                "case {case:?}: the checkpoint must hold what the program wrote, got {text:?}"
            );
        }
    }
}

/// RULE, the artifact leg: the object `ModelArtifacts.S3ModelArtifacts` names is readable from
/// the first read that reports the job's terminal status.
///
/// The rows differ in what WROTE the model directory the artifact is packed from: the default
/// train instructions, or the job's own declared program.
#[tokio::test]
async fn the_artifact_is_readable_from_the_read_that_reports_the_job_finished() {
    // (the case, the job name, the program the container runs)
    let rows: Vec<(&str, &str, Option<String>)> = vec![
        ("the default train instructions", "backed-default", None),
        (
            "a declared program that wrote the model directory",
            "backed-program",
            Some(
                "mkdir -p /opt/ml/model && echo trained-here > /opt/ml/model/artifact.txt"
                    .to_string(),
            ),
        ),
    ];

    for (case, job, program) in rows {
        let emulator = Emulator::new();
        let role = given_role(&emulator).await;
        let channels = given_channel(&emulator, job).await;
        let output = format!("s3://{BUCKET}/{job}/out/");
        let mut body = training_body(job, &role, channels, &output);
        if let Some(program) = program.as_ref() {
            with_program(&mut body, program);
        }
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;

        let read = poll_until_reported(&emulator, case, job, &["Completed"]).await;
        assert_eq!(
            str_field(&read, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: the job must complete, got {read}"
        );
        let named = read
            .get("ModelArtifacts")
            .map(|artifacts| str_field(artifacts, "S3ModelArtifacts"))
            .unwrap_or_default();
        assert_eq!(
            named,
            artifact_uri(&output, job),
            "case {case:?}: the read must name the artifact of this job, got {read}"
        );
        let key = artifact_key(&output, job);
        let bytes = emulator
            .object_bytes(BUCKET, &key)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the artifact {named:?} the read names must already be \
                     readable, got nothing under {key:?}"
                )
            });
        assert!(
            !bytes.is_empty(),
            "case {case:?}: the artifact must hold the archive the run built, got no bytes"
        );
    }
}
