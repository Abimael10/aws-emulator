// SPDX-License-Identifier: CC0-1.0

//! The PROGRAM a job's container runs, end to end: what a training job and a processing job
//! end in once the argument vector their specification declares has exited.
//!
//! RULE — a job that runs a container declares the program that container starts
//! (`AlgorithmSpecification$ContainerEntrypoint`, "the entrypoint script for a Docker
//! container used to run a training job. This script takes precedence over the default train
//! processing instructions", and `AppSpecification$ContainerEntrypoint`, "the entrypoint for a
//! container used to run a processing job", each followed by that specification's
//! `ContainerArguments`), so the status such a job ends in is the status that program exits
//! with: zero completes the job, and non-zero fails it with `FailureReason` in the form AWS
//! reports for a container that exited on its own — `AlgorithmError: <stderr tail>, exit code:
//! <status>`.
//!
//! A program this emulator does not perform at all CLAIMS NOTHING: no exit status is reported
//! for it, so such a job fails with a reason naming what was not performed rather than in the
//! `AlgorithmError` form, and reports no instant its work started.
//!
//! The rows span what the rule generalises over: the FORM of the declared vector (a bare
//! utility, a utility named by absolute path, a shell performing a program given as one
//! argument, a program nothing provides, and no declared program at all) and the STATUS it
//! exits with (zero, one, and another non-zero status).
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
/// the clock would have served; the budget is far longer than any span this state gives a
/// job's work.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// The members of `TrainingJobStatus` and of `ProcessingJobStatus` a job's work has ENDED in
/// (`smithy_query(query="shape",shape="TrainingJobStatus")` and `shape="ProcessingJobStatus"`
/// list the same three beside the transient ones).
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// One row of the container-program table.
struct Row {
    case: &'static str,
    /// The name every resource of the row is created under.
    name: &'static str,
    /// `ContainerEntrypoint`, as the request declares it — `None` declares none at all.
    entrypoint: Option<&'static [&'static str]>,
    /// `ContainerArguments`, as the request declares them.
    arguments: Option<&'static [&'static str]>,
    /// The status the job must end in.
    status: &'static str,
    /// The exit status the `FailureReason` has to carry, for a row whose program RAN and
    /// failed; `None` for a row whose program either succeeded or was never performed.
    exit_code: Option<i32>,
    /// Whether this emulator performs the declared program at all. A row it performs none of
    /// claims no exit status and reports no start instant.
    performed: bool,
}

/// The table: one row per form of declared program, each with the status it exits with.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no declared program: the image's own entrypoint, which nothing here starts",
            name: "program-absent",
            entrypoint: None,
            arguments: None,
            status: "Failed",
            exit_code: None,
            performed: false,
        },
        Row {
            case: "a utility that exits zero",
            name: "program-true",
            entrypoint: Some(&["true"]),
            arguments: None,
            status: "Completed",
            exit_code: None,
            performed: true,
        },
        Row {
            case: "the same utility named by its absolute path",
            name: "program-bin-true",
            entrypoint: Some(&["/usr/bin/true"]),
            arguments: None,
            status: "Completed",
            exit_code: None,
            performed: true,
        },
        Row {
            case: "a utility that exits one, named by its absolute path",
            name: "program-bin-false",
            entrypoint: Some(&["/bin/false"]),
            arguments: None,
            status: "Failed",
            exit_code: Some(1),
            performed: true,
        },
        Row {
            case: "a shell performing a program given as one argument",
            name: "program-shell-zero",
            entrypoint: Some(&["/bin/sh", "-c"]),
            arguments: Some(&["exit 0"]),
            status: "Completed",
            exit_code: None,
            performed: true,
        },
        Row {
            case: "the same shell exiting another non-zero status",
            name: "program-shell-nonzero",
            entrypoint: Some(&["/bin/sh", "-c"]),
            arguments: Some(&["exit 7"]),
            status: "Failed",
            exit_code: Some(7),
            performed: true,
        },
        Row {
            case: "a program this emulator performs none of",
            name: "program-unprovided",
            entrypoint: Some(&["/opt/ml/code/train.py"]),
            arguments: None,
            status: "Failed",
            exit_code: None,
            performed: false,
        },
    ]
}

