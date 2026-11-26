// SPDX-License-Identifier: CC0-1.0

//! What the SERVICE gives a job's container before and around the program it runs — driven end
//! to end over the real wire path (SageMaker, Amazon S3 and IAM on ONE shared engine, the way
//! the production router mounts every service), so what is asserted is what a client sees.
//!
//! RULE ONE — the directories the platform prepares EXIST when the entrypoint is invoked,
//! whether or not any object was placed in one. `ProcessingS3Output$LocalPath` is "the local
//! path of a directory where you want Amazon SageMaker to upload its contents to Amazon S3 …
//! This directory will be created by the platform and exist when your container's entrypoint
//! is invoked", `ProcessingS3Input$LocalPath` is "the local path in your container where you
//! want Amazon SageMaker to write input data to" and "must begin with `/opt/ml/processing/`",
//! and a training job's own tree is the same promise: "SageMaker makes the data available in
//! the `/opt/ml/input/data/<channel_name>` directory" and "a SageMaker container stores your
//! trained model artifacts in the /opt/ml/model directory". So a program that LISTS one of
//! those directories finds it, and a job whose program does that completes.
//!
//! RULE TWO — the container runs AS the job's execution role: "the Amazon Resource Name of an
//! IAM role that Amazon SageMaker can assume to perform tasks on your behalf"
//! (`CreateProcessingJob$RoleArn`, `CreateTrainingJob$RoleArn`). So an AWS call the program
//! makes itself is authorized as that role — it succeeds where the role is granted the action
//! and the object it wrote is really there afterwards, and the program's own call FAILS where
//! the role is not, which fails the job in the `AlgorithmError` form a container that exited
//! non-zero carries.
//!
//! RULE THREE — a job leg Amazon S3 refuses reports the MESSAGE Amazon S3 answered with: the
//! `FailureReason` carries that message in the error envelope naming the service, the HTTP
//! status and the error code (Amazon S3's list of error responses pairs the code
//! `AccessDenied` with the message "Access Denied", HTTP 403), so an operator reading the
//! reason for an authorization failure reads why the call was refused rather than the code
//! twice.
//!
//! RULE FOUR — every create that resolves an input data source refuses one whose location
//! names no object the execution role can list, with the one sentence that states the whole
//! diagnosis: `CreateProcessingJob` asks it per SageMaker-managed `S3Input` exactly as
//! `CreateTrainingJob` asks it per channel. The clause naming what Amazon S3 answered belongs
//! to a listing Amazon S3 REFUSED; a location Amazon S3 itself answered nothing for carries the
//! sentence alone.
//!
//! Every wait here is a poll for the CONDITION, never a pause of a fixed length, bounded by a
//! deadline far longer than any span this state gives a job's work — so it bounds a BROKEN
//! emulator and not the condition.

use std::time::Instant;

use serde_json::{json, Value};

use super::model_artifact_admission::{
    error_code, message, role_arn, str_field, Emulator, ACCOUNT, REGION,
};
use super::sagemaker_job_execution_over_s3::{
    allow, channel, every_s3_action_on, given_job_role, listed_keys, look_again, object_bytes,
    own_image, ROLE, SETTLE_BUDGET,
};

/// The `ProcessingJobStatus` / `TrainingJobStatus` members a job's work has ENDED in.
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The bucket every row of these tables reads and writes.
const BUCKET: &str = "container-contract";

/// A Python program that answers zero only when every directory named on its command line is a
/// directory of the container's own storage — through each of the readings a program reaches a
/// directory by: `os.listdir`, `os.scandir`, `os.stat` and `pathlib`.
///
/// Kept inside `ContainerArguments`' own member length (`@length max 256`), which is what a
/// request may carry per word.
const LIST_DIRECTORIES: &str = "import os,sys\n\
                                from pathlib import Path\n\
                                for d in sys.argv[1:]:\n\
                                \x20l=sorted(os.listdir(d))\n\
                                \x20with os.scandir(d) as s: assert sorted(e.name for e in s)==l\n\
                                \x20assert os.stat(d).st_size==0\n\
                                \x20assert sorted(p.name for p in Path(d).iterdir())==l\n";

