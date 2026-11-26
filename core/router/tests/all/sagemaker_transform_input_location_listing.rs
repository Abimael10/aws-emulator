// SPDX-License-Identifier: CC0-1.0

//! What `CreateTransformJob` settles about the dataset it was handed BEFORE it creates the
//! job.
//!
//! The rule under test: **`CreateTransformJob` LISTS the Amazon S3 location
//! `TransformInput.DataSource.S3DataSource.S3Uri` names before it creates anything.** A
//! location Amazon S3 answers holds no object is refused HTTP 400 `ValidationException` and no
//! job of that name exists afterwards, because the refusal is pre-creation, while the same
//! call against a location that holds objects is accepted and the job it created runs to a
//! status that is not `Failed`.
//!
//! WHICH sentence the refusal states follows what the service settled about the location: a
//! LOCATION (an `S3Uri` ending in the key delimiter) whose listing ANSWERED and held nothing
//! is a prefix read and found absent, stated with the S3 URL and the Region and nothing to
//! re-check, while an answer that names the absence of the bucket itself leaves "nothing
//! there" and "not allowed to look" indistinguishable, so it carries the whole diagnosis AWS
//! words for an input data source — the S3 URL, the Region the call addressed, the execution
//! role of the model the job would run, the bucket that role has to be allowed to list, and
//! what Amazon S3 itself answered.
//!
//! The rule spans three services over ONE engine — Amazon S3 for the dataset, IAM for the
//! role the job reads under, and the SageMaker control plane — so neither crate's own test
//! target can ask it, and the suite lives here. The harness (the emulator, the request
//! builders and the poll-until-settled reading) is the one
//! [`crate::sagemaker_endpoint_serves_a_trained_model`] stands up, so the two suites cannot
//! drift apart in how they drive the flow.
//!
//! The rows differ along what the rule generalises over: how many objects the location holds
//! (none, one, several), whether its key names a prefix or one object, and which shape an
//! empty location takes (a sibling of a populated prefix, a deeper path under a populated
//! key, a prefix nothing was ever written under, a bucket this account does not hold).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control, emulator, iam, image, send, settled, ACCOUNT, REGION,
};

/// The bucket the dataset and the model artifact live in.
const BUCKET: &str = "transform-prefix-listing";

