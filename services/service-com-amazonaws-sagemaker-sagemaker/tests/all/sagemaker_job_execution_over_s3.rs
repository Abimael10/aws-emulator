// SPDX-License-Identifier: CC0-1.0

//! What a SageMaker JOB really does with the Amazon S3 data it was given — driven end to end
//! over the real wire path (SageMaker, Amazon S3 and IAM on ONE shared engine, the way the
//! production router mounts every service), so what is asserted is what a client sees.
//!
//! The rules under test:
//!
//! * RULE ONE — `CreateTrainingJob`'s refusal of an input data source it found no object under
//!   carries a SECOND sentence, naming what Amazon S3 answered, exactly when the listing was
//!   refused by AUTHORIZATION rather than merely coming back empty; so a client can tell "the
//!   prefix holds nothing" from "this role may not list the bucket".
//! * RULE TWO — the read of an object a job makes is authorized against BOTH the policy on the
//!   object's bucket and the grant in force on the execution role, so a job whose input bucket
//!   explicitly denies that role ends in the modelled `Failed` status with a `FailureReason`
//!   naming the Amazon S3 access failure — never `Completed` over data it never read.
//! * RULE THREE — the model artifact a finished job reports holds what the job's CONTAINER
//!   wrote to `/opt/ml/model`: the files a declared program left there, and otherwise the
//!   model the default train instructions fitted, under the file name the ALGORITHM's container
//!   writes its model as.
//! * RULE FOUR — `FinalMetricDataList` is named per input channel and per the metric the
//!   algorithm's learning task is evaluated by, with every value measured over that channel's
//!   own records.
//! * RULE FIVE — a job created with `CheckpointConfig` has at least one object under that
//!   prefix once it reports `Completed`, with no object appearing before the job entered the
//!   stage that writes it.
//! * RULE SIX — a batch transform job RUNS: it scores every record of every input object with
//!   the model it names and writes one `<object>.out` per input object under
//!   `TransformOutput.S3OutputPath`, then reports `Completed`; one whose role may not read the
//!   input reports `Failed` with the access failure instead.
//!
//! The tables span what each rule generalises over: why a listing answered nothing, which
//! algorithm the job names, which objective it asked for, whether a container program wrote
//! the model directory, how many input objects and records a transform job is given, and which
//! leg of a job's work authorization stops.
//!
//! Every wait here is a poll for the CONDITION, never a pause of a fixed length, and that
//! holds for the conditions a case reaches THROUGH a status as well: the object a finished job
//! left is looked for again until Amazon S3 serves it ([`object_bytes`], [`keys_until`])
//! rather than taken as an instant consequence of the status just read, so a case is decided
//! by what the service answered and never by how fast the host ran. The one deadline a wait is
//! bounded by ([`look_again`]) is far longer than any span this state gives a job's work, so
//! it bounds a BROKEN emulator and not the condition.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::model_artifact_admission::{
    error_code, message, role_arn, str_field, trust_for, Emulator, ACCOUNT, REGION,
};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// How long a job is polled for the status it has to reach before a case fails, and how often.
/// What the poll waits for is the CONDITION, re-read in a loop, so a loaded host cannot fail a
/// case the clock would have served.
pub(crate) const SETTLE_BUDGET: Duration = Duration::from_secs(120);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// Fail `case` unless there is still time to look again for `what`, then wait one poll
/// interval before looking again.
///
/// The ONE place this suite bounds a wait. Every wait here is a poll for the CONDITION, so a
/// loaded host takes more turns of this loop rather than reaching a different answer, and the
/// deadline bounds a BROKEN emulator rather than the condition: it is far longer than any span
/// this state gives a job's work. `last` is what the final look saw, so a case that really
/// fails says what the service answered.
pub(crate) async fn look_again(deadline: Instant, case: &str, what: &str, last: &str) {
    assert!(
        Instant::now() < deadline,
        "case {case:?}: {what} did not hold within {SETTLE_BUDGET:?}; last saw {last}"
    );
    tokio::time::sleep(SETTLE_POLL).await;
}

/// The role every job below runs as.
pub(crate) const ROLE: &str = "job-execution-role";

/// An ECR-shaped path for SageMaker's built-in gradient-boosted tree algorithm, whose
/// repository is `sagemaker-xgboost` ("Docker registry paths and example code").
fn boosted_trees_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/sagemaker-xgboost:1.7-1")
}

/// An ECR-shaped path for a container of the caller's own.
pub(crate) fn own_image() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/my-trainer:1")
}

/// A policy document allowing `actions` on `resources`.
pub(crate) fn allow(actions: Value, resources: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resources }],
    })
}

/// Hold the job role in IAM with the Amazon S3 grant `grant`, and answer its ARN.
pub(crate) async fn given_job_role(emulator: &Emulator, grant: &Value) -> String {
    let arn = emulator.given_role(ROLE, &trust_for(SERVICE_PRINCIPAL)).await;
    emulator.given_role_policy(ROLE, grant).await;
    arn
}

/// The grant a job needs over `bucket` to read its input, write its output and write its
/// checkpoints.
pub(crate) fn every_s3_action_on(bucket: &str) -> Value {
    allow(
        json!(["s3:GetObject", "s3:PutObject", "s3:ListBucket"]),
        json!([
            format!("arn:aws:s3:::{bucket}"),
            format!("arn:aws:s3:::{bucket}/*")
        ]),
    )
}

