// SPDX-License-Identifier: CC0-1.0

//! An `s3RecordingConfig`'s bucket resolution is recomputed from S3's OWN state on every
//! write, and the refusal it answers names the condition that stopped it.
//!
//! RULE: Ground Station resolves the recording bucket before `CreateConfig` or `UpdateConfig`
//! stores the config, and WHICH of its two refusals the caller gets is decided by the answer
//! S3 gives about that bucket at the moment of the call:
//!
//! * S3 answering that there is no such bucket — and a `bucketArn` that names no bucket at
//!   all, which is the same thing settled without asking — is refused with the sentence saying
//!   the bucket does not exist, whose remedy is to create it or correct the ARN;
//! * S3 answering about the bucket while withholding where it is — the read refused — is
//!   refused with the sentence naming the permission that read needs.
//!
//! The dimension this suite varies is the one the sibling tables hold fixed: ONE `bucketArn`
//! and ONE role, with S3's own state moved under them between calls (the bucket created, its
//! read denied by a bucket policy, the denial lifted, the bucket deleted). The sentence has to
//! follow S3's current answer at each stage, so neither refusal can be a decision taken once
//! and remembered, nor one keyed to how the ARN is spelled. The role carries the recording
//! grant Ground Station's setup documents (`s3:GetBucketLocation`, `s3:PutObject`,
//! `s3:ListBucket`), so the stage where the bucket is simply not there must NOT send the
//! caller off to a role policy that is already right.
//!
//! Both refusals carry the modelled `InvalidParameterException` (HTTP 431) with
//! `parameterName` `configData.s3RecordingConfig.bucketArn`, and each sentence is fixed text,
//! interpolating no bucket, ARN, account or region.
//!
//! The three services this spans — Ground Station, IAM and S3 over one engine — and the front
//! doors onto them are the ones `all/groundstation_recording_bucket_role.rs` stands up, with
//! the bucket-policy writer and the error reader from
//! `all/groundstation_recording_bucket_location_read.rs`, rather than a second copy of either.

use serde_json::{json, Value};
use service_com_amazonaws_groundstation_groundstation::state::{
    GROUNDSTATION_SERVICE_PRINCIPAL, RECORDING_BUCKET_ABSENT, RECORDING_BUCKET_UNRESOLVABLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::groundstation_recording_bucket_location_read::{
    deny_bucket_read, error_code_of, put_bucket_policy,
};
use crate::groundstation_recording_bucket_role::{
    create_bucket, emulator, groundstation, iam, message_of, recording_config, role_with_trust,
    trust_allowing, BLAMED_PARAMETER, REGION,
};

/// The error shape both refusals carry.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence a bucket S3 reports no such bucket for is refused with. Written out here as
/// well as imported, so the stages below expect AWS's wording rather than whatever the crate
/// happens to carry.
const BUCKET_DOES_NOT_EXIST: &str = "Unable to create new S3 endpoint config - bucket does \
     not exist.";

/// The sentence a bucket S3 answers about while withholding its location is refused with.
const LOCATION_UNREADABLE: &str = "Unable to create new S3 endpoint config - unable to get \
     location of bucket. Ensure that Ground Station has s3:GetBucketLocation permissions on \
     the bucket.";

/// The one bucket every call below records to: the stages move S3's state under this ONE name.
const BUCKET: &str = "aws-groundstation-downlink";

/// The role every call below records through: held by this account, trusted by Ground Station,
/// and carrying the recording grant.
const ROLE_NAME: &str = "gs-recording-bucket-state";

/// The permission the withheld-location refusal names, which the role below already carries:
/// the stage where the bucket is absent must not name it.
const LOCATION_PERMISSION: &str = "s3:GetBucketLocation";

/// What a Ground Station recording role is granted on the bucket it records to — the bucket's
/// location, writing objects under it, and listing it — so no stage below is refused for a
/// grant the role is missing.
fn recording_access_policy(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [LOCATION_PERMISSION, "s3:ListBucket", "s3:PutObject"],
            "Resource": [
                format!("arn:aws:s3:::{bucket}"),
                format!("arn:aws:s3:::{bucket}/*"),
            ],
        }],
    })
    .to_string()
}

