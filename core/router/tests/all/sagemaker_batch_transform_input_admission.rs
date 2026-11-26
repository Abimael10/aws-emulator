// SPDX-License-Identifier: CC0-1.0

//! What `CreateTransformJob` TELLS a client about the `TransformInput` location it resolved,
//! driven end to end over the admission flow a batch-transform client performs: a bucket, the
//! dataset and the model artifact in Amazon S3, an execution role in IAM, `CreateModel`, and
//! then one transform job per input location.
//!
//! The rule under test: **a transform job's input location is resolved before the job exists,
//! and the refusal states WHAT THE SERVICE SETTLED about it.** A LOCATION — an `S3Uri` ending
//! in the key delimiter, the form `TransformS3DataSource$S3Uri`'s own doc writes a key name
//! prefix that names a location in — whose listing ANSWERED and holds nothing is a prefix the
//! service read and found absent, so it is refused 400 `ValidationException` stating that the
//! prefix does not exist, with the S3 URL the request named and the Region the call addressed
//! and nothing for the caller to re-check. Every other condition — a key name prefix no object
//! is matched by, an answer naming the absence of the bucket itself — leaves "nothing there"
//! and "not allowed to look" indistinguishable from the service's side, so the refusal carries
//! the whole diagnosis AWS words for an input data source: the S3 URL, the Region, the
//! execution role of the model the job would run, the bucket that role has to be allowed to
//! list, and then whatever Amazon S3 itself answered. Either way nothing is persisted: the
//! refused name appears in no listing and no job of it can be described.
//!
//! Why the distinction matters to a client: the diagnosis that names a grant to re-check is
//! also what a caller reads while a freshly created role's grants are still taking effect, so
//! a client that retries on it settles only on a refusal that states a fact it cannot wait out.
//!
//! The rule spans three services over ONE engine — Amazon S3 for the dataset and the artifact,
//! IAM for the role the job reads under, and the SageMaker control plane — so neither crate's
//! own test target can ask it, and the suite lives here. The harness (the emulator and the
//! request builders) is the one [`crate::sagemaker_endpoint_serves_a_trained_model`] stands up,
//! so the suites cannot drift apart in how they drive the flow.
//!
//! The rows differ along what the rule generalises over: the form of the `S3Uri` (a location
//! ending in the delimiter, a key name prefix that does not), what Amazon S3 answered about it
//! (a listing that held nothing, an answer naming the absence of the bucket), how many objects
//! the location holds (none, one, several), and the depth of the key.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control, emulator, iam, send, ACCOUNT, REGION,
};

/// The bucket the dataset and the model artifact live in.
const BUCKET: &str = "batch-transform-admission";

/// The model every row's job names, registered around the artifact the flow uploads.
const MODEL: &str = "batch-transform-admission-model";

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
async fn given_object(router: &MultiServiceRouter, key: &str, body: &[u8]) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}/{key}"),
        &[],
        body.to_vec(),
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

/// Hold the execution role in this account's IAM: it trusts the SageMaker service principal and
/// may read the dataset, read the artifact and write the transform output.
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
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:ListBucket"],
                "Resource": [
                    format!("arn:aws:s3:::{BUCKET}"),
                    format!("arn:aws:s3:::{BUCKET}/*"),
                ],
            },
            {
                "Effect": "Allow",
                "Action": ["s3:PutObject"],
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
        ],
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
            ("PolicyName", "batch-transform-access"),
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

/// One `CreateTransformJob` request against the location `s3_uri`.
fn transform_request(job: &str, s3_uri: &str, output: &str) -> Value {
    json!({
        "TransformJobName": job,
        "ModelName": MODEL,
        "TransformInput": {
            "DataSource": {"S3DataSource": {"S3DataType": "S3Prefix", "S3Uri": s3_uri}},
            "ContentType": "text/csv",
            "SplitType": "Line",
        },
        "TransformOutput": {"S3OutputPath": output},
        "TransformResources": {"InstanceType": "ml.m4.xlarge", "InstanceCount": 1},
    })
}

/// One row of the table: the location the request hands the job, and what the service settles
/// about it.
struct Row {
    case: &'static str,
    /// The bucket the request's `S3Uri` addresses, or `None` for the one the flow holds.
    bucket: Option<&'static str>,
    /// The key the request's `S3Uri` names under that bucket.
    key: &'static str,
    /// Whether Amazon S3 holds an object under the location.
    populated: bool,
    /// What Amazon S3 itself ANSWERED about the location, which the whole diagnosis reports
    /// after the location sentence — and `None` for a listing that answered and merely held
    /// nothing.
    s3_answer: Option<&'static str>,
}

/// The sentence a LOCATION the listing read and found holding nothing is refused with.
fn prefix_does_not_exist(s3_uri: &str) -> String {
    format!(
        "S3 prefix \"{s3_uri}\" given in input data source does not exist. Please ensure that \
         the bucket exists in the selected region ({REGION}) and that objects exist under that \
         S3 prefix."
    )
}

/// The whole diagnosis AWS words for an input data source whose condition the service cannot
/// tell apart: the S3 URL, the Region, the model's execution role, the bucket that role has to
/// be allowed to list, and then whatever Amazon S3 itself answered.
fn no_s3_objects_found(
    s3_uri: &str,
    role_arn: &str,
    bucket: &str,
    s3_answer: Option<&str>,
) -> String {
    let clause = s3_answer
        .map(|answer| format!(" Error message from S3: {answer}"))
        .unwrap_or_default();
    format!(
        "No S3 objects found under S3 URL \"{s3_uri}\" given in input data source. Please ensure \
         that the bucket exists in the selected region ({REGION}), that objects exist under that \
         S3 prefix, and that the role \"{role_arn}\" has \"s3:ListBucket\" permissions on bucket \
         \"{bucket}\".{clause}"
    )
}

