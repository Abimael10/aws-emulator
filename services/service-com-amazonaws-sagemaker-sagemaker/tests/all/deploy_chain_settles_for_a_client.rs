// SPDX-License-Identifier: CC0-1.0

//! WHETHER A CLIENT CAN GET TO THE END OF THE DOCUMENTED HOSTING FLOW: the window one
//! clock-owed lifecycle of this service settles inside, and the train → model → endpoint
//! configuration → endpoint → deployment → delete chain a hosting client drives through several
//! of them.
//!
//! RULE — a status this emulator owes the CLOCK settles inside the compressed band this project
//! gives such a transition, a few seconds past the call that started it, and that holds for the
//! WHOLE walk of a non-terminal status the model documents as an ORDERED SEQUENCE of stages
//! (`DescribeTrainingJob$SecondaryStatus`, `DescribeAutoMLJobV2$AutoMLJobSecondaryStatus`) rather
//! than for each stage of it on its own. A client does not wait out one lifecycle in isolation,
//! it CHAINS them — trains a model, registers the artifact it wrote, configures an endpoint,
//! waits for `InService`, deploys a second configuration and tears the endpoint down — so a walk
//! that alone fills such a client's budget leaves everything that follows it unreachable however
//! right each individual answer is: the endpoint that hosts the model, the new configuration the
//! deployment reports, and the refusal the deleted name answers.
//!
//! The band is a CEILING and not a target, and no span in it is zero: the first read a caller can
//! make after a create still finds the resource short of its terminal status, and a caller that
//! keeps reading observes the stages in the order the model documents them.
//!
//! [`every_clock_owed_job_walk_settles_inside_a_client_budget`]'s rows span what the rule
//! generalises over: the KIND of walk (a training job's, an AutoML job V2's — the two lifecycles
//! of this service whose non-terminal status is a documented sequence of stages) and how many
//! stages the walk has (a job with two channels of data to bring down, one with a single channel,
//! and one with none at all, which has no stage that downloads).
//! [`the_documented_hosting_chain_settles_for_a_client`] then drives the chain itself, with the
//! deployment leg the control plane's report is read back through, over rows that differ in how
//! many channels the training job declares and in what the second configuration deploys — a
//! variant of another name, and a fleet of another size.
//!
//! Every claim about a span is weighed against the instants the RESOURCE ITSELF reports — its own
//! stored schedule, which no host's speed changes — and every wait is a poll for the CONDITION
//! under a deadline far longer than any span this state gives a lifecycle
//! ([`SETTLE_DEADLINE`]), so nothing here is decided by how fast this host ran.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, through the real wire, on the harness [`crate::training_job_execution`] builds rather
//! than a second copy of it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{
    artifact_uri, channel, csv_records, error_code, read_and_write, str_field, training_body,
    Emulator, BUCKET,
};

/// The account the emulator mints its ARNs under, which the container images below name.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The longest ONE whole clock-owed walk may be in flight for, as the resource's own reported
/// instants put it.
///
/// The band this project gives a transition the clock owes, stated here as the ceiling the rule
/// fixes rather than read from the constant under test — a span read from that constant would
/// hold whatever the constant said. The chain below waits out FIVE such walks one after another
/// inside one client budget, and a hosting client's budget for a flow like it is about a minute,
/// so no single walk may take more than a small part of one; the figure leaves generous room
/// besides for the work a create performs in process before its walk is laid out.
const WALK_WINDOW: Duration = Duration::from_secs(10);

/// How long a poll keeps looking before the suite calls the emulator broken, and how often it
/// looks. Far longer than any span this state gives a lifecycle, so it bounds one that never
/// settles rather than the condition a row waits for.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
/// See [`SETTLE_DEADLINE`].
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// The `SecondaryStatus` members a training job's work walks, in the order
/// `DescribeTrainingJob$SecondaryStatus`'s own doc lists them
/// (smithy_query query=doc shape=DescribeTrainingJobResponse member=SecondaryStatus), with the
/// member that goes with the terminal status of a run that succeeded.
const TRAINING_STAGE_ORDER: [&str; 5] = [
    "Starting",
    "Downloading",
    "Training",
    "Uploading",
    "Completed",
];

