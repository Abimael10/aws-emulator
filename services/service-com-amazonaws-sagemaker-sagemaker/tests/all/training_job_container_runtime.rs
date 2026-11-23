// SPDX-License-Identifier: CC0-1.0

//! How long a training job's work TAKES, and what that decides: the runtime the job reports,
//! the runtime cap that can end it, and how long the stages of the work stay observable.
//!
//! RULE ONE — a job's RUNTIME is the time its algorithm container's work took. The container is
//! performed in process and spends none of this host's clock on a wait its program asks for, so
//! the time the work took is the time that program STATED it took, and it is what
//! `DescribeTrainingJob$TrainingTimeInSeconds` (the span between `TrainingStartTime` and
//! `TrainingEndTime`) reports.
//!
//! RULE TWO — `StoppingCondition$MaxRuntimeInSeconds` bounds that runtime: "the maximum length
//! of time, in seconds, that a training job … can run. If the job does not complete during this
//! time, SageMaker ends the job". So a job whose work does not fit inside the cap is ENDED on
//! the cap — the `Stopped`/`MaxRuntimeExceeded` pair `DescribeTrainingJob$SecondaryStatus`
//! groups together — having been granted the runtime it asked for and cut off after it, while a
//! job whose work fits inside the same cap completes with no `MaxRuntimeExceeded` attribution
//! anywhere. `ListTrainingJobs` reports the same status the describe does for both.
//!
//! RULE THREE — the stages of that work stay OBSERVABLE while the job is `InProgress`: a client
//! polling from the instant its create answers observes the job in an active-container stage
//! (`Downloading`, `Training`, `Uploading`) and not only in the `Starting` the job is accepted
//! in, and `SecondaryStatusTransitions` records every stage it was in, in order. The stage that
//! runs the CONTAINER is in effect for as long as that container's own work took, so a job whose
//! container really works for a while is observable in it for about as long.
//!
//! The rows span what the rules generalise over: how much work the container states (none, a
//! few seconds, and far more than any cap), where the cap falls relative to that work (past it,
//! inside it, at the member's modelled minimum, at the value its own doc names as the default,
//! and absent), and whether the job declares a container program at all.
//!
//! Nothing here sleeps for a fixed time or asserts on the wall clock: every wait is a poll that
//! looks again until the condition holds, under a budget far longer than the spans this state
//! gives a job's work, and every claim about a duration is weighed against the instants the JOB
//! itself reports.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the training-run suite's harness
//! ([`crate::training_job_execution`]) rather than a second copy of it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    channel, csv_records, read_and_write, str_field, training_body, Emulator, BUCKET,
};

/// How long a job is polled for the status it has to reach, and how often it is looked at.
/// What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a case
/// the clock would have served.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const SETTLE_POLL: Duration = Duration::from_millis(200);

/// The `TrainingJobStatus` members a job's work has ENDED in
/// (`smithy_query(query="shape",shape="TrainingJobStatus")`).
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The `SecondaryStatus` members of a job whose ALGORITHM CONTAINER is at work, i.e. the
/// non-terminal stages that follow the `Starting` a job is accepted in
/// (`smithy_query(query="doc",shape="DescribeTrainingJobResponse",member="SecondaryStatus")`).
const ACTIVE_STAGES: [&str; 3] = ["Downloading", "Training", "Uploading"];

/// The `SecondaryStatus` member that attributes a job's end to its runtime cap.
const MAX_RUNTIME_EXCEEDED: &str = "MaxRuntimeExceeded";

/// The grace SageMaker gives a capped job's container to save its artifacts: "SageMaker sends a
/// SIGTERM signal, which delays job termination for 120 seconds"
/// (`StoppingCondition$MaxRuntimeInSeconds`'s own doc), so a job the cap ended is over no later
/// than the cap plus that window.
const SIGTERM_GRACE_SECONDS: f64 = 120.0;

/// Which ENGINE performs a row's container program, i.e. which form the declared argument
/// vector takes: the two forms `AlgorithmSpecification$ContainerEntrypoint` can name, each
/// stating the time its work takes in its own way (a shell's `sleep`, a Python program's own
/// wait).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Program {
    /// `/bin/sh -c "sleep <seconds>"`.
    Shell,
    /// `python3 -c "import time; time.sleep(<seconds>)"`, the interpreter the SageMaker images
    /// provide.
    Python,
}

