// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the address `GetFunction` announces a deployment package at, and
//! for the bytes that address serves.
//!
//! The RULE asserted here, over a table of packages and of function identities rather than
//! one read: **a URL an operation hands the caller to fetch later is addressed to the
//! endpoint this emulator is reached on, and following it serves exactly the bytes the store
//! holds for the object it names.** For `Code$Location` — "A presigned URL that you can use
//! to download the deployment package" (`smithy_query(query="doc",
//! shape="FunctionCodeLocation", member="Location")`) — those bytes are the package of the
//! identity the read addressed, and their digest is what the same answer reports as
//! `Configuration.CodeSha256`. This emulator answers on its own endpoint alone, so a
//! location on any other host — a production AWS hostname among them — is one no client of
//! it can download anything from.
//!
//! The rows differ along what the rule generalises over: where the package came from (bytes
//! uploaded inline, an Amazon S3 object copied at create, an Amazon S3 object copied by a
//! later code update), how long it is, and WHICH identity of the function is read
//! (unqualified, `$LATEST`, a published version, an alias, and a version named in the
//! function reference itself). A second table drives the same read over emulators reached at
//! different base URLs, because the announced address has to follow the endpoint rather than
//! be a constant.
//!
//! Amazon S3 is mounted beside Lambda on one engine (as `core/router/src/lib.rs` mounts
//! them), so a package named by an Amazon S3 location is uploaded through S3's own surface.
//! Every harness piece comes from this test target's own modules: the Lambda wire helpers
//! `function_lifecycle.rs` publishes (`Answer`, `json_call`, `package_bytes`, `zip_file`,
//! `expected_code_sha256`) and the download-plane helpers `announced_download_locations.rs`
//! publishes (the two-service emulator, the S3 upload, the archive bytes, the announced
//! location of a read and the unsigned fetch of one).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness of this test target: the Lambda wire helpers `function_lifecycle.rs` publishes,
// and the download-plane helpers `announced_download_locations.rs` publishes — the emulator
// with Amazon S3 mounted beside Lambda, an S3 request that must be served, the archive bytes
// an S3-sourced package is taken from, the announced location of a read, and the unsigned
// fetch of one. Nothing of either is restated here.
use crate::announced_download_locations::{
    archive_bytes, emulator_at, fetch, get_function, location_of, publish_version, s3_ok,
};
use crate::function_lifecycle::{
    create_function, expected_code_sha256, json_call, make_update_due, package_bytes, zip_file,
    Answer, ROLE,
};

/// The base URL the emulator of the first table is reached on, which no announced location
/// may disagree with.
const BASE: &str = "https://lambda.emulator.test:7443";

/// The Amazon S3 bucket this suite uploads a deployment package to.
const SOURCE_BUCKET: &str = "deployment-packages";

/// The JSON pointer a `GetFunction` answer carries its package's download link at.
const CODE_LOCATION: &str = "/Code/Location";

/// `CreateFunction` of a .zip function whose `Code` member is `code`, which must succeed.
async fn create_zip_function(router: &MultiServiceRouter, name: &str, code: &Value) {
    let answer = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": code,
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {name:?}: CreateFunction must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// `CreateAlias` pointing `alias` at one published version.
async fn create_alias(
    router: &MultiServiceRouter,
    name: &str,
    alias: &str,
    version: &str,
) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/aliases");
    let body = json!({ "Name": alias, "FunctionVersion": version });
    json_call(router, "POST", &uri, &body).await
}

/// `UpdateFunctionCode`, whose package members are carried on the request itself.
async fn update_code(router: &MultiServiceRouter, name: &str, body: &Value) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/code");
    json_call(router, "PUT", &uri, body).await
}

/// One announced package download under test.
struct Download {
    /// The case name, which every assertion reports.
    case: &'static str,
    /// The `GetFunction` answer the location was announced in.
    answer: Answer,
    /// The bytes that were uploaded for the identity that read addressed.
    expected: Vec<u8>,
}

