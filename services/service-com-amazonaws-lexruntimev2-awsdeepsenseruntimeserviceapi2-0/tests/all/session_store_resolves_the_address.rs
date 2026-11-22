// SPDX-License-Identifier: CC0-1.0

//! A session READ or DELETE resolves the session against this runtime's own store and nothing
//! else, end to end over HTTP.
//!
//! The rule under test: a session is held under the four `@httpLabel`s of a session call
//! together — the bot, the alias, the locale and the session id — so an address naming no bot,
//! no alias of that bot or no locale of the version the alias points at holds no session, and
//! `GetSession` and `DeleteSession` answer every address they hold no session for the same way:
//! HTTP 404 `ResourceNotFoundException` whose sentence states the session is not there and
//! names the `sessionId` the request carried, which is its one variable part. Neither call
//! interprets input, so neither needs the built runtime model behind the labels nor a
//! control-plane record of them; the three calls that DO need it — `PutSession`,
//! `RecognizeText` and `RecognizeUtterance` — resolve the bot alias and the locale first and
//! are refused as the ADDRESS, which is what keeps the two conditions apart.
//!
//! The rows span what the rule generalises over:
//!
//! * the LABEL that names nothing: a well-formed `botId` nothing was created under, a
//!   well-formed `botAliasId` the bot does not have, an alias of ANOTHER bot, and a locale the
//!   bot has no record for — beside the fully resolving address as the control;
//! * both calls that answer from the stored session, and all three that need the address;
//! * the `sessionId` the sentence names, over the forms `SessionId` admits
//!   (`smithy_query(query="shape",shape="SessionId")`: `@length 2..100`, pattern
//!   `^[0-9a-zA-Z._:-]+$`) — at both ends of its length and over each character class;
//! * and the round trip: a session seeded through `PutSession` on the resolving address is read
//!   back, removed, and reported absent afterwards, so resolving the session alone withholds
//!   nothing a caller stored.

use serde_json::json;

use crate::harness::Emulator;

/// The locale the bot under test is built in.
const LOCALE: &str = "en_US";

/// How the runtime words an absent session: fixed text around the `sessionId` the request
/// carried, with U+2019 as the apostrophe.
fn session_absent(session_id: &str) -> String {
    format!("The session for user {session_id} doesn\u{2019}t exist or has expired.")
}

/// The sentence a call that needs the built model answers a `botId`/`botAliasId` pair that is
/// no alias of that bot: the two ids the request carried, then fixed text.
fn alias_not_valid(bot_id: &str, bot_alias_id: &str) -> String {
    format!("{bot_id}/{bot_alias_id} is not a valid BotId/BotAliasId combination.")
}

/// What Amazon Lex V2 answers a call addressed to a bot and locale whose model it cannot
/// serve. Entirely fixed text: it names neither the bot nor the locale.
const LOCALE_NOT_VALID: &str = "The provided BotId/BotLocale combination is not valid. \
     Please update the BotId/BotLocale combination and retry your request.";

/// Every `sessionId` form a row addresses, and what makes it different from the others.
fn session_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the member admits", "ab".to_string()),
        ("a hyphenated id", "read-or-remove-1".to_string()),
        (
            "an id carrying every punctuation class the pattern allows",
            "user_42.session:7-A".to_string(),
        ),
        ("digits only", "20261122".to_string()),
        ("the longest id the member admits", "z".repeat(100)),
    ]
}

/// RULE: every address `GetSession` and `DeleteSession` hold no session for earns the
/// absent-session sentence, whichever label names nothing — while the three calls that need
/// the built model behind the labels are refused as the address.
#[tokio::test]
async fn a_session_read_or_delete_reports_the_absent_session_for_every_address() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("SessionStoreBot", &[LOCALE]).await;
    let other = emulator.built_bot("SessionStoreOtherBot", &[LOCALE]).await;
    let other_alias = emulator.second_alias(&other, "OTHERALIAS");

    // (case, botId, botAliasId, localeId, the sentence a call that needs the ADDRESS earns).
    // Every row holds no session under those labels, so both the read and the delete report
    // the session that is not there.
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
            "a locale the bot has no record for",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            "de_DE".to_string(),
            LOCALE_NOT_VALID.to_string(),
        ),
        (
            "an address that resolves, with no session stored under it",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            LOCALE.to_string(),
            String::new(),
        ),
    ];

    for (case, bot_id, bot_alias_id, locale_id, address_refusal) in &rows {
        for (session_case, session_id) in session_ids() {
            let path = format!(
                "/bots/{bot_id}/botAliases/{bot_alias_id}/botLocales/{locale_id}/sessions/\
                 {session_id}"
            );
            for (operation, method) in [("GetSession", "GET"), ("DeleteSession", "DELETE")] {
                let reply = emulator.send(method, &path, None).await;
                let case = format!("{case:?} with {session_case} on {operation}");
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
                let expected = session_absent(&session_id);
                assert_eq!(
                    message, expected,
                    "case {case}: expected {expected:?}, got {message:?}"
                );
            }

            // The three calls that interpret input or seed a dialog state need the built model
            // behind those labels, so an address that names nothing is refused AS the address
            // — a different sentence from the one the read and the delete answered above.
            if address_refusal.is_empty() {
                continue;
            }
            for (operation, method, body) in [
                (
                    "PutSession",
                    "POST",
                    Some(json!({"sessionState": {"dialogAction": {"type": "Delegate"}}})),
                ),
                ("RecognizeText", "POST", Some(json!({"text": "hello"}))),
            ] {
                let case = format!("{case:?} with {session_case} on {operation}");
                let call_path = if operation == "RecognizeText" {
                    format!("{path}/text")
                } else {
                    path.clone()
                };
                let reply = emulator.send(method, &call_path, body).await;
                assert_eq!(
                    reply.status,
                    404,
                    "case {case}: expected HTTP 404, got {} {:?}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                );
                let message = reply.error_message();
                assert_eq!(
                    &message, address_refusal,
                    "case {case}: expected the address's own sentence {address_refusal:?}, got \
                     {message:?}"
                );
            }
        }
    }
}

/// RULE's other half, the ROUND TRIP: a session seeded on an address that resolves is read back
/// at it, removed by the delete, and reported absent afterwards — so resolving the session
/// alone serves every session a caller really stored.
#[tokio::test]
async fn a_stored_session_is_read_back_and_removed_at_its_own_address() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("SessionRoundTripBot", &[LOCALE]).await;

    for (case, session_id) in session_ids() {
        let path = bot.session_path(&session_id);
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
            "case {case}: seeding the session must succeed, got {} {:?}",
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
                .and_then(serde_json::Value::as_str),
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
            gone.status,
            404,
            "case {case}: the removed session is gone, got {} {:?}",
            gone.status,
            String::from_utf8_lossy(&gone.body)
        );
        let message = gone.error_message();
        let expected = session_absent(&session_id);
        assert_eq!(
            message, expected,
            "case {case}: expected {expected:?}, got {message:?}"
        );
    }
}
