// SPDX-License-Identifier: CC0-1.0

//! Every call addressed by the four session `@httpLabel`s resolves the BOT
//! ADDRESS first, end to end over HTTP.
//!
//! The rule under test: a session call names a bot, an alias of that bot, a
//! locale of the version the alias points at and a session id, and Amazon Lex V2
//! resolves the bot address against the bot's own definition before it answers
//! anything about the session — "if the bot, alias, or session identifier
//! doesn't exist, Amazon Lex V2 returns a BadRequestException. If the locale
//! doesn't exist or is not enabled for the alias, you receive a
//! BadRequestException"
//! (`smithy_query(query="doc",shape="GetSession",
//! model_path="services/service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0/smithy-model/com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0.json")`,
//! and the same two sentences on `DeleteSession`). So an address that resolves to
//! no alias of that bot, or to a locale that version cannot serve, is refused AS
//! the address — by the read and the delete exactly as by the three calls that
//! interpret input or seed a dialog state — and only an address that resolves can
//! report a session as absent.
//!
//! The rows span what the rule generalises over:
//!
//! * the LABEL that names nothing: a well-formed `botId` nothing was created
//!   under, a well-formed `botAliasId` the bot does not have, an alias of ANOTHER
//!   bot, a locale the bot has no record for, and a locale that was created but
//!   never built — beside the fully resolving address as the control;
//! * every operation the four labels address, including the two that only report
//!   or remove the session held under them;
//! * the `sessionId` the absent-session sentence names, over the forms
//!   `SessionId` admits (`smithy_query(query="shape",shape="SessionId")`:
//!   `@length 2..100`, pattern `^[0-9a-zA-Z._:-]+$`) — at both ends of its length
//!   and over each character class;
//! * and the round trip through an address that resolves: a session seeded there
//!   is reported back, removed and then reported absent, so resolving the address
//!   withholds nothing a caller stored.
//!
//! A control plane that drops the locale UNDER a stored session is the same rule
//! over time, and is asserted in `bot_addressing.rs`.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::harness::{Emulator, TEST_ALIAS};

/// The locale the bots under test are built in.
const LOCALE: &str = "en_US";

/// The text `requestContentType` `RecognizeUtterance` accepts.
const TEXT_REQUEST_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// What Amazon Lex V2 answers a call addressed to a bot and locale whose model it
/// cannot serve. Entirely fixed text: it names neither the bot nor the locale.
const LOCALE_NOT_VALID: &str = "The provided BotId/BotLocale combination is not valid. \
     Please update the BotId/BotLocale combination and retry your request.";

/// The sentence a `botId`/`botAliasId` pair that is no alias of that bot earns:
/// the two ids the request carried, then fixed text.
fn alias_not_valid(bot_id: &str, bot_alias_id: &str) -> String {
    format!("{bot_id}/{bot_alias_id} is not a valid BotId/BotAliasId combination.")
}

/// How the runtime words an absent session: fixed text around the `sessionId` the
/// request carried, with U+2019 as the apostrophe.
fn session_absent(session_id: &str) -> String {
    format!("The session for user {session_id} doesn\u{2019}t exist or has expired.")
}

/// One operation addressed to a session: the two that report or remove it, the
/// one that seeds it, and the two encodings of one turn. Every row of the table
/// is applied to all five, so what is asserted is the rule they share.
#[derive(Debug, Clone, Copy)]
enum Call {
    GetSession,
    DeleteSession,
    PutSession,
    RecognizeText,
    RecognizeUtterance,
}

