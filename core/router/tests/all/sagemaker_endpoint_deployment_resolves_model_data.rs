// SPDX-License-Identifier: CC0-1.0

//! Bringing an endpoint up DOWNLOADS the model data its variants' models name, so a
//! deployment whose model data is not there never reports itself serving.
//!
//! The rule under test: "If any of the models hosted at this endpoint get model data from an
//! Amazon S3 location, SageMaker uses Amazon Web Services Security Token Service to download
//! model artifacts from the S3 path you provided" (`CreateEndpoint`). `CreateEndpoint` is
//! ACCEPTED either way and the endpoint reports `Creating`; the outcome the clock then owes it
//! is `EndpointStatus` `InService` when every model's model data can be read as that model's
//! execution role, and `Failed` with a `FailureReason` naming the location that could not be
//! read otherwise — never `InService` for a fleet with no model behind it. An endpoint in that
//! state serves no traffic, so `InvokeEndpoint` against it is refused rather than answered.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — IAM for the role the deployment reads
//! under, Amazon S3 for the model artifact, the SageMaker control plane to register and deploy
//! the model, and the SageMaker runtime to invoke. The harness (the emulator, the request
//! builders, the role and the poll-until-settled reading) is the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]) rather than a second copy of it.
//!
//! The rows differ along what the rule generalises over: whether the model data is there at
//! all, which of a two-container inference pipeline's artifacts is missing, the form of the
//! key the artifact lives under (a nested prefix, the bucket root), and whether the model
//! names its model data as a `ModelDataUrl` or as a `ModelDataSource`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_object, given_role, image, invoke, send, settled,
    BUCKET,
};

/// The variant every endpoint below hosts its models on.
const VARIANT: &str = "AllTraffic";

/// An artifact's bytes. Nothing reads them in these cases: what is under test is whether the
/// deployment resolves the LOCATION, so the body only has to be an object Amazon S3 holds.
const ARTIFACT_BODY: &str = "{\"model\":{\"intercept\":1.0,\"weights\":[1.0],\"epochs\":1}}";

/// `DELETE /<bucket>/<key>` — `DeleteObject`, so a case can take an artifact away after the
/// model that names it was registered.
async fn delete_object(router: &MultiServiceRouter, key: &str) {
    let answer = send(
        router,
        "s3",
        "DELETE",
        &format!("/{BUCKET}/{key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert!(
        (200..300).contains(&answer.status),
        "setup: the object {key} must be deleted, got {} {}",
        answer.status,
        answer.text()
    );
}

/// How a model names one of its containers' model data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// `ContainerDefinition$ModelDataUrl`.
    Url,
    /// `ContainerDefinition$ModelDataSource.S3DataSource.S3Uri`.
    Source,
}

/// One container of the model a row deploys: where its model data lives, how the container
/// names it, and whether the case takes that object away before the endpoint is created.
struct Container {
    key: &'static str,
    names: Names,
    deleted: bool,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The containers the model carries: the first is the primary container.
    containers: Vec<Container>,
    /// The key whose absence must be named by the `FailureReason`, or `None` for a row whose
    /// deployment comes up.
    missing: Option<&'static str>,
}

/// The table. Every row stages each of its containers' objects first, so a row can only fail
/// for the object it deliberately deletes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every artifact still there",
            containers: vec![Container {
                key: "deploy/present/model.tar.gz",
                names: Names::Url,
                deleted: false,
            }],
            missing: None,
        },
        Row {
            case: "the only artifact deleted before the deployment",
            containers: vec![Container {
                key: "deploy/gone/model.tar.gz",
                names: Names::Url,
                deleted: true,
            }],
            missing: Some("deploy/gone/model.tar.gz"),
        },
        Row {
            case: "an artifact at the bucket root deleted",
            containers: vec![Container {
                key: "root-model.tar.gz",
                names: Names::Url,
                deleted: true,
            }],
            missing: Some("root-model.tar.gz"),
        },
        Row {
            case: "a model naming its data as a ModelDataSource, deleted",
            containers: vec![Container {
                key: "deploy/source/model.tar.gz",
                names: Names::Source,
                deleted: true,
            }],
            missing: Some("deploy/source/model.tar.gz"),
        },
        Row {
            case: "the SECOND container of a pipeline deleted",
            containers: vec![
                Container {
                    key: "deploy/pipeline/first.tar.gz",
                    names: Names::Url,
                    deleted: false,
                },
                Container {
                    key: "deploy/pipeline/second.tar.gz",
                    names: Names::Url,
                    deleted: true,
                },
            ],
            missing: Some("deploy/pipeline/second.tar.gz"),
        },
        Row {
            case: "a pipeline whose artifacts are all there",
            containers: vec![
                Container {
                    key: "deploy/whole/first.tar.gz",
                    names: Names::Url,
                    deleted: false,
                },
                Container {
                    key: "deploy/whole/second.tar.gz",
                    names: Names::Url,
                    deleted: false,
                },
            ],
            missing: None,
        },
    ]
}

