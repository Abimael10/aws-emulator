// SPDX-License-Identifier: CC0-1.0

//! The ONE rule the structured collection headers of this runtime are reported
//! by, driven through both operations that answer with them.
//!
//! `messages`, `interpretations` and `requestAttributes` travel on optional
//! `x-amz-lex-*` headers whose value is "compressed using gzip and then base64
//! encoded by Amazon Lex V2" (`smithy_query(query="operation",
//! operation="RecognizeUtterance")`, and the same per-member docs on
//! `PutSession`). Being optional `NonEmptyString`s, there is no header form of an
//! EMPTY collection: the header is reported exactly when the collection the
//! session holds is not empty, and then carries that collection's own JSON —
//! written with the model's own list, element, key and value schemas — under that
//! encoding.
//!
//! `PutSession` and `RecognizeUtterance` are the two operations that answer with
//! these headers, so both are driven here: for the same stored session they
//! report the same collection the same way, and an empty one as an absent header
//! in both.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::harness::{Emulator, TEST_ALIAS};

/// A `botId` satisfying the modelled `BotIdentifier` (len 10..10, pattern
/// `^[0-9a-zA-Z]+$`), for the fixture locale addressed by fixed labels.
const BOT_ID: &str = "ABCDEFGHIJ";

/// The locale the fixture builds, and the one every row addresses.
const LOCALE: &str = "en_US";

/// The text `requestContentType` the model lists ("Text format — `text/plain;
/// charset=utf-8`").
const TEXT_REQUEST_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// One `Message` of the modelled `Messages` list (`len 0..10`), numbered so a row
/// that stores several stores several different ones.
fn message(index: usize) -> Value {
    json!({"content": format!("message {index}"), "contentType": "PlainText"})
}

/// The session URI for `session_id` in the fixture locale.
fn fixture_session_path(session_id: &str) -> String {
    format!("/bots/{BOT_ID}/botAliases/{TEST_ALIAS}/botLocales/{LOCALE}/sessions/{session_id}")
}

/// Assert the absent-when-empty rule for one collection header: reported exactly
/// when `expected` is not empty, and then carrying `expected`'s own JSON.
fn assert_collection_header(
    case: &str,
    reply: &crate::harness::Reply,
    name: &str,
    expected: &Value,
) {
    let empty = match expected {
        Value::Array(items) => items.is_empty(),
        Value::Object(entries) => entries.is_empty(),
        other => panic!("case {case:?}: {name} must carry a collection, got {other:?}"),
    };
    let actual = reply.lex_header(name);
    assert_eq!(
        actual.is_some(),
        !empty,
        "case {case:?}: {name} must be reported exactly when its collection is not empty \
         (expected {expected:?}), got {actual:?}"
    );
    if !empty {
        assert_eq!(
            actual.as_ref(),
            Some(expected),
            "case {case:?}: expected {name} to carry {expected:?}, got {actual:?}"
        );
    }
}

/// One row of the `PutSession` table: what the request stores for each of the two
/// collections it reports.
#[derive(Debug, Clone)]
struct StoredCase {
    name: &'static str,
    session_id: &'static str,
    /// The `messages` the request stores, as the member's own JSON list.
    messages: Value,
    /// The `requestAttributes` the request stores, or `None` to send the member
    /// not at all.
    request_attributes: Option<Value>,
}

impl StoredCase {
    /// The attribute map the session ends up holding: an absent member stores
    /// nothing.
    fn stored_attributes(&self) -> Value {
        self.request_attributes.clone().unwrap_or_else(|| json!({}))
    }
}

/// The table: the rows differ along the count of each collection (none, one,
/// several, and the ten the modelled `Messages` list allows at most), which of
/// the two is empty, the character classes the attribute keys and values carry,
/// and whether the optional member is sent empty or not sent at all.
fn stored_cases() -> Vec<StoredCase> {
    vec![
        StoredCase {
            name: "neither collection holds anything",
            session_id: "both-empty",
            messages: json!([]),
            request_attributes: None,
        },
        StoredCase {
            name: "an empty attribute map sent explicitly",
            session_id: "empty-map",
            messages: json!([]),
            request_attributes: Some(json!({})),
        },
        StoredCase {
            name: "one of each",
            session_id: "one-each",
            messages: json!([message(1)]),
            request_attributes: Some(json!({"channel": "web"})),
        },
        StoredCase {
            name: "messages only",
            session_id: "messages-only",
            messages: json!([message(1), message(2)]),
            request_attributes: None,
        },
        StoredCase {
            name: "attributes only, with non-ASCII keys and values",
            session_id: "attributes-only",
            messages: json!([]),
            request_attributes: Some(json!({"canal": "téléphone", "étape": "2"})),
        },
        StoredCase {
            name: "the ten messages the modelled list allows at most",
            session_id: "ten-messages",
            messages: Value::Array((1..=10).map(message).collect()),
            request_attributes: Some(json!({"channel": "web", "locale": LOCALE})),
        },
    ]
}

