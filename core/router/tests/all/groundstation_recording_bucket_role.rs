// SPDX-License-Identifier: CC0-1.0

//! How Ground Station resolves an `s3RecordingConfig`'s recording bucket, across the three
//! services that question spans.
//!
//! RULE: Ground Station reaches the recording bucket by TAKING ON the role the config names
//! (`S3RecordingConfig$roleArn` is the role Ground Station assumes to write data to the
//! bucket), and `CreateConfig` stores the config only once it has the bucket's location. Which
//! refusal a caller gets says what stopped that resolution, because the two are what a caller
//! can act on differently:
//!
//! * the location read could not be MADE at all — this account's IAM holds no role under the
//!   ARN, or the role's trust policy does not admit Ground Station — so nothing is known about
//!   the bucket, and the refusal names the permission that read needs
//!   (`s3:GetBucketLocation`), whether or not the bucket is there;
//! * the read WAS made and answered that there is no such bucket — and a `bucketArn` that
//!   names no bucket at all, which is the same thing settled without asking — so the refusal
//!   says the bucket does not exist.
//!
//! Both carry the modelled `InvalidParameterException` (HTTP 431) and `parameterName`
//! `configData.s3RecordingConfig.bucketArn`, and each sentence is FIXED text, interpolating no
//! bucket, ARN, account or region.
//!
//! This suite lives here because the rule spans three services over one engine: the role is
//! created through IAM's own `CreateRole`, so what Ground Station reads is the trust document
//! IAM persisted, and the bucket through S3's own `CreateBucket`, so the bucket answer is
//! S3's. The rows differ along what the rule generalises over — whether the account holds the
//! bucket, whether it holds the role, what the role's trust policy admits, and whether the
//! `bucketArn` names a bucket at all — crossed over both writers of the one resolution path
//! (`CreateConfig` and `UpdateConfig`).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_groundstation_groundstation::state::{
    GROUNDSTATION_SERVICE_PRINCIPAL, RECORDING_BUCKET_ABSENT, RECORDING_BUCKET_UNRESOLVABLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The region every request below is signed for, and the region the bucket lands in.
const REGION: &str = "us-east-1";

/// The member of the request a bucket-resolution refusal blames.
const BLAMED_PARAMETER: &str = "configData.s3RecordingConfig.bucketArn";

/// The one bucket the account in these tests holds.
const HELD_BUCKET: &str = "aws-groundstation-downlink";

/// A bucket name nothing ever created.
const ABSENT_BUCKET: &str = "aws-groundstation-absent";

/// One emulator carrying Ground Station, IAM and S3 over ONE shared engine, exactly as the
/// production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_groundstation_groundstation::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// Create a role in IAM carrying `document` and hand back the ARN IAM minted for it.
async fn role_with_trust(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    element(&created, "Arn")
}

/// A trust policy letting `principal` assume the role.
fn trust_allowing(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// `PUT /<bucket>` against the emulator's S3 front door — `CreateBucket`, path-style, with no
/// `LocationConstraint`, so the bucket lands in the region the Ground Station calls are signed
/// for.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://s3.{REGION}.amazonaws.com/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let status = router.dispatch(request).await.status().as_u16();
    assert_eq!(
        status, 200,
        "CreateBucket {bucket:?}: expected the bucket to be created, got {status}"
    );
}

/// One restJson1 call to Ground Station, answered with the HTTP status and the JSON body.
async fn groundstation(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://groundstation.{REGION}.amazonaws.com{path}"))
        .header("host", format!("groundstation.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/groundstation/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        );
    let payload = match &body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("the test body serializes"))
        }
        None => bytes::Bytes::new(),
    };
    let response = router
        .dispatch(builder.body(payload).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An `s3RecordingConfig` recording to `bucket_arn` through `role_arn`.
fn recording_config(bucket_arn: &str, role_arn: &str) -> Value {
    json!({"s3RecordingConfig": {
        "bucketArn": bucket_arn,
        "roleArn": role_arn,
        "prefix": "data/{satellite_id}/{year}/{month}/{day}",
    }})
}

/// The `message` of a Ground Station failure body.
fn message_of(payload: &Value) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Which role a row names: one IAM holds and Ground Station may assume, one IAM holds nothing
/// under, and one IAM holds whose trust policy admits another service only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Assumable,
    Absent,
    TrustsAnotherService,
}

/// One row of the resolution table: the bucket ARN the config records to, the role it names,
/// and the refusal the write earns (`None` for a config that is stored).
struct Row {
    case: &'static str,
    bucket_arn: String,
    role: Role,
    refusal: Option<&'static str>,
}

