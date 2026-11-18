// SPDX-License-Identifier: CC0-1.0

//! Whether a training job's work is PERFORMED turns on the execution role SageMaker would
//! run it as, and on nothing about the job's own shape — driven end to end over the real
//! wire path (SageMaker, Amazon S3 and IAM mounted on ONE shared engine, the way the
//! production router mounts every service), so what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: `CreateTrainingJob` resolves the role the request names against this
//! account's IAM before it stores anything, and the job's work is then performed AS that
//! role:
//!
//! * a role whose trust document admits the SageMaker service principal for `sts:AssumeRole`
//!   backs a job that is accepted, runs, and settles `Completed` with no `FailureReason`,
//!   reporting `ModelArtifacts.S3ModelArtifacts` — an object Amazon S3 itself then holds
//!   under `<OutputDataConfig.S3OutputPath>/<TrainingJobName>/output/model.tar.gz`
//!   ("SageMaker saves the resulting model artifacts to an Amazon S3 location that you
//!   specify", `CreateTrainingJob`);
//! * a role whose trust document admits ANOTHER service principal only backs no run at all,
//!   so the call is refused 400 `ValidationException` naming the role the request supplied,
//!   no job is stored under that name, and the output location that job would have written
//!   to stays empty.
//!
//! The two halves of each row differ in ONE thing: which service principal the role's trust
//! document admits. The permission policy both roles carry, the channels, the algorithm
//! image, the hyperparameters and the compute are the same, so nothing but the trust
//! relationship can account for the difference in outcome.
//!
//! The table spans what the rule is INDEPENDENT of: how the channel's location is addressed
//! (one object's own key, prefixes, a manifest naming the objects), how many channels the job
//! declares, how many records and features its data carries, whether the algorithm is one of
//! SageMaker's own or a container of the caller's, whether the request carries
//! hyperparameters, and whether the output path carries a trailing separator. It also spans
//! which OTHER service principal the refused half's role admits, because every principal that
//! is not SageMaker's withholds the assume alike.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::model_artifact_admission::{
    error_code, message, str_field, trust_for, Emulator, ACCOUNT, REGION,
};
use super::training_job_execution::csv_records;

/// The service principal a SageMaker execution role's trust document has to admit
/// (`CreateTrainingJob`'s `RoleArn`: the ARN "that SageMaker assumes to perform tasks on your
/// behalf during model training").
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The bucket every case stages its channel objects in and writes its artifact to.
const BUCKET: &str = "sagemaker-trust-contrast";

/// How long a job is polled for the terminal status it has to reach before a case fails, and
/// how often. What the poll waits for is the CONDITION, re-read in a loop, so a loaded host
/// cannot fail a case the clock would have served.
const SETTLE_BUDGET: Duration = Duration::from_secs(120);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// One pair of jobs: the SAME request, submitted once with a role that trusts SageMaker and
/// once with a role that trusts `other_principal` instead.
struct Row {
    case: &'static str,
    /// The service principal the refused half's role admits instead of SageMaker's.
    other_principal: &'static str,
    /// The `InputDataConfig` both halves declare.
    channels: Value,
    /// The `AlgorithmSpecification.TrainingImage` both halves name.
    image: String,
    /// The `HyperParameters` both halves carry, `{}` for a row that carries none.
    hyper_parameters: Value,
    /// Whether the output path carries a trailing separator.
    trailing_separator: bool,
}

/// An ECR-shaped path for SageMaker's built-in gradient-boosted tree algorithm, whose
/// repository is `sagemaker-xgboost` ("Docker registry paths and example code").
fn built_in_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/sagemaker-xgboost:1.7-1")
}

/// An ECR-shaped path for a training container of the caller's own.
fn own_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/my-trainer:1")
}

/// One channel addressing `uri` as `data_type`.
fn channel(name: &str, uri: String, data_type: &str) -> Value {
    json!({
        "ChannelName": name,
        "DataSource": { "S3DataSource": { "S3DataType": data_type, "S3Uri": uri } },
        "ContentType": "text/csv",
    })
}

/// The Amazon S3 grant both roles of a pair carry: the object actions on the bucket's
/// objects, and the listing on the BUCKET itself, which is the resource `s3:ListBucket` is
/// granted on.
fn every_s3_action_on(bucket: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:PutObject"],
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
            },
            {
                "Effect": "Allow",
                "Action": "s3:ListBucket",
                "Resource": format!("arn:aws:s3:::{bucket}"),
            },
        ],
    })
}

