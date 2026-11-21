// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a deployment package named by an AMAZON S3 LOCATION: what a write
//! that names one is served or refused, which bytes the package ends up being, and what
//! RUNNING those bytes answers.
//!
//! The RULE asserted here, over a table of inputs for each of the three writes that store
//! such a package: **Lambda READS the package out of the Amazon S3 object the write names
//! ("For a .zip file archive, the code property specifies the location of the .zip file",
//! `CreateFunction`; "an Amazon S3 location", `LayerVersionContentInput`), so the object's
//! own bytes are the package — `CodeSize` and `CodeSha256` are measured from them — and a
//! write whose object Amazon S3 will not hand over is refused `InvalidParameterValueException`
//! (HTTP 400) reporting the code and the message Amazon S3 answered, with nothing stored.**
//! A bucket Amazon S3 holds no record of is one such answer (`NoSuchBucket`), and an object it
//! DOES hand over that is no readable .zip archive at all is refused too, with the sentence
//! Lambda words for an archive it could not unzip.
//!
//! Those bytes are the function's FROM THEN ON, which the second half of this file asserts
//! through the data plane: a `RequestResponse` `Invoke` runs the handler they hold and keeps
//! running it however the object is changed or deleted afterwards, and a stored package that
//! yields no module for the configured `Handler` is the runtime's own import failure rather
//! than the modelled refusal for an archive that could not be unzipped. See the comment block
//! above those suites.
//!
//! The rows differ along what the rule generalises over: whether the object is there, WHICH
//! part of the location is absent (the key, the bucket), the shape and the size of the key
//! and of the object, whether the object is an archive, and which of the three writes names
//! the location.
//!
//! Amazon S3 is mounted BESIDE Lambda on one engine, and every object read here is written
//! through S3's own `CreateBucket`/`PutObject` — so the digest asserted is the digest of the
//! bytes that bucket holds rather than one this test handed Lambda. The Lambda-side harness
//! (the `Answer` wrapper, `json_call` and `package_bytes`) is the one `function_lifecycle.rs`
//! publishes.
//!
//! Amazon S3 is mounted BESIDE Lambda on one engine, and every object read here is written
//! through S3's own `CreateBucket`/`PutObject` — so the digest asserted is the digest of the
//! bytes that bucket holds rather than one this test handed Lambda. The Lambda-side harness
//! (the `Answer` wrapper, `json_call` and `package_bytes`) is the one `function_lifecycle.rs`
//! publishes.

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, package_bytes, zip_file,
    Answer, REGION, ROLE,
};
// The archive writer a sibling suite publishes, which takes the entries of a package as
// (path, source) pairs — so the packages these suites upload are written by one writer rather
// than by a second copy of it.
use crate::node_invoke_run::package as handler_package;

/// The error code every refusal in this suite carries, and its status.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
const REFUSAL_STATUS: u16 = 400;

/// The bucket this suite writes its packages to.
const BUCKET: &str = "deployment-packages";

/// A bucket no `CreateBucket` is made for, so this emulator's Amazon S3 holds no record of
/// it at all and answers `NoSuchBucket` for the read.
const UNHELD_BUCKET: &str = "packages-of-another-account";

/// The bytes of a .zip archive holding one entry of `length` bytes — a real archive, which
/// is what Lambda unzips the package it stores out of. Written in memory, so the object the
/// bucket holds is exactly these bytes.
pub(crate) fn archive_bytes(length: usize) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer.start_file("index.py", options).expect("a new entry");
        writer
            .write_all(&package_bytes(length))
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// A SigV4 credential scope naming Amazon S3 in the SAME Region the Lambda harness signs
/// for. The signature is never checked: the router has request authentication off.
fn s3_authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Lambda and Amazon S3 on one engine, the shape `core/router/src/lib.rs` mounts them in:
/// the object a package names is read out of S3's own store, so it is written through S3's
/// own surface.
pub(crate) fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// One path-style Amazon S3 request, answered as it came back — the one place a suite of
/// this crate builds an Amazon S3 request, so a request that must succeed ([`s3_ok`]) and one
/// whose refusal is the point cannot disagree about how it is addressed or signed.
pub(crate) async fn s3_request(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", s3_authorization())
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: answered,
    }
}

/// One path-style Amazon S3 request that must be served.
pub(crate) async fn s3_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &[u8],
) {
    let answer = s3_request(router, method, uri, body).await;
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: S3 {method} {uri} must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// Create [`BUCKET`] through Amazon S3's own `CreateBucket`, so the store holds the bucket
/// a row's location names whether or not that row writes an object into it.
async fn create_bucket(router: &MultiServiceRouter, case: &str) {
    s3_ok(router, case, "PUT", &format!("/{BUCKET}"), &[]).await;
}

/// Put `bytes` under `key` in [`BUCKET`], through Amazon S3's own `PutObject`.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, bytes: &[u8]) {
    s3_ok(router, case, "PUT", &format!("/{BUCKET}/{key}"), bytes).await;
}

