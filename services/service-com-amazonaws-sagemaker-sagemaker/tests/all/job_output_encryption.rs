// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the SERVER-SIDE ENCRYPTION the Amazon S3 objects a SageMaker job
//! writes carry — SageMaker, Amazon S3, IAM and KMS mounted on ONE shared engine, the way
//! `core/router` assembles every service, so every bucket, key and object here is made through
//! the owning service's own operations and what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: when the output configuration of a job's work declares a KMS key, every
//! object SageMaker writes for that output is written with SSE-KMS under that key — so an
//! Amazon S3 `HeadObject` or `GetObject` on it reports `x-amz-server-side-encryption: aws:kms`
//! and names that key in `x-amz-server-side-encryption-aws-kms-key-id`. A configuration that
//! declares no key asks for no encryption of its own, so the object carries whatever the
//! destination bucket applies by default ("if you don't provide a KMS key ID, SageMaker uses the
//! default KMS key for Amazon S3 for your role's account", `OutputDataConfig$KmsKeyId`), which
//! for a bucket nothing has configured is SSE-S3 (`AES256`).
//!
//! The member says the key covers the whole of the job's output — "the KmsKeyId is applied to
//! all outputs" (`ProcessingOutputConfig$KmsKeyId`) — so every object of every declared output
//! is asserted, not just the first.
//!
//! The tables span what the rule generalises over: WHICH output configuration declares the key
//! (a processing job's `ProcessingOutputConfig`, a training job's `OutputDataConfig`), the FORM
//! the reference takes (a key ARN, a bare key id, an alias name — the spellings `KmsKeyId`
//! documents), and a configuration that declares no key at all. A reference KMS holds no key
//! for has its own case: the write cannot be made, so the job fails and leaves nothing.
//!
//! Every wait here is a poll for the CONDITION — the status the job reached, the object Amazon
//! S3 serves — never a pause of a fixed length, and the one deadline that bounds a poll is far
//! longer than any span this state gives a job's work, so it bounds a BROKEN emulator rather
//! than the condition.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, and so the Region whose endpoint holds the keys
/// those calls name: a KMS key is a regional resource, and an object and the key it is
/// encrypted under live at the same endpoint.
const REGION: &str = "us-east-1";