/// `PUT /<bucket>` — `CreateBucket`.
async fn given_bucket(router: &MultiServiceRouter, bucket: &str) {
    let answer = send(router, "s3", "PUT", &format!("/{bucket}"), &[], Vec::new()).await;
    assert_eq!(
        answer.status,
        200,
        "setup: the bucket {bucket} must be created, got {} {}",
        answer.status,
        answer.text()
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`.
async fn given_object(router: &MultiServiceRouter, bucket: &str, key: &str, body: &str) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}/{key}"),
        &[],
        body.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: the object {key} must be stored, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Hold the execution role in this account's IAM: it trusts the SageMaker service principal
/// and may read the bucket's objects and write under it.
async fn given_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "Service": service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL,
            },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:PutObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
            ],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "dataset-access"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The wire error shape a refusal names, as a client resolves it.
fn error_code(answered: &Value) -> String {
    answered
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split(':')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// The message a refusal carries, whichever casing the member arrived in.
fn error_message(answered: &Value) -> String {
    answered
        .get("message")
        .or_else(|| answered.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Comma-separated training records of ONE feature column, the target a linear function of
/// it, so the fitted model scores a dataset of single values.
fn training_data(rows: usize) -> String {
    (0..rows)
        .map(|row| {
            let feature = (row + 1) as f64;
            format!("{},{feature}\n", 3.0 + feature)
        })
        .collect()
}

/// One `CreateTransformJob` request against the location `s3_uri`.
fn transform_request(job: &str, model: &str, s3_uri: &str, output: &str) -> Value {
    json!({
        "TransformJobName": job,
        "ModelName": model,
        "TransformInput": {
            "DataSource": {"S3DataSource": {"S3DataType": "S3Prefix", "S3Uri": s3_uri}},
            "ContentType": "text/csv",
            "SplitType": "Line",
        },
        "TransformOutput": {"S3OutputPath": output},
        "TransformResources": {"InstanceType": "ml.m5.large", "InstanceCount": 1},
    })
}

/// Train a model on the bucket's own data and register it around the artifact that run
/// produced, so a transform job driven against it has a model that really scores records.
async fn given_trained_model(router: &MultiServiceRouter, role_arn: &str) -> String {
    let name = "prefix-listing-model";
    given_object(router, BUCKET, "train/train.csv", &training_data(24)).await;
    let (status, answered) = control(
        router,
        "CreateTrainingJob",
        &json!({
            "TrainingJobName": name,
            "AlgorithmSpecification": {"TrainingImage": image(), "TrainingInputMode": "File"},
            "RoleArn": role_arn,
            "InputDataConfig": [{
                "ChannelName": "train",
                "DataSource": {"S3DataSource": {
                    "S3DataType": "S3Prefix",
                    "S3Uri": format!("s3://{BUCKET}/train/"),
                }},
                "ContentType": "text/csv",
            }],
            "OutputDataConfig": {"S3OutputPath": format!("s3://{BUCKET}/train/out/")},
            "ResourceConfig": {
                "InstanceType": "ml.m5.large",
                "InstanceCount": 1,
                "VolumeSizeInGB": 10,
            },
            "StoppingCondition": {"MaxRuntimeInSeconds": 3600},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the training job must be accepted, got {status} {answered}"
    );
    let trained = settled(
        router,
        "setup",
        "DescribeTrainingJob",
        &json!({"TrainingJobName": name}),
        "TrainingJobStatus",
        "Completed",
    )
    .await;
    let artifact = trained
        .pointer("/ModelArtifacts/S3ModelArtifacts")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the completed job names no artifact: {trained}"))
        .to_string();
    let (status, answered) = control(
        router,
        "CreateModel",
        &json!({
            "ModelName": name,
            "PrimaryContainer": {"Image": image(), "ModelDataUrl": artifact},
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the model must be registered, got {status} {answered}"
    );
    name.to_string()
}

/// One row of the table: the location the request names and whether it holds objects.
struct Row {
    case: &'static str,
    /// The bucket the request's `S3Uri` addresses, or `None` for the one the suite holds.
    bucket: Option<&'static str>,
    /// The key the request's `S3Uri` names under that bucket.
    key: &'static str,
    /// Whether Amazon S3 holds an object under it.
    populated: bool,
    /// What Amazon S3 itself ANSWERED about the location, which the refusal reports after the
    /// location sentence — and `None` for a listing that answered and merely held nothing.
    s3_answer: Option<&'static str>,
}

/// RULE: `CreateTransformJob` lists the location its `TransformInput` names before it creates
/// the job — a location holding no object is refused and leaves no job, a populated one is
/// accepted and its job is not `Failed`.
#[tokio::test]
async fn create_transform_job_lists_the_location_its_input_names() {
    let router = emulator();
    given_bucket(&router, BUCKET).await;
    let role_arn = given_role(&router, "transform-prefix-role").await;
    let model = given_trained_model(&router, &role_arn).await;
    // The dataset the populated locations hold: single-feature records, one per line.
    given_object(&router, BUCKET, "one/records.csv", "1\n2\n3\n").await;
    given_object(&router, BUCKET, "many/part-0001.csv", "4\n5\n").await;
    given_object(&router, BUCKET, "many/part-0002.csv", "6\n").await;

    let rows = [
        Row {
            case: "a prefix holding one object",
            bucket: None,
            key: "one/",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "a prefix holding several objects",
            bucket: None,
            key: "many/",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "a key naming one object",
            bucket: None,
            key: "one/records.csv",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "an empty prefix beside a populated one",
            bucket: None,
            key: "empty/",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "an empty prefix deeper under a populated key",
            bucket: None,
            key: "one/records.csv/deeper/",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "a prefix nothing was ever written under",
            bucket: None,
            key: "untouched/",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "a bucket this account does not hold",
            bucket: Some("transform-prefix-listing-absent"),
            key: "data/",
            populated: false,
            // Amazon S3 "Error responses": `NoSuchBucket` — "The specified bucket does not
            // exist".
            s3_answer: Some("The specified bucket does not exist"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let job = format!("prefix-listing-{index}");
        let s3_uri = format!("s3://{}/{}", row.bucket.unwrap_or(BUCKET), row.key);
        let (status, answered) = control(
            &router,
            "CreateTransformJob",
            &transform_request(
                &job,
                &model,
                &s3_uri,
                &format!("s3://{BUCKET}/out/{index}/"),
            ),
        )
        .await;
        let (read_status, read) = control(
            &router,
            "DescribeTransformJob",
            &json!({"TransformJobName": job}),
        )
        .await;
        if row.populated {
            assert_eq!(
                status, 200,
                "case {:?}: a location holding objects must be accepted, got {status} \
                 {answered}",
                row.case
            );
            assert_eq!(
                read_status, 200,
                "case {:?}: the accepted job must be readable, got {read_status} {read}",
                row.case
            );
            // The job RUNS: one whose dataset and model are both there reaches the completed
            // status the clock owes it, never the failed one.
            let settled_read = settled(
                &router,
                row.case,
                "DescribeTransformJob",
                &json!({"TransformJobName": job}),
                "TransformJobStatus",
                "Completed",
            )
            .await;
            let reason = settled_read.get("FailureReason");
            assert!(
                reason.is_none(),
                "case {:?}: a completed job carries no failure reason, got {reason:?}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: a location holding no object must be refused, got {status} {answered}",
            row.case
        );
        let code = error_code(&answered);
        assert_eq!(
            code, "ValidationException",
            "case {:?}: expected a ValidationException, got {code:?} with {answered}",
            row.case
        );
        let message = error_message(&answered);
        let bucket = row.bucket.unwrap_or(BUCKET);
        // What the service settled about the location decides the sentence: a location whose
        // listing answered and merely held nothing is a prefix that is not there, while an
        // answer naming the absence of the bucket carries the whole diagnosis AWS words for an
        // input data source — the S3 URL the request named, the Region it addressed, the
        // execution role of the model the job would run and the bucket that role has to be
        // allowed to list — and then what Amazon S3 itself answered.
        let expected = match row.s3_answer {
            None if s3_uri.ends_with('/') => format!(
                "S3 prefix \"{s3_uri}\" given in input data source does not exist. Please ensure \
                 that the bucket exists in the selected region ({REGION}) and that objects \
                 exist under that S3 prefix."
            ),
            answer => {
                let clause = answer
                    .map(|answer| format!(" Error message from S3: {answer}"))
                    .unwrap_or_default();
                format!(
                    "No S3 objects found under S3 URL \"{s3_uri}\" given in input data source. \
                     Please ensure that the bucket exists in the selected region ({REGION}), \
                     that objects exist under that S3 prefix, and that the role \"{role_arn}\" \
                     has \"s3:ListBucket\" permissions on bucket \"{bucket}\".{clause}"
                )
            }
        };
        assert_eq!(
            message, expected,
            "case {:?}: expected the refusal the service settled for {s3_uri:?}, got {message:?}",
            row.case
        );
        // The refusal is PRE-CREATION, so it leaves nothing behind: the name is not taken.
        assert_eq!(
            read_status, 400,
            "case {:?}: the refused create must leave no job, got {read_status} {read}",
            row.case
        );
        let read_code = error_code(&read);
        assert_eq!(
            read_code, "ResourceNotFound",
            "case {:?}: expected the job not to be found, got {read_code:?} with {read}",
            row.case
        );
    }
}
