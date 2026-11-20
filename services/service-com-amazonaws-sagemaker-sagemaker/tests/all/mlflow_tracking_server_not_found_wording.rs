// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the ONE sentence SageMaker refuses an MLflow tracking server
//! the account does not hold with.
//!
//! RULE: every operation addressed by `TrackingServerName` refuses a name the account
//! holds no tracking server under with the modelled `ResourceNotFound` at HTTP 400, whose
//! `Message` is the fixed text `Tracking Server with the following ARN does not exist: `
//! followed by the ARN the request's name and signed Region resolve to — the ARN bare: not
//! quoted, with nothing after it, and never the bare name.
//!
//! The cases span that rule rather than one recorded call: both implemented tracking-server
//! operations, every `TrackingServerName` shape the model admits (a single character, a
//! hyphenated name, both published length bounds), several signed Regions and the unsigned
//! call that falls back to the emulator's default Region, and a name differing from a
//! stored one only in case.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, on the
//! crate's shared `Emulator` harness.

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, Emulator, ACCOUNT};

/// The fixed text of the refusal, as AWS words it for a missing tracking server: the
/// resource kind in title case, addressed BY ARN, closing with a colon and one space.
const NOT_FOUND_PREFIX: &str = "Tracking Server with the following ARN does not exist: ";

/// The ARN resource kind a tracking server carries, as `TrackingServerArn`'s modelled
/// pattern spells it
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:mlflow-tracking-server/`).
const KIND: &str = "mlflow-tracking-server";

/// `ResourceNotFound$Message` targets `FailureReason`, which is `@length max 1024`
/// (`smithy_query(query="shape",shape="ResourceNotFound,FailureReason")`).
const MESSAGE_MAX_LENGTH: usize = 1024;

/// The two implemented operations addressed by `TrackingServerName`, from
/// `smithy_query(query="operation",operation="DescribeMlflowTrackingServer,CreatePresignedMlflowTrackingServerUrl")`:
/// both model `ResourceNotFound` as their not-found error.
const OPERATIONS: [&str; 2] = [
    "DescribeMlflowTrackingServer",
    "CreatePresignedMlflowTrackingServerUrl",
];

/// A role ARN inside `RoleArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/mlflow-artifacts")
}

/// The ARN the refusal must name for `name` in `region`.
fn tracking_server_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:{KIND}/{name}")
}

/// The smallest `CreateMlflowTrackingServer` request: the members the model marks required.
fn minimal(name: &str) -> Value {
    json!({
        "TrackingServerName": name,
        "ArtifactStoreUri": "s3://artifacts-bucket/mlflow",
        "RoleArn": role_arn(),
    })
}

/// Assert the whole rule for one refusal: the status, the modelled error code, the exact
/// sentence, and the structural invariants the sentence carries (the ARN bare, nothing
/// after it, inside the modelled length).
fn assert_refusal(case: &str, status: u16, parsed: &Value, region: &str, name: &str) {
    let expected = format!("{NOT_FOUND_PREFIX}{}", tracking_server_arn(region, name));
    let actual = message(parsed);
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} with {parsed:?}"
    );
    assert_eq!(
        error_code(parsed),
        "ResourceNotFound",
        "case {case}: expected the modelled ResourceNotFound, got {parsed:?}"
    );
    assert_eq!(
        actual, expected,
        "case {case}: expected {expected:?}, got {actual:?}"
    );
    assert!(
        actual.starts_with(NOT_FOUND_PREFIX),
        "case {case}: expected the fixed text {NOT_FOUND_PREFIX:?} first, got {actual:?}"
    );
    assert!(
        actual.ends_with(&tracking_server_arn(region, name)),
        "case {case}: the sentence must end with the bare ARN — unquoted, no trailing \
         period, got {actual:?}"
    );
    assert!(
        !actual.contains('"'),
        "case {case}: the ARN must not be quoted, got {actual:?}"
    );
    assert!(
        actual.len() <= MESSAGE_MAX_LENGTH,
        "case {case}: Message is modelled len 0..{MESSAGE_MAX_LENGTH}, got {} bytes in \
         {actual:?}",
        actual.len()
    );
}