/// One request to the emulator's S3 front door that carries no body, answered with its HTTP
/// status: path-style, under the regional S3 host the other suites address.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> u16 {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    router.dispatch(request).await.status().as_u16()
}

/// `DELETE /<bucket>?policy` — `DeleteBucketPolicy`, which lifts the denial that makes S3
/// withhold the bucket's location.
async fn delete_bucket_policy(router: &MultiServiceRouter, bucket: &str) {
    let status = s3(router, "DELETE", &format!("/{bucket}?policy")).await;
    assert!(
        (200..300).contains(&status),
        "DeleteBucketPolicy {bucket:?}: expected the denial to be lifted, got {status}"
    );
}

/// `DELETE /<bucket>` — `DeleteBucket`, which takes the bucket away again, so S3 answers that
/// it holds no such bucket.
async fn delete_bucket(router: &MultiServiceRouter, bucket: &str) {
    let status = s3(router, "DELETE", &format!("/{bucket}")).await;
    assert!(
        (200..300).contains(&status),
        "DeleteBucket {bucket:?}: expected the bucket to be removed, got {status}"
    );
}

/// What a write at one stage earns: the config stored, or one of the two refusals.
#[derive(Clone, Copy, Debug)]
enum Expected {
    Stored,
    Refused(&'static str),
}

/// What is done to S3's own state before a stage's write, which is the only thing that moves
/// between the stages: the `bucketArn`, the role and the caller stay as they are.
#[derive(Clone, Copy, Debug)]
enum S3Change {
    /// The account comes to hold the bucket, whose read nothing denies.
    BucketCreated,
    /// The bucket is taken away, so S3 answers that it holds no such bucket.
    BucketDeleted,
    /// The bucket stays, with a policy denying its read, so S3 answers about it while
    /// withholding where it is.
    ReadDenied,
}

impl S3Change {
    /// Apply the change to the emulator's S3.
    async fn apply(self, router: &MultiServiceRouter) {
        match self {
            Self::BucketCreated => create_bucket(router, BUCKET).await,
            Self::BucketDeleted => delete_bucket(router, BUCKET).await,
            Self::ReadDenied => {
                put_bucket_policy(router, BUCKET, &deny_bucket_read(BUCKET)).await
            }
        }
    }
}

/// An `s3RecordingConfig` recording to `bucket_arn` under `prefix`: an update has to differ
/// from what is stored to show it took effect, which is why the prefix is a parameter here
/// while the create stages take the sibling suite's config as it stands.
fn recording_under_prefix(bucket_arn: &str, role_arn: &str, prefix: &str) -> Value {
    json!({"s3RecordingConfig": {
        "bucketArn": bucket_arn,
        "roleArn": role_arn,
        "prefix": prefix,
    }})
}

/// Assert one refusal: status, error shape, the sentence word for word, the member it blames,
/// that nothing of the request reached the sentence, and — for the bucket-does-not-exist
/// sentence — that it names no permission the caller never failed on.
fn assert_refusal(case: &str, status: u16, payload: &Value, expected: &str, quoted: &[&str]) {
    assert_eq!(
        status, 431,
        "case {case:?}: expected HTTP 431, got {status} with {payload}"
    );
    assert_eq!(
        error_code_of(payload),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {payload}"
    );
    assert_eq!(
        message_of(payload),
        expected,
        "case {case:?}: expected {expected:?}, got {payload}"
    );
    assert_eq!(
        payload.get("parameterName"),
        Some(&json!(BLAMED_PARAMETER)),
        "case {case:?}: expected the bucketArn member to be blamed, got {payload}"
    );
    for value in quoted {
        assert!(
            !message_of(payload).contains(value),
            "case {case:?}: the refusal interpolates nothing from the request, got {payload}"
        );
    }
    assert_eq!(
        message_of(payload).contains(LOCATION_PERMISSION),
        expected == LOCATION_UNREADABLE,
        "case {case:?}: only the withheld-location refusal names {LOCATION_PERMISSION}, got \
         {payload}"
    );
}

/// The crate's exported sentences and the ones this suite expects are the same text, so a
/// drift in either fails here rather than travelling unnoticed.
#[test]
fn the_sentences_this_suite_expects_are_the_ones_the_service_answers() {
    assert_eq!(
        RECORDING_BUCKET_ABSENT, BUCKET_DOES_NOT_EXIST,
        "case exported-constant: expected the bucket-does-not-exist sentence, got \
         {RECORDING_BUCKET_ABSENT:?}"
    );
    assert_eq!(
        RECORDING_BUCKET_UNRESOLVABLE, LOCATION_UNREADABLE,
        "case exported-constant: expected the unobtainable-location sentence, got \
         {RECORDING_BUCKET_UNRESOLVABLE:?}"
    );
}

/// One emulator carrying Ground Station, IAM and S3 over one engine, holding the role every
/// stage records through — and NO bucket yet. Hands back the role's ARN.
async fn harness() -> (MultiServiceRouter, String) {
    let router = emulator();
    let role_arn = role_with_trust(
        &router,
        ROLE_NAME,
        &trust_allowing(GROUNDSTATION_SERVICE_PRINCIPAL),
    )
    .await;
    let (status, answer) = iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "gs-recording-access"),
            ("PolicyDocument", &recording_access_policy(BUCKET)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: the recording grant must be attached to the role, got {status}: {answer}"
    );
    (router, role_arn)
}