/// The `AutoMLJobSecondaryStatus` members an AutoML job V2's work walks, in the order that shape
/// lists them (smithy_query query=shape shape=AutoMLJobSecondaryStatus), with the member that
/// goes with the terminal status of a run that succeeded.
const AUTO_ML_STAGE_ORDER: [&str; 6] = [
    "Starting",
    "AnalyzingData",
    "FeatureEngineering",
    "ModelTuning",
    "CandidateDefinitionsGenerated",
    "Completed",
];

/// The status a job of either kind is accepted in (`TrainingJobStatus`, `AutoMLJobStatus`).
const IN_PROGRESS: &str = "InProgress";

/// The members of those two enums nothing leaves on its own, so a poll that reaches one is over.
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// The `EndpointStatus` members a created endpoint walks
/// (smithy_query query=shape shape=EndpointStatus).
const ENDPOINT_CREATING: &str = "Creating";
/// See [`ENDPOINT_CREATING`]: the status a deployment puts the endpoint in.
const ENDPOINT_UPDATING: &str = "Updating";
/// See [`ENDPOINT_CREATING`]: the status an endpoint serves inference in.
const ENDPOINT_IN_SERVICE: &str = "InService";

/// Which lifecycle a row of the walk table drives.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// `CreateTrainingJob` → `DescribeTrainingJob`.
    TrainingJob,
    /// `CreateAutoMLJobV2` → `DescribeAutoMLJobV2`.
    AutoMlJob,
}

/// One row of the walk table: the lifecycle it drives and the data its run reads.
struct WalkRow {
    case: &'static str,
    /// The name the row's job is created under.
    name: &'static str,
    kind: Kind,
    /// The channels the row stages data for; how many stages the walk has follows from this.
    channels: &'static [&'static str],
    records: usize,
    features: usize,
}

/// The table of the RULE: whatever the walk is and however many stages it has, the whole of it
/// is in flight for no longer than one lifecycle's share of a client's budget.
fn walk_rows() -> Vec<WalkRow> {
    vec![
        WalkRow {
            case: "a training job with two channels to bring down",
            name: "walkwindowtwochannels",
            kind: Kind::TrainingJob,
            channels: &["train", "validation"],
            records: 40,
            features: 3,
        },
        WalkRow {
            case: "a training job with one channel",
            name: "walkwindowonechannel",
            kind: Kind::TrainingJob,
            channels: &["train"],
            records: 24,
            features: 2,
        },
        WalkRow {
            case: "a training job with no channel, so its work has no stage that downloads",
            name: "walkwindownochannel",
            kind: Kind::TrainingJob,
            channels: &[],
            records: 0,
            features: 0,
        },
        WalkRow {
            case: "an AutoML job V2, whose work walks the candidate stages",
            name: "walkwindowautoml",
            kind: Kind::AutoMlJob,
            channels: &["train"],
            records: 32,
            features: 2,
        },
    ]
}

/// The key the row's channel data is staged under.
fn channel_key(name: &str, channel_name: &str) -> String {
    format!("{name}/{channel_name}/data.csv")
}

/// The output location the row's run writes under.
fn output_location(name: &str) -> String {
    format!("s3://{BUCKET}/{name}/out/")
}

/// The `CreateTrainingJob` body for a row: one channel per staged prefix, and the member left out
/// altogether for a row with none — `CreateTrainingJobRequest$InputDataConfig` is bounded at one
/// channel or more (smithy_query query=shape shape=InputDataConfig).
fn training_job_body(row: &WalkRow, role_arn: &str) -> Value {
    let channels: Vec<Value> = row
        .channels
        .iter()
        .map(|name| {
            channel(
                name,
                &format!("s3://{BUCKET}/{}", channel_key(row.name, name)),
                "S3Prefix",
            )
        })
        .collect();
    let mut body = training_body(
        row.name,
        role_arn,
        Value::Array(channels.clone()),
        &output_location(row.name),
    );
    if channels.is_empty() {
        body.as_object_mut()
            .expect("the request body is an object")
            .remove("InputDataConfig");
    }
    body
}

