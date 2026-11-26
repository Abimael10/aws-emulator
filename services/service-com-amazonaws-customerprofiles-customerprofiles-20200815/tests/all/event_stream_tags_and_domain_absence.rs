// SPDX-License-Identifier: CC0-1.0

//! The two wire shapes of the event-stream surface this suite pins: the `Tags` member of
//! the create and read projections, and the sentence those operations word an absent
//! domain with.
//!
//! RULE 1: a tag-carrying projection of one event stream (`CreateEventStream`,
//! `GetEventStream`) reports `Tags` for as long as the stream exists — the map the create
//! carried, and PRESENT and empty for a stream created with none — so a caller that
//! indexes `Tags` can tell "this stream carries no tags" from "tags were not reported".
//! `Tags` is optional in both response shapes
//! (`smithy_query(query="operation", operation="CreateEventStream,GetEventStream")`), and
//! this is the convention the domain, calculated-attribute and segment-definition
//! projections of this service already follow.
//!
//! RULE 2: every operation bound under `/domains/{DomainName}/event-streams` words an
//! absent domain with that surface's own sentence — "Domain with name <DomainName> not
//! found", whose single variable part is the `{DomainName}` label of the request, verbatim
//! — rather than the sentence the domain operations themselves or the upload-job surface
//! use for the same absence.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the domain create are this crate's one test harness,
//! in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};

/// A destination `Uri` for a stream named `name`: the member is "The StreamARN of the
/// destination to deliver profile events to", persisted as stated and reported unchanged.
fn destination(name: &str) -> String {
    format!("arn:aws:kinesis:us-east-1:000000000000:stream/{name}")
}

/// `CreateEventStream` over the wire:
/// `POST /domains/{DomainName}/event-streams/{EventStreamName}`.
async fn create(emulator: &Emulator, domain: &str, name: &str, body: Value) -> (u16, Value) {
    emulator
        .post(&format!("/domains/{domain}/event-streams/{name}"), body)
        .await
}

/// `GetEventStream` over the wire:
/// `GET /domains/{DomainName}/event-streams/{EventStreamName}`.
async fn get(emulator: &Emulator, domain: &str, name: &str) -> (u16, Value) {
    emulator
        .send(
            "GET",
            &format!("/domains/{domain}/event-streams/{name}"),
            None,
        )
        .await
}

/// `Tags` as a projection of a stream created by `request` must report it: the map the
/// request supplied, or the empty map.
fn expected_tags(request: &Value) -> Value {
    request.get("Tags").cloned().unwrap_or_else(|| json!({}))
}

/// RULE 1 over the ways a create can carry tags: the create response and the read both
/// report the map, empty for a stream that carries none.
#[tokio::test]
async fn both_event_stream_projections_report_tags_empty_when_the_stream_carries_none() {
    let emulator = Emulator::new();
    emulator.create_domain("tagproj", 90).await;

    // The longest tag key and value the model admits: `TagKey` is @length(1..128) and
    // `TagValue` @length(0..256)
    // (`smithy_query(query="shape", shape="TagKey,TagValue")`).
    let longest_key = "k".repeat(128);
    let longest_value = "v".repeat(256);
    // The most tags one map may carry: `TagMap` is @length(1..50) on CreateEventStream's
    // `Tags` (`smithy_query(query="operation", operation="CreateEventStream")`).
    let fifty_tags: Value = Value::Object(
        (0..50)
            .map(|index| (format!("key{index}"), json!(format!("value{index}"))))
            .collect(),
    );

    // The rows differ along what the rule generalises over: whether the request carries
    // tags at all, how many it carries, and the shape of the keys and values in it.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "no Tags member at all",
            "Untagged".to_string(),
            json!({ "Uri": destination("untagged") }),
        ),
        (
            "one tag",
            "One_Tag".to_string(),
            json!({ "Uri": destination("one-tag"), "Tags": { "team": "crm" } }),
        ),
        (
            "several tags",
            "Several-Tags".to_string(),
            json!({
                "Uri": destination("several-tags"),
                "Tags": { "team": "crm", "cost-center": "42", "env": "prod" },
            }),
        ),
        (
            "a tag whose value is the empty string",
            "Empty_Value".to_string(),
            json!({ "Uri": destination("empty-value"), "Tags": { "team": "" } }),
        ),
        (
            "the longest key and value the model admits",
            "Longest_Tag".to_string(),
            json!({
                "Uri": destination("longest-tag"),
                "Tags": { longest_key.as_str(): longest_value.as_str() },
            }),
        ),
        (
            "as many tags as the map admits",
            "Fifty_Tags".to_string(),
            json!({ "Uri": destination("fifty-tags"), "Tags": fifty_tags }),
        ),
    ];

    for (case, name, request) in &rows {
        let expected = expected_tags(request);

        let (status, created) = create(&emulator, "tagproj", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateEventStream 200, got {status} {created:?}"
        );
        assert_eq!(
            created.get("Tags"),
            Some(&expected),
            "case {case:?}: expected the create to report Tags {expected:?}, got {:?}",
            created.get("Tags")
        );

        let (status, read) = get(&emulator, "tagproj", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetEventStream 200, got {status} {read:?}"
        );
        assert_eq!(
            read.get("Tags"),
            Some(&expected),
            "case {case:?}: expected the read to report Tags {expected:?}, got {:?}",
            read.get("Tags")
        );
    }
}