/// `CreateConfig` at one stage, checked against what that stage expects. Hands back the id of
/// a config that was stored.
async fn create_config(
    router: &MultiServiceRouter,
    name: &str,
    bucket_arn: &str,
    role_arn: &str,
    case: &str,
    expected: Expected,
) -> Option<String> {
    let config_data = recording_config(bucket_arn, role_arn);
    let (status, payload) = groundstation(
        router,
        "POST",
        "/config",
        Some(json!({ "name": name, "configData": config_data })),
    )
    .await;
    match expected {
        Expected::Stored => {
            assert_eq!(
                status, 200,
                "case {case:?}: a bucket Ground Station can locate must be stored, got {status} \
                 with {payload}"
            );
            assert_eq!(
                payload.get("configType"),
                Some(&json!("s3-recording")),
                "case {case:?}: expected an s3-recording config, got {payload}"
            );
            let config_id = payload["configId"]
                .as_str()
                .expect("a minted config id")
                .to_string();
            // Round trip: the stored config reads back with the configData it carried in.
            let (read_status, read_back) = groundstation(
                router,
                "GET",
                &format!("/config/s3-recording/{config_id}"),
                None,
            )
            .await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: expected the stored config to be readable, got {read_status} \
                 with {read_back}"
            );
            assert_eq!(
                read_back.get("configData"),
                Some(&config_data),
                "case {case:?}: expected the created configData, got {read_back}"
            );
            Some(config_id)
        }
        Expected::Refused(message) => {
            assert_refusal(case, status, &payload, message, &[bucket_arn, role_arn]);
            None
        }
    }
}

#[tokio::test]
async fn each_create_answers_the_condition_s3_reports_for_the_bucket_right_now() {
    let (router, role_arn) = harness().await;
    let bucket_arn = format!("arn:aws:s3:::{BUCKET}");
    let mut stored: Vec<String> = Vec::new();

    // Nothing holds the bucket yet: S3 answers that there is no such bucket.
    create_config(
        &router,
        "recording-before-the-bucket",
        &bucket_arn,
        &role_arn,
        "before the bucket is created",
        Expected::Refused(BUCKET_DOES_NOT_EXIST),
    )
    .await;

    // The bucket is created, and the SAME request is now stored.
    create_bucket(&router, BUCKET).await;
    create_config(
        &router,
        "recording-with-the-bucket",
        &bucket_arn,
        &role_arn,
        "after the bucket is created",
        Expected::Stored,
    )
    .await;
    stored.push("recording-with-the-bucket".to_string());

    // The bucket is still there, but S3 now refuses the read: the other sentence, for the
    // same ARN and the same role.
    put_bucket_policy(&router, BUCKET, &deny_bucket_read(BUCKET)).await;
    create_config(
        &router,
        "recording-with-the-read-denied",
        &bucket_arn,
        &role_arn,
        "after the bucket's read is denied",
        Expected::Refused(LOCATION_UNREADABLE),
    )
    .await;

    // The denial is lifted and the location can be had again.
    delete_bucket_policy(&router, BUCKET).await;
    create_config(
        &router,
        "recording-with-the-denial-lifted",
        &bucket_arn,
        &role_arn,
        "after the denial is lifted",
        Expected::Stored,
    )
    .await;
    stored.push("recording-with-the-denial-lifted".to_string());

    // The bucket is deleted: back to the bucket-does-not-exist sentence, not the one naming a
    // permission the role has held throughout.
    delete_bucket(&router, BUCKET).await;
    create_config(
        &router,
        "recording-after-the-bucket-is-gone",
        &bucket_arn,
        &role_arn,
        "after the bucket is deleted",
        Expected::Refused(BUCKET_DOES_NOT_EXIST),
    )
    .await;

    // The listing is the store: exactly the two stages that resolved are there, so no refused
    // stage wrote anything.
    let (status, listed) = groundstation(&router, "GET", "/config", None).await;
    assert_eq!(
        status, 200,
        "case refused-creates-store-nothing: expected ListConfigs to answer, got {status} with \
         {listed}"
    );
    let mut names: Vec<String> = listed["configList"]
        .as_array()
        .expect("a configList array")
        .iter()
        .map(|config| config["name"].as_str().unwrap_or_default().to_string())
        .collect();
    // Compared as a SET: which configs are there is the rule, the listing's order is not.
    names.sort();
    stored.sort();
    assert_eq!(
        names, stored,
        "case refused-creates-store-nothing: expected only the stored configs, got {listed}"
    );
}