/// A Python program that copies every file of its input directory into its output directory
/// through `pathlib`'s own text readings, which is how a program that names no encoding reads
/// and writes a text file.
///
/// Kept inside `ContainerArguments`' own member length (`@length max 256`).
const COPY_WITH_PATHLIB: &str = "from pathlib import Path\n\
                                 i=Path('/opt/ml/processing/input/data')\n\
                                 o=Path('/opt/ml/processing/output')\n\
                                 for p in i.iterdir():\n\
                                 \x20(o/p.name).write_text(p.read_text())\n";

/// A Python program that writes one object with the container's OWN AWS SDK call, as the job's
/// execution role.
const WRITE_WITH_THE_SDK: &str = "import boto3, os\n\
                                  s3 = boto3.client('s3')\n\
                                  s3.put_object(\n\
                                  \x20   Bucket=os.environ['OUT_BUCKET'],\n\
                                  \x20   Key=os.environ['OUT_KEY'],\n\
                                  \x20   Body=os.environ['OUT_BODY'].encode('utf-8'),\n\
                                  )\n";

/// What [`WRITE_WITH_THE_SDK`] writes, and where.
const WRITTEN_KEY: &str = "from-the-container/report.txt";
/// See [`WRITTEN_KEY`].
const WRITTEN_BODY: &str = "written by the container's own program";

/// A `CreateProcessingJob` body running `program` in the contained Python interpreter, with the
/// inputs, outputs and environment declared.
fn processing_request(
    name: &str,
    role: &str,
    program: &str,
    arguments: &[&str],
    inputs: Value,
    outputs: Value,
    environment: Value,
) -> Value {
    let mut vector = vec!["-c".to_string(), program.to_string()];
    vector.extend(arguments.iter().map(|word| (*word).to_string()));
    let mut body = json!({
        "ProcessingJobName": name,
        "AppSpecification": {
            "ImageUri": own_image(),
            "ContainerEntrypoint": ["python3"],
            "ContainerArguments": vector,
        },
        "ProcessingResources": {
            "ClusterConfig": {
                "InstanceType": "ml.m5.large",
                "InstanceCount": 1,
                "VolumeSizeInGB": 10,
            },
        },
        "RoleArn": role,
    });
    if inputs.as_array().is_some_and(|inputs| !inputs.is_empty()) {
        body["ProcessingInputs"] = inputs;
    }
    if outputs.as_array().is_some_and(|outputs| !outputs.is_empty()) {
        body["ProcessingOutputConfig"] = json!({ "Outputs": outputs });
    }
    if environment.as_object().is_some_and(|held| !held.is_empty()) {
        body["Environment"] = environment;
    }
    body
}

/// One declared `ProcessingInput` over `S3Input`.
fn s3_input(name: &str, uri: &str, directory: &str) -> Value {
    json!({
        "InputName": name,
        "S3Input": {
            "S3Uri": uri,
            "LocalPath": directory,
            "S3DataType": "S3Prefix",
            "S3InputMode": "File",
        },
    })
}

/// One declared `ProcessingOutput` over `S3Output`.
fn s3_output(name: &str, uri: &str, directory: &str) -> Value {
    json!({
        "OutputName": name,
        "S3Output": { "S3Uri": uri, "LocalPath": directory, "S3UploadMode": "EndOfJob" },
    })
}

/// A `CreateTrainingJob` body running `program` in the contained Python interpreter.
fn training_request(
    name: &str,
    role: &str,
    program: &str,
    arguments: &[&str],
    channels: Value,
    output_bucket: &str,
) -> Value {
    let mut vector = vec!["-c".to_string(), program.to_string()];
    vector.extend(arguments.iter().map(|word| (*word).to_string()));
    json!({
        "TrainingJobName": name,
        "AlgorithmSpecification": {
            "TrainingImage": own_image(),
            "TrainingInputMode": "File",
            "ContainerEntrypoint": ["python3"],
            "ContainerArguments": vector,
        },
        "RoleArn": role,
        "InputDataConfig": channels,
        "OutputDataConfig": { "S3OutputPath": format!("s3://{output_bucket}/out/") },
        "ResourceConfig": {
            "InstanceType": "ml.m5.large",
            "InstanceCount": 1,
            "VolumeSizeInGB": 10,
        },
    })
}

