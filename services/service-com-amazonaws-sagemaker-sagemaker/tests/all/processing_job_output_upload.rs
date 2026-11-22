// SPDX-License-Identifier: CC0-1.0

//! What an accepted PROCESSING job does with the Amazon S3 locations its request names —
//! driven end to end over the real wire path (SageMaker, Amazon S3 and IAM on ONE shared
//! engine, the way the production router mounts every service), so what is asserted is what a
//! client sees.
//!
//! RULE ONE — the output upload is SageMaker's own work, not the container's:
//! `ProcessingOutput$AppManaged` is "when `False` (default), output operations are managed by
//! Amazon SageMaker", `ProcessingS3Output$LocalPath` is "the local path of a directory where
//! you want Amazon SageMaker to upload its contents to Amazon S3", and `S3UploadMode` decides
//! "whether to upload the results of the processing job continuously or after the job
//! completes". So once the job reports a terminal status, every file its container wrote under
//! that directory is ONE object under `S3Output.S3Uri`, at the file's own path below the
//! directory, carrying exactly the bytes the container wrote — and an output the APPLICATION
//! manages is uploaded by the application instead, so the service leaves nothing for it.
//!
//! RULE TWO — those writes are made UNDER the job's `RoleArn`, so a destination bucket whose
//! policy EXPLICITLY DENIES that role the write is a job that FAILS: the modelled `Failed`
//! status with a `FailureReason` naming the Amazon S3 access failure, and nothing under the
//! declared prefix — never a `Completed` status for an upload that did not happen.
//!
//! RULE THREE — the input download is the same work in the other direction:
//! `ProcessingS3Input$LocalPath` is "the local path in your container where you want Amazon
//! SageMaker to write input data to", so the objects under `S3Input.S3Uri` are files of the
//! container's own storage from the program's first line, which is what lets the program read
//! them.
//!
//! RULE FOUR — every SageMaker-managed path of a processing job names a directory of the
//! container's own tree: `ProcessingS3Input$LocalPath` "must begin with `/opt/ml/processing/`",
//! and an output's `LocalPath` is the directory of that same tree which "will be created by the
//! platform and exist when your container's entrypoint is invoked". A request naming a
//! directory outside it is refused 400 `ValidationException` naming the member, with nothing
//! stored.
//!
//! RULE FIVE — the container a job's program runs in is that JOB's own: the data the service
//! downloaded for it and the files its own program wrote, built for that run and dropped with
//! it. So a later job's program sees nothing of an earlier job's, and an output whose
//! `LocalPath` names the directory another job's data was placed under uploads nothing.
//!
//! The table spans what RULE ONE generalises over: how many files the container writes, where
//! under the directory it writes them, whether the declared `S3Uri` carries a trailing
//! delimiter, which `S3UploadMode` the output asks for, how many outputs one job declares, and
//! whether the output is application-managed.
//!
//! Every wait here is a poll for the CONDITION, never a pause of a fixed length, bounded by a
//! deadline far longer than any span this state gives a job's work — so it bounds a BROKEN
//! emulator and not the condition.

use std::time::Instant;

use serde_json::{json, Value};

use super::model_artifact_admission::{role_arn, str_field, trust_for, Emulator, ACCOUNT, REGION};
use super::sagemaker_job_execution_over_s3::{
    every_s3_action_on, given_job_role, keys_until, listed_keys, look_again, object_bytes, ROLE,
    SETTLE_BUDGET,
};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The `ProcessingJobStatus` members a job's work has ENDED in
/// (`smithy_query(query="shape",shape="ProcessingJobStatus")` lists these three beside the
/// transient ones).
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The directory a SageMaker-managed processing output is uploaded from: `LocalPath` "is an
/// absolute path to a directory containing output files", under the `/opt/ml/processing/`
/// directory the model requires of an input and which the platform creates for an output.
const OUTPUT_DIRECTORY: &str = "/opt/ml/processing/output";

/// A second output directory, for the row that declares two outputs.
const SECOND_OUTPUT_DIRECTORY: &str = "/opt/ml/processing/reports";

/// The directory the row that reads its input is given its input at.
const INPUT_DIRECTORY: &str = "/opt/ml/processing/input";

/// An ECR-shaped path for a processing container of the caller's own.
fn processing_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/my-processor:1")
}

