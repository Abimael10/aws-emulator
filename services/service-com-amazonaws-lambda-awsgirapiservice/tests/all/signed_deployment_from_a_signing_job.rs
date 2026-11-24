// SPDX-License-Identifier: CC0-1.0

//! A deployment package signed by a real code-signing JOB, deployed to a function under an
//! enforcing code-signing configuration — the handoff AWS documents for Lambda code signing,
//! driven end to end over the three services that make it.
//!
//! RULE under test: what a deployment to a function attached to a code-signing configuration
//! is verified against is the signature the PACKAGE ITSELF carries, and a package signed by a
//! signing profile the configuration's `AllowedPublishers` names passes those checks — so
//! `UpdateFunctionCode` answers 200, stores that package, settles `LastUpdateStatus` to
//! `Successful` and reports the profile version and the signing job it was signed by. A
//! package signed by a profile the configuration does not allow fails them, and under the
//! `Enforce` policy Lambda blocks the deployment with `CodeVerificationFailedException` (400)
//! naming the function, leaving the stored package alone.
//!
//! Only a cross-service suite can pin this as a client meets it: the artifact has to be
//! produced by a real `signer:StartSigningJob` of an object a real `s3:PutObject` wrote,
//! fetched back through `s3:GetObject` the way a client fetches `signedObject.s3`, and then
//! handed to Lambda as the `ZipFile` of an update — so what Lambda verifies is the signing
//! service's own artifact rather than anything this suite assembled.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, expected_code_sha256, function_arn, json_call, make_activation_due, zip_file,
    Answer, REGION, ROLE,
};
use crate::s3_package_source::{s3_ok, s3_request};

/// Where `CreateCodeSigningConfig` is bound (`POST /2020-04-22/code-signing-configs`).
const CODE_SIGNING_CONFIGS_URI: &str = "/2020-04-22/code-signing-configs";

/// The `CodeSigningPolicy` value that BLOCKS a deployment whose signature checks failed.
const POLICY_ENFORCE: &str = "Enforce";

/// The modelled error a blocked deployment answers, and its status
/// (`smithy_query(query="shape", shape="CodeVerificationFailedException")`: `httpError` 400).
const CODE_VERIFICATION_FAILED: &str = "CodeVerificationFailedException";
const BLOCKED_STATUS: u16 = 400;

/// A signing platform of the S3 code-signing-job workflow the signing service publishes, and
/// the certificate ARN a profile on it signs with.
const SIGNING_PLATFORM: &str = "AWSLambda-SHA384-ECDSA";
const CERTIFICATE_ARN: &str = "arn:aws:acm:us-east-1:000000000000:certificate/abc";

/// The buckets the signing job reads its source from and writes its artifact to.
const SOURCE_BUCKET: &str = "signing-source";
const DESTINATION_BUCKET: &str = "signing-destination";

/// The status a job reports while its completion is still owed by the clock, and the one a
/// job that signed reports.
const JOB_IN_PROGRESS: &str = "InProgress";
const JOB_SUCCEEDED: &str = "Succeeded";

/// The status a function's `LastUpdateStatus` reports while the update's completion is owed,
/// and the one it settles to.
const UPDATE_IN_PROGRESS: &str = "InProgress";
const UPDATE_SUCCESSFUL: &str = "Successful";

/// How long a case looks again for a state the clock owes before giving up — generous, so a
/// loaded machine does not fail it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// Lambda, the signing service and Amazon S3 on ONE engine, the shape
/// `core/router/src/lib.rs` mounts them in.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_signer_wallabyservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// One request to the signing service's front door: the credential scope names `signer`,
/// which is what routes it there.
async fn signer_call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let host = format!("signer.{REGION}.amazonaws.com");
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the signer request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=emulator-test-key/20260101/{REGION}/signer/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let (parts, answered) = router.dispatch(request).await.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: answered,
    }
}

