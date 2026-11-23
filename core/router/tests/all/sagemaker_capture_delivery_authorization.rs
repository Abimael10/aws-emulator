// SPDX-License-Identifier: CC0-1.0

//! Capture DELIVERY is authorized against the endpoint's own execution role, and nothing else
//! about the invocation depends on it.
//!
//! The rule under test: an endpoint whose deployment carries a `DataCaptureConfig` writes the
//! exchange it served under `DestinationS3Uri` exactly when IAM allows the deployed model's
//! execution role `s3:PutObject` on the object that write addresses — the destination bucket's
//! own policy refuses what it EXPLICITLY DENIES, and otherwise the grant the role itself carries
//! decides — and the inference itself answers HTTP 200 either way: capture is a by-product of
//! serving, so a destination the role may not write leaves the answer untouched and no object
//! behind it.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — IAM for the execution role whose grant
//! decides the delivery, Amazon S3 for the artifact bucket and for a SEPARATE capture bucket,
//! the SageMaker control plane to deploy, and the SageMaker runtime to invoke. The harness (the
//! emulator, the request builders and the poll-until-settled reading) is the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]), and the artifact is the one the
//! capture suite deploys ([`crate::sagemaker_endpoint_captures_invocations_to_s3`]), so neither
//! can drift from how this one stands the flow up.
//!
//! The rows differ along what the rule generalises over: the resource a grant names (the whole
//! capture bucket, the destination prefix, another prefix of the same bucket), the action it
//! names, whether the role carries a grant on the capture bucket at all, whether the destination
//! bucket's own policy denies the write, and the form the destination's own prefix takes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_captures_invocations_to_s3::booster;
use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, iam, image, invoke, send, settled, ACCOUNT,
};

/// The bucket the model artifact lives in, which every row's role may read.
const ARTIFACT_BUCKET: &str = "sagemaker-capture-artifact-bucket";

/// The bucket the capture objects are delivered to, which is NOT the artifact's: a capture
/// destination is a location of its own, so the grant that decides the delivery is its own too.
const CAPTURE_BUCKET: &str = "sagemaker-capture-destination-bucket";

/// A second capture bucket, which carries a POLICY of its own: within one account an explicit
/// `Deny` in the destination bucket's policy wins over the execution role's own grant, so a
/// delivery under the prefix it denies is one IAM withholds whatever the role is allowed.
const GOVERNED_BUCKET: &str = "sagemaker-capture-governed-bucket";

/// The label inside [`GOVERNED_BUCKET`] whose prefix that bucket's policy denies the write of.
const DENIED_LABEL: &str = "policy-denied";

/// The prefix every row's destination is laid out under, inside [`CAPTURE_BUCKET`].
const CAPTURE_PREFIX: &str = "monitoring/data-capture";

/// Where the artifact is stored, as a container's own `model.tar.gz`.
const ARTIFACT_KEY: &str = "model/xgboost-model.tar.gz";

/// The variant every endpoint below hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// The exchanges every row drives: one record of three features each, so each invocation is its
/// own capture record.
const PAYLOADS: [&str; 3] = ["0.5,0.5,0.5\n", "7.5,1.25,0.5\n", "-3,4,5\n"];

/// `PUT /<bucket>` — `CreateBucket` for a named bucket.
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

/// `PUT /<bucket>/<key>` with raw bytes — `PutObject` for an artifact that is not text.
async fn given_bytes(router: &MultiServiceRouter, bucket: &str, key: &str, body: &[u8]) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{bucket}/{key}"),
        &[],
        body.to_vec(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: the object {key} must be stored in {bucket}, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Hold a policy on [`GOVERNED_BUCKET`] that explicitly DENIES this account's principals the
/// write of anything under the one destination prefix, and says nothing about the rest of the
/// bucket — `PutBucketPolicy`.
async fn given_denying_policy(router: &MultiServiceRouter) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "s3:PutObject",
            "Resource":
                format!("arn:aws:s3:::{GOVERNED_BUCKET}/{CAPTURE_PREFIX}/{DENIED_LABEL}/*"),
        }],
    })
    .to_string();
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{GOVERNED_BUCKET}?policy"),
        &[],
        document.into_bytes(),
    )
    .await;
    assert!(
        answer.status == 200 || answer.status == 204,
        "setup: the destination bucket's policy must be stored, got {} {}",
        answer.status,
        answer.text()
    );
}

/// The keys `bucket` holds under `prefix` — `ListObjectsV2`, read off the listing it answers.
async fn keys_under(router: &MultiServiceRouter, bucket: &str, prefix: &str) -> Vec<String> {
    let answer = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "the listing of {prefix} must be served, got {} {}",
        answer.status,
        answer.text()
    );
    let listing = answer.text();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next().map(str::to_string))
        .collect()
}