/// The `CreateAutoMLJobV2` body for a row: one tabular training channel over the records it
/// staged.
fn auto_ml_body(row: &WalkRow, role_arn: &str) -> Value {
    let channel_name = row
        .channels
        .first()
        .expect("an AutoML row stages one channel");
    json!({
        "AutoMLJobName": row.name,
        "AutoMLJobInputDataConfig": [{
            "ChannelType": "training",
            "ContentType": "text/csv;header=present",
            "DataSource": {
                "S3DataSource": {
                    "S3DataType": "S3Prefix",
                    "S3Uri": format!("s3://{BUCKET}/{}", channel_key(row.name, channel_name)),
                },
            },
        }],
        "OutputDataConfig": { "S3OutputPath": output_location(row.name) },
        "AutoMLProblemTypeConfig": {
            "TabularJobConfig": { "TargetAttributeName": "y", "ProblemType": "Regression" },
        },
        "AutoMLJobObjective": { "MetricName": "MSE" },
        "RoleArn": role_arn,
    })
}

/// Start the row's lifecycle, the way a client does.
async fn start(emulator: &Emulator, row: &WalkRow, role_arn: &str) {
    match row.kind {
        Kind::TrainingJob => {
            emulator
                .sagemaker_ok("CreateTrainingJob", &training_job_body(row, role_arn))
                .await;
        }
        Kind::AutoMlJob => {
            emulator
                .sagemaker_ok("CreateAutoMLJobV2", &auto_ml_body(row, role_arn))
                .await;
        }
    }
}

/// Read the row's record back, the way a polling client does.
async fn describe(emulator: &Emulator, row: &WalkRow) -> Value {
    match row.kind {
        Kind::TrainingJob => {
            emulator
                .sagemaker_ok(
                    "DescribeTrainingJob",
                    &json!({ "TrainingJobName": row.name }),
                )
                .await
        }
        Kind::AutoMlJob => {
            emulator
                .sagemaker_ok("DescribeAutoMLJobV2", &json!({ "AutoMLJobName": row.name }))
                .await
        }
    }
}

/// The primary status a read of the row's record reports.
fn status(kind: Kind, read: &Value) -> String {
    match kind {
        Kind::TrainingJob => str_field(read, "TrainingJobStatus"),
        Kind::AutoMlJob => str_field(read, "AutoMLJobStatus"),
    }
}

/// The stage of the walk a read of the row's record reports.
fn stage(kind: Kind, read: &Value) -> String {
    match kind {
        Kind::TrainingJob => str_field(read, "SecondaryStatus"),
        Kind::AutoMlJob => str_field(read, "AutoMLJobSecondaryStatus"),
    }
}

/// The member the record reports the instant its work ENDED in.
fn end_member(kind: Kind) -> &'static str {
    match kind {
        Kind::TrainingJob => "TrainingEndTime",
        Kind::AutoMlJob => "EndTime",
    }
}

/// The stages the walk is documented to pass through, in that order.
fn stage_order(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::TrainingJob => &TRAINING_STAGE_ORDER,
        Kind::AutoMlJob => &AUTO_ML_STAGE_ORDER,
    }
}

/// One timestamp a response reports, as the epoch seconds the wire carries it in.
fn seconds(read: &Value, member: &str, case: &str) -> f64 {
    read.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {read}"))
}

/// The `Message` an error body carries.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether `observed` is the stages of `order` in that order: every stage observed is one the
/// documented sequence names, and no later read reported an earlier stage of it.
fn walks_in_order(observed: &[String], order: &[&str]) -> bool {
    let mut reached = 0;
    for seen in observed {
        match order.iter().position(|stage| stage == seen) {
            Some(index) if index >= reached => reached = index,
            _ => return false,
        }
    }
    true
}