/// One row of the runtime-cap table.
struct Row {
    case: &'static str,
    /// The name every resource of the row is created under.
    name: &'static str,
    /// How many seconds of work the job's container program states it did — `None` for a row
    /// that declares no program at all, whose work is the default train instructions'.
    works_for: Option<u64>,
    /// Which program form states it ([`Program`]).
    program: Program,
    /// `StoppingCondition.MaxRuntimeInSeconds`, or `None` for a row that states no stopping
    /// condition.
    cap: Option<i64>,
    /// The `(TrainingJobStatus, SecondaryStatus)` pair the job must end in.
    terminal: (&'static str, &'static str),
}

/// The table of RULE ONE and RULE TWO.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "work far past the cap",
            name: "runtime-overrun",
            works_for: Some(1_800),
            program: Program::Shell,
            cap: Some(300),
            terminal: ("Stopped", MAX_RUNTIME_EXCEEDED),
        },
        Row {
            case: "the same work past the same cap, stated by a Python program",
            name: "runtime-overrun-python",
            works_for: Some(1_800),
            program: Program::Python,
            cap: Some(300),
            terminal: ("Stopped", MAX_RUNTIME_EXCEEDED),
        },
        Row {
            case: "a cap at the member's modelled minimum",
            name: "runtime-minimum-cap",
            works_for: Some(5),
            program: Program::Shell,
            cap: Some(1),
            terminal: ("Stopped", MAX_RUNTIME_EXCEEDED),
        },
        Row {
            case: "work well inside the same cap",
            name: "runtime-inside-cap",
            works_for: Some(5),
            program: Program::Shell,
            cap: Some(300),
            terminal: ("Completed", "Completed"),
        },
        Row {
            case: "a Python program's work well inside the same cap",
            name: "runtime-inside-cap-python",
            works_for: Some(5),
            program: Program::Python,
            cap: Some(300),
            terminal: ("Completed", "Completed"),
        },
        Row {
            case: "work inside the cap the member documents as its default",
            name: "runtime-default-cap",
            works_for: Some(1_800),
            program: Program::Shell,
            cap: Some(86_400),
            terminal: ("Completed", "Completed"),
        },
        Row {
            case: "no cap at all",
            name: "runtime-no-cap",
            works_for: Some(1_800),
            program: Program::Shell,
            cap: None,
            terminal: ("Completed", "Completed"),
        },
        Row {
            case: "no declared program, so no work of its own",
            name: "runtime-no-program",
            works_for: None,
            program: Program::Shell,
            cap: Some(86_400),
            terminal: ("Completed", "Completed"),
        },
    ]
}

/// Stage one row's channel and submit its job, with the program its container runs and the
/// stopping condition it asks for.
async fn given_job(emulator: &Emulator, role_arn: &str, row: &Row) {
    let key = format!("{}/train/data.csv", row.name);
    emulator
        .given_object(BUCKET, &key, &csv_records(8, 2))
        .await;
    let channels = json!([channel("train", &format!("s3://{BUCKET}/{key}"), "S3Prefix")]);
    let mut body = training_body(
        row.name,
        role_arn,
        channels,
        &format!("s3://{BUCKET}/{}/output/", row.name),
    );
    // The program the container runs: one that takes the time the row's work states and then
    // exits zero, which is a container that worked for that long and succeeded — stated the way
    // the row's engine states it.
    if let Some(seconds) = row.works_for {
        let (entrypoint, arguments) = match row.program {
            Program::Shell => (
                json!(["/bin/sh", "-c"]),
                json!([format!("sleep {seconds}")]),
            ),
            Program::Python => (
                json!(["python3", "-c"]),
                json!([format!("import time\ntime.sleep({seconds})\n")]),
            ),
        };
        body["AlgorithmSpecification"]["ContainerEntrypoint"] = entrypoint;
        body["AlgorithmSpecification"]["ContainerArguments"] = arguments;
    }
    match row.cap {
        Some(seconds) => body["StoppingCondition"] = json!({ "MaxRuntimeInSeconds": seconds }),
        None => {
            body.as_object_mut()
                .expect("the request body is an object")
                .remove("StoppingCondition");
        }
    }
    emulator.sagemaker_ok("CreateTrainingJob", &body).await;
}