/// Comma-separated training records: `rows` records whose first column is the target and whose
/// remaining `features` columns are the features, the target a linear function of them.
fn training_data(rows: usize, features: usize) -> String {
    (0..rows)
        .map(|row| {
            let values: Vec<f64> = (0..features).map(|column| (row + column + 1) as f64).collect();
            let target: f64 = 3.0 + values.iter().sum::<f64>();
            let mut record = vec![format!("{target}")];
            record.extend(values.iter().map(|value| format!("{value}")));
            format!("{}\n", record.join(","))
        })
        .collect()
}

/// Comma-separated records whose target is 0 or 1, which is the shape a binary learning task
/// is evaluated over.
fn labelled_data(rows: usize) -> String {
    (0..rows)
        .map(|row| format!("{},{}\n", row % 2, row))
        .collect()
}

/// The keys Amazon S3 lists under `prefix`, read through `ListObjectsV2`.
pub(crate) async fn listed_keys(emulator: &Emulator, bucket: &str, prefix: &str) -> Vec<String> {
    let (status, body) = emulator
        .s3(
            "GET",
            &format!("/{bucket}?list-type=2&prefix={prefix}"),
            &[],
        )
        .await;
    assert_eq!(
        status,
        200,
        "the listing of {bucket}/{prefix} must be served, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8_lossy(&body).to_string();
    text.split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// The bytes of one object, read through `GetObject` once Amazon S3 serves it.
///
/// What a case needs of a job that has finished is the OBJECT, so it looks again for THAT
/// condition instead of taking a status that implies it as proof the object is already there.
/// Whether it is, the first time a read reports the terminal status, is an ordering the
/// emulator keeps (`state::publish_due_artifact` makes the publication the clock owes before
/// the read it answers reports the stage); a case that ASSUMES that ordering rather than
/// looking again turns any lag into a failure decided by how fast the host ran.
pub(crate) async fn object_bytes(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    key: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let (status, body) = emulator.s3("GET", &format!("/{bucket}/{key}"), &[]).await;
        if status == 200 {
            return body;
        }
        look_again(
            deadline,
            case,
            &format!("Amazon S3 serves the object {bucket}/{key}"),
            &format!("{status} {}", String::from_utf8_lossy(&body)),
        )
        .await;
    }
}

/// Look again until the keys Amazon S3 lists under `prefix` are exactly `expected`, and answer
/// what the listing that saw them held.
///
/// The set of objects a finished job left is a CONDITION, read the same way as every other one
/// in this suite: `expected` is still asserted exactly by the caller, and what a loaded host
/// changes is how many times this looks, never what it answers.
pub(crate) async fn keys_until(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    prefix: &str,
    expected: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let mut held = listed_keys(emulator, bucket, prefix).await;
        held.sort();
        if held == expected {
            return held;
        }
        look_again(
            deadline,
            case,
            &format!("{bucket}/{prefix} holds exactly {expected:?}"),
            &format!("{held:?}"),
        )
        .await;
    }
}

/// The entries a `model.tar.gz` holds, as `(name, contents)`.
fn archive_entries(bytes: &[u8]) -> Vec<(String, String)> {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut read = Vec::new();
    for entry in archive.entries().expect("the artifact is a tar archive") {
        let mut entry = entry.expect("an archive entry");
        let name = entry
            .path()
            .expect("the entry names a path")
            .to_string_lossy()
            .to_string();
        let mut contents = String::new();
        entry
            .read_to_string(&mut contents)
            .expect("the entry's bytes are text");
        read.push((name, contents));
    }
    read
}

/// A `CreateTrainingJob` request over `bucket`, with `channels` as its input data config.
#[allow(clippy::too_many_arguments)]
fn training_request(
    name: &str,
    role: &str,
    bucket: &str,
    image: &str,
    channels: Value,
    hyper_parameters: Value,
    extra: Value,
) -> Value {
    let mut body = json!({
        "TrainingJobName": name,
        "AlgorithmSpecification": { "TrainingImage": image, "TrainingInputMode": "File" },
        "RoleArn": role,
        "InputDataConfig": channels,
        "HyperParameters": hyper_parameters,
        "OutputDataConfig": { "S3OutputPath": format!("s3://{bucket}/out/") },
        "ResourceConfig": {
            "InstanceType": "ml.m5.large",
            "InstanceCount": 1,
            "VolumeSizeInGB": 10,
        },
    });
    for (member, value) in extra.as_object().expect("the extra members are an object") {
        body[member] = value.clone();
    }
    body
}

/// One channel reading `s3://<bucket>/<prefix>`.
pub(crate) fn channel(name: &str, bucket: &str, prefix: &str) -> Value {
    json!({
        "ChannelName": name,
        "DataSource": {
            "S3DataSource": {
                "S3DataType": "S3Prefix",
                "S3Uri": format!("s3://{bucket}/{prefix}"),
            },
        },
        "ContentType": "text/csv",
    })
}

/// An instant as the epoch seconds the wire carries a timestamp in, so an instant this caller
/// held can be weighed against one a response reports.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .expect("a host clock at or after the epoch")
        .as_secs_f64()
}