/// A `CreateProcessingJob` body: `program` as the container's argument vector after
/// `/bin/sh -c`, with `inputs` and `outputs` as declared.
fn processing_request(name: &str, role: &str, program: &str, inputs: Value, outputs: Value) -> Value {
    let mut body = json!({
        "ProcessingJobName": name,
        "AppSpecification": {
            "ImageUri": processing_image(),
            "ContainerEntrypoint": ["/bin/sh", "-c"],
            "ContainerArguments": [program],
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
    if let Some(inputs) = inputs.as_array() {
        if !inputs.is_empty() {
            body["ProcessingInputs"] = json!(inputs);
        }
    }
    if outputs.as_array().is_some_and(|outputs| !outputs.is_empty()) {
        body["ProcessingOutputConfig"] = json!({ "Outputs": outputs });
    }
    body
}

/// One declared `ProcessingOutput` over `S3Output`.
fn s3_output(name: &str, uri: &str, directory: &str, mode: &str, app_managed: bool) -> Value {
    json!({
        "OutputName": name,
        "AppManaged": app_managed,
        "S3Output": { "S3Uri": uri, "LocalPath": directory, "S3UploadMode": mode },
    })
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

/// Poll `DescribeProcessingJob` until the job has reached a terminal status, and answer that
/// read.
async fn ended(emulator: &Emulator, case: &str, job: &str) -> Value {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = emulator
            .sagemaker_ok(case, "DescribeProcessingJob", &json!({ "ProcessingJobName": job }))
            .await;
        if TERMINAL_STATUSES.contains(&str_field(&read, "ProcessingJobStatus").as_str()) {
            return read;
        }
        look_again(
            deadline,
            case,
            "the processing job reached a terminal status",
            &read.to_string(),
        )
        .await;
    }
}

/// RULE ONE: every file the container wrote under a SageMaker-managed output's `LocalPath` is
/// one object under that output's `S3Uri`, at the file's own path below the directory, holding
/// what the container wrote.
#[tokio::test]
async fn a_completed_processing_job_uploads_what_its_container_wrote() {
    /// What one declared prefix has to hold once the job has ended: the prefix listed, the
    /// keys under it, and the `(key, body)` pairs read back through `GetObject`.
    type Uploaded = (String, Vec<String>, Vec<(String, String)>);
    /// One row: the case, the program the container runs, the outputs the job declares, and
    /// the keys each declared prefix has to hold with their bodies.
    struct Row {
        case: &'static str,
        program: String,
        outputs: Value,
        expected: Vec<Uploaded>,
    }
    let bucket = "processing-job-outputs";
    let uri = |prefix: &str| format!("s3://{bucket}/{prefix}");
    let rows = vec![
        Row {
            case: "three files under the output directory",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY} && echo one > {OUTPUT_DIRECTORY}/file_1.txt && \
                 echo two > {OUTPUT_DIRECTORY}/file_2.txt && \
                 echo three > {OUTPUT_DIRECTORY}/file_3.txt"
            ),
            outputs: json!([s3_output(
                "result",
                &uri("output/"),
                OUTPUT_DIRECTORY,
                "EndOfJob",
                false
            )]),
            expected: vec![(
                "output/".to_string(),
                vec![
                    "output/file_1.txt".to_string(),
                    "output/file_2.txt".to_string(),
                    "output/file_3.txt".to_string(),
                ],
                vec![
                    ("output/file_1.txt".to_string(), "one\n".to_string()),
                    ("output/file_2.txt".to_string(), "two\n".to_string()),
                    ("output/file_3.txt".to_string(), "three\n".to_string()),
                ],
            )],
        },
        Row {
            case: "a file nested below the output directory",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY}/nested && echo deep > {OUTPUT_DIRECTORY}/nested/part.txt"
            ),
            outputs: json!([s3_output(
                "result",
                &uri("output/"),
                OUTPUT_DIRECTORY,
                "EndOfJob",
                false
            )]),
            expected: vec![(
                "output/".to_string(),
                vec!["output/nested/part.txt".to_string()],
                vec![("output/nested/part.txt".to_string(), "deep\n".to_string())],
            )],
        },
        Row {
            case: "a declared S3Uri with no trailing delimiter",
            program: format!("mkdir -p {OUTPUT_DIRECTORY} && echo flat > {OUTPUT_DIRECTORY}/only.txt"),
            outputs: json!([s3_output(
                "result",
                &uri("plain"),
                OUTPUT_DIRECTORY,
                "EndOfJob",
                false
            )]),
            expected: vec![(
                "plain/".to_string(),
                vec!["plain/only.txt".to_string()],
                vec![("plain/only.txt".to_string(), "flat\n".to_string())],
            )],
        },
        Row {
            case: "an output asking for continuous upload",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY} && echo streamed > {OUTPUT_DIRECTORY}/live.txt"
            ),
            outputs: json!([s3_output(
                "result",
                &uri("continuous/"),
                OUTPUT_DIRECTORY,
                "Continuous",
                false
            )]),
            expected: vec![(
                "continuous/".to_string(),
                vec!["continuous/live.txt".to_string()],
                vec![("continuous/live.txt".to_string(), "streamed\n".to_string())],
            )],
        },
        Row {
            case: "two declared outputs, each from its own directory",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY} {SECOND_OUTPUT_DIRECTORY} && \
                 echo data > {OUTPUT_DIRECTORY}/data.csv && \
                 echo report > {SECOND_OUTPUT_DIRECTORY}/report.txt"
            ),
            outputs: json!([
                s3_output("result", &uri("two/data/"), OUTPUT_DIRECTORY, "EndOfJob", false),
                s3_output(
                    "reports",
                    &uri("two/reports/"),
                    SECOND_OUTPUT_DIRECTORY,
                    "EndOfJob",
                    false
                ),
            ]),
            expected: vec![
                (
                    "two/data/".to_string(),
                    vec!["two/data/data.csv".to_string()],
                    vec![("two/data/data.csv".to_string(), "data\n".to_string())],
                ),
                (
                    "two/reports/".to_string(),
                    vec!["two/reports/report.txt".to_string()],
                    vec![("two/reports/report.txt".to_string(), "report\n".to_string())],
                ),
            ],
        },
        Row {
            case: "an output the application manages itself",
            program: format!("mkdir -p {OUTPUT_DIRECTORY} && echo mine > {OUTPUT_DIRECTORY}/app.txt"),
            outputs: json!([s3_output(
                "result",
                &uri("app-managed/"),
                OUTPUT_DIRECTORY,
                "EndOfJob",
                true
            )]),
            expected: vec![("app-managed/".to_string(), Vec::new(), Vec::new())],
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        let job = format!("outputjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateProcessingJob",
                &processing_request(&job, &role, &row.program, json!([]), row.outputs.clone()),
            )
            .await;
        let read = ended(&emulator, case, &job).await;
        assert_eq!(
            str_field(&read, "ProcessingJobStatus"),
            "Completed",
            "case {case:?}: the job's program exited zero, so it completes, got {read}"
        );
        for (prefix, keys, bodies) in &row.expected {
            keys_until(&emulator, case, bucket, prefix, keys).await;
            for (key, body) in bodies {
                let held = object_bytes(&emulator, case, bucket, key).await;
                assert_eq!(
                    String::from_utf8_lossy(&held),
                    *body,
                    "case {case:?}: {key} must hold what the container wrote"
                );
            }
        }
    }
}

