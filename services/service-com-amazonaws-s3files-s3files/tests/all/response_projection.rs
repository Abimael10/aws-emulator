// SPDX-License-Identifier: CC0-1.0

//! How a stored S3 Files resource is PROJECTED onto the members a response reports,
//! end to end over HTTP.
//!
//! The suite reuses `file_system_lifecycle`'s harness, so every case goes through the
//! same `MultiServiceRouter` the emulator boots: routing, the restJson1 wire shapes and
//! the modelled response statuses are all exercised.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. A response member reporting the tags a resource carries is ALWAYS serialized: the
//!    stored set in order, and an EMPTY list for a resource no write has tagged.
//!    `TagList`'s `@length(1, 50)` bounds the tag set a tag WRITE may carry, not the
//!    collection a read reports, so no operation ever drops the member.
//! 2. A reported `creationTime` is a WHOLE-second epoch value — `creationTime` is
//!    documented "in seconds since 1970-01-01T00:00:00Z (Unix epoch time)" and
//!    restJson1 encodes a timestamp as epoch seconds — and it is the same value whichever
//!    operation reports the file system, because they all project one stored record.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::file_system_lifecycle::{
    call, create_access_point, create_file_system, new_router, Answer, ROLE_ARN,
};

/// `TagList`'s maximum element count, `@length(min: 1, max: 50)` (smithy_query: shape
/// `TagList`).
const MAX_TAGS: usize = 50;

/// The earliest instant a creation this test just made can plausibly carry, so a
/// `creationTime` reported in milliseconds, or as a count of something other than
/// seconds, cannot pass as epoch seconds. 2020-01-01T00:00:00Z.
const EPOCH_FLOOR: u64 = 1_577_836_800;

/// The furthest instant it can plausibly carry (2100-01-01T00:00:00Z): an epoch value in
/// milliseconds would land far beyond it.
const EPOCH_CEILING: u64 = 4_102_444_800;

/// One `Tag` as the wire carries it.
fn tag(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": value })
}

/// The table rule 1 is asserted over: how the case is named, and the `tags` member the
/// create carries (`None` for a create that names none at all). The rows differ along
/// everything the rule generalises over — whether the member is sent, how many tags it
/// carries (from one to `TagList`'s maximum), whether one of them is the `Name` tag the
/// service derives `name` from, and the tag values' own shape (an empty value is legal:
/// `TagValue` is `@length(max: 256)` with pattern `^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$`).
fn tag_projection_table() -> Vec<(&'static str, Option<Value>)> {
    let many: Vec<Value> = (0..MAX_TAGS)
        .map(|index| tag(&format!("key-{index}"), &format!("value-{index}")))
        .collect();
    vec![
        ("no tags member at all", None),
        ("a single tag", Some(json!([tag("env", "test")]))),
        ("only the Name tag", Some(json!([tag("Name", "projected")]))),
        (
            "three tags, one carrying an empty value",
            Some(json!([
                tag("Name", "three"),
                tag("team", "storage"),
                tag("note", ""),
            ])),
        ),
        (
            "TagList's maximum of 50 tags",
            Some(Value::Array(many)),
        ),
    ]
}

/// `CreateFileSystem` for one row of the table, on its own bucket so the rows cannot
/// collide.
async fn create_for(router: &MultiServiceRouter, row: usize, tags: Option<&Value>) -> Answer {
    let mut request = json!({
        "bucket": format!("arn:aws:s3:::s3files-projection-{row}"),
        "roleArn": ROLE_ARN,
    });
    if let Some(tags) = tags {
        request["tags"] = tags.clone();
    }
    create_file_system(router, request).await
}

/// The epoch seconds a reported `creationTime` carries, asserting rule 2: the value is a
/// WHOLE number of seconds (never a fractional reading of the service's clock) and lies
/// in the range a creation this test just made can fall in.
fn whole_epoch_seconds(case: &str, operation: &str, reported: &Value) -> u64 {
    let seconds = match reported {
        Value::Number(number) if number.is_u64() => number.as_u64().unwrap_or_default(),
        other => panic!(
            "case {case:?}: {operation} must report creationTime as a whole number of \
             epoch seconds, got {other:?}"
        ),
    };
    assert!(
        (EPOCH_FLOOR..EPOCH_CEILING).contains(&seconds),
        "case {case:?}: {operation} must report creationTime in seconds since the epoch, \
         got {seconds}"
    );
    seconds
}