/// The account id the emulator mints its ARNs under.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The header an object's encryption algorithm is reported in, and the one naming the KMS key
/// it was encrypted under — the `@httpHeader` members `HeadObject` and `GetObject` bind
/// (`smithy_query(query="operation",operation="HeadObject")` reports `ServerSideEncryption:
/// {httpHeader: "x-amz-server-side-encryption"}` and `SSEKMSKeyId:
/// {httpHeader: "x-amz-server-side-encryption-aws-kms-key-id"}`).
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The `ServerSideEncryption` value that is SSE-KMS, and the one that is SSE-S3 — the base
/// default a bucket nothing has configured applies
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` publishes both).
const SSE_KMS: &str = "aws:kms";
const SSE_S3: &str = "AES256";

/// How long a case polls for a condition before it fails, and how often it looks again.
///
/// GENEROUS on purpose: what is under test is WHAT the job wrote and under which encryption,
/// never how fast it got there, so the deadline bounds a broken emulator and not the condition.
const SETTLE_BUDGET: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// The `ProcessingJobStatus` and `TrainingJobStatus` members a job's work has ENDED in.
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// Fail `case` unless there is still time to look again for `what`, then wait one poll interval.
async fn look_again(deadline: Instant, case: &str, what: &str, last: &str) {
    assert!(
        Instant::now() < deadline,
        "case {case:?}: {what} did not hold within {SETTLE_BUDGET:?}; last saw {last}"
    );
    tokio::time::sleep(SETTLE_POLL).await;
}

/// A SigV4 credential scope naming `service`: it is what routes a request to that service. The
/// key id and signature are not credentials — this harness turns request authentication off, so
/// nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// SageMaker, Amazon S3, IAM and KMS on ONE shared engine, the way the router mounts every
/// service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One awsJson1_1 call, as a client makes it: `target` is the `X-Amz-Target` prefix the
    /// service publishes and `signing` the credential scope's service name.
    async fn json(&self, signing: &str, target: &str, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing}.{REGION}.amazonaws.com/"))
            .header("host", format!("{signing}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target}.{operation}"))
            .header("authorization", authorization(signing))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker call.
    async fn sagemaker(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.json("sagemaker", "SageMaker", operation, body).await
    }

    /// One SageMaker call that must be served.
    async fn sagemaker_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.sagemaker(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {answered}"
        );
        answered
    }

    /// A customer managed KMS key, made with KMS's own `CreateKey`, as `(key id, key ARN)`.
    async fn created_key(&self, case: &str, description: &str) -> (String, String) {
        let (status, answered) = self
            .json(
                "kms",
                "TrentService",
                "CreateKey",
                &json!({ "Description": description }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateKey must succeed, got {status} with {answered}"
        );
        let text = |member: &str| {
            answered["KeyMetadata"][member]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a created key carries {member}, got {answered}")
                })
                .to_string()
        };
        (text("KeyId"), text("Arn"))
    }

    /// An alias of `key_id`, made with KMS's own `CreateAlias`.
    async fn created_alias(&self, case: &str, alias: &str, key_id: &str) {
        let (status, answered) = self
            .json(
                "kms",
                "TrentService",
                "CreateAlias",
                &json!({ "AliasName": alias, "TargetKeyId": key_id }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAlias must succeed, got {status} with {answered}"
        );
    }

    /// One path-style Amazon S3 request: its status, its response headers and its body.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> (u16, http::HeaderMap, Vec<u8>) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            response.headers().clone(),
            response.body().to_vec(),
        )
    }

    /// `PUT /<bucket>` — `CreateBucket`.
    async fn given_bucket(&self, bucket: &str) {
        let (status, _headers, body) = self.s3("PUT", &format!("/{bucket}"), &[]).await;
        assert_eq!(
            status,
            200,
            "the bucket {bucket} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject`.
    async fn given_object(&self, bucket: &str, key: &str, body: &str) {
        let (status, _headers, answered) = self
            .s3("PUT", &format!("/{bucket}/{key}"), body.as_bytes())
            .await;
        assert_eq!(
            status,
            200,
            "the object {key} must be stored, got {status} {}",
            String::from_utf8_lossy(&answered)
        );
    }

    /// One IAM call (`awsQuery`), as a client makes it.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(
                &percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
                    .to_string(),
            );
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// Hold `role_name` in IAM, trusting the SageMaker service principal and carrying the
    /// Amazon S3 grant a job's own reads and writes are made under, and answer its ARN.
    async fn given_job_role(&self, role_name: &str, bucket: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": SERVICE_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let (status, body) = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", role_name),
                    ("AssumeRolePolicyDocument", &trust),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "the role {role_name} must be created, got {status} {body}"
        );
        let grant = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": ["s3:GetObject", "s3:PutObject", "s3:ListBucket"],
                "Resource": [
                    format!("arn:aws:s3:::{bucket}"),
                    format!("arn:aws:s3:::{bucket}/*"),
                ],
            }],
        })
        .to_string();
        let (status, body) = self
            .iam(
                "PutRolePolicy",
                &[
                    ("RoleName", role_name),
                    ("PolicyName", "job-output-access"),
                    ("PolicyDocument", &grant),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "the role {role_name} must carry its grant, got {status} {body}"
        );
        format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")
    }

    /// The keys Amazon S3 lists under `prefix`, read through `ListObjectsV2`.
    async fn listed_keys(&self, bucket: &str, prefix: &str) -> Vec<String> {
        let (status, _headers, body) = self
            .s3("GET", &format!("/{bucket}?list-type=2&prefix={prefix}"), &[])
            .await;
        assert_eq!(
            status,
            200,
            "the listing of {bucket}/{prefix} must be served, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        let text = String::from_utf8_lossy(&body).to_string();
        let mut keys: Vec<String> = text
            .split("<Key>")
            .skip(1)
            .filter_map(|tail| tail.split_once("</Key>").map(|(key, _)| key.to_string()))
            .collect();
        keys.sort();
        keys
    }

    /// Look again until Amazon S3 lists at least `count` keys under `prefix`, and answer them.
    async fn keys_until(&self, case: &str, bucket: &str, prefix: &str, count: usize) -> Vec<String> {
        let deadline = Instant::now() + SETTLE_BUDGET;
        loop {
            let held = self.listed_keys(bucket, prefix).await;
            if held.len() >= count {
                return held;
            }
            look_again(
                deadline,
                case,
                &format!("{bucket}/{prefix} holds {count} object(s)"),
                &format!("{held:?}"),
            )
            .await;
        }
    }

    /// The headers one stored object's metadata read reports, polled until Amazon S3 serves it.
    async fn head_object(&self, case: &str, bucket: &str, key: &str) -> http::HeaderMap {
        let deadline = Instant::now() + SETTLE_BUDGET;
        loop {
            let (status, headers, body) = self.s3("HEAD", &format!("/{bucket}/{key}"), &[]).await;
            if status == 200 {
                return headers;
            }
            look_again(
                deadline,
                case,
                &format!("Amazon S3 serves the metadata of {bucket}/{key}"),
                &format!("{status} {}", String::from_utf8_lossy(&body)),
            )
            .await;
        }
    }

    /// The headers the read that serves the bytes reports for one object.
    async fn get_object_headers(&self, case: &str, bucket: &str, key: &str) -> http::HeaderMap {
        let (status, headers, body) = self.s3("GET", &format!("/{bucket}/{key}"), &[]).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: GetObject of {bucket}/{key} must be served, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        headers
    }

    /// Poll one job read until it reports a status its work does not leave, and answer it.
    async fn ended(&self, case: &str, operation: &str, request: &Value, member: &str) -> Value {
        let deadline = Instant::now() + SETTLE_BUDGET;
        loop {
            let read = self.sagemaker_ok(case, operation, request).await;
            let status = read
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if TERMINAL_STATUSES.contains(&status.as_str()) {
                return read;
            }
            look_again(
                deadline,
                case,
                "the job reached a terminal status",
                &read.to_string(),
            )
            .await;
        }
    }
}

