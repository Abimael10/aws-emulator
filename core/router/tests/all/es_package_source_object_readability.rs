// SPDX-License-Identifier: CC0-1.0

//! An Amazon Elasticsearch Service package is the Amazon S3 OBJECT its
//! `PackageSource` names, across the one emulator.
//!
//! RULE: a call that imports package content — `CreatePackage` and every
//! `UpdatePackage` — resolves the object `PackageSource` names
//! (`S3BucketName`/`S3Key`, "the customer S3 location for importing the
//! package") in Amazon S3 BEFORE it writes anything, and an object the service
//! cannot read is refused as invalid input with the fixed sentence `Package file
//! not accessible. Please validate that you have access to the S3 object and S3
//! bucket is present in the same region.` Nothing of the request appears in that
//! sentence, and nothing is left behind by the refused call: the package is not
//! created, and a package whose update was refused keeps the content and the
//! version it already had.
//!
//! Only a cross-service suite can state that end to end: Amazon S3 owns the
//! bucket and the object, and the Elasticsearch control plane owns the package,
//! so what the package plane answers has to follow from what the object store
//! holds — an object the suite stages with `PutObject` is imported, and one it
//! never staged is refused.
//!
//! The rows differ along what the rule generalises over: WHY the object cannot
//! be read (a key the bucket does not hold, a bucket Amazon S3 does not hold, a
//! bucket name Amazon S3 holds no bucket under at all, an address naming a
//! prefix rather than an object, an address naming no key), the shape of the key
//! (a top-level key and a nested one), and the operation that names the source
//! (the create and the update).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The media type Amazon S3's own REST requests travel with.
const XML_CONTENT_TYPE: &str = "application/xml";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The sentence this service answers for a package source it cannot read. Fixed
/// text in its entirety: it names neither the bucket, the key nor the Region.
const NOT_ACCESSIBLE: &str = "Package file not accessible. Please validate that you have access \
                              to the S3 object and S3 bucket is present in the same region.";

/// The emulator as it is ASSEMBLED, on one engine: the object a package points at
/// is a record of this emulator's own Amazon S3.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential
    // scope so the router can name the target service, without a computed
    // signature.
    router.set_request_authentication(false);
    router
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for an assertion's failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// Dispatch one request to `service`, signed for [`REGION`] under `signing_name`.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    signing_name: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            ::http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{uri}"))
                .header("host", host)
                .header(
                    "authorization",
                    format!(
                        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/\
                         {signing_name}/aws4_request, SignedHeaders=host, \
                         Signature=not-verified"
                    ),
                )
                .header("content-type", content_type)
                .body(Bytes::from(body.to_vec()))
                .expect("the test request is well formed"),
        )
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One call to Amazon S3's own REST wire, path-addressed.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answered {
    dispatch(router, "s3", "s3", method, uri, XML_CONTENT_TYPE, body).await
}

/// One call to the Elasticsearch Service control plane.
async fn es(router: &MultiServiceRouter, uri: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "es",
        "es",
        "POST",
        uri,
        JSON_CONTENT_TYPE,
        body.to_string().as_bytes(),
    )
    .await
}

/// Create `bucket` where Amazon S3 holds buckets of [`REGION`].
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    // "If you don't specify a Region, the bucket is created in the US East (N.
    // Virginia) Region (us-east-1) by default" — which is the Region every call
    // here is signed for, so the create names no constraint.
    let created = s3(router, "PUT", &format!("/{bucket}"), &[]).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        created.status,
        created.text()
    );
}

/// Stage one object where Amazon S3's own clients write it.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        b"alpha, beta\ngamma, delta\n",
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({bucket:?}/{key:?}) must succeed, got {} with {}",
        written.status,
        written.text()
    );
}

/// The `PackageDetails` the packages of this account and Region are reported
/// under.
async fn described(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let answer = es(router, "/2015-01-01/packages/describe", &json!({})).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: DescribePackages must answer 200, got {} with {}",
        answer.status,
        answer.text()
    );
    answer
        .json()
        .pointer("/PackageDetailsList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Assert that `answer` is the refusal of an unreadable package source.
fn refused(answer: &Answered, case: &str) {
    assert_eq!(
        answer.status,
        400,
        "case {case:?}: an unreadable package source must be refused with 400, got {} with {}",
        answer.status,
        answer.text()
    );
    let message = answer
        .json()
        .pointer("/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no message, got {}", answer.text()));
    assert_eq!(
        message, NOT_ACCESSIBLE,
        "case {case:?}: unexpected refusal message, got {message:?}"
    );
    let error_type = answer
        .header("x-amzn-errortype")
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no x-amzn-errortype header"));
    assert!(
        !error_type.is_empty(),
        "case {case:?}: the refusal must name its error shape, got {error_type:?}"
    );
}