#[tokio::test]
async fn each_update_answers_the_condition_s3_reports_and_changes_nothing_when_refused() {
    let (router, role_arn) = harness().await;
    let bucket_arn = format!("arn:aws:s3:::{BUCKET}");

    create_bucket(&router, BUCKET).await;
    let first = recording_under_prefix(&bucket_arn, &role_arn, "downlink/first");
    let (status, created) = groundstation(
        &router,
        "POST",
        "/config",
        Some(json!({ "name": "recording-updated", "configData": first.clone() })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: the config to update must be stored, got {status} with {created}"
    );
    let path = format!(
        "/config/s3-recording/{}",
        created["configId"].as_str().expect("a minted config id")
    );

    /// One update: what S3's state is moved to first, the stage's name, the configData the
    /// update carries, and what it earns.
    struct Stage {
        change: S3Change,
        case: &'static str,
        config_data: Value,
        expected: Expected,
    }

    // Each stage runs after its own change to S3, and the stored configData after it is
    // whatever the last ACCEPTED update left.
    let mut expected_stored = first;
    for stage in [
        Stage {
            change: S3Change::BucketDeleted,
            case: "an update made while the bucket is gone",
            config_data: recording_under_prefix(&bucket_arn, &role_arn, "downlink/second"),
            expected: Expected::Refused(BUCKET_DOES_NOT_EXIST),
        },
        Stage {
            change: S3Change::BucketCreated,
            case: "an update made while the bucket resolves",
            config_data: recording_under_prefix(&bucket_arn, &role_arn, "downlink/third"),
            expected: Expected::Stored,
        },
        Stage {
            change: S3Change::ReadDenied,
            case: "an update made while the bucket's read is denied",
            config_data: recording_under_prefix(&bucket_arn, &role_arn, "downlink/fourth"),
            expected: Expected::Refused(LOCATION_UNREADABLE),
        },
    ] {
        stage.change.apply(&router).await;

        let case = stage.case;
        let (status, payload) = groundstation(
            &router,
            "PUT",
            &path,
            Some(json!({ "name": "recording-updated", "configData": stage.config_data.clone() })),
        )
        .await;
        match stage.expected {
            Expected::Stored => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an update whose bucket resolves must be stored, got {status} \
                     with {payload}"
                );
                expected_stored = stage.config_data.clone();
            }
            Expected::Refused(message) => {
                assert_refusal(
                    case,
                    status,
                    &payload,
                    message,
                    &[bucket_arn.as_str(), role_arn.as_str()],
                );
            }
        }

        // What the config records to after this stage: the last accepted update's configData,
        // so a refused update changed nothing.
        let (read_status, read_back) = groundstation(&router, "GET", &path, None).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected the config to be readable, got {read_status} with \
             {read_back}"
        );
        assert_eq!(
            read_back.get("configData"),
            Some(&expected_stored),
            "case {case:?}: expected {expected_stored}, got {read_back}"
        );
    }
}
