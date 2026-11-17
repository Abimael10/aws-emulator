// SPDX-License-Identifier: CC0-1.0

//! WHICH members a layer version's `Content` carries, and whose object they describe.
//!
//! The RULE asserted here, over a table of content sources rather than one: **a layer
//! version's `LayerVersionContentOutput` reports the identity of the archive LAMBDA HOLDS —
//! its `Location`, its `CodeSize` and the `CodeSha256` of the stored bytes — whichever way
//! the archive reached the service ("You can specify either an Amazon S3 location, or upload
//! a layer archive directly", `LayerVersionContentInput`), and the CALLER's own object is
//! reported beside it, as `ResolvedS3Object`, only in the storage mode that leaves the
//! archive there ("`REFERENCE` – Lambda references the layer archive from the specified
//! Amazon S3 bucket", against "`COPY` (default) – Uploads a copy of your layer archive to
//! Lambda").** The digest is what lets a client verify or compare the published bytes, so it
//! is a member of every version's content and not only of one uploaded inline; and because
//! the default mode copies the archive into Lambda's own store, the identity reported for it
//! does not depend on the source object still existing.
//!
//! The rows differ along what the rule generalises over: how the archive arrived (inline, an
//! Amazon S3 object in the default storage mode, an Amazon S3 object Lambda is asked to
//! reference), the archive's size, and whether the source object is deleted after the
//! publish. A second test states the digest as an invariant over the ROUTE: one byte string
//! published both ways carries one digest.
//!
//! What this suite does NOT re-assert, because a sibling suite owns it: that a publish from
//! an Amazon S3 location is refused when the object is not there or is no archive, and that
//! `CodeSize`/`CodeSha256` are the object's own (`s3_package_source.rs`, whose
//! Lambda-beside-S3 harness this suite reuses), and that the announced `Content.Location` is
//! presigned on this emulator's own endpoint and keeps serving the stored bytes after the
//! source object is deleted (`announced_download_locations.rs`).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{zip_file, Answer};
use crate::s3_package_source::{
    archive_bytes, digest_of, emulator, get_layer_version, publish_layer_version, s3_ok,
    s3_request,
};

/// The Amazon S3 bucket every row of this suite stages its archives in.
const SOURCE_BUCKET: &str = "layer-content-source";

/// The `S3ObjectStorageMode` that leaves the archive in the caller's bucket, as the member's
/// own documentation spells it
/// (`smithy_query(query="doc",shape="LayerVersionContentInput",member="S3ObjectStorageMode")`).
const STORAGE_MODE_REFERENCE: &str = "REFERENCE";

/// One path-style Amazon S3 request, answered as it came back — the read this suite makes to
/// have Amazon S3 itself confirm that a deleted source object is gone. A request that must
/// SUCCEED goes through `s3_ok`; both are `s3_package_source.rs`'s.
async fn s3_read(router: &MultiServiceRouter, uri: &str) -> Answer {
    s3_request(router, "GET", uri, &[]).await
}

/// The member names of a JSON object, sorted, so a comparison is about which members the
/// answer carries rather than about their order.
fn member_names(document: &Value) -> Vec<String> {
    let mut names: Vec<String> = document
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// The `Content` block of a layer-version answer, with the answer's text for a failure.
fn content_of(case: &str, answer: &Answer) -> Value {
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: the read of the layer version must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json().get("Content").cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a Content block, got {}",
            String::from_utf8_lossy(&answer.body)
        )
    })
}

/// Where a row's archive comes from.
enum Source {
    /// Uploaded inline in `Content.ZipFile`.
    Inline,
    /// Read out of the Amazon S3 object this suite stages under `key`.
    S3 {
        key: &'static str,
        /// The `S3ObjectStorageMode` the request names, or `None` to leave the member unset
        /// — which is `COPY` by the member's own documentation.
        storage_mode: Option<&'static str>,
        /// Whether the source object is deleted once the version is published, which the
        /// reported identity may not depend on.
        delete_source: bool,
    },
}

