// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two objects a training job's output location holds, as
//! `DescribeTrainingJob` reports them: the model archive (`ModelArtifacts`) and the job's
//! other output (`TrainingJobOutput`).
//!
//! RULE — both are named from the output location the request was accepted with and the job's
//! own name, under the one prefix the job's run writes to
//! (`<S3OutputPath>/<TrainingJobName>/output/`): the model archive is `model.tar.gz` there,
//! and the job's output archive — everything the run wrote beside the model it fitted — is
//! `output.tar.gz` there. Both are reported on every read of the job, before its run has
//! finished and after, and neither changes between reads.
//!
//! Driven through the harness [`crate::training_job_execution`] builds — SageMaker, Amazon S3
//! and IAM on ONE shared engine, every call travelling the real wire — so what is asserted is
//! what an SDK client would see.
//!
//! Nothing here is weighed against how fast this host ran it: the job is waited for by
//! looking again until it reports a status nothing leaves on its own, under a deadline far
//! longer than the span this state gives a job's whole work.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    channel, csv_records, read_and_write, str_field, training_body, Emulator, BUCKET,
};

/// The `TrainingJobStatus` members nothing leaves on its own, so a poll that reaches one is
/// over (`DescribeTrainingJob$TrainingJobStatus`).
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// How long the poll below gives the clock to settle a job, and how often it looks again:
/// what is waited for is the CONDITION, so a loaded host cannot fail a case.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// The object the two members name under `output` for `job`: the one prefix a run writes to,
/// and the archive inside it.
///
/// Written here from the documented layout — `ModelArtifacts$S3ModelArtifacts` is "for
/// example, `s3://bucket-name/keynameprefix/model.tar.gz`" and SageMaker writes a job's
/// output under `<S3OutputPath>/<TrainingJobName>/output/` — rather than from the service's
/// own reading of it.
fn output_object(output: &str, job: &str, object: &str) -> String {
    let mut base = output.to_string();
    while base.ends_with('/') {
        base.pop();
    }
    format!("{base}/{job}/output/{object}")
}

/// The `TrainingJobOutput.S3TrainingJobOutput` of a describe answer, and `None` when the
/// member is absent.
fn job_output(described: &Value) -> Option<String> {
    described
        .pointer("/TrainingJobOutput/S3TrainingJobOutput")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// RULE: every read of a training job names both objects its output location holds — the
/// model archive and the job's output archive — under the prefix the output location and the
/// job's name give, whatever the form of that location, and the same before and after the
/// job's run has finished.
#[tokio::test]
async fn a_training_job_names_the_model_archive_and_its_output_archive_under_one_prefix() {
    // Rows differ along what the rule generalises over: the form of the output location (a
    // trailing slash, no slash, a nested prefix, the bucket's own root) and the shape of the
    // job's name.
    let rows: [(&str, &str, &str); 5] = [
        (
            "an output prefix with a trailing slash",
            "job-output-trailing",
            "s3://sagemaker-training-run/out-trailing/",
        ),
        (
            "an output prefix with no trailing slash",
            "job-output-plain",
            "s3://sagemaker-training-run/out-plain",
        ),
        (
            "a nested output prefix",
            "job-output-nested",
            "s3://sagemaker-training-run/team/runs/2026/out",
        ),
        (
            "the bucket's own root",
            "job-output-root",
            "s3://sagemaker-training-run",
        ),
        (
            // `TrainingJobName` is `len 1..63` (smithy_query operation=CreateTrainingJob),
            // so this row sits on the upper bound.
            "a job name on the modelled maximum length",
            "a234567890123456789012345678901234567890123456789012345678901b",
            "s3://sagemaker-training-run/out-long",
        ),
    ];

    for (case, job, output) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role = emulator
            .given_role("job-output-role", Some(&read_and_write()))
            .await;
        emulator
            .given_object(BUCKET, "job-output/train.csv", &csv_records(8, 3))
            .await;

        emulator
            .sagemaker_ok(
                "CreateTrainingJob",
                &training_body(
                    job,
                    &role,
                    json!([channel(
                        "train",
                        &format!("s3://{BUCKET}/job-output/train.csv"),
                        "S3Prefix"
                    )]),
                    output,
                ),
            )
            .await;

        let expected_model = output_object(output, job, "model.tar.gz");
        let expected_output = output_object(output, job, "output.tar.gz");

        // Reported while the run is still in flight: both are the locations the job writes
        // to, so they are named before anything has been written there.
        let described = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        assert_eq!(
            job_output(&described).as_deref(),
            Some(expected_output.as_str()),
            "case {case:?}: expected TrainingJobOutput.S3TrainingJobOutput \
             {expected_output:?}, got {described}"
        );
        assert_eq!(
            described
                .pointer("/ModelArtifacts/S3ModelArtifacts")
                .and_then(Value::as_str),
            Some(expected_model.as_str()),
            "case {case:?}: expected ModelArtifacts.S3ModelArtifacts {expected_model:?}, got \
             {described}"
        );
    }
}