/// RULE: `PutSession` reports each collection header exactly when the session it
/// stored holds that collection, and then carries the collection's own JSON — and
/// what it reported is what the session persisted.
#[tokio::test]
async fn put_session_reports_a_collection_header_exactly_when_the_collection_is_not_empty() {
    let emulator = Emulator::new();
    emulator.built_locale(BOT_ID, TEST_ALIAS, LOCALE);

    for case in stored_cases() {
        let path = fixture_session_path(case.session_id);
        let mut body = json!({
            "sessionState": {"dialogAction": {"type": "Delegate"}},
            "messages": case.messages.clone(),
        });
        if let Some(attributes) = &case.request_attributes {
            body["requestAttributes"] = attributes.clone();
        }
        let put = emulator.send("POST", &path, Some(body)).await;
        assert_eq!(
            put.status,
            200,
            "case {:?}: PutSession must succeed, got {} {:?}",
            case.name,
            put.status,
            String::from_utf8_lossy(&put.body)
        );

        assert_collection_header(case.name, &put, "x-amz-lex-messages", &case.messages);
        assert_collection_header(
            case.name,
            &put,
            "x-amz-lex-request-attributes",
            &case.stored_attributes(),
        );

        // The headers report the stored session, so what the session persisted
        // is read back through `GetSession`: the member it reports in its body
        // is the same collection, empty list and all.
        let got = emulator.send("GET", &path, None).await;
        assert_eq!(
            got.status,
            200,
            "case {:?}: GetSession must find the session just stored, got {} {:?}",
            case.name,
            got.status,
            String::from_utf8_lossy(&got.body)
        );
        let body = got.json();
        assert_eq!(
            body.get("messages"),
            Some(&case.messages),
            "case {:?}: expected GetSession to report {:?}, got {body:?}",
            case.name,
            case.messages
        );
    }
}

/// Which bot a turn runs against: the rows need both a locale that authors a
/// message for the step a turn reaches and one that authors none, so the turn's
/// `messages` collection is non-empty in one and empty in the other.
#[derive(Debug, Clone, Copy)]
enum Fixture {
    /// A bot whose locale the control plane built, declaring an intent with a
    /// prompted slot — so a turn that matches it has a prompt to send.
    AuthoredBot,
    /// A built locale declaring no intent of its own: nothing matches, and the
    /// bot has nothing authored to say at the step the dialog reaches.
    SilentLocale,
}

/// One row of the turn table: the bot, what a preceding `PutSession` stored for
/// the attributes, what is then said, and whether the bot has a message for it.
#[derive(Debug, Clone)]
struct TurnCase {
    name: &'static str,
    fixture: Fixture,
    session_id: &'static str,
    /// The `requestAttributes` seeded into the session, or `None` for a session
    /// that holds none.
    request_attributes: Option<Value>,
    /// The utterance, carried as the `inputStream` payload.
    utterance: &'static str,
    /// Does the bot author a message for the step this turn reaches?
    expect_messages: bool,
}

/// The turn table: the rows differ along whether the bot authors a message for
/// the turn, whether the session holds request attributes, and the character
/// classes those attributes carry.
fn turn_cases() -> Vec<TurnCase> {
    vec![
        TurnCase {
            name: "a turn the bot prompts for, in a session holding attributes",
            fixture: Fixture::AuthoredBot,
            session_id: "prompted-with-attributes",
            request_attributes: Some(json!({"channel": "web"})),
            utterance: "I want to order a pizza",
            expect_messages: true,
        },
        TurnCase {
            name: "a turn the bot prompts for, in a session holding no attributes",
            fixture: Fixture::AuthoredBot,
            session_id: "prompted-no-attributes",
            request_attributes: None,
            utterance: "I want to order a pizza",
            expect_messages: true,
        },
        TurnCase {
            name: "a turn the bot has nothing to say to, in a session holding attributes",
            fixture: Fixture::SilentLocale,
            session_id: "silent-with-attributes",
            request_attributes: Some(json!({"canal": "téléphone", "étape": "2"})),
            utterance: "hello",
            expect_messages: false,
        },
        TurnCase {
            name: "a turn the bot has nothing to say to, in a session holding no attributes",
            fixture: Fixture::SilentLocale,
            session_id: "silent-no-attributes",
            request_attributes: None,
            utterance: "x",
            expect_messages: false,
        },
    ]
}