/// Poll `DescribeTrainingJob` until the job leaves `InProgress`, answering the read that saw a
/// terminal status together with every `SecondaryStatus` observed on the way and an upper bound
/// on when the artifact became listable under the output prefix.
struct Observed {
    terminal: Value,
    statuses: Vec<String>,
    /// The epoch seconds this caller held just AFTER the listing that first showed the
    /// artifact, and `None` while no listing has shown one.
    ///
    /// It is an UPPER bound on the instant the artifact appeared — the object was already
    /// there when that listing answered — which is what makes a claim about when it may appear
    /// independent of how fast this host ran anything: a host the poller is delayed on can
    /// only push this instant LATER, i.e. loosen every comparison made against it.
    artifact_listable_by: Option<f64>,
}

async fn observed(emulator: &Emulator, case: &str, job: &str, bucket: &str) -> Observed {
    let deadline = Instant::now() + SETTLE_BUDGET;
    let mut statuses: Vec<String> = Vec::new();
    let mut artifact_listable_by: Option<f64> = None;
    loop {
        let read = emulator
            .sagemaker_ok(case, "DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        let secondary = read
            .get("SecondaryStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if statuses.last() != Some(&secondary) {
            statuses.push(secondary.clone());
        }
        if artifact_listable_by.is_none() {
            let published = listed_keys(emulator, bucket, &format!("out/{job}/"))
                .await
                .iter()
                .any(|key| key.ends_with("model.tar.gz"));
            if published {
                artifact_listable_by = Some(epoch_seconds(SystemTime::now()));
            }
        }
        if str_field(&read, "TrainingJobStatus") != "InProgress" {
            return Observed {
                terminal: read,
                statuses,
                artifact_listable_by,
            };
        }
        look_again(
            deadline,
            case,
            "the job left InProgress",
            &read.to_string(),
        )
        .await;
    }
}

/// The `SecondaryStatusTransitions` a read reports: the job's OWN account of the stages its
/// work went through, so a claim weighed against it holds however coarsely a poller looked.
fn history(described: &Value, case: &str) -> Vec<Value> {
    described
        .get("SecondaryStatusTransitions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected SecondaryStatusTransitions in {described}")
        })
        .clone()
}

/// The `SecondaryStatus` values that history names, in the order the job was in them.
fn history_statuses(described: &Value, case: &str) -> Vec<String> {
    history(described, case)
        .iter()
        .map(|entry| str_field(entry, "Status"))
        .collect()
}

/// The instant a read's own history says the job ENTERED `status`, as epoch seconds.
fn history_entered(described: &Value, status: &str, case: &str) -> f64 {
    history(described, case)
        .iter()
        .find(|entry| str_field(entry, "Status") == status)
        .and_then(|entry| entry.get("StartTime").and_then(Value::as_f64))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {status} entry with a StartTime in {described}")
        })
}

/// Whether `observed` appears inside `history` in order, i.e. every status a poller reported is
/// one the job's own history names, and never before a status the poller reported earlier.
///
/// A poller only ever samples the history, so the statuses it reports are a SUBSEQUENCE of it,
/// whatever the host's speed let it catch: the claim holds on an idle host that sees every
/// stage and on a loaded one that skips some.
fn is_subsequence_of(observed: &[String], history: &[String]) -> bool {
    let mut entries = history.iter();
    observed
        .iter()
        .all(|status| entries.any(|entry| entry == status))
}

