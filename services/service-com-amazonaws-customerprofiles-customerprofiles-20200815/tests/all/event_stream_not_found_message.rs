// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the sentence the event-stream operations refuse an absent stream
//! with.
//!
//! The rule under test: when an EXISTING domain holds no event stream under the name a
//! request addressed, the refusal is `ResourceNotFoundException` (404) whose message is
//! the service's event-stream sentence, naming BOTH the stream and the domain it was
//! looked for in, each taken verbatim from the request's `{EventStreamName}` and
//! `{DomainName}` http labels. It holds for every operation that addresses one stream by
//! that pair (`GetEventStream`, `DeleteEventStream`), and whether the name was never
//! created or was removed by a delete.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing is this crate's one test harness, in
//! `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};

/// The message the service words that refusal with: the fixed text, the absent
/// `EventStreamName`, and the `DomainName` the request addressed.
fn expected_message(domain: &str, name: &str) -> String {
    format!("The EventStream with name {name} not found for domain {domain}")
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

/// Assert one refusal: 404, the modelled error class, and the sentence naming the stream
/// and its domain.
fn assert_refusal(case: &str, operation: &str, domain: &str, name: &str, got: (u16, Value)) {
    let (status, body) = got;
    assert_eq!(
        status, 404,
        "case {case:?}: expected {operation} 404, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException from {operation}, got {body:?}"
    );
    let expected = expected_message(domain, name);
    assert_eq!(
        error_message(&body),
        expected,
        "case {case:?}: expected {operation} message {expected:?}, got {:?}",
        error_message(&body)
    );
}

/// RULE: in an existing domain, an event-stream name the domain does not hold is refused
/// with the event-stream sentence carrying both labels the request named, whichever
/// operation addressed the stream.
#[tokio::test]
async fn an_absent_event_stream_names_itself_and_its_domain() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the shape and length of the
    // domain name and of the stream name, both within the labels' shared
    // `@length(1..64)` / `^[a-zA-Z0-9_-]+$` constraints
    // (`smithy_query(query="operation", operation="GetEventStream")`).
    let longest = "E".repeat(64);
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a plain name in a plain domain", "plaindom", "OrderEvents"),
        (
            "a hyphenated name in an underscored domain",
            "under_score_dom",
            "profile-events",
        ),
        ("single characters", "d", "e"),
        ("the longest name the label admits", "LongNameDom", &longest),
        (
            "digits only, in a domain named with digits",
            "0123456789",
            "42",
        ),
    ];

    for (case, domain, name) in &rows {
        emulator.create_domain(domain, 90).await;

        // A name the domain never held: every operation addressing one stream refuses it
        // with the same sentence.
        assert_refusal(case, "GetEventStream", domain, name, get(&emulator, domain, name).await);
        assert_refusal(
            case,
            "DeleteEventStream",
            domain,
            name,
            delete(&emulator, domain, name).await,
        );

        // The same name after a create and a delete: once the stream is gone, the readers
        // word its absence exactly as they did before it ever existed.
        let (status, created) = emulator
            .post(
                &format!("/domains/{domain}/event-streams/{name}"),
                json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/events" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateEventStream 200, got {status} {created:?}"
        );
        let (status, deleted) = delete(&emulator, domain, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteEventStream 200, got {status} {deleted:?}"
        );
        assert_refusal(
            case,
            "GetEventStream after delete",
            domain,
            name,
            get(&emulator, domain, name).await,
        );
        assert_refusal(
            case,
            "DeleteEventStream after delete",
            domain,
            name,
            delete(&emulator, domain, name).await,
        );
    }
}

/// RULE: the pair identifying a stream is matched as the request spelled it, so a name
/// that differs from a held one only in case is absent, and the sentence echoes the
/// spelling the request carried — not the one the domain holds.
#[tokio::test]
async fn a_held_name_in_another_case_is_absent_and_the_sentence_echoes_the_request() {
    let emulator = Emulator::new();
    emulator.create_domain("casedom", 90).await;

    let held = "Stored";
    let (status, created) = emulator
        .post(
            &format!("/domains/casedom/event-streams/{held}"),
            json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/stored" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {held:?}: expected CreateEventStream 200, got {status} {created:?}"
    );

    // Each row is another spelling of the held name the domain does NOT hold.
    for requested in ["stored", "STORED", "StoreD"] {
        let case = format!("{held:?} held, {requested:?} requested");
        assert_refusal(
            &case,
            "GetEventStream",
            "casedom",
            requested,
            get(&emulator, "casedom", requested).await,
        );
        assert_refusal(
            &case,
            "DeleteEventStream",
            "casedom",
            requested,
            delete(&emulator, "casedom", requested).await,
        );
    }

    // None of those refusals touched the stream the domain does hold.
    let (status, read) = get(&emulator, "casedom", held).await;
    assert_eq!(
        status, 200,
        "case {held:?}: expected the held stream still readable, got {status} {read:?}"
    );
}

/// RULE: the domain in the sentence is the one the request addressed, so the SAME absent
/// stream name read through two different existing domains is refused with two different
/// sentences — and a domain that does hold the name is not refused at all.
#[tokio::test]
async fn the_sentence_names_the_domain_the_request_addressed() {
    let emulator = Emulator::new();
    let domains = ["alpha", "beta", "gamma-3"];
    for domain in domains {
        emulator.create_domain(domain, 90).await;
    }

    let name = "SharedName";

    // Only the first domain holds the name; the others do not.
    let (status, created) = emulator
        .post(
            &format!("/domains/alpha/event-streams/{name}"),
            json!({ "Uri": "arn:aws:kinesis:us-east-1:000000000000:stream/shared" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the holder\": expected CreateEventStream 200, got {status} {created:?}"
    );

    let (status, read) = get(&emulator, "alpha", name).await;
    assert_eq!(
        status, 200,
        "case \"the holder\": expected GetEventStream 200, got {status} {read:?}"
    );

    for domain in ["beta", "gamma-3"] {
        let case = format!("the same name in {domain}");
        assert_refusal(
            &case,
            "GetEventStream",
            domain,
            name,
            get(&emulator, domain, name).await,
        );
    }

    // The two refusals differ, and they differ exactly in the domain they name.
    let (_, beta_body) = get(&emulator, "beta", name).await;
    let (_, gamma_body) = get(&emulator, "gamma-3", name).await;
    let beta = error_message(&beta_body);
    let gamma = error_message(&gamma_body);
    assert_ne!(
        beta, gamma,
        "case \"two domains\": expected the sentence to name the domain addressed, got \
         {beta:?} for both"
    );
    assert!(
        beta.ends_with("beta") && gamma.ends_with("gamma-3"),
        "case \"two domains\": expected each sentence to end in its own domain, got {beta:?} \
         and {gamma:?}"
    );
}