/// The `CreateTrainingJob` body of one half of a pair: `row`'s own shape, the role it runs as
/// and the location it writes to.
fn training_body(row: &Row, job: &str, role_arn: &str, output: &str) -> Value {
    json!({
        "TrainingJobName": job,
        "AlgorithmSpecification": {
            "TrainingImage": row.image,
            "TrainingInputMode": "File",
        },
        "RoleArn": role_arn,
        "InputDataConfig": row.channels,
        "HyperParameters": row.hyper_parameters,
        "OutputDataConfig": { "S3OutputPath": output },
        "ResourceConfig": {
            "InstanceType": "ml.m5.large",
            "InstanceCount": 1,
            "VolumeSizeInGB": 10,
        },
        "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
    })
}

/// How SageMaker words a `RoleArn` it cannot assume: the role ARN as the caller sent it, then
/// the fixed sentence naming what the role has to allow. Written out here rather than taken
/// from the implementation, so the assertion is on AWS's own wording.
fn could_not_assume_role(role_arn: &str) -> String {
    format!(
        "Could not assume role {role_arn}. Please ensure that the role exists and allows \
         principal '{SERVICE_PRINCIPAL}' to assume the role."
    )
}

/// The keys Amazon S3 lists under `prefix`, read through `ListObjectsV2` over the harness's
/// own Amazon S3 front door.
async fn listed_keys(emulator: &Emulator, bucket: &str, prefix: &str) -> Vec<String> {
    let (status, body) = emulator
        .s3("GET", &format!("/{bucket}?list-type=2&prefix={prefix}"), &[])
        .await;
    assert_eq!(
        status,
        200,
        "the listing of {bucket}/{prefix} must be served, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8_lossy(&body).to_string();
    text.split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// Poll `DescribeTrainingJob` until `job` leaves `InProgress`, answering the read that saw a
/// terminal status.
async fn settled(emulator: &Emulator, case: &str, job: &str) -> Value {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let read = emulator
            .sagemaker_ok(case, "DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        if str_field(&read, "TrainingJobStatus") != "InProgress" {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never left InProgress; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// RULE: of two training jobs that differ in nothing but the trust document of the role they
/// name, the one whose role admits the SageMaker service principal runs and reports the
/// artifact it wrote, and the one whose role admits another service principal is refused at
/// the call and writes nothing at all.
#[tokio::test]
async fn the_roles_trust_document_decides_whether_a_training_job_runs() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;

    // One object addressed by its own key.
    emulator
        .given_object(BUCKET, "one/data.csv", &csv_records(6, 2))
        .await;
    // Two channels, each a prefix of its own.
    emulator
        .given_object(BUCKET, "split/train/part-1.csv", &csv_records(4, 1))
        .await;
    emulator
        .given_object(BUCKET, "split/validation/part-1.csv", &csv_records(3, 1))
        .await;
    // A manifest naming one object by a prefix and a suffix.
    emulator
        .given_object(BUCKET, "listed/rows.csv", &csv_records(7, 3))
        .await;
    emulator
        .given_object(
            BUCKET,
            "manifest.json",
            &json!([{ "prefix": format!("s3://{BUCKET}/listed/") }, "rows.csv"]).to_string(),
        )
        .await;

    let rows = [
        Row {
            case: "one channel addressed by the object's own key, a container of the caller's \
                   own, and an output path with a trailing separator",
            other_principal: "ec2.amazonaws.com",
            channels: json!([channel(
                "train",
                format!("s3://{BUCKET}/one/data.csv"),
                "S3Prefix"
            )]),
            image: own_image(),
            hyper_parameters: json!({}),
            trailing_separator: true,
        },
        Row {
            case: "two channels under prefixes, the built-in algorithm with its \
                   hyperparameters, and an output path with no trailing separator",
            other_principal: "lambda.amazonaws.com",
            channels: json!([
                channel(
                    "train",
                    format!("s3://{BUCKET}/split/train/"),
                    "S3Prefix"
                ),
                channel(
                    "validation",
                    format!("s3://{BUCKET}/split/validation/"),
                    "S3Prefix"
                ),
            ]),
            image: built_in_image(),
            hyper_parameters: json!({ "objective": "reg:squarederror", "num_round": "5" }),
            trailing_separator: false,
        },
        Row {
            case: "a manifest naming the channel's objects",
            other_principal: "glue.amazonaws.com",
            channels: json!([channel(
                "train",
                format!("s3://{BUCKET}/manifest.json"),
                "ManifestFile"
            )]),
            image: built_in_image(),
            hyper_parameters: json!({}),
            trailing_separator: true,
        },
    ];

    // The accepted halves are submitted first and read afterwards: their terminal status is
    // the clock's, so every job of the table is already running by the time the first of them
    // is polled.
    let mut accepted: Vec<(&'static str, String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let trusted_role = format!("trust-contrast-trusted-{index}");
        let other_role = format!("trust-contrast-other-{index}");
        let trusted_arn = emulator
            .given_role(&trusted_role, &trust_for(SERVICE_PRINCIPAL))
            .await;
        let other_arn = emulator
            .given_role(&other_role, &trust_for(row.other_principal))
            .await;
        // The SAME permission document on both roles: the only difference between them is
        // which principal their trust document admits.
        let grant = every_s3_action_on(BUCKET);
        emulator.given_role_policy(&trusted_role, &grant).await;
        emulator.given_role_policy(&other_role, &grant).await;

        let refused_job = format!("trust-contrast-refused-{index}");
        let refused_output = output_path(&refused_job, row.trailing_separator);
        let (status, answered) = emulator
            .sagemaker(
                "CreateTrainingJob",
                &training_body(row, &refused_job, &other_arn, &refused_output),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered), message(&answered)),
            (
                400,
                "ValidationException".to_string(),
                could_not_assume_role(&other_arn)
            ),
            "case {:?}: a role that admits {} only backs no training job, got {status} with \
             {answered}",
            row.case,
            row.other_principal
        );
        // Nothing was stored under the refused name.
        let (status, described) = emulator
            .sagemaker(
                "DescribeTrainingJob",
                &json!({ "TrainingJobName": &refused_job }),
            )
            .await;
        assert_eq!(
            (status, described.get("TrainingJobStatus").cloned()),
            (400, None),
            "case {:?}: a refused create leaves the name absent, got {status} with {described}",
            row.case
        );

        let running_job = format!("trust-contrast-run-{index}");
        let running_output = output_path(&running_job, row.trailing_separator);
        let created = emulator
            .sagemaker_ok(
                row.case,
                "CreateTrainingJob",
                &training_body(row, &running_job, &trusted_arn, &running_output),
            )
            .await;
        assert_eq!(
            str_field(&created, "TrainingJobArn"),
            format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:training-job/{running_job}"),
            "case {:?}: the ARN the accepted job is minted under, got {created}",
            row.case
        );
        // The refused half's output location holds nothing: the prefix a job that was never
        // created would have written to is still empty once its trusted twin has been
        // submitted.
        let under_refused = listed_keys(&emulator, BUCKET, &prefix_of(&refused_output)).await;
        assert!(
            under_refused.is_empty(),
            "case {:?}: the refused job must write nothing under {refused_output}, got \
             {under_refused:?}",
            row.case
        );
        accepted.push((row.case, running_job, running_output));
    }

    for (case, job, output) in accepted {
        let read = settled(&emulator, case, &job).await;
        assert_eq!(
            (
                str_field(&read, "TrainingJobStatus"),
                read.get("FailureReason").cloned()
            ),
            ("Completed".to_string(), None),
            "case {case:?}: a job whose role SageMaker can assume performs its work, got {read}"
        );
        let artifact = format!("{}/{job}/output/model.tar.gz", output.trim_end_matches('/'));
        assert_eq!(
            read.pointer("/ModelArtifacts/S3ModelArtifacts")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            artifact,
            "case {case:?}: the artifact URI under the declared output path, got {read}"
        );
        // The object the describe names is an object Amazon S3 itself holds.
        let key = artifact
            .strip_prefix(&format!("s3://{BUCKET}/"))
            .expect("the output location is in this suite's bucket")
            .to_string();
        let listed = listed_keys(&emulator, BUCKET, &prefix_of(&output)).await;
        assert!(
            listed.contains(&key),
            "case {case:?}: Amazon S3 must list the artifact {key} under the output path, got \
             {listed:?}"
        );
        let (status, body) = emulator.s3("GET", &format!("/{BUCKET}/{key}"), &[]).await;
        assert!(
            status == 200 && !body.is_empty(),
            "case {case:?}: the artifact {key} must be readable, got {status} with {} bytes",
            body.len()
        );
    }
}

/// The `S3OutputPath` one half of a pair writes to, keyed by the job's own name so the two
/// halves of a row cannot write to the same location.
fn output_path(job: &str, trailing_separator: bool) -> String {
    let separator = if trailing_separator { "/" } else { "" };
    format!("s3://{BUCKET}/out/{job}{separator}")
}

/// The key prefix an `s3://` output location names inside [`BUCKET`].
fn prefix_of(output: &str) -> String {
    output
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .expect("the output location is in this suite's bucket")
        .to_string()
}
