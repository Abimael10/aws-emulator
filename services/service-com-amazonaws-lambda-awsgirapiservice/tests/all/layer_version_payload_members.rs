// SPDX-License-Identifier: CC0-1.0

//! WHAT a layer-version payload reports, across every operation that reports one.
//!
//! Two rules, each asserted over a table of inputs rather than one request:
//!
//! 1. **A layer version's `LayerVersionContentOutput` describes the archive Lambda holds, and
//!    describes it the same way on every operation that reports that version**: `CodeSize` is
//!    "The size of the layer archive in bytes", `CodeSha256` is "The SHA-256 hash of the layer
//!    archive" and `Location` is "A link to the layer archive in Amazon S3 that is valid for 10
//!    minutes" — a presigned link, which this emulator vends against its own endpoint
//!    (`smithy_query(query="shape", shape="LayerVersionContentOutput")`). The archive reached
//!    the service either way the model allows ("You can specify either an Amazon S3 location,
//!    or upload a layer archive directly", `LayerVersionContentInput`), which is not part of
//!    what is reported about it: the three members are the stored archive's own size, its own
//!    digest and one address serving its own bytes, identical on the publish's answer, on
//!    `GetLayerVersion` and on `GetLayerVersionByArn`.
//! 2. **Every ISO-8601 instant a layer read or a layer LISTING reports carries the numeric UTC
//!    offset the model's own example spells** — `LayerVersionsListItem$CreatedDate` is
//!    documented as "The date that the version was created, in ISO 8601 format. For example,
//!    `2018-11-27T15:10:45.123+0000`"
//!    (`smithy_query(query="doc", shape="LayerVersionsListItem", member="CreatedDate")`) — and
//!    a version is immutable once published, so every surface reporting one reports the same
//!    instant: the publish's own answer, `GetLayerVersion`, `GetLayerVersionByArn`, the
//!    `ListLayerVersions` entry and the `LatestMatchingVersion` of the `ListLayers` entry,
//!    filtered or not.
//!
//! The rows differ along what the rules generalise over: how the archive arrived (inline, an
//! Amazon S3 object), the archive's size, which operation reports the version, how many
//! versions the layer holds, and whether the listing that reports it was filtered.
//!
//! Both rules are held by siblings over parts of this member set already — the presigned link
//! over all three reporting operations (`announced_download_locations.rs`, whose
//! Lambda-beside-S3 harness and presigned-link reduction this suite reuses), the size and the
//! digest over the two content routes (`layer_content_identity.rs`), and a listing entry's
//! `CreatedDate` against the publish's own (`layer_version_listing.rs`, `layer_listing.rs`,
//! `layer_listing_entry_members.rs`) — so what this suite adds is the two rules asserted
//! WHOLE, in one place, over the cross product the issue this file answers spans: the size and
//! the digest on `GetLayerVersionByArn` as well, the three surfaces shown to address one
//! object, and the documented instant FORM read off each listing entry itself rather than
//! inherited from the publish it is compared with.
//!
//! What it does NOT re-assert, because a sibling owns it: that a publish from an Amazon S3
//! location is refused when the object is not there or is no archive (`s3_package_source.rs`),
//! which member set a content block carries and when the caller's own object is reported beside
//! it (`layer_content_identity.rs`), and that an announced location keeps serving the stored
//! bytes after the source object is deleted (`announced_download_locations.rs`).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness of the suite that owns the announced-link rule: its emulator (Lambda beside
// Amazon S3 on one engine, reached at a base URL), its Amazon S3 staging helpers, and its
// reduction of a presigned link to the object and terms it carries — so the requests this
// suite stages objects with are signed exactly as that harness signs them.
use crate::announced_download_locations::{
    addressed_object, archive_bytes, emulator_at, fetch, get_layer_version_by_arn, s3_ok,
};
use crate::function_lifecycle::{
    expected_code_sha256, json_call, zip_file, Answer, ACCOUNT, REGION,
};
use crate::function_url_reported_members::assert_offset_instant;
use crate::s3_package_source::{get_layer_version, list_layer_versions, publish_layer_version};

/// The base URL the emulator of this suite is reached on, which every announced location has
/// to lead back to.
const BASE: &str = "https://layer-payloads.test:8443";

/// The Amazon S3 bucket this suite stages the archives of its S3-sourced rows in.
const SOURCE_BUCKET: &str = "layer-payload-source";

