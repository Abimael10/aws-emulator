// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH Amazon S3 API Lambda names when it refuses a write whose
//! package or layer archive is named by an Amazon S3 location: `CreateFunction` (`POST
//! /2015-03-31/functions`), `UpdateFunctionCode` (`PUT
//! /2015-03-31/functions/{FunctionName}/code`) and `PublishLayerVersion` (`POST
//! /2018-10-31/layers/{LayerName}/versions`), with Amazon S3 mounted beside Lambda on ONE
//! engine.
//!
//! The RULE asserted here: **the read Lambda makes of the named object is the VERSIONED
//! Amazon S3 API exactly when the location pins an object version ("To retrieve a specific
//! version, you must [...] use the versionId subresource", `GetObject`), and the
//! `InvalidParameterValueException` (HTTP 400) a refused read is reported as names the API
//! that read was made with — `GetObjectVersion` for a pinned version and `GetObject` for a
//! location naming none — around the code and the sentence Amazon S3 itself answered.** A
//! pinned version the bucket DOES hold is served, so the versioned read is a read and not a
//! refusal.
//!
//! The rows differ along what the rule generalises over: whether a version is pinned, WHICH
//! form the pinned token takes (one of the published shape the bucket never minted, the
//! `null` token a write made before versioning carries), what is missing about the location
//! (the version, the key, the bucket, nothing), and which of the three writes names the
//! location. The error code and the sentence are never written out here: each row reads them
//! out of Amazon S3's OWN answer to the same read, so only the API name and the fixed text
//! around it are this test's own.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::element;
use crate::function_lifecycle::{
    create_function, expected_code_sha256, json_call, make_activation_due, package_bytes,
    zip_file, Answer, ROLE,
};
// Lambda and Amazon S3 over one engine, a real .zip archive, the layer-publish call, and the
// two readers of an answer, as the sibling Amazon S3 package suite publishes them.
use crate::s3_package_source::{
    archive_bytes, emulator, member, message, publish_layer_version, s3_ok, s3_request,
};

/// The bucket every object of this suite is written to.
const BUCKET: &str = "pinned-package-objects";

/// A bucket no `CreateBucket` is made for, so Amazon S3 holds no record of it at all.
const ABSENT_BUCKET: &str = "no-such-package-bucket";

/// The key the archive is written under, and one the bucket holds nothing under.
const KEY: &str = "archive.zip";
const ABSENT_KEY: &str = "never-written.zip";

/// A version id of the SHAPE Amazon S3 publishes — alphanumerics, at least 32 characters —
/// that this bucket never minted, so a read pinned to it is a read of a version that does not
/// exist rather than a malformed argument.
const NEVER_MINTED_VERSION: &str = "nEveRmInTedVeRsIonIdThIrTyTwOcHaR";

/// The token Amazon S3 reports for a version written before any versioning state was set
/// ("objects stored in your bucket before you set the versioning state have a version ID of
/// null", Amazon S3 User Guide, *Using versioning in S3 buckets*). A bucket that was
/// versioning-enabled before its first write holds no such version.
const NULL_VERSION: &str = "null";

/// The error code and the status every refusal of this suite carries.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
const REFUSAL_STATUS: u16 = 400;

/// The function `UpdateFunctionCode` is driven against, and the layer the publishes go to.
const FUNCTION: &str = "pinned-package-function";
const LAYER: &str = "pinned-archive-layer";

/// The runtime and handler every function this suite creates is configured with. Nothing here
/// invokes, so the package only has to be stored.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// Which of the three writes that store a package from an Amazon S3 location is making the
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    Create,
    Update,
    Publish,
}

impl Write {
    /// The operation's own name, for the case text of every assertion.
    fn label(self) -> &'static str {
        match self {
            Self::Create => "CreateFunction",
            Self::Update => "UpdateFunctionCode",
            Self::Publish => "PublishLayerVersion",
        }
    }

    /// The status the write answers when the location is served.
    fn served_status(self) -> u16 {
        match self {
            // "SUCCESS_STATUS: 201" for both of the creating writes; an update answers 200.
            Self::Create | Self::Publish => 201,
            Self::Update => 200,
        }
    }

    /// Where the digest of the stored archive sits in this write's own answer.
    fn digest_pointer(self) -> &'static str {
        match self {
            Self::Create | Self::Update => "/CodeSha256",
            // A layer version reports its archive under `Content`
            // (`smithy_query(query="operation",operation="PublishLayerVersion")`).
            Self::Publish => "/Content/CodeSha256",
        }
    }
}