/// `GetFunction` announces its package on an address this emulator serves, and that address
/// serves the uploaded bytes byte for byte, under the digest the same read reports.
#[tokio::test]
async fn get_function_announces_its_package_on_this_emulators_own_endpoint() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_at(&engine, BASE);

    // One function whose code is uploaded inline, published as version 1, aliased, and then
    // REPLACED: the published version keeps serving the package it froze, and the function's
    // own identities serve the new one.
    let created = package_bytes(128);
    let replaced = package_bytes(301);
    create_zip_function(
        &router,
        "code-inline",
        &json!({ "ZipFile": zip_file(&created) }),
    )
    .await;
    make_update_due(&engine);
    let published = publish_version(&router, "code-inline").await;
    assert_eq!(
        published.status,
        201,
        "PublishVersion must succeed, got {} {}",
        published.status,
        String::from_utf8_lossy(&published.body)
    );
    make_update_due(&engine);
    let aliased = create_alias(&router, "code-inline", "live", "1").await;
    assert_eq!(
        aliased.status,
        201,
        "CreateAlias must succeed, got {} {}",
        aliased.status,
        String::from_utf8_lossy(&aliased.body)
    );
    let recoded = update_code(
        &router,
        "code-inline",
        &json!({ "ZipFile": zip_file(&replaced) }),
    )
    .await;
    assert_eq!(
        recoded.status,
        200,
        "UpdateFunctionCode must succeed, got {} {}",
        recoded.status,
        String::from_utf8_lossy(&recoded.body)
    );
    make_update_due(&engine);

    // The shortest package there is.
    let smallest = package_bytes(1);
    create_zip_function(
        &router,
        "code-smallest",
        &json!({ "ZipFile": zip_file(&smallest) }),
    )
    .await;
    make_update_due(&engine);

    // A package taken from an Amazon S3 object whose SOURCE object is then deleted: Lambda
    // "uploads a copy of your deployment package", so the announced location keeps serving
    // the copy it stored.
    let from_s3 = archive_bytes(64);
    s3_ok(
        &router,
        "an S3-sourced package",
        "PUT",
        &format!("/{SOURCE_BUCKET}"),
        &[],
    )
    .await;
    s3_ok(
        &router,
        "an S3-sourced package",
        "PUT",
        &format!("/{SOURCE_BUCKET}/create.zip"),
        &from_s3,
    )
    .await;
    create_zip_function(
        &router,
        "code-from-s3",
        &json!({ "S3Bucket": SOURCE_BUCKET, "S3Key": "create.zip" }),
    )
    .await;
    make_update_due(&engine);
    s3_ok(
        &router,
        "an S3-sourced package",
        "DELETE",
        &format!("/{SOURCE_BUCKET}/create.zip"),
        &[],
    )
    .await;

    // And a package a later code update took from an Amazon S3 object.
    let updated_from_s3 = archive_bytes(512);
    s3_ok(
        &router,
        "a package updated from Amazon S3",
        "PUT",
        &format!("/{SOURCE_BUCKET}/update.zip"),
        &updated_from_s3,
    )
    .await;
    create_zip_function(
        &router,
        "code-updated-from-s3",
        &json!({ "ZipFile": zip_file(&created) }),
    )
    .await;
    make_update_due(&engine);
    let recoded_from_s3 = update_code(
        &router,
        "code-updated-from-s3",
        &json!({ "S3Bucket": SOURCE_BUCKET, "S3Key": "update.zip" }),
    )
    .await;
    assert_eq!(
        recoded_from_s3.status,
        200,
        "UpdateFunctionCode from Amazon S3 must succeed, got {} {}",
        recoded_from_s3.status,
        String::from_utf8_lossy(&recoded_from_s3.body)
    );
    make_update_due(&engine);

    let rows = vec![
        Download {
            case: "the function's own package, read with no qualifier",
            answer: get_function(&router, "code-inline", None).await,
            expected: replaced.clone(),
        },
        Download {
            case: "the same package read at $LATEST",
            answer: get_function(&router, "code-inline", Some("$LATEST")).await,
            expected: replaced.clone(),
        },
        Download {
            case: "the package a published version froze",
            answer: get_function(&router, "code-inline", Some("1")).await,
            expected: created.clone(),
        },
        Download {
            case: "the package an alias resolves to",
            answer: get_function(&router, "code-inline", Some("live")).await,
            expected: created.clone(),
        },
        Download {
            case: "a version named in the function reference itself",
            answer: get_function(&router, "code-inline:1", None).await,
            expected: created.clone(),
        },
        Download {
            case: "a one-byte package",
            answer: get_function(&router, "code-smallest", None).await,
            expected: smallest.clone(),
        },
        Download {
            case: "a package copied from a since-deleted Amazon S3 object",
            answer: get_function(&router, "code-from-s3", None).await,
            expected: from_s3.clone(),
        },
        Download {
            case: "a package a code update took from Amazon S3",
            answer: get_function(&router, "code-updated-from-s3", None).await,
            expected: updated_from_s3.clone(),
        },
    ];

    for row in &rows {
        let case = row.case;
        let body = row.answer.json();
        let url = location_of(case, &row.answer, CODE_LOCATION);

        // The announced address is this emulator's own, and names no host only AWS serves.
        assert!(
            url.starts_with(&format!("{BASE}/")),
            "case {case:?}: expected a location on {BASE}, got {url:?}"
        );
        assert!(
            !url.contains("amazonaws.com"),
            "case {case:?}: a location may not name a host only AWS serves, got {url:?}"
        );
        // A .zip package is served out of a store, which is what `RepositoryType` says.
        assert_eq!(
            body.pointer("/Code/RepositoryType").and_then(Value::as_str),
            Some("S3"),
            "case {case:?}: expected the repository type of a .zip package, got {body}"
        );

        // Following it serves exactly the bytes that were uploaded for that identity.
        let served = fetch(&router, "GET", &url).await;
        assert_eq!(
            served.status,
            200,
            "case {case:?}: expected 200 following {url:?}, got {} {}",
            served.status,
            String::from_utf8_lossy(&served.body)
        );
        assert_eq!(
            served.body.as_ref(),
            row.expected.as_slice(),
            "case {case:?}: expected the {} uploaded bytes, got {} bytes",
            row.expected.len(),
            served.body.len()
        );
        // And the read's own `CodeSha256` is the digest of the bytes that address serves,
        // which is what a client verifying a downloaded package compares.
        let digest = expected_code_sha256(&served.body);
        assert_eq!(
            body.pointer("/Configuration/CodeSha256")
                .and_then(Value::as_str),
            Some(digest.as_str()),
            "case {case:?}: expected CodeSha256 to be the served bytes' digest {digest:?}, \
             got {body}"
        );
        assert_eq!(
            body.pointer("/Configuration/CodeSize")
                .and_then(Value::as_i64),
            i64::try_from(row.expected.len()).ok(),
            "case {case:?}: expected a CodeSize of {}, got {body}",
            row.expected.len()
        );

        // A download is a pure READ: following it again serves the same bytes.
        let again = fetch(&router, "GET", &url).await;
        assert_eq!(
            again.body,
            served.body,
            "case {case:?}: expected the same bytes on a second fetch, got {} bytes",
            again.body.len()
        );
    }

    // Two identities holding different packages are two different addresses.
    let latest = location_of(rows[0].case, &rows[0].answer, CODE_LOCATION);
    let frozen = location_of(rows[2].case, &rows[2].answer, CODE_LOCATION);
    assert_ne!(
        latest, frozen,
        "each identity's location must address its own package, got {latest:?} twice"
    );
}