/// Poll a job's describe until it reports a terminal status, and answer that read.
async fn ended(emulator: &Emulator, case: &str, target: &str, member: &str, name: &str) -> Value {
    let status_member = format!("{member}Status");
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = emulator
            .sagemaker_ok(case, target, &json!({ format!("{member}Name"): name }))
            .await;
        if TERMINAL_STATUSES.contains(&str_field(&read, &status_member).as_str()) {
            return read;
        }
        look_again(deadline, case, "the job reached a terminal status", &read.to_string()).await;
    }
}

/// RULE ONE: the directories the platform prepares are directories of the container's own
/// storage from the program's first line.
///
/// The rows differ along WHICH directories the request makes the platform prepare and whether
/// any object is placed in them: a job that declares neither an input nor an output, one that
/// declares an output only, one whose declared paths are nested below the tree's own
/// directories, and one whose input prefix really holds an object.
#[tokio::test]
async fn the_platform_prepares_a_processing_container_s_directories() {
    /// One row: the case, the inputs and outputs declared, and the directories the program
    /// lists.
    struct Row {
        case: &'static str,
        inputs: Value,
        outputs: Value,
        listed: Vec<&'static str>,
    }
    let uri = format!("s3://{BUCKET}/out/");
    let rows = vec![
        Row {
            case: "a job that declares neither an input nor an output",
            inputs: json!([]),
            outputs: json!([]),
            listed: vec!["/opt/ml/processing/input", "/opt/ml/processing/output"],
        },
        Row {
            case: "a job that declares an output only",
            inputs: json!([]),
            outputs: json!([s3_output("out", &uri, "/opt/ml/processing/output")]),
            listed: vec!["/opt/ml/processing/input", "/opt/ml/processing/output"],
        },
        Row {
            case: "declared paths nested below the tree's own directories",
            inputs: json!([s3_input(
                "data",
                &format!("s3://{BUCKET}/train/"),
                "/opt/ml/processing/input/data"
            )]),
            outputs: json!([s3_output("out", &uri, "/opt/ml/processing/output/reports")]),
            listed: vec![
                "/opt/ml/processing/input",
                "/opt/ml/processing/input/data",
                "/opt/ml/processing/output",
                "/opt/ml/processing/output/reports",
            ],
        },
        Row {
            case: "an input prefix that really holds an object",
            inputs: json!([s3_input(
                "data",
                &format!("s3://{BUCKET}/train/"),
                "/opt/ml/processing/input/data"
            )]),
            outputs: json!([]),
            listed: vec![
                "/opt/ml/processing/input",
                "/opt/ml/processing/input/data",
                "/opt/ml/processing/output",
            ],
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(BUCKET)).await;
        emulator.given_bucket(BUCKET).await;
        emulator
            .given_object(BUCKET, "train/data.csv", "1,2\n3,4\n")
            .await;
        // Every row's declared location holds an object, so what is asserted is the DIRECTORY
        // the platform prepares rather than the data a download happened to place in one: the
        // row that declares none at all has nothing placed anywhere.
        let name = format!("platformtree{index}");
        let created = emulator
            .sagemaker(
                "CreateProcessingJob",
                &processing_request(
                    &name,
                    &role,
                    LIST_DIRECTORIES,
                    &row.listed,
                    row.inputs,
                    row.outputs,
                    json!({}),
                ),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: expected the job to be accepted, got {} with {}",
            created.0, created.1
        );
        let read = ended(&emulator, case, "DescribeProcessingJob", "ProcessingJob", &name).await;
        assert_eq!(
            str_field(&read, "ProcessingJobStatus"),
            "Completed",
            "case {case:?}: every listed directory has to exist for the program, got {}",
            read
        );
    }
}

