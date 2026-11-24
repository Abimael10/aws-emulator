// SPDX-License-Identifier: CC0-1.0

//! What a deployment reports when the model data its production variants name cannot be
//! DOWNLOADED — driven end to end over the real wire path (SageMaker, Amazon S3 and IAM
//! mounted on ONE shared engine, the way the router mounts every service), so what is
//! asserted is what a client sees.
//!
//! THE RULE UNDER TEST: an endpoint whose deployment could not download a variant's model data
//! settles in the modelled `Failed` status carrying a `FailureReason` that names the BUCKET and
//! the KEY of the object the download was attempted on — the two parts of that model's
//! `ModelDataUrl` — in SageMaker's own fixed wording, and nothing else of the request or the
//! state. A deployment whose model data IS downloadable settles `InService` with no
//! `FailureReason`, so the reason is reported for the failure and only for it.
//!
//! The rows span what the rule generalises over: the shape of the location (a key under a
//! nested prefix, one at the bucket root, one carrying dots and dashes), which bucket holds it,
//! what makes the download fail (the object is gone, or the bucket's own policy withholds the
//! read of exactly that key), and WHEN the condition arises — before the endpoint is created,
//! so the create resolves the outcome, and after it, so the instant the fleet was due to serve
//! resolves it. Every row deploys a SECOND endpoint over a byte-identical copy at a live key as
//! its control.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::model_artifact_admission::{role_arn, str_field, trust_for, Emulator, ACCOUNT, REGION};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The bytes every row's artifact is staged with; the control's copy is identical.
const ARTIFACT: &str = "a model archive";

/// The key the intact copy of each row's artifact lives at.
const INTACT_KEY: &str = "intact-copy/model.tar.gz";

/// The variant each endpoint hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// Every member of `com.amazonaws.sagemaker#EndpointStatus`
/// (`smithy_query(query="shape",shape="EndpointStatus")`): a read never reports a status
/// outside the modelled enum.
const ENDPOINT_STATUS_VALUES: [&str; 9] = [
    "OutOfService",
    "Creating",
    "Updating",
    "SystemUpdating",
    "RollingBack",
    "InService",
    "Deleting",
    "Failed",
    "UpdateRollbackFailed",
];

/// The members of `EndpointStatus` a create has SETTLED in, whatever it settled as: a poll
/// stops at one of these.
const SETTLED_ENDPOINT_STATUSES: [&str; 3] = ["InService", "Failed", "OutOfService"];

/// How long an endpoint is polled for the status its create already owes it, and how often it
/// is looked at.
///
/// GENEROUS on purpose: what is under test is WHICH status the endpoint settles in and what it
/// reports beside it, never how fast it gets there, so each case re-reads the condition in a
/// loop and this is only the deadline that stops a broken emulator from hanging the suite.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// What makes a row's download fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// The object is DELETED, so Amazon S3 answers that the location holds nothing.
    ObjectDeleted,
    /// The bucket's own policy WITHHOLDS the read of that one key from the model's role.
    ReadWithheld,
}

/// When the row's cause arises, relative to the `CreateEndpoint` that asks for the fleet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// Before the create, so the create resolves the outcome it stores.
    BeforeTheCreate,
    /// After the create was accepted, so the instant the fleet was due to serve resolves it.
    AfterTheCreate,
}

/// One deployment the rule is asserted over.
struct Row {
    case: &'static str,
    /// The bucket both copies of the row's artifact live in.
    bucket: &'static str,
    /// The key the BROKEN endpoint's model data is named at.
    key: &'static str,
    cause: Cause,
    when: When,
}

/// The table: one row per shape of the location, cause and moment the rule spans.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key under a nested prefix, gone after the create was accepted",
            bucket: "hosting-artifacts",
            key: "output/training-run/output/model.tar.gz",
            cause: Cause::ObjectDeleted,
            when: When::AfterTheCreate,
        },
        Row {
            case: "a key at the bucket root, gone before the create",
            bucket: "root-artifacts",
            key: "model.tar.gz",
            cause: Cause::ObjectDeleted,
            when: When::BeforeTheCreate,
        },
        Row {
            case: "a key carrying dots and dashes, gone after the create was accepted",
            bucket: "dotted-artifacts",
            key: "runs/run-2026.01/model-v2.tar.gz",
            cause: Cause::ObjectDeleted,
            when: When::AfterTheCreate,
        },
        Row {
            case: "a key whose read the bucket's policy withholds, withheld before the create",
            bucket: "withholding-artifacts",
            key: "fenced/model.tar.gz",
            cause: Cause::ReadWithheld,
            when: When::BeforeTheCreate,
        },
        Row {
            case: "a key whose read the bucket's policy withholds, withheld after the create",
            bucket: "late-withholding-artifacts",
            key: "fenced/nested/model.tar.gz",
            cause: Cause::ReadWithheld,
            when: When::AfterTheCreate,
        },
    ]
}

