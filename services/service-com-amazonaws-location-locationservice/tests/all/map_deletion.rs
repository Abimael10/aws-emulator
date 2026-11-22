// SPDX-License-Identifier: CC0-1.0

//! Retiring a map resource, end to end over the wire.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine: a map that stops answering
//! after the delete stopped answering because it left PERSISTED state, not because
//! a handler branched on this request.
//!
//! RULE under test: a delete of a named control-plane resource retires it
//! permanently, so every later call that resolves that name — the resource's own
//! read, a read derived from it, the tag plane, and a second delete — resolves
//! nothing and is refused with `ResourceNotFoundException` (HTTP 404, the
//! `x-amzn-errortype` header naming the shape), while the resources the delete did
//! not name keep everything they held and the retired name is free to create again.
//!
//! The table spans what the rule generalises over: the punctuation and the lengths
//! `ResourceName` admits (`@length 1..100`, `^[-._\w]+$`, so a single character and
//! the 100-character bound are both rows), the kinds of basemap a map can carry
//! (vector, raster, hybrid and open-data styles), and whether the retired map held
//! tags. The map the issue report replays is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one HTTP harness this crate's suites share, declared from `tests/all.rs`.
use crate::harness::{call, emulator, encode, millis_of, Answer, ACCOUNT, REGION};

/// One row: a map to retire, and the map kept beside it.
struct Case {
    case: &'static str,
    /// The name the row deletes. `String` because one row is built to the
    /// published length bound.
    name: String,
    /// The style the retired map carries.
    style: &'static str,
    /// The tags it was created with.
    tags: &'static [(&'static str, &'static str)],
}

/// The table. Each row differs from the others along the name's shape, the style's
/// basemap kind, and the tag map the retired map held.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a vector map with no tags",
            name: "retired".to_string(),
            style: "VectorEsriStreets",
            tags: &[],
        },
        Case {
            case: "a tagged raster map whose name carries every admitted punctuation",
            name: "A_b-c.1".to_string(),
            style: "RasterEsriImagery",
            tags: &[("env", "staging"), ("team", "fleet")],
        },
        Case {
            case: "a hybrid map named by a single character",
            // `ResourceName` is `@length 1..100`: the shallow bound itself.
            name: "x".to_string(),
            style: "HybridHereExploreSatellite",
            tags: &[("owner", "maps")],
        },
        Case {
            case: "an open-data map named at the longest admitted length",
            // `ResourceName` is `@length 1..100`: the published maximum.
            name: "m".repeat(100),
            style: "VectorOpenDataStandardDark",
            tags: &[],
        },
    ]
}

/// The map kept beside the retired one in every row, so a delete is shown to retire
/// the map it names and nothing else.
const BYSTANDER: &str = "kept_map.2";
const BYSTANDER_STYLE: &str = "VectorHereExplore";
const BYSTANDER_TAG: (&str, &str) = ("generation", "first");

fn tags_body(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

async fn create_map(router: &MultiServiceRouter, name: &str, style: &str, tags: &[(&str, &str)]) {
    let mut body = json!({ "MapName": name, "Configuration": { "Style": style } });
    if !tags.is_empty() {
        body["Tags"] = tags_body(tags);
    }
    let created = call(router, "POST", "/maps/v0/maps", Some(body)).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}/{style:?}: CreateMap must succeed, got {} {:?}",
        created.status, created.body
    );
}

async fn describe_map(router: &MultiServiceRouter, name: &str) -> Answer {
    call(router, "GET", &format!("/maps/v0/maps/{name}"), None).await
}

async fn delete_map(router: &MultiServiceRouter, name: &str) -> Answer {
    call(router, "DELETE", &format!("/maps/v0/maps/{name}"), None).await
}