/// The declared vector as a JSON list, or absent.
fn declared(words: Option<&'static [&'static str]>) -> Option<Value> {
    words.map(|words| Value::Array(words.iter().map(|word| json!(word)).collect()))
}

/// Poll `target` until the job has reached a terminal status, and answer that read.
async fn ended(emulator: &Emulator, target: &str, body: &Value, status_member: &str, case: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.sagemaker_ok(target, body).await;
        let status = str_field(&read, status_member);
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal {status_member}; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Assert the terminal status and the `FailureReason` the row's outcome carries: the
/// `AlgorithmError` form for a program that RAN and exited non-zero — fixed text around the
/// status it ended with — and, for a program this emulator performed none of, a reason that
/// claims no exit status at all.
fn assert_outcome(read: &Value, row: &Row, status_member: &str) {
    let case = row.case;
    assert_eq!(
        str_field(read, status_member),
        row.status,
        "case {case:?}: expected the status the program exited with, got {read}"
    );
    let reason = str_field(read, "FailureReason");
    match (row.exit_code, row.performed) {
        (Some(code), _) => assert_eq!(
            reason,
            format!("AlgorithmError: , exit code: {code}"),
            "case {case:?}: expected AWS's AlgorithmError form for exit status {code}, got \
             {read}"
        ),
        (None, true) => assert_eq!(
            read.get("FailureReason"),
            None,
            "case {case:?}: a job whose program succeeded carries no FailureReason, got {read}"
        ),
        (None, false) => assert!(
            !reason.is_empty()
                && !reason.contains("AlgorithmError")
                && !reason.contains("exit code"),
            "case {case:?}: a job whose program was never performed must say so and claim no \
             exit status, got {reason:?}"
        ),
    }
}

/// THE RULE, over a training job: the declared program decides the job's terminal status, and
/// a job whose program succeeded still reports the artifact its output location promises.
#[tokio::test]
async fn a_training_job_ends_in_the_status_its_declared_program_exits_with() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("sagemaker-program-role", Some(&read_and_write()))
            .await;
        let key = format!("{}/train/data.csv", row.name);
        emulator
            .given_object(BUCKET, &key, &csv_records(12, 2))
            .await;
        let channels = json!([channel("train", &format!("s3://{BUCKET}/{key}"), "S3Prefix")]);
        let mut body = training_body(
            row.name,
            &role_arn,
            channels,
            &format!("s3://{BUCKET}/{}/output/", row.name),
        );
        // A training job's default train instructions are SageMaker's own, so a request that
        // declares no entrypoint is the row where the algorithm itself trains.
        if let Some(entrypoint) = declared(row.entrypoint) {
            body["AlgorithmSpecification"]["ContainerEntrypoint"] = entrypoint;
        }
        if let Some(arguments) = declared(row.arguments) {
            body["AlgorithmSpecification"]["ContainerArguments"] = arguments;
        }
        // The row with no declared program is the one the algorithm's own default train
        // instructions serve, so it completes: only a DECLARED program takes precedence over
        // them.
        let expected = Row {
            status: if row.entrypoint.is_none() {
                "Completed"
            } else {
                row.status
            },
            exit_code: if row.entrypoint.is_none() {
                None
            } else {
                row.exit_code
            },
            performed: row.entrypoint.is_none() || row.performed,
            ..row
        };

        emulator.sagemaker_ok("CreateTrainingJob", &body).await;
        let read = ended(
            &emulator,
            "DescribeTrainingJob",
            &json!({ "TrainingJobName": expected.name }),
            "TrainingJobStatus",
            case,
        )
        .await;
        assert_outcome(&read, &expected, "TrainingJobStatus");
        if expected.status == "Completed" {
            assert!(
                read.pointer("/ModelArtifacts/S3ModelArtifacts")
                    .and_then(Value::as_str)
                    .is_some_and(|uri| uri.starts_with(&format!("s3://{BUCKET}/"))),
                "case {case:?}: a completed job names the artifact it wrote, got {read}"
            );
        }
    }
}