/// A `Runtime` entry a filtered listing selects on, from the model's own enum
/// (`smithy_query(query="shape", shape="Runtime")`).
const RUNTIME: &str = "python3.12";

/// An `Architecture` entry a filtered listing selects on, from the model's own enum
/// (`smithy_query(query="shape", shape="Architecture")`).
const ARCHITECTURE: &str = "x86_64";

/// `ListLayers` with a query string (`""` for none).
///
/// The crate's listing suites address `ListLayers` through their own per-Region harness
/// (`layer_listing.rs`), which signs for a Region a row names; this suite drives the one
/// harness its other calls go through, so the call is spelled here against that one.
async fn list_layers(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = format!("/2018-10-31/layers{query}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// The JSON body of an answer that has to have succeeded, with its text for a failure.
fn served(case: &str, surface: &str, answer: &Answer) -> Value {
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: {surface} must succeed, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json()
}

/// A member of a document, by JSON pointer, as the string it has to be.
fn string_at(case: &str, surface: &str, document: &Value, pointer: &str) -> String {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: {surface} must report {pointer}, got {document}")
        })
        .to_string()
}

/// One instant member of a document: asserted to be in the form the model's own example
/// spells, and answered so two surfaces can be compared on it.
fn offset_instant(case: &str, surface: &str, document: &Value, pointer: &str) -> String {
    let value = document.pointer(pointer).cloned().unwrap_or(Value::Null);
    assert_offset_instant(&format!("{case} on {surface}"), pointer, &value);
    // The form assertion above has already panicked on anything but a string instant, so this
    // states that invariant rather than yielding an empty instant a comparison would carry.
    value
        .as_str()
        .expect("an instant in the documented form is a string")
        .to_string()
}

/// Where a row's archive comes from.
enum Source {
    /// Uploaded inline in `Content.ZipFile`.
    Inline,
    /// Read out of the Amazon S3 object this suite stages under `key`.
    S3 { key: &'static str },
}

/// One published archive under test.
struct Row {
    case: &'static str,
    /// The layer the version is published under.
    layer: &'static str,
    /// How many bytes the archive's single entry holds, which is what makes each row's
    /// archive its own byte string of its own length.
    length: usize,
    source: Source,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a small archive uploaded inline",
            layer: "lyr-payload-inline",
            length: 72,
            source: Source::Inline,
        },
        Row {
            case: "a larger archive uploaded inline",
            layer: "lyr-payload-inline-larger",
            length: 3_072,
            source: Source::Inline,
        },
        Row {
            case: "a small archive read out of an Amazon S3 object",
            layer: "lyr-payload-from-s3",
            length: 160,
            source: Source::S3 {
                key: "helpers.zip",
            },
        },
        Row {
            case: "a larger archive read out of an Amazon S3 object",
            layer: "lyr-payload-from-s3-larger",
            length: 4_096,
            source: Source::S3 {
                key: "nested/larger-helpers.zip",
            },
        },
    ]
}