/// RULE: a SageMaker-managed path of a processing job names a directory of the container's own
/// tree — `ProcessingS3Input$LocalPath` "must begin with `/opt/ml/processing/`", and an
/// output's `LocalPath` is the directory of that same tree the platform creates — so a value
/// naming a directory outside it is refused 400 `ValidationException` naming the member, with
/// nothing stored and no job created.
///
/// The rows differ along the member carrying the path and the shape of the value: an output
/// path outside the tree, one that walks out of it, a relative one, an output the APPLICATION
/// manages (whose own paths are its business), and an input path outside the tree.
#[tokio::test]
async fn a_local_path_outside_the_container_tree_is_refused() {
    let bucket = "processing-local-paths";
    /// One row: the case, the inputs and outputs the request declares, and the member the
    /// refusal has to name — `None` for a request that is accepted.
    struct Row {
        case: &'static str,
        inputs: Value,
        outputs: Value,
        refused: Option<&'static str>,
    }
    let uri = format!("s3://{bucket}/out/");
    let rows = vec![
        Row {
            case: "an output path inside the container's tree",
            inputs: json!([]),
            outputs: json!([s3_output("result", &uri, OUTPUT_DIRECTORY, "EndOfJob", false)]),
            refused: None,
        },
        Row {
            case: "an output path outside the container's tree",
            inputs: json!([]),
            outputs: json!([s3_output("result", &uri, "/etc", "EndOfJob", false)]),
            refused: Some("ProcessingOutputConfig.Outputs[result].S3Output.LocalPath"),
        },
        Row {
            case: "an output path that walks out of the container's tree",
            inputs: json!([]),
            outputs: json!([s3_output(
                "result",
                &uri,
                "/opt/ml/processing/../../../etc",
                "EndOfJob",
                false
            )]),
            refused: Some("ProcessingOutputConfig.Outputs[result].S3Output.LocalPath"),
        },
        Row {
            case: "a relative output path",
            inputs: json!([]),
            outputs: json!([s3_output("result", &uri, "output", "EndOfJob", false)]),
            refused: Some("ProcessingOutputConfig.Outputs[result].S3Output.LocalPath"),
        },
        Row {
            case: "an output the application manages, whose own path is its business",
            inputs: json!([]),
            outputs: json!([s3_output("result", &uri, "/var/app/output", "EndOfJob", true)]),
            refused: None,
        },
        Row {
            case: "an input path outside the container's tree",
            inputs: json!([s3_input("data", &format!("s3://{bucket}/in/"), "/etc")]),
            outputs: json!([]),
            refused: Some("ProcessingInputs[data].S3Input.LocalPath"),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        emulator.given_object(bucket, "in/data.csv", "1,2\n").await;
        let job = format!("pathjob{index}");
        let body = processing_request(
            &job,
            &role,
            &format!("mkdir -p {OUTPUT_DIRECTORY} && echo done > {OUTPUT_DIRECTORY}/out.txt"),
            row.inputs,
            row.outputs,
        );
        let (status, answered) = emulator.sagemaker("CreateProcessingJob", &body).await;
        match row.refused {
            Some(member) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: a path outside the container's tree is refused, got \
                     {status} with {answered}"
                );
                let message = str_field(&answered, "message");
                assert!(
                    message.contains(member),
                    "case {case:?}: the refusal must name {member}, got {message:?}"
                );
                // Nothing was stored for a refused request, so the job is not there.
                let (read, body) = emulator
                    .sagemaker("DescribeProcessingJob", &json!({ "ProcessingJobName": job }))
                    .await;
                assert_ne!(
                    read, 200,
                    "case {case:?}: a refused create stores no job, got {read} with {body}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: a path inside the container's tree is accepted, got {status} \
                 with {answered}"
            ),
        }
    }
}

