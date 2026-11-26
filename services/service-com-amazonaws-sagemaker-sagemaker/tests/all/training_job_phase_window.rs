// SPDX-License-Identifier: CC0-1.0

//! WHAT A CLIENT IN ANOTHER PROCESS SEES of a training job's work: whether the stages
//! `DescribeTrainingJob` groups under `InProgress` are in effect long enough for a poll loop of
//! its own to land inside one of them.
//!
//! RULE — a non-terminal status whose model documents an ordered sequence of observable
//! sub-stages is revealed one whole span per stage, and the stages that run the CONTAINER are
//! together in effect for LONGER than the interval a polling client sleeps between two reads.
//! `DescribeTrainingJob$SecondaryStatus` is exactly such a status: while `TrainingJobStatus` is
//! `InProgress` the secondary status walks `Starting`, the optional `Downloading`, `Training`
//! and `Uploading`, and a client learns that the container is running — and that the model
//! artifact has been written — only by POLLING. A client polls a job on the scale of SECONDS,
//! commonly ten of them, so a walk narrower than that interval is reported by the API and
//! observed by nobody: every such client sees the job teleport from `InProgress` to its terminal
//! status, and `SecondaryStatusTransitions` names stages it never had a chance to see.
//!
//! The suite below therefore polls at a CLIENT's cadence ([`POLL_CADENCE`]) rather than as fast
//! as an in-process caller can, and asserts twice over:
//!
//! * end to end, that such a poller finds the job in one of the stages that run the container
//!   while it is still `InProgress`;
//! * against the job's OWN reported schedule, that the window those stages occupy is wider than
//!   that cadence — which is the rule itself, and is decided by the instants the job reports
//!   rather than by how fast this host ran the poller.
//!
//! The table spans what the rule generalises over: how many channels the job declares and
//! whether it declares any at all (a job with no data to bring down has the FEWEST such stages,
//! which is the binding case), and whether its container states work of its own.
//!
//! Nothing here waits for a fixed time outside a poll loop and nothing asserts on elapsed wall
//! clock: every wait looks again until the condition holds, under a deadline far longer than any
//! span this state gives a job's work.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the harnesses of [`crate::sagemaker_job_execution_over_s3`]
//! and [`crate::model_artifact_admission`] rather than a second copy of them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::model_artifact_admission::{str_field, Emulator};
use crate::sagemaker_job_execution_over_s3::{
    channel, every_s3_action_on, given_job_role, own_image,
};

/// The bucket every object of this suite lives in.
const BUCKET: &str = "training-job-phase-window";

/// The cadence a client in ANOTHER PROCESS polls `DescribeTrainingJob` on.
///
/// Ten seconds is what a progress view, a waiter or a test harness sleeps between two reads of
/// a training job — the SDK's own waiter for this operation sleeps far longer — so it is the
/// interval the stages of the work have to outlast. It is a CADENCE, not a timeout: nothing
/// below fails because a span elapsed, only because the service answered the wrong thing.
const POLL_CADENCE: Duration = Duration::from_secs(10);

/// How long the poll loop keeps looking before the suite calls the emulator broken. Far longer
/// than the whole walk of a job's work, so it bounds a lifecycle that never settles rather than
/// the condition a row waits for.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The `SecondaryStatus` members that mean the job's ML instances are doing its work — the
/// stages a client polls for to learn that the container is running and that the artifact has
/// been uploaded (`DescribeTrainingJob$SecondaryStatus`'s own doc, under the `InProgress`
/// group: smithy_query query=doc shape=DescribeTrainingJobResponse member=SecondaryStatus).
const ACTIVE_CONTAINER: [&str; 4] = [
    "Downloading",
    "DownloadingTrainingImage",
    "Training",
    "Uploading",
];

/// The `TrainingJobStatus` members nothing leaves on its own, so a poll that reaches one is over
/// (smithy_query query=shape shape=TrainingJobStatus).
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// One row of the table: the job's name, the channels it declares and the work its container
/// states, when it declares a program at all.
struct Row {
    case: &'static str,
    name: &'static str,
    channels: &'static [&'static str],
    works_for: Option<u64>,
}

