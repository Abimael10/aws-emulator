// SPDX-License-Identifier: CC0-1.0

//! WHEN the object a training run produced is there: the publication of the model artifact
//! against the status the clock owes the job, read by several clients at once.
//!
//! RULE — a read that reports a training job past the stage which uploads its artifact is
//! BACKED by that object: the publication the clock owes and the move out of `InProgress` it
//! owes fall due at the same instant, so any read reporting `Completed` with
//! `ModelArtifacts.S3ModelArtifacts` is answered only once Amazon S3 really holds that
//! object — whoever made the publication (the read itself, or the instant armed by the create
//! for a client that never calls this service again) and whatever else is reading the same
//! job at the same time.
//!
//! This is the invariant a train-then-deploy client depends on: it polls `DescribeTrainingJob`
//! to decide when the artifact exists and then hands the URI it read to `CreateModel`, so a
//! `Completed` answered while the upload is still in flight sends it to an object that is not
//! there.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine — what `core/router` assembles — through `MultiServiceRouter::dispatch`, so what is
//! asserted is what an SDK client would see. The harness, the body builders and the
//! artifact-location reading are the training-run suite's ([`crate::training_job_execution`])
//! rather than a second copy of them.
//!
//! Every wait here is a poll that looks again until the condition it waits for holds, under a
//! deadline far longer than any span this state gives a job's work, so no claim rests on how
//! fast the host runs. The readers differ in how OFTEN they look, which is what decides who
//! makes the publication: a reader looking more often than the span gets there first, while
//! one looking coarsely finds the publication the armed instant already made — and the
//! invariant is the same for both.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_key, artifact_uri, channel, csv_records, read_and_write, training_body, Emulator,
    BUCKET,
};

/// How long a job is polled for the terminal status it has to reach before a case fails. What
/// is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a case the
/// clock would have served; the budget is far longer than the span this state gives a job's
/// work.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often each reader of a case looks again. The spread is what makes the publication fall
/// to different publishers: the first reader looks again as soon as its last read answered, so
/// one of its reads is in flight at whatever instant the clock makes the publication due, while
/// the longest interval is longer than the whole of a job's work, so that reader only ever sees
/// what the armed instant published. A zero interval is a reader that yields and looks again —
/// it waits for the CONDITION like every other one, just without pausing in between.
const READER_INTERVALS: [Duration; 4] = [
    Duration::ZERO,
    Duration::from_millis(37),
    Duration::from_millis(113),
    Duration::from_millis(601),
];

/// How often the client that watches the OUTPUT PREFIX instead of the API looks again.
const WATCH_INTERVAL: Duration = Duration::from_millis(50);

/// How many jobs of each row the sole-poller case runs: one job's publication falls due once,
/// so the trials of a race are the jobs, not the reads.
const COPIES_PER_ROW: usize = 3;

/// The two statuses a job of these cases may be read in: it is accepted `InProgress` and its
/// run has everything it needs, so the clock settles it `Completed`
/// (`com.amazonaws.sagemaker#TrainingJobStatus`,
/// `smithy_query(query="shape",shape="TrainingJobStatus")`).
const STATUS_IN_PROGRESS: &str = "InProgress";
/// See [`STATUS_IN_PROGRESS`].
const STATUS_COMPLETED: &str = "Completed";

/// One row of the publication table.
struct Row {
    case: &'static str,
    /// The name every resource of the row is created under.
    name: &'static str,
    /// `OutputDataConfig.S3OutputPath`, as the request writes it.
    output: String,
    /// The channels the job declares; the first is the `train` channel the fit reads.
    channels: &'static [&'static str],
    /// How many records each channel's data carries.
    records: usize,
    /// How many feature columns each record carries.
    features: usize,
    /// Whether the row's channels address a PREFIX holding several objects rather than one
    /// object by its own key.
    prefix_addressed: bool,
}

/// The publication table: the rows differ in the form of the output location the artifact
/// lands under, how a channel's location is addressed, how many channels the job declares and
/// how much data they carry — what the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one object, an output prefix ending in a separator",
            name: "publish-single",
            output: format!("s3://{BUCKET}/published/"),
            channels: &["train"],
            records: 16,
            features: 3,
            prefix_addressed: false,
        },
        Row {
            case: "a prefix holding several objects, a nested output prefix",
            name: "publish-prefix",
            output: format!("s3://{BUCKET}/runs/nested/out"),
            channels: &["train", "validation"],
            records: 12,
            features: 1,
            prefix_addressed: true,
        },
        Row {
            case: "the bucket root as the output location",
            name: "publish-bucket-root",
            output: format!("s3://{BUCKET}"),
            channels: &["train"],
            records: 8,
            features: 2,
            prefix_addressed: false,
        },
    ]
}

