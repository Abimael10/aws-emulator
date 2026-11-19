// SPDX-License-Identifier: CC0-1.0

//! WHEN the objects a batch transform job wrote are there: the output of a job against the
//! terminal status the clock owes it, read by several clients at once at different rates.
//!
//! RULE — a read that reports a batch transform job `Completed` is BACKED by the objects the
//! job's `TransformOutput.S3OutputPath` is to hold: "For every S3 object used as input for the
//! transform job, batch transform stores the transformed data with an .out suffix in a
//! corresponding subfolder in the location in the output prefix"
//! (`TransformOutput$S3OutputPath`), so by the time ANY read answers that terminal status the
//! `.out` object is readable — whoever settled the job (this read, or an earlier read of
//! another client) and however coarsely that client polls.
//!
//! This is the invariant a transform-then-consume client depends on: it polls
//! `DescribeTransformJob` to decide when the results exist and then reads them out of the
//! output prefix, so a `Completed` answered while the output is still missing sends it to an
//! object that is not there. It is the same invariant the training side pins for the model
//! artifact ([`crate::training_artifact_publication`]), stated for the output of a transform
//! job, which no suite pinned before: a suite that relies on the ordering instead of asserting
//! it turns any lag into a failure decided by how fast the host ran.
//!
//! The rows differ along how OFTEN a client looks, which is what decides WHICH read settles a
//! job: every job is watched by TWO clients at once — one looking again as soon as its last
//! read answered, and one looking at the row's own rate — so whichever gets there first
//! settles the job and the other reads a job somebody else settled. The invariant is the same
//! for both, so every read of every client holds it.
//!
//! Every wait here is a poll for the CONDITION under a deadline far longer than the span this
//! state gives a transform job's work, so no claim rests on how fast the host runs. The
//! harness, the role, the trained model and the listing are the job-execution suite's
//! ([`crate::sagemaker_job_execution_over_s3`]) rather than a second copy of them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::model_artifact_admission::{str_field, Emulator};
use super::sagemaker_job_execution_over_s3::{
    every_s3_action_on, given_job_role, given_trained_artifact, listed_keys, own_image,
    SETTLE_BUDGET,
};

/// How often each client of a row looks again. The spread is what makes a job fall to
/// different settlers: a client looking again as soon as its last read answered has a read in
/// flight at whatever instant the clock makes the terminal status due, while the longest
/// interval is longer than the whole of a job's work, so that client reads a job the faster one
/// already settled. A zero interval is a client that yields and looks again — it waits for the
/// CONDITION like every other one, just without pausing in between.
const READER_INTERVALS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(37),
    Duration::from_millis(113),
    Duration::from_millis(601),
];

/// The bucket every object of this suite lives in.
const BUCKET: &str = "transform-publication";

/// How many records the one input object of each job carries, so the count of predictions its
/// `.out` has to hold is known.
const RECORDS: usize = 3;

/// The model every job of this suite applies.
const MODEL: &str = "publicationmodel";

/// The transform job of row `index`.
fn job_name(index: usize) -> String {
    format!("publicationjob{index}")
}

/// The object the job of row `index` has to leave under its output location.
fn output_key(index: usize) -> String {
    format!("scored{index}/records.csv.out")
}