/// RULE ONE, for a TRAINING job: each declared channel's directory and the model directory are
/// there for the container's program.
///
/// The rows differ along how many channels the job declares and whether a channel's prefix
/// holds an object at all.
#[tokio::test]
async fn the_platform_prepares_a_training_container_s_directories() {
    // (case, the channels declared, the directories the program lists)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "one channel",
            json!([channel("train", BUCKET, "train/")]),
            vec!["/opt/ml/input/data/train", "/opt/ml/model"],
        ),
        (
            "two channels, one of them with no object of its own",
            json!([
                channel("train", BUCKET, "train/"),
                channel("validation", BUCKET, "train/")
            ]),
            vec![
                "/opt/ml/input/data/train",
                "/opt/ml/input/data/validation",
                "/opt/ml/model",
            ],
        ),
    ];

    for (index, (case, channels, listed)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(BUCKET)).await;
        emulator.given_bucket(BUCKET).await;
        emulator
            .given_object(BUCKET, "train/data.csv", "1,2\n3,4\n")
            .await;
        let name = format!("platformtrain{index}");
        let created = emulator
            .sagemaker(
                "CreateTrainingJob",
                &training_request(&name, &role, LIST_DIRECTORIES, &listed, channels, BUCKET),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: expected the job to be accepted, got {} with {}",
            created.0, created.1
        );
        let read = ended(&emulator, case, "DescribeTrainingJob", "TrainingJob", &name).await;
        assert_eq!(
            str_field(&read, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: every listed directory has to exist for the program, got {}",
            read
        );
    }
}

/// RULE ONE, over the whole leg a program works through: the files the service placed in the
/// container are readable, and what the program writes into its output directory is uploaded —
/// with the program reading and writing text the way one that names no encoding does (through
/// `pathlib`, which hands `open` the environment's own encoding).
///
/// The rows differ along WHAT the input holds: one file, several files, and bytes that are not
/// ASCII, because the encoding is what this row set turns on.
#[tokio::test]
async fn a_program_reads_and_writes_the_container_s_text_files() {
    // (case, the files the input prefix holds as (key under the prefix, contents))
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("one file of plain text", vec![("data.csv", "1,2\n3,4\n")]),
        (
            "several files",
            vec![("a.csv", "1,2\n"), ("b.csv", "3,4\n"), ("c.csv", "5,6\n")],
        ),
        (
            "text outside ASCII, which is what an encoding decides",
            vec![("données.csv", "1,é\n2,ü\n")],
        ),
    ];

    for (index, (case, files)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(BUCKET)).await;
        emulator.given_bucket(BUCKET).await;
        for (name, contents) in &files {
            emulator
                .given_object(BUCKET, &format!("in{index}/{name}"), contents)
                .await;
        }
        let name = format!("pathlibcopy{index}");
        let outputs = json!([s3_output(
            "out",
            &format!("s3://{BUCKET}/out{index}/"),
            "/opt/ml/processing/output"
        )]);
        let created = emulator
            .sagemaker(
                "CreateProcessingJob",
                &processing_request(
                    &name,
                    &role,
                    COPY_WITH_PATHLIB,
                    &[],
                    json!([s3_input(
                        "data",
                        &format!("s3://{BUCKET}/in{index}/"),
                        "/opt/ml/processing/input/data"
                    )]),
                    outputs,
                    json!({}),
                ),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: expected the job to be accepted, got {} with {}",
            created.0, created.1
        );
        let read = ended(&emulator, case, "DescribeProcessingJob", "ProcessingJob", &name).await;
        assert_eq!(
            str_field(&read, "ProcessingJobStatus"),
            "Completed",
            "case {case:?}: the program reads and writes its own text files, got {read}"
        );
        let mut expected: Vec<String> = files
            .iter()
            .map(|(name, _)| format!("out{index}/{name}"))
            .collect();
        expected.sort();
        let written = listed_keys(&emulator, BUCKET, &format!("out{index}/")).await;
        assert_eq!(
            written, expected,
            "case {case:?}: every file the program wrote is one object under the declared \
             prefix, got {written:?}"
        );
        for (name, contents) in &files {
            let body = object_bytes(&emulator, case, BUCKET, &format!("out{index}/{name}")).await;
            assert_eq!(
                String::from_utf8_lossy(&body),
                *contents,
                "case {case:?}: the object carries the text the program copied"
            );
        }
    }
}