/// RULE: a transform job's input location is resolved before the job exists, and the refusal
/// states what the service settled about it — a location read and found empty is a prefix that
/// does not exist, while a condition the service cannot tell apart carries the whole diagnosis.
#[tokio::test]
async fn a_transform_input_location_is_refused_with_what_the_service_settled() {
    let router = emulator();
    given_bucket(&router, BUCKET).await;
    // The flow a batch-transform client performs: the model artifact and the dataset in Amazon
    // S3, the execution role in IAM, and the model registered around that artifact.
    given_object(&router, "model/artifact.tar.gz", b"model-artifact").await;
    given_object(&router, "input_populated/records.csv", b"1\n2\n3\n").await;
    given_object(&router, "input_many/part-0001.csv", b"4\n5\n").await;
    given_object(&router, "input_many/part-0002.csv", b"6\n").await;
    let role_arn = given_role(&router, "batch-transform-admission-role").await;
    let (status, answered) = control(
        &router,
        "CreateModel",
        &json!({
            "ModelName": MODEL,
            "PrimaryContainer": {
                "Image": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/transform:1"),
                "ModelDataUrl": format!("s3://{BUCKET}/model/artifact.tar.gz"),
            },
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the model must be registered around the artifact, got {status} {answered}"
    );

    let rows = [
        Row {
            case: "a location holding one object",
            bucket: None,
            key: "input_populated/",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "a location holding several objects",
            bucket: None,
            key: "input_many/",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "a key name prefix one stored object is matched by",
            bucket: None,
            key: "input_populated/records.csv",
            populated: true,
            s3_answer: None,
        },
        Row {
            case: "a location beside a populated one, holding nothing",
            bucket: None,
            key: "input_empty/",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "a deeper location under a stored key, holding nothing",
            bucket: None,
            key: "input_populated/records.csv/deeper/",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "a key name prefix nothing stored is matched by",
            bucket: None,
            key: "input_populated/absent.csv",
            populated: false,
            s3_answer: None,
        },
        Row {
            case: "a location in a bucket this account does not hold",
            bucket: Some("batch-transform-admission-absent"),
            key: "input/",
            populated: false,
            // Amazon S3 "Error responses": `NoSuchBucket` — "The specified bucket does not
            // exist".
            s3_answer: Some("The specified bucket does not exist"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let job = format!("batch-transform-admission-{index}");
        let bucket = row.bucket.unwrap_or(BUCKET);
        let s3_uri = format!("s3://{bucket}/{}", row.key);
        let (status, answered) = control(
            &router,
            "CreateTransformJob",
            &transform_request(&job, &s3_uri, &format!("s3://{BUCKET}/out/{index}/")),
        )
        .await;
        let (read_status, read) = control(
            &router,
            "DescribeTransformJob",
            &json!({"TransformJobName": job}),
        )
        .await;
        let (list_status, listed) = control(
            &router,
            "ListTransformJobs",
            &json!({"NameContains": job.clone()}),
        )
        .await;
        assert_eq!(
            list_status, 200,
            "case {:?}: the listing must be served, got {list_status} {listed}",
            row.case
        );
        let summaries = listed
            .get("TransformJobSummaries")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();

        if row.populated {
            assert_eq!(
                status, 200,
                "case {:?}: a location holding objects must be accepted, got {status} {answered}",
                row.case
            );
            assert_eq!(
                read_status, 200,
                "case {:?}: the accepted job must be readable, got {read_status} {read}",
                row.case
            );
            // The job the create admitted is the one a client polls: it starts out running, so
            // the status a caller reads straight after the call is not the failed one.
            let reported = read
                .get("TransformJobStatus")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_ne!(
                reported, "Failed",
                "case {:?}: an admitted job must not be born failed, got {read}",
                row.case
            );
            assert_eq!(
                summaries, 1,
                "case {:?}: the created job must be the one summary its name matches, got \
                 {listed}",
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
        let expected = match row.s3_answer {
            None if row.key.ends_with('/') => prefix_does_not_exist(&s3_uri),
            answer => no_s3_objects_found(&s3_uri, &role_arn, bucket, answer),
        };
        assert_eq!(
            message, expected,
            "case {:?}: expected the refusal the service settled for {s3_uri:?}, got {message:?}",
            row.case
        );
        // A prefix the service READ and found empty is a fact it settled, so the refusal asks
        // the caller to re-check no grant: that half of the diagnosis belongs to the conditions
        // it cannot see.
        if row.s3_answer.is_none() && row.key.ends_with('/') {
            assert!(
                !message.contains("s3:ListBucket") && !message.contains(&role_arn),
                "case {:?}: a prefix read and found empty states the fact alone, got {message:?}",
                row.case
            );
        }
        // The refusal is PRE-CREATION, so it leaves nothing behind: no job to describe and no
        // summary under that name.
        assert_eq!(
            read_status, 400,
            "case {:?}: the refused create must leave no job, got {read_status} {read}",
            row.case
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFound",
            "case {:?}: expected the job not to be found, got {read}",
            row.case
        );
        assert_eq!(
            summaries, 0,
            "case {:?}: the refused name must appear in no listing, got {listed}",
            row.case
        );
    }
}
