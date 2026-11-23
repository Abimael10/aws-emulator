// SPDX-License-Identifier: CC0-1.0

//! What a call that only REPORTS or REMOVES a session resolves, driven end to end over HTTP.
//!
//! The rule, one level above the recorded case (a `GetSession` on a well-formed `botId` and
//! `botAliasId` pair nothing was created under, for a session id nothing had stored):
//!
//! > A session of this runtime is held under the WHOLE address its four `@httpLabel` values
//! > name, so `GetSession` and `DeleteSession` resolve that session and nothing else: a
//! > lookup that finds none is HTTP 404 `ResourceNotFoundException` with the sentence Amazon
//! > Lex V2 states an absent session in — "The session for user " + the `sessionId` the
//! > request carried + " doesn\u{2019}t exist or has expired." — whichever of the labels is
//! > the one naming nothing, while a session that IS stored under that address is reported
//! > and removed there. Only the three calls that interpret an utterance or seed a dialog
//! > state need the bot's own built model, and only they refuse the ADDRESS.
//!
//! The rows span what the rule generalises over: which label names nothing (a well-formed
//! `botId` nothing was created under, a well-formed `botAliasId` the bot does not have, an
//! alias of ANOTHER bot, the bot's own alias id in another case, a locale the bot has no
//! record for, a locale outside the ones the service supports, and a locale that was created
//! but never built), both operations that answer from a stored session, and the `sessionId`
//! forms the member admits (`smithy_query(query="shape",shape="SessionId")`: `@length
//! 2..100`, pattern `^[0-9a-zA-Z._:-]+$`) — at both ends of its length and over each
//! character class.
//!
//! The address refusals the other three calls answer with, and the whole table they are
//! asserted over, are `session_address_resolution.rs`'s.

use serde_json::json;

use crate::harness::{Emulator, TEST_ALIAS};

/// The locale the fixture bots are built in.
const LOCALE: &str = "en_US";

/// How the runtime words an absent session: fixed text around the `sessionId` the request
/// carried, with U+2019 as the apostrophe.
fn session_absent(session_id: &str) -> String {
    format!("The session for user {session_id} doesn\u{2019}t exist or has expired.")
}

/// Every `sessionId` form a row is asked for, and what makes it different from the others.
fn session_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the member admits", "ab".to_string()),
        ("a hyphenated id", "report-or-remove-1".to_string()),
        (
            "an id carrying every punctuation class the pattern allows",
            "user_7.session:2-B".to_string(),
        ),
        ("digits only", "20261124".to_string()),
        ("the longest id the member admits", "q".repeat(100)),
    ]
}

/// RULE: every address whose labels name nothing is reported as the SESSION that is not
/// there, by the read and by the delete, for every `sessionId` form.
#[tokio::test]
async fn a_session_report_names_the_session_whichever_label_names_nothing() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("SessionReportBot", &[LOCALE]).await;
    let other = emulator.built_bot("SessionReportOtherBot", &[LOCALE]).await;
    let other_alias = emulator.second_alias(&other, "OTHERALIAS");

    // A bot whose locale was created and authored but never BUILT: it has no runtime model,
    // which is the condition the three interpreting calls refuse as the address.
    let unbuilt_bot_id = emulator.create_bot("SessionReportUnbuiltBot").await;
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

    // (case, botId, botAliasId, localeId) — each row is an address one of whose labels names
    // nothing, beside the unbuilt locale, which names a record the runtime cannot serve.
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "a botId nothing was created under",
            "AAAAAAAAAA".to_string(),
            bot.alias_id.clone(),
            LOCALE.to_string(),
        ),
        (
            "a well-formed botAliasId the bot does not have",
            bot.bot_id.clone(),
            "BBBBBBBBBB".to_string(),
            LOCALE.to_string(),
        ),
        (
            "an alias of another bot",
            bot.bot_id.clone(),
            other_alias.alias_id.clone(),
            LOCALE.to_string(),
        ),
        (
            "the bot's own alias id in lower case",
            bot.bot_id.clone(),
            bot.alias_id.to_lowercase(),
            LOCALE.to_string(),
        ),
        (
            "a locale the bot has no record for",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            "de_DE".to_string(),
        ),
        (
            "a locale outside the ones the service supports",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            "xx_XX".to_string(),
        ),
        (
            "a locale that was created but never built",
            unbuilt_bot_id.clone(),
            TEST_ALIAS.to_string(),
            LOCALE.to_string(),
        ),
        (
            "an address that resolves, with no session stored under it",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            LOCALE.to_string(),
        ),
    ];

    for (case, bot_id, bot_alias_id, locale_id) in rows.iter() {
        for (id_case, session_id) in session_ids() {
            // No session is ever stored under any of these addresses, so the same ids are
            // asked for on every row; the address is what differs.
            let path = format!(
                "/bots/{bot_id}/botAliases/{bot_alias_id}/botLocales/{locale_id}/sessions/\
                 {session_id}"
            );
            for (operation, method) in [("GetSession", "GET"), ("DeleteSession", "DELETE")] {
                let reply = emulator.send(method, &path, None).await;
                let case = format!("{case:?} with {id_case} on {operation}");
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
                let expected = session_absent(&session_id);
                let message = reply.error_message();
                assert_eq!(
                    message, expected,
                    "case {case}: expected {expected:?}, got {message:?}"
                );
            }
        }
    }
}

/// RULE's other side: a session STORED under an address is reported there and removed there,
/// and is reported absent once it has been — so resolving the session rather than the address
/// withholds nothing a caller stored.
#[tokio::test]
async fn a_stored_session_is_reported_and_removed_at_its_own_address() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("StoredSessionBot", &[LOCALE]).await;

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
            "case {case:?}: PutSession on a resolving address is served, got {} {:?}",
            seeded.status,
            String::from_utf8_lossy(&seeded.body)
        );

        let read = emulator.send("GET", &path, None).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the stored session is reported at its own address, got {} {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let stored = read.json();
        assert_eq!(
            stored
                .pointer("/sessionState/sessionAttributes/marker")
                .and_then(serde_json::Value::as_str),
            Some(session_id.as_str()),
            "case {case:?}: expected the attributes the seed stored, got {stored:?}"
        );

        let removed = emulator.send("DELETE", &path, None).await;
        assert_eq!(
            removed.status,
            200,
            "case {case:?}: the delete removes the session it addresses, got {} {:?}",
            removed.status,
            String::from_utf8_lossy(&removed.body)
        );
        let gone = emulator.send("GET", &path, None).await;
        assert_eq!(
            (gone.status, gone.error_message().as_str()),
            (404, session_absent(&session_id).as_str()),
            "case {case:?}: the removed session is reported absent, got {} {:?}",
            gone.status,
            String::from_utf8_lossy(&gone.body)
        );
    }
}