/// The container definition for `container`, naming its model data the way the row asks for.
fn container_body(container: &Container) -> Value {
    let uri = format!("s3://{BUCKET}/{}", container.key);
    let mut body = json!({ "Image": image() });
    match container.names {
        Names::Url => body["ModelDataUrl"] = json!(uri),
        Names::Source => {
            body["ModelDataSource"] = json!({
                "S3DataSource": {
                    "S3Uri": uri,
                    "S3DataType": "S3Object",
                    "CompressionType": "Gzip",
                },
            });
        }
    }
    body
}

/// Every `EndpointStatus` member nothing leaves on its own once a create has settled
/// (`smithy_query(query="shape",shape="EndpointStatus")` lists `OutOfService`, `Creating`,
/// `Updating`, `SystemUpdating`, `RollingBack`, `InService`, `Deleting`, `Failed` and
/// `UpdateRollbackFailed`): a create ends in one of these two.
const CREATE_OUTCOMES: [&str; 2] = ["InService", "Failed"];

/// Poll `DescribeEndpoint` until the create's outcome is settled, answering that read. Waits
/// for the CONDITION, never for a fixed span.
async fn settled_endpoint(router: &MultiServiceRouter, case: &str, endpoint: &str) -> Value {
    let deadline = std::time::Instant::now()
        + crate::sagemaker_endpoint_serves_a_trained_model::SETTLE_BUDGET;
    loop {
        let read = control_ok(
            router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint }),
        )
        .await;
        let status = read
            .get("EndpointStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if CREATE_OUTCOMES.contains(&status) {
            return read;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the endpoint never settled its create; last read {read}"
        );
        tokio::time::sleep(crate::sagemaker_endpoint_serves_a_trained_model::SETTLE_POLL).await;
    }
}