/// One response header, as text.
fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// An ECR-shaped path for a container of the caller's own.
fn own_image(repository: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{repository}:1")
}

/// The directories the processing rows below write their two outputs from: both under
/// `/opt/ml/processing/`, which is the tree a processing container's paths name.
const FIRST_OUTPUT_DIRECTORY: &str = "/opt/ml/processing/output";
const SECOND_OUTPUT_DIRECTORY: &str = "/opt/ml/processing/reports";

/// A `CreateProcessingJob` body whose container writes two files under each of two output
/// directories, with `output_config` as its `ProcessingOutputConfig`.
fn processing_request(name: &str, role: &str, output_config: Value) -> Value {
    let program = format!(
        "mkdir -p {FIRST_OUTPUT_DIRECTORY} {SECOND_OUTPUT_DIRECTORY} && \
         echo first > {FIRST_OUTPUT_DIRECTORY}/first.txt && \
         echo second > {FIRST_OUTPUT_DIRECTORY}/second.txt && \
         echo report > {SECOND_OUTPUT_DIRECTORY}/report.txt"
    );
    json!({
        "ProcessingJobName": name,
        "AppSpecification": {
            "ImageUri": own_image("my-processor"),
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
        "ProcessingOutputConfig": output_config,
    })
}

/// The two declared outputs of the processing rows: one per directory the program writes.
fn two_outputs(bucket: &str) -> Value {
    json!([
        {
            "OutputName": "result",
            "AppManaged": false,
            "S3Output": {
                "S3Uri": format!("s3://{bucket}/output/"),
                "LocalPath": FIRST_OUTPUT_DIRECTORY,
                "S3UploadMode": "EndOfJob",
            },
        },
        {
            "OutputName": "reports",
            "AppManaged": false,
            "S3Output": {
                "S3Uri": format!("s3://{bucket}/reports/"),
                "LocalPath": SECOND_OUTPUT_DIRECTORY,
                "S3UploadMode": "EndOfJob",
            },
        },
    ])
}

/// RULE: every object a processing job writes for its declared outputs is encrypted with the
/// key `ProcessingOutputConfig.KmsKeyId` names — "the KmsKeyId is applied to all outputs" — and
/// a configuration naming no key leaves the destination bucket's own default encryption.
#[tokio::test]
async fn a_processing_job_encrypts_every_output_object_with_the_key_it_declared() {
    /// Which spelling of the declared key a row sends, resolved once the key exists.
    enum Declared {
        /// `ProcessingOutputConfig` carries no `KmsKeyId` at all.
        None,
        /// The key's own ARN.
        Arn,
        /// The bare key id.
        Id,
        /// An alias of the key, as `alias/<name>`.
        Alias,
    }

    // The rows differ along the FORM the declared reference takes — the spellings
    // `ProcessingOutputConfig$KmsKeyId` documents ("an ID of a KMS key, ARN of a KMS key, or
    // alias of a KMS key") — and along declaring no key at all.
    let rows: [(&str, Declared, &str); 4] = [
        ("no key declared", Declared::None, SSE_S3),
        ("the key's ARN", Declared::Arn, SSE_KMS),
        ("a bare key id", Declared::Id, SSE_KMS),
        ("an alias of the key", Declared::Alias, SSE_KMS),
    ];

    for (index, (case, declared, expected_algorithm)) in rows.into_iter().enumerate() {
        let bucket = "processing-output-encryption";
        let emulator = Emulator::new();
        emulator.given_bucket(bucket).await;
        let role = emulator.given_job_role("processing-output-role", bucket).await;
        let (key_id, key_arn) = emulator.created_key(case, "processing job output").await;
        let alias = format!("alias/processing-output-{index}");
        emulator.created_alias(case, &alias, &key_id).await;

        let mut output_config = json!({ "Outputs": two_outputs(bucket) });
        let reference = match declared {
            Declared::None => None,
            Declared::Arn => Some(key_arn.clone()),
            Declared::Id => Some(key_id.clone()),
            Declared::Alias => Some(alias.clone()),
        };
        if let Some(reference) = &reference {
            output_config["KmsKeyId"] = json!(reference);
        }

        let job = format!("encryptedoutputjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateProcessingJob",
                &processing_request(&job, &role, output_config),
            )
            .await;
        let read = emulator
            .ended(
                case,
                "DescribeProcessingJob",
                &json!({ "ProcessingJobName": &job }),
                "ProcessingJobStatus",
            )
            .await;
        assert_eq!(
            read.get("ProcessingJobStatus").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: the job's program exited zero, so it completes, got {read}"
        );

        // Both declared outputs, so what is asserted is the key applied to ALL of them.
        for (prefix, count) in [("output/", 2), ("reports/", 1)] {
            let keys = emulator.keys_until(case, bucket, prefix, count).await;
            assert_eq!(
                keys.len(),
                count,
                "case {case:?}: expected {count} object(s) under {prefix}, got {keys:?}"
            );
            for object in &keys {
                // Both reads report the encryption the object is STORED under.
                for (read_name, headers) in [
                    (
                        "HeadObject",
                        emulator.head_object(case, bucket, object).await,
                    ),
                    (
                        "GetObject",
                        emulator.get_object_headers(case, bucket, object).await,
                    ),
                ] {
                    let algorithm = header(&headers, SSE_HEADER);
                    assert_eq!(
                        algorithm.as_deref(),
                        Some(expected_algorithm),
                        "case {case:?}: {read_name} of {object} must report \
                         {expected_algorithm:?}, got {algorithm:?}"
                    );
                    // Whichever spelling the request used, the key the object records is the
                    // one KMS resolved it to, which is that key's ARN.
                    let reported_key = header(&headers, SSE_KEY_HEADER);
                    let expected_key = reference.as_ref().map(|_| key_arn.clone());
                    assert_eq!(
                        reported_key, expected_key,
                        "case {case:?}: {read_name} of {object} must name the declared key \
                         {expected_key:?}, got {reported_key:?}"
                    );
                }
            }
        }
    }
}

/// RULE (the same one, seen from its failure side): the declared key is really the key the
/// write is made under, so a reference KMS holds no key for is a write that cannot be made —
/// the job fails on its upload leg and leaves no object behind.
#[tokio::test]
async fn a_processing_job_whose_declared_key_resolves_to_no_key_fails_and_writes_nothing() {
    let case = "a key id of this account that no CreateKey ever minted";
    let bucket = "processing-output-dangling-key";
    let emulator = Emulator::new();
    emulator.given_bucket(bucket).await;
    let role = emulator.given_job_role("dangling-key-role", bucket).await;

    let job = "danglingkeyjob";
    let mut output_config = json!({ "Outputs": two_outputs(bucket) });
    // The hyphenated 8-4-4-4-12 text of a key id, which no key of this endpoint carries.
    output_config["KmsKeyId"] = json!("4f5fb9d9-3e64-4ee1-a07a-0cf0bcfee9f9");
    emulator
        .sagemaker_ok(
            case,
            "CreateProcessingJob",
            &processing_request(job, &role, output_config),
        )
        .await;

    let read = emulator
        .ended(
            case,
            "DescribeProcessingJob",
            &json!({ "ProcessingJobName": job }),
            "ProcessingJobStatus",
        )
        .await;
    assert_eq!(
        read.get("ProcessingJobStatus").and_then(Value::as_str),
        Some("Failed"),
        "case {case:?}: an output that cannot be written fails the job, got {read}"
    );
    let reason = read
        .get("FailureReason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        reason.contains("Data upload failed"),
        "case {case:?}: the reason must name the upload leg that failed, got {reason:?}"
    );
    let keys = emulator.listed_keys(bucket, "").await;
    assert!(
        keys.is_empty(),
        "case {case:?}: a job that failed its upload leaves no object, got {keys:?}"
    );
}

/// RULE (the same rule, another output configuration): a training job's model artifact is one
/// of the job's outputs, so it is encrypted with the key `OutputDataConfig.KmsKeyId` names.
#[tokio::test]
async fn a_training_job_encrypts_its_model_artifact_with_the_key_it_declared() {
    // The rows differ along whether the output configuration declares a key.
    let rows: [(&str, bool, &str); 2] = [
        ("no key declared", false, SSE_S3),
        ("the key's ARN", true, SSE_KMS),
    ];

    for (index, (case, declares_key, expected_algorithm)) in rows.into_iter().enumerate() {
        let bucket = "training-output-encryption";
        let emulator = Emulator::new();
        emulator.given_bucket(bucket).await;
        let role = emulator.given_job_role("training-output-role", bucket).await;
        let (_key_id, key_arn) = emulator.created_key(case, "training job output").await;
        // Comma-separated records: the first column is the target and the rest the features,
        // which is the form a training channel carries.
        let records: String = (0..8)
            .map(|row| format!("{},{},{}\n", row * 2, row, row + 1))
            .collect();
        emulator
            .given_object(bucket, "training/train.csv", &records)
            .await;

        let job = format!("encryptedartifactjob{index}");
        let mut output = json!({ "S3OutputPath": format!("s3://{bucket}/out/") });
        if declares_key {
            output["KmsKeyId"] = json!(key_arn);
        }
        emulator
            .sagemaker_ok(
                case,
                "CreateTrainingJob",
                &json!({
                    "TrainingJobName": &job,
                    "AlgorithmSpecification": {
                        "TrainingImage": own_image("my-trainer"),
                        "TrainingInputMode": "File",
                    },
                    "RoleArn": role,
                    "InputDataConfig": [{
                        "ChannelName": "train",
                        "DataSource": {
                            "S3DataSource": {
                                "S3DataType": "S3Prefix",
                                "S3Uri": format!("s3://{bucket}/training/train.csv"),
                            },
                        },
                        "ContentType": "text/csv",
                    }],
                    "OutputDataConfig": output,
                    "ResourceConfig": {
                        "InstanceType": "ml.m5.large",
                        "InstanceCount": 1,
                        "VolumeSizeInGB": 10,
                    },
                    "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
                }),
            )
            .await;

        let read = emulator
            .ended(
                case,
                "DescribeTrainingJob",
                &json!({ "TrainingJobName": &job }),
                "TrainingJobStatus",
            )
            .await;
        assert_eq!(
            read.get("TrainingJobStatus").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: the run produced an artifact, so the job completes, got {read}"
        );

        // The artifact goes under `<S3OutputPath>/<TrainingJobName>/output/model.tar.gz`, the
        // location the job itself reports.
        let artifact = format!("out/{job}/output/model.tar.gz");
        let reported = read
            .pointer("/ModelArtifacts/S3ModelArtifacts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reported,
            format!("s3://{bucket}/{artifact}"),
            "case {case:?}: the job must name its own artifact, got {reported:?}"
        );
        let headers = emulator.head_object(case, bucket, &artifact).await;
        let algorithm = header(&headers, SSE_HEADER);
        assert_eq!(
            algorithm.as_deref(),
            Some(expected_algorithm),
            "case {case:?}: HeadObject of {artifact} must report {expected_algorithm:?}, got \
             {algorithm:?}"
        );
        let reported_key = header(&headers, SSE_KEY_HEADER);
        let expected_key = declares_key.then(|| key_arn.clone());
        assert_eq!(
            reported_key, expected_key,
            "case {case:?}: HeadObject of {artifact} must name the declared key \
             {expected_key:?}, got {reported_key:?}"
        );
    }
}
