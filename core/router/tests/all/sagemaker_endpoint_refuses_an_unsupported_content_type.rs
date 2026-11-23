// SPDX-License-Identifier: CC0-1.0

//! What a refused invocation leaves behind: NOTHING.
//!
//! RULE — an invocation the model container refuses (the media type the payload was offered
//! under is one it does not accept, which SageMaker passes through as the modelled `ModelError`)
//! changes nothing about the endpoint or the model behind it: the inference an accepted
//! invocation is answered with is byte-identical before and after the refusal, and refusing
//! consumes nothing that a later invocation needs. `InvokeEndpoint` computes its answer from the
//! deployed model's own parameters, so a refusal is an answer about ONE request and never a
//! state change — the endpoint stays `InService` with no failure reason either way, which
//! [`crate::sagemaker_endpoint_serves_a_trained_model`] asserts beside the refusal's own shape.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine the way
//! `core/router/src/lib.rs` mounts them — IAM for the execution role, Amazon S3 for the training
//! data and the model artifact, the SageMaker control plane to train and deploy, and the
//! SageMaker runtime to invoke — so it stands that flow up through the harness that suite
//! already shares rather than a second copy of it.
//!
//! The rows differ along what the rule generalises over: which media type the refused invocation
//! offers (one no container would take, a generic binary type, and one that only looks like an
//! accepted type) and which accepted media type the invocations around it are made under.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_object, given_role, image, invoke, settled, Answer,
    BUCKET,
};

/// How many feature columns the staged training data carries, so a payload is a record of that
/// width.
const FEATURES: usize = 2;

/// Comma-separated training records: the target in the first column, the features in the rest.
fn training_data(rows: usize) -> String {
    (0..rows)
        .map(|row| {
            let values: Vec<f64> = (0..FEATURES)
                .map(|column| (row + column + 1) as f64)
                .collect();
            let target: f64 = 3.0 + values.iter().sum::<f64>();
            let mut record = vec![format!("{target}")];
            record.extend(values.iter().map(|value| format!("{value}")));
            format!("{}\n", record.join(","))
        })
        .collect()
}

/// Train a model over the staged data and deploy it behind `endpoint`, answering once the
/// endpoint is `InService`.
async fn given_served_endpoint(router: &MultiServiceRouter, endpoint: &str) {
    let role_arn = given_role(router).await;
    let key = format!("{endpoint}/train.csv");
    given_object(router, &key, &training_data(20)).await;
    control_ok(
        router,
        "CreateTrainingJob",
        &json!({
            "TrainingJobName": endpoint,
            "AlgorithmSpecification": {
                "TrainingImage": image(),
                "TrainingInputMode": "File",
            },
            "RoleArn": role_arn,
            "InputDataConfig": [{
                "ChannelName": "train",
                "DataSource": {
                    "S3DataSource": {
                        "S3DataType": "S3Prefix",
                        "S3Uri": format!("s3://{BUCKET}/{key}"),
                    },
                },
                "ContentType": "text/csv",
            }],
            "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/{endpoint}/out/") },
            "ResourceConfig": {
                "InstanceType": "ml.m5.large",
                "InstanceCount": 1,
                "VolumeSizeInGB": 10,
            },
            "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
        }),
    )
    .await;
    let trained = settled(
        router,
        "setup",
        "DescribeTrainingJob",
        &json!({ "TrainingJobName": endpoint }),
        "TrainingJobStatus",
        "Completed",
    )
    .await;
    let artifact = trained
        .pointer("/ModelArtifacts/S3ModelArtifacts")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the completed job names no artifact: {trained}"))
        .to_string();
    control_ok(
        router,
        "CreateModel",
        &json!({
            "ModelName": endpoint,
            "PrimaryContainer": { "Image": image(), "ModelDataUrl": artifact },
            "ExecutionRoleArn": role_arn,
        }),
    )
    .await;
    let config = format!("{endpoint}-config");
    control_ok(
        router,
        "CreateEndpointConfig",
        &json!({
            "EndpointConfigName": config,
            "ProductionVariants": [{
                "VariantName": "AllTraffic",
                "ModelName": endpoint,
                "InstanceType": "ml.m5.large",
                "InitialInstanceCount": 1,
            }],
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
        "setup",
        "DescribeEndpoint",
        &json!({ "EndpointName": endpoint }),
        "EndpointStatus",
        "InService",
    )
    .await;
}

/// One invocation of `endpoint` offering the media type `offered`.
async fn offered(
    router: &MultiServiceRouter,
    endpoint: &str,
    media_type: &str,
    payload: &str,
) -> Answer {
    invoke(
        router,
        endpoint,
        &[("content-type", media_type)],
        payload.as_bytes(),
    )
    .await
}

/// THE RULE: a refused invocation leaves the served model answering exactly as it did before.
#[tokio::test]
async fn a_refused_invocation_changes_nothing_the_next_one_reads() {
    let router = emulator();
    given_bucket(&router).await;
    let endpoint = "refusal-leaves-nothing";
    given_served_endpoint(&router, endpoint).await;

    // The one record every invocation below carries, which the model was fitted on.
    let payload = "4.0,5.0";
    // (case, the media type the refused invocation offers, the accepted media type the
    // invocations around it are made under)
    let rows: [(&str, &str, &str); 3] = [
        (
            "a media type no container would take",
            "application/x-unsupported-by-xgboost",
            "text/csv",
        ),
        (
            "a generic binary media type",
            "application/octet-stream",
            "application/json",
        ),
        (
            "a media type that only looks like an accepted one",
            "text/csv-ish",
            "text/csv",
        ),
    ];

    for (case, refused, accepted) in rows {
        let before = offered(&router, endpoint, accepted, payload).await;
        assert_eq!(
            before.status,
            200,
            "case {case:?}: the accepted media type is served, got {} with {}",
            before.status,
            before.text()
        );

        let refusal = offered(&router, endpoint, refused, payload).await;
        assert_eq!(
            (refusal.status, refusal.error_code()),
            (424, "ModelError".to_string()),
            "case {case:?}: the container's refusal is passed through as the modelled \
             ModelError, got {} with {}",
            refusal.status,
            refusal.text()
        );

        // Nothing the refusal did reaches the next invocation: the same request is answered
        // with the same bytes, and the endpoint still reports itself serving with no failure.
        let after = offered(&router, endpoint, accepted, payload).await;
        assert_eq!(
            (after.status, after.text()),
            (before.status, before.text()),
            "case {case:?}: the identical accepted invocation is answered identically after a \
             refusal, got {} with {}",
            after.status,
            after.text()
        );
        let described = control_ok(
            &router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint }),
        )
        .await;
        assert_eq!(
            described.get("EndpointStatus").and_then(Value::as_str),
            Some("InService"),
            "case {case:?}: a refused invocation leaves the endpoint serving, got {described}"
        );
        assert!(
            described.get("FailureReason").is_none(),
            "case {case:?}: a refused invocation is no failure of the endpoint's, got {described}"
        );
    }
}