/// The three writes, so every row of every table below is driven through each of them.
const WRITES: [Write; 3] = [Write::Create, Write::Update, Write::Publish];

/// Create [`BUCKET`] with VERSIONING enabled, so a key can hold more than one version and a
/// location can pin one of them.
async fn given_versioned_bucket(router: &MultiServiceRouter, case: &str) {
    s3_ok(router, case, "PUT", &format!("/{BUCKET}"), &[]).await;
    s3_ok(
        router,
        case,
        "PUT",
        &format!("/{BUCKET}?versioning"),
        b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
    )
    .await;
}

/// Write `bytes` under [`KEY`] through Amazon S3's own `PutObject`, answering the version id
/// the bucket minted for that object.
async fn given_object(router: &MultiServiceRouter, case: &str, bytes: &[u8]) -> String {
    let written = s3_request(router, "PUT", &format!("/{BUCKET}/{KEY}"), bytes).await;
    assert!(
        (200..300).contains(&written.status),
        "case {case:?}: PutObject must succeed, got {} {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
    written
        .header("x-amz-version-id")
        .unwrap_or_else(|| panic!("case {case:?}: PutObject must report the minted version id"))
}

/// The `Code` member every write of this suite names, pinned to `version` when one is given.
fn location(bucket: &str, key: &str, version: Option<&str>) -> Value {
    let mut named = json!({ "S3Bucket": bucket, "S3Key": key });
    if let Some(version) = version {
        named["S3ObjectVersion"] = json!(version);
    }
    named
}

/// The Amazon S3 API a read of a location performs: the versioned one exactly when the
/// location PINS a version, and the whole object's own read otherwise.
fn read_call(version: Option<&str>) -> &'static str {
    match version {
        Some(_) => "GetObjectVersion",
        None => "GetObject",
    }
}

/// The sentence a write naming `version` is refused with for an object Amazon S3 answered
/// `code`/`message` about, as AWS Lambda words it: fixed text around the API the read was made
/// with and the two parts Amazon S3 itself worded.
fn expected_refusal(version: Option<&str>, code: &str, message: &str) -> String {
    format!(
        "Error occurred while {call}. S3 Error Code: {code}. S3 Error Message: {message}",
        call = read_call(version)
    )
}