/// The table of the RULE: the window has to outlast a client's cadence whatever the job's work
/// is, and least of all for the job with the fewest stages.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no channel at all, so the work has no stage that downloads data",
            name: "phasewindownochannel",
            channels: &[],
            works_for: None,
        },
        Row {
            case: "one channel of records to bring down",
            name: "phasewindowonechannel",
            channels: &["train"],
            works_for: None,
        },
        Row {
            case: "a training channel beside a validation channel",
            name: "phasewindowtwochannels",
            channels: &["train", "validation"],
            works_for: None,
        },
        Row {
            case: "a container program that states work of its own",
            name: "phasewindowcontainerwork",
            channels: &["train"],
            works_for: Some(5),
        },
    ]
}

/// A `CreateTrainingJob` body for `row`: its channels, its output location and the program its
/// container runs when the row states one.
fn training_body(row: &Row, role: &str) -> Value {
    let channels: Vec<Value> = row
        .channels
        .iter()
        .map(|name| channel(name, BUCKET, &format!("{}/{name}/", row.name)))
        .collect();
    let mut body = json!({
        "TrainingJobName": row.name,
        "AlgorithmSpecification": { "TrainingImage": own_image(), "TrainingInputMode": "File" },
        "RoleArn": role,
        "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/{}/out/", row.name) },
        "ResourceConfig": {
            "InstanceType": "ml.m5.large",
            "InstanceCount": 1,
            "VolumeSizeInGB": 10,
        },
        // Far past the work any row states, so no row is ended by the cap instead of finishing.
        "StoppingCondition": { "MaxRuntimeInSeconds": 86_400 },
    });
    // `CreateTrainingJobRequest$InputDataConfig` is bounded at one channel or more
    // (smithy_query query=shape shape=InputDataConfig), so a job with no channel declares the
    // member not at all.
    if !channels.is_empty() {
        body["InputDataConfig"] = Value::Array(channels);
    }
    if let Some(seconds) = row.works_for {
        body["AlgorithmSpecification"]["ContainerEntrypoint"] = json!(["/bin/sh", "-c"]);
        body["AlgorithmSpecification"]["ContainerArguments"] = json!([format!("sleep {seconds}")]);
    }
    body
}

/// Comma-separated training records: `rows` records whose first column is the target and whose
/// second is the one feature it is a linear function of.
fn training_records(rows: usize) -> String {
    (0..rows)
        .map(|row| format!("{},{row}\n", 3 + row))
        .collect()
}

/// One read a poller made: what the job reported as its primary and secondary status.
type Seen = (String, String);

/// One `SecondaryStatusTransitions` entry: the status, when the job entered it and when it left
/// it (`None` while it has not).
type Stage = (String, f64, Option<f64>);

/// The history a read reports, in the order it reports it.
fn history(read: &Value, case: &str) -> Vec<Stage> {
    read.get("SecondaryStatusTransitions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected SecondaryStatusTransitions in {read}"))
        .iter()
        .map(|entry| {
            let start = entry
                .get("StartTime")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("case {case:?}: expected a StartTime in {entry}"));
            (
                str_field(entry, "Status"),
                start,
                entry.get("EndTime").and_then(Value::as_f64),
            )
        })
        .collect()
}