/// RULE: the container a job's program runs in is that JOB's own — the data the service
/// downloaded for it and the files its own program wrote — so a later job's program sees
/// nothing of an earlier job's, and an output whose `LocalPath` names the directory another
/// job's input was placed under uploads nothing.
///
/// Driven as a client would drive it: two jobs created in turn against ONE emulator, the
/// second naming the first's directories.
#[tokio::test]
async fn one_jobs_container_holds_nothing_of_another_jobs() {
    let case = "a second job naming the first job's directories";
    let bucket = "processing-job-isolation";
    let emulator = Emulator::new();
    let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
    emulator.given_bucket(bucket).await;
    emulator
        .given_object(bucket, "private/secret.csv", "7,8\n")
        .await;

    // The FIRST job: its input is downloaded into its own container, and its program copies
    // what it was given to its own output.
    emulator
        .sagemaker_ok(
            case,
            "CreateProcessingJob",
            &processing_request(
                "isolationfirst",
                &role,
                &format!(
                    "mkdir -p {OUTPUT_DIRECTORY} && cat {INPUT_DIRECTORY}/secret.csv > \
                     {OUTPUT_DIRECTORY}/copy.csv"
                ),
                json!([s3_input(
                    "data",
                    &format!("s3://{bucket}/private/"),
                    INPUT_DIRECTORY
                )]),
                json!([s3_output(
                    "result",
                    &format!("s3://{bucket}/first/"),
                    OUTPUT_DIRECTORY,
                    "EndOfJob",
                    false
                )]),
            ),
        )
        .await;
    let first = ended(&emulator, case, "isolationfirst").await;
    assert_eq!(
        str_field(&first, "ProcessingJobStatus"),
        "Completed",
        "case {case:?}: the first job's program exited zero, so it completes, got {first}"
    );
    keys_until(
        &emulator,
        case,
        bucket,
        "first/",
        &["first/copy.csv".to_string()],
    )
    .await;

    // The SECOND job declares NO input of its own, a program that writes nothing, and outputs
    // over the two directories the first job's data lived in. Its container is its own, so
    // there is nothing under either of them to upload.
    emulator
        .sagemaker_ok(
            case,
            "CreateProcessingJob",
            &processing_request(
                "isolationsecond",
                &role,
                "true",
                json!([]),
                json!([
                    s3_output(
                        "stolen-input",
                        &format!("s3://{bucket}/second/input/"),
                        INPUT_DIRECTORY,
                        "EndOfJob",
                        false
                    ),
                    s3_output(
                        "stolen-output",
                        &format!("s3://{bucket}/second/output/"),
                        OUTPUT_DIRECTORY,
                        "EndOfJob",
                        false
                    ),
                ]),
            ),
        )
        .await;
    let second = ended(&emulator, case, "isolationsecond").await;
    assert_eq!(
        str_field(&second, "ProcessingJobStatus"),
        "Completed",
        "case {case:?}: the second job's program exited zero, so it completes, got {second}"
    );
    let held = listed_keys(&emulator, bucket, "second/").await;
    assert!(
        held.is_empty(),
        "case {case:?}: the second job's container holds nothing of the first's, so its \
         outputs upload nothing, got {held:?}"
    );
}

