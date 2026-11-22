// SPDX-License-Identifier: CC0-1.0

//! An endpoint answers in the space the model's own LEARNING TASK is defined on.
//!
//! RULE: a model is trained for the learning task its `HyperParameters` declare — XGBoost's
//! `objective`, whose own documentation gives each family's output: the logistic families
//! (`binary:logistic`, `reg:logistic`) answer a PROBABILITY, the count, gamma, tweedie and
//! survival families answer a positive quantity, and the squared-error, absolute-error,
//! raw-logit, hinge and ranking families answer the score itself. So `InvokeEndpoint` on an
//! `InService` endpoint answers, per record, a value inside the range that task's output is
//! defined on — never a value the task could not produce, such as a probability above one —
//! and the value stays monotone in the record's features, because the task's output function
//! is applied to the model's own score.
//!
//! Only a cross-service suite can ask this: the flow spans four services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — IAM for the role the training run reads
//! and writes under, Amazon S3 for the training data and the model artifact, the SageMaker
//! control plane to train and deploy, and the SageMaker runtime to invoke. The harness (the
//! emulator, the request builders, the role, the poll-until-settled reading and the reading of
//! an answer's predictions) is the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]) rather than a second copy of it.
//!
//! The rows differ along what the rule generalises over: the learning task the request
//! declares (each output family, a member of a family whose own output is the raw score, and no
//! task at all), the shape of the target column the task is trained on (class labels, a
//! continuous measurement, counts) and the range the answer has to lie in. Every row is probed
//! with a record BEYOND the data it was fitted on, which is where a model answering its score
//! instead of its task's output leaves that range.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_serves_a_trained_model::{
    control_ok, emulator, given_bucket, given_object, given_role, image, invoke, predictions,
    settled, BUCKET,
};

/// The variant every endpoint below hosts its model on.
const VARIANT: &str = "AllTraffic";

/// How many feature columns every model below is fitted on.
const FEATURES: usize = 2;

/// The shape of the target column a task is trained on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Targets {
    /// Two class labels, which is what a probability task is trained on.
    Labels,
    /// A continuous measurement, which is what a squared-error regression is trained on.
    Measurements,
    /// Small whole counts, which is what a count task is trained on.
    Counts,
}

/// `rows` comma-separated records of [`FEATURES`] feature columns, the first column the target
/// in the shape `targets` asks for and the features ascending, so the target is a function of
/// them and a fit has something to learn.
fn training_data(targets: Targets, rows: usize) -> String {
    (0..rows)
        .map(|row| {
            let values: Vec<f64> = (0..FEATURES)
                .map(|column| (row + column + 1) as f64)
                .collect();
            let total: f64 = values.iter().sum();
            let target = match targets {
                Targets::Labels => f64::from(total > rows as f64),
                Targets::Measurements => 3.0 + total,
                Targets::Counts => (total / 10.0).round(),
            };
            let mut record = vec![format!("{target}")];
            record.extend(values.iter().map(|value| format!("{value}")));
            format!("{}\n", record.join(","))
        })
        .collect()
}

/// One record of [`FEATURES`] values, every value `value`.
fn probe(value: f64) -> String {
    (0..FEATURES)
        .map(|_| format!("{value}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// One row of the table: the learning task, the data it is trained on, and the range its
/// answer has to lie in.
struct Row {
    case: &'static str,
    /// The `objective` hyperparameter the request declares, or `None` for a request that
    /// declares no task.
    objective: Option<&'static str>,
    targets: Targets,
    /// The closed range the task's output is defined on.
    range: (f64, f64),
    /// The feature value the HIGH record carries, past the data the model was fitted on
    /// (which runs to `40 + FEATURES`): far past it where the task's output is BOUNDED, and
    /// just past it where the output grows with the score, which is where an extrapolation
    /// of any model stops being a number.
    high: f64,
}

/// The table. `reg:logistic` and `count:poisson` are families no report cited.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a binary probability task",
            objective: Some("binary:logistic"),
            targets: Targets::Labels,
            range: (0.0, 1.0),
            high: 1_000.0,
        },
        Row {
            case: "a regression on a probability target",
            objective: Some("reg:logistic"),
            targets: Targets::Labels,
            range: (0.0, 1.0),
            high: 1_000.0,
        },
        Row {
            case: "a count task, whose output is a positive quantity",
            objective: Some("count:poisson"),
            targets: Targets::Counts,
            range: (0.0, f64::MAX),
            high: 50.0,
        },
        Row {
            case: "a binary task answering the raw score",
            objective: Some("binary:logitraw"),
            targets: Targets::Labels,
            range: (f64::MIN, f64::MAX),
            high: 1_000.0,
        },
        Row {
            case: "a squared-error regression",
            objective: Some("reg:squarederror"),
            targets: Targets::Measurements,
            range: (f64::MIN, f64::MAX),
            high: 1_000.0,
        },
        Row {
            case: "no learning task declared at all",
            objective: None,
            targets: Targets::Measurements,
            range: (f64::MIN, f64::MAX),
            high: 1_000.0,
        },
    ]
}