/// Every path that resolves a map name, for the reads this crate serves from a
/// stored map resource: the resource's own read, its style descriptor, one tile of
/// its pyramid, and both halves of its sprite sheet.
fn map_resolving_paths(name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("DescribeMap", format!("/maps/v0/maps/{name}")),
        (
            "GetMapStyleDescriptor",
            format!("/maps/v0/maps/{name}/style-descriptor"),
        ),
        ("GetMapTile", format!("/maps/v0/maps/{name}/tiles/0/0/0")),
        (
            "GetMapSprites (json)",
            format!("/maps/v0/maps/{name}/sprites/sprites.json"),
        ),
        (
            "GetMapSprites (png)",
            format!("/maps/v0/maps/{name}/sprites/sprites.png"),
        ),
    ]
}

/// The status, the wire error CODE and the whole message the shared tagging frontend in
/// front of the ARN-addressed tag operations answers for an ARN that resolves to no
/// resource: its own `NotFoundException` and one fixed sentence, rather than the modelled
/// `ResourceNotFoundException` and the map-naming sentence a call that named the map in
/// its own URI earns. Written out here rather than read off the crate, so this suite pins
/// the wire contract itself.
const TAG_PLANE_NOT_FOUND_CODE: &str = "NotFoundException";
const TAG_PLANE_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// A refusal's status, wire error code, `x-amzn-errortype` header and message, for a
/// call that named the map in its own URI: the MODELLED `ResourceNotFoundException`.
fn assert_not_found(answer: &Answer, name: &str, case: &str) {
    assert_refused_as_missing(
        answer,
        "ResourceNotFoundException",
        &format!("Map not found: {name}."),
        case,
    );
}

/// The same class from the ARN-addressed tag plane, which the shared tagging frontend
/// serves: one route over every taggable resource, so it answers that frontend's own error
/// code and its single fixed sentence instead of naming the map.
fn assert_tag_plane_not_found(answer: &Answer, case: &str) {
    assert_refused_as_missing(
        answer,
        TAG_PLANE_NOT_FOUND_CODE,
        TAG_PLANE_NOT_FOUND_MESSAGE,
        case,
    );
}

/// One 404: the status, the header, the wire code and the sentence the caller expects.
fn assert_refused_as_missing(answer: &Answer, code: &str, message: &str, case: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404, got {} {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.error_type.is_some(),
        "case {case:?}: the refusal must carry the x-amzn-errortype header, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected error code {code}, got {:?} ({:?})",
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.message(),
        message,
        "case {case:?}: expected message {message:?}, got {:?}",
        answer.message()
    );
}

/// The `Tags` member of a response, sorted, or `None` when the member is absent.
fn reported_tags(body: &Value) -> Option<Vec<(String, String)>> {
    let mut tags: Vec<(String, String)> = body
        .get("Tags")?
        .as_object()?
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
        .collect();
    tags.sort();
    Some(tags)
}

#[tokio::test]
async fn a_delete_retires_the_map_so_every_later_read_of_the_name_is_refused() {
    for row in cases() {
        let router = emulator();
        let name = row.name.as_str();
        create_map(&router, name, row.style, row.tags).await;
        create_map(&router, BYSTANDER, BYSTANDER_STYLE, &[BYSTANDER_TAG]).await;

        // The positive control: before the delete, every path that resolves the
        // name is served.
        for (op, path) in map_resolving_paths(name) {
            let served = call(&router, "GET", &path, None).await;
            assert_eq!(
                served.status, 200,
                "case {:?} ({op}): the map must be readable before the delete, got {} {:?}",
                row.case, served.status, served.body
            );
        }

        let deleted = delete_map(&router, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {:?}: DeleteMap must succeed, got {} {:?}",
            row.case, deleted.status, deleted.body
        );

        // Every way back into the retired map is refused: its own read, the reads
        // derived from its stored configuration, and a second delete of the name.
        for (op, path) in map_resolving_paths(name) {
            let refused = call(&router, "GET", &path, None).await;
            assert_not_found(&refused, name, &format!("{}: {op} after delete", row.case));
        }
        assert_not_found(
            &delete_map(&router, name).await,
            name,
            &format!("{}: delete after delete", row.case),
        );

        // The tag plane resolves the same stored map, so the retired map's ARN
        // names nothing there either.
        let arn = format!("arn:aws:geo:{REGION}:{ACCOUNT}:map/{name}");
        let encoded_arn = encode(&arn);
        assert_tag_plane_not_found(
            &call(&router, "GET", &format!("/tags/{encoded_arn}"), None).await,
            &format!("{}: ListTagsForResource after delete", row.case),
        );
        assert_tag_plane_not_found(
            &call(
                &router,
                "POST",
                &format!("/tags/{encoded_arn}"),
                Some(json!({ "Tags": { "late": "tag" } })),
            )
            .await,
            &format!("{}: TagResource after delete", row.case),
        );

        // The map the delete did not name keeps everything it held.
        let kept = describe_map(&router, BYSTANDER).await;
        assert_eq!(
            kept.status, 200,
            "case {:?}: a map the delete did not name must still be readable, got {} {:?}",
            row.case, kept.status, kept.body
        );
        assert_eq!(
            reported_tags(&kept.json()),
            Some(vec![(
                BYSTANDER_TAG.0.to_string(),
                BYSTANDER_TAG.1.to_string()
            )]),
            "case {:?}: a map the delete did not name must keep its tags, got {:?}",
            row.case,
            kept.json().get("Tags")
        );
    }
}