/// Rule 1 and rule 2 over the file-system projection: every operation that reports a
/// file system serializes `tags` — empty when the file system carries none — and the one
/// whole-second `creationTime` the stored record holds.
#[tokio::test]
async fn every_file_system_response_serializes_tags_and_a_whole_second_creation_time() {
    let router = new_router();
    // What each row created, so the listing can be checked against all of them at once.
    let mut created_rows: Vec<(&str, String, u64)> = Vec::new();

    for (row, (case, tags)) in tag_projection_table().into_iter().enumerate() {
        let expected_tags = tags.clone().unwrap_or_else(|| json!([]));

        let created = create_for(&router, row, tags.as_ref()).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected 201 from CreateFileSystem, got {} / {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.field("tags"),
            &expected_tags,
            "case {case:?}: CreateFileSystem must serialize tags, got {:?}",
            created.field("tags")
        );
        let created_seconds = whole_epoch_seconds(case, "CreateFileSystem", created.field("creationTime"));

        let id = created.text("fileSystemId");
        let read = call(&router, "GET", &format!("/file-systems/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetFileSystem, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.field("tags"),
            &expected_tags,
            "case {case:?}: GetFileSystem must serialize tags, got {:?}",
            read.field("tags")
        );
        assert_eq!(
            whole_epoch_seconds(case, "GetFileSystem", read.field("creationTime")),
            created_seconds,
            "case {case:?}: the read must report the creationTime the create stored"
        );

        // The tagging read answers from the same stored tag set.
        let listed_tags = call(&router, "GET", &format!("/resource-tags/{id}"), None).await;
        assert_eq!(
            listed_tags.status, 200,
            "case {case:?}: expected 200 from ListTagsForResource, got {} / {:?}",
            listed_tags.status, listed_tags.body
        );
        assert_eq!(
            listed_tags.field("tags"),
            &expected_tags,
            "case {case:?}: ListTagsForResource must serialize tags, got {:?}",
            listed_tags.field("tags")
        );

        created_rows.push((case, id, created_seconds));
    }

    // The listing projects the same records: one whole-second creationTime per file
    // system, equal to the one its own create and read reported.
    let listing = call(&router, "GET", "/file-systems?maxResults=100", None).await;
    assert_eq!(
        listing.status, 200,
        "expected 200 from ListFileSystems, got {} / {:?}",
        listing.status, listing.body
    );
    let described = listing
        .field("fileSystems")
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("expected a fileSystems list, got {:?}", listing.body));
    for (case, id, created_seconds) in &created_rows {
        let item = described
            .iter()
            .find(|item| item["fileSystemId"] == json!(id))
            .unwrap_or_else(|| panic!("case {case:?}: {id} must be listed, got {described:?}"));
        assert_eq!(
            whole_epoch_seconds(case, "ListFileSystems", &item["creationTime"]),
            *created_seconds,
            "case {case:?}: the listing must report the creationTime the create stored"
        );
    }
}

/// Rule 1 over the access-point projection: the resource whose tags the same rule
/// governs, reported by its own create and read.
#[tokio::test]
async fn every_access_point_response_serializes_tags() {
    let router = new_router();
    let parent = create_for(&router, 900, None).await;
    assert_eq!(
        parent.status, 201,
        "expected 201 from CreateFileSystem, got {} / {:?}",
        parent.status, parent.body
    );
    let file_system_id = parent.text("fileSystemId");

    for (case, tags) in tag_projection_table() {
        let expected_tags = tags.clone().unwrap_or_else(|| json!([]));
        let mut extra = json!({});
        if let Some(tags) = &tags {
            extra["tags"] = tags.clone();
        }

        let created = create_access_point(&router, &file_system_id, extra).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateAccessPoint, got {} / {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.field("tags"),
            &expected_tags,
            "case {case:?}: CreateAccessPoint must serialize tags, got {:?}",
            created.field("tags")
        );

        let id = created.text("accessPointId");
        let read = call(&router, "GET", &format!("/access-points/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetAccessPoint, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.field("tags"),
            &expected_tags,
            "case {case:?}: GetAccessPoint must serialize tags, got {:?}",
            read.field("tags")
        );

        let listed_tags = call(&router, "GET", &format!("/resource-tags/{id}"), None).await;
        assert_eq!(
            listed_tags.field("tags"),
            &expected_tags,
            "case {case:?}: ListTagsForResource must serialize an access point's tags, \
             got {:?}",
            listed_tags.field("tags")
        );
    }
}