/// SageMaker's own `FailureReason` for model data a deployment could not download: a fixed
/// sentence whose only variable parts are the bucket and the key of the object the download was
/// attempted on.
fn expected_reason(bucket: &str, key: &str) -> String {
    format!(
        "Failed to download model data(bucket: {bucket}, key: {key}). Please ensure that there \
         is an object located at the URL and that the role passed to CreateModel has permissions \
         to download the model."
    )
}

/// A `PrimaryContainer` naming the inference image and the location its model data lives at.
fn primary_container(model_data: &str) -> Value {
    json!({
        "Image": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/inference:1"),
        "ModelDataUrl": model_data,
    })
}

/// A bucket policy that ALLOWS the role every object of the bucket and DENIES it exactly
/// `withheld_key`, so one of the two copies is readable and the other is not.
fn policy_withholding(bucket: &str, withheld_key: &str, role: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "AWS": role },
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
            },
            {
                "Effect": "Deny",
                "Principal": { "AWS": role },
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{bucket}/{withheld_key}"),
            },
        ],
    })
}

/// Delete the object at `key` and assert Amazon S3 then answers that it holds nothing there,
/// which is the premise the row's download fails on.
async fn given_object_deleted(emulator: &Emulator, case: &str, bucket: &str, key: &str) {
    let (status, body) = emulator
        .s3("DELETE", &format!("/{bucket}/{key}"), &[])
        .await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: the object {key} must be deleted, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let (head, _) = emulator.s3("HEAD", &format!("/{bucket}/{key}"), &[]).await;
    assert_eq!(
        head, 404,
        "case {case:?}: Amazon S3 must answer 404 for the deleted object {key}, got {head}"
    );
}

/// Bring the row's cause about.
async fn given_the_cause(emulator: &Emulator, row: &Row, role: &str) {
    match row.cause {
        Cause::ObjectDeleted => {
            given_object_deleted(emulator, row.case, row.bucket, row.key).await;
        }
        Cause::ReadWithheld => {
            emulator
                .given_bucket_policy(row.bucket, &policy_withholding(row.bucket, row.key, role))
                .await;
        }
    }
}

/// Create the model and the configuration hosting it, both under `name`, and answer the
/// configuration's name.
///
/// Separate from the `CreateEndpoint` below because the hosting creates resolve the artifact
/// SYNCHRONOUSLY — `CreateModel` refuses a location Amazon S3 answers no object for — so every
/// row registers its model while both copies are readable, and what the deployment then reports
/// is about the DOWNLOAD rather than about the create.
async fn given_model_and_config(
    emulator: &Emulator,
    case: &str,
    name: &str,
    role: &str,
    artifact: &str,
) -> String {
    emulator
        .sagemaker_ok(
            case,
            "CreateModel",
            &json!({
                "ModelName": name,
                "PrimaryContainer": primary_container(artifact),
                "ExecutionRoleArn": role,
            }),
        )
        .await;
    let config = format!("{name}-config");
    emulator
        .sagemaker_ok(
            case,
            "CreateEndpointConfig",
            &json!({
                "EndpointConfigName": config,
                "ProductionVariants": [{
                    "VariantName": VARIANT,
                    "ModelName": name,
                    "InstanceType": "ml.m5.large",
                    "InitialInstanceCount": 1,
                }],
            }),
        )
        .await;
    config
}

/// Ask for the fleet serving `config` under the endpoint name `name`.
async fn given_endpoint(emulator: &Emulator, case: &str, name: &str, config: &str) {
    emulator
        .sagemaker_ok(
            case,
            "CreateEndpoint",
            &json!({ "EndpointName": name, "EndpointConfigName": config }),
        )
        .await;
}