/// `PutSigningProfile`, answering the `profileVersionArn` a signature of that profile names.
async fn put_signing_profile(router: &MultiServiceRouter, name: &str) -> String {
    let answer = signer_call(
        router,
        "PUT",
        &format!("/signing-profiles/{name}"),
        &json!({
            "platformId": SIGNING_PLATFORM,
            "signingMaterial": { "certificateArn": CERTIFICATE_ARN },
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "PutSigningProfile {name:?} must answer 200, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.string("profileVersionArn").unwrap_or_else(|| {
        panic!(
            "PutSigningProfile {name:?} must answer a profileVersionArn, got {}",
            String::from_utf8_lossy(&answer.body)
        )
    })
}

/// The bytes a signing job of `profile` produced for the source object `(key, version)`:
/// `StartSigningJob`, polled to its terminal status, then the artifact read back out of
/// Amazon S3 at exactly the location the job reported — what a client fetches.
async fn signed_artifact(
    router: &MultiServiceRouter,
    case: &str,
    profile: &str,
    key: &str,
    version: &str,
    token: &str,
) -> (Vec<u8>, String) {
    let started = signer_call(
        router,
        "POST",
        "/signing-jobs",
        &json!({
            "source": { "s3": { "bucketName": SOURCE_BUCKET, "key": key, "version": version } },
            "destination": { "s3": { "bucketName": DESTINATION_BUCKET, "prefix": "signed/" } },
            "profileName": profile,
            "clientRequestToken": token,
        }),
    )
    .await;
    assert_eq!(
        started.status,
        200,
        "case {case:?}: StartSigningJob must answer 200, got {} {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let job_id = started.string("jobId").unwrap_or_else(|| {
        panic!(
            "case {case:?}: StartSigningJob must answer a jobId, got {}",
            String::from_utf8_lossy(&started.body)
        )
    });

    // The terminal status is owed by the clock, so the case looks again until the job has
    // one rather than waiting a fixed time.
    let deadline = SystemTime::now() + SETTLE_DEADLINE;
    let described = loop {
        let described = signer_call(
            router,
            "GET",
            &format!("/signing-jobs/{job_id}"),
            &Value::Null,
        )
        .await;
        let status = described.string("status").unwrap_or_default();
        if status != JOB_IN_PROGRESS {
            assert_eq!(
                status, JOB_SUCCEEDED,
                "case {case:?}: expected the signing job to have succeeded, got {}",
                String::from_utf8_lossy(&described.body)
            );
            break described;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the signing job {job_id} never settled: {}",
            String::from_utf8_lossy(&described.body)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    // Fetched from the location the JOB reported, through Amazon S3's own front door.
    let reported = described.json()["signedObject"]["s3"].clone();
    let bucket = reported["bucketName"].as_str().unwrap_or_default().to_string();
    let object_key = reported["key"].as_str().unwrap_or_default().to_string();
    assert!(
        !bucket.is_empty() && !object_key.is_empty(),
        "case {case:?}: expected the job to report where it wrote the artifact, got {reported:?}"
    );
    let fetched = s3_request(router, "GET", &format!("/{bucket}/{object_key}"), &[]).await;
    assert_eq!(
        fetched.status,
        200,
        "case {case:?}: expected GetObject 200 for the signed artifact s3://{bucket}/{object_key}, \
         got {} {}",
        fetched.status,
        String::from_utf8_lossy(&fetched.body)
    );
    (fetched.body.to_vec(), job_id)
}

/// `CreateCodeSigningConfig` allowing exactly `publishers`, under the `Enforce` policy.
async fn create_code_signing_config(router: &MultiServiceRouter, publishers: &[String]) -> String {
    let allowed: Vec<Value> = publishers.iter().map(|arn| json!(arn)).collect();
    let answer = json_call(
        router,
        "POST",
        CODE_SIGNING_CONFIGS_URI,
        &json!({
            "AllowedPublishers": { "SigningProfileVersionArns": allowed },
            "CodeSigningPolicies": { "UntrustedArtifactOnDeployment": POLICY_ENFORCE },
            "Description": "signed deployments only",
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "CreateCodeSigningConfig must answer 201, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json()["CodeSigningConfig"]["CodeSigningConfigArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "CreateCodeSigningConfig must answer an ARN, got {}",
                String::from_utf8_lossy(&answer.body)
            )
        })
        .to_string()
}

/// `UpdateFunctionCode` deploying `package` inline.
async fn update_code(router: &MultiServiceRouter, function: &str, package: &[u8]) -> Answer {
    json_call(
        router,
        "PUT",
        &format!("/2015-03-31/functions/{function}/code"),
        &json!({ "ZipFile": zip_file(package) }),
    )
    .await
}

/// `GetFunctionConfiguration` on `function`.
async fn get_configuration(router: &MultiServiceRouter, function: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/configuration"),
        &Value::Null,
    )
    .await
}

/// Read the function's configuration until `LastUpdateStatus` settles, and answer what it
/// settled to — the condition, looked at again, rather than a wait of a fixed length.
async fn settled_update_status(router: &MultiServiceRouter, function: &str) -> String {
    let deadline = SystemTime::now() + SETTLE_DEADLINE;
    loop {
        let answer = get_configuration(router, function).await;
        let status = answer.string("LastUpdateStatus").unwrap_or_default();
        if status != UPDATE_IN_PROGRESS {
            return status;
        }
        assert!(
            SystemTime::now() < deadline,
            "the update of {function} never settled: {}",
            String::from_utf8_lossy(&answer.body)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A .zip source object holding one entry of `length` bytes, written by the archive writer a
/// sibling suite publishes.
fn source_archive(length: usize) -> Vec<u8> {
    crate::s3_package_source::archive_bytes(length)
}

/// RULE: a package a signing job of an ALLOWED publisher produced is deployed, and one a job
/// of another publisher produced is blocked — both artifacts coming from the same signing
/// service, over the same objects, so the only difference between them is who signed.
#[tokio::test]
async fn a_package_signed_by_an_allowed_publisher_deploys_and_one_signed_by_another_does_not() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);

    // The two publishers, and the configuration that trusts only the first.
    let allowed_profile = "Release_Profile";
    let other_profile = "Candidate_Profile";
    let allowed_version_arn = put_signing_profile(&router, allowed_profile).await;
    let other_version_arn = put_signing_profile(&router, other_profile).await;
    assert_ne!(
        allowed_version_arn, other_version_arn,
        "the two profiles must be two publishers"
    );
    let config = create_code_signing_config(&router, std::slice::from_ref(&allowed_version_arn))
        .await;

    // The source object both jobs sign, in a version-enabled bucket, and the destination the
    // artifacts are written to.
    s3_ok(&router, "setup", "PUT", &format!("/{SOURCE_BUCKET}"), &[]).await;
    s3_ok(
        &router,
        "setup",
        "PUT",
        &format!("/{SOURCE_BUCKET}?versioning"),
        b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
    )
    .await;
    s3_ok(
        &router,
        "setup",
        "PUT",
        &format!("/{DESTINATION_BUCKET}"),
        &[],
    )
    .await;
    let source_key = "handler.zip";
    let put = s3_request(
        &router,
        "PUT",
        &format!("/{SOURCE_BUCKET}/{source_key}"),
        &source_archive(512),
    )
    .await;
    assert_eq!(
        put.status,
        200,
        "PutObject of the source archive must answer 200, got {} {}",
        put.status,
        String::from_utf8_lossy(&put.body)
    );
    let source_version = put.header("x-amz-version-id").unwrap_or_default();
    assert!(
        !source_version.is_empty(),
        "the version-enabled source bucket must report a version id for the source object"
    );

    // The function, created from an UNSIGNED package before the enforcing configuration is
    // attached: the create's own deployment is verified the same way.
    let function = "signed-deployments";
    let created = create_function(
        &router,
        &json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&source_archive(64)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction must answer 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);
    let attached = json_call(
        &router,
        "PUT",
        &format!("/2020-06-30/functions/{function}/code-signing-config"),
        &json!({ "CodeSigningConfigArn": config }),
    )
    .await;
    assert_eq!(
        attached.status,
        200,
        "PutFunctionCodeSigningConfig must answer 200, got {} {}",
        attached.status,
        String::from_utf8_lossy(&attached.body)
    );
    let stored_before = created.string("CodeSha256").unwrap_or_default();

    // THE POSITIVE CONTROL: the artifact the allowed publisher's own signing job produced.
    let (signed, job_id) = signed_artifact(
        &router,
        "the allowed publisher",
        allowed_profile,
        source_key,
        &source_version,
        "token-allowed",
    )
    .await;
    let deployed = update_code(&router, function, &signed).await;
    assert_eq!(
        deployed.status,
        200,
        "expected 200 from UpdateFunctionCode for a package the allowed publisher signed, got {} {}",
        deployed.status,
        String::from_utf8_lossy(&deployed.body)
    );
    assert_eq!(
        deployed.string("CodeSha256"),
        Some(expected_code_sha256(&signed)),
        "expected the deployed artifact's own digest, got {}",
        String::from_utf8_lossy(&deployed.body)
    );
    assert_eq!(
        deployed.string("SigningProfileVersionArn"),
        Some(allowed_version_arn.clone()),
        "expected the function to report the profile version that signed the artifact, got {}",
        String::from_utf8_lossy(&deployed.body)
    );
    assert_eq!(
        deployed.string("SigningJobArn"),
        Some(format!(
            "arn:aws:signer:{REGION}:000000000000:/signing-jobs/{job_id}"
        )),
        "expected the function to report the signing job that signed the artifact, got {}",
        String::from_utf8_lossy(&deployed.body)
    );
    let settled = settled_update_status(&router, function).await;
    assert_eq!(
        settled, UPDATE_SUCCESSFUL,
        "expected the deployment to settle to {UPDATE_SUCCESSFUL}, got {settled:?}"
    );
    // The round trip: an independent read reports the artifact that was deployed and who
    // signed it.
    let read = get_configuration(&router, function).await;
    assert_eq!(
        read.string("CodeSha256"),
        Some(expected_code_sha256(&signed)),
        "expected a later read to report the deployed artifact, got {}",
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        read.string("SigningProfileVersionArn"),
        Some(allowed_version_arn.clone()),
        "expected a later read to report the publisher, got {}",
        String::from_utf8_lossy(&read.body)
    );
    assert_ne!(
        read.string("CodeSha256"),
        Some(stored_before.clone()),
        "expected the deployment to have replaced the package the function was created with"
    );

    // AND THE OTHER SIGNER: the same source object, signed by the publisher the
    // configuration does not allow, is the deployment Lambda blocks.
    let (mismatched, _) = signed_artifact(
        &router,
        "a publisher the configuration does not allow",
        other_profile,
        source_key,
        &source_version,
        "token-other",
    )
    .await;
    assert_ne!(
        mismatched, signed,
        "the two jobs must have produced two artifacts"
    );
    let refused = update_code(&router, function, &mismatched).await;
    assert_eq!(
        refused.status,
        BLOCKED_STATUS,
        "expected {BLOCKED_STATUS} from UpdateFunctionCode for a package no allowed publisher \
         signed, got {} {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert_eq!(
        refused.error_code(),
        CODE_VERIFICATION_FAILED,
        "expected {CODE_VERIFICATION_FAILED}, got {:?}",
        refused.error_code()
    );
    assert_eq!(
        refused.string("Message"),
        Some(format!(
            "Lambda cannot deploy the function. The function or layer might be signed using a \
             signature that the client is not configured to accept. Check the provided signature \
             for {}.",
            function_arn(function)
        )),
        "expected the refusal to name the function whose deployment was blocked, got {}",
        String::from_utf8_lossy(&refused.body)
    );

    // Nothing of the blocked deployment landed: the function still serves the artifact the
    // allowed publisher signed, and still reports that publisher.
    let after = get_configuration(&router, function).await;
    assert_eq!(
        after.string("CodeSha256"),
        Some(expected_code_sha256(&signed)),
        "expected the blocked deployment to leave the stored package alone, got {}",
        String::from_utf8_lossy(&after.body)
    );
    assert_eq!(
        after.string("SigningProfileVersionArn"),
        Some(allowed_version_arn),
        "expected the blocked deployment to leave the reported publisher alone, got {}",
        String::from_utf8_lossy(&after.body)
    );
}
