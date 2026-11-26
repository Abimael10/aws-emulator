// SPDX-License-Identifier: CC0-1.0

//! What `CreateTrainingJob` TELLS a caller whose input data source answered no object, driven
//! end to end over the wire with SageMaker, Amazon S3 and IAM mounted on ONE shared engine.
//!
//! THE RULE UNDER TEST: the refusal of an input data source that answered no object reports
//! whatever Amazon S3 ITSELF answered about the location, as a trailing clause after the
//! location sentence — so the absence of the BUCKET is reported with the sentence Amazon S3
//! answers for that absence, while a listing that answered and merely held no object carries no
//! such clause, Amazon S3 having said nothing of its own about it. A client can therefore tell a
//! mistyped or deleted bucket (fix the URI, create the bucket) from an empty prefix (wait for
//! the data to land) off the message.
//!
//! The rows differ along WHAT Amazon S3 answered about the location and along the FORM of the
//! location it was asked about: a bucket this account does not hold, another absent bucket under
//! a deeper prefix, a prefix of an existing bucket that holds nothing, a key nothing is stored
//! at, a deeper prefix under a stored key, and — varying the number of channels and which of
//! them is unreadable — a second channel naming an absent bucket. A channel whose location holds
//! an object is accepted, which is what makes the rows above a refusal of the location rather
//! than of every call.

use serde_json::{json, Value};

use super::model_artifact_admission::{error_code, message, Emulator, REGION};
use super::sagemaker_job_execution_over_s3::{allow, channel, given_job_role, own_image};

/// The bucket whose objects the readable rows name.
const BUCKET: &str = "training-answer-input";

/// What Amazon S3 answers a request naming a bucket it does not hold: Amazon S3 "Error
/// responses" lists `NoSuchBucket` — "The specified bucket does not exist" — which is the
/// sentence a refusal reporting Amazon S3's own answer carries.
const NO_SUCH_BUCKET: &str = "The specified bucket does not exist";

/// The refusal AWS words for an input data source it found no object under: the four facts of
/// the location sentence, then whatever Amazon S3 itself answered about the location.
fn no_objects_found(s3_uri: &str, role_arn: &str, bucket: &str, s3_answer: Option<&str>) -> String {
    let clause = s3_answer
        .map(|answer| format!(" Error message from S3: {answer}"))
        .unwrap_or_default();
    format!(
        "No S3 objects found under S3 URL \"{s3_uri}\" given in input data source. Please \
         ensure that the bucket exists in the selected region ({REGION}), that objects exist \
         under that S3 prefix, and that the role \"{role_arn}\" has \"s3:ListBucket\" \
         permissions on bucket \"{bucket}\".{clause}"
    )
}

/// One `CreateTrainingJob` request whose input data config is `channels`.
fn training_request(name: &str, role: &str, channels: Value) -> Value {
    json!({
        "TrainingJobName": name,
        "AlgorithmSpecification": { "TrainingImage": own_image(), "TrainingInputMode": "File" },
        "RoleArn": role,
        "InputDataConfig": channels,
        "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/out/") },
        "ResourceConfig": {
            "InstanceType": "ml.m5.large",
            "InstanceCount": 1,
            "VolumeSizeInGB": 10,
        },
        "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
    })
}