/// The metric names a describe reports, sorted.
fn metric_names(described: &Value) -> Vec<String> {
    let mut names: Vec<String> = described
        .get("FinalMetricDataList")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics
                .iter()
                .filter_map(|metric| metric.get("MetricName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// RULE ONE: the refusal of an input data source that answered no object carries the Amazon S3
/// authorization clause exactly when the listing was REFUSED, and the location sentence alone
/// when it merely answered nothing.
///
/// The rows differ along WHY the listing answered nothing: a role with no Amazon S3 grant at
/// all, a role granted the listing of another bucket only, a prefix that holds nothing, and a
/// bucket that does not exist.
#[tokio::test]
async fn the_no_s3_objects_refusal_names_what_s3_answered_when_the_listing_was_refused() {
    /// One row: the case, the grant the role carries, the bucket and prefix the channel reads,
    /// and whether the refusal has to carry the Amazon S3 authorization clause.
    struct Row {
        case: &'static str,
        grant: Value,
        bucket: &'static str,
        prefix: &'static str,
        refused_by_s3: bool,
    }
    let rows = vec![
        Row {
            case: "a role with no Amazon S3 grant at all",
            grant: allow(json!("sts:AssumeRole"), json!("*")),
            bucket: "listing-refused",
            prefix: "train/",
            refused_by_s3: true,
        },
        Row {
            case: "a role granted the listing of another bucket only",
            grant: allow(json!("s3:ListBucket"), json!("arn:aws:s3:::some-other-bucket")),
            bucket: "listing-refused",
            prefix: "train/",
            refused_by_s3: true,
        },
        Row {
            case: "a prefix the bucket holds nothing under",
            grant: every_s3_action_on("listing-refused"),
            bucket: "listing-refused",
            prefix: "nothing-here/",
            refused_by_s3: false,
        },
        Row {
            case: "a bucket that does not exist",
            grant: allow(json!("s3:*"), json!("*")),
            bucket: "no-such-input-bucket",
            prefix: "train/",
            refused_by_s3: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &row.grant).await;
        emulator.given_bucket("listing-refused").await;
        emulator
            .given_object("listing-refused", "train/data.csv", &training_data(8, 2))
            .await;

        let name = format!("listingjob{index}");
        let (status, answered) = emulator
            .sagemaker(
                "CreateTrainingJob",
                &training_request(
                    &name,
                    &role,
                    "listing-refused",
                    &own_image(),
                    json!([channel("train", row.bucket, row.prefix)]),
                    json!({}),
                    json!({}),
                ),
            )
            .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a data source that answers no object must be refused, got {status} \
             with {answered}"
        );
        let answered_message = message(&answered);
        let located = format!(
            "No S3 objects found under S3 URL \"s3://{}/{}\" given in input data source. Please \
             ensure that the bucket exists in the selected region ({REGION}), that objects \
             exist under that S3 prefix, and that the role \"{role}\" has \"s3:ListBucket\" \
             permissions on bucket \"{}\".",
            row.bucket, row.prefix, row.bucket
        );
        let clause = format!(
            " Error message from S3: User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/SageMaker \
             is not authorized to perform: s3:ListBucket on resource: \
             \"arn:aws:s3:::{}\" because no identity-based policy allows the s3:ListBucket \
             action",
            row.bucket
        );
        let expected = if row.refused_by_s3 {
            format!("{located}{clause}")
        } else {
            located
        };
        assert_eq!(
            answered_message, expected,
            "case {case:?}: expected {expected:?}, got {answered_message:?}"
        );
        let (read_status, read) = emulator
            .sagemaker("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        assert_ne!(
            read_status, 200,
            "case {case:?}: a refused create must store nothing, got {read_status} with {read}"
        );
    }
}

/// RULE TWO: a job whose input bucket's policy EXPLICITLY DENIES the execution role is
/// ACCEPTED and then ends `Failed`, with a `FailureReason` naming the Amazon S3 access
/// failure — while a policy that merely says nothing about the role leaves the role's own grant
/// standing, which is IAM's rule for a read inside one account.
///
/// The rows differ along what the bucket policy says: an explicit deny of the read for the
/// role, an explicit deny of every Amazon S3 action, a policy that grants the role nothing, and
/// a policy that allows the read.
#[tokio::test]
async fn a_bucket_policy_that_denies_the_role_fails_the_training_job() {
    let bucket = "policy-fenced-input";
    let role = role_arn(ROLE);
    let objects = format!("arn:aws:s3:::{bucket}/*");
    /// One row: the case, the bucket policy, and whether the job has to fail.
    struct Row {
        case: &'static str,
        policy: Value,
        failed: bool,
    }
    let rows = vec![
        Row {
            case: "an explicit deny of the read for the role",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "AWS": role.clone() },
                        "Action": "s3:*",
                        "Resource": objects.clone(),
                    },
                    {
                        "Effect": "Deny",
                        "Principal": { "AWS": role.clone() },
                        "Action": "s3:GetObject",
                        "Resource": objects.clone(),
                    },
                ],
            }),
            failed: true,
        },
        Row {
            case: "an explicit deny of every Amazon S3 action for the role",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "AWS": role.clone() },
                    "Action": "s3:*",
                    "Resource": objects.clone(),
                }],
            }),
            failed: true,
        },
        Row {
            case: "a policy that says nothing about the role",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": role_arn("someone-else") },
                    "Action": "s3:GetObject",
                    "Resource": objects.clone(),
                }],
            }),
            failed: false,
        },
        Row {
            case: "a deny aimed at another principal",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "AWS": role_arn("someone-else") },
                    "Action": "s3:GetObject",
                    "Resource": objects.clone(),
                }],
            }),
            failed: false,
        },
        Row {
            case: "a policy that allows the read",
            policy: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": role.clone() },
                    "Action": "s3:*",
                    "Resource": objects.clone(),
                }],
            }),
            failed: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role_ref = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        // A bucket policy's `Principal.AWS` is resolved against IAM when it is stored, so the
        // role a row names has to be one the account holds.
        emulator
            .given_role("someone-else", &trust_for(SERVICE_PRINCIPAL))
            .await;
        emulator.given_bucket(bucket).await;
        emulator
            .given_object(bucket, "train/data.csv", &training_data(12, 2))
            .await;
        emulator.given_bucket_policy(bucket, &row.policy).await;

        let name = format!("policyjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateTrainingJob",
                &training_request(
                    &name,
                    &role_ref,
                    bucket,
                    &own_image(),
                    json!([channel("train", bucket, "train/")]),
                    json!({}),
                    json!({}),
                ),
            )
            .await;
        let seen = observed(&emulator, case, &name, bucket).await;
        let status = str_field(&seen.terminal, "TrainingJobStatus");
        let reason = seen
            .terminal
            .get("FailureReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if row.failed {
            assert_eq!(
                status, "Failed",
                "case {case:?}: a job the bucket policy fences off must fail, got {status} with \
                 {reason:?}"
            );
            assert!(
                reason.contains("AccessDenied"),
                "case {case:?}: the reason must name the Amazon S3 access failure, got {reason:?}"
            );
        } else {
            assert_eq!(
                status, "Completed",
                "case {case:?}: a policy that allows the read fails nothing, got {status} with \
                 {reason:?}"
            );
        }
    }
}