/// RULE TWO: an AWS call the container's own program makes is made as the job's execution role
/// — it is served where that role is granted the action, and refused where it is not, which is
/// the program's own failure.
///
/// The rows differ along the GRANT the execution role carries, which is what decides the
/// answer: every Amazon S3 action on the bucket, and a grant that does not reach it.
#[tokio::test]
async fn a_container_program_s_own_aws_call_is_made_as_the_execution_role() {
    // (case, the grant the role carries, whether the program's own call is served)
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "a role granted every Amazon S3 action on the bucket",
            every_s3_action_on(BUCKET),
            true,
        ),
        (
            "a role whose grant does not reach the bucket",
            allow(json!("s3:*"), json!("arn:aws:s3:::some-other-bucket/*")),
            false,
        ),
    ];

    for (index, (case, grant, served)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &grant).await;
        emulator.given_bucket(BUCKET).await;
        let name = format!("sdkjob{index}");
        let created = emulator
            .sagemaker(
                "CreateProcessingJob",
                &processing_request(
                    &name,
                    &role,
                    WRITE_WITH_THE_SDK,
                    &[],
                    json!([]),
                    json!([]),
                    json!({
                        "OUT_BUCKET": BUCKET,
                        "OUT_KEY": WRITTEN_KEY,
                        "OUT_BODY": WRITTEN_BODY,
                    }),
                ),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: expected the job to be accepted, got {} with {}",
            created.0, created.1
        );
        let read = ended(&emulator, case, "DescribeProcessingJob", "ProcessingJob", &name).await;
        let status = str_field(&read, "ProcessingJobStatus");
        let expected = if served { "Completed" } else { "Failed" };
        assert_eq!(
            status, expected,
            "case {case:?}: expected the job to end {expected}, got {read}"
        );
        let written = listed_keys(&emulator, BUCKET, "from-the-container/").await;
        if served {
            assert_eq!(
                written,
                vec![WRITTEN_KEY.to_string()],
                "case {case:?}: the object the program wrote has to be in Amazon S3, got \
                 {written:?}"
            );
            let body = object_bytes(&emulator, case, BUCKET, WRITTEN_KEY).await;
            assert_eq!(
                String::from_utf8_lossy(&body),
                WRITTEN_BODY,
                "case {case:?}: the object carries what the program wrote"
            );
        } else {
            assert!(
                written.is_empty(),
                "case {case:?}: a call the role may not make writes nothing, got {written:?}"
            );
            let reason = str_field(&read, "FailureReason");
            assert!(
                reason.starts_with("AlgorithmError:"),
                "case {case:?}: a program that failed inside the container reports the \
                 AlgorithmError class, got {reason:?}"
            );
        }
    }
}