/// Every row of the table, built once so both writers are driven over the same set.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a held bucket through a role Ground Station may assume",
            bucket_arn: format!("arn:aws:s3:::{HELD_BUCKET}"),
            role: Role::Assumable,
            refusal: None,
        },
        Row {
            case: "a key under a held bucket through a role Ground Station may assume",
            bucket_arn: format!("arn:aws:s3:::{HELD_BUCKET}/downlink/2026"),
            role: Role::Assumable,
            refusal: None,
        },
        Row {
            case: "an absent bucket through a role Ground Station may assume",
            bucket_arn: format!("arn:aws:s3:::{ABSENT_BUCKET}"),
            role: Role::Assumable,
            refusal: Some(RECORDING_BUCKET_ABSENT),
        },
        Row {
            case: "a value that is no S3 bucket ARN, through a role it may assume",
            bucket_arn: format!("arn:aws:iam::{}:role/not-a-bucket", engine::EMULATOR_ACCOUNT_ID),
            role: Role::Assumable,
            refusal: Some(RECORDING_BUCKET_ABSENT),
        },
        Row {
            case: "an absent bucket through a role this account does not hold",
            bucket_arn: format!("arn:aws:s3:::{ABSENT_BUCKET}"),
            role: Role::Absent,
            refusal: Some(RECORDING_BUCKET_UNRESOLVABLE),
        },
        Row {
            case: "a held bucket through a role this account does not hold",
            bucket_arn: format!("arn:aws:s3:::{HELD_BUCKET}"),
            role: Role::Absent,
            refusal: Some(RECORDING_BUCKET_UNRESOLVABLE),
        },
        Row {
            case: "an absent bucket through a role that trusts another service",
            bucket_arn: format!("arn:aws:s3:::{ABSENT_BUCKET}"),
            role: Role::TrustsAnotherService,
            refusal: Some(RECORDING_BUCKET_UNRESOLVABLE),
        },
        Row {
            case: "a held bucket through a role that trusts another service",
            bucket_arn: format!("arn:aws:s3:::{HELD_BUCKET}"),
            role: Role::TrustsAnotherService,
            refusal: Some(RECORDING_BUCKET_UNRESOLVABLE),
        },
    ]
}

/// One emulator holding the bucket this account has and the two roles the rows name, with the
/// ARN of each role.
async fn harness() -> (MultiServiceRouter, Vec<(Role, String)>) {
    let router = emulator();
    create_bucket(&router, HELD_BUCKET).await;
    let assumable = role_with_trust(
        &router,
        "gs-recording-role",
        &trust_allowing(GROUNDSTATION_SERVICE_PRINCIPAL),
    )
    .await;
    let other = role_with_trust(
        &router,
        "gs-recording-role-elsewhere",
        &trust_allowing("ec2.amazonaws.com"),
    )
    .await;
    let roles = vec![
        (Role::Assumable, assumable),
        (
            Role::Absent,
            format!(
                "arn:aws:iam::{}:role/gs-recording-role-nothing-holds",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
        (Role::TrustsAnotherService, other),
    ];
    (router, roles)
}

/// The ARN of the role `role` names in a harness.
fn role_arn_of(roles: &[(Role, String)], role: Role) -> String {
    roles
        .iter()
        .find(|(held, _)| *held == role)
        .map(|(_, arn)| arn.clone())
        .unwrap_or_else(|| panic!("the harness holds a role for {role:?}"))
}

#[tokio::test]
async fn a_recording_bucket_that_cannot_be_located_is_refused_for_its_own_condition() {
    let (router, roles) = harness().await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let role_arn = role_arn_of(&roles, row.role);
        let (status, payload) = groundstation(
            &router,
            "POST",
            "/config",
            Some(json!({
                "name": format!("recording-{index}"),
                "configData": recording_config(&row.bucket_arn, &role_arn),
            })),
        )
        .await;

        match row.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a bucket Ground Station can locate must be stored, got \
                     {status} with {payload}"
                );
                assert_eq!(
                    payload.get("configType"),
                    Some(&json!("s3-recording")),
                    "case {case:?}: expected an s3-recording config, got {payload}"
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, 431,
                    "case {case:?}: expected HTTP 431, got {status} with {payload}"
                );
                assert_eq!(
                    message_of(&payload),
                    expected,
                    "case {case:?}: expected {expected:?}, got {payload}"
                );
                assert_eq!(
                    payload.get("parameterName"),
                    Some(&json!(BLAMED_PARAMETER)),
                    "case {case:?}: expected the bucketArn member to be blamed, got {payload}"
                );
                // The sentences are fixed text: nothing of the request reaches them.
                for quoted in [row.bucket_arn.as_str(), role_arn.as_str()] {
                    assert!(
                        !message_of(&payload).contains(quoted),
                        "case {case:?}: the refusal interpolates nothing from the request, got \
                         {payload}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn an_update_answers_the_same_condition_as_a_create() {
    let (router, roles) = harness().await;
    let assumable = role_arn_of(&roles, Role::Assumable);

    // A config whose bucket Ground Station CAN locate, so every refused update below has a
    // stored config to leave untouched.
    let held = recording_config(&format!("arn:aws:s3:::{HELD_BUCKET}"), &assumable);
    let (status, created) = groundstation(
        &router,
        "POST",
        "/config",
        Some(json!({ "name": "recording-held", "configData": held })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case setup: the control config must be stored, got {status} with {created}"
    );
    let path = format!(
        "/config/s3-recording/{}",
        created["configId"].as_str().expect("a minted config id")
    );

    for row in rows() {
        let Some(expected) = row.refusal else {
            continue;
        };
        let case = row.case;
        let role_arn = role_arn_of(&roles, row.role);
        let (status, payload) = groundstation(
            &router,
            "PUT",
            &path,
            Some(json!({
                "name": "recording-held",
                "configData": recording_config(&row.bucket_arn, &role_arn),
            })),
        )
        .await;
        assert_eq!(
            status, 431,
            "case update to {case:?}: expected HTTP 431, got {status} with {payload}"
        );
        assert_eq!(
            message_of(&payload),
            expected,
            "case update to {case:?}: expected {expected:?}, got {payload}"
        );
    }

    // Every one of those updates was refused, so the stored config still records where it was
    // created to record.
    let (status, read_back) = groundstation(&router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case refused-updates-change-nothing: expected the config to be readable, got {status} \
         with {read_back}"
    );
    assert_eq!(
        read_back.get("configData"),
        Some(&recording_config(&format!("arn:aws:s3:::{HELD_BUCKET}"), &assumable)),
        "case refused-updates-change-nothing: expected the created configData, got {read_back}"
    );
}
