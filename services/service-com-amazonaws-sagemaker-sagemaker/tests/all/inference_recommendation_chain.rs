// SPDX-License-Identifier: CC0-1.0

//! The CHAIN a client drives to a scaling recommendation: train a model, deploy it behind an
//! endpoint, benchmark that endpoint with an Inference Recommender job and ask
//! `GetScalingConfigurationRecommendation` what to autoscale it by.
//!
//! RULE — an Inference Recommender job's single move out of `PENDING` is DECIDED by the call
//! that starts it: `CreateInferenceRecommendationsJob` performs the benchmark and records what
//! it measured before it answers, so the clock owes the terminal status from that instant and
//! the FIRST request after the create reports it. A client driving the job with the documented
//! poll-until-settled loop spends none of its polling interval on it, so a workflow that chains
//! the job onto a training job and an endpoint reaches its last call inside its own deadline.
//!
//! RULE — the job benchmarks the endpoint it names, over the traffic its own sample payload
//! carries, and reports what the invocations MEASURED: the performance of an endpoint the
//! caller already held is reported under `EndpointPerformances`, "the performance results from
//! running an Inference Recommender job on an existing endpoint", naming that endpoint and the
//! metrics measured against it; the scaling configuration is then computed from the
//! recommendation.
//!
//! The rows span what those rules generalise over: the traffic the benchmark sends (no sample
//! payload at all, one record, several records) and therefore how many invocations the
//! measurement is taken over.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the training-run suite's harness
//! ([`crate::training_job_execution`]) rather than a second copy of it.

use serde_json::{json, Value};

use crate::managed_job_orchestration::{ended, TERMINAL_RECOMMENDATION_STATUSES};
use crate::training_job_execution::{
    channel, csv_records, read_and_write, str_field, training_body, Emulator, BUCKET,
};

/// How many records the training channel carries, and how many feature columns each record
/// holds beside its target — the width every payload the model is applied to has to carry.
const CHANNEL_RECORDS: usize = 24;
const CHANNEL_FEATURES: usize = 3;

/// One record of the width the trained model takes, as a `text/csv` payload carries it.
fn payload_record(seed: usize) -> Vec<u8> {
    (0..CHANNEL_FEATURES)
        .map(|column| (seed + column + 1).to_string())
        .collect::<Vec<String>>()
        .join(",")
        .into_bytes()
}

/// The sample payload archive a job names: "this path must point to a single gzip compressed
/// tar archive (.tar.gz suffix)" (`RecommendationJobPayloadConfig$SamplePayloadUrl`), one entry
/// per record the benchmark sends.
fn sample_payload_archive(records: usize) -> Vec<u8> {
    let entries: Vec<(String, Vec<u8>)> = (0..records)
        .map(|record| (format!("payload-{record}.csv"), payload_record(record)))
        .collect();
    service_com_amazonaws_sagemaker_sagemaker::state::model_archive(&entries)
        .expect("the sample payload archive is written")
}

/// The `EndpointStatus` members an endpoint's deployment has SETTLED in
/// (`smithy_query(query="shape",shape="EndpointStatus")`), which is what a client deploying one
/// polls for.
const SETTLED_ENDPOINT_STATUSES: [&str; 3] = ["InService", "Failed", "OutOfService"];