/// The announced address FOLLOWS the endpoint this emulator is reached on: the same function
/// read over emulators with different base URLs announces its package on each one, and the
/// package is fetchable there. The rows differ along the base URL's form — a host name with
/// a port, a loopback address with a port, and a host name with no port at all.
#[tokio::test]
async fn the_announced_package_address_follows_the_endpoint_the_emulator_is_reached_on() {
    let package = package_bytes(96);
    for base in [BASE, "https://127.0.0.1:36917", "https://emulator.test"] {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_at(&engine, base);
        create_zip_function(
            &router,
            "code-endpoint",
            &json!({ "ZipFile": zip_file(&package) }),
        )
        .await;
        make_update_due(&engine);

        let answer = get_function(&router, "code-endpoint", None).await;
        let url = location_of(base, &answer, CODE_LOCATION);
        assert!(
            url.starts_with(&format!("{base}/")),
            "case {base:?}: expected a location on {base}, got {url:?}"
        );
        let served = fetch(&router, "GET", &url).await;
        assert_eq!(
            (served.status, served.body.as_ref()),
            (200, package.as_slice()),
            "case {base:?}: expected the {} uploaded bytes at {url:?}, got {} {}",
            package.len(),
            served.status,
            String::from_utf8_lossy(&served.body)
        );
    }
}
