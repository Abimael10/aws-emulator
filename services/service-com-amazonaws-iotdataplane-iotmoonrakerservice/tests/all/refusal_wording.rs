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
//! * A topic a retained-message read resolves has to BE one topic: a string carrying an
//!   MQTT subscription wildcard matches a set of topics and identifies none, so the read
//!   refuses it `InvalidRequestException` (HTTP 400) with the same sentence the publish
//!   path answers it with. The broker's publish-time part and byte CEILINGS are not
//!   borrowed: they bound what the broker carries, so a name past one of them is a key
//!   the store holds nothing under and is `ResourceNotFoundException` (HTTP 404) with the
//!   service's fixed not-found sentence, exactly as a well-formed absent name is. The
//!   400-vs-404 distinction is the whole signal: a 404 tells a caller its probe found
//!   nothing, while a 400 tells it the string it asked about is no topic to probe.
//! * A `responseTopic` is the topic a responder is told to publish its reply to, so it
//!   has to be one topic too: a reply topic carrying an MQTT subscription wildcard names
//!   none, and the publish is refused `InvalidRequestException` (HTTP 400) with the
//!   service's own fixed sentence for that condition — which is NOT the publish-topic
//!   one — before anything is retained, while a reply topic that names one topic is
//!   carried and the publish retained.
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
//!
//! The topic-identity rule is also asserted as a PROPERTY over GENERATED topic strings,
//! because it is an invariant with two sides that no enumeration can close: a read refuses
//! a string if and only if it carries a wildcard, whatever its depth, its length or its
//! alphabet. The generator (`topic_strategy`) draws from a fixed seed, so the cases are the
//! same on every machine, and the run asserts that the generated set reached each class.

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

/// One topic name the broker will not take, the message it is refused with, and whether
/// the string names ONE topic at all — a name past a ceiling does, a topic filter does
/// not, which is what the read path draws its own line on.
struct MalformedTopic {
    name: &'static str,
    topic: String,
    expected: String,
    one_topic: bool,
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
            one_topic: false,
        },
        MalformedTopic {
            name: "a bare multi-level wildcard",
            topic: "#".to_string(),
            expected: filter.clone(),
            one_topic: false,
        },
        MalformedTopic {
            name: "a bare single-level wildcard",
            topic: "+".to_string(),
            expected: filter.clone(),
            one_topic: false,
        },
        MalformedTopic {
            name: "a single-level wildcard between levels",
            topic: "fleet-telemetry/+/temperature".to_string(),
            expected: filter.clone(),
            one_topic: false,
        },
        MalformedTopic {
            name: "a single-level wildcard inside a level",
            topic: "fleet+telemetry".to_string(),
            expected: filter.clone(),
            one_topic: false,
        },
        MalformedTopic {
            name: "a multi-level wildcard inside a level",
            topic: "fleet#telemetry".to_string(),
            expected: filter,
            one_topic: false,
        },
        MalformedTopic {
            name: "one level past the part ceiling",
            topic: name_of_parts(MAX_TOPIC_PARTS + 1),
            expected: too_deep.clone(),
            one_topic: true,
        },
        MalformedTopic {
            name: "far past the part ceiling",
            topic: name_of_parts(3 * MAX_TOPIC_PARTS),
            expected: too_deep,
            one_topic: true,
        },
        MalformedTopic {
            name: "one byte past the byte ceiling",
            topic: "y".repeat(MAX_TOPIC_BYTES + 1),
            expected: too_long.clone(),
            one_topic: true,
        },
        MalformedTopic {
            name: "far past the byte ceiling",
            topic: "z".repeat(4 * MAX_TOPIC_BYTES),
            expected: too_long,
            one_topic: true,
        },
        MalformedTopic {
            // The ceiling counts BYTES, so a two-byte character costs two of them.
            name: "multi-byte characters past the byte ceiling in bytes, not in characters",
            topic: "é".repeat(MAX_TOPIC_BYTES / 2 + 1),
            expected: format!("Topic can't contain more than {MAX_TOPIC_BYTES} bytes"),
            one_topic: true,
        },
        MalformedTopic {
            name: "past both ceilings at once",
            topic: name_of_parts(MAX_TOPIC_PARTS + 1) + &"z".repeat(MAX_TOPIC_BYTES),
            expected: format!("Topic can't contain more than {MAX_TOPIC_PARTS} parts"),
            one_topic: true,
        },
        MalformedTopic {
            name: "a wildcard on a name that is also too deep",
            topic: name_of_parts(MAX_TOPIC_PARTS + 2) + "/#",
            expected: "Invalid topic filter".to_string(),
            one_topic: false,
        },
    ]
}