/// Stage `job`'s channel data in Amazon S3, as the row addresses it, and answer the
/// `InputDataConfig` naming those locations.
async fn given_channels(emulator: &Emulator, row: &Row, job: &str) -> Value {
    let mut channels = Vec::new();
    for name in row.channels {
        let data = csv_records(row.records, row.features);
        let uri = if row.prefix_addressed {
            // The same records split over two objects under one prefix, so the channel's
            // location answers more than one object.
            let prefix = format!("{job}/{name}");
            let lines: Vec<&str> = data.lines().collect();
            let (head, tail) = lines.split_at(lines.len() / 2);
            for (part, half) in [("part-1", head), ("part-2", tail)] {
                emulator
                    .given_object(
                        BUCKET,
                        &format!("{prefix}/{part}.csv"),
                        &format!("{}\n", half.join("\n")),
                    )
                    .await;
            }
            format!("s3://{BUCKET}/{prefix}/")
        } else {
            let key = format!("{job}/{name}/data.csv");
            emulator.given_object(BUCKET, &key, &data).await;
            format!("s3://{BUCKET}/{key}")
        };
        channels.push(channel(name, &uri, "S3Prefix"));
    }
    Value::Array(channels)
}

/// The execution role every job of a case runs under: readable channels, a writable output
/// location. Created once per engine, so a case with several jobs runs them all under the one
/// role the way an account does.
async fn given_role(emulator: &Emulator) -> String {
    emulator.given_bucket(BUCKET).await;
    emulator
        .given_role("sagemaker-publication-role", Some(&read_and_write()))
        .await
}

/// Stage `job`'s channel data and accept it as a training job of the row's shape.
async fn given_accepted_job(emulator: &Emulator, row: &Row, role_arn: &str, job: &str) {
    let channels = given_channels(emulator, row, job).await;
    emulator
        .sagemaker_ok(
            "CreateTrainingJob",
            &training_body(job, role_arn, channels, &row.output),
        )
        .await;
}

/// One client watching the job: polls `DescribeTrainingJob` every `interval` until the job
/// reports `Completed`, holding the rule on EVERY read it makes — a read reporting the
/// terminal status names the artifact under the job's output location AND Amazon S3 already
/// holds that object. Answers the bytes it read out of Amazon S3.
async fn read_until_completed(
    emulator: Arc<Emulator>,
    case: &'static str,
    job: String,
    output: String,
    interval: Duration,
) -> Vec<u8> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "TrainingJobName": job });
    let want = artifact_uri(&output, &job);
    let key = artifact_key(&output, &job);
    loop {
        let read = emulator.sagemaker_ok("DescribeTrainingJob", &body).await;
        let status = read
            .get("TrainingJobStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == STATUS_COMPLETED {
            let artifact = read
                .pointer("/ModelArtifacts/S3ModelArtifacts")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                artifact, want,
                "case {case:?} reader {interval:?}: the completed job names its artifact under \
                 the output location the request gave, got {read}"
            );
            let held = emulator.object_bytes(BUCKET, &key).await;
            let held = held.unwrap_or_else(|| {
                panic!(
                    "case {case:?} reader {interval:?}: the read reported {STATUS_COMPLETED} \
                     naming {artifact:?}, so Amazon S3 must already hold that object; it holds \
                     nothing at {key:?}"
                )
            });
            assert!(
                held.len() > 2 && held[0] == 0x1f && held[1] == 0x8b,
                "case {case:?} reader {interval:?}: the published object must be the compressed \
                 archive a client untars, got {} bytes starting {:?}",
                held.len(),
                held.iter().take(2).collect::<Vec<_>>()
            );
            return held;
        }
        assert_eq!(
            status, STATUS_IN_PROGRESS,
            "case {case:?} reader {interval:?}: a job whose data is readable and whose output \
             location is writable runs to completion, got {status:?} with {read}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?} reader {interval:?}: the job never reached a terminal status; last \
             read {read}"
        );
        if interval.is_zero() {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(interval).await;
        }
    }
}

/// THE RULE for a client polling ALONE: no sibling reader can have made the publication for
/// it, so the read that first reports `Completed` is the one the publication fell to — and it
/// is answered only once the object is there.
///
/// This is the shape a train-then-deploy client has: one poller, which hands the URI it read
/// straight to `CreateModel`. Every row's job is watched by exactly one reader, and the rows
/// run at the same time on the one engine, so each reader is alone with its own job while the
/// store is as busy as an account's is.
#[tokio::test(flavor = "multi_thread")]
async fn the_only_client_polling_a_job_reads_completed_only_once_the_object_is_there() {
    let emulator = Arc::new(Emulator::new());
    let role_arn = given_role(&emulator).await;

    // Every row, several times over: one job's publication falls due once, so the trials are
    // the jobs — each with its own output location, data shape and sole reader.
    let mut watched = Vec::new();
    for row in rows() {
        for copy in 0..COPIES_PER_ROW {
            let job = format!("{}-{copy}", row.name);
            given_accepted_job(&emulator, &row, &role_arn, &job).await;
            watched.push((
                row.case,
                job.clone(),
                row.output.clone(),
                tokio::spawn(read_until_completed(
                    Arc::clone(&emulator),
                    row.case,
                    job,
                    row.output.clone(),
                    Duration::ZERO,
                )),
            ));
        }
    }

    for (case, job, output, handle) in watched {
        let held = handle
            .await
            .unwrap_or_else(|error| panic!("case {case:?} job {job:?}: {error}"));
        assert_eq!(
            emulator
                .object_bytes(BUCKET, &artifact_key(&output, &job))
                .await,
            Some(held),
            "case {case:?} job {job:?}: the object the poller was answered over stays exactly \
             as it read it"
        );
    }
}

