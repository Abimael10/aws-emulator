// SPDX-License-Identifier: CC0-1.0

//! How this data plane WORDS the refusals it answers from a handler, and on which paths
//! it reaches them, end to end over HTTP through the router this service mounts in
//! production.
//!
//! The rules these cases assert:
//!
//! * The broker's over-size refusal states the published PAYLOAD CEILING and nothing of
//!   the request that crossed it: one fixed sentence, the same for every over-size
//!   message, so a client matching it reads one number — the limit it has to shrink to.
//! * The broker's topic-name rules belong to the paths that DELIVER a message. A
//!   retained-message read resolves the topic as a store key and nothing else, so every
//!   name holding no retained message — one carrying an MQTT subscription wildcard or
//!   past the broker's part or byte ceiling just as much as a well-formed one — is
//!   `ResourceNotFoundException` (HTTP 404) with the service's fixed not-found sentence,
//!   never `InvalidRequestException` (HTTP 400). The 400-vs-404 distinction is the whole
//!   signal: a 400 would tell a caller to stop retrying a probe that simply found
//!   nothing, and a publish to the same name keeps answering its own 400.
//! * A shadow name in this service's reserved `$` namespace is not a name a client may
//!   address — on the write path or on either read path — and is refused
//!   `InvalidRequestException` (HTTP 400) naming the shadow the request asked for. Only
//!   the FIRST character is reserved, so a `$` later in a name is ordinary.
//!
//! Each table spans what its rule generalises over — payload size, wildcard kind and
//! position, ceiling crossed, shadow-name form, and which operation addresses the shadow
//! — and the recorded case that filed the issue is one row among them. Every limit comes
//! from the crate (`state::MAX_PUBLISH_PAYLOAD_BYTES`, `state::MAX_TOPIC_PARTS`,
//! `state::MAX_TOPIC_BYTES`) so no row can drift from the rule the handler enforces,
//! while the SENTENCES are written out here, independently of the renderers under test.

use serde_json::{json, Value};

use crate::publish_topic_and_payload_limits::{encode_component, retained_topics};
use crate::shadow_and_retained_message::{Answer, Harness};
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::{
    MAX_PUBLISH_PAYLOAD_BYTES, MAX_TOPIC_BYTES, MAX_TOPIC_PARTS,
};