impl Call {
    fn name(self) -> &'static str {
        match self {
            Self::GetSession => "GetSession",
            Self::DeleteSession => "DeleteSession",
            Self::PutSession => "PutSession",
            Self::RecognizeText => "RecognizeText",
            Self::RecognizeUtterance => "RecognizeUtterance",
        }
    }

    /// Does this call answer from a session that is already STORED? A read and a
    /// delete do, so on an address that resolves they report the session that is
    /// not there, while the other three start the session they address.
    fn answers_from_a_stored_session(self) -> bool {
        matches!(self, Self::GetSession | Self::DeleteSession)
    }

    /// Send this call against `path`, with a body its own model accepts, so
    /// nothing but the addressing can refuse it.
    async fn send(self, emulator: &Emulator, path: &str) -> crate::harness::Reply {
        match self {
            Self::GetSession => emulator.send("GET", path, None).await,
            Self::DeleteSession => emulator.send("DELETE", path, None).await,
            Self::PutSession => {
                emulator
                    .send(
                        "POST",
                        path,
                        Some(json!({"sessionState": {"dialogAction": {"type": "Delegate"}}})),
                    )
                    .await
            }
            Self::RecognizeText => {
                emulator
                    .send(
                        "POST",
                        &format!("{path}/text"),
                        Some(json!({"text": "I want to order a pizza"})),
                    )
                    .await
            }
            Self::RecognizeUtterance => {
                emulator
                    .send_raw(
                        "POST",
                        &format!("{path}/utterance"),
                        Bytes::from_static(b"I want to order a pizza"),
                        TEXT_REQUEST_CONTENT_TYPE,
                        &[],
                    )
                    .await
            }
        }
    }
}

/// EVERY operation this service binds to a session address.
fn session_calls() -> [Call; 5] {
    [
        Call::GetSession,
        Call::DeleteSession,
        Call::PutSession,
        Call::RecognizeText,
        Call::RecognizeUtterance,
    ]
}

/// Every `sessionId` form a row addresses, and what makes it different from the
/// others.
fn session_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the member admits", "ab".to_string()),
        ("a hyphenated id", "read-or-remove-1".to_string()),
        (
            "an id carrying every punctuation class the pattern allows",
            "user_42.session:7-A".to_string(),
        ),
        ("digits only", "20261121".to_string()),
        ("the longest id the member admits", "z".repeat(100)),
    ]
}

/// RULE: an address that resolves to no alias of the bot, or to a locale the
/// alias's version cannot serve, is refused as the ADDRESS by every call the four
/// labels address — the same sentence, whichever operation asked.
#[tokio::test]
async fn every_session_call_refuses_an_address_that_resolves_to_nothing() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("AddressResolutionBot", &[LOCALE]).await;
    let other = emulator
        .built_bot("AddressResolutionOtherBot", &[LOCALE])
        .await;
    let other_alias = emulator.second_alias(&other, "OTHERALIAS");

    // A second bot with a locale that was created and authored but never BUILT:
    // "the bot is ready for use and can be tested using any utterance" is the
    // built status, and a locale that never reached it has no model to serve.
    let unbuilt_bot_id = emulator.create_bot("AddressResolutionUnbuiltBot").await;
    emulator.create_locale(&unbuilt_bot_id, LOCALE).await;
    emulator
        .create_intent_with_slot(&unbuilt_bot_id, LOCALE)
        .await;
    emulator.settle_control_plane();
    let status = emulator.bot_locale_status(&unbuilt_bot_id, LOCALE).await;
    assert_eq!(
        status, "NotBuilt",
        "the unbuilt row must address a locale that exists and was never built, got {status:?}"
    );

    // (case, botId, botAliasId, localeId, the sentence the address earns)
    let rows: Vec<(&str, String, String, String, String)> = vec![
        (
            "a botId nothing was created under",
            "AAAAAAAAAA".to_string(),
            bot.alias_id.clone(),
            LOCALE.to_string(),
            alias_not_valid("AAAAAAAAAA", &bot.alias_id),
        ),
        (
            "a well-formed botAliasId the bot does not have",
            bot.bot_id.clone(),
            "BBBBBBBBBB".to_string(),
            LOCALE.to_string(),
            alias_not_valid(&bot.bot_id, "BBBBBBBBBB"),
        ),
        (
            "an alias of another bot",
            bot.bot_id.clone(),
            other_alias.alias_id.clone(),
            LOCALE.to_string(),
            alias_not_valid(&bot.bot_id, &other_alias.alias_id),
        ),
        (
            "the bot's own alias id in lower case",
            bot.bot_id.clone(),
            bot.alias_id.to_lowercase(),
            LOCALE.to_string(),
            alias_not_valid(&bot.bot_id, &bot.alias_id.to_lowercase()),
        ),
        (
            "a locale the bot has no record for",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            "de_DE".to_string(),
            LOCALE_NOT_VALID.to_string(),
        ),
        (
            "a locale outside the ones the service supports",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            "xx_XX".to_string(),
            LOCALE_NOT_VALID.to_string(),
        ),
        (
            "a locale that was created but never built",
            unbuilt_bot_id.clone(),
            TEST_ALIAS.to_string(),
            LOCALE.to_string(),
            LOCALE_NOT_VALID.to_string(),
        ),
    ];

    for (index, (case, bot_id, bot_alias_id, locale_id, expected)) in rows.iter().enumerate() {
        for call in session_calls() {
            let session_id = format!("address-{index}-{}", call.name());
            let path = format!(
                "/bots/{bot_id}/botAliases/{bot_alias_id}/botLocales/{locale_id}/sessions/\
                 {session_id}"
            );
            let reply = call.send(&emulator, &path).await;
            let case = format!("{case:?} on {}", call.name());
            assert_eq!(
                reply.status,
                404,
                "case {case}: expected HTTP 404, got {} {:?}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            assert!(
                reply.error_type().ends_with("ResourceNotFoundException"),
                "case {case}: expected ResourceNotFoundException, got {:?}",
                reply.error_type()
            );
            let message = reply.error_message();
            assert_eq!(
                &message, expected,
                "case {case}: expected the address's own sentence {expected:?}, got {message:?}"
            );

            // The refusal had no effect: the same session id on the bot's own
            // resolving address is still absent.
            let read = emulator
                .send("GET", &bot.session_path(&session_id), None)
                .await;
            assert_eq!(
                (read.status, read.error_message().as_str()),
                (404, session_absent(&session_id).as_str()),
                "case {case}: the refused call must leave no session behind, got {} {:?}",
                read.status,
                String::from_utf8_lossy(&read.body)
            );
        }
    }
}

