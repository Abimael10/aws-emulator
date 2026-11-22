// SPDX-License-Identifier: CC0-1.0

//! What a COMPLETED AutoML job V2 reports its best candidate as DEPLOYABLE as: the
//! `InferenceContainers` definition a client passes straight to `CreateModel`.
//!
//! RULE: `AutoMLCandidate$InferenceContainers` is "information about the recommended inference
//! container definitions", and an `AutoMLContainerDefinition` carries the required `Image` (the
//! Amazon ECR path of the container) and `ModelDataUrl` (the location of the model artifacts).
//! So a candidate a completed job picked carries at least one definition whose `ModelDataUrl`
//! names the model artifact the run really wrote under the job's
//! `OutputDataConfig.S3OutputPath` — an object Amazon S3 holds — and whose `Image` is a
//! container reference, so the documented Autopilot deploy path (describe the job, read the
//! best candidate's container, register it with `CreateModel`) needs nothing re-derived.
//!
//! Driven end to end with the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE
//! shared engine — the harness [`crate::training_job_execution`] defines for this target — so
//! every object is read and written in the service that OWNS objects.
//!
//! The rows differ along what the rule generalises over: the problem type the candidates solve
//! (a regression, a binary classification, a multiclass one the run infers from the target
//! column), how many records and feature columns the dataset carries, and the form the output
//! location takes (a prefix ending in a separator, one without, a nested prefix).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{csv_records, read_and_write, str_field, Emulator, BUCKET};

/// How long a job is polled for its terminal status before the case fails, and how often.
/// What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a case
/// the clock would have served.
const TERMINAL_BUDGET: Duration = Duration::from_secs(90);
const TERMINAL_POLL: Duration = Duration::from_millis(50);

/// `AutoMLJobStatus`'s members nothing leaves on its own
/// (`smithy_query(query="shape",shape="AutoMLJobStatus")`).
const TERMINAL_STATUSES: &[&str] = &["Completed", "Failed", "Stopped"];

/// One row of the table.
struct Row {
    case: &'static str,
    /// The dataset the training channel is staged from.
    data: String,
    /// The members the request's `TabularJobConfig` carries beyond `TargetAttributeName`.
    tabular: Value,
    /// The request's `AutoMLJobObjective`, which is given together with a `ProblemType` or
    /// not at all.
    objective: Option<Value>,
    /// The output location the job writes under.
    output: String,
}

/// Comma-separated records whose first column is a CLASS LABEL over `classes` labels and whose
/// remaining columns are features the label is a function of.
fn csv_classes(rows: usize, classes: i64) -> String {
    (0..rows)
        .map(|row| {
            let label = (row as i64) % classes;
            let first = label as f64 * 10.0 + (row % 3) as f64;
            let second = label as f64 * 10.0 - (row % 2) as f64;
            format!("{label},{first},{second}\n")
        })
        .collect()
}

/// The table. Every row's dataset holds records of the tabular form, so no row can pass by
/// reading nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a regression over a prefix ending in a separator",
            data: csv_records(40, 3),
            tabular: json!({ "ProblemType": "Regression" }),
            objective: Some(json!({ "MetricName": "MSE" })),
            output: format!("s3://{BUCKET}/candidates/regression/"),
        },
        Row {
            case: "a binary classification under a nested prefix with no separator",
            data: csv_classes(40, 2),
            tabular: json!({
                "ProblemType": "BinaryClassification",
                "CompletionCriteria": { "MaxCandidates": 3 },
            }),
            objective: Some(json!({ "MetricName": "F1" })),
            output: format!("s3://{BUCKET}/candidates/nested/binary"),
        },
        Row {
            case: "a problem type the run infers from three class labels",
            data: csv_classes(45, 3),
            tabular: json!({}),
            objective: None,
            output: format!("s3://{BUCKET}/candidates/inferred/"),
        },
        Row {
            case: "a two-column dataset over the bucket root",
            data: csv_records(24, 1),
            tabular: json!({ "ProblemType": "Regression" }),
            objective: Some(json!({ "MetricName": "RMSE" })),
            output: format!("s3://{BUCKET}/"),
        },
    ]
}

/// A `CreateAutoMLJobV2` body for `row`.
fn create_body(name: &str, role_arn: &str, key: &str, row: &Row) -> Value {
    let mut tabular = json!({ "TargetAttributeName": "y" });
    let object = tabular.as_object_mut().expect("a JSON object");
    for (member, value) in row.tabular.as_object().expect("a JSON object") {
        object.insert(member.clone(), value.clone());
    }
    let mut body = json!({
        "AutoMLJobName": name,
        "AutoMLJobInputDataConfig": [{
            "ChannelType": "training",
            "ContentType": "text/csv;header=present",
            "DataSource": {
                "S3DataSource": {
                    "S3DataType": "S3Prefix",
                    "S3Uri": format!("s3://{BUCKET}/{key}"),
                },
            },
        }],
        "OutputDataConfig": { "S3OutputPath": row.output.clone() },
        "AutoMLProblemTypeConfig": { "TabularJobConfig": tabular },
        "RoleArn": role_arn,
    });
    if let Some(objective) = row.objective.clone() {
        body["AutoMLJobObjective"] = objective;
    }
    body
}