/// Poll `DescribeEndpoint` until the create has SETTLED, answering the read that saw it —
/// checking on the way that every status it is observed in is a modelled one.
async fn settled_endpoint(emulator: &Emulator, case: &str, endpoint: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "EndpointName": endpoint });
    loop {
        let read = emulator.sagemaker_ok(case, "DescribeEndpoint", &body).await;
        let status = str_field(&read, "EndpointStatus");
        assert!(
            ENDPOINT_STATUS_VALUES.contains(&status.as_str()),
            "case {case:?}: EndpointStatus must be a modelled value, got {status:?}"
        );
        if SETTLED_ENDPOINT_STATUSES.contains(&status.as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint never settled; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// THE RULE: a deployment that could not download its model data fails with the bucket and the
/// key of that object named in SageMaker's own wording, while a deployment over a live copy of
/// the same bytes serves.
#[tokio::test]
async fn a_failed_download_names_the_bucket_and_key_it_could_not_download() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(row.bucket).await;
        // Two byte-identical copies: the one the row breaks, and the control's.
        emulator.given_object(row.bucket, row.key, ARTIFACT).await;
        emulator
            .given_object(row.bucket, INTACT_KEY, ARTIFACT)
            .await;

        let role_name = "sagemaker-hosting-role";
        let role = emulator
            .given_role(role_name, &trust_for(SERVICE_PRINCIPAL))
            .await;
        emulator.given_s3_read_grant(role_name).await;
        assert_eq!(
            role,
            role_arn(role_name),
            "case {case:?}: the role of this account carries its own ARN"
        );

        let broken = "broken-endpoint";
        let intact = "intact-endpoint";
        let broken_uri = format!("s3://{}/{}", row.bucket, row.key);
        let intact_uri = format!("s3://{}/{INTACT_KEY}", row.bucket);

        // Both models are registered while both objects are readable, so what each deployment
        // then reports is about the DOWNLOAD and not about the create.
        let intact_config =
            given_model_and_config(&emulator, case, intact, &role, &intact_uri).await;
        let broken_config =
            given_model_and_config(&emulator, case, broken, &role, &broken_uri).await;
        if row.when == When::BeforeTheCreate {
            given_the_cause(&emulator, &row, &role).await;
        }
        given_endpoint(&emulator, case, intact, &intact_config).await;
        given_endpoint(&emulator, case, broken, &broken_config).await;
        if row.when == When::AfterTheCreate {
            given_the_cause(&emulator, &row, &role).await;
        }

        // The broken endpoint settles Failed, naming the bucket and the key it could not
        // download — and nothing of the request or the state beside them.
        let failed = settled_endpoint(&emulator, case, broken).await;
        assert_eq!(
            str_field(&failed, "EndpointStatus"),
            "Failed",
            "case {case:?}: a deployment whose model data cannot be downloaded fails, got \
             {failed}"
        );
        let reason = str_field(&failed, "FailureReason");
        assert_eq!(
            reason,
            expected_reason(row.bucket, row.key),
            "case {case:?}: the FailureReason must attribute the failure to the download of this \
             bucket and key, got {reason:?}"
        );

        // The control is untouched: the same bytes at a live key serve, with no reason at all.
        let serving = settled_endpoint(&emulator, case, intact).await;
        assert_eq!(
            str_field(&serving, "EndpointStatus"),
            "InService",
            "case {case:?}: the deployment over the live copy must serve, got {serving}"
        );
        assert_eq!(
            serving.get("FailureReason"),
            None,
            "case {case:?}: a deployment that failed at nothing carries no FailureReason, got \
             {serving}"
        );

        // The control plane never resolved the location it was given: the model keeps echoing
        // the URI the create stored, broken or not.
        let model = emulator
            .sagemaker_ok(case, "DescribeModel", &json!({ "ModelName": broken }))
            .await;
        assert_eq!(
            model
                .pointer("/PrimaryContainer/ModelDataUrl")
                .and_then(Value::as_str),
            Some(broken_uri.as_str()),
            "case {case:?}: DescribeModel keeps reporting the location the create stored, got \
             {model}"
        );
    }
}

/// RULE, at the sentence this service stores: whatever `s3://` location a model names, the
/// reason reports it SPLIT into the bucket and the key, with the fixed text unchanged and the
/// URI itself nowhere in it.
#[test]
fn the_reason_carries_the_location_split_into_a_bucket_and_a_key() {
    use service_com_amazonaws_sagemaker_sagemaker::state::{
        failed_to_download_model_data, S3Location,
    };

    // Each row is the `ModelDataUrl` a model carries and the two parts it names — the shapes
    // the modelled `S3Uri` pattern (`^(https|s3)://([^/]+)/?(.*)$`,
    // `smithy_query(query="shape",shape="S3Uri")`) admits for an `s3://` location.
    let rows = [
        ("s3://bucket/model.tar.gz", "bucket", "model.tar.gz"),
        (
            "s3://artifacts.example-bucket/deep/nested/prefix/model.tar.gz",
            "artifacts.example-bucket",
            "deep/nested/prefix/model.tar.gz",
        ),
        (
            "s3://artifacts/run-2026.01/model+v2.tar.gz",
            "artifacts",
            "run-2026.01/model+v2.tar.gz",
        ),
        ("s3://artifacts/", "artifacts", ""),
        ("s3://artifacts", "artifacts", ""),
    ];
    for (uri, bucket, key) in rows {
        let location = S3Location::parse(uri)
            .unwrap_or_else(|| panic!("case {uri:?}: an s3:// location names a bucket"));
        let reason = failed_to_download_model_data(&location);
        assert_eq!(
            reason,
            expected_reason(bucket, key),
            "case {uri:?}: the reason names the bucket and the key of the location"
        );
        assert!(
            !reason.contains("s3://"),
            "case {uri:?}: the reason names the parts, not the URI, got {reason:?}"
        );
    }
}
