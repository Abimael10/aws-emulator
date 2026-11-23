// SPDX-License-Identifier: CC0-1.0

//! What an Inference Recommender job reports about an endpoint the CALLER already holds, and
//! what a scaling configuration computed from it answers.
//!
//! RULE — `RecommendationJobInputConfig$Endpoints` is "existing customer endpoints on which to
//! run an Inference Recommender job", and
//! `DescribeInferenceRecommendationsJob$EndpointPerformances` is "the performance results from
//! running an Inference Recommender job on an existing endpoint". So a job that named such an
//! endpoint reaches the terminal `COMPLETED`, reports that endpoint under
//! `EndpointPerformances[].EndpointInfo.EndpointName` with the `InferenceMetrics` its own
//! invocations measured, and `GetScalingConfigurationRecommendation` — addressed by
//! `InferenceRecommendationsJobName` and that same `EndpointName` — answers a
//! `DynamicScalingConfiguration` whose `MinCapacity` is at least one and whose `MaxCapacity` is
//! at least its `MinCapacity`, echoing both names back. A job whose tests ran on endpoints
//! SageMaker deployed for them reports no `EndpointPerformances`: no endpoint of the caller's
//! was under test.
//!
//! The rows span what the rule generalises over: where the benchmark's traffic comes from (the
//! sample payload the job names, and the generator when it names none), whether the job names
//! the model under test itself or leaves it to the endpoint it benchmarks, and how the endpoint
//! under test is reached (one the account already holds, and one the job deploys for an
//! `EndpointConfigurations` entry of its own).
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the training-run suite's harness
//! ([`crate::training_job_execution`]) rather than a second copy of it. Every wait is a poll for
//! the CONDITION, never a fixed sleep.

use serde_json::{json, Value};

use crate::managed_job_orchestration::{ended, TERMINAL_RECOMMENDATION_STATUSES};
use crate::training_job_execution::{
    channel, csv_records, read_and_write, str_field, training_body, Emulator, BUCKET,
};

/// How many feature columns the staged training data carries, which is the width of the model
/// the benchmark's traffic has to carry for the model to be applied to it.
const FEATURES: usize = 3;

/// The statuses a training job's work has ENDED in.
const TERMINAL_TRAINING_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// Train a model and deploy it behind the endpoint `name`, and answer the execution role.
async fn given_served_endpoint(emulator: &Emulator, name: &str, case: &str) -> String {
    let role_arn = emulator
        .given_role("sagemaker-benchmark-role", Some(&read_and_write()))
        .await;
    let key = format!("{name}/train/data.csv");
    emulator
        .given_object(BUCKET, &key, &csv_records(16, FEATURES))
        .await;
    let body = training_body(
        name,
        &role_arn,
        json!([channel("train", &format!("s3://{BUCKET}/{key}"), "S3Prefix")]),
        &format!("s3://{BUCKET}/{name}/output/"),
    );
    emulator.sagemaker_ok("CreateTrainingJob", &body).await;
    let trained = ended(
        emulator,
        "DescribeTrainingJob",
        &json!({ "TrainingJobName": name }),
        "TrainingJobStatus",
        &TERMINAL_TRAINING_STATUSES,
        case,
    )
    .await;
    assert_eq!(
        str_field(&trained, "TrainingJobStatus"),
        "Completed",
        "case {case:?}: the model under test has to be trained first, got {trained}"
    );
    let artifact = trained
        .pointer("/ModelArtifacts/S3ModelArtifacts")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    emulator
        .sagemaker_ok(
            "CreateModel",
            &json!({
                "ModelName": name,
                "PrimaryContainer": {
                    "Image": "683313688378.dkr.ecr.us-west-2.amazonaws.com/sagemaker-xgboost:1.7-1",
                    "ModelDataUrl": artifact,
                },
                "ExecutionRoleArn": role_arn,
            }),
        )
        .await;
    let config = format!("{name}-config");
    emulator
        .sagemaker_ok(
            "CreateEndpointConfig",
            &json!({
                "EndpointConfigName": config,
                "ProductionVariants": [{
                    "VariantName": "AllTraffic",
                    "ModelName": name,
                    "InstanceType": "ml.m5.large",
                    "InitialInstanceCount": 1,
                }],
            }),
        )
        .await;
    emulator
        .sagemaker_ok(
            "CreateEndpoint",
            &json!({ "EndpointName": name, "EndpointConfigName": config }),
        )
        .await;
    role_arn
}