/// The text of one object — `GetObject`.
async fn object_text(router: &MultiServiceRouter, bucket: &str, key: &str) -> String {
    let answer = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "the object {key} must be readable, got {} {}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// Hold a role in this account's IAM that trusts the SageMaker service principal, may read the
/// artifact object, and carries whatever `grant` the row adds beside that.
async fn given_role(router: &MultiServiceRouter, name: &str, grant: Option<Value>) -> String {
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
    let mut statements = vec![
        json!({
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{ARTIFACT_BUCKET}/{ARTIFACT_KEY}"),
        }),
        json!({
            "Effect": "Allow",
            "Action": "s3:ListBucket",
            "Resource": format!("arn:aws:s3:::{ARTIFACT_BUCKET}"),
        }),
        json!({
            "Effect": "Allow",
            "Action": "logs:*",
            "Resource": "*",
        }),
    ];
    statements.extend(grant);
    let permissions = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
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
            ("PolicyName", "capture-delivery"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Deploy the artifact behind `endpoint` under `role_arn`, capturing the whole exchange of every
/// invocation under `destination`, and answer once the endpoint is `InService`.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    endpoint: &str,
    role_arn: &str,
    destination: &str,
) {
    control_ok(
        router,
        "CreateModel",
        &json!({
            "ModelName": name,
            "PrimaryContainer": {
                "Image": image(),
                "ModelDataUrl": format!("s3://{ARTIFACT_BUCKET}/{ARTIFACT_KEY}"),
            },
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    let config = format!("{name}-config");
    control_ok(
        router,
        "CreateEndpointConfig",
        &json!({
            "EndpointConfigName": config,
            "ProductionVariants": [{
                "VariantName": VARIANT,
                "ModelName": name,
                "InstanceType": "ml.m5.large",
                "InitialInstanceCount": 1,
            }],
            "DataCaptureConfig": {
                "EnableCapture": true,
                "InitialSamplingPercentage": 100,
                "DestinationS3Uri": destination,
                "CaptureOptions": [{ "CaptureMode": "Input" }, { "CaptureMode": "Output" }],
            },
        }),
    )
    .await;
    control_ok(
        router,
        "CreateEndpoint",
        &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
    )
    .await;
    settled(
        router,
        case,
        "DescribeEndpoint",
        &json!({ "EndpointName": endpoint }),
        "EndpointStatus",
        "InService",
    )
    .await;
}

/// One row: the grant the endpoint's execution role carries on the capture bucket, the label its
/// destination is laid out under, and whether the delivery that grant decides happens.
struct Row {
    case: &'static str,
    /// The bucket the row's destination is in: the plain capture bucket, or the one carrying a
    /// policy of its own ([`GOVERNED_BUCKET`]).
    bucket: &'static str,
    /// The extra statement the row's role carries, when it carries one.
    grant: Option<Value>,
    /// The label the row's destination is laid out under, inside [`CAPTURE_PREFIX`].
    label: &'static str,
    /// Whether the destination is written with a trailing separator, which is a form a caller
    /// writes one in.
    trailing_separator: bool,
    /// Whether the capture objects the invocations produce are delivered.
    delivered: bool,
}

/// One statement allowing `action` on `resource`.
fn allow(action: &str, resource: String) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The table. The rows differ along the resource a grant names, the action it names, whether the
/// role carries a grant on the capture bucket at all, the form the destination takes, and whether
/// the destination bucket's own policy denies the write the role is otherwise allowed.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a grant on the whole capture bucket",
            bucket: CAPTURE_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{CAPTURE_BUCKET}/*"),
            )),
            label: "bucketwide",
            trailing_separator: false,
            delivered: true,
        },
        Row {
            case: "a grant scoped to the destination prefix",
            bucket: CAPTURE_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{CAPTURE_BUCKET}/{CAPTURE_PREFIX}/*"),
            )),
            label: "withwrite",
            trailing_separator: false,
            delivered: true,
        },
        Row {
            case: "a destination written with a trailing separator",
            bucket: CAPTURE_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{CAPTURE_BUCKET}/{CAPTURE_PREFIX}/*"),
            )),
            label: "withwriteslash",
            trailing_separator: true,
            delivered: true,
        },
        Row {
            case: "no grant on the capture bucket at all",
            bucket: CAPTURE_BUCKET,
            grant: None,
            label: "nowrite",
            trailing_separator: false,
            delivered: false,
        },
        Row {
            case: "a grant on another prefix of the same bucket",
            bucket: CAPTURE_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{CAPTURE_BUCKET}/elsewhere/*"),
            )),
            label: "otherprefix",
            trailing_separator: false,
            delivered: false,
        },
        Row {
            case: "a grant of another action on the destination prefix",
            bucket: CAPTURE_BUCKET,
            grant: Some(allow(
                "s3:GetObject",
                format!("arn:aws:s3:::{CAPTURE_BUCKET}/{CAPTURE_PREFIX}/*"),
            )),
            label: "readonly",
            trailing_separator: false,
            delivered: false,
        },
        Row {
            case: "a destination the bucket's own policy denies the write of",
            bucket: GOVERNED_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{GOVERNED_BUCKET}/*"),
            )),
            label: DENIED_LABEL,
            trailing_separator: false,
            delivered: false,
        },
        Row {
            case: "a destination that policy's deny does not cover",
            bucket: GOVERNED_BUCKET,
            grant: Some(allow(
                "s3:PutObject",
                format!("arn:aws:s3:::{GOVERNED_BUCKET}/*"),
            )),
            label: "governed-elsewhere",
            trailing_separator: false,
            delivered: true,
        },
    ]
}

/// RULE: the capture objects an endpoint delivers under `DestinationS3Uri` are the ones the
/// deployed model's execution role is allowed `s3:PutObject` on, and the inference answers HTTP
/// 200 whether that grant is there or not.
#[tokio::test]
async fn capture_delivery_follows_the_execution_roles_write_grant() {
    let router = emulator();
    given_bucket(&router, ARTIFACT_BUCKET).await;
    given_bucket(&router, CAPTURE_BUCKET).await;
    given_bucket(&router, GOVERNED_BUCKET).await;
    given_denying_policy(&router).await;
    let archive = service_com_amazonaws_sagemaker_sagemaker::state::model_archive(&[(
        "xgboost-model".to_string(),
        booster("reg:squarederror", 1.0, 1.25, 7.5).into_bytes(),
    )])
    .expect("the model archive the artifact is");
    given_bytes(&router, ARTIFACT_BUCKET, ARTIFACT_KEY, &archive).await;

    // What the first row's endpoint answered for each payload, which every other row's has to
    // answer too.
    let mut served: Option<Vec<String>> = None;

    for row in rows() {
        let case = row.case;
        let label = row.label;
        let role = given_role(&router, &format!("capture-role-{label}"), row.grant.clone()).await;
        let separator = if row.trailing_separator { "/" } else { "" };
        let destination = format!(
            "s3://{}/{CAPTURE_PREFIX}/{label}{separator}",
            row.bucket
        );
        let endpoint = format!("capture-endpoint-{label}");
        deploy(
            &router,
            case,
            &format!("capture-model-{label}"),
            &endpoint,
            &role,
            &destination,
        )
        .await;

        // The inference is what the operation answers, and the capture write is not: every
        // invocation is served identically whatever the role may write.
        let mut answered: Vec<String> = Vec::new();
        for payload in PAYLOADS {
            let answer = invoke(
                &router,
                &endpoint,
                &[("content-type", "text/csv")],
                payload.as_bytes(),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: InvokeEndpoint with {payload:?} must be served whatever the \
                 execution role may write, got {} {}",
                answer.status,
                answer.text()
            );
            answered.push(answer.text());
        }
        // Capture is a BY-PRODUCT of serving: the same payload against the same deployed model
        // is answered the same whether the delivery that follows is authorized or not.
        match &served {
            None => served = Some(answered.clone()),
            Some(first) => assert_eq!(
                &answered, first,
                "case {case:?}: the inference must not depend on the capture grant, got \
                 {answered:?} against {first:?}"
            ),
        }

        let prefix = format!("{CAPTURE_PREFIX}/{label}/");
        let keys = keys_under(&router, row.bucket, &prefix).await;
        if !row.delivered {
            assert!(
                keys.is_empty(),
                "case {case:?}: a role the write is withheld from delivers no capture object \
                 under {prefix:?}, got {keys:?}"
            );
            continue;
        }
        assert_eq!(
            keys.len(),
            PAYLOADS.len(),
            "case {case:?}: every served invocation is one capture object under {prefix:?}, got \
             {keys:?}"
        );
        let mut captured_inputs: Vec<String> = Vec::new();
        for key in &keys {
            assert!(
                key.starts_with(&format!("{prefix}{endpoint}/{VARIANT}/")),
                "case {case:?}: a capture object is laid out under the endpoint and the variant, \
                 got {key:?}"
            );
            let text = object_text(&router, row.bucket, key).await;
            let record: Value = text
                .lines()
                .next()
                .and_then(|line| serde_json::from_str(line).ok())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {key:?} holds no capture record, got {text:?}")
                });
            let input = record
                .pointer("/captureData/endpointInput/data")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {key:?} records no request half, got {record}")
                });
            assert!(
                record
                    .pointer("/captureData/endpointOutput/data")
                    .and_then(Value::as_str)
                    .is_some_and(|answered| !answered.is_empty()),
                "case {case:?}: {key:?} must pair the request with the answer, got {record}"
            );
            captured_inputs.push(input.to_string());
        }
        for payload in PAYLOADS {
            assert!(
                captured_inputs.iter().any(|input| input == payload),
                "case {case:?}: the exchange {payload:?} must be one of the records delivered, \
                 got {captured_inputs:?}"
            );
        }
    }
}