/// The `TrainingJobStatus` members a run has ENDED in.
const TERMINAL_TRAINING_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// Train a model on this emulator's own training and deploy it behind an endpoint, polling each
/// resource for the status its own clock owes it.
///
/// Answers the execution role's ARN.
async fn given_served_endpoint(emulator: &Emulator, name: &str, case: &str) -> String {
    let role_arn = emulator
        .given_role("sagemaker-chain-role", Some(&read_and_write()))
        .await;
    let key = format!("{name}/train/data.csv");
    emulator
        .given_object(BUCKET, &key, &csv_records(CHANNEL_RECORDS, CHANNEL_FEATURES))
        .await;
    emulator
        .sagemaker_ok(
            "CreateTrainingJob",
            &training_body(
                name,
                &role_arn,
                json!([channel("train", &format!("s3://{BUCKET}/{key}"), "S3Prefix")]),
                &format!("s3://{BUCKET}/{name}/output/"),
            ),
        )
        .await;

    // The run is the job's own work and the clock reports each stage of it, so the job is
    // polled for the status it ends in.
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
        .unwrap_or_else(|| panic!("case {case:?}: no artifact in {trained}"))
        .to_string();

    emulator
        .sagemaker_ok(
            "CreateModel",
            &json!({
                "ModelName": name,
                "PrimaryContainer": {
                    "Image": "000000000000.dkr.ecr.us-east-1.amazonaws.com/xgboost:1",
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
    let deployed = ended(
        emulator,
        "DescribeEndpoint",
        &json!({ "EndpointName": name }),
        "EndpointStatus",
        &SETTLED_ENDPOINT_STATUSES,
        case,
    )
    .await;
    assert_eq!(
        str_field(&deployed, "EndpointStatus"),
        "InService",
        "case {case:?}: the deployment the create resolved comes up, got {deployed}"
    );
    role_arn
}

/// THE RULE: the chain reaches its last call without a polling client paying an interval per
/// transition, and the recommendation it answers names the endpoint the job benchmarked.
#[tokio::test]
async fn the_chain_to_a_scaling_recommendation_is_driven_to_its_end() {
    // Rows: the case, the name every resource takes, and how many records the job's sample
    // payload carries (none at all means the job names no payload, and the benchmark generates
    // a record of the width the model takes).
    let rows: [(&str, &str, usize); 3] = [
        ("no sample payload", "chainplain", 0),
        ("a sample payload of one record", "chainone", 1),
        ("a sample payload of several records", "chainmany", 4),
    ];

    for (case, name, payload_records) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = given_served_endpoint(&emulator, name, case).await;

        let mut input_config = json!({
            "ModelName": name,
            "Endpoints": [{ "EndpointName": name }],
        });
        if payload_records > 0 {
            let key = format!("{name}/payload.tar.gz");
            emulator
                .given_object_bytes(BUCKET, &key, &sample_payload_archive(payload_records))
                .await;
            input_config["ContainerConfig"] = json!({
                "Framework": "XGBOOST",
                "PayloadConfig": {
                    "SamplePayloadUrl": format!("s3://{BUCKET}/{key}"),
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

        // The benchmark ran inside the create, so this FIRST read after it reports what it
        // measured: a polling client spends none of its interval on the job.
        let settled = emulator
            .sagemaker_ok(
                "DescribeInferenceRecommendationsJob",
                &json!({ "JobName": name }),
            )
            .await;
        assert_eq!(
            str_field(&settled, "Status"),
            "COMPLETED",
            "case {case:?}: the first read after the create reports the benchmark's own \
             outcome, got {settled}"
        );
        assert!(
            TERMINAL_RECOMMENDATION_STATUSES.contains(&str_field(&settled, "Status").as_str()),
            "case {case:?}: the reported status is a terminal one, got {settled}"
        );
        // Reading it again changes nothing: the move happened at the instant it was due, not
        // because something read the job.
        let reread = emulator
            .sagemaker_ok(
                "DescribeInferenceRecommendationsJob",
                &json!({ "JobName": name }),
            )
            .await;
        assert_eq!(
            reread, settled,
            "case {case:?}: a later read reports exactly the same job, got {reread}"
        );
        // The endpoint the caller already held is reported as that endpoint's own measured
        // performance, which is the member `EndpointPerformances` exists for.
        let performances = settled
            .get("EndpointPerformances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            performances.len(),
            1,
            "case {case:?}: one performance result for the existing endpoint under test, got \
             {settled}"
        );
        let performance = &performances[0];
        assert_eq!(
            performance
                .pointer("/EndpointInfo/EndpointName")
                .and_then(Value::as_str),
            Some(name),
            "case {case:?}: the performance names the endpoint it was measured on, got \
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
            "case {case:?}: the metrics are the ones the invocations against that endpoint \
             measured, got {performance}"
        );
        let recommendations = settled
            .get("InferenceRecommendations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            recommendations.len(),
            1,
            "case {case:?}: one measured recommendation for the endpoint under test, got \
             {settled}"
        );
        let measured = &recommendations[0];
        assert_eq!(
            measured
                .pointer("/EndpointConfiguration/EndpointName")
                .and_then(Value::as_str),
            Some(name),
            "case {case:?}: the recommendation names the benchmarked endpoint, got {measured}"
        );
        let recommendation_id = measured
            .get("RecommendationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !recommendation_id.is_empty(),
            "case {case:?}: a measured recommendation carries its own id, got {measured}"
        );

        // The last call of the chain: the scaling configuration computed from that
        // recommendation, addressed by the endpoint the job benchmarked.
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
                scaling
                    .get("InferenceRecommendationsJobName")
                    .and_then(Value::as_str),
                scaling.get("EndpointName").and_then(Value::as_str),
            ),
            (Some(name), Some(name)),
            "case {case:?}: the answer echoes the job and the endpoint it was asked about, got \
             {scaling}"
        );
        let min = scaling
            .pointer("/DynamicScalingConfiguration/MinCapacity")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let max = scaling
            .pointer("/DynamicScalingConfiguration/MaxCapacity")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            1 <= min && min <= max,
            "case {case:?}: the recommended capacity is a usable range, got {scaling}"
        );
    }
}

/// THE RULE's other side: a job whose benchmark could not invoke the model under test reports
/// the modelled failure with the condition that stopped it, and no recommendation — so a
/// polling client reaches a terminal status either way rather than waiting on one that never
/// comes.
#[tokio::test]
async fn a_benchmark_that_could_not_invoke_the_model_reports_why() {
    // Rows: the case and the sample payload's records, each a payload the model cannot be
    // applied to (a record of the wrong width, and bytes holding no record at all).
    let rows: [(&str, &str); 2] = [
        ("a record of the wrong width", "1,2"),
        ("bytes holding no record", "not-a-record"),
    ];

    for (case, payload) in rows {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let name = "chainrefused";
        let role_arn = given_served_endpoint(&emulator, name, case).await;
        let key = format!("{name}/payload.tar.gz");
        let archive = service_com_amazonaws_sagemaker_sagemaker::state::model_archive(&[(
            "payload.csv".to_string(),
            payload.as_bytes().to_vec(),
        )])
        .expect("the sample payload archive is written");
        emulator.given_object_bytes(BUCKET, &key, &archive).await;

        emulator
            .sagemaker_ok(
                "CreateInferenceRecommendationsJob",
                &json!({
                    "JobName": name,
                    "JobType": "Default",
                    "RoleArn": role_arn,
                    "InputConfig": {
                        "ModelName": name,
                        "Endpoints": [{ "EndpointName": name }],
                        "ContainerConfig": {
                            "PayloadConfig": { "SamplePayloadUrl": format!("s3://{BUCKET}/{key}") },
                        },
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
            "FAILED",
            "case {case:?}: a benchmark that could not invoke the model must fail, got {settled}"
        );
        assert!(
            !str_field(&settled, "FailureReason").is_empty(),
            "case {case:?}: a failed job must say what stopped it, got {settled}"
        );
        assert!(
            settled.get("InferenceRecommendations").is_none(),
            "case {case:?}: nothing was measured, so no recommendation is reported, got \
             {settled}"
        );
        assert!(
            settled.get("EndpointPerformances").is_none(),
            "case {case:?}: nothing was measured on the endpoint either, so no performance is \
             reported, got {settled}"
        );
    }
}