/// RULE THREE: a leg of a job's work Amazon S3 refused reports the MESSAGE Amazon S3 answered
/// with, in the envelope naming the service, the status and the error code.
///
/// The rows differ along the LEG that was refused — the input the job downloads and the model
/// artifact it uploads — because the same answer is reported for each.
#[tokio::test]
async fn an_amazon_s3_refusal_reports_the_message_amazon_s3_answered() {
    /// One row: the case, the grant the role carries, and the leg the reason has to name.
    struct Row {
        case: &'static str,
        grant: Value,
        leg: &'static str,
    }
    let rows = vec![
        Row {
            case: "a role that may not read the channel's object",
            grant: allow(
                json!(["s3:ListBucket", "s3:PutObject"]),
                json!([
                    format!("arn:aws:s3:::{BUCKET}"),
                    format!("arn:aws:s3:::{BUCKET}/*")
                ]),
            ),
            leg: "ClientError: Data download failed:",
        },
        Row {
            case: "a role that may not write the model artifact",
            grant: allow(
                json!(["s3:ListBucket", "s3:GetObject"]),
                json!([
                    format!("arn:aws:s3:::{BUCKET}"),
                    format!("arn:aws:s3:::{BUCKET}/*")
                ]),
            ),
            leg: "ClientError: Model artifact upload failed:",
        },
    ];
    // The answer Amazon S3 gives a refused call, as the SDK inside a container reports one.
    let answered = service_com_amazonaws_sagemaker_sagemaker::state::access_denied_detail();

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &row.grant).await;
        emulator.given_bucket(BUCKET).await;
        emulator
            .given_object(BUCKET, "train/data.csv", "1,2\n3,4\n")
            .await;
        let name = format!("deniedleg{index}");
        let created = emulator
            .sagemaker(
                "CreateTrainingJob",
                &training_request(
                    &name,
                    &role,
                    "import sys\nsys.exit(0)\n",
                    &[],
                    json!([channel("train", BUCKET, "train/")]),
                    BUCKET,
                ),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: expected the job to be accepted, got {} with {}",
            created.0, created.1
        );
        let read = ended(&emulator, case, "DescribeTrainingJob", "TrainingJob", &name).await;
        assert_eq!(
            str_field(&read, "TrainingJobStatus"),
            "Failed",
            "case {case:?}: a leg Amazon S3 refused fails the job, got {read}"
        );
        let reason = str_field(&read, "FailureReason");
        assert!(
            reason.starts_with(row.leg),
            "case {case:?}: expected the reason to name the leg {:?}, got {reason:?}",
            row.leg
        );
        assert!(
            reason.contains(&answered),
            "case {case:?}: expected the reason to carry {answered:?}, got {reason:?}"
        );
        // The message Amazon S3 answers with, and the code it answers under, are both in it:
        // a client that classifies the reason by the code and an operator reading why the call
        // was refused are each served.
        assert!(
            reason.contains("Access Denied") && reason.contains("Error Code: AccessDenied"),
            "case {case:?}: expected the Amazon S3 message and its code, got {reason:?}"
        );
    }
}