/// RULE THREE and RULE FOUR: the artifact holds what the job's container wrote to
/// `/opt/ml/model`, and the final metrics are named per channel and per the metric the
/// algorithm's learning task is evaluated by.
///
/// The rows differ along the algorithm (the built-in boosted-trees image, a container of the
/// caller's own), the objective (binary, regression, none), the number of channels, and
/// whether a declared container program wrote the model directory.
#[tokio::test]
async fn the_artifact_and_the_metrics_come_from_the_job_that_produced_them() {
    let bucket = "job-outputs";
    /// One row: the case, the image, the hyperparameters, the channels, the extra members, the
    /// archive entries the artifact has to hold and the metric names the describe has to
    /// report.
    struct Row {
        case: &'static str,
        image: String,
        hyper_parameters: Value,
        channels: Vec<&'static str>,
        extra: Value,
        entries: Vec<&'static str>,
        metrics: Vec<&'static str>,
    }
    let rows = vec![
        Row {
            case: "the built-in algorithm with a binary objective over two channels",
            image: boosted_trees_image(),
            hyper_parameters: json!({ "num_round": "10", "objective": "binary:logistic" }),
            channels: vec!["train", "validation"],
            extra: json!({}),
            entries: vec!["xgboost-model"],
            metrics: vec!["train:error", "validation:error"],
        },
        Row {
            case: "the built-in algorithm with a regression objective",
            image: boosted_trees_image(),
            hyper_parameters: json!({ "objective": "reg:squarederror" }),
            channels: vec!["train"],
            extra: json!({}),
            entries: vec!["xgboost-model"],
            metrics: vec!["train:rmse"],
        },
        Row {
            case: "a container of the caller's own, with no objective",
            image: own_image(),
            hyper_parameters: json!({}),
            channels: vec!["train"],
            extra: json!({}),
            entries: vec!["model.json"],
            metrics: vec!["train:rmse"],
        },
        Row {
            case: "a declared program that writes the model directory itself",
            image: own_image(),
            hyper_parameters: json!({}),
            channels: vec!["train"],
            extra: json!({
                "AlgorithmSpecification": {
                    "TrainingImage": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/my-trainer:1"),
                    "TrainingInputMode": "File",
                    "ContainerEntrypoint": [
                        "/bin/sh",
                        "-c",
                        "mkdir -p /opt/ml/model && echo trained-here > /opt/ml/model/artifact.txt",
                    ],
                },
            }),
            entries: vec!["artifact.txt"],
            metrics: vec!["train:rmse"],
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        for name in &row.channels {
            emulator
                .given_object(bucket, &format!("{name}/data.csv"), &labelled_data(16))
                .await;
        }
        let channels: Vec<Value> = row
            .channels
            .iter()
            .map(|name| channel(name, bucket, &format!("{name}/")))
            .collect();

        let job = format!("artifactjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateTrainingJob",
                &training_request(
                    &job,
                    &role,
                    bucket,
                    &row.image,
                    json!(channels),
                    row.hyper_parameters.clone(),
                    row.extra.clone(),
                ),
            )
            .await;
        let seen = observed(&emulator, case, &job, bucket).await;
        assert_eq!(
            str_field(&seen.terminal, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: the job must complete, got {}",
            seen.terminal
        );

        let names = metric_names(&seen.terminal);
        assert_eq!(
            names, row.metrics,
            "case {case:?}: expected the metrics {:?}, got {names:?}",
            row.metrics
        );
        for metric in seen
            .terminal
            .get("FinalMetricDataList")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let value = metric.get("Value").and_then(Value::as_f64);
            assert!(
                value.is_some_and(|value| value.is_finite() && value >= 0.0),
                "case {case:?}: every metric carries a measured value, got {metric}"
            );
        }

        let artifact = str_field(
            seen.terminal
                .get("ModelArtifacts")
                .unwrap_or(&Value::Null),
            "S3ModelArtifacts",
        );
        let key = artifact
            .strip_prefix(&format!("s3://{bucket}/"))
            .unwrap_or_else(|| panic!("case {case:?}: the artifact URI names this bucket: {artifact}"))
            .to_string();
        let entries = archive_entries(&object_bytes(&emulator, case, bucket, &key).await);
        let mut held: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        held.sort_unstable();
        assert_eq!(
            held, row.entries,
            "case {case:?}: expected the archive to hold {:?}, got {held:?}",
            row.entries
        );
        if let Some((_, contents)) = entries.iter().find(|(name, _)| name == "artifact.txt") {
            assert_eq!(
                contents.trim(),
                "trained-here",
                "case {case:?}: the entry holds what the program wrote, got {contents:?}"
            );
        }

        // RULE FIVE's ordering half: the artifact must never be listable before the job
        // reported the stage that uploads it, which is over once the work ends. It is weighed
        // against the job's OWN account of when it entered that stage and against the instant
        // this caller held the artifact already listable at — never against two separate
        // observations paired by how fast this host made them, which a host delayed between
        // them could pair wrongly.
        let entered_uploading = history_entered(&seen.terminal, "Uploading", case);
        let listable_by = seen.artifact_listable_by.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the artifact must be listable under the output prefix by the \
                 time the job is {}, got {}",
                str_field(&seen.terminal, "TrainingJobStatus"),
                seen.terminal
            )
        });
        assert!(
            listable_by >= entered_uploading,
            "case {case:?}: the artifact must appear no earlier than the stage that uploads it, \
             which the job entered at {entered_uploading}, but it was already listable by \
             {listable_by} over {:?}",
            seen.statuses
        );
        // The stages the job went through are read from the history it reports, which names
        // every stage the clock put it in whether or not a poller happened to look inside that
        // stage's window; what the poller did report is held to that same history.
        let history = history_statuses(&seen.terminal, case);
        for stage in ["Starting", "Downloading", "Training", "Uploading"] {
            assert!(
                history.iter().any(|entry| entry == stage),
                "case {case:?}: the job must pass through {stage}, got {history:?} over {:?}",
                seen.statuses
            );
        }
        assert!(
            is_subsequence_of(&seen.statuses, &history),
            "case {case:?}: every status the poller reported must be one the job's history \
             names, and in that order, got {:?} over {history:?}",
            seen.statuses
        );
    }
}