/// RULE TWO: a destination bucket whose policy explicitly denies the job's role the WRITE
/// fails the job with the Amazon S3 access failure and leaves nothing under the prefix, while a
/// policy that says nothing about the role leaves the role's own grant standing — IAM's rule
/// for a write inside one account.
///
/// The rows differ along what the policy says: an explicit deny of the write for the role, an
/// explicit deny of every Amazon S3 action, a deny aimed at another principal, and a policy
/// that allows the write.
#[tokio::test]
async fn a_bucket_policy_that_denies_the_write_fails_the_processing_job() {
    let bucket = "processing-output-fenced";
    let role = role_arn(ROLE);
    let objects = format!("arn:aws:s3:::{bucket}/*");
    /// One row: the case, the bucket policy, and whether the job has to fail.
    struct Row {
        case: &'static str,
        policy: Value,
        failed: bool,
    }
    let rows = vec![
        Row {
            case: "an explicit deny of the write for the role",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "AWS": role.clone() },
                        "Action": "s3:*",
                        "Resource": objects.clone(),
                    },
                    {
                        "Effect": "Deny",
                        "Principal": { "AWS": role.clone() },
                        "Action": "s3:PutObject",
                        "Resource": objects.clone(),
                    },
                ],
            }),
            failed: true,
        },
        Row {
            case: "an explicit deny of every Amazon S3 action for the role",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "AWS": role.clone() },
                    "Action": "s3:*",
                    "Resource": objects.clone(),
                }],
            }),
            failed: true,
        },
        Row {
            case: "a deny aimed at another principal",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "AWS": role_arn("someone-else") },
                    "Action": "s3:PutObject",
                    "Resource": objects.clone(),
                }],
            }),
            failed: false,
        },
        Row {
            case: "a policy that allows the write",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": role.clone() },
                    "Action": "s3:*",
                    "Resource": objects.clone(),
                }],
            }),
            failed: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role_ref = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        // A bucket policy's `Principal.AWS` is resolved against IAM when it is stored, so the
        // role a row names has to be one the account holds.
        emulator
            .given_role("someone-else", &trust_for(SERVICE_PRINCIPAL))
            .await;
        emulator.given_bucket(bucket).await;
        emulator.given_bucket_policy(bucket, &row.policy).await;

        let prefix = format!("denied{index}/");
        let job = format!("deniedjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateProcessingJob",
                &processing_request(
                    &job,
                    &role_ref,
                    &format!(
                        "mkdir -p {OUTPUT_DIRECTORY} && echo result > {OUTPUT_DIRECTORY}/out.txt"
                    ),
                    json!([]),
                    json!([s3_output(
                        "result",
                        &format!("s3://{bucket}/{prefix}"),
                        OUTPUT_DIRECTORY,
                        "EndOfJob",
                        false
                    )]),
                ),
            )
            .await;
        let read = ended(&emulator, case, &job).await;
        let status = str_field(&read, "ProcessingJobStatus");
        let reason = str_field(&read, "FailureReason");
        if row.failed {
            assert_eq!(
                status, "Failed",
                "case {case:?}: a job whose output bucket fences the role off must fail, got \
                 {status} with {reason:?}"
            );
            assert!(
                reason.contains("AccessDenied"),
                "case {case:?}: the reason must name the Amazon S3 access failure, got {reason:?}"
            );
            let held = listed_keys(&emulator, bucket, &prefix).await;
            assert!(
                held.is_empty(),
                "case {case:?}: a refused upload leaves nothing under the prefix, got {held:?}"
            );
        } else {
            assert_eq!(
                status, "Completed",
                "case {case:?}: a policy that denies the role nothing fails nothing, got \
                 {status} with {reason:?}"
            );
            keys_until(
                &emulator,
                case,
                bucket,
                &prefix,
                &[format!("{prefix}out.txt")],
            )
            .await;
        }
    }
}