/// RULE: neither object is derived from the read that reports it — a job settled by the clock
/// names the same two objects a job still running does.
#[tokio::test]
async fn the_named_objects_do_not_change_when_the_job_settles() {
    let case = "a job read before and after the clock settles it";
    let job = "job-output-settled";
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("job-output-settled-role", Some(&read_and_write()))
        .await;
    emulator
        .given_object(BUCKET, "job-output-settled/train.csv", &csv_records(8, 3))
        .await;

    let output = format!("s3://{BUCKET}/out-settled");
    emulator
        .sagemaker_ok(
            "CreateTrainingJob",
            &training_body(
                job,
                &role,
                json!([channel(
                    "train",
                    &format!("s3://{BUCKET}/job-output-settled/train.csv"),
                    "S3Prefix"
                )]),
                &output,
            ),
        )
        .await;
    let running = emulator
        .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
        .await;

    // Look again until the job reports a status nothing leaves on its own; only the loop's
    // condition decides when it stops.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let settled = loop {
        let described = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        if TERMINAL.contains(&str_field(&described, "TrainingJobStatus").as_str()) {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job {job} must reach a terminal status, last read {described}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    };
    assert_eq!(
        job_output(&settled),
        job_output(&running),
        "case {case:?}: the output archive a settled job names, got {settled}"
    );
    assert_eq!(
        settled.pointer("/ModelArtifacts/S3ModelArtifacts"),
        running.pointer("/ModelArtifacts/S3ModelArtifacts"),
        "case {case:?}: the model archive a settled job names, got {settled}"
    );
    assert_eq!(
        job_output(&settled).as_deref(),
        Some(output_object(&output, job, "output.tar.gz").as_str()),
        "case {case:?}: expected the job's own output archive, got {settled}"
    );
}

/// RULE: the output archive a job names is the job's OWN — two jobs writing to the same
/// output location name two different objects, each under its own name.
#[tokio::test]
async fn two_jobs_sharing_an_output_location_name_their_own_output_archives() {
    let case = "two jobs under one output prefix";
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("shared-output-role", Some(&read_and_write()))
        .await;
    emulator
        .given_object(BUCKET, "shared-output/train.csv", &csv_records(8, 3))
        .await;

    let output = format!("s3://{BUCKET}/shared-output-prefix");
    let mut named = Vec::new();
    for job in ["shared-output-first", "shared-output-second"] {
        emulator
            .sagemaker_ok(
                "CreateTrainingJob",
                &training_body(
                    job,
                    &role,
                    json!([channel(
                        "train",
                        &format!("s3://{BUCKET}/shared-output/train.csv"),
                        "S3Prefix"
                    )]),
                    &output,
                ),
            )
            .await;
        let described = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        let reported = job_output(&described)
            .unwrap_or_else(|| panic!("case {case:?}: expected a TrainingJobOutput in {described}"));
        assert_eq!(
            reported,
            output_object(&output, job, "output.tar.gz"),
            "case {case:?}: the archive is named under the job's own name, got {described}"
        );
        named.push(reported);
    }
    assert_ne!(
        named[0], named[1],
        "case {case:?}: two jobs name two objects, got {named:?}"
    );
}