/// THE RULE, over a processing job: the declared program decides the job's terminal status,
/// and a job whose program never started reports no start instant for it.
#[tokio::test]
async fn a_processing_job_ends_in_the_status_its_declared_program_exits_with() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let role_arn = emulator.given_role("sagemaker-processing-role", None).await;
        let mut app = json!({
            "ImageUri": format!("000000000000.dkr.ecr.us-east-1.amazonaws.com/{}:1", row.name),
        });
        if let Some(entrypoint) = declared(row.entrypoint) {
            app["ContainerEntrypoint"] = entrypoint;
        }
        if let Some(arguments) = declared(row.arguments) {
            app["ContainerArguments"] = arguments;
        }
        let body = json!({
            "ProcessingJobName": row.name,
            "AppSpecification": app,
            "ProcessingResources": {
                "ClusterConfig": {
                    "InstanceType": "ml.m5.large",
                    "InstanceCount": 1,
                    "VolumeSizeInGB": 10,
                },
            },
            "RoleArn": role_arn,
        });

        emulator.sagemaker_ok("CreateProcessingJob", &body).await;
        let read = ended(
            &emulator,
            "DescribeProcessingJob",
            &json!({ "ProcessingJobName": row.name }),
            "ProcessingJobStatus",
            case,
        )
        .await;
        assert_outcome(&read, &row, "ProcessingJobStatus");
        // "The time at which the processing job started": a job whose program ran reports it,
        // and one whose program was never performed reports none.
        let started = read.get("ProcessingStartTime").is_some();
        assert_eq!(
            started, row.performed,
            "case {case:?}: ProcessingStartTime belongs to a job whose program ran, got {read}"
        );
    }
}

/// RULE: the environment the job declares is the environment its program sees, so a program
/// branching on one of its variables ends the job on the status that branch takes.
///
/// The rows differ along the VALUE the environment carries, which is what the program reads.
#[tokio::test]
async fn the_declared_environment_steers_the_program() {
    // (case, the value `MODE` carries, the status the job ends in, the exit status reported)
    let rows: [(&str, &str, &str, Option<i32>); 3] = [
        ("the branch that exits zero", "ok", "Completed", None),
        ("the branch that exits one", "fail", "Failed", Some(1)),
        ("a value neither branch names", "other", "Completed", None),
    ];
    for (case, mode, status, exit_code) in rows {
        let emulator = Emulator::new();
        let role_arn = emulator.given_role("sagemaker-environment-role", None).await;
        let name = format!("environment-{mode}");
        let body = json!({
            "ProcessingJobName": name,
            "AppSpecification": {
                "ImageUri": "000000000000.dkr.ecr.us-east-1.amazonaws.com/env:1",
                "ContainerEntrypoint": ["/bin/sh", "-c"],
                "ContainerArguments": ["if [ \"$MODE\" = fail ]; then exit 1; fi"],
            },
            "Environment": { "MODE": mode },
            "ProcessingResources": {
                "ClusterConfig": {
                    "InstanceType": "ml.m5.large",
                    "InstanceCount": 1,
                    "VolumeSizeInGB": 10,
                },
            },
            "RoleArn": role_arn,
        });
        emulator.sagemaker_ok("CreateProcessingJob", &body).await;
        let read = ended(
            &emulator,
            "DescribeProcessingJob",
            &json!({ "ProcessingJobName": name }),
            "ProcessingJobStatus",
            case,
        )
        .await;
        assert_eq!(
            str_field(&read, "ProcessingJobStatus"),
            status,
            "case {case:?}: expected the status the environment selected, got {read}"
        );
        match exit_code {
            Some(code) => assert_eq!(
                str_field(&read, "FailureReason"),
                format!("AlgorithmError: , exit code: {code}"),
                "case {case:?}: expected the AlgorithmError form, got {read}"
            ),
            None => assert_eq!(
                read.get("FailureReason"),
                None,
                "case {case:?}: a job whose program succeeded carries no FailureReason, got \
                 {read}"
            ),
        }
    }
}
