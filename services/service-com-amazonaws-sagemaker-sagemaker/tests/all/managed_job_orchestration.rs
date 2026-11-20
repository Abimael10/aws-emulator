// SPDX-License-Identifier: CC0-1.0

//! The jobs SageMaker drives on the caller's behalf, end to end: the training jobs a
//! HYPERPARAMETER TUNING job launches and picks a best one from, and the load test an
//! INFERENCE RECOMMENDER job runs against an endpoint before
//! `GetScalingConfigurationRecommendation` computes a scaling configuration from it.
//!
//! RULE — a job whose work is OTHER work the service performs is driven server-side, and what
//! it reports is what that work produced:
//!
//! * "Hyperparameter tuning uses the value of this metric to evaluate the training jobs it
//!   launches, and returns the training job that results in either the highest or lowest value
//!   for this metric" (`HyperParameterTuningJobObjective`), bounded by
//!   `ResourceLimits$MaxNumberOfTrainingJobs` — so the tuning job launches that many training
//!   jobs, each of which exists in its own right and carries the tuning job's `TuningJobArn`,
//!   and `BestTrainingJob` names the one the objective picked;
//! * `RecommendationMetrics` is "the metrics used to decide what recommendation to make",
//!   `MaxInvocations` "the expected maximum number of requests per minute for the instance" and
//!   `ModelLatency` "the expected model latency at maximum invocation per minute" — so a job
//!   that benchmarked an endpoint reaches `COMPLETED` carrying one measured recommendation per
//!   endpoint it tested, and `GetScalingConfigurationRecommendation` answers a
//!   `DynamicScalingConfiguration` from it.
//!
//! The rows span what each rule generalises over: for the tuning job, how many training jobs
//! it is allowed to launch and which way its objective is optimised; for the recommendation
//! job, how the model under test is reached (an endpoint the account already holds, or the
//! endpoint the job deploys for an `EndpointConfigurations` entry of its own).
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the training-run suite's harness
//! ([`crate::training_job_execution`]) rather than a second copy of it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    channel, csv_records, read_and_write, str_field, Emulator, BUCKET,
};

/// How long a job is polled for the status it has to reach, and how often it is looked at.
/// What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a case
/// the clock would have served.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// The `HyperParameterTuningJobStatus` members a tuning job's work has ENDED in
/// (`smithy_query(query="shape",shape="HyperParameterTuningJobStatus")`).
const TERMINAL_TUNING_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The `RecommendationJobStatus` members a recommendation job's work has ENDED in
/// (`smithy_query(query="shape",shape="RecommendationJobStatus")`).
pub(crate) const TERMINAL_RECOMMENDATION_STATUSES: [&str; 3] = ["COMPLETED", "FAILED", "STOPPED"];

/// Stage one channel's records in Amazon S3 and answer the `InputDataConfig` addressing it.
async fn given_channel(emulator: &Emulator, prefix: &str, name: &str) -> Value {
    let key = format!("{prefix}/{name}/data.csv");
    emulator
        .given_object(BUCKET, &key, &csv_records(24, 3))
        .await;
    json!([channel(name, &format!("s3://{BUCKET}/{key}"), "S3Prefix")])
}

/// Poll `target` until the job reaches one of `terminal`, and answer that read.
///
/// `pub(crate)` so a sibling suite that drives a job of this service to its terminal status
/// polls through this one helper rather than a copy of it.
pub(crate) async fn ended(
    emulator: &Emulator,
    target: &str,
    body: &Value,
    status_member: &str,
    terminal: &[&str],
    case: &str,
) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.sagemaker_ok(target, body).await;
        if terminal.contains(&str_field(&read, status_member).as_str()) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal {status_member}; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// THE RULE for a tuning job: it launches the training jobs its definition declares, each of
