// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the event-stream surface of a domain: `CreateEventStream` and the
//! readers and the delete that serve what it stored (`GetEventStream`,
//! `ListEventStreams`, `DeleteEventStream`).
//!
//! The rule under test: an event stream is a resource of its domain, identified by the
//! (domain, `EventStreamName`) pair. A create persists the destination `Uri` the request
//! names and the tags it carries, mints the stream's ARN once, and answers from the record
//! it stored; the readers report that same record — the stream is `RUNNING` with a
//! `HEALTHY` destination and reports no instant for a stop or an unhealthy change that
//! never happened — and reading changes nothing. A name the domain already holds is
//! refused `BadRequestException` (400) with the stored stream untouched, a delete removes
//! the stream and nothing else, and a domain, or a stream in it, that is not stored is
//! `ResourceNotFoundException` (404).
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    ARN_SERVICE, EVENT_STREAM_DESTINATION_HEALTHY, EVENT_STREAM_STATE_RUNNING,
};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};

/// The region the test harness signs for, which is the region a minted ARN carries.
const REGION: &str = "us-east-1";

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

/// `ListEventStreams` over the wire: `GET /domains/{DomainName}/event-streams`, with the
/// paging parameters the model binds to the query string (`max-results`, `next-token`).
async fn list(
    emulator: &Emulator,
    domain: &str,
    max_results: Option<u32>,
    next_token: Option<&str>,
) -> (u16, Value) {
    let mut query = Vec::new();
    if let Some(max) = max_results {
        query.push(format!("max-results={max}"));
    }
    if let Some(token) = next_token {
        query.push(format!("next-token={token}"));
    }
    let path = if query.is_empty() {
        format!("/domains/{domain}/event-streams")
    } else {
        format!("/domains/{domain}/event-streams?{}", query.join("&"))
    };
    emulator.send("GET", &path, None).await
}

/// `DeleteEventStream` over the wire:
/// `DELETE /domains/{DomainName}/event-streams/{EventStreamName}`.
async fn delete(emulator: &Emulator, domain: &str, name: &str) -> (u16, Value) {
    emulator
        .send(
            "DELETE",
            &format!("/domains/{domain}/event-streams/{name}"),
            None,
        )
        .await
}

/// The ARN a stream of `domain` named `name` is minted with: this service's ARN namespace
/// and the resource path the stream's own operations are bound to.
fn expected_arn(domain: &str, name: &str) -> String {
    engine::emulator_arn(
        ARN_SERVICE,
        REGION,
        &format!("domains/{domain}/event-streams/{name}"),
    )
}