/// Poll `DescribeAutoMLJobV2` until the job reaches a status nothing leaves on its own.
async fn settled(emulator: &Emulator, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_BUDGET;
    loop {
        let read = emulator
            .sagemaker_ok("DescribeAutoMLJobV2", &json!({ "AutoMLJobName": name }))
            .await;
        if TERMINAL_STATUSES.contains(&str_field(&read, "AutoMLJobStatus").as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status; last read {read}"
        );
        tokio::time::sleep(TERMINAL_POLL).await;
    }
}

/// The key of an object of [`BUCKET`] named by `uri`.
fn key_of(case: &str, uri: &str) -> String {
    uri.strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| panic!("case {case:?}: {uri} is not an object of the harness bucket"))
        .to_string()
}

/// RULE: a completed job's best candidate carries at least one inference container definition
/// naming a container image and the model artifact the run wrote under the job's output
/// location, and that pair registers as a model as it stands.
#[tokio::test]
async fn the_best_candidate_carries_the_container_it_is_deployable_as() {
    for row in rows() {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let key = "candidates/input/data.csv";
        emulator.given_object(BUCKET, key, &row.data).await;
        let role_arn = emulator
            .given_role("automl-candidate-role", Some(&read_and_write()))
            .await;
        let case = row.case;
        let name = "automl-candidate";

        emulator
            .sagemaker_ok(
                "CreateAutoMLJobV2",
                &create_body(name, &role_arn, key, &row),
            )
            .await;
        let read = settled(&emulator, case, name).await;
        assert_eq!(
            str_field(&read, "AutoMLJobStatus"),
            "Completed",
            "case {case:?}: an accepted job must run to Completed, got {read}"
        );

        let containers = read
            .pointer("/BestCandidate/InferenceContainers")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the best candidate carries no InferenceContainers: {read}")
            });
        assert!(
            !containers.is_empty(),
            "case {case:?}: a completed job's best candidate must carry at least one \
             inference container definition, got {read}"
        );
        // `AutoMLCandidate$InferenceContainers` is `@length 0..5`.
        assert!(
            containers.len() <= 5,
            "case {case:?}: at most five container definitions, got {}",
            containers.len()
        );

        let output = row.output.trim_end_matches('/').to_string();
        for (index, container) in containers.iter().enumerate() {
            let image = str_field(container, "Image");
            // `AutoMLContainerDefinition$Image` is required, `@length 0..255`, pattern
            // `^[\S]+$`: a container reference carrying no whitespace.
            assert!(
                !image.is_empty()
                    && image.len() <= 255
                    && !image.chars().any(char::is_whitespace),
                "case {case:?}: container {index}'s Image must be a container reference, got \
                 {image:?}"
            );
            let artifact = str_field(container, "ModelDataUrl");
            // `ModelDataUrl` is required, `@length 0..1024`, pattern
            // `^(https|s3)://([^/]+)/?(.*)$`.
            assert!(
                artifact.starts_with("s3://") && artifact.len() <= 1024,
                "case {case:?}: container {index}'s ModelDataUrl must name a location, got \
                 {artifact:?}"
            );
            assert!(
                artifact.starts_with(&output),
                "case {case:?}: container {index}'s ModelDataUrl must lie under the job's \
                 output location {output:?}, got {artifact:?}"
            );
            let held = emulator
                .object_bytes(BUCKET, &key_of(case, &artifact))
                .await;
            assert!(
                held.is_some_and(|bytes| !bytes.is_empty()),
                "case {case:?}: Amazon S3 must hold the model artifact container {index} \
                 names at {artifact}"
            );

            // The documented deploy path: the definition registers as a model as it stands,
            // and the registered model reports back exactly what the candidate named.
            let model = format!("{name}-model-{index}");
            emulator
                .sagemaker_ok(
                    "CreateModel",
                    &json!({
                        "ModelName": model,
                        "PrimaryContainer": { "Image": image, "ModelDataUrl": artifact },
                        "ExecutionRoleArn": role_arn,
                    }),
                )
                .await;
            let described = emulator
                .sagemaker_ok("DescribeModel", &json!({ "ModelName": model }))
                .await;
            assert_eq!(
                (
                    described
                        .pointer("/PrimaryContainer/Image")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    described
                        .pointer("/PrimaryContainer/ModelDataUrl")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ),
                (image.clone(), artifact.clone()),
                "case {case:?}: the model registered from container {index} must report the \
                 candidate's own container, got {described}"
            );
        }

        // The candidate's artifact is the one the candidate-definition notebook names it by,
        // so the two reports of the run agree; the best candidate is one of the job's own.
        let candidate = str_field(
            read.pointer("/BestCandidate")
                .unwrap_or_else(|| panic!("case {case:?}: no BestCandidate in {read}")),
            "CandidateName",
        );
        let first = str_field(
            containers
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: no container definition")),
            "ModelDataUrl",
        );
        assert!(
            first.contains(&candidate),
            "case {case:?}: the artifact must be the best candidate {candidate:?}'s own, got \
             {first:?}"
        );
    }
}