/// them a training job this account holds, and reports the one its objective picked.
#[tokio::test]
async fn a_tuning_job_launches_its_training_jobs_and_reports_the_best_one() {
    // Rows: the case, how many training jobs the tuning job may launch, and which way its
    // objective is optimised.
    let rows: [(&str, &str, i64, &str); 3] = [
        ("one training job, minimising", "tune-one", 1, "Minimize"),
        ("three training jobs, minimising", "tune-three", 3, "Minimize"),
        ("two training jobs, maximising", "tune-max", 2, "Maximize"),
    ];

    for (case, name, jobs, direction) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("sagemaker-tuning-role", Some(&read_and_write()))
            .await;
        let channels = given_channel(&emulator, name, "validation").await;

        let body = json!({
            "HyperParameterTuningJobName": name,
            "HyperParameterTuningJobConfig": {
                "Strategy": "Random",
                "HyperParameterTuningJobObjective": {
                    "Type": direction,
                    "MetricName": "validation:rmse",
                },
                "ResourceLimits": {
                    "MaxNumberOfTrainingJobs": jobs,
                    "MaxParallelTrainingJobs": 1,
                },
                "ParameterRanges": {
                    "IntegerParameterRanges": [{
                        "Name": "max_depth",
                        "MinValue": "2",
                        "MaxValue": "8",
                    }],
                },
            },
            "TrainingJobDefinition": {
                "DefinitionName": "tuned",
                "AlgorithmSpecification": {
                    "TrainingImage": "000000000000.dkr.ecr.us-east-1.amazonaws.com/trainer:1",
                    "TrainingInputMode": "File",
                },
                "RoleArn": role_arn,
                "InputDataConfig": channels,
                "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/{name}/output/") },
                "ResourceConfig": {
                    "InstanceType": "ml.m5.large",
                    "InstanceCount": 1,
                    "VolumeSizeInGB": 10,
                },
                "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
                "StaticHyperParameters": { "objective": "reg:squarederror" },
            },
        });
        emulator
            .sagemaker_ok("CreateHyperParameterTuningJob", &body)
            .await;

        let settled = ended(
            &emulator,
            "DescribeHyperParameterTuningJob",
            &json!({ "HyperParameterTuningJobName": name }),
            "HyperParameterTuningJobStatus",
            &TERMINAL_TUNING_STATUSES,
            case,
        )
        .await;
        assert_eq!(
            str_field(&settled, "HyperParameterTuningJobStatus"),
            "Completed",
            "case {case:?}: every launched training job ran, so the tuning job completes, got \
             {settled}"
        );
        assert_eq!(
            settled.pointer("/TrainingJobStatusCounters/Completed"),
            Some(&json!(jobs)),
            "case {case:?}: the counters report the training jobs it launched, got {settled}"
        );
        assert_eq!(
            settled.pointer("/ObjectiveStatusCounters/Succeeded"),
            Some(&json!(jobs)),
            "case {case:?}: each launched job evaluated the objective metric, got {settled}"
        );

        // `BestTrainingJob` names a training job that EXISTS, carries the objective metric it
        // was picked by, and reports the tuning job that launched it.
        let best = settled
            .get("BestTrainingJob")
            .unwrap_or_else(|| panic!("case {case:?}: no BestTrainingJob in {settled}"))
            .clone();
        assert_eq!(
            best.pointer("/FinalHyperParameterTuningJobObjectiveMetric/MetricName"),
            Some(&json!("validation:rmse")),
            "case {case:?}: the best job carries the objective metric, got {best}"
        );
        assert_eq!(
            best.get("ObjectiveStatus").and_then(Value::as_str),
            Some("Succeeded"),
            "case {case:?}: the best job's objective metric was evaluated, got {best}"
        );
        assert_eq!(
            best.get("TrainingJobDefinitionName").and_then(Value::as_str),
            Some("tuned"),
            "case {case:?}: the best job names the definition it came from, got {best}"
        );
        let child = str_field(&best, "TrainingJobName");
        let described = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": child }))
            .await;
        assert_eq!(
            str_field(&described, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: the training job the tuning job launched is one this account \
             holds, and it ran, got {described}"
        );
        assert_eq!(
            described.get("TuningJobArn").and_then(Value::as_str),
            settled
                .get("HyperParameterTuningJobArn")
                .and_then(Value::as_str),
            "case {case:?}: the launched job names the tuning job that launched it, got \
             {described}"
        );
        assert!(
            described
                .pointer("/HyperParameters/max_depth")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
                .is_some_and(|value| (2..=8).contains(&value)),
            "case {case:?}: the launched job's searched hyperparameter sits inside the \
             declared range, got {described}"
        );
        assert_eq!(
            described.pointer("/HyperParameters/objective"),
            Some(&json!("reg:squarederror")),
            "case {case:?}: every launched job is given the static hyperparameters, got \
             {described}"
        );
        // The objective value the tuning job ranked by is a value the run MEASURED, which is
        // what the training job itself reports as its final metric.
        let measured = described
            .get("FinalMetricDataList")
            .and_then(Value::as_array)
            .map(|metrics| {
                metrics
                    .iter()
                    .filter_map(|metric| metric.get("Value").and_then(Value::as_f64))
                    .collect::<Vec<f64>>()
            })
            .unwrap_or_default();
        let ranked = best
            .pointer("/FinalHyperParameterTuningJobObjectiveMetric/Value")
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN);
        assert!(
            measured
                .iter()
                .any(|value| (value - ranked).abs() < 1e-3),
            "case {case:?}: the objective value is one the run measured, got {ranked} against \
             {measured:?}"
        );
    }
}

