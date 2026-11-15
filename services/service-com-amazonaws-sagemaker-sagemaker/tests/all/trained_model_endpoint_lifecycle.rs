// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of a trained model: the training job that produces the model artifact, the
//! endpoint deployed from that artifact, and the refusal the control plane answers once the
//! endpoint is deleted — the pipeline a train-then-deploy client runs, in one flow.
//!
//! RULE — an accepted `CreateTrainingJob` whose channel data the execution role can read and
//! whose output location it can write is EXECUTED server-side, so the clock settles it in the
//! modelled `Completed` status with NO `FailureReason` and `DescribeTrainingJob` reports
//! `ModelArtifacts.S3ModelArtifacts` naming an object Amazon S3 itself holds under the
//! request's `OutputDataConfig.S3OutputPath`; that artifact is deployable as it stands —
//! `CreateModel` → `CreateEndpointConfig` → `CreateEndpoint` carries the endpoint from
//! `Creating` to `InService` — and `DeleteEndpoint` then removes the endpoint ALONE: the reader
//! refuses the name it addressed with the modelled 400 `ValidationException`, while the object
//! the run wrote, which no endpoint owns, is still readable and still the one the job reports.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine — what `core/router` assembles — so every object is read and written in the service
//! that OWNS objects and every run is authorized against the role records IAM itself wrote.
//! Every call travels the real wire through `MultiServiceRouter::dispatch`, so what is asserted
//! is what an SDK client would see. The harness, the body builders and the artifact-location
//! reading are the training-run suite's ([`crate::training_job_execution`]) rather than a second
//! copy of them.
//!
//! The data plane's half of the same pipeline — what `InvokeEndpoint` answers for a payload once
//! the endpoint is `InService` — lives in the SageMaker runtime's own service and is asked in
//! `core/router/tests/all/sagemaker_endpoint_serves_a_trained_model.rs`, the suite that can mount
//! both planes; this one asks the control plane's half, which is what this crate owns.
//!
//! The rows span what the rule generalises over: how a channel's location is addressed (one
//! object, a prefix holding several), how many channels the job declares, how many records and
//! feature columns its data carries, and the form of the output location the artifact lands
//! under (a prefix ending in a separator, one without, a nested prefix, the bucket root).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_key, artifact_uri, channel, csv_records, error_code, read_and_write, training_body,
    Emulator, BUCKET,
};

/// The account the emulator mints its ARNs under.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The variant each endpoint hosts its one model on.
const VARIANT: &str = "AllTraffic";

/// How long a resource is polled for the status it has to reach before a case fails, and how
/// often it is looked at. What is waited for is the CONDITION, re-read in a loop, so a loaded
/// host cannot fail a case the clock would have served; the budget is far longer than any span
/// this state gives a job's work or a deployment.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// Every member of `com.amazonaws.sagemaker#TrainingJobStatus`
/// (`smithy_query(query="shape",shape="TrainingJobStatus")`): a job never reads a status outside
/// the modelled enum.
const TRAINING_JOB_STATUS_VALUES: [&str; 6] = [
    "InProgress",
    "Completed",
    "Failed",
    "Stopping",
    "Stopped",
    "Deleting",
];

/// The members of `TrainingJobStatus` a job's work has ENDED in, whatever it ended as. A poll
/// stops at one of these, so a job that settled `Failed` is reported with the reason it carries
/// instead of waiting the budget out.
const TERMINAL_TRAINING_STATUSES: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// Every member of `com.amazonaws.sagemaker#EndpointStatus`
/// (`smithy_query(query="shape",shape="EndpointStatus")`).
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

/// The most bytes `ModelArtifacts$S3ModelArtifacts` may carry: `@length 0..1024` on the modelled
/// `S3Uri` (`smithy_query(query="shape",shape="ModelArtifacts")`).
const MAX_S3_URI_BYTES: usize = 1024;

/// The `Message` an error body carries. (`error_code`, the other half of a refusal, is the
/// training-run harness's own reading, imported above rather than copied.)
fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row of the lifecycle table.
struct Row {
    case: &'static str,
    /// The name every resource of the row is created under.
    name: &'static str,
    /// `OutputDataConfig.S3OutputPath`, as the request writes it.
    output: String,
    /// How many feature columns each record carries.
    features: usize,
    /// Whether the row's channels address a PREFIX holding several objects rather than one
    /// object by its own key.
    prefix_addressed: bool,
    /// The channels the job declares; the first is the `train` channel the fit reads.
    channels: &'static [&'static str],
    /// How many records each channel's data carries.
    records: usize,
}