/// RULE THREE: the objects under a SageMaker-managed input's `S3Uri` are files of the
/// container's own storage under its `LocalPath`, so a program that reads one and writes it to
/// its output directory leaves an object holding what the input held.
///
/// The rows differ along how the input's location is addressed and how many objects it holds:
/// a prefix over one object, and a prefix over two.
#[tokio::test]
async fn a_processing_job_reads_the_inputs_the_service_placed_in_its_container() {
    let bucket = "processing-job-inputs";
    /// One row: the case, the objects staged under the input prefix, the prefix the input
    /// addresses, the program the container runs over the files placed for it, and the keys
    /// the output prefix has to hold with their bodies.
    struct Row {
        case: &'static str,
        staged: Vec<(&'static str, &'static str)>,
        prefix: &'static str,
        program: String,
        expected: Vec<(&'static str, &'static str)>,
    }
    let rows = vec![
        Row {
            case: "a prefix over one object",
            staged: vec![("in/one/data.csv", "1,2\n3,4\n")],
            prefix: "in/one/",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY} && cat {INPUT_DIRECTORY}/data.csv > \
                 {OUTPUT_DIRECTORY}/data.csv"
            ),
            expected: vec![("copied/data.csv", "1,2\n3,4\n")],
        },
        Row {
            case: "a prefix over two objects",
            staged: vec![("in/two/first.csv", "5,6\n"), ("in/two/second.csv", "7,8\n")],
            prefix: "in/two/",
            program: format!(
                "mkdir -p {OUTPUT_DIRECTORY} && cat {INPUT_DIRECTORY}/first.csv > \
                 {OUTPUT_DIRECTORY}/first.csv && cat {INPUT_DIRECTORY}/second.csv > \
                 {OUTPUT_DIRECTORY}/second.csv"
            ),
            expected: vec![("copied/first.csv", "5,6\n"), ("copied/second.csv", "7,8\n")],
        },
    ];
    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        for (key, body) in &row.staged {
            emulator.given_object(bucket, key, body).await;
        }
        let job = format!("inputjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateProcessingJob",
                &processing_request(
                    &job,
                    &role,
                    &row.program,
                    json!([s3_input(
                        "data",
                        &format!("s3://{bucket}/{}", row.prefix),
                        INPUT_DIRECTORY
                    )]),
                    json!([s3_output(
                        "result",
                        &format!("s3://{bucket}/copied/"),
                        OUTPUT_DIRECTORY,
                        "EndOfJob",
                        false
                    )]),
                ),
            )
            .await;
        let read = ended(&emulator, case, &job).await;
        assert_eq!(
            str_field(&read, "ProcessingJobStatus"),
            "Completed",
            "case {case:?}: the job's program exited zero, so it completes, got {read}"
        );
        let keys: Vec<String> = row
            .expected
            .iter()
            .map(|(key, _)| (*key).to_string())
            .collect();
        keys_until(&emulator, case, bucket, "copied/", &keys).await;
        for (key, body) in &row.expected {
            let held = object_bytes(&emulator, case, bucket, key).await;
            assert_eq!(
                String::from_utf8_lossy(&held),
                *body,
                "case {case:?}: {key} must hold what the input object held"
            );
        }
    }
}