/// One content source under test.
struct Row {
    case: &'static str,
    /// The layer the version is published under.
    layer: &'static str,
    /// How many bytes the archive's single entry holds, which is what makes each row's
    /// archive its own byte string.
    length: usize,
    source: Source,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an archive uploaded inline",
            layer: "lyr-inline",
            length: 96,
            source: Source::Inline,
        },
        Row {
            // The two members this row adds to what `s3_package_source.rs` already asserts
            // for an object the bucket holds are the MEMBER SET and the absence of
            // `ResolvedS3Object` in the copying mode; the size and the digest are asserted
            // here too because the member set is meaningless without them.
            case: "an Amazon S3 object, the default storage mode",
            layer: "lyr-from-s3",
            length: 256,
            source: Source::S3 {
                key: "helpers.zip",
                storage_mode: None,
                delete_source: false,
            },
        },
        Row {
            case: "an Amazon S3 object whose source object is deleted after the publish",
            layer: "lyr-from-deleted-source",
            length: 512,
            source: Source::S3 {
                key: "deleted-afterwards.zip",
                storage_mode: None,
                delete_source: true,
            },
        },
        Row {
            case: "an Amazon S3 object Lambda is asked to reference rather than copy",
            layer: "lyr-referenced",
            length: 320,
            source: Source::S3 {
                key: "referenced.zip",
                storage_mode: Some(STORAGE_MODE_REFERENCE),
                delete_source: false,
            },
        },
    ]
}

/// RULE: every layer version's `Content` carries the identity of the archive Lambda holds —
/// `Location`, `CodeSize` and the `CodeSha256` of the stored bytes — however the archive
/// arrived, the caller's own object being reported beside it only in `REFERENCE` mode; a
/// source object deleted after the publish changes none of it.
#[tokio::test]
async fn layer_content_carries_the_identity_of_the_archive_lambda_holds() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    s3_ok(
        &router,
        "the table's bucket",
        "PUT",
        &format!("/{SOURCE_BUCKET}"),
        &[],
    )
    .await;

    for row in rows() {
        let case = row.case;
        let archive = archive_bytes(row.length);
        let (content_request, staged_key, storage_mode, delete_source) = match row.source {
            Source::Inline => (json!({ "ZipFile": zip_file(&archive) }), None, None, false),
            Source::S3 {
                key,
                storage_mode,
                delete_source,
            } => {
                s3_ok(
                    &router,
                    case,
                    "PUT",
                    &format!("/{SOURCE_BUCKET}/{key}"),
                    &archive,
                )
                .await;
                let mut content = json!({ "S3Bucket": SOURCE_BUCKET, "S3Key": key });
                if let Some(mode) = storage_mode {
                    content["S3ObjectStorageMode"] = json!(mode);
                }
                (content, Some(key), storage_mode, delete_source)
            }
        };
        let referenced = storage_mode == Some(STORAGE_MODE_REFERENCE);

        let published = publish_layer_version(
            &router,
            row.layer,
            &json!({ "Content": content_request, "Description": case }),
        )
        .await;
        assert_eq!(
            published.status,
            201,
            "case {case:?}: expected 201, got {} {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );

        // The members the content carries: the archive's own identity, plus the caller's
        // object only where Lambda was asked to reference it. `UncompressedCodeSize` is a
        // member of every layer version a read reports — the recorded behaviour
        // `LayerVersionRecord::content` states, where it carries the archive's EXPANDED size,
        // which no storage mode produces a copy of — so the set it belongs to is asserted
        // here and its value is that member's own concern.
        let mut expected_members = vec![
            "CodeSha256".to_string(),
            "CodeSize".to_string(),
            "Location".to_string(),
            "UncompressedCodeSize".to_string(),
        ];
        if referenced {
            expected_members.push("ResolvedS3Object".to_string());
        }
        expected_members.sort();

        let expected_size = i64::try_from(archive.len()).unwrap_or(i64::MAX);
        let expected_digest = digest_of(&archive);
        let expected_resolved = referenced.then(|| {
            json!({
                "S3Bucket": SOURCE_BUCKET,
                "S3Key": staged_key.expect("an S3-sourced row stages a key"),
            })
        });

        // Asserted on the publish's own answer and on the independent read of what it
        // stored, so nothing here trusts the publishing response alone — and once more after
        // the source object is deleted, which nothing reported may depend on.
        let mut surfaces: Vec<(String, Answer)> = vec![
            ("the publish's own answer".to_string(), published),
            (
                "GetLayerVersion".to_string(),
                get_layer_version(&router, row.layer, 1).await,
            ),
        ];
        if delete_source {
            let key = staged_key.expect("a row that deletes its source stages a key");
            s3_ok(
                &router,
                case,
                "DELETE",
                &format!("/{SOURCE_BUCKET}/{key}"),
                &[],
            )
            .await;
            // Amazon S3 itself confirms the source object is gone, so what the version still
            // reports is Lambda's own copy of the archive.
            let source = s3_read(&router, &format!("/{SOURCE_BUCKET}/{key}")).await;
            assert_eq!(
                source.status,
                404,
                "case {case:?}: the source object must be gone from Amazon S3, got {} {}",
                source.status,
                String::from_utf8_lossy(&source.body)
            );
            surfaces.push((
                "GetLayerVersion after the source object was deleted".to_string(),
                get_layer_version(&router, row.layer, 1).await,
            ));
        }

        for (surface, answer) in &surfaces {
            let content = content_of(case, answer);
            let members = member_names(&content);
            assert_eq!(
                members, expected_members,
                "case {case:?} on {surface}: expected Content members {expected_members:?}, \
                 got {members:?}"
            );
            let size = content.get("CodeSize").and_then(Value::as_i64);
            assert_eq!(
                size,
                Some(expected_size),
                "case {case:?} on {surface}: expected CodeSize {expected_size}, got {size:?}"
            );
            let digest = content
                .get("CodeSha256")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                digest.as_deref(),
                Some(expected_digest.as_str()),
                "case {case:?} on {surface}: expected CodeSha256 {expected_digest:?}, got \
                 {digest:?}"
            );
            let resolved = content.get("ResolvedS3Object").cloned();
            assert_eq!(
                resolved, expected_resolved,
                "case {case:?} on {surface}: expected ResolvedS3Object \
                 {expected_resolved:?}, got {resolved:?}"
            );
            let location = content.get("Location").and_then(Value::as_str);
            assert!(
                location.is_some_and(|location| !location.is_empty()),
                "case {case:?} on {surface}: expected an announced Location, got {location:?}"
            );
        }
    }
}