/// The base64 SHA-256 of `bytes`, computed here so the test never reads the digest it is
/// checking out of the answer it is checking.
pub(crate) fn digest_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(bytes))
}

/// The sentence a write is refused with for an object Amazon S3 answered `code`/`message`
/// about, as AWS Lambda words it.
fn refusal(code: &str, message: &str) -> String {
    format!("Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}")
}

/// The refusal for a key the bucket holds nothing under: Amazon S3's `NoSuchKey` and the
/// message it answers it with.
fn no_such_key_refusal() -> String {
    refusal("NoSuchKey", "The specified key does not exist.")
}

/// The refusal for a bucket Amazon S3 holds no record of: its `NoSuchBucket` and the message
/// it answers it with.
fn no_such_bucket_refusal() -> String {
    refusal("NoSuchBucket", "The specified bucket does not exist")
}

/// The sentence a write is refused with for an object that is no readable .zip archive, as
/// AWS Lambda words it.
const COULD_NOT_UNZIP: &str =
    "Could not unzip uploaded file. Please check your file, then try to upload again.";

/// The `message` of a wire error, under either casing the JSON envelopes use.
///
/// `pub(crate)` because every suite reading a refusal of an Amazon S3 location back reads it
/// this one way (`tests/all/s3_pinned_object_version_refusal.rs`).
pub(crate) fn message(answer: &Answer) -> Option<String> {
    answer.string("message").or_else(|| answer.string("Message"))
}

/// A member of a JSON document, by pointer. `pub(crate)` for the same reason
/// [`message`] is.
pub(crate) fn member(document: &Value, pointer: &str) -> Option<Value> {
    document.pointer(pointer).cloned()
}

/// `GetFunction` on a function name.
async fn get_function(router: &MultiServiceRouter, name: &str) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `UpdateFunctionCode` on a function name.
async fn update_function_code(
    router: &MultiServiceRouter,
    name: &str,
    body: &Value,
) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/code");
    json_call(router, "PUT", &uri, body).await
}

/// `PublishLayerVersion` on a layer name.
pub(crate) async fn publish_layer_version(
    router: &MultiServiceRouter,
    name: &str,
    body: &Value,
) -> Answer {
    let uri = format!("/2018-10-31/layers/{name}/versions");
    json_call(router, "POST", &uri, body).await
}