/// The service's fixed answer when a topic holds no retained message, written out here
/// rather than taken from the renderer under test.
const RETAINED_MESSAGE_NOT_FOUND: &str = "Retained message not found";

#[tokio::test]
async fn a_retained_message_read_refuses_a_filter_and_reports_absence_past_a_ceiling() {
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

        // …and the read path's, which asks only whether the string names ONE topic. A
        // topic filter names none, so the read refuses it with the publish path's own
        // sentence; a name past a ceiling names one the store simply holds nothing under.
        let read = harness.get_retained_message(&encoded).await;
        let (expected_status, expected_code, expected_message) = if case.one_topic {
            (404, "ResourceNotFoundException", RETAINED_MESSAGE_NOT_FOUND)
        } else {
            (400, "InvalidRequestException", case.expected.as_str())
        };
        assert_eq!(
            read.status,
            expected_status,
            "case {:?}: expected the read of this name to be {expected_status}, got {} with \
             body {}",
            case.name,
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            expected_code,
            "case {:?}: expected {expected_code} on the read, got {}",
            case.name,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            message(&read),
            expected_message,
            "case {:?}: expected {expected_message:?}, got {:?}",
            case.name,
            message(&read)
        );
        // Either answer is one fixed sentence: neither carries the name the caller asked
        // about.
        assert!(
            !message(&read).contains(&case.topic),
            "case {:?}: expected nothing of the request in the refusal, got {:?}",
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

    // A name the broker WOULD publish to and that holds nothing answers the same as one
    // past a ceiling, so the read path draws its line at whether the string names a topic.
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

/// One topic name, whether the broker's PUBLISH path takes it, and whether the string
/// names ONE topic. The read path's answer is not a function of the publish flag: for a
/// name it is a function of the store alone, and for a filter it is the refusal.
struct ReadKeyCase {
    name: String,
    topic: String,
    publishable: bool,
    one_topic: bool,
}

/// Rows spanning what the rule generalises over — how many `/`-separated levels the name
/// carries, how many bytes it is, and whether it carries an MQTT wildcard — with a row at
/// each published ceiling and past it. The recorded cases, a name one level past the part
/// ceiling and a trailing multi-level wildcard, are two rows among them.
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
            one_topic: true,
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
            one_topic: true,
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
            one_topic: false,
        });
    }
    cases
}