/// THE RULE: every read that reports the job `Completed` is backed by the object in Amazon S3,
/// whoever published it and however many clients are reading at the same time.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_reported_completed_is_backed_by_the_artifact_in_amazon_s3() {
    for row in rows() {
        let case = row.case;
        let emulator = Arc::new(Emulator::new());
        let role_arn = given_role(&emulator).await;
        given_accepted_job(&emulator, &row, &role_arn, row.name).await;

        // Several clients watch the one job at once, each looking at its own rate, so the
        // publication falls to a read for some of them and to the armed instant for others.
        let mut readers = Vec::new();
        for interval in READER_INTERVALS {
            readers.push(tokio::spawn(read_until_completed(
                Arc::clone(&emulator),
                case,
                row.name.to_string(),
                row.output.clone(),
                interval,
            )));
        }
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for (reader, handle) in readers.into_iter().enumerate() {
            seen.push(
                handle
                    .await
                    .unwrap_or_else(|error| panic!("case {case:?} reader {reader}: {error}")),
            );
        }

        // One run produced ONE object, so every reader read the same bytes — the publication
        // happened exactly once however many readers raced for it.
        let first = seen.first().expect("every case has a reader").clone();
        for (reader, bytes) in seen.iter().enumerate() {
            assert_eq!(
                bytes, &first,
                "case {case:?} reader {reader}: every reader must read the one object the run \
                 produced, got {} bytes against the first reader's {}",
                bytes.len(),
                first.len()
            );
        }

        // And the job keeps reporting it: a later read answers the same artifact over the same
        // object, so nothing the publication did is undone by being read again.
        let after = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": row.name }))
            .await;
        assert_eq!(
            after.pointer("/ModelArtifacts/S3ModelArtifacts"),
            Some(&Value::String(artifact_uri(&row.output, row.name))),
            "case {case:?}: a settled job keeps reporting the artifact it wrote, got {after}"
        );
        assert_eq!(
            emulator
                .object_bytes(BUCKET, &artifact_key(&row.output, row.name))
                .await,
            Some(first),
            "case {case:?}: the published object is still the one the readers read"
        );
    }
}

/// THE RULE for a client that never calls this service again: the publication is owed by the
/// CLOCK, so the object appears under the output location without anybody describing the job —
/// and the describe that comes afterwards reports the very object the watcher found.
#[tokio::test]
async fn the_artifact_appears_for_a_client_that_only_watches_the_output_prefix() {
    let row = Row {
        case: "a client that only watches the output prefix",
        name: "publish-unwatched",
        output: format!("s3://{BUCKET}/unwatched/"),
        channels: &["train"],
        records: 10,
        features: 2,
        prefix_addressed: false,
    };
    let case = row.case;
    let emulator = Emulator::new();
    let role_arn = given_role(&emulator).await;
    given_accepted_job(&emulator, &row, &role_arn, row.name).await;

    // Nothing is described: the watcher looks only at Amazon S3, the way a client that submits
    // a job and then waits for the object does.
    let key = artifact_key(&row.output, row.name);
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let watched = loop {
        if let Some(bytes) = emulator.object_bytes(BUCKET, &key).await {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the artifact never appeared at {key:?} for a client that never \
             described the job"
        );
        tokio::time::sleep(WATCH_INTERVAL).await;
    };
    assert!(
        watched.len() > 2 && watched[0] == 0x1f && watched[1] == 0x8b,
        "case {case:?}: the object the watcher found must be the compressed archive a client \
         untars, got {} bytes starting {:?}",
        watched.len(),
        watched.iter().take(2).collect::<Vec<_>>()
    );

    // The job agrees with what the watcher found: the same object, and the terminal status.
    let read = emulator
        .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": row.name }))
        .await;
    assert_eq!(
        read.pointer("/ModelArtifacts/S3ModelArtifacts")
            .and_then(Value::as_str),
        Some(artifact_uri(&row.output, row.name).as_str()),
        "case {case:?}: the job reports the object the watcher found, got {read}"
    );
    assert_eq!(
        read.get("TrainingJobStatus").and_then(Value::as_str),
        Some(STATUS_COMPLETED),
        "case {case:?}: a job whose artifact is published has finished its work, got {read}"
    );
    assert_eq!(
        emulator.object_bytes(BUCKET, &key).await,
        Some(watched),
        "case {case:?}: the describe left the published object exactly as the watcher read it"
    );
}