/// RULE FIVE: a job created with `CheckpointConfig` has at least one object under that prefix
/// once it completes, written inside its own run.
///
/// The rows differ along the checkpoint prefix's shape and along whether the job's own program
/// wrote the checkpoint directory.
#[tokio::test]
async fn a_job_with_a_checkpoint_config_leaves_an_object_under_its_prefix() {
    let bucket = "job-checkpoints";
    // (case, the checkpoint config, the extra members, the names the prefix has to hold)
    let rows: Vec<(&str, Value, Value, Vec<&str>)> = vec![
        (
            "the default train instructions",
            json!({
                "S3Uri": format!("s3://{bucket}/checkpoints/"),
                "LocalPath": "/opt/ml/checkpoints",
            }),
            json!({}),
            vec!["checkpoints/model.json"],
        ),
        (
            "a prefix with no trailing delimiter",
            json!({ "S3Uri": format!("s3://{bucket}/saved") }),
            json!({}),
            vec!["saved/model.json"],
        ),
        (
            "a program that writes the checkpoint directory itself",
            json!({
                "S3Uri": format!("s3://{bucket}/checkpoints/"),
                "LocalPath": "/opt/ml/checkpoints",
            }),
            json!({
                "AlgorithmSpecification": {
                    "TrainingImage": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/my-trainer:1"),
                    "TrainingInputMode": "File",
                    "ContainerEntrypoint": [
                        "/bin/sh",
                        "-c",
                        "mkdir -p /opt/ml/model /opt/ml/checkpoints && \
                         echo done > /opt/ml/model/artifact.txt && \
                         echo step-3 > /opt/ml/checkpoints/state.txt",
                    ],
                },
            }),
            vec!["checkpoints/state.txt"],
        ),
    ];

    for (index, (case, checkpoints, extra, expected)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        emulator
            .given_object(bucket, "train/data.csv", &training_data(12, 2))
            .await;

        let job = format!("checkpointjob{index}");
        let mut body = training_request(
            &job,
            &role,
            bucket,
            &own_image(),
            json!([channel("train", bucket, "train/")]),
            json!({}),
            extra,
        );
        body["CheckpointConfig"] = checkpoints;
        emulator
            .sagemaker_ok(case, "CreateTrainingJob", &body)
            .await;
        let seen = observed(&emulator, case, &job, bucket).await;
        assert_eq!(
            str_field(&seen.terminal, "TrainingJobStatus"),
            "Completed",
            "case {case:?}: the job must complete, got {}",
            seen.terminal
        );
        for name in &expected {
            let prefix = name.split('/').next().unwrap_or_default();
            let held = listed_keys(&emulator, bucket, &format!("{prefix}/")).await;
            assert!(
                held.iter().any(|key| key == name),
                "case {case:?}: expected the checkpoint {name:?} under the prefix, got {held:?}"
            );
        }
    }
}

