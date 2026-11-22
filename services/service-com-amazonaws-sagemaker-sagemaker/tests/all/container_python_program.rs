// SPDX-License-Identifier: CC0-1.0

//! The PYTHON program a job's container declares, end to end: what a training job and a
//! processing job whose `ContainerEntrypoint` names the interpreter end in, and what that
//! program leaves behind.
//!
//! RULE — a job that runs a container declares the program that container starts
//! (`AlgorithmSpecification$ContainerEntrypoint`, "the entrypoint script for a Docker container
//! used to run a training job. This script takes precedence over the default train processing
//! instructions", and `AppSpecification$ContainerEntrypoint`, "the entrypoint for a container
//! used to run a processing job", each followed by that specification's `ContainerArguments`).
//! SageMaker's own algorithm and framework images are Python environments, so a vector naming
//! the PYTHON INTERPRETER with a program for it is a job whose work IS that Python program: it
//! is RUN, over the data the service places in the container — a channel's objects under
//! `/opt/ml/input/data/<ChannelName>/`, which is where "SageMaker makes the data available" —
//! the job ends in the status that program exited with, and what it wrote to `/opt/ml/model`
//! ("a SageMaker container stores your trained model artifacts in the /opt/ml/model directory.
//! After training has completed, by default, these artifacts are uploaded to your Amazon S3
//! bucket as compressed files") is what the job's model artifact holds.
//!
//! The rows span what the rule generalises over: the FORM the interpreter is named by (the bare
//! name, an absolute path, a version-qualified name), the form the program is given in (a
//! command on the command line, a script file in the container), and the STATUS the program
//! exits with (zero, the status it chose itself, the one a raise ends on, and the one the
//! interpreter reports for a file it could not open).
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the training-run suite's harness
//! ([`crate::training_job_execution`]) rather than a second copy of it. Every wait is a poll
//! for the CONDITION, bounded by a deadline far longer than any span this state gives a job's
//! work.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::sagemaker_job_execution_over_s3::archive_entries;
use crate::training_job_execution::{
    artifact_key, channel, read_and_write, str_field, training_body, Emulator, BUCKET,
};

/// How long a condition is polled for before a case fails, and how often it is looked at. The
/// budget is far longer than any span this state gives a job's work, so it bounds a BROKEN
/// emulator rather than the condition — a Python program is run by an interpreter this process
/// stands up, which takes as long as the host needs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(300);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// The `TrainingJobStatus` and `ProcessingJobStatus` members a job's work has ENDED in.
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The channel every training row below reads, which is the one SageMaker's built-in
/// algorithms take their training data from.
const CHANNEL: &str = "train";

/// The records every training row stages in its channel: four comma-separated rows, which is
/// what the program below counts.
const RECORDS: &str = "1,2\n3,4\n5,6\n7,8\n";

/// The program the completing rows run: it reads every file the service placed in its channel
/// directory and writes what it counted to the model directory.
///
/// It is written compactly because `ContainerArguments`' own member is `@length 0..256`, which
/// is the ceiling a real `python3 -c` program declared in one of these jobs lives under too.
const COUNTING_PROGRAM: &str = "import os\n\
     d='/opt/ml/input/data/train'\n\
     n=0\n\
     for f in sorted(os.listdir(d)):\n\
     \x20n+=len(open(d+'/'+f).read().split())\n\
     os.makedirs('/opt/ml/model',exist_ok=True)\n\
     open('/opt/ml/model/artifact.txt','w').write('rows=%d\\n'%n)\n";

/// What [`COUNTING_PROGRAM`] leaves in the model directory for [`RECORDS`].
const COUNTED: &str = "rows=4\n";