/// RULE: a layer version's content is the stored archive's own size, the stored archive's own
/// digest and one presigned address serving the stored archive's own bytes — reported
/// identically by the publish, by `GetLayerVersion` and by `GetLayerVersionByArn`, however the
/// archive arrived and whatever its size.
#[tokio::test]
async fn a_layer_versions_content_describes_the_stored_archive_on_every_surface() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_at(&engine, BASE);
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
        let content_request = match row.source {
            Source::Inline => json!({ "ZipFile": zip_file(&archive) }),
            Source::S3 { key } => {
                s3_ok(
                    &router,
                    case,
                    "PUT",
                    &format!("/{SOURCE_BUCKET}/{key}"),
                    &archive,
                )
                .await;
                json!({ "S3Bucket": SOURCE_BUCKET, "S3Key": key })
            }
        };

        let published = publish_layer_version(
            &router,
            row.layer,
            &json!({ "Content": content_request, "Description": case }),
        )
        .await;
        assert_eq!(
            published.status,
            201,
            "case {case:?}: expected 201 from PublishLayerVersion, got {} {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        let publish_body = served(case, "PublishLayerVersion", &published);
        let version = publish_body
            .get("Version")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: the publish must report a Version"));
        let version_arn = string_at(case, "PublishLayerVersion", &publish_body, "/LayerVersionArn");
        assert_eq!(
            version_arn,
            format!(
                "arn:aws:lambda:{REGION}:{ACCOUNT}:layer:{}:{version}",
                row.layer
            ),
            "case {case:?}: expected the version ARN of {}:{version}, got {version_arn:?}",
            row.layer
        );

        // What the archive's own bytes say about it, computed here rather than read out of
        // the answer being checked.
        let expected_size = i64::try_from(archive.len()).unwrap_or(i64::MAX);
        let expected_digest = expected_code_sha256(&archive);

        let surfaces: Vec<(&str, Answer)> = vec![
            ("PublishLayerVersion", published),
            (
                "GetLayerVersion",
                get_layer_version(&router, row.layer, version).await,
            ),
            (
                "GetLayerVersionByArn",
                get_layer_version_by_arn(&router, &version_arn).await,
            ),
        ];

        // The ONE object every surface's link addresses: a link is presigned afresh per read
        // ("valid for 10 minutes"), so what two reads have to agree on is the object and the
        // terms rather than the text of the URL.
        let mut addressed: Vec<(&str, String)> = Vec::new();
        for (surface, answer) in &surfaces {
            let body = served(case, surface, answer);
            let content = body.get("Content").cloned().unwrap_or_else(|| {
                panic!("case {case:?}: {surface} must report a Content block, got {body}")
            });

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

            // The link is presigned (`addressed_object` asserts the instant, the signature
            // and the credential scope), is addressed to this emulator's own endpoint, and
            // serves exactly the stored archive.
            let location = string_at(case, surface, &content, "/Location");
            assert!(
                location.starts_with(&format!("{BASE}/")),
                "case {case:?} on {surface}: expected a Location on {BASE}, got {location:?}"
            );
            addressed.push((
                surface,
                addressed_object(
                    &format!("{case} on {surface}"),
                    &Value::String(location.clone()),
                ),
            ));

            let fetched = fetch(&router, "GET", &location).await;
            assert_eq!(
                fetched.status,
                200,
                "case {case:?} on {surface}: expected 200 following the Location, got {} {}",
                fetched.status,
                String::from_utf8_lossy(&fetched.body)
            );
            assert_eq!(
                fetched.body.as_ref(),
                archive.as_slice(),
                "case {case:?} on {surface}: expected the Location to serve the {} stored \
                 bytes, got {} bytes",
                archive.len(),
                fetched.body.len()
            );
        }

        let (first_surface, first_object) = addressed
            .first()
            .cloned()
            .expect("every row reports at least one surface");
        for (surface, object) in &addressed {
            assert_eq!(
                object, &first_object,
                "case {case:?}: {surface} must address the same archive as {first_surface}, \
                 got {object:?} against {first_object:?}"
            );
        }
    }
}

/// One layer this suite publishes versions of, for the instant table.
struct InstantRow {
    case: &'static str,
    layer: &'static str,
    /// One `PublishLayerVersion` body per version, oldest first.
    versions: Vec<Value>,
    /// Whether the newest version declares [`RUNTIME`] and [`ARCHITECTURE`], so a FILTERED
    /// `ListLayers` reports it too.
    filterable: bool,
}

fn instant_rows() -> Vec<InstantRow> {
    // A version published with no optional member at all, and one carrying the members a
    // filtered listing selects on: the instant's form is a property of neither.
    let plain = json!({ "Content": { "ZipFile": zip_file(&archive_bytes(80)) } });
    let declared = json!({
        "Content": { "ZipFile": zip_file(&archive_bytes(144)) },
        "Description": "the newest version of a layer that declares compatibility",
        "CompatibleRuntimes": [RUNTIME],
        "CompatibleArchitectures": [ARCHITECTURE],
    });
    vec![
        InstantRow {
            case: "a layer holding one version",
            layer: "lyr-instants-single",
            versions: vec![plain.clone()],
            filterable: false,
        },
        InstantRow {
            case: "a layer holding three versions, the newest declaring compatibility",
            layer: "lyr-instants-multi",
            versions: vec![plain.clone(), plain.clone(), declared],
            filterable: true,
        },
    ]
}