/// RULE SIX: a batch transform job scores every record of every input object with the model it
/// names, writes one `<object>.out` per input object and reports `Completed`; one whose role
/// may not read the input reports `Failed` with the access failure.
///
/// The rows differ along how many input objects and records the job is given, and along which
/// leg of the work authorization stops.
#[tokio::test]
async fn a_batch_transform_job_scores_its_input_and_writes_one_out_object() {
    let bucket = "transform-data";
    /// One row: the case, the input objects, the Amazon S3 grant the role carries, and the
    /// status the job has to reach.
    struct Row {
        case: &'static str,
        inputs: Vec<(&'static str, &'static str)>,
        grant: Option<Value>,
        completed: bool,
    }
    let rows = vec![
        Row {
            case: "one object of one record",
            inputs: vec![("input/one.csv", "2\n")],
            grant: None,
            completed: true,
        },
        Row {
            case: "several objects of several records",
            inputs: vec![
                ("input/one.csv", "1\n3\n5\n"),
                ("input/two.csv", "7\n9\n"),
            ],
            grant: None,
            completed: true,
        },
        Row {
            case: "a role that may not read the input",
            inputs: vec![("input/one.csv", "2\n")],
            grant: Some(allow(
                json!(["s3:PutObject", "s3:ListBucket"]),
                json!([
                    format!("arn:aws:s3:::{bucket}"),
                    format!("arn:aws:s3:::{bucket}/*")
                ]),
            )),
            completed: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &every_s3_action_on(bucket)).await;
        emulator.given_bucket(bucket).await;
        // The model the transform applies is one this emulator's own training produced, so the
        // artifact it reads carries parameters a run really measured.
        let artifact = given_trained_artifact(&emulator, case, bucket, &role, index).await;
        for (key, body) in &row.inputs {
            emulator.given_object(bucket, key, body).await;
        }
        let model = format!("transformmodel{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateModel",
                &json!({
                    "ModelName": model,
                    "ExecutionRoleArn": role,
                    "PrimaryContainer": { "Image": own_image(), "ModelDataUrl": artifact },
                }),
            )
            .await;
        // The grant a row narrows is narrowed AFTER the model exists, so what the row observes
        // is the transform's own reads and not the training that produced the artifact.
        if let Some(grant) = row.grant.as_ref() {
            emulator.given_role_policy(ROLE, grant).await;
        }

        let job = format!("transformjob{index}");
        emulator
            .sagemaker_ok(
                case,
                "CreateTransformJob",
                &json!({
                    "TransformJobName": job,
                    "ModelName": model,
                    "TransformInput": {
                        "DataSource": {
                            "S3DataSource": {
                                "S3DataType": "S3Prefix",
                                "S3Uri": format!("s3://{bucket}/input/"),
                            },
                        },
                        "ContentType": "text/csv",
                        "SplitType": "Line",
                    },
                    "TransformOutput": { "S3OutputPath": format!("s3://{bucket}/scored/") },
                    "TransformResources": { "InstanceType": "ml.m5.large", "InstanceCount": 1 },
                }),
            )
            .await;

        let deadline = Instant::now() + SETTLE_BUDGET;
        let settled = loop {
            let read = emulator
                .sagemaker_ok(case, "DescribeTransformJob", &json!({ "TransformJobName": job }))
                .await;
            if str_field(&read, "TransformJobStatus") != "InProgress" {
                break read;
            }
            look_again(
                deadline,
                case,
                "the transform job left InProgress",
                &read.to_string(),
            )
            .await;
        };
        let status = str_field(&settled, "TransformJobStatus");
        let reason = settled
            .get("FailureReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !row.completed {
            assert_eq!(
                status, "Failed",
                "case {case:?}: a job whose role may not read the input must fail, got {status} \
                 with {reason:?}"
            );
            assert!(
                reason.contains("AccessDenied"),
                "case {case:?}: the reason must name the access failure, got {reason:?}"
            );
            assert!(
                listed_keys(&emulator, bucket, "scored/").await.is_empty(),
                "case {case:?}: a job that read nothing writes nothing"
            );
            continue;
        }
        assert_eq!(
            status, "Completed",
            "case {case:?}: the job must complete, got {status} with {reason:?}"
        );
        assert_eq!(
            settled.get("FailureReason"),
            None,
            "case {case:?}: a completed job reports no failure reason, got {settled}"
        );
        assert!(
            settled.get("TransformStartTime").is_some()
                && settled.get("TransformEndTime").is_some(),
            "case {case:?}: a completed job reports the span of its work, got {settled}"
        );

        let mut expected: Vec<String> = row
            .inputs
            .iter()
            .map(|(key, _)| {
                let name = key.rsplit('/').next().unwrap_or(key);
                format!("scored/{name}.out")
            })
            .collect();
        expected.sort();
        // The objects a finished job left are a CONDITION, looked for again until they are
        // there rather than taken as an instant consequence of the status just read. The
        // exactness of the set is what `keys_until` loops on, so the assertion below is the
        // expectation stated once more where the rule is read.
        let written = keys_until(&emulator, case, bucket, "scored/", &expected).await;
        assert_eq!(
            written, expected,
            "case {case:?}: expected one .out per input object, got {written:?}"
        );
        for ((key, body), out) in row.inputs.iter().zip(&expected) {
            let records = body.lines().filter(|line| !line.is_empty()).count();
            let scored = String::from_utf8(object_bytes(&emulator, case, bucket, out).await)
                .expect("the scored object is text");
            let predictions: Vec<&str> = scored.lines().filter(|line| !line.is_empty()).collect();
            assert_eq!(
                predictions.len(),
                records,
                "case {case:?}: {key} holds {records} record(s), so {out} must hold that many \
                 predictions, got {predictions:?}"
            );
            for prediction in &predictions {
                assert!(
                    prediction.parse::<f64>().is_ok(),
                    "case {case:?}: every line of {out} is a prediction, got {prediction:?}"
                );
            }
        }
    }
}