/// Poll `job` until it reaches a terminal `TrainingJobStatus`, and answer that read.
async fn ended(emulator: &Emulator, job: &str, case: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        if TERMINAL_STATUSES.contains(&str_field(&read, "TrainingJobStatus").as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// A timestamp member of a read, which this protocol renders as epoch seconds.
fn instant(read: &Value, member: &str, case: &str) -> f64 {
    read.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {read}"))
}

/// Every `SecondaryStatus` the job's history names, in the order it reports them.
fn history(read: &Value) -> Vec<String> {
    read.get("SecondaryStatusTransitions")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| str_field(entry, "Status"))
                .collect()
        })
        .unwrap_or_default()
}

/// RULE ONE and RULE TWO: the cap is measured against the time the job's CONTAINER worked for,
/// so a job whose work does not fit inside it is ended on the cap and one whose work fits
/// completes — and the runtime the job reports is the work's own.
#[tokio::test]
async fn the_runtime_cap_is_measured_against_the_work_the_container_did() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role_arn = emulator
        .given_role("sagemaker-runtime-role", Some(&read_and_write()))
        .await;

    // Every job is submitted FIRST, so the clock reveals all of them at once rather than one
    // after another: what each row asserts is the schedule of its own job.
    let rows = rows();
    for row in &rows {
        given_job(&emulator, &role_arn, row).await;
    }

    for row in &rows {
        let case = row.case;
        let read = ended(&emulator, row.name, case).await;
        let (status, secondary) = row.terminal;
        assert_eq!(
            (
                str_field(&read, "TrainingJobStatus"),
                str_field(&read, "SecondaryStatus"),
            ),
            (status.to_string(), secondary.to_string()),
            "case {case:?}: expected the job to end {status}/{secondary}, got {read}"
        );

        let created_at = instant(&read, "CreationTime", case);
        let started_at = instant(&read, "TrainingStartTime", case);
        let ended_at = instant(&read, "TrainingEndTime", case);
        if secondary == MAX_RUNTIME_EXCEEDED {
            let cap = row.cap.expect("a capped row states its cap") as f64;
            // The job was granted the runtime it asked for, and cut off after it.
            assert!(
                ended_at - created_at >= cap,
                "case {case:?}: expected the job to be granted its {cap}s of runtime, got \
                 {} seconds in {read}",
                ended_at - created_at
            );
            assert!(
                ended_at - started_at <= cap + SIGTERM_GRACE_SECONDS,
                "case {case:?}: expected the job to be cut off within {cap}s plus the SIGTERM \
                 grace, got {} seconds in {read}",
                ended_at - started_at
            );
            assert!(
                read.get("ModelArtifacts")
                    .and_then(|artifacts| artifacts.get("S3ModelArtifacts"))
                    .is_some(),
                "case {case:?}: the describe still reports where the artifacts would be, got \
                 {read}"
            );
        } else {
            // A job the cap did not touch carries no attribution to it anywhere: not in the
            // secondary status, not in the history, and not in a failure reason.
            let trail = history(&read).join(",");
            let reason = str_field(&read, "FailureReason");
            assert!(
                !trail.contains(MAX_RUNTIME_EXCEEDED) && !reason.contains(MAX_RUNTIME_EXCEEDED),
                "case {case:?}: expected no {MAX_RUNTIME_EXCEEDED} attribution, got \
                 {trail:?} and {reason:?}"
            );
            // RULE ONE: the runtime reported is the time the container's work took, so a job
            // whose program stated more work than this emulator spends revealing it still
            // reports the work's own span.
            let reported = read
                .get("TrainingTimeInSeconds")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let worked_for = i64::try_from(row.works_for.unwrap_or_default()).unwrap_or_default();
            assert!(
                reported >= worked_for,
                "case {case:?}: expected a reported runtime of at least the {worked_for}s the \
                 container worked, got {reported} in {read}"
            );
            assert!(
                (ended_at - started_at - reported as f64).abs() <= 1.0,
                "case {case:?}: the reported runtime is the span between the start and the end \
                 instants, got {reported} against {} in {read}",
                ended_at - started_at
            );
        }

        // `ListTrainingJobs` summarises the same record, so it agrees with the describe.
        let listed = emulator
            .sagemaker_ok(
                "ListTrainingJobs",
                &json!({ "NameContains": row.name, "MaxResults": 10 }),
            )
            .await;
        let summary = listed
            .get("TrainingJobSummaries")
            .and_then(Value::as_array)
            .and_then(|summaries| {
                summaries
                    .iter()
                    .find(|summary| str_field(summary, "TrainingJobName") == row.name)
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: expected {} in {listed}", row.name));
        assert_eq!(
            (
                str_field(&summary, "TrainingJobStatus"),
                str_field(&summary, "SecondaryStatus"),
            ),
            (status.to_string(), secondary.to_string()),
            "case {case:?}: the listing agrees with the describe, got {summary}"
        );
    }
}

/// RULE THREE: a client polling from the instant its create answers observes the job in an
/// active-container stage before the terminal status, and the history names every stage it was
/// in.
///
/// The rows differ along how much work the container states, which is what the stage that runs
/// it is in effect for: none at all, a few seconds, and no declared program whatever.
#[tokio::test]
async fn the_stages_of_the_work_stay_observable_to_a_poller() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role_arn = emulator
        .given_role("sagemaker-observable-role", Some(&read_and_write()))
        .await;

    // (case, the job's name, the seconds its container works for, which engine states it)
    let rows: [(&str, &str, Option<u64>, Program); 4] = [
        (
            "a container that works for a while",
            "observable-working",
            Some(6),
            Program::Shell,
        ),
        (
            "a Python container that works for a while",
            "observable-working-python",
            Some(6),
            Program::Python,
        ),
        (
            "a container that states no work",
            "observable-instant",
            Some(0),
            Program::Shell,
        ),
        (
            "no declared program at all",
            "observable-default",
            None,
            Program::Shell,
        ),
    ];
    for (case, name, works_for, program) in rows {
        let row = Row {
            case,
            name,
            works_for,
            program,
            cap: Some(86_400),
            terminal: ("Completed", "Completed"),
        };
        given_job(&emulator, &role_arn, &row).await;
    }

    // Every job is polled from the instant its create answered, each read recorded, until all
    // of them have reached a terminal status.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut observed: Vec<Vec<String>> = vec![Vec::new(); rows.len()];
    let mut settled: Vec<Option<Value>> = vec![None; rows.len()];
    while settled.iter().any(Option::is_none) {
        for (index, (case, name, _, _)) in rows.iter().enumerate() {
            if settled[index].is_some() {
                continue;
            }
            let read = emulator
                .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
                .await;
            let status = str_field(&read, "TrainingJobStatus");
            let secondary = str_field(&read, "SecondaryStatus");
            if status == "InProgress" {
                observed[index].push(secondary);
            } else {
                assert!(
                    TERMINAL_STATUSES.contains(&status.as_str()),
                    "case {case:?}: expected a modelled status, got {read}"
                );
                settled[index] = Some(read);
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected every job to settle; still running: {settled:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }

    for (index, (case, name, works_for, _)) in rows.iter().enumerate() {
        let seen = &observed[index];
        assert!(
            seen.iter().any(|status| ACTIVE_STAGES.contains(&status.as_str())),
            "case {case:?}: expected the poller to observe one of {ACTIVE_STAGES:?} while \
             {name} was InProgress, got {seen:?}"
        );
        let read = settled[index]
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected {name} to have settled"));
        assert_eq!(
            str_field(&read, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: the job's work succeeded, so it completes, got {read}"
        );
        // The history names every stage the job was in, in the canonical order, and the
        // terminal status last.
        let trail = history(&read);
        let expected: Vec<String> = ["Starting", "Downloading", "Training", "Uploading", "Completed"]
            .iter()
            .map(|status| (*status).to_string())
            .collect();
        assert_eq!(
            trail, expected,
            "case {case:?}: expected the canonical history {expected:?}, got {trail:?}"
        );
        // Every stage the poller saw is one the history names, so nothing was observed that the
        // job does not report having been in.
        for status in seen {
            assert!(
                trail.contains(status),
                "case {case:?}: the stage {status:?} a poll observed is in the history, got \
                 {trail:?}"
            );
        }
        // RULE ONE again, from the other side: the runtime reported covers the work the
        // container stated it did.
        let reported = read
            .get("TrainingTimeInSeconds")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let worked_for = i64::try_from(works_for.unwrap_or_default()).unwrap_or_default();
        assert!(
            reported >= worked_for,
            "case {case:?}: expected a reported runtime of at least {worked_for}s, got \
             {reported} in {read}"
        );
    }
}