/// RULE: an endpoint's create settles `InService` only when every model it hosts holds the
/// model data its containers name; one whose model data is gone settles `Failed` with the
/// `FailureReason` naming that location, and serves nothing.
#[tokio::test]
async fn a_deployment_settles_on_whether_its_model_data_can_be_downloaded() {
    for row in rows() {
        let router = emulator();
        given_bucket(&router).await;
        let role_arn = given_role(&router).await;
        for container in &row.containers {
            given_object(&router, container.key, ARTIFACT_BODY).await;
        }
        let case = row.case;
        let name = "deployment-model-data";
        let endpoint = format!("{name}-endpoint");

        let mut body = json!({
            "ModelName": name,
            "ExecutionRoleArn": role_arn,
        });
        let containers: Vec<Value> = row.containers.iter().map(container_body).collect();
        if containers.len() == 1 {
            body["PrimaryContainer"] = containers
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: one container"))
                .clone();
        } else {
            // An inference pipeline is registered as `Containers`, the list of the containers
            // the request is routed through.
            body["Containers"] = json!(containers);
        }
        control_ok(&router, "CreateModel", &body).await;

        // The artifacts a row takes away go AFTER the model is registered: the create resolves
        // them synchronously, so this is the state the deployment itself has to resolve.
        for container in row.containers.iter().filter(|container| container.deleted) {
            delete_object(&router, container.key).await;
        }

        let config = format!("{name}-config");
        control_ok(
            &router,
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
        // The create itself is served: the download is part of bringing the fleet up, not of
        // accepting the call.
        control_ok(
            &router,
            "CreateEndpoint",
            &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
        )
        .await;

        let settled = settled_endpoint(&router, case, &endpoint).await;
        let status = settled
            .get("EndpointStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let reason = settled
            .get("FailureReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match row.missing {
            None => {
                assert_eq!(
                    status, "InService",
                    "case {case:?}: a deployment whose model data is all there must serve, \
                     got {settled}"
                );
                assert!(
                    reason.is_empty(),
                    "case {case:?}: a deployment that came up reports no FailureReason, got \
                     {reason:?}"
                );
            }
            Some(key) => {
                assert_eq!(
                    status, "Failed",
                    "case {case:?}: a deployment whose model data is gone must not serve, \
                     got {settled}"
                );
                let named = format!("s3://{BUCKET}/{key}");
                assert!(
                    reason.contains(&named),
                    "case {case:?}: the FailureReason must name the model data location \
                     {named:?}, got {reason:?}"
                );
            }
        }

        // The status is the endpoint's own, so reading it again reports the same thing: the
        // clock settled the create, and a read settles nothing of its own.
        let again = control_ok(
            &router,
            "DescribeEndpoint",
            &json!({ "EndpointName": endpoint }),
        )
        .await;
        assert_eq!(
            again, settled,
            "case {case:?}: a settled endpoint must read back identically"
        );

        // A fleet that never came up serves no traffic: the invocation is refused rather than
        // answered with something no model computed.
        if row.missing.is_some() {
            let answered = invoke(
                &router,
                &endpoint,
                &[("content-type", "text/csv")],
                b"1.0,2.0,3.0",
            )
            .await;
            assert_ne!(
                answered.status, 200,
                "case {case:?}: an endpoint that failed to deploy must serve nothing, got \
                 {} {}",
                answered.status,
                answered.text()
            );
            assert!(
                answered.message().contains(&status),
                "case {case:?}: the refusal must name the endpoint's status {status:?}, got \
                 {:?}",
                answered.message()
            );
        }
    }
}

/// RULE (the same rule over a TRAINED model's own artifact, and over the other order the
/// deletion can fall in): the artifact a deployment downloads is the object the training run
/// wrote, so an endpoint created over a model whose trained artifact is gone — whether it went
/// before the create or while the fleet was coming up — settles `Failed` naming it, and one
/// over a model whose artifact is still there serves.
#[tokio::test]
async fn a_trained_models_artifact_decides_the_deployment_whenever_it_goes() {
    // (case, whether the artifact is deleted, whether the deletion falls after the create)
    let rows: [(&str, bool, bool); 3] = [
        ("the trained artifact is still there", false, false),
        ("the trained artifact deleted before the create", true, false),
        (
            "the trained artifact deleted while the fleet comes up",
            true,
            true,
        ),
    ];
    for (case, deleted, after_the_create) in rows {
        let router = emulator();
        given_bucket(&router).await;
        let role_arn = given_role(&router).await;
        let name = "trained-model-data";
        let endpoint = format!("{name}-endpoint");
        let key = format!("{name}/train.csv");
        given_object(&router, &key, &training_records(40, 2)).await;
        control_ok(
            &router,
            "CreateTrainingJob",
            &json!({
                "TrainingJobName": name,
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
                "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/{name}/out/") },
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
            &router,
            case,
            "DescribeTrainingJob",
            &json!({ "TrainingJobName": name }),
            "TrainingJobStatus",
            "Completed",
        )
        .await;
        let artifact = trained
            .pointer("/ModelArtifacts/S3ModelArtifacts")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the job names no artifact: {trained}"))
            .to_string();
        let artifact_key = artifact
            .strip_prefix(&format!("s3://{BUCKET}/"))
            .unwrap_or_else(|| panic!("case {case:?}: {artifact} is not an object of {BUCKET}"))
            .to_string();

        control_ok(
            &router,
            "CreateModel",
            &json!({
                "ModelName": name,
                "PrimaryContainer": { "Image": image(), "ModelDataUrl": artifact },
                "ExecutionRoleArn": role_arn,
            }),
        )
        .await;
        if deleted && !after_the_create {
            delete_object(&router, &artifact_key).await;
        }

        let config = format!("{name}-config");
        control_ok(
            &router,
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
        control_ok(
            &router,
            "CreateEndpoint",
            &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
        )
        .await;
        if deleted && after_the_create {
            delete_object(&router, &artifact_key).await;
        }

        let read = settled_endpoint(&router, case, &endpoint).await;
        let status = read
            .get("EndpointStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let reason = read
            .get("FailureReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if deleted {
            assert_eq!(
                status, "Failed",
                "case {case:?}: a deployment with no model artifact to download must not \
                 serve, got {read}"
            );
            assert!(
                reason.contains(&artifact),
                "case {case:?}: the FailureReason must name {artifact:?}, got {reason:?}"
            );
        } else {
            assert_eq!(
                status, "InService",
                "case {case:?}: a deployment whose artifact is there must serve, got {read}"
            );
            assert!(
                reason.is_empty(),
                "case {case:?}: an endpoint that came up reports no FailureReason, got \
                 {reason:?}"
            );
        }
    }
}

/// Comma-separated training records: `rows` records whose first column is the target and whose
/// remaining `features` columns are the features, the target a linear function of them — so
/// the run has records of the tabular form to fit.
fn training_records(rows: usize, features: usize) -> String {
    (0..rows)
        .map(|row| {
            let values: Vec<f64> = (0..features)
                .map(|column| (row + column + 1) as f64)
                .collect();
            let target: f64 = 3.0 + values.iter().sum::<f64>();
            let mut record = vec![format!("{target}")];
            record.extend(values.iter().map(|value| format!("{value}")));
            format!("{}\n", record.join(","))
        })
        .collect()
}