/// One row of the create table: the source a create names, and whether the
/// object store serves it.
struct Row {
    case: &'static str,
    /// The package's name, which is also its id within the Region.
    name: &'static str,
    bucket: &'static str,
    key: &'static str,
    /// Whether the bucket is created before the call.
    bucket_held: bool,
    /// Whether the object is staged at `key` before the call.
    object_staged: bool,
    /// Whether the import must be accepted.
    importable: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the object the bucket holds",
        name: "dict-held",
        bucket: "es-package-source-held",
        key: "dictionaries/synonyms.txt",
        bucket_held: true,
        object_staged: true,
        importable: true,
    },
    Row {
        case: "an object at a top-level key the bucket holds",
        name: "dict-top-level",
        bucket: "es-package-source-top",
        key: "synonyms.txt",
        bucket_held: true,
        object_staged: true,
        importable: true,
    },
    Row {
        case: "a key the bucket does not hold",
        name: "dict-no-key",
        bucket: "es-package-source-empty",
        key: "dictionaries/absent.txt",
        bucket_held: true,
        object_staged: false,
        importable: false,
    },
    Row {
        case: "a bucket Amazon S3 does not hold",
        name: "dict-no-bucket",
        bucket: "es-package-source-absent",
        key: "synonyms.txt",
        bucket_held: false,
        object_staged: false,
        importable: false,
    },
    Row {
        case: "a bucket name Amazon S3 holds no bucket under",
        name: "dict-bad-bucket",
        // Upper case: outside Amazon S3's general-purpose bucket naming rules, so
        // no bucket anywhere carries this name.
        bucket: "ES-Package-Source",
        key: "synonyms.txt",
        bucket_held: false,
        object_staged: false,
        importable: false,
    },
    Row {
        case: "an address naming a prefix rather than an object",
        name: "dict-prefix",
        bucket: "es-package-source-prefix",
        key: "dictionaries/",
        bucket_held: true,
        object_staged: false,
        importable: false,
    },
    Row {
        case: "an address naming no key at all",
        name: "dict-no-key-named",
        bucket: "es-package-source-keyless",
        key: "",
        bucket_held: true,
        object_staged: false,
        importable: false,
    },
];

/// RULE: a create imports the object its source names, and is refused with the
/// fixed sentence when the object store cannot serve that object.
#[tokio::test]
async fn a_create_imports_the_object_its_source_names() {
    let router = build_router();
    for row in ROWS {
        let case = row.case;
        if row.bucket_held {
            create_bucket(&router, case, row.bucket).await;
        }
        if row.object_staged {
            put_object(&router, case, row.bucket, row.key).await;
        }

        let answer = es(
            &router,
            "/2015-01-01/packages",
            &json!({
                "PackageName": row.name,
                "PackageType": "TXT-DICTIONARY",
                "PackageSource": {"S3BucketName": row.bucket, "S3Key": row.key}
            }),
        )
        .await;

        if row.importable {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: a staged object must be imported, got {} with {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.json().pointer("/PackageDetails/PackageName"),
                Some(&json!(row.name)),
                "case {case:?}: the create must report the package it minted, got {}",
                answer.text()
            );
        } else {
            refused(&answer, case);
        }
    }

    // Nothing the object store could not serve was created: the listing holds
    // exactly the rows whose object was staged.
    let listed = described(&router, "after the table").await;
    let mut names: Vec<String> = listed
        .iter()
        .filter_map(|entry| {
            entry
                .pointer("/PackageName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    names.sort();
    let mut expected: Vec<String> = ROWS
        .iter()
        .filter(|row| row.importable)
        .map(|row| row.name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "only the packages whose object the store serves may exist, got {names:?}"
    );
}

/// RULE: an update imports the object ITS source names too, so a revision
/// pointed at an object the store cannot serve is refused and the package keeps
/// the content and version it had.
#[tokio::test]
async fn an_update_imports_the_object_its_source_names() {
    let router = build_router();
    let bucket = "es-package-update-source";
    let first_key = "dictionaries/v1.txt";
    let next_key = "dictionaries/v2.txt";
    create_bucket(&router, "the update table", bucket).await;
    put_object(&router, "the update table", bucket, first_key).await;

    let created = es(
        &router,
        "/2015-01-01/packages",
        &json!({
            "PackageName": "dict-revised",
            "PackageType": "TXT-DICTIONARY",
            "PackageSource": {"S3BucketName": bucket, "S3Key": first_key}
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "the create of a staged object must succeed, got {} with {}",
        created.status,
        created.text()
    );
    let package_id = created
        .json()
        .pointer("/PackageDetails/PackageID")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("the create reported no PackageID, got {}", created.text()));

    // Rows differ along whether the object the revision names is one the store
    // serves, and how it misses when it does not: (case, bucket, key, imported).
    let rows: [(&str, &str, &str, bool); 3] = [
        ("a key the bucket does not hold yet", bucket, next_key, false),
        (
            "a bucket Amazon S3 does not hold",
            "es-package-update-absent",
            first_key,
            false,
        ),
        ("the object once it is staged", bucket, next_key, true),
    ];
    for (case, row_bucket, key, importable) in rows {
        if importable {
            put_object(&router, case, row_bucket, key).await;
        }
        let answer = es(
            &router,
            "/2015-01-01/packages/update",
            &json!({
                "PackageID": package_id,
                "PackageSource": {"S3BucketName": row_bucket, "S3Key": key},
                "CommitMessage": case
            }),
        )
        .await;
        let version = if importable {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: a staged object must be imported, got {} with {}",
                answer.status,
                answer.text()
            );
            "v2"
        } else {
            refused(&answer, case);
            // The refused revision left the package on the version it had.
            "v1"
        };
        let history = dispatch(
            &router,
            "es",
            "es",
            "GET",
            &format!("/2015-01-01/packages/{package_id}/history"),
            JSON_CONTENT_TYPE,
            &[],
        )
        .await;
        assert_eq!(
            history.status,
            200,
            "case {case:?}: the version history must answer 200, got {} with {}",
            history.status,
            history.text()
        );
        assert_eq!(
            history
                .json()
                .pointer("/PackageVersionHistoryList/0/PackageVersion"),
            Some(&json!(version)),
            "case {case:?}: the newest version must be {version}, got {}",
            history.text()
        );
    }
}