/// Train a model on `row`'s data under `row`'s learning task and deploy it behind `endpoint`,
/// answering once the endpoint is `InService`.
async fn trained_and_deployed(router: &MultiServiceRouter, row: &Row, name: &str, endpoint: &str) {
    let case = row.case;
    let role_arn = given_role(router).await;
    let key = format!("{name}/train.csv");
    given_object(router, &key, &training_data(row.targets, 40)).await;
    let mut body = json!({
        "TrainingJobName": name,
        "AlgorithmSpecification": {
            // The repository SageMaker publishes its gradient-boosting algorithm under, so
            // the job names that algorithm and its `objective` hyperparameter is the one the
            // algorithm documents.
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
    });
    if let Some(objective) = row.objective {
        body["HyperParameters"] = json!({ "objective": objective, "num_round": "10" });
    }
    control_ok(router, "CreateTrainingJob", &body).await;
    let trained = settled(
        router,
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
        .unwrap_or_else(|| panic!("case {case:?}: the completed job names no artifact: {trained}"))
        .to_string();

    control_ok(
        router,
        "CreateModel",
        &json!({
            "ModelName": name,
            "PrimaryContainer": { "Image": image(), "ModelDataUrl": artifact },
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

/// RULE: every value an endpoint serves lies inside the range its model's learning task is
/// defined on, and rises with the record's features.
#[tokio::test]
async fn an_endpoint_answers_inside_its_learning_tasks_own_range() {
    for (index, row) in rows().iter().enumerate() {
        let router = emulator();
        given_bucket(&router).await;
        let case = row.case;
        let name = format!("task-space-{index}");
        let endpoint = format!("{name}-endpoint");
        trained_and_deployed(&router, row, &name, &endpoint).await;

        // Two records: one at the low end of the data the model was fitted on, and one past
        // its high end — where a model answering its score rather than its task's output
        // leaves the task's range.
        let low = probe(1.0);
        let high = probe(row.high);
        let (lo, hi) = row.range;
        let mut served: Vec<f64> = Vec::new();
        for (which, payload) in [("low", &low), ("high", &high)] {
            let answer = invoke(
                &router,
                &endpoint,
                &[("content-type", "text/csv"), ("accept", "text/csv")],
                payload.as_bytes(),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: the {which} record must be served, got {} with {}",
                answer.status,
                answer.text()
            );
            let scores = predictions(&answer, case);
            assert_eq!(
                scores.len(),
                1,
                "case {case:?}: one prediction for the {which} record, got {scores:?}"
            );
            let score = scores[0];
            assert!(
                score.is_finite() && (lo..=hi).contains(&score),
                "case {case:?}: the {which} record's score must lie in [{lo}, {hi}], got \
                 {score}"
            );
            served.push(score);
        }
        assert!(
            served[1] > served[0],
            "case {case:?}: the score must rise with the record's features, got {served:?}"
        );

        // The answer is the model's own computation, so the same record is answered the same
        // way twice: nothing about the first invocation changed what the second is served.
        let again = invoke(
            &router,
            &endpoint,
            &[("content-type", "text/csv"), ("accept", "text/csv")],
            high.as_bytes(),
        )
        .await;
        assert_eq!(
            predictions(&again, case),
            vec![served[1]],
            "case {case:?}: the same record must be answered the same way"
        );
    }
}