/// RULE: `RecognizeUtterance` reports each collection header by the SAME rule —
/// absent for an empty collection, else the collection's own JSON — and reports
/// the attributes the sibling `PutSession` stored exactly as that operation
/// reported them.
#[tokio::test]
async fn a_turn_reports_its_collection_headers_by_the_same_rule_as_put_session() {
    for case in turn_cases() {
        let (emulator, path) = match case.fixture {
            Fixture::AuthoredBot => {
                let (emulator, bot) = Emulator::with_bot().await;
                let path = bot.session_path(case.session_id);
                (emulator, path)
            }
            Fixture::SilentLocale => {
                let emulator = Emulator::new();
                emulator.built_locale(BOT_ID, TEST_ALIAS, LOCALE);
                (emulator, fixture_session_path(case.session_id))
            }
        };

        // The session the turn is taken in, seeded with the attributes this row
        // stores — so the two operations are reporting ONE stored collection.
        let mut seed = json!({
            "sessionState": {"dialogAction": {"type": "Delegate"}},
            "messages": [],
        });
        if let Some(attributes) = &case.request_attributes {
            seed["requestAttributes"] = attributes.clone();
        }
        let put = emulator.send("POST", &path, Some(seed)).await;
        assert_eq!(
            put.status,
            200,
            "case {:?}: seeding the session must succeed, got {} {:?}",
            case.name,
            put.status,
            String::from_utf8_lossy(&put.body)
        );

        let turn = emulator
            .send_raw(
                "POST",
                &format!("{path}/utterance"),
                Bytes::from(case.utterance),
                TEXT_REQUEST_CONTENT_TYPE,
                &[],
            )
            .await;
        assert_eq!(
            turn.status,
            200,
            "case {:?}: RecognizeUtterance must succeed, got {} {:?}",
            case.name,
            turn.status,
            String::from_utf8_lossy(&turn.body)
        );

        // The turn carried no attributes of its own, so what it reports is the
        // map the session already held — reported by the same rule, and so
        // decoding to exactly what `PutSession` answered with for it.
        let stored_attributes = case.request_attributes.clone().unwrap_or_else(|| json!({}));
        assert_collection_header(
            case.name,
            &turn,
            "x-amz-lex-request-attributes",
            &stored_attributes,
        );
        assert_eq!(
            turn.lex_header("x-amz-lex-request-attributes"),
            put.lex_header("x-amz-lex-request-attributes"),
            "case {:?}: the two operations must report one stored attribute map the same way",
            case.name
        );

        // The messages of the turn are the bot's own for the step the dialog
        // reached, and the session persisted them: the header is reported
        // exactly when that list is not empty, and carries it.
        let got = emulator.send("GET", &path, None).await;
        assert_eq!(
            got.status,
            200,
            "case {:?}: GetSession must find the session the turn advanced, got {} {:?}",
            case.name,
            got.status,
            String::from_utf8_lossy(&got.body)
        );
        let session = got.json();
        let persisted_messages = session
            .get("messages")
            .cloned()
            .unwrap_or_else(|| json!([]));
        assert_eq!(
            persisted_messages.as_array().map(Vec::is_empty),
            Some(!case.expect_messages),
            "case {:?}: expected the bot to author {} for this turn, got {persisted_messages:?}",
            case.name,
            if case.expect_messages {
                "a message"
            } else {
                "nothing"
            }
        );
        assert_collection_header(case.name, &turn, "x-amz-lex-messages", &persisted_messages);

        // The third collection of a turn: "a list of intents that Amazon Lex V2
        // determined might satisfy the user's utterance". Every turn interprets
        // the utterance against the locale, so this one is never empty — the
        // non-empty side of the same rule, written from a list member of its own.
        let interpretations = turn.lex_header("x-amz-lex-interpretations");
        let count = interpretations
            .as_ref()
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        assert!(
            count > 0,
            "case {:?}: expected the turn's interpretations to be reported, got {interpretations:?}",
            case.name
        );
    }
}