/// Stage a sample payload archive — the gzip-compressed tar
/// `RecommendationJobPayloadConfig$SamplePayloadUrl` names — holding `entries` objects of
/// inference records, and answer its URI.
async fn given_sample_payload(emulator: &Emulator, name: &str, entries: usize) -> String {
    let row: String = (1..=FEATURES)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let files: Vec<(String, Vec<u8>)> = (0..entries)
        .map(|index| {
            (
                format!("payload-{index}.csv"),
                format!("{row}\n{row}\n").into_bytes(),
            )
        })
        .collect();
    let archive = service_com_amazonaws_sagemaker_sagemaker::state::model_archive(&files)
        .expect("the sample payload archive is built");
    let key = format!("{name}/payload.tar.gz");
    emulator.given_object_bytes(BUCKET, &key, &archive).await;
    format!("s3://{BUCKET}/{key}")
}

/// THE RULE: a job that benchmarked an endpoint the caller holds reports that endpoint's
/// measured performance, and a scaling configuration follows from it by endpoint name.
#[tokio::test]
async fn a_benchmark_of_an_existing_endpoint_reports_its_performance() {
    // (case, the name every resource takes, how many objects the sample payload holds — none
    // at all for a job that names no payload, and whether the job names the model under test
    // itself or leaves it to the endpoint it benchmarks)
    let rows: [(&str, &str, Option<usize>, bool); 3] = [
        (
            "traffic the job's sample payload carries",
            "rec-payload",
            Some(2),
            true,
        ),
        ("traffic the generator sends", "rec-generated", None, true),
        (
            "the model named only by the endpoint under test",
            "rec-endpoint-only",
            None,
            false,
        ),
    ];

    for (case, name, payload_entries, names_model) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = given_served_endpoint(&emulator, name, case).await;

        let mut input_config = json!({ "Endpoints": [{ "EndpointName": name }] });
        if names_model {
            input_config["ModelName"] = json!(name);
        }
        if let Some(entries) = payload_entries {
            let uri = given_sample_payload(&emulator, name, entries).await;
            input_config["ContainerConfig"] = json!({
                "PayloadConfig": {
                    "SamplePayloadUrl": uri,
                    "SupportedContentTypes": ["text/csv"],
                },
            });
        }
        emulator
            .sagemaker_ok(
                "CreateInferenceRecommendationsJob",
                &json!({
                    "JobName": name,
                    "JobType": "Default",
                    "RoleArn": role_arn,
                    "InputConfig": input_config,
                }),
            )
            .await;

        let settled = ended(
            &emulator,
            "DescribeInferenceRecommendationsJob",
            &json!({ "JobName": name }),
            "Status",
            &TERMINAL_RECOMMENDATION_STATUSES,
            case,
        )
        .await;
        assert_eq!(
            str_field(&settled, "Status"),
            "COMPLETED",
            "case {case:?}: the benchmark invoked the endpoint's model, so the job completes, \
             got {settled}"
        );

        // What the job measured ON THE CALLER'S OWN ENDPOINT, which is what
        // `EndpointPerformances` reports.
        let performances = settled
            .get("EndpointPerformances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            performances.len(),
            1,
            "case {case:?}: expected the performance of the endpoint under test, got {settled}"
        );
        let performance = &performances[0];
        assert_eq!(
            performance
                .pointer("/EndpointInfo/EndpointName")
                .and_then(Value::as_str),
            Some(name),
            "case {case:?}: the performance names the endpoint the job benchmarked, got \
             {performance}"
        );
        let invocations = performance
            .pointer("/Metrics/MaxInvocations")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let latency = performance
            .pointer("/Metrics/ModelLatency")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            invocations >= 1 && latency >= 1,
            "case {case:?}: the metrics are the ones the invocations measured, got {performance}"
        );

        // And the scaling configuration a caller asks for BY ENDPOINT NAME is computed from
        // that same measurement.
        let scaling = emulator
            .sagemaker_ok(
                "GetScalingConfigurationRecommendation",
                &json!({
                    "InferenceRecommendationsJobName": name,
                    "EndpointName": name,
                }),
            )
            .await;
        assert_eq!(
            (
                str_field(&scaling, "InferenceRecommendationsJobName"),
                str_field(&scaling, "EndpointName"),
            ),
            (name.to_string(), name.to_string()),
            "case {case:?}: the answer echoes the job and the endpoint it was computed for, \
             got {scaling}"
        );
        let min_capacity = scaling
            .pointer("/DynamicScalingConfiguration/MinCapacity")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let max_capacity = scaling
            .pointer("/DynamicScalingConfiguration/MaxCapacity")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            min_capacity >= 1 && max_capacity >= min_capacity,
            "case {case:?}: expected a capacity band of at least one instance, got {scaling}"
        );
        assert_eq!(
            scaling.pointer("/Metric/ModelLatency"),
            Some(&json!(latency)),
            "case {case:?}: the metric it reports is the measured latency, got {scaling}"
        );

        // The endpoint the job benchmarked is untouched by it: it still serves, with no failure
        // reason.
        let endpoint = emulator
            .sagemaker_ok("DescribeEndpoint", &json!({ "EndpointName": name }))
            .await;
        assert!(
            endpoint.get("FailureReason").is_none(),
            "case {case:?}: the benchmark leaves the endpoint unharmed, got {endpoint}"
        );
    }
}