/// One row: the case, the channels the request declares, which of their locations the refusal
/// is about, and what Amazon S3 answered about that location — `None` on a row whose location
/// holds an object, which is accepted instead.
struct Row {
    case: &'static str,
    /// The `(channel name, bucket, prefix)` of every channel the request declares, in the
    /// order it carries them.
    channels: Vec<(&'static str, &'static str, &'static str)>,
    /// The `(bucket, prefix)` the refusal has to name, and `None` for an accepted row.
    refused: Option<(&'static str, &'static str)>,
    /// What Amazon S3 itself answered about that location, and `None` where it answered a
    /// listing that merely held nothing.
    s3_answer: Option<&'static str>,
}

/// RULE: `CreateTrainingJob`'s refusal of an input data source that answered no object carries
/// what Amazon S3 itself answered about the location — the absence of the bucket named with
/// Amazon S3's own sentence, and no clause at all for a listing that answered and held nothing
/// — and stores no job; a channel whose location holds an object is accepted.
#[tokio::test]
async fn the_refusal_reports_what_s3_answered_about_the_absent_location() {
    let emulator = Emulator::new();
    // A role granted every Amazon S3 action, so nothing a row observes is the identity-based
    // question: what is left is the answer Amazon S3 gave about the location itself.
    let role_arn = given_job_role(&emulator, &allow(json!("s3:*"), json!("*"))).await;
    emulator.given_bucket(BUCKET).await;
    emulator
        .given_object(BUCKET, "train/data.csv", "3,1,2\n6,2,3\n")
        .await;
    emulator.given_object(BUCKET, "single.csv", "3,1,2\n").await;

    let rows = [
        Row {
            case: "a bucket this account does not hold",
            channels: vec![("train", "training-answer-nowhere", "train/")],
            refused: Some(("training-answer-nowhere", "train/")),
            s3_answer: Some(NO_SUCH_BUCKET),
        },
        Row {
            case: "another absent bucket, named under a deeper prefix",
            channels: vec![("train", "training-answer-deleted", "sets/2026/part-1/")],
            refused: Some(("training-answer-deleted", "sets/2026/part-1/")),
            s3_answer: Some(NO_SUCH_BUCKET),
        },
        Row {
            case: "a prefix the bucket holds nothing under",
            channels: vec![("train", BUCKET, "nothing-here/")],
            refused: Some((BUCKET, "nothing-here/")),
            s3_answer: None,
        },
        Row {
            case: "a key nothing is stored at",
            channels: vec![("train", BUCKET, "train/absent.csv")],
            refused: Some((BUCKET, "train/absent.csv")),
            s3_answer: None,
        },
        Row {
            case: "a deeper prefix under a stored key",
            channels: vec![("train", BUCKET, "single.csv/deeper/")],
            refused: Some((BUCKET, "single.csv/deeper/")),
            s3_answer: None,
        },
        Row {
            case: "a second channel naming an absent bucket",
            channels: vec![
                ("train", BUCKET, "train/"),
                ("validation", "training-answer-missing", "validation/"),
            ],
            refused: Some(("training-answer-missing", "validation/")),
            s3_answer: Some(NO_SUCH_BUCKET),
        },
        Row {
            case: "a prefix holding an object",
            channels: vec![("train", BUCKET, "train/")],
            refused: None,
            s3_answer: None,
        },
        Row {
            case: "a key naming one stored object",
            channels: vec![("train", BUCKET, "single.csv")],
            refused: None,
            s3_answer: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("answerjob{index}");
        let channels: Vec<Value> = row
            .channels
            .iter()
            .map(|(channel_name, bucket, prefix)| channel(channel_name, bucket, prefix))
            .collect();
        let (status, answered) = emulator
            .sagemaker(
                "CreateTrainingJob",
                &training_request(&name, &role_arn, json!(channels)),
            )
            .await;

        let Some((bucket, prefix)) = row.refused else {
            assert_eq!(
                status, 200,
                "case {case:?}: a channel holding an object must be accepted, got {status} with \
                 {answered}"
            );
            let (read_status, read) = emulator
                .sagemaker("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
                .await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: the accepted create must store the job, got {read_status} with \
                 {read}"
            );
            continue;
        };

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a location answering no object must be refused, got {status} with \
             {answered}"
        );
        let s3_uri = format!("s3://{bucket}/{prefix}");
        let expected = no_objects_found(&s3_uri, &role_arn, bucket, row.s3_answer);
        let read_message = message(&answered);
        assert_eq!(
            read_message, expected,
            "case {case:?}: expected {expected:?}, got {read_message:?}"
        );
        // The refusal is PRE-CREATION, so nothing was stored under the name it was made with.
        let (read_status, read) = emulator
            .sagemaker("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        assert_ne!(
            read_status, 200,
            "case {case:?}: a refused create must store nothing, got {read_status} with {read}"
        );
    }
}