/// What AMAZON S3 itself answers about the same read, as the code and the sentence it worded —
/// so a row asserts the API name and the fixed text around it and never Amazon S3's own
/// wording.
async fn what_amazon_s3_answers(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    version: Option<&str>,
) -> (String, String) {
    let uri = match version {
        Some(version) => format!("/{bucket}/{key}?versionId={version}"),
        None => format!("/{bucket}/{key}"),
    };
    let read = s3_request(router, "GET", &uri, &[]).await;
    assert!(
        read.status >= 400,
        "case {case:?}: Amazon S3 must refuse the read of {uri}, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let body = String::from_utf8_lossy(&read.body).to_string();
    (element(&body, "Code"), element(&body, "Message"))
}

/// Drive one of the three writes at the Amazon S3 location `bucket`/`key`, pinned to `version`
/// when one is given, answering what it answered.
///
/// `UpdateFunctionCode` needs a function to update: it is created from an inline package first,
/// and the `Pending` window the create opens is brought forward rather than waited out, because
/// Lambda refuses a modification inside it.
async fn perform(
    engine: &Arc<engine::InMemoryEngine>,
    router: &MultiServiceRouter,
    write: Write,
    bucket: &str,
    key: &str,
    version: Option<&str>,
) -> Answer {
    let named = location(bucket, key, version);
    match write {
        Write::Create => {
            create_function(
                router,
                &json!({
                    "FunctionName": FUNCTION,
                    "Role": ROLE,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Code": named,
                }),
            )
            .await
        }
        Write::Update => {
            let created = create_function(
                router,
                &json!({
                    "FunctionName": FUNCTION,
                    "Role": ROLE,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Code": { "ZipFile": zip_file(&package_bytes(64)) },
                }),
            )
            .await;
            assert_eq!(
                created.status,
                201,
                "the function under test must be created, got {} {}",
                created.status,
                String::from_utf8_lossy(&created.body)
            );
            make_activation_due(engine);
            json_call(
                router,
                "PUT",
                &format!("/2015-03-31/functions/{FUNCTION}/code"),
                &named,
            )
            .await
        }
        Write::Publish => {
            publish_layer_version(router, LAYER, &json!({ "Content": named })).await
        }
    }
}

/// One row of the refusal table: a location Amazon S3 will not serve.
struct RefusedRow {
    case: &'static str,
    /// The bucket the location names.
    bucket: &'static str,
    /// The key it names.
    key: &'static str,
    /// The version it pins, or `None` for a location naming none.
    version: Option<&'static str>,
}

/// RULE: a refused read of an Amazon S3 location is reported as `InvalidParameterValueException`
/// (HTTP 400) naming the Amazon S3 API the read was MADE with — the versioned one exactly when
/// the location pinned a version — around the code and the sentence Amazon S3 answered. Asserted
/// for each of the three writes that store a package from such a location.
#[tokio::test]
async fn a_refusal_names_the_amazon_s3_api_the_read_was_made_with() {
    let rows = [
        RefusedRow {
            case: "a version id of the published shape the bucket never minted",
            bucket: BUCKET,
            key: KEY,
            version: Some(NEVER_MINTED_VERSION),
        },
        RefusedRow {
            case: "the null version of a key written while versioning was enabled",
            bucket: BUCKET,
            key: KEY,
            version: Some(NULL_VERSION),
        },
        RefusedRow {
            case: "a pinned version of a key the bucket holds nothing under",
            bucket: BUCKET,
            key: ABSENT_KEY,
            version: Some(NEVER_MINTED_VERSION),
        },
        RefusedRow {
            case: "a pinned version in a bucket Amazon S3 holds no record of",
            bucket: ABSENT_BUCKET,
            key: KEY,
            version: Some(NEVER_MINTED_VERSION),
        },
        RefusedRow {
            case: "a key the bucket holds nothing under, with no version pinned",
            bucket: BUCKET,
            key: ABSENT_KEY,
            version: None,
        },
        RefusedRow {
            case: "a bucket Amazon S3 holds no record of, with no version pinned",
            bucket: ABSENT_BUCKET,
            key: KEY,
            version: None,
        },
    ];

    for row in rows {
        for write in WRITES {
            let case = format!("{} on {}", row.case, write.label());
            let engine = Arc::new(engine::InMemoryEngine::new());
            let router = emulator(&engine);
            given_versioned_bucket(&router, &case).await;
            given_object(&router, &case, &archive_bytes(512)).await;

            let (code, worded_by_s3) =
                what_amazon_s3_answers(&router, &case, row.bucket, row.key, row.version).await;
            let expected = expected_refusal(row.version, &code, &worded_by_s3);

            let refused =
                perform(&engine, &router, write, row.bucket, row.key, row.version).await;
            assert_eq!(
                refused.status, REFUSAL_STATUS,
                "case {case:?}: expected {REFUSAL_STATUS}, got {} {}",
                refused.status,
                String::from_utf8_lossy(&refused.body)
            );
            assert!(
                refused.error_code().contains(INVALID_PARAMETER),
                "case {case:?}: expected {INVALID_PARAMETER}, got {}",
                refused.error_code()
            );
            let answered = message(&refused);
            assert_eq!(
                answered.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}

/// RULE (the other half): a pinned version the bucket DOES hold is READ — the versioned read is
/// a read, not a refusal — so the write is served and the stored archive is that version's own
/// bytes.
#[tokio::test]
async fn a_pinned_version_the_bucket_holds_is_read_and_served() {
    for write in WRITES {
        let case = format!("the version the bucket minted on {}", write.label());
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        given_versioned_bucket(&router, &case).await;
        let bytes = archive_bytes(512);
        let version = given_object(&router, &case, &bytes).await;

        let served = perform(&engine, &router, write, BUCKET, KEY, Some(&version)).await;
        assert_eq!(
            served.status,
            write.served_status(),
            "case {case:?}: expected {}, got {} {}",
            write.served_status(),
            served.status,
            String::from_utf8_lossy(&served.body)
        );
        let expected = expected_code_sha256(&bytes);
        let reported = member(&served.json(), write.digest_pointer())
            .and_then(|digest| digest.as_str().map(str::to_string));
        assert_eq!(
            reported.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: the stored archive must be that version's own bytes, whose digest \
             is {expected:?}, got {reported:?}"
        );
    }
}