/// Train a model over `bucket` and answer the artifact its run wrote, so a transform job has a
/// model whose parameters a run of this emulator really measured.
///
/// What the caller then does with the answer is hand it to `CreateModel`, which REFUSES model
/// data it cannot read — so this waits for the object itself to be served, not merely for the
/// status that implies it.
pub(crate) async fn given_trained_artifact(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
    role: &str,
    index: usize,
) -> String {
    emulator
        .given_object(bucket, "train/data.csv", &training_data(16, 1))
        .await;
    let job = format!("transformtraining{index}");
    emulator
        .sagemaker_ok(
            case,
            "CreateTrainingJob",
            &training_request(
                &job,
                role,
                bucket,
                &own_image(),
                json!([channel("train", bucket, "train/")]),
                json!({}),
                json!({}),
            ),
        )
        .await;
    let seen = observed(emulator, case, &job, bucket).await;
    assert_eq!(
        str_field(&seen.terminal, "TrainingJobStatus"),
        "Completed",
        "case {case:?}: the model the transform applies must be trained, got {}",
        seen.terminal
    );
    let artifact = str_field(
        seen.terminal
            .get("ModelArtifacts")
            .unwrap_or(&Value::Null),
        "S3ModelArtifacts",
    );
    served_artifact(emulator, case, bucket, artifact).await
}

/// The artifact URI `uri`, once Amazon S3 serves the object it names.
///
/// A job's artifact is published by whichever read or armed instant claims it first
/// (`state::publish_due_artifact`), so what a caller needs before handing the URI on is the
/// OBJECT: looked for again until it is served, under the one deadline this suite bounds a
/// wait with.
async fn served_artifact(emulator: &Emulator, case: &str, bucket: &str, uri: String) -> String {
    let key = uri
        .strip_prefix(&format!("s3://{bucket}/"))
        .unwrap_or_else(|| panic!("case {case:?}: the artifact URI names this bucket: {uri}"))
        .to_string();
    object_bytes(emulator, case, bucket, &key).await;
    uri
}

/// RULE: `CreateModel` refuses model data the execution role cannot read, with nothing stored,
/// and words the two conditions apart: an object that is not there is model data it could not
/// FIND, and one the role's own policies do not let it read is model data it could not ACCESS.
///
/// The rows differ along why the read fails: the key is absent, the role carries no grant for
/// the object, the role is granted another bucket only, and the role is granted the read.
#[tokio::test]
async fn create_model_refuses_model_data_the_execution_role_cannot_read() {
    let bucket = "model-artifacts";
    let held = format!("s3://{bucket}/models/model.tar.gz");
    let absent = format!("s3://{bucket}/models/missing.tar.gz");
    /// One row: the case, the grant the role carries, the model data URL, and the refusal
    /// expected — `None` for a row the create accepts.
    struct Row {
        case: &'static str,
        grant: Value,
        url: String,
        refusal: Option<Refusal>,
    }
    /// Which of the two sentences a row is refused with.
    enum Refusal {
        NotFound,
        NotAccessible,
    }
    let rows = vec![
        Row {
            case: "a readable object",
            grant: every_s3_action_on(bucket),
            url: held.clone(),
            refusal: None,
        },
        Row {
            case: "a key nothing is stored under",
            grant: every_s3_action_on(bucket),
            url: absent.clone(),
            refusal: Some(Refusal::NotFound),
        },
        Row {
            case: "a role carrying no Amazon S3 grant at all",
            grant: allow(json!("sts:AssumeRole"), json!("*")),
            url: held.clone(),
            refusal: Some(Refusal::NotAccessible),
        },
        Row {
            case: "a role granted the read of another bucket only",
            grant: allow(json!("s3:GetObject"), json!("arn:aws:s3:::some-other-bucket/*")),
            url: held.clone(),
            refusal: Some(Refusal::NotAccessible),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let role = given_job_role(&emulator, &row.grant).await;
        emulator.given_bucket(bucket).await;
        emulator
            .given_object(bucket, "models/model.tar.gz", "a model archive")
            .await;

        let name = format!("artifactmodel{index}");
        let (status, answered) = emulator
            .sagemaker(
                "CreateModel",
                &json!({
                    "ModelName": name,
                    "ExecutionRoleArn": role,
                    "PrimaryContainer": { "Image": own_image(), "ModelDataUrl": row.url },
                }),
            )
            .await;
        let (read_status, read) = emulator
            .sagemaker("DescribeModel", &json!({ "ModelName": name }))
            .await;
        let Some(refusal) = row.refusal else {
            assert_eq!(
                status, 200,
                "case {case:?}: a readable artifact must be accepted, got {status} with \
                 {answered}"
            );
            assert_eq!(
                read_status, 200,
                "case {case:?}: the accepted create must store the model, got {read_status} \
                 with {read}"
            );
            continue;
        };
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: unreadable model data must be refused, got {status} with {answered}"
        );
        let expected = match refusal {
            Refusal::NotFound => format!("Could not find model data at {}.", row.url),
            Refusal::NotAccessible => format!(
                "Could not access model data at {}. Please ensure that the role \"{role}\" \
                 exists and that its trust relationship policy allows the action \
                 \"sts:AssumeRole\" for the service principal \"{SERVICE_PRINCIPAL}\". Also \
                 ensure that the role has \"s3:GetObject\" permissions and that the object is \
                 located in {REGION}. If your Model uses multiple models or uncompressed \
                 models, please ensure that the role has \"s3:ListBucket\" permission.",
                row.url
            ),
        };
        let answered_message = message(&answered);
        assert_eq!(
            answered_message, expected,
            "case {case:?}: expected {expected:?}, got {answered_message:?}"
        );
        assert_ne!(
            read_status, 200,
            "case {case:?}: a refused create must store nothing, got {read_status} with {read}"
        );
    }
}
