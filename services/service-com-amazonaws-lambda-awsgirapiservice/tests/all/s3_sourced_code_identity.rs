// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a deployment package named by an AMAZON S3 LOCATION: Amazon S3's own
//! `PutObject` and then `CreateFunction` / `UpdateFunctionCode` (`POST
//! /2015-03-31/functions`, `PUT /2015-03-31/functions/{FunctionName}/code`) and `Invoke` over
//! the router, with Amazon S3 mounted beside Lambda on ONE engine.
//!
//! The rule under test: a package named by an Amazon S3 location IS the object's own bytes —
//! `CodeSize` is their length and `CodeSha256` their base64 SHA-256, identical to what a
//! `ZipFile` deployment of the same bytes reports and pinned to the `S3ObjectVersion` when one
//! is given, and the function runs the files that object carried — while a location Amazon S3
//! will not serve refuses the call with Amazon S3's own code and sentence relayed, leaving the
//! function's previous code in place.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which operation stores the package (`CreateFunction`,
//! `UpdateFunctionCode`), how it names the bytes (inline, an Amazon S3 location, that location
//! pinned to one version of the object), how long the package is, which version of a key's
//! object is current, and what Amazon S3 answers about the location (an object, a key it holds
//! nothing under, a bucket it holds no record of).
//!
//! The suites that INVOKE run a handler, so they take the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::element;
use crate::event_source_mappings::{await_function_active, call, router_on, Answer};
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` every function of this suite is configured with.
const RUNTIME: &str = "python3.12";

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// The bucket every object of this suite is written to.
const BUCKET: &str = "deployment-packages";

/// Lambda and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// object this suite writes with Amazon S3's own `PutObject` is the object Lambda follows when
/// it stores a package named by that location.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// What one Amazon S3 request answered: its status, the version id it reported and its body.
struct Written {
    status: u16,
    version_id: Option<String>,
    body: String,
}

/// One request to Amazon S3, made as a CLIENT makes one: its protocol is `restXml`, so the
/// request is a method and a path and the answer is a document.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Written {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Written {
        status: parts.status.as_u16(),
        version_id: parts
            .headers
            .get("x-amz-version-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// Create the bucket every object goes into, with VERSIONING enabled so a key can hold more
/// than one object and a package can be pinned to one of them.
async fn given_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        created.status, 200,
        "CreateBucket must succeed, got {} {}",
        created.status, created.body
    );
    let versioned = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?versioning"),
        b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
    )
    .await;
    assert_eq!(
        versioned.status, 200,
        "PutBucketVersioning must succeed, got {} {}",
        versioned.status, versioned.body
    );
}

/// Write `bytes` under `key` through Amazon S3's own `PutObject`, answering the version id the
/// bucket gave that object.
async fn given_object(router: &MultiServiceRouter, key: &str, bytes: &[u8]) -> String {
    let written = s3(router, "PUT", &format!("/{BUCKET}/{key}"), bytes).await;
    assert_eq!(
        written.status, 200,
        "PutObject({key:?}) must succeed, got {} {}",
        written.status, written.body
    );
    written
        .version_id
        .unwrap_or_else(|| panic!("PutObject({key:?}) must report the object's version id"))
}

/// The base64 SHA-256 of `bytes`, which is what `CodeSha256` reports for a stored package:
/// "the SHA256 hash of the function's deployment package"
/// (`smithy_query(query="operation",operation="CreateFunction")`).
fn digest_of(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// A deployment package holding the probe handler and `marker`, so two packages of this suite
/// differ in their bytes and in what the function they deploy answers.
fn probe_package(marker: &str) -> Vec<u8> {
    package(&[("index.py", PROBE_SOURCE), ("marker.txt", marker)])
}

/// `CreateFunction` with the `Code` member `code` names, waited out to `Active`, answering the
/// configuration the store then holds.
async fn create_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    code: Value,
) -> Answer {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "Code": code,
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} must answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
    read_configuration(router, name).await
}

/// `GetFunctionConfiguration`, which is where the size and the digest of the stored package are
/// read back.
async fn read_configuration(router: &MultiServiceRouter, name: &str) -> Answer {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetFunctionConfiguration {name:?} must answer 200, got {} {}",
        read.status,
        read.text()
    );
    read
}

/// The size and the digest one configuration reports, as the pair this suite compares.
fn identity(configuration: &Answer) -> (i64, String) {
    let document = configuration.json();
    let size = document
        .get("CodeSize")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("a configuration must report CodeSize: {document}"));
    let digest = document
        .get("CodeSha256")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a configuration must report CodeSha256: {document}"))
        .to_string();
    (size, digest)
}

/// `Invoke`, answering what the handler returned.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({}),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: Invoke {name:?} must answer 200, got {} {}",
        answered.status,
        answered.text()
    );
    let failure = answered.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case:?}: the handler reads the files its own package carried, so the invocation \
         carries no function error, got {failure:?} with {}",
        answered.text()
    );
    answered.json()
}

/// A package named by an Amazon S3 location is the object's own bytes: the size and the digest
/// are measured from them and are the ones a `ZipFile` deployment of the same bytes reports,
/// for a package of each size class — and the function runs the files that object carried.
#[tokio::test]
async fn an_s3_sourced_package_reports_the_objects_own_size_and_digest() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    given_bucket(&router).await;
    let long_marker = "a longer marker\n".repeat(64);
    // (case, the marker the package carries, which is what makes the packages differ)
    let rows: [(&str, &str); 3] = [
        ("an ordinary package", "first"),
        ("a package with no marker text at all", ""),
        (
            "a package whose marker is longer than one line",
            long_marker.as_str(),
        ),
    ];
    for (case, marker) in rows {
        let bytes = probe_package(marker);
        let stem = case.replace(' ', "-");
        let key = format!("{stem}.zip");
        given_object(&router, &key, &bytes).await;
        let inline = format!("inline-{stem}");
        let sourced = format!("sourced-{stem}");
        let uploaded = create_function(
            &router,
            case,
            &inline,
            json!({ "ZipFile": base64::engine::general_purpose::STANDARD.encode(&bytes) }),
        )
        .await;
        let followed = create_function(
            &router,
            case,
            &sourced,
            json!({ "S3Bucket": BUCKET, "S3Key": key }),
        )
        .await;
        let expected = (bytes.len() as i64, digest_of(&bytes));
        let uploaded_identity = identity(&uploaded);
        let followed_identity = identity(&followed);
        assert_eq!(
            followed_identity, expected,
            "case {case:?}: the S3-sourced package's size and digest must be the object's own \
             {expected:?}, got {followed_identity:?}"
        );
        assert_eq!(
            uploaded_identity, followed_identity,
            "case {case:?}: a ZipFile deployment of the same bytes must report the same size \
             and digest, got {uploaded_identity:?} against {followed_identity:?}"
        );
        // ... and the function RUNS the files that object carried, which is the other half of
        // storing a package: the handler reads its own package back and reports what it holds.
        let ran = invoke(&router, case, &sourced).await;
        let expected_run = json!({
            "marker": marker,
            "own_source_bytes": PROBE_SOURCE.len(),
        });
        assert_eq!(
            ran, expected_run,
            "case {case:?}: the S3-sourced function must run the object's own files, got {ran}"
        );
        let uploaded_run = invoke(&router, case, &inline).await;
        assert_eq!(
            uploaded_run, ran,
            "case {case:?}: the two deployments of one package must answer alike, got \
             {uploaded_run} against {ran}"
        );
    }
}

/// A package pinned to an `S3ObjectVersion` is the bytes of THAT version of the object, and one
/// naming no version is the bytes of the object the key holds now.
#[tokio::test]
async fn an_s3_object_version_pins_the_deployed_package() {
    let router = emulator();
    given_bucket(&router).await;
    let key = "pinned.zip";
    let first = probe_package("first");
    let second = probe_package("second");
    let pinned_version = given_object(&router, key, &first).await;
    let current_version = given_object(&router, key, &second).await;
    assert_ne!(
        pinned_version, current_version,
        "a versioned bucket must give each write of one key its own version id, got \
         {pinned_version:?} twice"
    );
    // (case, the version the Code names, the bytes the function must be deployed from)
    let rows: [(&str, Option<&str>, &[u8]); 2] = [
        (
            "pinned to the first version",
            Some(pinned_version.as_str()),
            &first,
        ),
        ("naming no version at all", None, &second),
    ];
    for (case, version, bytes) in rows {
        let name = format!("pinned-{}", case.replace(' ', "-"));
        let mut code = json!({ "S3Bucket": BUCKET, "S3Key": key });
        if let Some(version) = version {
            code["S3ObjectVersion"] = json!(version);
        }
        let created = create_function(&router, case, &name, code).await;
        let expected = (bytes.len() as i64, digest_of(bytes));
        let reported = identity(&created);
        assert_eq!(
            reported, expected,
            "case {case:?}: the deployed package must be the named version's own bytes \
             {expected:?}, got {reported:?}"
        );
    }
}

/// An Amazon S3 location Amazon S3 will not serve refuses the update with its own code and
/// sentence relayed, and the function keeps the code it already had.
#[tokio::test]
async fn an_s3_location_amazon_s3_will_not_serve_refuses_the_update() {
    let router = emulator();
    given_bucket(&router).await;
    let deployed = probe_package("deployed");
    let key = "deployed.zip";
    given_object(&router, key, &deployed).await;
    let name = "keeps-its-previous-code";
    let created = create_function(
        &router,
        "the deployed function",
        name,
        json!({ "S3Bucket": BUCKET, "S3Key": key }),
    )
    .await;
    let before = identity(&created);
    assert_eq!(
        before,
        (deployed.len() as i64, digest_of(&deployed)),
        "the function must start out deployed from the object's own bytes, got {before:?}"
    );
    // (case, the bucket the update names, the key it names)
    let rows: [(&str, &str, &str); 2] = [
        ("a key the bucket holds nothing under", BUCKET, "absent.zip"),
        (
            "a bucket Amazon S3 holds no record of",
            "no-such-deployment-bucket",
            key,
        ),
    ];
    for (case, bucket, named_key) in rows {
        // What AMAZON S3 itself answers about that location, which is what the refusal relays:
        // the code it classified the read under and the sentence it worded.
        let read = s3(&router, "GET", &format!("/{bucket}/{named_key}"), b"").await;
        assert!(
            read.status >= 400,
            "case {case:?}: Amazon S3 must refuse the read, got {} {}",
            read.status,
            read.body
        );
        let code = element(&read.body, "Code");
        let message = element(&read.body, "Message");
        let refused = call(
            &router,
            "PUT",
            &format!("{FUNCTIONS_URI}/{name}/code"),
            &json!({ "S3Bucket": bucket, "S3Key": named_key }),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: UpdateFunctionCode must be refused 400, got {} {}",
            refused.status,
            refused.text()
        );
        assert!(
            refused.error_code().contains("InvalidParameterValueException"),
            "case {case:?}: the refusal must be InvalidParameterValueException, got {}",
            refused.error_code()
        );
        let expected = format!(
            "Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}"
        );
        let worded = refused
            .string("message")
            .or_else(|| refused.string("Message"));
        assert_eq!(
            worded.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: the refusal must relay what Amazon S3 answered, got {worded:?}"
        );
        let after = identity(&read_configuration(&router, name).await);
        assert_eq!(
            after, before,
            "case {case:?}: the refused update must leave the function's own package in place, \
             got {after:?} against {before:?}"
        );
    }
}

/// A valid Amazon S3 location in an UPDATE deploys that object's bytes, which is the same rule
/// the create follows.
#[tokio::test]
async fn an_update_from_an_s3_location_deploys_that_objects_bytes() {
    let router = emulator();
    given_bucket(&router).await;
    let first = probe_package("before the update");
    let second = probe_package("after the update");
    given_object(&router, "before.zip", &first).await;
    given_object(&router, "after.zip", &second).await;
    let name = "updated-from-s3";
    let created = create_function(
        &router,
        "the deployed function",
        name,
        json!({ "S3Bucket": BUCKET, "S3Key": "before.zip" }),
    )
    .await;
    let before = identity(&created);
    assert_eq!(
        before,
        (first.len() as i64, digest_of(&first)),
        "the function must start out deployed from the first object, got {before:?}"
    );
    let updated = call(
        &router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/code"),
        &json!({ "S3Bucket": BUCKET, "S3Key": "after.zip" }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionCode must answer 200, got {} {}",
        updated.status,
        updated.text()
    );
    let expected = (second.len() as i64, digest_of(&second));
    let reported = identity(&updated);
    assert_eq!(
        reported, expected,
        "the update must report the new object's own size and digest {expected:?}, got \
         {reported:?}"
    );
    await_function_active(&router, "the updated function", name).await;
    let read = identity(&read_configuration(&router, name).await);
    assert_eq!(
        read, expected,
        "the stored configuration must report the new object's own size and digest, got {read:?}"
    );
}

/// The probe every function of this suite runs: it reads the files its OWN deployment package
/// carried — the marker beside it and its own module source — and reports what they hold, so an
/// assertion about the deployed package is an assertion about what the function really runs.
const PROBE_SOURCE: &str = r#"
import os

TASK_ROOT = os.environ["LAMBDA_TASK_ROOT"]


def handler(event, context):
    with open(os.path.join(TASK_ROOT, "marker.txt")) as reading:
        marker = reading.read()
    with open(os.path.join(TASK_ROOT, "index.py"), "rb") as reading:
        source = reading.read()
    return {"marker": marker, "own_source_bytes": len(source)}
"#;