/// Train a model, deploy it behind an endpoint and answer `(role, model name, endpoint name,
/// artifact URI)` — the state a recommendation job benchmarks.
async fn given_served_model(emulator: &Emulator, name: &str, case: &str) -> (String, String) {
    let role_arn = emulator
        .given_role("sagemaker-recommender-role", Some(&read_and_write()))
        .await;
    let channels = given_channel(emulator, name, "train").await;
    let body = crate::training_job_execution::training_body(
        name,
        &role_arn,
        channels,
        &format!("s3://{BUCKET}/{name}/output/"),
    );
    emulator.sagemaker_ok("CreateTrainingJob", &body).await;
    let trained = ended(
        emulator,
        "DescribeTrainingJob",
        &json!({ "TrainingJobName": name }),
        "TrainingJobStatus",
        &["Completed", "Failed", "Stopped"],
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
                    "Image": "000000000000.dkr.ecr.us-east-1.amazonaws.com/trainer:1",
                    "ModelDataUrl": artifact,
                },
                "ExecutionRoleArn": role_arn,
            }),
        )
        .await;
    (role_arn, artifact)
}

/// THE RULE for a recommendation job: it benchmarks the model under test and reports what the
/// benchmark MEASURED, which a scaling configuration is then computed from.
#[tokio::test]
async fn a_recommendation_job_benchmarks_the_model_and_a_scaling_configuration_follows() {
    // Rows: the case, the name every resource takes, and whether the job names an EXISTING
    // endpoint to benchmark or has SageMaker deploy one for an endpoint configuration of its
    // own.
    let rows: [(&str, &str, bool); 2] = [
        ("an endpoint the account already holds", "rec-existing", true),
        ("the endpoint the job deploys itself", "rec-deployed", false),
    ];

    for (case, name, existing_endpoint) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let (role_arn, _artifact) = given_served_model(&emulator, name, case).await;

        let mut input_config = json!({ "ModelName": name });
        if existing_endpoint {
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
            input_config["Endpoints"] = json!([{ "EndpointName": name }]);
        } else {
            input_config["EndpointConfigurations"] = json!([{ "InstanceType": "ml.c5.xlarge" }]);
            input_config["ResourceLimit"] = json!({ "MaxNumberOfTests": 1, "MaxParallelOfTests": 1 });
        }

        emulator
            .sagemaker_ok(
                "CreateInferenceRecommendationsJob",
                &json!({
                    "JobName": name,
                    "JobType": "Advanced",
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
            "case {case:?}: the benchmark invoked the model, so the job completes, got {settled}"
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
        let measured = &recommendations[0];
        let invocations = measured
            .pointer("/Metrics/MaxInvocations")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let latency = measured
            .pointer("/Metrics/ModelLatency")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            invocations >= 1 && latency >= 1,
            "case {case:?}: the metrics are the ones the invocations measured, got {measured}"
        );
        let benchmarked = measured
            .pointer("/EndpointConfiguration/EndpointName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if existing_endpoint {
            assert_eq!(
                benchmarked, name,
                "case {case:?}: the job benchmarked the endpoint it named, got {measured}"
            );
            assert_eq!(
                measured.pointer("/EndpointConfiguration/InstanceType"),
                Some(&json!("ml.m5.large")),
                "case {case:?}: the test ran on the endpoint's own ML compute, got {measured}"
            );
        } else {
            assert!(
                benchmarked.starts_with("sm-ep-"),
                "case {case:?}: the job reports the endpoint it deployed for the test, got \
                 {measured}"
            );
            assert_eq!(
                measured.pointer("/EndpointConfiguration/InstanceType"),
                Some(&json!("ml.c5.xlarge")),
                "case {case:?}: the test ran on the instance type under test, got {measured}"
            );
        }

        // And the recommendation is what a scaling configuration is computed from, by the
        // `RecommendationId` the job reports for it.
        let recommendation_id = measured
            .get("RecommendationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let scaling = emulator
            .sagemaker_ok(
                "GetScalingConfigurationRecommendation",
                &json!({
                    "InferenceRecommendationsJobName": name,
                    "RecommendationId": recommendation_id,
                    "TargetCpuUtilizationPerCore": 50,
                }),
            )
            .await;
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
            "case {case:?}: the scaling configuration is computed from the measurement, got \
             {scaling}"
        );
        assert_eq!(
            scaling.get("RecommendationId").and_then(Value::as_str),
            Some(recommendation_id.as_str()),
            "case {case:?}: the answer names the recommendation it was computed from, got \
             {scaling}"
        );
        assert_eq!(
            scaling.pointer("/Metric/ModelLatency"),
            Some(&json!(latency)),
            "case {case:?}: the metric it reports is the measured latency, got {scaling}"
        );
    }
}