/// THE OTHER HALF of the rule: a job whose tests ran on an endpoint SageMaker deployed for them
/// reports no `EndpointPerformances`, because no endpoint of the caller's was under test — while
/// the recommendation it measured is reported as such.
#[tokio::test]
async fn a_job_that_deployed_its_own_endpoint_reports_no_endpoint_performance() {
    let case = "the endpoint the job deploys itself";
    let name = "rec-deployed-only";
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role_arn = given_served_endpoint(&emulator, name, case).await;

    emulator
        .sagemaker_ok(
            "CreateInferenceRecommendationsJob",
            &json!({
                "JobName": name,
                "JobType": "Advanced",
                "RoleArn": role_arn,
                "InputConfig": {
                    "ModelName": name,
                    "EndpointConfigurations": [{ "InstanceType": "ml.c5.xlarge" }],
                    "ResourceLimit": { "MaxNumberOfTests": 1, "MaxParallelOfTests": 1 },
                },
            }),
        )
        .await;

    let settled = ended(
        &emulator,
        "DescribeInferenceRecommendationsJob",
        &json!({ "JobName": name }),
        "Status",
        &TERMINAL_RECOMMENDATION_STATUSES,
        case,
    )
    .await;
    assert_eq!(
        str_field(&settled, "Status"),
        "COMPLETED",
        "case {case:?}: the benchmark invoked the model under test, so the job completes, got \
         {settled}"
    );
    assert!(
        settled.get("EndpointPerformances").is_none(),
        "case {case:?}: no endpoint of the caller's was under test, so none is reported, got \
         {settled}"
    );
    let recommendations = settled
        .get("InferenceRecommendations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        recommendations.len(),
        1,
        "case {case:?}: one measured recommendation per endpoint tested, got {settled}"
    );
    let benchmarked = recommendations[0]
        .pointer("/EndpointConfiguration/EndpointName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        benchmarked.starts_with("sm-ep-"),
        "case {case:?}: the job reports the endpoint it deployed for the test, got {settled}"
    );
}