#[tokio::test]
async fn a_retained_message_read_answers_from_the_store_for_every_topic_name() {
    let harness = Harness::new();

    for case in read_key_cases() {
        let encoded = encode_component(&case.topic);
        // What a read of this string answers while the store holds nothing for it: a name
        // is absent, a filter is no topic to probe.
        let (empty_status, empty_code, empty_message) = if case.one_topic {
            (404, "ResourceNotFoundException", RETAINED_MESSAGE_NOT_FOUND)
        } else {
            (400, "InvalidRequestException", "Invalid topic filter")
        };

        // Nothing is retained yet, so every NAME — one the broker would publish to and
        // one past a ceiling alike — is absent, with the one fixed not-found sentence.
        let before = harness.get_retained_message(&encoded).await;
        assert_eq!(
            before.status,
            empty_status,
            "case {:?}: expected an empty store to answer {empty_status} for this string, got \
             {} with body {}",
            case.name,
            before.status,
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            before.error_code(),
            empty_code,
            "case {:?}: expected {empty_code}, got {}",
            case.name,
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            message(&before),
            empty_message,
            "case {:?}: expected {empty_message:?}, got {:?}",
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
            // …and the read answers exactly as it did before it, the refusal having
            // stored nothing.
            assert_eq!(
                after.status,
                empty_status,
                "case {:?}: expected the read to stay {empty_status} after the refused \
                 publish, got {} with body {}",
                case.name,
                after.status,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                after.error_code(),
                empty_code,
                "case {:?}: expected {empty_code}, got {}",
                case.name,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                message(&after),
                empty_message,
                "case {:?}: expected {empty_message:?}, got {:?}",
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

/// Strings carrying an MQTT wildcard, spanning which wildcard it is, where it sits
/// (alone, leading, between levels, trailing, inside a level), how many the string
/// carries, and what the rest of the string looks like. The recorded case, a multi-level
/// wildcard on the leaf of a base topic, is one row among them.
pub(crate) const TOPIC_FILTERS: &[(&str, &str)] = &[
    // The recorded case.
    ("a multi-level wildcard on the leaf of a base topic", "fleet-telemetry/#"),
    ("a bare multi-level wildcard", "#"),
    ("a bare single-level wildcard", "+"),
    ("a single-level wildcard as the leaf", "sensors/plant-1/+"),
    ("a single-level wildcard between levels", "sensors/+/temperature"),
    ("a single-level wildcard as the first level", "+/temperature"),
    ("a multi-level wildcard inside a level", "fleet#telemetry"),
    ("a single-level wildcard inside a level", "fleet+telemetry"),
    ("both wildcards in one string", "sensors/+/readings/#"),
    ("two single-level wildcards", "+/+"),
    ("a wildcard past the part ceiling too", "a/b/c/d/e/f/g/h/i/#"),
    ("a wildcard beside a multi-byte level", "temperatur-mäßig/+"),
];

/// Topic NAMES the read path resolves against the store. The first rows carry the
/// characters that NEIGHBOUR `+` (0x2B) and `#` (0x23) in the ASCII table — `*`, `,`, `"`,
/// `$` — so no row can pass by refusing a whole character class, and the rest vary the
/// depth, the width and the alphabet of the name.
pub(crate) const TOPIC_NAMES: &[(&str, &str)] = &[
    ("an asterisk, the character below `+`", "sensors/*/temperature"),
    ("a comma, the character above `+`", "sensors/,/temperature"),
    ("a quote, the character below `#`", "sensors/\"/temperature"),
    ("a dollar outside the first position", "sensors/$batch/temperature"),
    ("a single level", "heartbeat"),
    ("a deep name", "a/b/c/d/e/f/g/h"),
    ("a multi-byte name", "temperatur-mäßig/温度"),
    ("the word hash spelt out", "fleet-telemetry/hash"),
    ("the word plus spelt out", "fleet-telemetry/plus"),
];

#[tokio::test]
async fn a_retained_message_read_refuses_every_topic_filter() {
    let harness = Harness::new();

    // A retained message under the base the first rows filter on, so a refusal cannot be
    // read as "the store is empty": the filter would MATCH this topic if a filter were a
    // key.
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

    for (case, topic) in TOPIC_FILTERS {
        let read = harness.get_retained_message(&encode_component(topic)).await;
        assert_eq!(
            read.status,
            400,
            "case {case:?}: expected the filter {topic:?} to be refused 400, got {} with \
             body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException for {topic:?}, got {}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            message(&read),
            "Invalid topic filter",
            "case {case:?}: expected the filter sentence, got {:?}",
            message(&read)
        );
        // One fixed sentence: it carries nothing of the string the caller sent.
        assert!(
            !message(&read).contains(topic),
            "case {case:?}: expected nothing of {topic:?} in the refusal, got {:?}",
            message(&read)
        );
    }

    // The refused reads neither consumed nor cleared the retained message the filters
    // would have matched: the read path reports, it does not write.
    let read = harness
        .get_retained_message(&encode_component("fleet-telemetry/leaf"))
        .await;
    assert_eq!(
        read.status,
        200,
        "expected the retained message to still be readable, got {} with body {}",
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

#[tokio::test]
async fn a_retained_message_read_resolves_every_topic_name_against_the_store() {
    let harness = Harness::new();

    for (case, topic) in TOPIC_NAMES {
        let encoded = encode_component(topic);
        // Nothing is retained under it yet, so the name is ABSENT — the answer a probe
        // reads as "nothing here", which no name may be refused with instead.
        let absent = harness.get_retained_message(&encoded).await;
        assert_eq!(
            absent.status,
            404,
            "case {case:?}: expected the unretained name {topic:?} to be 404, got {} with \
             body {}",
            absent.status,
            String::from_utf8_lossy(&absent.body)
        );
        assert_eq!(
            absent.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {topic:?}, got {}",
            String::from_utf8_lossy(&absent.body)
        );
        assert_eq!(
            message(&absent),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {case:?}: expected the fixed not-found sentence, got {:?}",
            message(&absent)
        );

        // …and once a publish retains one under the same name, the same read answers from
        // the store, so the name is a key the whole way through.
        let published = harness
            .publish(&encoded, 1, true, None, case.as_bytes().to_vec())
            .await;
        assert_eq!(
            published.status,
            200,
            "case {case:?}: expected the broker to take {topic:?}, got {} with body {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        let read = harness.get_retained_message(&encoded).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the retained message under {topic:?} to be readable, \
             got {} with body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.json().get("topic").and_then(Value::as_str),
            Some(*topic),
            "case {case:?}: expected the stored topic back, got {}",
            String::from_utf8_lossy(&read.body)
        );
        let payload = read
            .json()
            .get("payload")
            .and_then(Value::as_str)
            .map(|encoded| aws_smithy_types::base64::decode(encoded).expect("base64"))
            .unwrap_or_default();
        assert_eq!(
            payload,
            case.as_bytes(),
            "case {case:?}: expected the published payload back, got {:?}",
            String::from_utf8_lossy(&payload)
        );
    }
}

/// The sentence the service refuses a response topic carrying an MQTT wildcard with,
/// written out here rather than taken from the renderer under test. It is FIXED text for
/// this condition: nothing of the request is interpolated into it, and it is not the
/// publish-topic sentence.
pub(crate) const INVALID_RESPONSE_TOPIC: &str =
    "This response topic isn't valid. Response topics must not contain MQTT wildcard \
     characters (+ or #)";

/// A `Publish` that names a `responseTopic`, which the operation binds to the query
/// parameter of the same name (`smithy_query(query="operation", operation="Publish")`).
/// `retain=true`, so a publish that reached the broker is one the store would hold.
async fn publish_with_response_topic(
    harness: &Harness,
    topic: &str,
    response_topic: Option<&str>,
    payload: Vec<u8>,
) -> Answer {
    let mut uri = format!("/topics/{}?qos=1&retain=true", encode_component(topic));
    if let Some(response_topic) = response_topic {
        uri.push_str("&responseTopic=");
        uri.push_str(&encode_component(response_topic));
    }
    harness.send("POST", &uri, &[], payload).await
}

#[tokio::test]
async fn a_publish_whose_response_topic_carries_a_wildcard_is_refused_and_retains_nothing() {
    let harness = Harness::new();

    for (index, (case, response_topic)) in TOPIC_FILTERS.iter().enumerate() {
        // A distinct publish topic per row, so each row's own topic is what the
        // retained-message read below reports on.
        let topic = format!("response-flow/refused-{index}");
        let refused = publish_with_response_topic(
            &harness,
            &topic,
            Some(response_topic),
            b"reply-wanted".to_vec(),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected the reply topic {response_topic:?} to be refused 400, \
             got {} with body {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException for {response_topic:?}, got {}",
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            message(&refused),
            INVALID_RESPONSE_TOPIC,
            "case {case:?}: expected the response-topic sentence, got {:?}",
            message(&refused)
        );
        // The condition is its own: a bad reply topic is not reported as a bad publish
        // topic, which is what a client branching on the wording reads.
        assert_ne!(
            message(&refused),
            "Invalid topic filter",
            "case {case:?}: expected the response-topic sentence rather than the \
             publish-topic one, got {:?}",
            message(&refused)
        );

        // The refusal came before any state change: the publish asked for `retain=true`
        // and its own topic holds nothing.
        let read = harness.get_retained_message(&encode_component(&topic)).await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: expected the refused publish to retain nothing under {topic:?}, \
             got {} with body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {topic:?}, got {}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            message(&read),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {case:?}: expected the fixed not-found sentence, got {:?}",
            message(&read)
        );
    }

    // The same store from the other side: no refused row left a retained message behind.
    let retained = retained_topics(&harness).await;
    assert!(
        retained.is_empty(),
        "expected the refused publishes to retain nothing, got {retained:?}"
    );
}

#[tokio::test]
async fn a_publish_whose_response_topic_names_one_topic_is_accepted_and_retained() {
    let harness = Harness::new();

    // A request that names NO response topic asks for no reply topic at all, and every
    // topic-name row is a name a reply could be published to — including the ones
    // carrying the characters that neighbour `+` and `#`.
    let mut rows: Vec<(&str, Option<&str>)> = vec![("no response topic at all", None)];
    rows.extend(
        TOPIC_NAMES
            .iter()
            .map(|(case, topic)| (*case, Some(*topic))),
    );

    for (index, (case, response_topic)) in rows.into_iter().enumerate() {
        let topic = format!("response-flow/accepted-{index}");
        let payload = format!("payload for {case}").into_bytes();
        let accepted =
            publish_with_response_topic(&harness, &topic, response_topic, payload.clone()).await;
        assert_eq!(
            accepted.status,
            200,
            "case {case:?}: expected the reply topic {response_topic:?} to be taken, got {} \
             with body {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );

        // …and the publish it carried is the topic's retained message, so the accepted
        // leg is backed by the store rather than by the acknowledgement alone.
        let read = harness.get_retained_message(&encode_component(&topic)).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the accepted publish to be retained under {topic:?}, \
             got {} with body {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let stored = read
            .json()
            .get("payload")
            .and_then(Value::as_str)
            .map(|encoded| aws_smithy_types::base64::decode(encoded).expect("base64"))
            .unwrap_or_default();
        assert_eq!(
            stored,
            payload,
            "case {case:?}: expected the published payload back, got {:?}",
            String::from_utf8_lossy(&stored)
        );
    }
}

/// The characters one generated topic level is drawn from: ordinary name characters, the
/// characters that NEIGHBOUR the two MQTT wildcards in the ASCII table (`*`, `,`, `"`),
/// and a two- and a three-byte character so a name's BYTES and its characters differ. The
/// wildcards themselves are NOT in here: whether a generated string carries one is its own
/// dimension ([`topic_strategy`]), so the two classes are drawn in comparable numbers
/// rather than one of them by chance. `$` is left out on purpose: a leading `$` is the
/// reserved system namespace, which is its own rule with its own suite
/// (`super::reserved_topic_namespace`). The shape puts no constraint of its own on a
/// topic (`smithy_query(query="shape",shape="Topic")` reports no length and no pattern),
/// so the service's own rules are the whole of what a generated string is weighed
/// against.
const TOPIC_CHARACTERS: &[char] = &['a', 'b', 'z', '0', '9', '-', '_', '*', ',', '"', 'é', '温'];

/// The two MQTT subscription wildcards — the single-level `+` and the multi-level `#` —
/// one of which a generated string may carry anywhere in it.
const MQTT_WILDCARDS: &[char] = &['+', '#'];

/// The seed the generated cases below are drawn with — the 32 bytes the generator's
/// ChaCha algorithm takes. FIXED, so the suite runs the same cases on every machine, and
/// named in every assertion so a failure says which draw produced it.
const TOPIC_SEED: &str = "iotdata-retained-topic-identity!";

/// How many generated topics the property runs over. Bounded: each case is a handful of
/// in-process requests, so the suite stays fast.
const TOPIC_CASES: usize = 48;

/// Topics drawn from [`TOPIC_CHARACTERS`]: one to ten `/`-separated levels of one to six
/// characters each, zero to two long filler levels, and — on half the cases — one MQTT
/// wildcard inserted at a generated position. A generated string therefore varies along
/// everything the two rules divide on: whether it carries a wildcard, which wildcard it is
/// and where it sits (alone in a level, inside one, between levels, at either end), how
/// many levels the name has, and how many BYTES it is (the filler is what carries a name
/// past the byte ceiling).
fn topic_strategy() -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::strategy::Strategy;

    let level = proptest::collection::vec(proptest::sample::select(TOPIC_CHARACTERS), 1..=6)
        .prop_map(|characters| characters.into_iter().collect::<String>());
    let name = (proptest::collection::vec(level, 1..=10), 0..=2usize).prop_map(
        |(levels, fillers)| {
            let mut parts = levels;
            for filler in 0..fillers {
                parts.push(format!("{}{filler}", "w".repeat(120)));
            }
            parts.join("/")
        },
    );
    let wildcard = proptest::option::of((proptest::sample::select(MQTT_WILDCARDS), 0..=255usize));
    (name, wildcard).prop_map(|(name, wildcard)| match wildcard {
        None => name,
        Some((character, position)) => {
            let mut characters: Vec<char> = name.chars().collect();
            let at = position % (characters.len() + 1);
            characters.insert(at, character);
            characters.into_iter().collect()
        }
    })
}

/// [`TOPIC_CASES`] topics from [`topic_strategy`], drawn with [`TOPIC_SEED`]. Nothing is
/// written to disk: failure persistence is off, and the generated cases are values.
fn generated_topics() -> Vec<String> {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

    assert_eq!(
        TOPIC_SEED.len(),
        32,
        "the generator's seed is 32 bytes, got {}",
        TOPIC_SEED.len()
    );
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: TOPIC_CASES as u32,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, TOPIC_SEED.as_bytes()),
    );
    let strategy = topic_strategy();
    (0..TOPIC_CASES)
        .map(|case| {
            strategy
                .new_tree(&mut runner)
                .unwrap_or_else(|error| {
                    panic!("seed {TOPIC_SEED:?}: case {case} could not be generated: {error}")
                })
                .current()
        })
        .collect()
}

/// Why the BROKER would not take `topic` as the destination of a message, or `None` when
/// it would: a string carrying an MQTT wildcard is a filter and no name at all, and a name
/// is bounded in levels and, independently, in bytes. The sentences are written out here
/// rather than taken from the renderer under test; the limits come from the crate so no
/// case can drift from the rule.
fn publish_refusal(topic: &str) -> Option<String> {
    if topic.contains('+') || topic.contains('#') {
        return Some("Invalid topic filter".to_string());
    }
    if topic.split('/').count() > MAX_TOPIC_PARTS {
        return Some(format!("Topic can't contain more than {MAX_TOPIC_PARTS} parts"));
    }
    if topic.len() > MAX_TOPIC_BYTES {
        return Some(format!("Topic can't contain more than {MAX_TOPIC_BYTES} bytes"));
    }
    None
}

/// The topic the control retained message is held under, so every case can observe that a
/// refused call changed nothing. No generated topic can collide with it: `c`, `o`, `n`,
/// `t`, `r`, `l` and `f` are not in [`TOPIC_CHARACTERS`].
const CONTROL_TOPIC: &str = "control/leaf";

#[tokio::test]
async fn a_retained_message_read_refuses_exactly_the_generated_topics_that_are_filters() {
    // What each class of generated string was answered as, so a generated set that drifted
    // into one class alone cannot pass this as a property.
    let mut filters = 0_usize;
    let mut past_a_ceiling = 0_usize;
    let mut retained = 0_usize;

    for (case, topic) in generated_topics().into_iter().enumerate() {
        // A fresh store per case, so each generated name starts out retaining nothing
        // whatever an earlier case published.
        let harness = Harness::new();
        let control = harness
            .publish(
                &encode_component(CONTROL_TOPIC),
                1,
                true,
                None,
                b"control".to_vec(),
            )
            .await;
        assert_eq!(
            control.status,
            200,
            "seed {TOPIC_SEED:?} case {case} ({topic:?}): the control publish must be accepted, \
             got {} with body {}",
            control.status,
            String::from_utf8_lossy(&control.body)
        );

        let encoded = encode_component(&topic);
        let refusal = publish_refusal(&topic);
        let is_filter = topic.contains('+') || topic.contains('#');

        // The read of a name nothing is retained under: a filter is no name to resolve and
        // is refused, and every other string — however deep or long — is a key the store
        // holds nothing under, which is absence.
        let (expected_status, expected_code, expected_message): (u16, &str, &str) = if is_filter {
            (400, "InvalidRequestException", "Invalid topic filter")
        } else {
            (404, "ResourceNotFoundException", RETAINED_MESSAGE_NOT_FOUND)
        };
        let before = harness.get_retained_message(&encoded).await;
        assert_eq!(
            before.status,
            expected_status,
            "seed {TOPIC_SEED:?} case {case} ({topic:?}, {} levels, {} bytes): expected the read \
             of an unretained name to be {expected_status}, got {} with body {}",
            topic.split('/').count(),
            topic.len(),
            before.status,
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            before.error_code(),
            expected_code,
            "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected {expected_code}, got {}",
            String::from_utf8_lossy(&before.body)
        );
        assert_eq!(
            message(&before),
            expected_message,
            "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected {expected_message:?}, got {:?}",
            message(&before)
        );

        // The write path's verdict on the SAME string: it borrows the identity rule and
        // adds its own ceilings, each with its own sentence.
        let published = harness
            .publish(&encoded, 1, true, None, b"attempt".to_vec())
            .await;
        match refusal.as_deref() {
            Some(sentence) => {
                assert_eq!(
                    published.status,
                    400,
                    "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the publish to be \
                     refused 400, got {} with body {}",
                    published.status,
                    String::from_utf8_lossy(&published.body)
                );
                assert_eq!(
                    message(&published),
                    sentence,
                    "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the publish refusal \
                     {sentence:?}, got {:?}",
                    message(&published)
                );
            }
            None => assert_eq!(
                published.status,
                200,
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the broker to take the \
                 name, got {} with body {}",
                published.status,
                String::from_utf8_lossy(&published.body)
            ),
        }

        // …and the read once more. For a filter the two paths answer IDENTICALLY, which is
        // the whole of the identity rule; for a name past a ceiling the read still reports
        // absence; for a name the broker took the read answers from the store.
        let after = harness.get_retained_message(&encoded).await;
        if is_filter {
            filters += 1;
            assert_eq!(
                after.status,
                published.status,
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the read of a filter to \
                 answer the publish path's status {}, got {} with body {}",
                published.status,
                after.status,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                after.error_code(),
                published.error_code(),
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the read of a filter to \
                 answer the publish path's code {}, got {}",
                published.error_code(),
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                message(&after),
                message(&published),
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the read of a filter to \
                 answer the publish path's sentence {:?}, got {:?}",
                message(&published),
                message(&after)
            );
        } else if refusal.is_some() {
            past_a_ceiling += 1;
            assert_eq!(
                after.status,
                404,
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected a name past a ceiling to \
                 read as absent, got {} with body {}",
                after.status,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                message(&after),
                RETAINED_MESSAGE_NOT_FOUND,
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the absent-message \
                 sentence, got {:?}",
                message(&after)
            );
        } else {
            retained += 1;
            assert_eq!(
                after.status,
                200,
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the retained message to \
                 read back, got {} with body {}",
                after.status,
                String::from_utf8_lossy(&after.body)
            );
            assert_eq!(
                after.json().get("topic").and_then(Value::as_str),
                Some(topic.as_str()),
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the stored topic back, \
                 got {}",
                String::from_utf8_lossy(&after.body)
            );
            let payload = after
                .json()
                .get("payload")
                .and_then(Value::as_str)
                .map(|encoded| aws_smithy_types::base64::decode(encoded).expect("base64"))
                .unwrap_or_default();
            assert_eq!(
                payload,
                b"attempt".to_vec(),
                "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the published payload \
                 back, got {:?}",
                String::from_utf8_lossy(&payload)
            );
        }

        // The store from the other side: it holds the generated name only when the broker
        // took it — a filter is never a key — and the control message throughout, so no
        // refused call and no read fanned out or cleared anything.
        let listed = retained_topics(&harness).await;
        assert_eq!(
            listed.contains(&topic),
            refusal.is_none(),
            "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the listing to hold the name \
             only when the publish was taken, got {listed:?}"
        );
        let control_read = harness
            .get_retained_message(&encode_component(CONTROL_TOPIC))
            .await;
        assert_eq!(
            control_read.status,
            200,
            "seed {TOPIC_SEED:?} case {case} ({topic:?}): expected the control message to still \
             be readable, got {} with body {}",
            control_read.status,
            String::from_utf8_lossy(&control_read.body)
        );
    }

    // The generated set spans the three classes the two rules produce, so none of the
    // branches above was asserted on an empty set.
    for (class, observed) in [
        ("topic filters refused on both paths", filters),
        ("names past one of the broker's ceilings", past_a_ceiling),
        ("names the broker took and retained", retained),
    ] {
        assert!(
            observed >= 2,
            "seed {TOPIC_SEED:?}: expected the {TOPIC_CASES} generated topics to span the \
             class {class:?} at least twice, got {observed}"
        );
    }
}