#[tokio::test]
async fn deleting_a_name_no_map_is_stored_under_is_refused_and_retires_nothing() {
    let router = emulator();
    // One map exists, so the refusals below are about the requested name rather
    // than about an empty store.
    create_map(&router, "Exists", "VectorEsriTopographic", &[]).await;

    // Names that satisfy `ResourceName`'s `^[-._\w]+$` but name no map, including
    // case variants of the one that does exist: the lookup is by exact name.
    for name in ["missing", "exists", "EXISTS", "Exists.2", "x", "a_b-c.1"] {
        assert_not_found(
            &delete_map(&router, name).await,
            name,
            &format!("delete {name:?}"),
        );
    }

    // A refused delete retires nothing.
    let still_there = describe_map(&router, "Exists").await;
    assert_eq!(
        still_there.status, 200,
        "a refused delete must leave the stored map readable, got {} {:?}",
        still_there.status, still_there.body
    );
}

#[tokio::test]
async fn a_retired_map_name_is_free_again_and_reads_back_as_the_new_resource() {
    let router = emulator();
    create_map(
        &router,
        "recycled",
        "VectorEsriStreets",
        &[("generation", "first")],
    )
    .await;
    let first = describe_map(&router, "recycled").await.json();

    let deleted = delete_map(&router, "recycled").await;
    assert_eq!(
        deleted.status, 200,
        "DeleteMap must succeed, got {} {:?}",
        deleted.status, deleted.body
    );

    // The name is free: the create is accepted rather than refused as a conflict.
    create_map(&router, "recycled", "VectorGrabStandardDark", &[]).await;

    let again = describe_map(&router, "recycled").await;
    assert_eq!(
        again.status, 200,
        "the re-created map must be readable, got {} {:?}",
        again.status, again.body
    );
    let again = again.json();
    // What the name reads back is the NEW map: nothing the retired one held
    // survived the delete.
    assert_eq!(
        again
            .get("Configuration")
            .and_then(|configuration| configuration.get("Style"))
            .and_then(Value::as_str),
        Some("VectorGrabStandardDark"),
        "the re-created map must report its own Style, got {again}"
    );
    assert_eq!(
        again.get("DataSource").and_then(Value::as_str),
        Some("Grab"),
        "the re-created map must report its own DataSource, got {again}"
    );
    assert_eq!(
        reported_tags(&again),
        Some(Vec::new()),
        "the re-created map must hold none of the retired one's tags, got {:?}",
        again.get("Tags")
    );
    assert!(
        millis_of(&again, "CreateTime") >= millis_of(&first, "CreateTime"),
        "the re-created map's CreateTime must be its own, got {:?} against the retired map's {:?}",
        again.get("CreateTime"),
        first.get("CreateTime")
    );
}