/// Poll the row's record until its status is one nothing leaves on its own, answering that read
/// and the stages observed on the way.
async fn poll_until_terminal(
    emulator: &Emulator,
    row: &WalkRow,
    first: &Value,
) -> (Value, Vec<String>) {
    let case = row.case;
    let mut observed = vec![stage(row.kind, first)];
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = describe(emulator, row).await;
        let seen = stage(row.kind, &read);
        if observed.last() != Some(&seen) {
            observed.push(seen);
        }
        if TERMINAL.contains(&status(row.kind, &read).as_str()) {
            return (read, observed);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the lifecycle must reach a terminal status, last read {read}, having \
             observed {observed:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// RULE: the WHOLE clock-owed walk of a job's work is in flight for no longer than one
/// lifecycle's share of a client's budget, with its stages observable in the documented order and
/// its non-terminal status observable at all.
#[tokio::test]
async fn every_clock_owed_job_walk_settles_inside_a_client_budget() {
    for row in walk_rows() {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("walk-window-role", Some(&read_and_write()))
            .await;
        for name in row.channels {
            emulator
                .given_object(
                    BUCKET,
                    &channel_key(row.name, name),
                    &csv_records(row.records, row.features),
                )
                .await;
        }

        start(&emulator, &row, &role_arn).await;

        // No span of a walk is zero: the FIRST read a caller can make, with the create's answer
        // in hand, still finds the job short of its terminal status.
        let first = describe(&emulator, &row).await;
        assert_eq!(
            status(row.kind, &first),
            IN_PROGRESS,
            "case {case:?}: the first read after the create must find the job {IN_PROGRESS}, \
             got {first}"
        );

        let (settled, observed) = poll_until_terminal(&emulator, &row, &first).await;
        assert_eq!(
            status(row.kind, &settled),
            "Completed",
            "case {case:?}: a job whose work succeeded ends Completed, got {settled}"
        );
        assert!(
            walks_in_order(&observed, stage_order(row.kind)),
            "case {case:?}: the stages observed must be stages of {:?} in that order, got \
             {observed:?}",
            stage_order(row.kind)
        );

        // And the RULE itself, weighed against the instants the JOB reports rather than against
        // how fast this host polled it.
        let created = seconds(&settled, "CreationTime", case);
        let ended = seconds(&settled, end_member(row.kind), case);
        let walk = ended - created;
        assert!(
            walk > 0.0 && walk <= WALK_WINDOW.as_secs_f64(),
            "case {case:?}: the whole walk must be in flight for a span inside {WALK_WINDOW:?} \
             and not for none of one, got {walk} seconds in {settled}"
        );
    }
}

/// One row of the chain table.
struct ChainRow {
    case: &'static str,
    /// The name the job, the model and the endpoint of the row are created under.
    name: &'static str,
    channels: &'static [&'static str],
    records: usize,
    features: usize,
    /// The variant the SECOND configuration hosts the model on, and the fleet it asks for: what
    /// the endpoint must report once the deployment is owed complete.
    deployed_variant: &'static str,
    deployed_instances: i64,
}

/// The table of the chain: the flow has to get to its end whatever the job's data is and whatever
/// the deployment changes.
fn chain_rows() -> Vec<ChainRow> {
    vec![
        ChainRow {
            case: "one channel, a deployment that widens the fleet",
            name: "hostingchainsingle",
            channels: &["train"],
            records: 24,
            features: 3,
            deployed_variant: "AllTraffic",
            deployed_instances: 2,
        },
        ChainRow {
            case: "two channels, a deployment that renames the variant",
            name: "hostingchaintwochannels",
            channels: &["train", "validation"],
            records: 40,
            features: 2,
            deployed_variant: "Blue",
            deployed_instances: 1,
        },
    ]
}

/// The variant the FIRST configuration of every row hosts the model on.
const SERVING_VARIANT: &str = "AllTraffic";

/// An `EndpointConfig` body hosting `model` on one variant of its own.
fn config_body(config: &str, model: &str, variant: &str, instances: i64) -> Value {
    json!({
        "EndpointConfigName": config,
        "ProductionVariants": [{
            "VariantName": variant,
            "ModelName": model,
            "InstanceType": "ml.m5.large",
            "InitialInstanceCount": instances,
        }],
    })
}

/// Poll `DescribeEndpoint` until the endpoint reports `InService`, answering that read.
async fn in_service(emulator: &Emulator, case: &str, endpoint: &str) -> Value {
    let body = json!({ "EndpointName": endpoint });
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator.sagemaker_ok("DescribeEndpoint", &body).await;
        if str_field(&read, "EndpointStatus") == ENDPOINT_IN_SERVICE {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint must reach {ENDPOINT_IN_SERVICE}, last read {read}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// RULE, driven as the chain a hosting client runs: train, register the artifact, configure an
/// endpoint, wait for `InService`, deploy a second configuration and delete the endpoint — every
/// leg's clock-owed walk inside the band, so the whole chain has an outcome for the client that
/// drives it.
#[tokio::test]
async fn the_documented_hosting_chain_settles_for_a_client() {
    for row in chain_rows() {
        let case = row.case;
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let role_arn = emulator
            .given_role("hosting-chain-role", Some(&read_and_write()))
            .await;

        // Train: the job is accepted non-terminal and the clock settles it inside the band.
        let channels: Vec<Value> = row
            .channels
            .iter()
            .map(|name| {
                channel(
                    name,
                    &format!("s3://{BUCKET}/{}", channel_key(row.name, name)),
                    "S3Prefix",
                )
            })
            .collect();
        for name in row.channels {
            emulator
                .given_object(
                    BUCKET,
                    &channel_key(row.name, name),
                    &csv_records(row.records, row.features),
                )
                .await;
        }
        let output = output_location(row.name);
        emulator
            .sagemaker_ok(
                "CreateTrainingJob",
                &training_body(row.name, &role_arn, Value::Array(channels), &output),
            )
            .await;
        let job_row = WalkRow {
            case,
            name: row.name,
            kind: Kind::TrainingJob,
            channels: row.channels,
            records: row.records,
            features: row.features,
        };
        let first = describe(&emulator, &job_row).await;
        assert_eq!(
            status(Kind::TrainingJob, &first),
            IN_PROGRESS,
            "case {case:?}: the first read after the create must find the job {IN_PROGRESS}, \
             got {first}"
        );
        let (trained, _) = poll_until_terminal(&emulator, &job_row, &first).await;
        assert_eq!(
            status(Kind::TrainingJob, &trained),
            "Completed",
            "case {case:?}: the job the chain depends on must complete, got {trained}"
        );
        let artifact = trained
            .pointer("/ModelArtifacts/S3ModelArtifacts")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the completed job names no artifact in \
                 {trained}"))
            .to_string();
        assert_eq!(
            artifact,
            artifact_uri(&output, row.name),
            "case {case:?}: the artifact lands under the output location the request named"
        );
        let job_walk =
            seconds(&trained, "TrainingEndTime", case) - seconds(&trained, "CreationTime", case);
        assert!(
            job_walk > 0.0 && job_walk <= WALK_WINDOW.as_secs_f64(),
            "case {case:?}: the job's whole walk must be in flight for a span inside \
             {WALK_WINDOW:?} and not for none of one, got {job_walk} seconds in {trained}"
        );

        // Register: the model carries the artifact the job wrote, and reads back as registered.
        let image = format!("{ACCOUNT}.dkr.ecr.us-east-1.amazonaws.com/trainer:1");
        emulator
            .sagemaker_ok(
                "CreateModel",
                &json!({
                    "ModelName": row.name,
                    "PrimaryContainer": { "Image": image, "ModelDataUrl": artifact },
                    "ExecutionRoleArn": role_arn,
                }),
            )
            .await;
        let model = emulator
            .sagemaker_ok("DescribeModel", &json!({ "ModelName": row.name }))
            .await;
        assert_eq!(
            model
                .pointer("/PrimaryContainer/ModelDataUrl")
                .and_then(Value::as_str),
            Some(artifact.as_str()),
            "case {case:?}: the registered model carries the artifact the job wrote, got {model}"
        );

        // Host: two configurations, the endpoint created from the first.
        let serving_config = format!("{}-serving", row.name);
        let deployed_config = format!("{}-deployed", row.name);
        emulator
            .sagemaker_ok(
                "CreateEndpointConfig",
                &config_body(&serving_config, row.name, SERVING_VARIANT, 1),
            )
            .await;
        emulator
            .sagemaker_ok(
                "CreateEndpointConfig",
                &config_body(
                    &deployed_config,
                    row.name,
                    row.deployed_variant,
                    row.deployed_instances,
                ),
            )
            .await;
        emulator
            .sagemaker_ok(
                "CreateEndpoint",
                &json!({ "EndpointName": row.name, "EndpointConfigName": serving_config }),
            )
            .await;
        let creating = emulator
            .sagemaker_ok("DescribeEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        assert_eq!(
            str_field(&creating, "EndpointStatus"),
            ENDPOINT_CREATING,
            "case {case:?}: the first read after the create must find the endpoint \
             {ENDPOINT_CREATING}, got {creating}"
        );
        let serving = in_service(&emulator, case, row.name).await;
        assert_eq!(
            str_field(&serving, "EndpointConfigName"),
            serving_config,
            "case {case:?}: the endpoint reports the configuration it was created from, got \
             {serving}"
        );
        assert_eq!(
            serving
                .pointer("/ProductionVariants/0/VariantName")
                .and_then(Value::as_str),
            Some(SERVING_VARIANT),
            "case {case:?}: the endpoint hosts the variant its configuration declared, got \
             {serving}"
        );
        let creation_walk =
            seconds(&serving, "LastModifiedTime", case) - seconds(&serving, "CreationTime", case);
        assert!(
            creation_walk > 0.0 && creation_walk <= WALK_WINDOW.as_secs_f64(),
            "case {case:?}: the endpoint's walk to {ENDPOINT_IN_SERVICE} must be in flight for a \
             span inside {WALK_WINDOW:?} and not for none of one, got {creation_walk} seconds in \
             {serving}"
        );

        // Deploy: the second configuration, with the endpoint reported `Updating` until the clock
        // owes the deployment complete and reporting the deployed configuration from then on.
        emulator
            .sagemaker_ok(
                "UpdateEndpoint",
                &json!({
                    "EndpointName": row.name,
                    "EndpointConfigName": deployed_config,
                }),
            )
            .await;
        let updating = emulator
            .sagemaker_ok("DescribeEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        assert_eq!(
            str_field(&updating, "EndpointStatus"),
            ENDPOINT_UPDATING,
            "case {case:?}: the first read after the deployment must find the endpoint \
             {ENDPOINT_UPDATING}, got {updating}"
        );
        let deployed = in_service(&emulator, case, row.name).await;
        assert_eq!(
            str_field(&deployed, "EndpointConfigName"),
            deployed_config,
            "case {case:?}: the deployed endpoint reports the configuration it was given, got \
             {deployed}"
        );
        assert_eq!(
            (
                deployed
                    .pointer("/ProductionVariants/0/VariantName")
                    .and_then(Value::as_str),
                deployed
                    .pointer("/ProductionVariants/0/CurrentInstanceCount")
                    .and_then(Value::as_i64),
            ),
            (Some(row.deployed_variant), Some(row.deployed_instances)),
            "case {case:?}: the deployed endpoint reports the fleet the second configuration \
             declared, got {deployed}"
        );
        let deployment_walk = seconds(&deployed, "LastModifiedTime", case)
            - seconds(&updating, "LastModifiedTime", case);
        assert!(
            deployment_walk > 0.0 && deployment_walk <= WALK_WINDOW.as_secs_f64(),
            "case {case:?}: the deployment's walk back to {ENDPOINT_IN_SERVICE} must be in \
             flight for a span inside {WALK_WINDOW:?} and not for none of one, got \
             {deployment_walk} seconds in {deployed}"
        );

        // And down: the endpoint is gone, and the reader refuses the name the caller addressed.
        emulator
            .sagemaker_ok("DeleteEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        let (refusal_status, refused) = emulator
            .sagemaker("DescribeEndpoint", &json!({ "EndpointName": row.name }))
            .await;
        assert_eq!(
            (refusal_status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: the deleted endpoint must be gone, got {refusal_status} with {refused}"
        );
        assert_eq!(
            error_message(&refused),
            format!("Could not find endpoint \"{}\".", row.name),
            "case {case:?}: the refusal names the endpoint the caller asked for, got {refused}"
        );
    }
}