/// RULE FOUR: `CreateProcessingJob` refuses an input data source whose location names no object
/// the execution role can list, with the sentence every create that resolves one answers — and
/// the clause that sentence ends with carries what Amazon S3 ANSWERED: the identity-based
/// refusal for a listing authorization withheld, Amazon S3's own sentence for a location it
/// answered is not there, and nothing at all for a listing that answered and merely held
/// nothing.
///
/// The rows differ along WHY the location answers nothing: a prefix the bucket holds nothing
/// under, a role whose grant does not reach the bucket at all, a bucket that does not exist
/// though the role may list it — and a location that answers an object the role may list, which
/// is accepted.
#[tokio::test]
async fn create_processing_job_refuses_an_input_source_that_names_no_listable_object() {
    /// What the refusal's sentence ends with, which is what Amazon S3 answered about the
    /// location.
    #[derive(PartialEq, Eq)]
    enum Answered {
        /// A listing that answered and held nothing: Amazon S3 refused nothing, so the
        /// sentence stands alone.
        Nothing,
        /// The listing was withheld from the role, so the clause is that identity refusal.
        IdentityRefusal,
        /// Amazon S3 answered that what the location names is not there, in a sentence of its
        /// own — reported as that sentence rather than as one written here.
        AmazonS3Sentence,
    }
    /// One row: the case, the grant, the bucket and prefix the input reads, whether the create
    /// is refused and what the refusal's clause carries.
    struct Row {
        case: &'static str,
        grant: Value,
        bucket: &'static str,
        prefix: &'static str,
        refused: bool,
        clause: Answered,
    }
    let rows = vec![
        Row {
            case: "a prefix the bucket holds nothing under",
            grant: every_s3_action_on(BUCKET),
            bucket: BUCKET,
            prefix: "nothing-here/",
            refused: true,
            clause: Answered::Nothing,
        },
        Row {
            case: "a bucket that does not exist, though the role may list it",
            grant: allow(json!("s3:*"), json!("*")),
            bucket: "no-such-input-bucket",
            prefix: "train/",
            refused: true,
            clause: Answered::AmazonS3Sentence,
        },
        Row {
            case: "a role with no Amazon S3 grant at all",
            grant: allow(json!("sts:AssumeRole"), json!("*")),
            bucket: BUCKET,
            prefix: "train/",
            refused: true,
            clause: Answered::IdentityRefusal,
        },
        Row {
            case: "a location that answers an object the role may list",
            grant: every_s3_action_on(BUCKET),
            bucket: BUCKET,
            prefix: "train/",
            refused: false,
            clause: Answered::Nothing,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &row.grant).await;
        emulator.given_bucket(BUCKET).await;
        emulator
            .given_object(BUCKET, "train/data.csv", "1,2\n3,4\n")
            .await;
        let name = format!("procinput{index}");
        let uri = format!("s3://{}/{}", row.bucket, row.prefix);
        let (status, answered) = emulator
            .sagemaker(
                "CreateProcessingJob",
                &processing_request(
                    &name,
                    &role,
                    "import sys\nsys.exit(0)\n",
                    &[],
                    json!([s3_input("data", &uri, "/opt/ml/processing/input/data")]),
                    json!([]),
                    json!({}),
                ),
            )
            .await;
        if !row.refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the job to be accepted, got {status} with {answered}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: an input data source that answers no object must be refused, got \
             {status} with {answered}"
        );
        let located = format!(
            "No S3 objects found under S3 URL \"{uri}\" given in input data source. Please \
             ensure that the bucket exists in the selected region ({REGION}), that objects \
             exist under that S3 prefix, and that the role \"{role}\" has \"s3:ListBucket\" \
             permissions on bucket \"{}\".",
            row.bucket
        );
        let clause = format!(
            " Error message from S3: User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/SageMaker \
             is not authorized to perform: s3:ListBucket on resource: \"arn:aws:s3:::{}\" \
             because no identity-based policy allows the s3:ListBucket action",
            row.bucket
        );
        let answered_message = message(&answered);
        match row.clause {
            // The sentence alone: Amazon S3 answered and refused nothing, so there is nothing
            // of its own to report.
            Answered::Nothing => assert_eq!(
                answered_message, located,
                "case {case:?}: expected {located:?}, got {answered_message:?}"
            ),
            // The listing was withheld from the role, so the clause is that refusal, named
            // principal, action and resource included.
            Answered::IdentityRefusal => {
                let expected = format!("{located}{clause}");
                assert_eq!(
                    answered_message, expected,
                    "case {case:?}: expected {expected:?}, got {answered_message:?}"
                );
            }
            // Amazon S3 answered that what the location names is not there. The sentence it
            // words that with is Amazon S3's own, so what is asserted is that the refusal
            // reports one rather than a wording written here.
            Answered::AmazonS3Sentence => {
                let reported = answered_message
                    .strip_prefix(&located)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the refusal to begin {located:?}, got \
                             {answered_message:?}"
                        )
                    });
                assert!(
                    reported.starts_with(" Error message from S3: ")
                        && reported.len() > " Error message from S3: ".len(),
                    "case {case:?}: expected the clause to report what Amazon S3 answered, got \
                     {reported:?}"
                );
                assert!(
                    !reported.contains("not authorized to perform"),
                    "case {case:?}: a listing nothing withheld carries no authorization \
                     refusal, got {reported:?}"
                );
            }
        }
        // Nothing is created for a refused call: the job the request named is not there.
        let (read_status, _) = emulator
            .sagemaker("DescribeProcessingJob", &json!({ "ProcessingJobName": name }))
            .await;
        assert_ne!(
            read_status, 200,
            "case {case:?}: a refused create stores no job, but one was described"
        );
        // The role ARN the refusal echoes is the one the request sent.
        assert_eq!(
            role,
            role_arn(ROLE),
            "case {case:?}: the role the rows share is the one the refusal names"
        );
    }
}