/// RULE (the same one, as an invariant over the ROUTE): `CodeSha256` is the digest of the
/// ARCHIVE, so one byte string published both inline and out of an Amazon S3 object carries
/// one digest — the route the bytes arrived by is not part of their identity.
#[tokio::test]
async fn one_archive_carries_one_digest_whichever_route_it_arrived_by() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    s3_ok(
        &router,
        "the comparison's bucket",
        "PUT",
        &format!("/{SOURCE_BUCKET}"),
        &[],
    )
    .await;

    // The sizes differ so each row is its own byte string, which is what makes the
    // cross-route comparison about the bytes rather than about one archive.
    for (index, length) in [64_usize, 1_024, 4_096].into_iter().enumerate() {
        let archive = archive_bytes(length);
        let case = format!("an archive of {length} entry bytes");
        let key = format!("compared-{index}.zip");
        s3_ok(
            &router,
            &case,
            "PUT",
            &format!("/{SOURCE_BUCKET}/{key}"),
            &archive,
        )
        .await;

        let from_s3 = publish_layer_version(
            &router,
            &format!("lyr-compared-s3-{index}"),
            &json!({ "Content": { "S3Bucket": SOURCE_BUCKET, "S3Key": key } }),
        )
        .await;
        let inline = publish_layer_version(
            &router,
            &format!("lyr-compared-inline-{index}"),
            &json!({ "Content": { "ZipFile": zip_file(&archive) } }),
        )
        .await;

        let expected = digest_of(&archive);
        for (route, answer) in [
            ("an Amazon S3 object", &from_s3),
            ("an inline upload", &inline),
        ] {
            let content = content_of(&case, answer);
            let digest = content
                .get("CodeSha256")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                digest.as_deref(),
                Some(expected.as_str()),
                "case {case:?} by {route}: expected CodeSha256 {expected:?}, got {digest:?}"
            );
        }
    }
}