/// The `message` of a response body.
fn message(answer: &Answer) -> String {
    answer
        .json()
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A valid shadow state document, so no shadow row is refused for its body.
fn state_document() -> Vec<u8> {
    serde_json::to_vec(&json!({"state": {"reported": {"edge": "ok"}}})).expect("serializable")
}

/// A `/`-separated topic name of `parts` levels.
fn name_of_parts(parts: usize) -> String {
    (0..parts)
        .map(|index| format!("p{index}"))
        .collect::<Vec<String>>()
        .join("/")
}

#[tokio::test]
async fn an_oversize_payload_refusal_states_the_ceiling_and_nothing_of_the_request() {
    let harness = Harness::new();
    // The sentence the broker answers an over-size message with, written out here rather
    // than taken from the renderer under test: fixed text and the published ceiling.
    let expected = format!("payload larger than {MAX_PUBLISH_PAYLOAD_BYTES} bytes");

    for (case, size) in [
        ("one byte past the quota", MAX_PUBLISH_PAYLOAD_BYTES + 1),
        ("two bytes past the quota", MAX_PUBLISH_PAYLOAD_BYTES + 2),
        ("a kilobyte past the quota", MAX_PUBLISH_PAYLOAD_BYTES + 1024),
        ("twice the quota", 2 * MAX_PUBLISH_PAYLOAD_BYTES),
    ] {
        let answer = harness
            .publish(&format!("over-{size}"), 1, true, None, vec![0x50_u8; size])
            .await;
        assert_eq!(
            answer.status,
            413,
            "case {case:?}: expected 413 for {size} bytes, got {} with body {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            "RequestEntityTooLargeException",
            "case {case:?}: expected RequestEntityTooLargeException, got {}",
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            message(&answer),
            expected,
            "case {case:?}: expected the refusal naming only the ceiling, got {:?}",
            message(&answer)
        );
        // Every row answers the SAME sentence, so it cannot carry the size that row sent.
        assert!(
            !message(&answer).contains(&size.to_string()),
            "case {case:?}: expected nothing of the request's {size} bytes in the refusal, \
             got {:?}",
            message(&answer)
        );
    }
}

/// One topic name the broker will not take, and the message it is refused with.
struct MalformedTopic {
    name: &'static str,
    topic: String,
    expected: String,
}

fn malformed_topics() -> Vec<MalformedTopic> {
    // The two ceilings' sentences, written out here with the crate's own limits in them.
    let too_deep = format!("Topic can't contain more than {MAX_TOPIC_PARTS} parts");
    let too_long = format!("Topic can't contain more than {MAX_TOPIC_BYTES} bytes");
    // A topic carrying an MQTT subscription wildcard is a topic FILTER, not a name.
    let filter = "Invalid topic filter".to_string();
    vec![
        MalformedTopic {
            // The recorded case: a multi-level wildcard on the leaf of a base topic.
            name: "a trailing multi-level wildcard under a base topic",
            topic: "fleet-telemetry/#".to_string(),
            expected: filter.clone(),
        },
        MalformedTopic {
            name: "a bare multi-level wildcard",
            topic: "#".to_string(),
            expected: filter.clone(),
        },
        MalformedTopic {
            name: "a bare single-level wildcard",
            topic: "+".to_string(),
            expected: filter.clone(),
        },
        MalformedTopic {
            name: "a single-level wildcard between levels",
            topic: "fleet-telemetry/+/temperature".to_string(),
            expected: filter.clone(),
        },
        MalformedTopic {
            name: "a single-level wildcard inside a level",
            topic: "fleet+telemetry".to_string(),
            expected: filter.clone(),
        },
        MalformedTopic {
            name: "a multi-level wildcard inside a level",
            topic: "fleet#telemetry".to_string(),
            expected: filter,
        },
        MalformedTopic {
            name: "one level past the part ceiling",
            topic: name_of_parts(MAX_TOPIC_PARTS + 1),
            expected: too_deep.clone(),
        },
        MalformedTopic {
            name: "far past the part ceiling",
            topic: name_of_parts(3 * MAX_TOPIC_PARTS),
            expected: too_deep,
        },
        MalformedTopic {
            name: "one byte past the byte ceiling",
            topic: "y".repeat(MAX_TOPIC_BYTES + 1),
            expected: too_long.clone(),
        },
        MalformedTopic {
            name: "far past the byte ceiling",
            topic: "z".repeat(4 * MAX_TOPIC_BYTES),
            expected: too_long,
        },
        MalformedTopic {
            // The ceiling counts BYTES, so a two-byte character costs two of them.
            name: "multi-byte characters past the byte ceiling in bytes, not in characters",
            topic: "é".repeat(MAX_TOPIC_BYTES / 2 + 1),
            expected: format!("Topic can't contain more than {MAX_TOPIC_BYTES} bytes"),
        },
        MalformedTopic {
            name: "past both ceilings at once",
            topic: name_of_parts(MAX_TOPIC_PARTS + 1) + &"z".repeat(MAX_TOPIC_BYTES),
            expected: format!("Topic can't contain more than {MAX_TOPIC_PARTS} parts"),
        },
        MalformedTopic {
            name: "a wildcard on a name that is also too deep",
            topic: name_of_parts(MAX_TOPIC_PARTS + 2) + "/#",
            expected: "Invalid topic filter".to_string(),
        },
    ]
}

/// The service's fixed answer when a topic holds no retained message, written out here
/// rather than taken from the renderer under test.
const RETAINED_MESSAGE_NOT_FOUND: &str = "Retained message not found";

#[tokio::test]
async fn a_retained_message_read_reports_absence_for_a_name_the_broker_would_not_publish_to() {
    let harness = Harness::new();

    // A legal sibling under the same base, so the contrast is between two reads that
    // differ only in whether the name is a topic name at all.
    let stored = harness
        .publish(
            &encode_component("fleet-telemetry/leaf"),
            1,
            true,
            None,
            b"leaf".to_vec(),
        )
        .await;
    assert_eq!(
        stored.status,
        200,
        "the setup publish must be accepted, got {} with body {}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );

    for case in malformed_topics() {
        let encoded = encode_component(&case.topic);
        // The write path's verdict…
        let published = harness
            .publish(&encoded, 1, true, None, b"attempt".to_vec())
            .await;
        assert_eq!(
            published.status,
            400,
            "case {:?}: expected the publish to be refused 400, got {} with body {}",
            case.name,
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        assert_eq!(
            message(&published),
            case.expected,
            "case {:?}: expected the publish refusal {:?}, got {:?}",
            case.name,
            case.expected,
            message(&published)
        );

        // …and the read path's, which reports the store and not the broker's rules: the
        // name the publish was refused for can hold nothing, so it is absent.
        let read = harness.get_retained_message(&encoded).await;
        assert_eq!(
            read.status,
            404,
            "case {:?}: expected the read of a name no publish could store to be 404, got \
             {} with body {}",
            case.name,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {:?}: expected ResourceNotFoundException on the read, got {}",
            case.name,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            message(&read),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {:?}: expected the fixed not-found sentence, got {:?}",
            case.name,
            message(&read)
        );
        // The not-found answer is one fixed sentence: it carries neither the name the
        // caller asked about nor the ceiling the publish path would have named.
        assert!(
            !message(&read).contains(&case.topic) && message(&read) != case.expected,
            "case {:?}: expected nothing of the request or of the publish refusal in the \
             not-found answer, got {:?}",
            case.name,
            message(&read)
        );
        // …and the refused publish retained nothing under that name, which the listing
        // observes independently of the read.
        let listed = retained_topics(&harness).await;
        assert!(
            !listed.contains(&case.topic),
            "case {:?}: a refused publish must retain nothing, got the topic among {listed:?}",
            case.name
        );
    }

    // A name the broker WOULD publish to and that holds nothing answers exactly the
    // same, so the read path draws no distinction between the two kinds of absence.
    for (case, topic) in [
        ("a legal sibling under the same base", "fleet-telemetry/never"),
        ("a single-character name", "q"),
        ("a name at the part ceiling", &name_of_parts(MAX_TOPIC_PARTS)),
        ("a name at the byte ceiling", &"w".repeat(MAX_TOPIC_BYTES)),
        ("a multi-byte name", "temperatur-mäßig/温度"),
    ] {
        let read = harness.get_retained_message(&encode_component(topic)).await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: expected a well-formed absent name to be 404, got {} with body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            message(&read),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {case:?}: expected the absent-message answer, got {:?}",
            message(&read)
        );
    }

    // …and the stored sibling still reads back, so no row above broke the read path.
    let read = harness
        .get_retained_message(&encode_component("fleet-telemetry/leaf"))
        .await;
    assert_eq!(
        read.status,
        200,
        "expected the stored retained message to still be readable, got {} with body {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        read.json().get("topic").and_then(Value::as_str),
        Some("fleet-telemetry/leaf"),
        "expected the stored topic back, got {}",
        String::from_utf8_lossy(&read.body)
    );
}

/// One topic name, and whether the broker's PUBLISH path takes it. The read path's
/// answer is not a function of this flag: it is a function of the store alone.
struct ReadKeyCase {
    name: String,
    topic: String,
    publishable: bool,
}

/// Rows spanning what the rule generalises over — how many `/`-separated levels the name
/// carries, how many bytes it is, and whether it carries an MQTT wildcard — with a row at
/// each published ceiling and past it. The recorded case, a name one level past the part
/// ceiling, is one row among them.
fn read_key_cases() -> Vec<ReadKeyCase> {
    let mut cases = Vec::new();
    for parts in [
        1,
        MAX_TOPIC_PARTS - 1,
        MAX_TOPIC_PARTS,
        MAX_TOPIC_PARTS + 1,
        MAX_TOPIC_PARTS + 4,
        3 * MAX_TOPIC_PARTS,
    ] {
        cases.push(ReadKeyCase {
            name: format!("{parts} `/`-separated level(s)"),
            topic: name_of_parts(parts),
            publishable: parts <= MAX_TOPIC_PARTS,
        });
    }
    for bytes in [
        1,
        MAX_TOPIC_BYTES - 1,
        MAX_TOPIC_BYTES,
        MAX_TOPIC_BYTES + 1,
        4 * MAX_TOPIC_BYTES,
    ] {
        cases.push(ReadKeyCase {
            name: format!("a single level of {bytes} byte(s)"),
            topic: "n".repeat(bytes),
            publishable: bytes <= MAX_TOPIC_BYTES,
        });
    }
    for (name, topic) in [
        ("a single-level wildcard between levels", "sensors/+/temperature"),
        ("a trailing multi-level wildcard", "sensors/#"),
    ] {
        cases.push(ReadKeyCase {
            name: name.to_string(),
            topic: topic.to_string(),
            publishable: false,
        });
    }
    cases
}

#[tokio::test]
async fn a_retained_message_read_answers_from_the_store_whatever_the_topics_shape() {
    let harness = Harness::new();

    for case in read_key_cases() {
        let encoded = encode_component(&case.topic);

        // Nothing is retained yet, so every name — one the broker would publish to and
        // one it would not — is absent, with the one fixed not-found sentence.
        let before = harness.get_retained_message(&encoded).await;
        assert_eq!(
            before.status,
            404,
            "case {:?}: expected an empty store to answer 404 for every name, got {} with \
             body {}",
            case.name,
            before.status,
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            before.error_code(),
            "ResourceNotFoundException",
            "case {:?}: expected ResourceNotFoundException, got {}",
            case.name,
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            message(&before),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {:?}: expected the fixed not-found sentence, got {:?}",
            case.name,
            message(&before)
        );

        let published = harness
            .publish(&encoded, 1, true, None, b"stored".to_vec())
            .await;
        let after = harness.get_retained_message(&encoded).await;
        if !case.publishable {
            // The write path keeps its own verdict…
            assert_eq!(
                published.status,
                400,
                "case {:?}: expected the publish to be refused 400, got {} with body {}",
                case.name,
                published.status,
                String::from_utf8_lossy(&published.body)
            );
            // …and the read still reports the store, which the refusal left empty.
            assert_eq!(
                after.status,
                404,
                "case {:?}: expected the read to stay 404 after the refused publish, got {} \
                 with body {}",
                case.name,
                after.status,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                after.error_code(),
                "ResourceNotFoundException",
                "case {:?}: expected ResourceNotFoundException, got {}",
                case.name,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                message(&after),
                RETAINED_MESSAGE_NOT_FOUND,
                "case {:?}: expected the fixed not-found sentence, got {:?}",
                case.name,
                message(&after)
            );
            continue;
        }

        assert_eq!(
            published.status,
            200,
            "case {:?}: expected the broker to take the name, got {} with body {}",
            case.name,
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        assert_eq!(
            after.status,
            200,
            "case {:?}: expected the stored retained message to be readable, got {} with \
             body {}",
            case.name,
            after.status,
            String::from_utf8_lossy(&after.body)
        );
        assert_eq!(
            after.json().get("topic").and_then(Value::as_str),
            Some(case.topic.as_str()),
            "case {:?}: expected the stored topic back, got {}",
            case.name,
            String::from_utf8_lossy(&after.body)
        );

        // An empty retained payload clears the topic, and the SAME key is absent again:
        // the read tracks the store in both directions.
        let cleared = harness.publish(&encoded, 1, true, None, Vec::new()).await;
        assert_eq!(
            cleared.status,
            200,
            "case {:?}: expected the clearing publish to be taken, got {} with body {}",
            case.name,
            cleared.status,
            String::from_utf8_lossy(&cleared.body)
        );
        let gone = harness.get_retained_message(&encoded).await;
        assert_eq!(
            gone.status,
            404,
            "case {:?}: expected the cleared topic to be absent, got {} with body {}",
            case.name,
            gone.status,
            String::from_utf8_lossy(&gone.body)
        );
        assert_eq!(
            gone.error_code(),
            "ResourceNotFoundException",
            "case {:?}: expected ResourceNotFoundException, got {}",
            case.name,
            String::from_utf8_lossy(&gone.body)
        );
    }
}

/// Shadow names in the reserved `$` namespace, which no request may address. Every one
/// is inside the modelled `shadowName` bounds (length 1..64, pattern
/// `^[$a-zA-Z0-9:_-]+$` — `smithy_query(query="operation",
/// operation="UpdateThingShadow")`), so each reaches the handler and the namespace rule
/// is what refuses it.
const RESERVED_SHADOW_NAMES: &[&str] = &[
    "$",
    "$aws",
    "$aws-fleet-telemetry",
    "$shadow:1",
    "$_internal",
    "$0",
];

/// Shadow names the service does address: a `$` anywhere but the first character is
/// ordinary, and so is every name without one.
const ORDINARY_SHADOW_NAMES: &[&str] = &["a", "sensor", "fleet$a", "edge:1_b-2", "x$"];

#[tokio::test]
async fn a_reserved_shadow_name_is_refused_on_every_path_that_addresses_a_shadow() {
    let harness = Harness::new();

    for name in RESERVED_SHADOW_NAMES {
        let expected = format!("Invalid shadow name : {name}");
        let uri = format!(
            "/things/reservedThing/shadow?name={}",
            encode_component(name)
        );
        for (operation, method, body) in [
            ("UpdateThingShadow", "POST", state_document()),
            ("GetThingShadow", "GET", Vec::new()),
            ("DeleteThingShadow", "DELETE", Vec::new()),
        ] {
            let answer = harness.send(method, &uri, &[], body).await;
            let case = format!("{operation} addressing the reserved shadow name {name:?}");
            assert_eq!(
                answer.status,
                400,
                "case {case:?}: expected 400, got {} with body {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException, got {}",
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                message(&answer),
                expected,
                "case {case:?}: expected the reserved-name refusal {expected:?}, got {:?}",
                message(&answer)
            );
        }
    }

    // A refused update stores nothing, so the thing has no named shadow to list — an
    // empty listing, not an absence.
    let listed = harness.list_named_shadows("reservedThing", "").await;
    assert_eq!(
        listed.status,
        200,
        "expected the empty listing of a thing holding no shadow, got {} with body {}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    assert_eq!(
        listed
            .json()
            .get("results")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "expected no named shadows after the refused updates, got body {}",
        String::from_utf8_lossy(&listed.body)
    );
}

#[tokio::test]
async fn an_ordinary_shadow_name_round_trips_whatever_its_form() {
    let harness = Harness::new();

    for name in ORDINARY_SHADOW_NAMES {
        let uri = format!("/things/ordinaryThing/shadow?name={}", encode_component(name));
        let accepted = harness.send("POST", &uri, &[], state_document()).await;
        assert_eq!(
            accepted.status,
            200,
            "case {name:?}: expected the update to be accepted, got {} with body {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        assert_eq!(
            accepted.json().pointer("/version").and_then(Value::as_i64),
            Some(1),
            "case {name:?}: expected the first version of this shadow name, got {}",
            String::from_utf8_lossy(&accepted.body)
        );

        // Round trip: the reader answers from what the update stored.
        let read = harness.send("GET", &uri, &[], Vec::new()).await;
        assert_eq!(
            read.status,
            200,
            "case {name:?}: expected the stored shadow to be readable, got {} with body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.json().pointer("/state/reported/edge").and_then(Value::as_str),
            Some("ok"),
            "case {name:?}: expected the stored state back, got {}",
            String::from_utf8_lossy(&read.body)
        );
    }

    // Every accepted name is one of the thing's named shadows, in the listing's order.
    let listed = harness.list_named_shadows("ordinaryThing", "").await;
    assert_eq!(
        listed.status,
        200,
        "expected the thing's named shadows to be listed, got {} with body {}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    let names: Vec<String> = listed
        .json()
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for name in ORDINARY_SHADOW_NAMES {
        assert!(
            names.contains(&(*name).to_string()),
            "case {name:?}: expected the accepted shadow name among {names:?}"
        );
    }
}