/// RULE: a client polling at its own cadence observes one of the stages that run the container
/// while the job is still `InProgress`, and the job's own schedule says that window is wider
/// than such a cadence.
#[tokio::test(flavor = "multi_thread")]
async fn a_poller_on_a_client_cadence_observes_an_active_container_phase() {
    let emulator = Emulator::new();
    let role = given_job_role(&emulator, &every_s3_action_on(BUCKET)).await;
    emulator.given_bucket(BUCKET).await;

    let rows = rows();
    // Every job is accepted before anything is polled, so one poll loop at a client's cadence
    // walks all of them: the rows share the loop instead of each paying its own wait.
    for row in &rows {
        for name in row.channels {
            emulator
                .given_object(
                    BUCKET,
                    &format!("{}/{name}/data.csv", row.name),
                    &training_records(16),
                )
                .await;
        }
        emulator
            .sagemaker_ok(row.case, "CreateTrainingJob", &training_body(row, &role))
            .await;
    }

    let mut seen: Vec<Vec<Seen>> = rows.iter().map(|_| Vec::new()).collect();
    let mut settled: Vec<Option<Value>> = rows.iter().map(|_| None).collect();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    while settled.iter().any(Option::is_none) {
        for (index, row) in rows.iter().enumerate() {
            if settled[index].is_some() {
                continue;
            }
            let read = emulator
                .sagemaker_ok(
                    row.case,
                    "DescribeTrainingJob",
                    &json!({ "TrainingJobName": row.name }),
                )
                .await;
            let status = str_field(&read, "TrainingJobStatus");
            seen[index].push((status.clone(), str_field(&read, "SecondaryStatus")));
            if TERMINAL.contains(&status.as_str()) {
                settled[index] = Some(read);
            }
        }
        if settled.iter().all(Option::is_some) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the poll loop\": every job must reach a terminal status, having seen {seen:?}"
        );
        tokio::time::sleep(POLL_CADENCE).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let reads = &seen[index];
        let read = settled[index]
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the poll loop left no terminal read"));

        // End to end: a poller on a client's cadence looked WHILE the container was working.
        let observed: Vec<&Seen> = reads
            .iter()
            .filter(|(status, secondary)| {
                status == "InProgress" && ACTIVE_CONTAINER.contains(&secondary.as_str())
            })
            .collect();
        assert!(
            !observed.is_empty(),
            "case {case:?}: a client polling every {POLL_CADENCE:?} must find the job in one of \
             {ACTIVE_CONTAINER:?} while it is still InProgress, got the reads {reads:?} and the \
             history {:?}",
            history(&read, case)
        );

        // The job's own schedule: every stage it was in is reported with the instants it was in
        // effect over, each entry closing where the next opens.
        let walked = history(&read, case);
        let last = walked.len().saturating_sub(1);
        for (position, (status, start, end)) in walked.iter().enumerate() {
            if position == last {
                continue;
            }
            let left = end.unwrap_or_else(|| {
                panic!("case {case:?}: the {status} entry must report when it ended, got {read}")
            });
            assert!(
                left > *start,
                "case {case:?}: the {status} entry must end after it began, got {left} against \
                 {start} in {read}"
            );
            assert_eq!(
                Some(left),
                walked.get(position + 1).map(|(_, next, _)| *next),
                "case {case:?}: the {status} entry must end where the next one begins, got {read}"
            );
        }

        // Every status the poller reported is a stage the history names, so what a client
        // observed is what the job says it was in.
        for (_, secondary) in reads {
            assert!(
                walked.iter().any(|(status, ..)| status == secondary),
                "case {case:?}: the observed {secondary} must be named by the history, got {:?} \
                 in {read}",
                walked.iter().map(|(status, ..)| status).collect::<Vec<_>>()
            );
        }

        // And the RULE itself, weighed against the instants the JOB reports rather than against
        // how fast this host polled: the stages that run the container are in effect, together,
        // for longer than a client's cadence.
        let opened = walked
            .iter()
            .find(|(status, ..)| ACTIVE_CONTAINER.contains(&status.as_str()))
            .map(|(_, start, _)| *start)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a stage that runs the container in {read}")
            });
        let closed = walked
            .iter()
            .rfind(|(status, ..)| ACTIVE_CONTAINER.contains(&status.as_str()))
            .and_then(|(_, _, end)| *end)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the last container stage to have ended in {read}")
            });
        assert!(
            closed - opened > POLL_CADENCE.as_secs_f64(),
            "case {case:?}: the stages that run the container must be in effect for longer than \
             a client's {POLL_CADENCE:?} cadence, got {} seconds of them in {read}",
            closed - opened
        );
        assert_eq!(
            (
                str_field(&read, "TrainingJobStatus"),
                str_field(&read, "SecondaryStatus"),
            ),
            ("Completed".to_string(), "Completed".to_string()),
            "case {case:?}: a job whose work succeeded ends Completed/Completed, got {read}"
        );
    }
}