/// The `EventStreamName`s one listing reports, in the order it reported them.
fn listed_names(body: &Value) -> Vec<String> {
    body.get("Items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("EventStreamName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a create persists the destination and the tags the request carried, mints the ARN
/// once, and answers from the record it stored, so the reader reports the same stream —
/// running, with a healthy destination and no instant for a change that never happened —
/// and reading it changes nothing.
#[tokio::test]
async fn a_created_event_stream_reads_back_as_it_was_stored() {
    let emulator = Emulator::new();
    emulator.create_domain("streamdom", 90).await;

    // The longest `EventStreamName` the model admits: its `@length(max: 64)`.
    let longest_name = "A".repeat(64);

    // The rows differ along what the rule generalises over: the form of the destination
    // ARN (partition, region, account, stream name), the shape of the stream's own name,
    // and whether the request carries tags.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a stream carrying only the members a create requires",
            "OrderEvents",
            json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/orders" }),
        ),
        (
            "a hyphenated name, a destination in another region, and tags",
            "profile-events",
            json!({
                "Uri": "arn:aws:kinesis:eu-west-1:123456789012:stream/profile-events",
                "Tags": { "team": "crm", "cost-center": "42" },
            }),
        ),
        (
            "an underscored name and a destination in another partition",
            "profile_events_2",
            json!({
                "Uri": "arn:aws-cn:kinesis:cn-north-1:210987654321:stream/profile_events_2",
                "Tags": { "team": "analytics" },
            }),
        ),
        (
            "the longest name the model admits",
            &longest_name,
            json!({ "Uri": "arn:aws:kinesis:us-west-2:555555555555:stream/long-name" }),
        ),
    ];

    for (case, name, request) in &rows {
        let (status, created) = create(&emulator, "streamdom", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateEventStream 200, got {status} {created:?}"
        );

        // The create answers from the record it stored: the ARN it minted for the name it
        // was addressed by, and the tag map the request carried.
        let arn = expected_arn("streamdom", name);
        assert_eq!(
            created.get("EventStreamArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected EventStreamArn {arn:?}, got {:?}",
            created.get("EventStreamArn")
        );
        assert_eq!(
            created.get("Tags"),
            request.get("Tags"),
            "case {case:?}: expected the Tags the request carried ({:?}), got {:?}",
            request.get("Tags"),
            created.get("Tags")
        );

        // The reader serves that same record.
        let (status, read) = get(&emulator, "streamdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetEventStream 200, got {status} {read:?}"
        );
        let expected_destination = json!({
            "Uri": request.get("Uri").cloned().unwrap_or(Value::Null),
            "Status": EVENT_STREAM_DESTINATION_HEALTHY,
        });
        for (member, expected) in [
            ("DomainName", json!("streamdom")),
            ("EventStreamArn", json!(arn)),
            ("State", json!(EVENT_STREAM_STATE_RUNNING)),
            ("DestinationDetails", expected_destination),
        ] {
            assert_eq!(
                read.get(member),
                Some(&expected),
                "case {case:?}: expected {member} = {expected:?}, got {:?}",
                read.get(member)
            );
        }
        assert_eq!(
            read.get("Tags"),
            request.get("Tags"),
            "case {case:?}: expected the stored Tags ({:?}), got {:?}",
            request.get("Tags"),
            read.get("Tags")
        );
        // Nothing stopped the stream and nothing found its destination unhealthy, so
        // neither instant is reported.
        assert_eq!(
            read.get("StoppedSince"),
            None,
            "case {case:?}: expected no StoppedSince, got {:?}",
            read.get("StoppedSince")
        );
        let created_at = read.get("CreatedAt").cloned().unwrap_or(Value::Null);
        assert!(
            created_at.is_number(),
            "case {case:?}: expected a CreatedAt timestamp, got {created_at:?}"
        );

        // A read reports, so a second one answers exactly the same.
        let (status, again) = get(&emulator, "streamdom", name).await;
        assert_eq!(
            (status, &again),
            (200, &read),
            "case {case:?}: a read must not change what a later caller sees"
        );
    }
}

/// RULE: a listing reports every stream of the domain it addressed, each from the same
/// record the reader serves, in `EventStreamName` order, and `MaxResults` with the
/// `NextToken` of the previous page walks that order exactly once.
#[tokio::test]
async fn a_listing_reports_the_streams_of_its_domain_in_name_order() {
    let emulator = Emulator::new();
    emulator.create_domain("listdom", 90).await;
    emulator.create_domain("otherdom", 90).await;

    // Created out of order, so what is asserted is the ORDER the listing imposes.
    let created_order = ["zulu", "alpha", "Mike", "bravo"];
    for name in created_order {
        let (status, body) = create(
            &emulator,
            "listdom",
            name,
            json!({
                "Uri": format!("arn:aws:kinesis:us-east-1:000000000000:stream/{name}"),
                "Tags": { "stream": name },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected CreateEventStream 200, got {status} {body:?}"
        );
    }
    // The identity is the PAIR, so another domain's stream of the same name is a
    // different stream and belongs to no listing but its own.
    let (status, body) = create(
        &emulator,
        "otherdom",
        "alpha",
        json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/elsewhere" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"another domain's stream\": expected 200, got {status} {body:?}"
    );

    let mut expected: Vec<String> = created_order.iter().map(|name| name.to_string()).collect();
    expected.sort();

    let (status, listed) = list(&emulator, "listdom", None, None).await;
    assert_eq!(
        status, 200,
        "case \"one page\": expected ListEventStreams 200, got {status} {listed:?}"
    );
    assert_eq!(
        listed_names(&listed),
        expected,
        "case \"one page\": expected the domain's streams in name order, got {:?}",
        listed_names(&listed)
    );
    assert_eq!(
        listed.get("NextToken"),
        None,
        "case \"one page\": expected no NextToken on the last page, got {:?}",
        listed.get("NextToken")
    );

    // Each item is the same record the reader serves.
    for name in &expected {
        let (_, read) = get(&emulator, "listdom", name).await;
        let item = listed
            .get("Items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("EventStreamName").and_then(Value::as_str) == Some(name))
            })
            .cloned()
            .unwrap_or(Value::Null);
        for (member, expected_value) in [
            ("DomainName", read.get("DomainName").cloned()),
            ("EventStreamArn", read.get("EventStreamArn").cloned()),
            ("State", read.get("State").cloned()),
            ("Tags", read.get("Tags").cloned()),
        ] {
            assert_eq!(
                item.get(member).cloned(),
                expected_value,
                "case {name:?}: expected the listing's {member} to be the reader's \
                 ({expected_value:?}), got {:?}",
                item.get(member)
            );
        }
        let destination = read
            .get("DestinationDetails")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for member in ["Uri", "Status"] {
            assert_eq!(
                item.get("DestinationSummary").and_then(|s| s.get(member)),
                destination.get(member),
                "case {name:?}: expected the summary's {member} to be the reader's ({:?}), \
                 got {:?}",
                destination.get(member),
                item.get("DestinationSummary").and_then(|s| s.get(member))
            );
        }
    }

    // Paging walks that same order exactly once, whatever the page size.
    for page_size in [1_u32, 2, 3, expected.len() as u32, 100] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (status, page) = list(&emulator, "listdom", Some(page_size), token.as_deref()).await;
            assert_eq!(
                status, 200,
                "case {page_size:?}: expected ListEventStreams 200, got {status} {page:?}"
            );
            let names = listed_names(&page);
            assert!(
                names.len() <= page_size as usize,
                "case {page_size:?}: expected at most {page_size} items, got {names:?}"
            );
            walked.extend(names);
            match page.get("NextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case {page_size:?}: expected paging to walk the name order exactly once, got \
             {walked:?}"
        );
    }

    // A listing reports, so a second one answers exactly the same.
    let (status, again) = list(&emulator, "listdom", None, None).await;
    assert_eq!(
        (status, &again),
        (200, &listed),
        "case \"a repeated listing\": a read must not change what a later caller sees"
    );

    // The other domain reports only its own stream.
    let (status, elsewhere) = list(&emulator, "otherdom", None, None).await;
    assert_eq!(
        (status, listed_names(&elsewhere)),
        (200, vec!["alpha".to_string()]),
        "case \"another domain\": expected only that domain's own stream, got {elsewhere:?}"
    );
}

/// RULE: the (domain, name) pair identifies one stream, so a create of a name the domain
/// already holds is refused `BadRequestException` (400) and leaves the stored stream's
/// destination and tags untouched.
#[tokio::test]
async fn an_event_stream_name_identifies_one_stream_in_its_domain() {
    let emulator = Emulator::new();
    emulator.create_domain("conflictdom", 90).await;

    let rows: Vec<(&str, &str, Value, Value)> = vec![
        (
            "a loser naming another destination",
            "Shared_Name",
            json!({
                "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/winner",
                "Tags": { "team": "crm" },
            }),
            json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/loser" }),
        ),
        (
            "a loser naming the same destination",
            "Another-Shared",
            json!({ "Uri": "arn:aws:kinesis:eu-west-1:123456789012:stream/same" }),
            json!({
                "Uri": "arn:aws:kinesis:eu-west-1:123456789012:stream/same",
                "Tags": { "team": "analytics" },
            }),
        ),
    ];

    for (case, name, winner, loser) in &rows {
        let (status, created) = create(&emulator, "conflictdom", name, winner.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to win, got {status} {created:?}"
        );

        let (status, refused) = create(&emulator, "conflictdom", name, loser.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a name the domain holds, got {status} \
             {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {refused:?}"
        );
        let expected = format!("Event stream with name {name} already exists");
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: expected the already-exists sentence {expected:?}, got {:?}",
            error_message(&refused)
        );

        // The refused create changed nothing: the read still reports the winner's
        // destination and tags.
        let (status, read) = get(&emulator, "conflictdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the winner still readable, got {status} {read:?}"
        );
        assert_eq!(
            read.get("DestinationDetails").and_then(|d| d.get("Uri")),
            winner.get("Uri"),
            "case {case:?}: the refused create must leave the winner's Uri ({:?}), got {:?}",
            winner.get("Uri"),
            read.get("DestinationDetails").and_then(|d| d.get("Uri"))
        );
        assert_eq!(
            read.get("Tags"),
            winner.get("Tags"),
            "case {case:?}: the refused create must leave the winner's Tags ({:?}), got {:?}",
            winner.get("Tags"),
            read.get("Tags")
        );
    }
}

/// RULE: a delete removes the stream it names and nothing else, so the reader and the
/// listing no longer report it while every other stream of the domain still reads back,
/// and a second delete of the same name has nothing left to remove.
#[tokio::test]
async fn a_deleted_event_stream_is_gone_and_its_siblings_remain() {
    let emulator = Emulator::new();
    emulator.create_domain("deldom", 90).await;

    let names = ["first", "second", "third"];
    for name in names {
        let (status, body) = create(
            &emulator,
            "deldom",
            name,
            json!({ "Uri": format!("arn:aws:kinesis:us-east-1:000000000000:stream/{name}") }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected CreateEventStream 200, got {status} {body:?}"
        );
    }

    let mut remaining: Vec<String> = names.iter().map(|name| name.to_string()).collect();
    remaining.sort();

    for name in names {
        let (status, deleted) = delete(&emulator, "deldom", name).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected DeleteEventStream 200, got {status} {deleted:?}"
        );

        remaining.retain(|held| held != name);

        // The stream is gone from the reader and from the listing.
        let (status, read) = get(&emulator, "deldom", name).await;
        assert_eq!(
            status, 404,
            "case {name:?}: expected the deleted stream to be absent, got {status} {read:?}"
        );
        let (status, listed) = list(&emulator, "deldom", None, None).await;
        assert_eq!(
            (status, listed_names(&listed)),
            (200, remaining.clone()),
            "case {name:?}: expected the remaining streams {remaining:?}, got {listed:?}"
        );

        // Nothing is left to delete, so a second delete of the same name is refused.
        let (status, refused) = delete(&emulator, "deldom", name).await;
        assert_eq!(
            status, 404,
            "case {name:?}: expected a second delete to find nothing, got {status} {refused:?}"
        );
        let expected = format!("EventStream {name} is not found");
        assert_eq!(
            error_message(&refused),
            expected,
            "case {name:?}: expected {expected:?}, got {:?}",
            error_message(&refused)
        );

        // Every sibling still reads back, destination and all.
        for held in &remaining {
            let (status, read) = get(&emulator, "deldom", held).await;
            assert_eq!(
                status, 200,
                "case {name:?}: expected the sibling {held:?} untouched, got {status} {read:?}"
            );
            let uri = format!("arn:aws:kinesis:us-east-1:000000000000:stream/{held}");
            assert_eq!(
                read.get("DestinationDetails")
                    .and_then(|d| d.get("Uri"))
                    .and_then(Value::as_str),
                Some(uri.as_str()),
                "case {name:?}: expected the sibling {held:?} to keep its destination {uri:?}, \
                 got {:?}",
                read.get("DestinationDetails").and_then(|d| d.get("Uri"))
            );
        }
    }
}

/// RULE: a reference to a resource that is not stored is `ResourceNotFoundException`
/// (404), worded for the resource that is missing — the domain, or the stream in it.
#[tokio::test]
async fn an_absent_domain_or_event_stream_is_not_found() {
    let emulator = Emulator::new();
    emulator.create_domain("here", 90).await;

    let request = json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/anything" });

    for (case, domain) in [
        ("a domain that was never created", "nowhere"),
        ("a domain whose name differs in case", "Here"),
    ] {
        for (operation, (status, refused)) in [
            (
                "CreateEventStream",
                create(&emulator, domain, "Any_Name", request.clone()).await,
            ),
            ("GetEventStream", get(&emulator, domain, "Any_Name").await),
            ("ListEventStreams", list(&emulator, domain, None, None).await),
            (
                "DeleteEventStream",
                delete(&emulator, domain, "Any_Name").await,
            ),
        ] {
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
            let expected = format!("Domain {domain} is not found");
            assert_eq!(
                error_message(&refused),
                expected,
                "case {case:?}: expected {expected:?} from {operation}, got {:?}",
                error_message(&refused)
            );
        }
    }

    // The domain exists, the stream does not: the refusal names the stream.
    let (status, created) = create(&emulator, "here", "Stored", request.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the stored stream\": expected 200, got {status} {created:?}"
    );
    for (case, name) in [
        ("a stream that was never created", "Absent"),
        ("a stream whose name differs in case", "stored"),
    ] {
        for (operation, (status, refused)) in [
            ("GetEventStream", get(&emulator, "here", name).await),
            ("DeleteEventStream", delete(&emulator, "here", name).await),
        ] {
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
            let expected = format!("EventStream {name} is not found");
            assert_eq!(
                error_message(&refused),
                expected,
                "case {case:?}: expected {expected:?} from {operation}, got {:?}",
                error_message(&refused)
            );
        }
    }

    // A domain that holds no stream at all lists none, rather than being refused.
    emulator.create_domain("empty", 90).await;
    let (status, listed) = list(&emulator, "empty", None, None).await;
    assert_eq!(
        (status, listed_names(&listed)),
        (200, Vec::<String>::new()),
        "case \"a domain holding no stream\": expected an empty listing, got {listed:?}"
    );
}

/// RULE: a tag key outside the bounds `TagKey` declares is refused `BadRequestException`
/// (400) and nothing is persisted — the key is the one half of a tag map the request
/// boundary does not validate.
#[tokio::test]
async fn a_tag_key_outside_the_declared_bounds_is_refused() {
    let emulator = Emulator::new();
    emulator.create_domain("tagdom", 90).await;

    // The rows are the three ways the key shape's own constraints are broken: the
    // reserved prefix its pattern's lookahead excludes, a character the pattern admits
    // nowhere, and a key past its declared maximum length.
    let rows: Vec<(&str, &str, String)> = vec![
        ("the reserved prefix", "Reserved", "aws:owner".to_string()),
        ("a character the pattern admits nowhere", "Spaced", "team name".to_string()),
        ("past the declared maximum length", "TooLong", "k".repeat(129)),
    ];

    for (case, name, key) in &rows {
        let (status, refused) = create(
            &emulator,
            "tagdom",
            name,
            json!({
                "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/tagged",
                "Tags": { key.as_str(): "crm" },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {refused:?}"
        );

        // Nothing was persisted, so the reader holds no such stream.
        let (status, read) = get(&emulator, "tagdom", name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused create to persist nothing, got {status} \
             {read:?}"
        );
    }
}