/// RULE: every read that reports a batch transform job `Completed` is backed by the `.out`
/// object the job's output location is to hold, whichever client settled the job.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_reporting_a_transform_job_completed_is_backed_by_its_output_objects() {
    let setup = "the model every job of this suite applies";
    let emulator = Arc::new(Emulator::new());
    let role = given_job_role(&emulator, &every_s3_action_on(BUCKET)).await;
    emulator.given_bucket(BUCKET).await;
    // The model the transforms apply is one this emulator's own training produced, so the
    // artifact they read carries parameters a run really measured.
    let artifact = given_trained_artifact(&emulator, setup, BUCKET, &role, 0).await;
    emulator
        .sagemaker_ok(
            setup,
            "CreateModel",
            &json!({
                "ModelName": MODEL,
                "ExecutionRoleArn": role,
                "PrimaryContainer": { "Image": own_image(), "ModelDataUrl": artifact },
            }),
        )
        .await;

    // One job per row, each reading its own input object and writing under its own output
    // prefix, so what a client observes is its own job's output and never another's. Every job
    // is accepted before any client starts looking, so the clients really do watch at the same
    // time and which read settles which job is decided by their rates.
    let records: String = (0..RECORDS).map(|record| format!("{record}\n")).collect();
    for index in 0..READER_INTERVALS.len() {
        emulator
            .given_object(BUCKET, &format!("in{index}/records.csv"), &records)
            .await;
        emulator
            .sagemaker_ok(
                setup,
                "CreateTransformJob",
                &json!({
                    "TransformJobName": job_name(index),
                    "ModelName": MODEL,
                    "TransformInput": {
                        "DataSource": {
                            "S3DataSource": {
                                "S3DataType": "S3Prefix",
                                "S3Uri": format!("s3://{BUCKET}/in{index}/"),
                            },
                        },
                        "ContentType": "text/csv",
                        "SplitType": "Line",
                    },
                    "TransformOutput": { "S3OutputPath": format!("s3://{BUCKET}/scored{index}/") },
                    "TransformResources": { "InstanceType": "ml.m5.large", "InstanceCount": 1 },
                }),
            )
            .await;
    }

    // Two clients per job, each on a task of its own so none of them can hold the others up:
    // one that never pauses between reads, and one looking again at the row's own rate.
    let mut watching = Vec::new();
    for (index, interval) in READER_INTERVALS.into_iter().enumerate() {
        for (who, interval) in [
            ("a client that never pauses", Duration::ZERO),
            ("a client looking again at its own rate", interval),
        ] {
            watching.push((
                index,
                who,
                tokio::spawn(watch(Arc::clone(&emulator), index, interval, who)),
            ));
        }
    }
    for (index, who, handle) in watching {
        handle
            .await
            .unwrap_or_else(|error| panic!("job {index} watched by {who}: {error}"));
    }
}

/// One client watching the job of row `index`: polls `DescribeTransformJob` every `interval`
/// until the job leaves `InProgress`, then reads the object the terminal status promised.
///
/// The wait is on the CONDITION — the status, re-read in a loop — and the deadline only bounds
/// an emulator that never settles the job at all; `interval` decides how OFTEN this client
/// looks, never how long the case may take.
async fn watch(emulator: Arc<Emulator>, index: usize, interval: Duration, who: &'static str) {
    let case = format!("job {index} watched by {who} every {interval:?}");
    let job = job_name(index);
    let deadline = Instant::now() + SETTLE_BUDGET;
    let settled = loop {
        let read = emulator
            .sagemaker_ok(
                &case,
                "DescribeTransformJob",
                &json!({ "TransformJobName": job }),
            )
            .await;
        if str_field(&read, "TransformJobStatus") != "InProgress" {
            break read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the transform job did not leave InProgress within \
             {SETTLE_BUDGET:?}; last read {read}"
        );
        if interval.is_zero() {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(interval).await;
        }
    };
    assert_eq!(
        str_field(&settled, "TransformJobStatus"),
        "Completed",
        "case {case:?}: a job whose run scored its input must complete, got {settled}"
    );

    // The read above is the FIRST one this client saw a terminal status in, so the object it
    // promised has to be there already: the listing and the read below look ONCE, because
    // being backed by that object is what the status means.
    let key = output_key(index);
    let held = listed_keys(&emulator, BUCKET, &format!("scored{index}/")).await;
    assert_eq!(
        held,
        vec![key.clone()],
        "case {case:?}: a completed job's output location holds one .out per input object, got \
         {held:?}"
    );
    let (status, body) = emulator.s3("GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the object a completed job reported writing must be served, got \
         {status} {}",
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8(body).expect("the scored object is text");
    let predictions: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        predictions.len(),
        RECORDS,
        "case {case:?}: the input object holds {RECORDS} records, so {key} must hold that many \
         predictions, got {predictions:?}"
    );
    for prediction in &predictions {
        assert!(
            prediction.parse::<f64>().is_ok(),
            "case {case:?}: every line of {key} is a prediction, got {prediction:?}"
        );
    }
}