/// Poll `target` until the job has reached a terminal status, and answer that read.
async fn ended(emulator: &Emulator, case: &str, target: &str, body: &Value, member: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.sagemaker_ok(target, body).await;
        if TERMINAL_STATUSES.contains(&str_field(&read, member).as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal {member}; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Poll Amazon S3 until it serves the object `key` holds, and answer its bytes.
async fn object_until(emulator: &Emulator, case: &str, key: &str) -> Vec<u8> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(bytes) = emulator.object_bytes(BUCKET, key).await {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon S3 never served {BUCKET}/{key}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// THE RULE over a TRAINING job: the declared Python program is run over the job's own channel
/// data, the job ends in the status that program exited with, and what the program wrote to the
/// model directory is what the artifact holds.
#[tokio::test]
async fn a_training_job_runs_the_python_program_its_entrypoint_declares() {
    /// One row: the case, the name every resource of the row is created under, the argument
    /// vector the specification declares, the status the job ends in, the exit status its
    /// `FailureReason` has to carry, and the artifact entry it has to leave.
    struct Row {
        case: &'static str,
        name: &'static str,
        entrypoint: Vec<&'static str>,
        arguments: Vec<String>,
        status: &'static str,
        exit_code: Option<i32>,
        entry: Option<(&'static str, &'static str)>,
    }
    let rows = vec![
        Row {
            case: "the bare interpreter name with a command on the command line",
            name: "pythonbare",
            entrypoint: vec!["python3"],
            arguments: vec!["-c".to_string(), COUNTING_PROGRAM.to_string()],
            status: "Completed",
            exit_code: None,
            entry: Some(("artifact.txt", COUNTED)),
        },
        Row {
            case: "the interpreter named by an absolute path",
            name: "pythonpath",
            entrypoint: vec!["/usr/bin/python3"],
            arguments: vec!["-c".to_string(), COUNTING_PROGRAM.to_string()],
            status: "Completed",
            exit_code: None,
            entry: Some(("artifact.txt", COUNTED)),
        },
        Row {
            case: "a version-qualified interpreter",
            name: "pythonqualified",
            entrypoint: vec!["python3.11"],
            arguments: vec!["-c".to_string(), COUNTING_PROGRAM.to_string()],
            status: "Completed",
            exit_code: None,
            entry: Some(("artifact.txt", COUNTED)),
        },
        Row {
            case: "a program that exits a status of its own",
            name: "pythonexit",
            entrypoint: vec!["python3"],
            arguments: vec!["-c".to_string(), "import sys\nsys.exit(3)\n".to_string()],
            status: "Failed",
            exit_code: Some(3),
            entry: None,
        },
        Row {
            case: "a program that raises",
            name: "pythonraise",
            entrypoint: vec!["python3"],
            arguments: vec!["-c".to_string(), "raise ValueError('no data')\n".to_string()],
            status: "Failed",
            exit_code: Some(1),
            entry: None,
        },
        Row {
            case: "a script file the container does not carry",
            name: "pythonnoscript",
            entrypoint: vec!["python3", "/opt/ml/code/train.py"],
            arguments: Vec::new(),
            status: "Failed",
            // What the interpreter itself writes and exits with for a file it cannot open.
            exit_code: Some(2),
            entry: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("sagemaker-python-role", Some(&read_and_write()))
            .await;
        let key = format!("{}/{CHANNEL}/data.csv", row.name);
        emulator.given_object(BUCKET, &key, RECORDS).await;
        let channels = json!([channel(CHANNEL, &format!("s3://{BUCKET}/{key}"), "S3Prefix")]);
        let output = format!("s3://{BUCKET}/{}/output/", row.name);
        let mut body = training_body(row.name, &role_arn, channels, &output);
        body["AlgorithmSpecification"]["ContainerEntrypoint"] =
            Value::Array(row.entrypoint.iter().map(|word| json!(word)).collect());
        if !row.arguments.is_empty() {
            body["AlgorithmSpecification"]["ContainerArguments"] =
                Value::Array(row.arguments.iter().map(|word| json!(word)).collect());
        }

        emulator.sagemaker_ok("CreateTrainingJob", &body).await;
        let read = ended(
            &emulator,
            case,
            "DescribeTrainingJob",
            &json!({ "TrainingJobName": row.name }),
            "TrainingJobStatus",
        )
        .await;
        assert_eq!(
            str_field(&read, "TrainingJobStatus"),
            row.status,
            "case {case:?}: expected the status the program exited with, got {read}"
        );
        match row.exit_code {
            Some(code) => {
                let reason = str_field(&read, "FailureReason");
                assert!(
                    reason.starts_with("AlgorithmError:")
                        && reason.ends_with(&format!("exit code: {code}")),
                    "case {case:?}: expected AWS's AlgorithmError form for exit status {code}, \
                     got {reason:?}"
                );
            }
            None => assert_eq!(
                read.get("FailureReason"),
                None,
                "case {case:?}: a job whose program succeeded carries no FailureReason, got \
                 {read}"
            ),
        }
        if let Some((name, contents)) = row.entry {
            let archive = object_until(&emulator, case, &artifact_key(&output, row.name)).await;
            let entries = archive_entries(&archive);
            let held: Vec<&str> = entries.iter().map(|(entry, _)| entry.as_str()).collect();
            assert_eq!(
                held,
                vec![name],
                "case {case:?}: the artifact holds what the program wrote to the model \
                 directory, got {held:?}"
            );
            let body = entries
                .iter()
                .find(|(entry, _)| entry == name)
                .map(|(_, body)| body.clone())
                .unwrap_or_default();
            assert_eq!(
                body, contents,
                "case {case:?}: {name} must hold what the program computed over its channel"
            );
        }
    }
}

/// THE RULE over a PROCESSING job: the declared Python program is run the same way, and the
/// files it leaves under a declared output's `LocalPath` are uploaded to that output's `S3Uri`
/// — so the two halves of a job's own work meet on one job.
#[tokio::test]
async fn a_processing_job_runs_its_python_program_and_uploads_what_it_wrote() {
    let case = "a python program writing its processing output";
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role_arn = emulator
        .given_role("sagemaker-python-processing-role", Some(&read_and_write()))
        .await;
    let program = "import os\n\
         os.makedirs('/opt/ml/processing/output', exist_ok=True)\n\
         with open('/opt/ml/processing/output/summary.txt', 'w') as handle:\n\
         \x20   handle.write('computed\\n')\n";
    let body = json!({
        "ProcessingJobName": "pythonprocessing",
        "AppSpecification": {
            "ImageUri": "000000000000.dkr.ecr.us-east-1.amazonaws.com/processor:1",
            "ContainerEntrypoint": ["python3"],
            "ContainerArguments": ["-c", program],
        },
        "ProcessingResources": {
            "ClusterConfig": {
                "InstanceType": "ml.m5.large",
                "InstanceCount": 1,
                "VolumeSizeInGB": 10,
            },
        },
        "ProcessingOutputConfig": {
            "Outputs": [{
                "OutputName": "result",
                "S3Output": {
                    "S3Uri": format!("s3://{BUCKET}/python-processing/"),
                    "LocalPath": "/opt/ml/processing/output",
                    "S3UploadMode": "EndOfJob",
                },
            }],
        },
        "RoleArn": role_arn,
    });
    emulator.sagemaker_ok("CreateProcessingJob", &body).await;
    let read = ended(
        &emulator,
        case,
        "DescribeProcessingJob",
        &json!({ "ProcessingJobName": "pythonprocessing" }),
        "ProcessingJobStatus",
    )
    .await;
    assert_eq!(
        str_field(&read, "ProcessingJobStatus"),
        "Completed",
        "case {case:?}: the program exited zero, so the job completes, got {read}"
    );
    let held = object_until(&emulator, case, "python-processing/summary.txt").await;
    assert_eq!(
        String::from_utf8_lossy(&held),
        "computed\n",
        "case {case:?}: the uploaded object must hold what the program wrote"
    );
}
