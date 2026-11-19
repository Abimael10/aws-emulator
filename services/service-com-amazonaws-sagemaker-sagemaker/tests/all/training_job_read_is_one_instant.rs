// SPDX-License-Identifier: CC0-1.0

//! The ONE instant a read of a training job answers as of, end to end.
//!
//! RULE — the publication the stage that uploads a job's model artifact owes and the move into
//! the terminal status that follows that stage fall due at the SAME instant, so a read decides
//! both against ONE reading of the clock: a read that reports a job `Completed`, naming
//! `ModelArtifacts.S3ModelArtifacts`, is answered only once Amazon S3 holds that object. A
//! client that polls the status to decide when the artifact is ready — a download, packaging or
//! deployment chain reads the two in that order — therefore never sees a completed job whose
//! artifact is still to appear.
//!
//! This file is the single end-to-end net for that rule, and it owns neither of the two tables
//! that span it: the publication table — output-location forms, prefix-addressed channels,
//! several readers at different rates, a client that only watches the output prefix, and a job
//! that declares no channel at all — belongs to [`crate::training_artifact_publication`], and
//! the instants either side of the due instant are driven directly, with no clock in play, by
//! `the_archive_and_the_terminal_status_are_owed_at_one_instant` in this crate's job state. What
//! is here is the shape neither of those has: one client, polling coarsely — looking again far
//! less often than the span the state gives a stage of the work — so the read that first reports
//! the terminal status comes well after the stage that uploaded the artifact, and the rule is
//! the same for it.
//!
//! Driven through the harness [`crate::training_job_execution`] builds — SageMaker, Amazon S3
//! and IAM on ONE shared engine, every call travelling the real wire — so what is asserted is
//! what an SDK client would see.
//!
//! Nothing here is weighed against how fast this host ran it: the wait is a poll that looks
//! again until the job reports a status nothing leaves on its own, under a deadline far longer
//! than the span the state gives a job's whole work, and the claim is made of whichever read
//! first reports that status, as the next thing this client does.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_key, artifact_uri, channel, csv_records, read_and_write, str_field, training_body,
    Emulator, BUCKET,
};

/// The `TrainingJobStatus` members nothing leaves on its own, so a poll that reaches one is
/// over (`DescribeTrainingJob$TrainingJobStatus`).
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// How long the poll below gives the clock to settle the job: far longer than the span this
/// crate's state gives a job's whole work, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often this client looks again: longer than the span the state gives one stage of the
/// work, so the read that first reports the terminal status is made well after the stage that
/// published the artifact.
const LOOKS_AGAIN_EVERY: Duration = Duration::from_secs(1);

/// RULE: the read that first reports a job `Completed` finds its artifact in Amazon S3, even
/// for a client that looks again only rarely.
#[tokio::test]
async fn the_read_that_first_reports_a_completed_job_finds_its_artifact_in_amazon_s3() {
    let case = "one client looking again more rarely than a stage of the work";
    let job = "one-instant-coarse-poll";
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("one-instant-role", Some(&read_and_write()))
        .await;
    emulator
        .given_object(BUCKET, "one-instant/train.csv", &csv_records(8, 3))
        .await;

    let output = format!("s3://{BUCKET}/one-instant-output/{job}/");
    emulator
        .sagemaker_ok(
            "CreateTrainingJob",
            &training_body(
                job,
                &role,
                json!([channel(
                    "train",
                    &format!("s3://{BUCKET}/one-instant/train.csv"),
                    "S3Prefix"
                )]),
                &output,
            ),
        )
        .await;

    // Look again until the job reports a status nothing leaves on its own. Only the loop's
    // condition decides when it stops; the deadline is there so a job that never settles fails
    // with what it was last reported as rather than hanging.
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
        tokio::time::sleep(LOOKS_AGAIN_EVERY).await;
    };
    assert_eq!(
        (
            str_field(&settled, "TrainingJobStatus"),
            str_field(&settled, "SecondaryStatus"),
        ),
        ("Completed".to_string(), "Completed".to_string()),
        "case {case:?}: expected a run that could read its channel and write its artifact to \
         complete, got {settled}"
    );
    let artifact = settled
        .pointer("/ModelArtifacts/S3ModelArtifacts")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        artifact,
        artifact_uri(&output, job),
        "case {case:?}: expected the artifact under the declared output path, got {settled}"
    );
    // The look-up is the very next call this client makes, so what it answers is what the read
    // that first reported the completed job had already left behind.
    assert!(
        emulator
            .object_bytes(BUCKET, &artifact_key(&output, job))
            .await
            .is_some(),
        "case {case:?}: expected the read that first reported the job completed to find \
         {artifact} naming an object Amazon S3 holds, got {settled}"
    );
}