/// The lifecycle table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one object, an output prefix ending in a separator",
            name: "lifecycle-single",
            output: format!("s3://{BUCKET}/output/"),
            features: 3,
            prefix_addressed: false,
            channels: &["train"],
            records: 24,
        },
        Row {
            case: "a prefix holding several objects, an output prefix with no separator",
            name: "lifecycle-prefix",
            output: format!("s3://{BUCKET}/artifacts"),
            features: 1,
            prefix_addressed: true,
            channels: &["train"],
            records: 12,
        },
        Row {
            case: "two channels, a nested output prefix",
            name: "lifecycle-two-channels",
            output: format!("s3://{BUCKET}/runs/nested/out/"),
            features: 5,
            prefix_addressed: false,
            channels: &["train", "validation"],
            records: 40,
        },
        Row {
            case: "the bucket root as the output location",
            name: "lifecycle-bucket-root",
            output: format!("s3://{BUCKET}"),
            features: 2,
            prefix_addressed: true,
            channels: &["train"],
            records: 8,
        },
    ]
}

/// Stage the row's channel data in Amazon S3 and answer the `InputDataConfig` addressing it.
async fn given_channels(emulator: &Emulator, row: &Row) -> Value {
    let mut channels = Vec::new();
    for name in row.channels {
        let data = csv_records(row.records, row.features);
        let uri = if row.prefix_addressed {
            // The same records split over two objects under one prefix, so the channel's
            // location answers more than one object.
            let prefix = format!("{}/{name}", row.name);
            let lines: Vec<&str> = data.lines().collect();
            let (head, tail) = lines.split_at(lines.len() / 2);
            emulator
                .given_object(
                    BUCKET,
                    &format!("{prefix}/part-1.csv"),
                    &format!("{}\n", head.join("\n")),
                )
                .await;
            emulator
                .given_object(
                    BUCKET,
                    &format!("{prefix}/part-2.csv"),
                    &format!("{}\n", tail.join("\n")),
                )
                .await;
            format!("s3://{BUCKET}/{prefix}/")
        } else {
            let key = format!("{}/{name}/data.csv", row.name);
            emulator.given_object(BUCKET, &key, &data).await;
            format!("s3://{BUCKET}/{key}")
        };
        channels.push(channel(name, &uri, "S3Prefix"));
    }
    Value::Array(channels)
}