/// `ListLayerVersions` on a layer name.
async fn list_layer_versions(router: &MultiServiceRouter, name: &str) -> Answer {
    let uri = format!("/2018-10-31/layers/{name}/versions");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `GetLayerVersion` on a layer name and a version number.
pub(crate) async fn get_layer_version(
    router: &MultiServiceRouter,
    name: &str,
    version: i64,
) -> Answer {
    let uri = format!("/2018-10-31/layers/{name}/versions/{version}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// One row of the create table: a function whose `Code` names an Amazon S3 location.
struct CreateRow {
    case: &'static str,
    function: &'static str,
    /// The key the location names.
    key: &'static str,
    /// The bytes to write under that key first — `None` writes nothing, so the location
    /// names an object the bucket does not hold.
    object: Option<Vec<u8>>,
    /// The bucket the location names, when it is not the one this suite writes to.
    bucket: Option<&'static str>,
    /// The sentence the write must be refused with, or `None` when it must be served.
    refusal: Option<String>,
}

fn create_rows() -> Vec<CreateRow> {
    vec![
        CreateRow {
            case: "an object the bucket holds",
            function: "from-held-object",
            key: "worker.zip",
            object: Some(archive_bytes(96)),
            bucket: None,
            refusal: None,
        },
        CreateRow {
            case: "an object of another size, under a nested key",
            function: "from-nested-key",
            key: "builds/2026/01/worker-v2.zip",
            object: Some(archive_bytes(4_096)),
            bucket: None,
            refusal: None,
        },
        CreateRow {
            case: "a key the bucket holds nothing under",
            function: "from-absent-key",
            key: "never-uploaded.zip",
            object: None,
            bucket: None,
            refusal: Some(no_such_key_refusal()),
        },
        CreateRow {
            case: "a key of another shape the bucket holds nothing under",
            function: "from-absent-nested-key",
            key: "builds/2026/01/never-uploaded.zip",
            object: None,
            bucket: None,
            refusal: Some(no_such_key_refusal()),
        },
        CreateRow {
            case: "a bucket Amazon S3 holds no record of",
            function: "from-unheld-bucket",
            key: "worker.zip",
            object: None,
            bucket: Some(UNHELD_BUCKET),
            refusal: Some(no_such_bucket_refusal()),
        },
        CreateRow {
            case: "an object that is no archive at all",
            function: "from-plain-object",
            key: "not-an-archive.zip",
            object: Some(package_bytes(96)),
            bucket: None,
            refusal: Some(COULD_NOT_UNZIP.to_string()),
        },
        CreateRow {
            case: "an empty object, which is no archive either",
            function: "from-empty-object",
            key: "empty.zip",
            object: Some(Vec::new()),
            bucket: None,
            refusal: Some(COULD_NOT_UNZIP.to_string()),
        },
    ]
}

/// RULE: `CreateFunction` reads the package out of the Amazon S3 object the `Code` names —
/// so the function's `CodeSize` and `CodeSha256` are the object's own — and is refused
/// `InvalidParameterValueException` with what Amazon S3 answered, or with the sentence for an
/// object it cannot unzip, with no function created.
#[tokio::test]
async fn a_create_reads_the_package_out_of_the_object_its_code_names() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the table's bucket").await;

    for row in create_rows() {
        if let Some(bytes) = &row.object {
            put_object(&router, row.case, row.key, bytes).await;
        }
        let created = create_function(
            &router,
            &json!({
                "FunctionName": row.function,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": {
                    "S3Bucket": row.bucket.unwrap_or(BUCKET),
                    "S3Key": row.key,
                },
            }),
        )
        .await;

        // An object Amazon S3 will not hand over, and one that is no archive, are each
        // refused — and nothing is created.
        if let Some(expected) = &row.refusal {
            assert_eq!(
                created.status, REFUSAL_STATUS,
                "case {:?}: expected {REFUSAL_STATUS}, got {} {}",
                row.case,
                created.status,
                String::from_utf8_lossy(&created.body)
            );
            assert!(
                created.error_code().contains(INVALID_PARAMETER),
                "case {:?}: expected {INVALID_PARAMETER}, got {:?}",
                row.case,
                created.error_code()
            );
            let worded = message(&created);
            assert_eq!(
                worded.as_deref(),
                Some(expected.as_str()),
                "case {:?}: expected {expected:?}, got {worded:?}",
                row.case
            );
            let read = get_function(&router, row.function).await;
            assert_eq!(
                read.status, 404,
                "case {:?}: expected no function to have been created, got {} {}",
                row.case,
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            continue;
        }

        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201, got {} {}",
            row.case,
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        // The size and the digest the function reports are the OBJECT's, computed here from
        // the bytes the bucket holds.
        let bytes = row.object.clone().unwrap_or_default();
        let expected_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let expected_digest = Some(digest_of(&bytes));
        for (surface, document) in [
            ("the create's own answer", created.json()),
            (
                "GetFunction",
                member(&get_function(&router, row.function).await.json(), "/Configuration")
                    .unwrap_or(Value::Null),
            ),
        ] {
            let size = member(&document, "/CodeSize").and_then(|size| size.as_i64());
            assert_eq!(
                size,
                Some(expected_size),
                "case {:?} on {surface}: expected CodeSize {expected_size}, got {size:?}",
                row.case
            );
            let reported = member(&document, "/CodeSha256")
                .and_then(|digest| digest.as_str().map(str::to_string));
            assert_eq!(
                reported, expected_digest,
                "case {:?} on {surface}: expected CodeSha256 {expected_digest:?}, got \
                 {reported:?}",
                row.case
            );
        }
    }
}

/// One row of the code-update table.
struct UpdateRow {
    case: &'static str,
    key: &'static str,
    /// The bytes to write under that key first, or `None` for a key the bucket holds
    /// nothing under.
    object: Option<Vec<u8>>,
}

/// RULE: `UpdateFunctionCode` follows a new Amazon S3 location the same way — the object's
/// bytes become the function's package, and an object the bucket does not hold is refused
/// with the function's code left exactly as it was.
#[tokio::test]
async fn a_code_update_follows_the_new_location_the_same_way() {
    let rows = [
        UpdateRow {
            case: "an object the bucket holds",
            key: "updated.zip",
            object: Some(archive_bytes(512)),
        },
        UpdateRow {
            case: "an object of another size",
            key: "updated-again.zip",
            object: Some(archive_bytes(1_024)),
        },
        UpdateRow {
            case: "a key the bucket holds nothing under",
            key: "never-updated.zip",
            object: None,
        },
    ];

    for row in rows {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        create_bucket(&router, row.case).await;

        // The function starts from an inline package, so the digest it carries before the
        // update is one this test knows and the update's effect is visible.
        let original = package_bytes(64);
        let created = create_function(
            &router,
            &json!({
                "FunctionName": "updated-from-s3",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD.encode(&original),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: the function under test must be created, got {} {}",
            row.case,
            created.status,
            String::from_utf8_lossy(&created.body)
        );

        // The create answers the function `Pending`, and Lambda refuses a modification of a
        // function in that window; the transition is owed by the CLOCK, so the due instant is
        // moved into the past rather than waited out.
        make_activation_due(&engine);

        let bytes = row.object.clone();
        if let Some(bytes) = &bytes {
            put_object(&router, row.case, row.key, bytes).await;
        }
        let updated = update_function_code(
            &router,
            "updated-from-s3",
            &json!({ "S3Bucket": BUCKET, "S3Key": row.key }),
        )
        .await;

        let expected_digest = match &bytes {
            Some(bytes) => {
                assert_eq!(
                    updated.status, 200,
                    "case {:?}: expected 200, got {} {}",
                    row.case,
                    updated.status,
                    String::from_utf8_lossy(&updated.body)
                );
                digest_of(bytes)
            }
            None => {
                assert_eq!(
                    updated.status, REFUSAL_STATUS,
                    "case {:?}: expected {REFUSAL_STATUS}, got {} {}",
                    row.case,
                    updated.status,
                    String::from_utf8_lossy(&updated.body)
                );
                let worded = message(&updated);
                assert_eq!(
                    worded.as_deref(),
                    Some(no_such_key_refusal().as_str()),
                    "case {:?}: expected {:?}, got {worded:?}",
                    row.case,
                    no_such_key_refusal()
                );
                // Nothing changed: the function still holds the package it was created with.
                digest_of(&original)
            }
        };

        let read = get_function(&router, "updated-from-s3").await;
        let reported = member(&read.json(), "/Configuration/CodeSha256")
            .and_then(|digest| digest.as_str().map(str::to_string));
        assert_eq!(
            reported.as_deref(),
            Some(expected_digest.as_str()),
            "case {:?}: expected CodeSha256 {expected_digest:?}, got {reported:?}",
            row.case
        );
    }
}

/// One row of the layer-publish table.
struct PublishRow {
    case: &'static str,
    /// The layer the version is published under.
    layer: &'static str,
    /// The key the location names.
    key: &'static str,
    /// The bytes to write under that key first, or `None` for a key the bucket holds
    /// nothing under.
    object: Option<Vec<u8>>,
    /// The sentence the publish must be refused with, or `None` when it must be served.
    refusal: Option<String>,
}

/// RULE: `PublishLayerVersion` follows the Amazon S3 location its `Content` names the same
/// way — the object's bytes are the archive it reports a size and a digest for, and an object
/// the bucket does not hold, or one that is no archive at all, is refused with no version
/// published.
#[tokio::test]
async fn a_layer_publish_follows_the_location_its_content_names() {
    let rows = vec![
        PublishRow {
            case: "an object the bucket holds",
            layer: "held-archive",
            key: "layer.zip",
            object: Some(archive_bytes(256)),
            refusal: None,
        },
        PublishRow {
            case: "an object of another size, under a nested key",
            layer: "nested-archive",
            key: "layers/deps/helpers.zip",
            object: Some(archive_bytes(2_048)),
            refusal: None,
        },
        PublishRow {
            case: "a key the bucket holds nothing under",
            layer: "absent-archive",
            key: "never-published.zip",
            object: None,
            refusal: Some(no_such_key_refusal()),
        },
        PublishRow {
            case: "an object that is no archive at all",
            layer: "plain-object",
            key: "not-an-archive.zip",
            object: Some(package_bytes(256)),
            refusal: Some(COULD_NOT_UNZIP.to_string()),
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the table's bucket").await;

    for row in rows {
        let (case, layer, key) = (row.case, row.layer, row.key);
        let expected_refusal = row.refusal;
        let bytes = row.object;
        if let Some(bytes) = &bytes {
            put_object(&router, case, key, bytes).await;
        }
        let published = publish_layer_version(
            &router,
            layer,
            &json!({ "Content": { "S3Bucket": BUCKET, "S3Key": key } }),
        )
        .await;

        if let Some(expected) = &expected_refusal {
            assert_eq!(
                published.status, REFUSAL_STATUS,
                "case {case:?}: expected {REFUSAL_STATUS}, got {} {}",
                published.status,
                String::from_utf8_lossy(&published.body)
            );
            assert!(
                published.error_code().contains(INVALID_PARAMETER),
                "case {case:?}: expected {INVALID_PARAMETER}, got {:?}",
                published.error_code()
            );
            let worded = message(&published);
            assert_eq!(
                worded.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {worded:?}"
            );
            // Nothing was published: the layer holds no version at all.
            let listed = list_layer_versions(&router, layer).await;
            let versions = member(&listed.json(), "/LayerVersions")
                .and_then(|versions| versions.as_array().map(Vec::len))
                .unwrap_or_default();
            assert_eq!(
                versions, 0,
                "case {case:?}: expected no version to have been published, got {}",
                String::from_utf8_lossy(&listed.body)
            );
            continue;
        }
        let bytes = bytes.unwrap_or_default();
        let bytes = &bytes;

        assert_eq!(
            published.status, 201,
            "case {case:?}: expected 201, got {} {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        let expected_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let expected_digest = digest_of(bytes);
        let read = get_layer_version(&router, layer, 1).await;
        for (surface, document) in [
            ("the publish's own answer", published.json()),
            ("GetLayerVersion", read.json()),
        ] {
            let reported_size = member(&document, "/Content/CodeSize").and_then(|size| size.as_i64());
            assert_eq!(
                reported_size,
                Some(expected_size),
                "case {case:?} on {surface}: expected CodeSize {expected_size}, got \
                 {reported_size:?}"
            );
            let reported = member(&document, "/Content/CodeSha256")
                .and_then(|digest| digest.as_str().map(str::to_string));
            assert_eq!(
                reported.as_deref(),
                Some(expected_digest.as_str()),
                "case {case:?} on {surface}: expected CodeSha256 {expected_digest:?}, got \
                 {reported:?}"
            );
        }
    }
}

// What the WRITES above store, the RUN below spends: a package read out of an Amazon S3
// object is one Lambda then OWNS, so an invocation runs those bytes and keeps running them
// however the object changes afterwards.
//
// The RULE these three suites assert: **the call that writes a package reads the Amazon S3
// object its location names once, and the function owns those bytes from then on — so a
// `RequestResponse` `Invoke` runs the handler held in them and answers 200 with what that run
// produced, and nothing re-reads the object at invocation time, whatever becomes of it.** The
// last suite asserts the ERROR CLASS the same ownership implies: a stored package that yields
// no module for the configured `Handler` is a failure of the runtime's INITIALIZATION phase —
// the served 200 carrying that runtime's own `Runtime.ImportModuleError` document and the
// `X-Amz-Function-Error: Unhandled` signal — while the modelled `InvalidZipFileException`
// (HTTP 502, "Lambda could not unzip the deployment package") is reserved for stored bytes
// that are no readable .zip archive.
//
// The rows differ along what the rule generalises over: which runtime the package is written
// for and in which module system, what the handler computes, how many entries the archive
// holds, the shape of the key the location names, which write stored the package
// (`CreateFunction` or `UpdateFunctionCode`), and what happens to the object afterwards
// (deleted, replaced with another package, left alone).

/// The header `Invoke`'s `InvocationType` is bound to (`httpHeader: X-Amz-Invocation-Type`),
/// and the synchronous value, which RUNS the function.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const REQUEST_RESPONSE: &str = "RequestResponse";

/// The two headers a synchronous invocation reports a run under: the version it ran
/// (`Invoke$ExecutedVersion`, `httpHeader: X-Amz-Executed-Version`) and the failure signal
/// (`Invoke$FunctionError`, `httpHeader: X-Amz-Function-Error`).
const EXECUTED_VERSION_HEADER: &str = "x-amz-executed-version";
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The unpublished version an invocation stating no qualifier resolves to, which is what
/// `X-Amz-Executed-Version` then names.
const LATEST: &str = "$LATEST";

/// The `FunctionError` signal of a run that ended in an error the function did not handle.
const UNHANDLED: &str = "Unhandled";

/// The `errorType` a runtime reports when it cannot import the module the configured
/// `Handler` names — the class AWS's own managed runtimes raise in the initialization phase.
const IMPORT_MODULE_ERROR: &str = "Runtime.ImportModuleError";

/// The error this service answers for stored bytes that are no readable .zip archive, and the
/// status its model gives it (`InvalidZipFileException`, errorKind server, httpError 502).
const INVALID_ZIP_FILE: &str = "InvalidZipFileException";
const INVALID_ZIP_FILE_STATUS: u16 = 502;

/// How long one invocation of these suites' functions is given, as the function's own
/// `Timeout`: far past what any of these handlers needs on any host, because what is under
/// test is WHICH bytes ran and not how fast an interpreter gets through them.
const RUN_TIMEOUT_SECONDS: i64 = 60;

/// Remove `key` from [`BUCKET`] through Amazon S3's own `DeleteObject`, and check the object
/// really is gone — the premise of every row that then invokes a function whose package came
/// out of an object nobody holds any more.
async fn delete_object(router: &MultiServiceRouter, case: &str, key: &str) {
    s3_ok(router, case, "DELETE", &format!("/{BUCKET}/{key}"), &[]).await;
    let read = s3_request(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: the object must be gone from the bucket, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
}

/// `CreateFunction` for a `Zip` function whose `Code` names the Amazon S3 location
/// [`BUCKET`]/`key` and nothing else about the package.
async fn create_from_object(
    router: &MultiServiceRouter,
    name: &str,
    runtime: &str,
    handler: &str,
    key: &str,
) -> Answer {
    create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": runtime,
            "Handler": handler,
            "Timeout": RUN_TIMEOUT_SECONDS,
            "Code": { "S3Bucket": BUCKET, "S3Key": key },
        }),
    )
    .await
}

/// `Invoke` with `InvocationType=RequestResponse` on the function `name`.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    crate::function_lifecycle::send(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        "application/json",
        serde_json::to_vec(event).expect("the event serializes"),
        &[(INVOCATION_TYPE_HEADER, REQUEST_RESPONSE.to_string())],
    )
    .await
}

/// The `CodeSha256` Lambda reports for the function's unpublished version.
async fn reported_digest(router: &MultiServiceRouter, name: &str) -> Option<String> {
    member(
        &get_function(router, name).await.json(),
        "/Configuration/CodeSha256",
    )
    .and_then(|digest| digest.as_str().map(str::to_string))
}

/// Assert that `answer` is a RUN that was served, whose payload is `expected`.
fn assert_ran(case: &str, answer: &Answer, expected: &Value) {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the run to be answered 200, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.header(FUNCTION_ERROR_HEADER),
        None,
        "case {case:?}: expected no function error, got {:?}",
        answer.header(FUNCTION_ERROR_HEADER)
    );
    assert_eq!(
        answer.header(EXECUTED_VERSION_HEADER).as_deref(),
        Some(LATEST),
        "case {case:?}: expected the executed version {LATEST}, got {:?}",
        answer.header(EXECUTED_VERSION_HEADER)
    );
    let payload = answer.json();
    assert_eq!(
        &payload, expected,
        "case {case:?}: expected the handler's own value {expected}, got {payload}"
    );
}

/// One row of the create-and-run table: the package the bucket holds, the configuration the
/// function names it under, and what invoking it must answer.
struct RunRow {
    case: &'static str,
    function: &'static str,
    /// The Amazon S3 key the `Code` location names.
    key: &'static str,
    runtime: &'static str,
    handler: &'static str,
    /// The archive's entries, as (path, source) pairs.
    entries: &'static [(&'static str, &'static str)],
    /// The event the invocation carries.
    event: Value,
    /// What the handler returns for that event.
    expected: Value,
}

fn run_rows() -> Vec<RunRow> {
    vec![
        RunRow {
            case: "a Python package under a flat key",
            function: "s3-flat-python",
            key: "worker.zip",
            runtime: "python3.12",
            handler: "lambda_function.lambda_handler",
            entries: &[(
                "lambda_function.py",
                "def lambda_handler(event, context):\n    return {'stage': 'alpha'}\n",
            )],
            event: json!({}),
            expected: json!({"stage": "alpha"}),
        },
        RunRow {
            case: "a Python package of another module name, under a nested key",
            function: "s3-nested-python",
            key: "builds/2026/07/app-rev.zip",
            runtime: "python3.13",
            handler: "app.run",
            entries: &[(
                "app.py",
                "def run(event, context):\n    return {'doubled': event['n'] * 2}\n",
            )],
            event: json!({"n": 21}),
            expected: json!({"doubled": 42}),
        },
        RunRow {
            case: "a Python package of several modules, the handler importing a sibling",
            function: "s3-multi-module-python",
            key: "releases/bundle.with.dots.zip",
            runtime: "python3.12",
            handler: "lambda_function.lambda_handler",
            entries: &[
                (
                    "lambda_function.py",
                    "import helper\n\n\ndef lambda_handler(event, context):\n    \
                     return {'joined': helper.join(event['parts'])}\n",
                ),
                ("helper.py", "def join(parts):\n    return '-'.join(parts)\n"),
            ],
            event: json!({"parts": ["ship", "it"]}),
            expected: json!({"joined": "ship-it"}),
        },
        RunRow {
            case: "a node ES module package",
            function: "s3-node-esm",
            key: "node/handler.zip",
            runtime: "nodejs20.x",
            handler: "index.handler",
            entries: &[(
                "index.mjs",
                "export const handler = async (event) => ({ keys: Object.keys(event).sort() });",
            )],
            event: json!({"b": 2, "a": 1}),
            expected: json!({"keys": ["a", "b"]}),
        },
    ]
}

/// RULE: an invocation of a function whose `Code` named an Amazon S3 location RUNS the
/// handler that object held, and the function reports the object's own digest.
#[tokio::test]
async fn an_invocation_runs_the_handler_the_referenced_object_held() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the table's bucket").await;

    for row in run_rows() {
        let case = row.case;
        let bytes = handler_package(row.entries);
        put_object(&router, case, row.key, &bytes).await;

        let created =
            create_from_object(&router, row.function, row.runtime, row.handler, row.key).await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected the create to be served 201, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        // The create answers the function `Pending`, and Lambda takes no invocation in that
        // window; the transition is owed by the CLOCK, so the due instant is brought forward
        // rather than waited out.
        make_activation_due(&engine);

        assert_ran(case, &invoke(&router, row.function, &row.event).await, &row.expected);

        // ... and the package that ran is the OBJECT's, which the digest the function reports
        // says: the expected value is computed here from the bytes the bucket was given.
        let expected_digest = digest_of(&bytes);
        let reported = reported_digest(&router, row.function).await;
        assert_eq!(
            reported.as_deref(),
            Some(expected_digest.as_str()),
            "case {case:?}: expected CodeSha256 {expected_digest:?}, got {reported:?}"
        );
    }
}

/// What happens to the Amazon S3 object after the package was read out of it.
#[derive(Clone, Copy)]
enum Afterwards {
    /// Nothing: the object stays where it is.
    Untouched,
    /// The object is deleted, so nobody holds it any more.
    Deleted,
    /// The object is replaced with a DIFFERENT package, whose handler answers differently.
    Replaced,
}

/// One row of the ownership table.
struct OwnershipRow {
    case: &'static str,
    function: &'static str,
    key: &'static str,
    afterwards: Afterwards,
}

/// RULE: the bytes the write read ARE the function's from then on — the object is not read
/// again at invocation time, so deleting it or replacing it changes neither the payload the
/// function answers nor the digest it reports.
#[tokio::test]
async fn the_package_stays_the_functions_after_the_object_changes() {
    let rows = [
        OwnershipRow {
            case: "the object is left where it is",
            function: "owned-untouched",
            key: "owned/untouched.zip",
            afterwards: Afterwards::Untouched,
        },
        OwnershipRow {
            case: "the object is deleted after the create read it",
            function: "owned-deleted",
            key: "owned/deleted.zip",
            afterwards: Afterwards::Deleted,
        },
        OwnershipRow {
            case: "the object is replaced with a different package",
            function: "owned-replaced",
            key: "owned/replaced.zip",
            afterwards: Afterwards::Replaced,
        },
    ];

    // The package every row is created from, and the one the "replaced" row puts under the
    // same key afterwards: two archives whose handlers answer differently, so a run of the
    // wrong bytes shows up in the payload.
    let held = handler_package(&[(
        "lambda_function.py",
        "def lambda_handler(event, context):\n    return {'generation': 'first'}\n",
    )]);
    let successor = handler_package(&[(
        "lambda_function.py",
        "def lambda_handler(event, context):\n    return {'generation': 'second'}\n",
    )]);
    let expected = json!({"generation": "first"});

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the table's bucket").await;

    for row in rows {
        let case = row.case;
        put_object(&router, case, row.key, &held).await;
        let created = create_from_object(
            &router,
            row.function,
            "python3.12",
            "lambda_function.lambda_handler",
            row.key,
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected the create to be served 201, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        make_activation_due(&engine);

        // The run BEFORE the object is touched, so each row starts from a function known to
        // answer.
        assert_ran(case, &invoke(&router, row.function, &json!({})).await, &expected);

        match row.afterwards {
            Afterwards::Untouched => {}
            Afterwards::Deleted => delete_object(&router, case, row.key).await,
            Afterwards::Replaced => put_object(&router, case, row.key, &successor).await,
        }

        assert_ran(case, &invoke(&router, row.function, &json!({})).await, &expected);
        let expected_digest = digest_of(&held);
        let reported = reported_digest(&router, row.function).await;
        assert_eq!(
            reported.as_deref(),
            Some(expected_digest.as_str()),
            "case {case:?}: expected the CodeSha256 of the package that was read, \
             {expected_digest:?}, got {reported:?}"
        );
    }
}

/// One row of the code-update-and-run table: the package a later `UpdateFunctionCode` points
/// the function at, and what invoking it must then answer.
struct UpdateRunRow {
    case: &'static str,
    function: &'static str,
    /// The key the create's location named.
    first_key: &'static str,
    /// The key the update's location names.
    second_key: &'static str,
    entries: &'static [(&'static str, &'static str)],
    expected: Value,
}

/// RULE: `UpdateFunctionCode` naming an Amazon S3 location reads that object the same way, so
/// the function then runs ITS handler — and owns those bytes just as the create's were, which
/// removing the object afterwards does not change.
#[tokio::test]
async fn a_code_update_by_reference_runs_the_new_objects_handler() {
    let rows = [
        UpdateRunRow {
            case: "a replacement package under a flat key",
            function: "updated-by-reference",
            first_key: "updates/v1.zip",
            second_key: "updates/v2.zip",
            entries: &[(
                "lambda_function.py",
                "def lambda_handler(event, context):\n    return {'revision': 2}\n",
            )],
            expected: json!({"revision": 2}),
        },
        UpdateRunRow {
            case: "a replacement package of several modules, under a nested key",
            function: "updated-by-nested-reference",
            first_key: "updates/nested/v1.zip",
            second_key: "updates/nested/2026/07/v2.zip",
            entries: &[
                (
                    "lambda_function.py",
                    "import helper\n\n\ndef lambda_handler(event, context):\n    \
                     return {'revision': helper.REVISION}\n",
                ),
                ("helper.py", "REVISION = 7\n"),
            ],
            expected: json!({"revision": 7}),
        },
    ];

    // The package every row is CREATED from, whose handler answers differently from the one
    // the update points at.
    let original = handler_package(&[(
        "lambda_function.py",
        "def lambda_handler(event, context):\n    return {'revision': 1}\n",
    )]);

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    create_bucket(&router, "the table's bucket").await;

    for row in rows {
        let case = row.case;
        put_object(&router, case, row.first_key, &original).await;
        let created = create_from_object(
            &router,
            row.function,
            "python3.12",
            "lambda_function.lambda_handler",
            row.first_key,
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected the create to be served 201, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        make_activation_due(&engine);
        assert_ran(
            case,
            &invoke(&router, row.function, &json!({})).await,
            &json!({"revision": 1}),
        );

        let replacement = handler_package(row.entries);
        put_object(&router, case, row.second_key, &replacement).await;
        let updated = update_function_code(
            &router,
            row.function,
            &json!({ "S3Bucket": BUCKET, "S3Key": row.second_key }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: expected the code update to be served 200, got {} {}",
            updated.status,
            String::from_utf8_lossy(&updated.body)
        );
        // "During this time, you can't modify the function, but you can still invoke it":
        // the update's own window is the clock's as well, so it is brought forward too.
        make_update_due(&engine);

        assert_ran(
            case,
            &invoke(&router, row.function, &json!({})).await,
            &row.expected,
        );
        let expected_digest = digest_of(&replacement);
        let reported = reported_digest(&router, row.function).await;
        assert_eq!(
            reported.as_deref(),
            Some(expected_digest.as_str()),
            "case {case:?}: expected CodeSha256 {expected_digest:?}, got {reported:?}"
        );

        // And the update's bytes are the function's too: the object they were read out of is
        // removed, and the function still runs them.
        delete_object(&router, case, row.second_key).await;
        assert_ran(
            case,
            &invoke(&router, row.function, &json!({})).await,
            &row.expected,
        );
    }
}

/// What the store holds for the function's package, in the error-class table below.
enum StoredPackage {
    /// Nothing: the `Code` named an Amazon S3 location this emulator resolved no object for,
    /// so no bytes were stored for the version.
    NoBytes,
    /// A readable archive holding the given (path, source) entries.
    Archive(&'static [(&'static str, &'static str)]),
    /// Bytes that are no readable .zip archive at all.
    NotAnArchive,
}

/// One row of the error-class table.
struct ClassRow {
    case: &'static str,
    function: &'static str,
    stored: StoredPackage,
    /// `Some(error type)` when the invocation must be a SERVED RUN reporting an
    /// initialization failure, `None` when it must be the modelled unzip refusal.
    import_failure: Option<&'static str>,
}

/// RULE: a stored package that yields NO module for the configured `Handler` is the runtime's
/// own import failure — HTTP 200, `X-Amz-Function-Error: Unhandled` and
/// `Runtime.ImportModuleError` — whether the archive simply carries no entry that handler
/// names or the store holds no bytes for the package at all; `InvalidZipFileException` (502,
/// "Lambda could not unzip the deployment package") is reserved for stored bytes that are no
/// readable archive.
///
/// The first row's function is created with a `Code` naming an Amazon S3 location on a router
/// carrying NO Amazon S3 at all, which is a read that shows nothing about the object: it
/// refuses nothing and stores no bytes, so the stored package is empty.
#[tokio::test]
async fn a_package_with_no_module_to_import_is_the_runtimes_import_failure() {
    let rows = [
        ClassRow {
            case: "a package the store holds no bytes for",
            function: "no-bytes-held",
            stored: StoredPackage::NoBytes,
            import_failure: Some(IMPORT_MODULE_ERROR),
        },
        ClassRow {
            case: "an archive holding no entry the handler names",
            function: "no-such-module",
            stored: StoredPackage::Archive(&[(
                "other.py",
                "def lambda_handler(event, context):\n    return {}\n",
            )]),
            import_failure: Some(IMPORT_MODULE_ERROR),
        },
        ClassRow {
            case: "bytes that are no readable archive at all",
            function: "not-an-archive",
            stored: StoredPackage::NotAnArchive,
            import_failure: None,
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    // Lambda ALONE on this engine: the first row's Amazon S3 read reaches no Amazon S3, so
    // nothing is shown about the object its location names.
    let router = crate::function_lifecycle::router_on(&engine);

    for row in rows {
        let case = row.case;
        let code = match row.stored {
            StoredPackage::NoBytes => json!({ "S3Bucket": BUCKET, "S3Key": "never-read.zip" }),
            StoredPackage::Archive(entries) => {
                json!({ "ZipFile": zip_file(&handler_package(entries)) })
            }
            StoredPackage::NotAnArchive => json!({ "ZipFile": zip_file(&package_bytes(64)) }),
        };
        let created = create_function(
            &router,
            &json!({
                "FunctionName": row.function,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "lambda_function.lambda_handler",
                "Timeout": RUN_TIMEOUT_SECONDS,
                "Code": code,
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: expected the create to be served 201, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        make_activation_due(&engine);

        let answered = invoke(&router, row.function, &json!({})).await;

        let Some(expected_type) = row.import_failure else {
            assert_eq!(
                answered.status, INVALID_ZIP_FILE_STATUS,
                "case {case:?}: expected the modelled {INVALID_ZIP_FILE_STATUS} for bytes that \
                 are no archive, got {} {}",
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
            assert!(
                answered.error_code().contains(INVALID_ZIP_FILE),
                "case {case:?}: expected {INVALID_ZIP_FILE}, got {:?}",
                answered.error_code()
            );
            continue;
        };

        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected an initialization failure to be a served 200, got {} {}",
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        assert_eq!(
            answered.header(FUNCTION_ERROR_HEADER).as_deref(),
            Some(UNHANDLED),
            "case {case:?}: expected the {UNHANDLED} signal, got {:?}",
            answered.header(FUNCTION_ERROR_HEADER)
        );
        let document = answered.json();
        let reported = document.get("errorType").and_then(Value::as_str);
        assert_eq!(
            reported,
            Some(expected_type),
            "case {case:?}: expected errorType {expected_type:?}, got {document}"
        );
        // The message is the RUNTIME's own, naming the module the `Handler` asked it to
        // import — never this service's own wording for a package it does not hold.
        let worded = document
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            worded.contains("lambda_function"),
            "case {case:?}: expected the import failure to name the module the handler named, \
             got {worded:?}"
        );
    }
}