/// RULE: every operation addressed by `TrackingServerName` refuses a name no create stored
/// with `ResourceNotFound` and the one sentence above, for every name shape the model
/// admits and whichever Region the caller signed for.
#[tokio::test]
async fn every_tracking_server_reader_words_a_missing_server_by_its_arn() {
    let emulator = Emulator::new();
    // One server that DOES exist, in the Region the case-difference row signs for, so a
    // refusal below is about the name the request carried and not about an empty store.
    let (status, created) = emulator
        .call_in("eu-west-3", "CreateMlflowTrackingServer", &minimal("stored"))
        .await;
    assert_eq!(
        status, 200,
        "CreateMlflowTrackingServer must answer HTTP 200, got {status} with {created:?}"
    );

    // `TrackingServerName` is `len 1..256, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,255}$`, so
    // the rows span both published bounds, a hyphenated name, a digit-led name and a name
    // differing from the stored one only in case — each in a Region of its own, since the
    // sentence names the ARN the signed Region resolves to.
    let longest = format!("z{}", "y".repeat(255));
    let rows: Vec<(&str, &str, String)> = vec![
        ("never-created", "us-east-1", "absent".to_string()),
        ("shortest-legal-name", "us-west-2", "a".to_string()),
        ("longest-legal-name", "ap-southeast-2", longest),
        (
            "hyphenated-name",
            "eu-central-1",
            "team-mlflow-01".to_string(),
        ),
        ("digit-led-name", "sa-east-1", "0server".to_string()),
        (
            "case-differs-from-a-stored-name",
            "eu-west-3",
            "Stored".to_string(),
        ),
    ];

    for operation in OPERATIONS {
        for (row, region, name) in &rows {
            let (status, parsed) = emulator
                .call_in(region, operation, &json!({ "TrackingServerName": name }))
                .await;
            assert_refusal(
                &format!("{operation}/{row}/{region}"),
                status,
                &parsed,
                region,
                name,
            );
            // A refusal brings nothing into being: the same call refused twice over.
            let (again, parsed_again) = emulator
                .call_in(region, operation, &json!({ "TrackingServerName": name }))
                .await;
            assert_refusal(
                &format!("{operation}/{row}/{region}/repeated"),
                again,
                &parsed_again,
                region,
                name,
            );
        }
    }

    // The stored server is untouched by every refusal above: its reader still answers 200.
    let (status, described) = emulator
        .call_in(
            "eu-west-3",
            "DescribeMlflowTrackingServer",
            &json!({ "TrackingServerName": "stored" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case stored-after-refusals: the stored server must still read back, got {status} \
         with {described:?}"
    );
}

/// RULE: the Region in the ARN the sentence names is the one the request resolves to, so an
/// unsigned call — which names no Region — is worded against the emulator's default Region
/// rather than against no Region at all.
#[tokio::test]
async fn an_unsigned_call_words_the_refusal_against_the_default_region() {
    let emulator = Emulator::new();
    for operation in OPERATIONS {
        let name = "unsigned-absent";
        let (status, parsed) = emulator
            .call_unsigned(operation, &json!({ "TrackingServerName": name }))
            .await;
        assert_refusal(
            &format!("{operation}/unsigned"),
            status,
            &parsed,
            // `state::DEFAULT_REGION`, the Region every unsigned request's ARNs carry.
            "us-east-1",
            name,
        );
    }
}

/// RULE: a tracking server that EXISTS is never worded as missing — the sentence belongs to
/// the absent-name path alone, so a stored server reads back and presigns in every Region
/// it was created in.
#[tokio::test]
async fn a_stored_tracking_server_is_never_refused_as_missing() {
    let emulator = Emulator::new();
    let rows = [
        ("us-east-1", "present-east"),
        ("eu-west-3", "present-eu"),
        ("ap-southeast-2", "a"),
    ];
    for (region, name) in rows {
        let (status, created) = emulator
            .call_in(region, "CreateMlflowTrackingServer", &minimal(name))
            .await;
        assert_eq!(
            status, 200,
            "case {region}/{name}: CreateMlflowTrackingServer must answer HTTP 200, got \
             {status} with {created:?}"
        );
        for operation in OPERATIONS {
            let (status, parsed) = emulator
                .call_in(region, operation, &json!({ "TrackingServerName": name }))
                .await;
            assert_eq!(
                status, 200,
                "case {region}/{name}/{operation}: a stored server is not missing, got \
                 {status} with {parsed:?}"
            );
            assert!(
                !message(&parsed).contains(NOT_FOUND_PREFIX),
                "case {region}/{name}/{operation}: a stored server must not be worded as \
                 missing, got {parsed:?}"
            );
        }
    }
}