/// Poll `DescribeTrainingJob` until the job's work has ENDED, and answer the read that saw it
/// end — asserting there that an accepted job whose data was readable and whose output location
/// was writable ended `Completed`, reporting whatever `FailureReason` it carries when it did not.
async fn ended_training_job(emulator: &Emulator, case: &str, job: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "TrainingJobName": job });
    loop {
        let read = emulator.sagemaker_ok("DescribeTrainingJob", &body).await;
        let status = read
            .get("TrainingJobStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            TRAINING_JOB_STATUS_VALUES.contains(&status.as_str()),
            "case {case:?}: TrainingJobStatus must be a modelled value, got {status:?}"
        );
        if TERMINAL_TRAINING_STATUSES.contains(&status.as_str()) {
            assert_eq!(
                status, "Completed",
                "case {case:?}: the accepted job must be run to completion, got {status:?} with \
                 FailureReason {:?}",
                read.get("FailureReason")
            );
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal TrainingJobStatus; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Poll `DescribeEndpoint` until the endpoint is `InService`, answering the read that saw it —
/// checking on the way that every status it is observed in is a modelled one, and one a created
/// endpoint may be in.
async fn in_service_endpoint(emulator: &Emulator, case: &str, endpoint: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let body = json!({ "EndpointName": endpoint });
    loop {
        let read = emulator.sagemaker_ok("DescribeEndpoint", &body).await;
        let status = read
            .get("EndpointStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            ENDPOINT_STATUS_VALUES.contains(&status.as_str()),
            "case {case:?}: EndpointStatus must be a modelled value, got {status:?}"
        );
        assert!(
            status == "Creating" || status == "InService",
            "case {case:?}: a created endpoint is coming up or is up, got {status:?} with {read}"
        );
        if status == "InService" {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint never reached InService; last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// THE RULE: an accepted job is run, what it wrote is deployable as it stands, and deleting the
/// endpoint that served it removes the endpoint alone.
#[tokio::test]
async fn a_trained_model_is_deployable_and_outlives_the_endpoint_that_served_it() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("sagemaker-lifecycle-role", Some(&read_and_write()))
            .await;
        let channels = given_channels(&emulator, &row).await;

        // The job is ACCEPTED, and the acceptance names the job this account now holds.
        let accepted = emulator
            .sagemaker_ok(
                "CreateTrainingJob",
                &training_body(row.name, &role_arn, channels, &row.output),
            )
            .await;
        let job_arn = accepted
            .get("TrainingJobArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let job_resource = format!(":{ACCOUNT}:training-job/{}", row.name);
        assert!(
            job_arn.starts_with("arn:aws:sagemaker:") && job_arn.ends_with(&job_resource),
            "case {case:?}: the acceptance must name the job's ARN in this account, got \
             {job_arn:?}"
        );

        // It is RUN: the clock settles it Completed, with no FailureReason and the artifact the
        // run wrote under the output location the request named.
        let ended = ended_training_job(&emulator, case, row.name).await;
        assert_eq!(
            ended.get("FailureReason"),
            None,
            "case {case:?}: a completed job carries no FailureReason, got {ended}"
        );
        assert_eq!(
            ended.get("SecondaryStatus").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: the secondary status of a finished run, got {ended}"
        );
        let artifact = ended
            .pointer("/ModelArtifacts/S3ModelArtifacts")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the completed job names no artifact: {ended}")
            })
            .to_string();
        assert_eq!(
            artifact,
            artifact_uri(&row.output, row.name),
            "case {case:?}: the artifact lands under the output location the request named"
        );
        assert!(
            artifact.starts_with("s3://") && artifact.len() <= MAX_S3_URI_BYTES,
            "case {case:?}: the artifact URI must be an S3Uri inside the modelled length, got \
             {artifact:?}"
        );

        // And the object is REALLY there: Amazon S3 answers it, as the gzip-compressed archive
        // `ModelArtifacts` documents ("uploaded to your Amazon S3 bucket as compressed files").
        let key = artifact_key(&row.output, row.name);
        let held = emulator
            .object_bytes(BUCKET, &key)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: Amazon S3 holds no object at {artifact:?}"));
        assert!(
            held.len() > 2 && held[0] == 0x1f && held[1] == 0x8b,
            "case {case:?}: the artifact must be the compressed archive a client untars, got {} \
             bytes starting {:?}",
            held.len(),
            held.iter().take(2).collect::<Vec<_>>()
        );

        // The artifact is DEPLOYABLE as reported: a model carrying it, a configuration hosting
        // that model, and an endpoint the clock carries to InService.
        emulator
            .sagemaker_ok(
                "CreateModel",
                &json!({
                    "ModelName": row.name,
                    "PrimaryContainer": {
                        "Image": format!("{ACCOUNT}.dkr.ecr.us-east-1.amazonaws.com/trainer:1"),
                        "ModelDataUrl": artifact,
                    },
                    "ExecutionRoleArn": role_arn,
                }),
            )
            .await;
        let config = format!("{}-config", row.name);
        emulator
            .sagemaker_ok(
                "CreateEndpointConfig",
                &json!({
                    "EndpointConfigName": config,
                    "ProductionVariants": [{
                        "VariantName": VARIANT,
                        "ModelName": row.name,
                        "InstanceType": "ml.m5.large",
                        "InitialInstanceCount": 1,
                    }],
                }),
            )
            .await;
        emulator
            .sagemaker_ok(
                "CreateEndpoint",
                &json!({ "EndpointName": row.name, "EndpointConfigName": config }),
            )
            .await;
        let serving = in_service_endpoint(&emulator, case, row.name).await;
        assert_eq!(
            serving.get("EndpointConfigName").and_then(Value::as_str),
            Some(config.as_str()),
            "case {case:?}: the endpoint reports the configuration it was created from, got \
             {serving}"
        );
        assert_eq!(
            serving
                .pointer("/ProductionVariants/0/VariantName")
                .and_then(Value::as_str),
            Some(VARIANT),
            "case {case:?}: the endpoint hosts the variant its configuration declared, got \
             {serving}"
        );

        // Then the endpoint is GONE: the delete answers the modelled empty output and the reader
        // refuses the name the caller addressed.
        let deleted = emulator
            .sagemaker_ok("DeleteEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        assert_eq!(
            deleted,
            json!({}),
            "case {case:?}: DeleteEndpoint's modelled output has no members, got {deleted}"
        );
        let (status, refused) = emulator
            .sagemaker("DescribeEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: the deleted endpoint must be gone, got {status} with {refused}"
        );
        assert_eq!(
            error_message(&refused),
            format!("Could not find endpoint \"{}\".", row.name),
            "case {case:?}: the refusal names the endpoint the caller asked for"
        );

        // The artifact is the training job's, not the endpoint's: the object the run wrote is
        // where it was, byte for byte, and the job still reports it.
        assert_eq!(
            emulator.object_bytes(BUCKET, &key).await.as_deref(),
            Some(held.as_slice()),
            "case {case:?}: the model artifact must outlive the endpoint that served it"
        );
        let after = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": row.name }))
            .await;
        assert_eq!(
            after.pointer("/ModelArtifacts/S3ModelArtifacts"),
            ended.pointer("/ModelArtifacts/S3ModelArtifacts"),
            "case {case:?}: the job keeps reporting the artifact it wrote, got {after}"
        );
        assert_eq!(
            after.get("TrainingJobStatus").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: a settled job's status does not move, got {after}"
        );
    }
}