/// RULE: every instant a layer read or listing reports carries the numeric UTC offset the
/// model's own example spells, and a published version's instant is one value every surface
/// reports — the publish, `GetLayerVersion`, `GetLayerVersionByArn`, the `ListLayerVersions`
/// entry and the `ListLayers` entry's `LatestMatchingVersion`, filtered or not.
#[tokio::test]
async fn every_layer_instant_carries_the_numeric_utc_offset_on_every_surface() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_at(&engine, BASE);

    for row in instant_rows() {
        let case = row.case;

        // What each published version's `CreatedDate` is, read off the publish that stored
        // it: a version is immutable, so every later read of it reports this value.
        let mut created: Vec<(i64, String)> = Vec::new();
        for body in &row.versions {
            let published = publish_layer_version(&router, row.layer, body).await;
            let publish_body = served(case, "PublishLayerVersion", &published);
            let version = publish_body
                .get("Version")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("case {case:?}: the publish must report a Version"));
            created.push((
                version,
                offset_instant(case, "PublishLayerVersion", &publish_body, "/CreatedDate"),
            ));
        }

        for (version, instant) in &created {
            let version_arn = format!(
                "arn:aws:lambda:{REGION}:{ACCOUNT}:layer:{}:{version}",
                row.layer
            );
            let reads: Vec<(String, Answer)> = vec![
                (
                    format!("GetLayerVersion of version {version}"),
                    get_layer_version(&router, row.layer, *version).await,
                ),
                (
                    format!("GetLayerVersionByArn of version {version}"),
                    get_layer_version_by_arn(&router, &version_arn).await,
                ),
            ];
            for (surface, answer) in &reads {
                let body = served(case, surface, answer);
                let reported = offset_instant(case, surface, &body, "/CreatedDate");
                assert_eq!(
                    &reported, instant,
                    "case {case:?}: {surface} must report the stored CreatedDate {instant:?}, \
                     got {reported:?}"
                );
            }
        }

        // `ListLayerVersions` reports every version the layer holds, each with its own
        // instant.
        let listed = served(
            case,
            "ListLayerVersions",
            &list_layer_versions(&router, row.layer).await,
        );
        let entries = listed
            .get("LayerVersions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: ListLayerVersions must report LayerVersions, got {listed}")
            });
        assert_eq!(
            entries.len(),
            created.len(),
            "case {case:?}: expected {} listed versions, got {}",
            created.len(),
            entries.len()
        );
        for (version, instant) in &created {
            let entry = entries
                .iter()
                .find(|entry| entry.get("Version").and_then(Value::as_i64) == Some(*version))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: ListLayerVersions must report version {version}, got \
                         {listed}"
                    )
                });
            let surface = format!("the ListLayerVersions entry for version {version}");
            let reported = offset_instant(case, &surface, entry, "/CreatedDate");
            assert_eq!(
                &reported, instant,
                "case {case:?}: {surface} must report the stored CreatedDate {instant:?}, got \
                 {reported:?}"
            );
        }

        // `ListLayers` reports the NEWEST version of the layer, whose instant is that
        // version's own — through an unfiltered listing, and through one filtered to the
        // runtime and architecture that version declares.
        let (newest_version, newest_instant) = created
            .last()
            .cloned()
            .expect("every row publishes at least one version");
        let mut queries: Vec<String> = vec![String::new()];
        if row.filterable {
            queries.push(format!(
                "?CompatibleRuntime={RUNTIME}&CompatibleArchitecture={ARCHITECTURE}"
            ));
        }
        for query in &queries {
            let surface = if query.is_empty() {
                "the ListLayers entry".to_string()
            } else {
                format!("the ListLayers entry under {query}")
            };
            let body = served(case, &surface, &list_layers(&router, query).await);
            let entry = body
                .get("Layers")
                .and_then(Value::as_array)
                .and_then(|layers| {
                    layers
                        .iter()
                        .find(|entry| {
                            entry.get("LayerName").and_then(Value::as_str) == Some(row.layer)
                        })
                        .cloned()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: {surface} must report the layer {}, got {body}",
                        row.layer
                    )
                });
            let listed_version = entry
                .pointer("/LatestMatchingVersion/Version")
                .and_then(Value::as_i64);
            assert_eq!(
                listed_version,
                Some(newest_version),
                "case {case:?}: {surface} must report version {newest_version} as the newest, \
                 got {listed_version:?}"
            );
            let reported =
                offset_instant(case, &surface, &entry, "/LatestMatchingVersion/CreatedDate");
            assert_eq!(
                reported, newest_instant,
                "case {case:?}: {surface} must report the stored CreatedDate \
                 {newest_instant:?}, got {reported:?}"
            );
        }
    }
}
