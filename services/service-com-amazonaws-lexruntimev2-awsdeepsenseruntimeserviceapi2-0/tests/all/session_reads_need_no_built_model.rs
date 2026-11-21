// SPDX-License-Identifier: CC0-1.0

//! Reading or removing a stored SESSION needs no BUILT model behind the locale it
//! is addressed in.
//!
//! The rule under test: the four `@httpLabel`s of a session call address the
//! bot/alias PAIR and a locale of the version that alias points at — "Returns
//! session information for a specified bot, alias, and user. … If the bot, alias,
//! or session identifier doesn't exist, Amazon Lex V2 returns a
//! BadRequestException. If the locale doesn't exist or is not enabled for the
//! alias, you receive a BadRequestException."
//! (`smithy_query(query="doc",shape="GetSession",
//! model_path="services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/smithy-model/com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0.json")`).
//! Being BUILT is what gives a locale a runtime MODEL, and only a call that
//! INTERPRETS input needs one. So on a locale that exists and was never built:
//!
//! * `GetSession` and `DeleteSession` resolve the address and are refused for the
//!   SESSION — HTTP 404 `ResourceNotFoundException` whose sentence names the
//!   `sessionId` the request carried;
//! * `RecognizeText` and `PutSession`, which need the model, are refused for the
//!   ADDRESS with the fixed `BotId`/`BotLocale` sentence;
//! * and once the locale IS built, the read reports a session `PutSession` stored,
//!   so the resolution above withholds nothing from a built locale.
//!
//! The rows span the `sessionId` the label carries — the one variable part of the
//! session-absence sentence — across the forms `SessionId` admits
//! (`smithy_query(query="shape",shape="SessionId")`: `@length 2..100`, pattern
//! `^[0-9a-zA-Z._:-]+$`), at both ends of its length and over each character class
//! the pattern allows.

use crate::harness::{Emulator, TEST_ALIAS};

/// The locale every row addresses: created and authored, never built.
const LOCALE: &str = "en_US";

/// What Amazon Lex V2 answers a call addressed to a bot and locale whose model it
/// cannot serve. Entirely fixed text: it names neither the bot nor the locale.
const LOCALE_NOT_VALID: &str = "The provided BotId/BotLocale combination is not valid. \
     Please update the BotId/BotLocale combination and retry your request.";

/// How the runtime words an absent session: fixed text around the `sessionId` the
/// request carried, with U+2019 as the apostrophe.
fn session_absent(session_id: &str) -> String {
    format!("The session for user {session_id} doesn\u{2019}t exist or has expired.")
}

/// Every `sessionId` form a row addresses, and what makes it different from the
/// others.
fn session_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the member admits", "ab".to_string()),
        ("a hyphenated id", "resume-or-start-1".to_string()),
        (
            "an id carrying every punctuation class the pattern allows",
            "user_42.session:7-A".to_string(),
        ),
        ("digits only", "20261121".to_string()),
        // One past nothing: the longest id the member admits, so the sentence is
        // built from a label at the bound as well as inside it.
        ("the longest id the member admits", "z".repeat(100)),
    ]
}

#[tokio::test]
async fn a_session_call_on_an_unbuilt_locale_is_refused_for_the_session_not_the_address() {
    let emulator = Emulator::new();
    let bot_id = emulator.create_bot("UnbuiltSessionReadBot").await;
    emulator.create_locale(&bot_id, LOCALE).await;
    emulator.create_intent_with_slot(&bot_id, LOCALE).await;
    emulator.settle_control_plane();
    let status = emulator.bot_locale_status(&bot_id, LOCALE).await;
    assert_eq!(
        status, "NotBuilt",
        "the locale under test must exist and be unbuilt, got {status:?}"
    );

    for (case, session_id) in session_ids() {
        let path =
            format!("/bots/{bot_id}/botAliases/{TEST_ALIAS}/botLocales/{LOCALE}/sessions/{session_id}");
        // The two calls that only read or remove a stored session: the locale
        // addresses them, and the session they name is not there.
        for (operation, method) in [("GetSession", "GET"), ("DeleteSession", "DELETE")] {
            let reply = emulator.send(method, &path, None).await;
            let case = format!("{case} on {operation}");
            assert_eq!(
                reply.status,
                404,
                "case {case:?}: expected HTTP 404, got {} {:?}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            assert!(
                reply.error_type().ends_with("ResourceNotFoundException"),
                "case {case:?}: expected ResourceNotFoundException, got {:?}",
                reply.error_type()
            );
            let message = reply.error_message();
            let expected = session_absent(&session_id);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }

        // The calls that need the MODEL are refused for the address instead, on
        // the very same labels.
        for (operation, method, body) in [
            (
                "RecognizeText",
                "POST",
                Some(::serde_json::json!({"text": "i would like a chocolate shake"})),
            ),
            (
                "PutSession",
                "POST",
                Some(::serde_json::json!({"sessionState": {"dialogAction": {"type": "Delegate"}}})),
            ),
        ] {
            let target = if operation == "RecognizeText" {
                format!("{path}/text")
            } else {
                path.clone()
            };
            let reply = emulator.send(method, &target, body).await;
            let case = format!("{case} on {operation}");
            assert_eq!(
                reply.status,
                404,
                "case {case:?}: expected HTTP 404, got {} {:?}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            let message = reply.error_message();
            assert_eq!(
                message, LOCALE_NOT_VALID,
                "case {case:?}: expected the address refusal {LOCALE_NOT_VALID:?}, got \
                 {message:?}"
            );
        }
    }

    // None of those calls brought a session into existence: the locale is built,
    // which is the only thing that was missing, and the reads still find nothing.
    emulator.build_locale(&bot_id, LOCALE).await;
    let built = emulator.bot_locale_status(&bot_id, LOCALE).await;
    assert_eq!(built, "Built", "the build must settle, got {built:?}");
    for (case, session_id) in session_ids() {
        let path =
            format!("/bots/{bot_id}/botAliases/{TEST_ALIAS}/botLocales/{LOCALE}/sessions/{session_id}");
        let reply = emulator.send("GET", &path, None).await;
        let message = reply.error_message();
        assert_eq!(
            (reply.status, message.as_str()),
            (404, session_absent(&session_id).as_str()),
            "case {case:?}: a refused call must have started no session, got {} {:?}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
    }

    // And the read reports what a write stores, so the resolution above withholds
    // nothing from the locale once it is built.
    let stored_path =
        format!("/bots/{bot_id}/botAliases/{TEST_ALIAS}/botLocales/{LOCALE}/sessions/round-trip");
    let seeded = emulator
        .send(
            "POST",
            &stored_path,
            Some(::serde_json::json!({"sessionState": {"dialogAction": {"type": "Delegate"}}})),
        )
        .await;
    assert_eq!(
        seeded.status,
        200,
        "PutSession on the built locale must be served, got {} {:?}",
        seeded.status,
        String::from_utf8_lossy(&seeded.body)
    );
    let read = emulator.send("GET", &stored_path, None).await;
    assert_eq!(
        read.status,
        200,
        "the stored session must be read back, got {} {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        read.json()
            .get("sessionId")
            .and_then(::serde_json::Value::as_str),
        Some("round-trip"),
        "the read must report the session the write stored, got {:?}",
        read.json()
    );
}