/// RULE's other side: on an address that DOES resolve, the read and the delete
/// report the session itself — absent until something stores one, and reported as
/// stored afterwards — while the three calls that seed or interpret are served.
#[tokio::test]
async fn an_address_that_resolves_reports_the_session_itself() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("ResolvingAddressBot", &[LOCALE]).await;

    for (case, session_id) in session_ids() {
        let path = bot.session_path(&session_id);
        // Nothing is stored yet, so the two that answer from a stored session
        // report the SESSION that is not there — the address resolved.
        for call in session_calls() {
            if !call.answers_from_a_stored_session() {
                continue;
            }
            let reply = call.send(&emulator, &path).await;
            let case = format!("{case} on {}", call.name());
            assert_eq!(
                reply.status,
                404,
                "case {case}: expected HTTP 404, got {} {:?}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            let message = reply.error_message();
            let expected = session_absent(&session_id);
            assert_eq!(
                message, expected,
                "case {case}: expected {expected:?}, got {message:?}"
            );
        }

        // The seed is served, and from then on the read reports what it stored
        // and the delete removes it: the resolution withholds nothing.
        let seeded = emulator
            .send(
                "POST",
                &path,
                Some(json!({
                    "sessionState": {
                        "dialogAction": {"type": "Delegate"},
                        "sessionAttributes": {"marker": session_id.clone()},
                    },
                })),
            )
            .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case}: PutSession on a resolving address is served, got {} {:?}",
            seeded.status,
            String::from_utf8_lossy(&seeded.body)
        );

        let read = emulator.send("GET", &path, None).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: the stored session is read back at its own address, got {} {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let stored = read.json();
        assert_eq!(
            stored
                .pointer("/sessionState/sessionAttributes/marker")
                .and_then(Value::as_str),
            Some(session_id.as_str()),
            "case {case}: expected the attributes the seed stored, got {stored:?}"
        );

        let removed = emulator.send("DELETE", &path, None).await;
        assert_eq!(
            removed.status,
            200,
            "case {case}: the delete removes the session it addresses, got {} {:?}",
            removed.status,
            String::from_utf8_lossy(&removed.body)
        );
        let gone = emulator.send("GET", &path, None).await;
        assert_eq!(
            (gone.status, gone.error_message().as_str()),
            (404, session_absent(&session_id).as_str()),
            "case {case}: the removed session is reported absent, got {} {:?}",
            gone.status,
            String::from_utf8_lossy(&gone.body)
        );
    }
}