/// RULE 2 over every operation of the surface and over the shapes a domain name takes: an
/// absent domain is `ResourceNotFoundException` (404) worded by the event-stream surface.
#[tokio::test]
async fn an_absent_domain_is_worded_by_the_event_stream_surface() {
    let emulator = Emulator::new();
    // A domain that IS stored, so what the rows below exercise is the absence of the name
    // they address and not an empty store.
    emulator.create_domain("stored_domain", 90).await;

    // The rows differ along the shape of the name the request's `{DomainName}` label
    // carries: `name` is @length(1..64) with pattern ^[a-zA-Z0-9_-]+$
    // (`smithy_query(query="operation", operation="CreateEventStream")`), so each of these
    // is a syntactically valid name the store does not hold.
    let longest_domain = "D".repeat(64);
    let domains: Vec<(&str, String)> = vec![
        ("a name that was never created", "nowhere".to_string()),
        (
            "the stored name, differing in case",
            "Stored_Domain".to_string(),
        ),
        ("the shortest name the model admits", "d".to_string()),
        ("the longest name the model admits", longest_domain),
        ("a hyphenated name", "no-such-domain".to_string()),
        ("an underscored name", "no_such_domain".to_string()),
    ];

    for (case, domain) in &domains {
        let expected = format!("Domain with name {domain} not found");
        let name = "Any_Name";
        let refusals = [
            (
                "CreateEventStream",
                create(
                    &emulator,
                    domain,
                    name,
                    json!({ "Uri": destination("anything") }),
                )
                .await,
            ),
            ("GetEventStream", get(&emulator, domain, name).await),
            (
                "ListEventStreams",
                emulator
                    .send("GET", &format!("/domains/{domain}/event-streams"), None)
                    .await,
            ),
            (
                "DeleteEventStream",
                emulator
                    .send(
                        "DELETE",
                        &format!("/domains/{domain}/event-streams/{name}"),
                        None,
                    )
                    .await,
            ),
        ];

        for (operation, (status, refused)) in refusals {
            assert_eq!(
                status, 404,
                "case {case:?}: expected {operation} 404, got {status} {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException from {operation}, got \
                 {refused:?}"
            );
            assert_eq!(
                error_message(&refused),
                expected,
                "case {case:?}: expected {expected:?} from {operation}, got {:?}",
                error_message(&refused)
            );
        }
    }

    // The sentence is the SURFACE's, not this service's only one: the same absent name
    // addressed through the domain itself and through the upload-job surface keeps each of
    // those wordings, so the distinction AWS draws between them survives.
    let (status, refused) = emulator.send("GET", "/domains/nowhere", None).await;
    assert_eq!(
        (status, error_message(&refused)),
        (404, "Domain nowhere is not found"),
        "case \"GetDomain\": expected the domain surface's own sentence, got {refused:?}"
    );
}
