// SPDX-License-Identifier: CC0-1.0

//! Addressing and seeding: which calls this runtime serves at all, and what a
//! seeded dialog state may say.
//!
//! The Lex V2 runtime has no bot of its own — "Amazon Lex V2 then interprets the
//! user input using the machine learning model that it build for the bot" — so
//! every test here creates the bot through the Lex Models V2 control plane
//! mounted on the same engine, exactly as a client would, and then talks to it.
//!
//! The rules under test, each over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. A call addressed by the four session labels (`GetSession`,
//!    `DeleteSession`, `PutSession`, `RecognizeText`, `RecognizeUtterance`)
//!    resolves the bot ADDRESS behind those labels before it touches the session
//!    store: the `botId`/`botAliasId` PAIR and the addressed locale are resolved
//!    against the control plane, so an alias that is not an alias of that bot,
//!    and a locale the alias's version cannot serve, are refused with HTTP 404
//!    `ResourceNotFoundException` in AWS's own words — and the refusal has no
//!    effect, so no session comes into existence and none is removed. The whole
//!    rule is asserted over its own table in `session_address_resolution.rs`.
//! 2. A `sessionState` seeded by `PutSession` names resources the resolved
//!    locale defines: an intent it does not declare, or an elicited slot that
//!    intent does not declare, is HTTP 400 `ValidationException` and stores
//!    nothing.
//! 3. The `messages` a `PutSession` reports are the ones the BOT authors for
//!    the dialog state it seeds when the request carries none of its own, and
//!    the caller's own messages when it does.
//! 4. A `botId` that is not a well-formed bot identifier is refused, whichever
//!    session operation it is addressed through, with the runtime's own fixed
//!    sentence for a bot address it cannot look a bot up by — while a refusal
//!    that blames another label keeps that label's own report.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::harness::{
    Bot, Emulator, CLOSING_MESSAGE, CONFIRMATION_PROMPT, DRAFT, INTENT, SLOT, SLOT_PROMPT,
    TEST_ALIAS,
};

/// The text `requestContentType` `RecognizeUtterance` accepts.
const TEXT_REQUEST_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// The intent every locale is created with, which the control plane adds to a
/// locale as it creates it.
const FALLBACK_INTENT: &str = "FallbackIntent";

/// What the sentence for a locale that has no built model is: entirely fixed
/// text, so it is the same for every bot and every locale.
const LOCALE_NOT_VALID: &str = "The provided BotId/BotLocale combination is not valid. \
     Please update the BotId/BotLocale combination and retry your request.";

/// One operation addressed to a session: `PutSession`, which seeds a session's
/// dialog state, the two encodings of one turn, and the two operations that read
/// and delete the session. Each row of a table below is applied to a whole set of
/// them, so what is asserted is the addressing rule they share rather than one
/// operation's spelling of it.
enum Call {
    GetSession,
    DeleteSession,
    PutSession,
    RecognizeText,
    RecognizeUtterance,
}

impl Call {
    fn name(&self) -> &'static str {
        match self {
            Self::GetSession => "GetSession",
            Self::DeleteSession => "DeleteSession",
            Self::PutSession => "PutSession",
            Self::RecognizeText => "RecognizeText",
            Self::RecognizeUtterance => "RecognizeUtterance",
        }
    }

    /// Does this call answer from a session that is already STORED? A read and
    /// a delete do, so a case that expects one of them to be served seeds the
    /// session first; `PutSession` and the two encodings of a turn create the
    /// session they address. Every one of them resolves the bot ADDRESS first,
    /// whichever side of this it is on.
    fn needs_a_stored_session(&self) -> bool {
        match self {
            Self::GetSession | Self::DeleteSession => true,
            Self::PutSession | Self::RecognizeText | Self::RecognizeUtterance => false,
        }
    }

    /// Send this call against `path`, with a body this operation's own model
    /// accepts and a dialog state the fixture bot's locale defines, so nothing
    /// but the addressing can refuse it.
    async fn send(&self, emulator: &Emulator, path: &str) -> crate::harness::Reply {
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

/// EVERY operation this service binds to a session address — the five that bind
/// the same four `@httpLabel` values, each of which resolves that address
/// against the control plane before the session store is touched.
fn session_calls() -> [Call; 5] {
    [
        Call::GetSession,
        Call::DeleteSession,
        Call::PutSession,
        Call::RecognizeText,
        Call::RecognizeUtterance,
    ]
}

/// The sentence AWS states for a `botId`/`botAliasId` pair that is not one of
/// its bot's: the two ids from the request, then fixed text.
fn alias_not_valid(bot_id: &str, bot_alias_id: &str) -> String {
    format!("{bot_id}/{bot_alias_id} is not a valid BotId/BotAliasId combination.")
}

/// The sentence AWS states for a session that is not there.
fn session_not_found(session_id: &str) -> String {
    format!("The session for user {session_id} doesn\u{2019}t exist or has expired.")
}

/// Seed the session the path addresses with `PutSession`, so a call that answers
/// from a STORED session has one to answer from — what those rows are about is
/// the addressing, not whether a conversation happens to be in progress.
async fn seed(emulator: &Emulator, path: &str, case: &str) {
    let reply = Call::PutSession.send(emulator, path).await;
    assert_eq!(
        reply.status,
        200,
        "case {case:?}: seeding {path} must succeed, got {} {:?}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
}

/// Rule 1, on the alias: the pair is resolved, so an alias id the bot does not
/// have — however well formed, and even when it is another bot's — is the
/// modelled 404, and the refused call leaves no session behind.
#[tokio::test]
async fn a_call_is_refused_unless_the_bot_alias_pair_resolves() {
    let emulator = Emulator::new();
    let bot = emulator.built_bot("AddressedBot", &["en_US"]).await;
    let other = emulator.built_bot("OtherBot", &["en_US"]).await;
    // An alias that IS an alias — of the other bot, not of this one.
    let other_alias = emulator.second_alias(&other, "OTHERALIAS");

    // (case, botId, botAliasId, resolves)
    //
    // The rows differ along what the pair can be wrong in: the alias id (a
    // well-formed id of no alias, an id that is an alias of another bot, the
    // test alias id spelled in another case) and the bot id (one no bot was
    // created under).
    let rows: Vec<(&str, String, String, bool)> = vec![
        (
            "the bot's own alias",
            bot.bot_id.clone(),
            bot.alias_id.clone(),
            true,
        ),
        (
            "a well-formed alias id the bot does not have",
            bot.bot_id.clone(),
            "XXXXXXXXXX".to_string(),
            false,
        ),
        (
            "an alias of another bot",
            bot.bot_id.clone(),
            other_alias.alias_id.clone(),
            false,
        ),
        (
            "the test alias id in lower case",
            bot.bot_id.clone(),
            bot.alias_id.to_lowercase(),
            false,
        ),
        (
            "a botId no bot was created under",
            "0123456789".to_string(),
            bot.alias_id.clone(),
            false,
        ),
        (
            "the other bot addressed through its own alias",
            other.bot_id.clone(),
            other_alias.alias_id.clone(),
            true,
        ),
    ];

    for (index, (case, bot_id, bot_alias_id, resolves)) in rows.iter().enumerate() {
        for call in session_calls() {
            let session_id = format!("alias-{index}-{}", call.name());
            let path = format!(
                "/bots/{bot_id}/botAliases/{bot_alias_id}/botLocales/{}/sessions/{session_id}",
                bot.locale()
            );
            // A row whose pair resolves has to find the call servable, so a
            // read or a delete is given the session it answers from.
            if *resolves && call.needs_a_stored_session() {
                seed(&emulator, &path, case).await;
            }
            let reply = call.send(&emulator, &path).await;
            if *resolves {
                assert_eq!(
                    reply.status,
                    200,
                    "case {case:?} on {}: the pair resolves, so the call is served, got {} {:?}",
                    call.name(),
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                );
                continue;
            }
            assert_eq!(
                reply.status,
                404,
                "case {case:?} on {}: expected HTTP 404, got {} {:?}",
                call.name(),
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            assert!(
                reply.error_type().ends_with("ResourceNotFoundException"),
                "case {case:?} on {}: expected ResourceNotFoundException, got {:?}",
                call.name(),
                reply.error_type()
            );
            let message = reply.error_message();
            // The pair resolves to no alias of that bot, so every call — the
            // read and the delete included — carries the PAIR's own sentence.
            let expected = alias_not_valid(bot_id, bot_alias_id);
            assert_eq!(
                message, expected,
                "case {case:?} on {}: expected {expected:?}, got {message:?}",
                call.name()
            );

            // The refusal had no effect: the same session id on the bot's REAL
            // alias is still absent.
            let read = emulator
                .send("GET", &bot.session_path(&session_id), None)
                .await;
            assert_eq!(
                read.status,
                404,
                "case {case:?} on {}: the refused call must create no session, got {} {:?}",
                call.name(),
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            assert_eq!(
                read.error_message(),
                session_not_found(&session_id),
                "case {case:?} on {}: an absent session is stated as AWS states it",
                call.name()
            );
        }
    }
}

/// Rule 1, on the locale: a locale with no built model has no runtime model, so
/// the call is refused until the build settles — and the session the refused
/// call would have started does not exist, before or after the build.
#[tokio::test]
async fn a_call_is_refused_until_the_locale_is_built() {
    let emulator = Emulator::new();
    let bot_id = emulator.create_bot("UnbuiltBot").await;
    // Two locales: one that will be built, one left as created.
    for locale_id in ["en_US", "en_GB"] {
        emulator.create_locale(&bot_id, locale_id).await;
        emulator.create_intent_with_slot(&bot_id, locale_id).await;
    }
    emulator.settle_control_plane();
    for locale_id in ["en_US", "en_GB"] {
        let status = emulator.bot_locale_status(&bot_id, locale_id).await;
        assert_eq!(
            status, "NotBuilt",
            "locale {locale_id:?}: a created locale is not built yet, got {status:?}"
        );
    }

    let bot = Bot {
        bot_id: bot_id.clone(),
        // Every bot is created with the alias Amazon Lex keeps for testing it,
        // so that is the alias this one is addressed through.
        alias_id: TEST_ALIAS.to_string(),
        locales: vec!["en_US".to_string()],
    };

    // (case, localeId) — every row is a locale with no built model: one created
    // but never built, one the bot has no locale for at all, and one outside the
    // locales the service supports. None of them has a runtime model, so none of
    // them serves a call that interprets input; the session store holds nothing
    // for any of them either.
    let unbuilt: [(&str, &str); 3] = [
        ("a created locale that was never built", "en_US"),
        ("a locale the bot has no record for", "de_DE"),
        ("a locale the service does not support", "xx_XX"),
    ];

    for (case, locale_id) in unbuilt {
        for call in session_calls() {
            let session_id = format!("locale-{}", call.name());
            let path = format!(
                "/bots/{bot_id}/botAliases/{}/botLocales/{locale_id}/sessions/{session_id}",
                bot.alias_id
            );
            let reply = call.send(&emulator, &path).await;
            assert_eq!(
                reply.status,
                404,
                "case {case:?} on {}: expected HTTP 404, got {} {:?}",
                call.name(),
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            assert!(
                reply.error_type().ends_with("ResourceNotFoundException"),
                "case {case:?} on {}: expected ResourceNotFoundException, got {:?}",
                call.name(),
                reply.error_type()
            );
            // Every call resolves the locale behind the labels, so every one of
            // them is refused as the ADDRESS on every row here.
            let expected = LOCALE_NOT_VALID.to_string();
            let message = reply.error_message();
            assert_eq!(
                message, expected,
                "case {case:?} on {}: expected {expected:?}, got {message:?}",
                call.name()
            );
        }
    }

    // Building the locale is what makes the call servable — and the sessions the
    // refused calls would have started are still absent afterwards.
    emulator.build_locale(&bot_id, "en_US").await;
    let status = emulator.bot_locale_status(&bot_id, "en_US").await;
    assert_eq!(
        status, "Built",
        "the submitted build must have settled, got {status:?}"
    );
    for call in session_calls() {
        let session_id = format!("locale-{}", call.name());
        let read = emulator
            .send("GET", &bot.session_path(&session_id), None)
            .await;
        assert_eq!(
            read.status,
            404,
            "on {}: the refused call must have created no session, got {} {:?}",
            call.name(),
            read.status,
            String::from_utf8_lossy(&read.body)
        );

        let servable = bot.session_path(&format!("built-{}", call.name()));
        if call.needs_a_stored_session() {
            seed(&emulator, &servable, "the built locale").await;
        }
        let served = call.send(&emulator, &servable).await;
        assert_eq!(
            served.status,
            200,
            "on {}: the built locale serves the call, got {} {:?}",
            call.name(),
            served.status,
            String::from_utf8_lossy(&served.body)
        );
    }

    // The locale that was never built is still refused: building one locale
    // builds no other.
    let reply = Call::RecognizeText
        .send(
            &emulator,
            &format!(
                "/bots/{bot_id}/botAliases/{}/botLocales/en_GB/sessions/still-unbuilt",
                bot.alias_id
            ),
        )
        .await;
    assert_eq!(
        reply.status,
        404,
        "the locale that was not built is still refused, got {} {:?}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
}

/// Rule 1, after the fact: once the control plane drops the locale a session was
/// stored in, that address resolves to no locale this runtime can serve, so every
/// call on it — the read and the delete included — is refused as the ADDRESS;
/// and the session the runtime stored is left exactly as it was, which the read
/// shows once the locale is built again.
#[tokio::test]
async fn a_stored_session_outlives_the_control_plane_dropping_its_locale() {
    let (emulator, bot) = Emulator::with_bot().await;
    let session_id = "survives-the-drop";
    let path = bot.session_path(session_id);

    let put = emulator
        .send(
            "POST",
            &path,
            Some(json!({
                "sessionState": {
                    "dialogAction": {"type": "Delegate"},
                    "sessionAttributes": {"marker": "kept"},
                },
            })),
        )
        .await;
    assert_eq!(
        put.status,
        200,
        "the session is stored while the address resolves, got {} {:?}",
        put.status,
        String::from_utf8_lossy(&put.body)
    );

    // The control plane drops the locale that session is held in, as a client
    // drops it: "when you delete a locale, all intents, slots, and slot types
    // defined for the locale are also deleted".
    let dropped = emulator
        .send_control_plane(
            "DELETE",
            &format!(
                "/bots/{}/botversions/{DRAFT}/botlocales/{}",
                bot.bot_id,
                bot.locale()
            ),
            None,
        )
        .await;
    assert_eq!(
        dropped.status,
        202,
        "DeleteBotLocale must be accepted, got {} {:?}",
        dropped.status,
        String::from_utf8_lossy(&dropped.body)
    );

    // The address now resolves to no locale the runtime can serve, so every call
    // on it is refused with the locale's own sentence.
    for call in session_calls() {
        let reply = call.send(&emulator, &path).await;
        assert_eq!(
            reply.status,
            404,
            "on {}: a dropped locale is refused, got {} {:?}",
            call.name(),
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        assert!(
            reply.error_type().ends_with("ResourceNotFoundException"),
            "on {}: expected ResourceNotFoundException, got {:?}",
            call.name(),
            reply.error_type()
        );
        let message = reply.error_message();
        assert_eq!(
            message,
            LOCALE_NOT_VALID,
            "on {}: expected the locale's own sentence, got {message:?}",
            call.name()
        );
    }

    // And the READ is refused the same way while the locale is dropped: the
    // session is still in this runtime's store, but the address it is held under
    // no longer resolves, so what the caller is told is that the address is not
    // one Amazon Lex V2 serves.
    let while_dropped = emulator.send("GET", &path, None).await;
    assert_eq!(
        while_dropped.status,
        404,
        "the read is refused while the locale is dropped, got {} {:?}",
        while_dropped.status,
        String::from_utf8_lossy(&while_dropped.body)
    );
    assert_eq!(
        while_dropped.error_message(),
        LOCALE_NOT_VALID,
        "the read carries the locale's own sentence, got {:?}",
        while_dropped.error_message()
    );

    // Built again, and the session is exactly as it was left: none of the
    // refused calls touched the store.
    emulator.create_locale(&bot.bot_id, bot.locale()).await;
    emulator
        .create_intent_with_slot(&bot.bot_id, bot.locale())
        .await;
    emulator.build_locale(&bot.bot_id, bot.locale()).await;

    let read = emulator.send("GET", &path, None).await;
    assert_eq!(
        read.status,
        200,
        "the refusals removed nothing, got {} {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    let stored = read.json();
    assert_eq!(
        stored
            .pointer("/sessionState/sessionAttributes/marker")
            .and_then(Value::as_str),
        Some("kept"),
        "the stored session is the one that was seeded, got {stored:?}"
    );

    // And the delete the dropped locale refused is served now that the address
    // resolves again.
    let deleted = emulator.send("DELETE", &path, None).await;
    assert_eq!(
        deleted.status,
        200,
        "the delete is served against a built locale, got {} {:?}",
        deleted.status,
        String::from_utf8_lossy(&deleted.body)
    );
    let gone = emulator.send("GET", &path, None).await;
    assert_eq!(
        gone.status,
        404,
        "the deleted session is gone, got {} {:?}",
        gone.status,
        String::from_utf8_lossy(&gone.body)
    );
    assert_eq!(
        gone.error_message(),
        session_not_found(session_id),
        "the absent session is stated as AWS states it, got {:?}",
        gone.error_message()
    );
}

/// Rule 2: a seeded dialog state names resources the locale defines.
#[tokio::test]
async fn a_seeded_session_state_must_name_what_the_locale_defines() {
    let (emulator, bot) = Emulator::with_bot().await;

    // (case, sessionState, the refusal owed — None where the seed is stored)
    //
    // The rows differ along what the seed can name: an intent (one the locale
    // declares, the one it was created with, one it does not declare, one
    // differing only in case) and the slot an `ElicitSlot` names (one of that
    // intent, one of no intent), plus a seed that names neither.
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "no intent named at all",
            json!({"dialogAction": {"type": "Delegate"}}),
            None,
        ),
        (
            "an intent the locale declares",
            json!({"intent": {"name": INTENT, "state": "InProgress"}}),
            None,
        ),
        (
            "the intent the locale was created with",
            json!({"intent": {"name": FALLBACK_INTENT}}),
            None,
        ),
        (
            "an intent the locale does not declare",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": SLOT},
                "intent": {"name": "NoSuchIntent", "state": "InProgress"},
            }),
            Some("The intent is invalid."),
        ),
        (
            "an intent differing from a declared one only in case",
            json!({"intent": {"name": INTENT.to_lowercase()}}),
            Some("The intent is invalid."),
        ),
        (
            "eliciting a slot of that intent",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": SLOT},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            None,
        ),
        (
            "eliciting a slot the intent does not declare",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": "Flavor"},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            Some("The slot is invalid."),
        ),
    ];

    for (index, (case, state, refusal)) in rows.iter().enumerate() {
        let session_id = format!("seed-{index}");
        let path = bot.session_path(&session_id);
        let reply = emulator
            .send("POST", &path, Some(json!({"sessionState": state})))
            .await;
        match refusal {
            None => {
                assert_eq!(
                    reply.status,
                    200,
                    "case {case:?}: the seed names what the locale defines, got {} {:?}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                );
                assert_eq!(
                    emulator.send("GET", &path, None).await.status,
                    200,
                    "case {case:?}: the stored seed must be readable"
                );
            }
            Some(expected) => {
                assert_eq!(
                    reply.status,
                    400,
                    "case {case:?}: expected HTTP 400, got {} {:?}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                );
                assert!(
                    reply.error_type().ends_with("ValidationException"),
                    "case {case:?}: expected ValidationException, got {:?}",
                    reply.error_type()
                );
                let message = reply.error_message();
                assert_eq!(
                    message, *expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                // The refused seed stored nothing.
                let read = emulator.send("GET", &path, None).await;
                assert_eq!(
                    read.status,
                    404,
                    "case {case:?}: a refused seed must create no session, got {} {:?}",
                    read.status,
                    String::from_utf8_lossy(&read.body)
                );
                assert_eq!(
                    read.error_message(),
                    session_not_found(&session_id),
                    "case {case:?}: an absent session is stated as AWS states it"
                );
            }
        }
    }
}

/// Rule 3: the messages of a seeded dialog state are the bot's own, and a
/// request's own messages win over them.
#[tokio::test]
async fn a_seeded_dialog_state_answers_with_the_bots_authored_messages() {
    let (emulator, bot) = Emulator::with_bot().await;

    // (case, sessionState, messages in the request, the contents owed)
    //
    // The rows span the dialog states a bot authors messages for — eliciting a
    // slot, confirming an intent, closing one — plus the states it authors none
    // for and the case where the request carries its own.
    let rows: Vec<(&str, Value, Option<Value>, Vec<&str>)> = vec![
        (
            "eliciting the intent's prompted slot",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": SLOT},
                "intent": {"name": INTENT, "state": "InProgress"},
                "sessionAttributes": {"seededBy": "put-session"},
            }),
            None,
            vec![SLOT_PROMPT],
        ),
        (
            "confirming the intent",
            json!({
                "dialogAction": {"type": "ConfirmIntent"},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            None,
            vec![CONFIRMATION_PROMPT],
        ),
        (
            "closing the intent",
            json!({
                "dialogAction": {"type": "Close"},
                "intent": {"name": INTENT, "state": "ReadyForFulfillment"},
            }),
            None,
            vec![CLOSING_MESSAGE],
        ),
        (
            "delegating the next step, which the bot authors no message for",
            json!({
                "dialogAction": {"type": "Delegate"},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            None,
            vec![],
        ),
        (
            "eliciting an intent, with none named",
            json!({"dialogAction": {"type": "ElicitIntent"}}),
            None,
            vec![],
        ),
        (
            "the caller's own messages, which are what was last sent",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": SLOT},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            Some(json!([
                {"content": "Which size?", "contentType": "PlainText"},
                {"content": "Small, medium or large?", "contentType": "PlainText"},
            ])),
            vec!["Which size?", "Small, medium or large?"],
        ),
        (
            "an empty message list, which says the caller sent none of its own",
            json!({
                "dialogAction": {"type": "ElicitSlot", "slotToElicit": SLOT},
                "intent": {"name": INTENT, "state": "InProgress"},
            }),
            Some(json!([])),
            vec![],
        ),
    ];

    for (index, (case, state, messages, expected)) in rows.iter().enumerate() {
        let path = bot.session_path(&format!("messages-{index}"));
        let mut body = json!({"sessionState": state});
        if let Some(messages) = messages {
            body["messages"] = messages.clone();
        }
        let put = emulator.send("POST", &path, Some(body)).await;
        assert_eq!(
            put.status,
            200,
            "case {case:?}: PutSession must succeed, got {} {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );

        let header = put.lex_header("x-amz-lex-messages");
        let actual = contents(header.as_ref());
        assert_eq!(
            actual, *expected,
            "case {case:?}: expected the messages {expected:?}, got {actual:?}"
        );
        // Every message reported carries the type the bot authored it in.
        if let Some(reported) = header.as_ref().and_then(Value::as_array) {
            for message in reported {
                assert_eq!(
                    message.get("contentType").and_then(Value::as_str),
                    Some("PlainText"),
                    "case {case:?}: a plain-text message is reported as PlainText, \
                     got {message:?}"
                );
            }
        }

        // What was last sent to the user is session state: a read reports the
        // same messages.
        let stored = emulator.send("GET", &path, None).await.json();
        let read_back = contents(stored.get("messages"));
        assert_eq!(
            read_back, *expected,
            "case {case:?}: GetSession must report the same messages, got {stored:?}"
        );
    }
}

/// The `content` of each message of a `messages` list, in order.
fn contents(messages: Option<&Value>) -> Vec<&str> {
    messages
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|message| message.get("content").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

// --- Rule 4: an identifier the bot address cannot be read from ---------------

/// The sentence the Lex V2 runtime answers a request whose bot identifier is not
/// a well-formed one with. Entirely FIXED text: no member name, no echoed value
/// and no bound appear in it.
const INVALID_BOT_OR_ALIAS: &str = "INVALID_REQUEST - Invalid bot name or alias";

/// Rule 4: the bot identifier is the first thing a session address is read from,
/// so one that is not a well-formed `BotIdentifier` — whatever it fails, and
/// whatever else the request also gets wrong — is HTTP 400 `ValidationException`
/// in the runtime's own fixed words, from every operation. A well-formed one is
/// never refused that way, whether or not a bot was created under it.
#[tokio::test]
async fn a_malformed_bot_identifier_is_refused_in_the_runtimes_own_fixed_words() {
    let (emulator, bot) = Emulator::with_bot().await;

    // (case, botId, sessionId, the botId is well formed)
    //
    // The rows differ along what a value can fail in the modelled identifier
    // (`BotIdentifier`: length exactly 10, pattern `^[0-9a-zA-Z]+$`) — too long,
    // too short, each published bound met exactly, and a character the pattern
    // excludes at either end — plus a row whose session id is malformed too, so
    // which of two refused labels is reported is pinned as well.
    let rows: Vec<(&str, String, String, bool)> = vec![
        (
            "the bot's own identifier: ten alphanumeric characters",
            bot.bot_id.clone(),
            "well-formed-bot".to_string(),
            true,
        ),
        (
            "ten alphanumeric characters no bot was created under",
            "0123456789".to_string(),
            "unknown-bot".to_string(),
            true,
        ),
        (
            "eleven alphanumeric characters: one past the maximum",
            "AAAAAAAAAAA".to_string(),
            "too-long".to_string(),
            false,
        ),
        (
            "nine alphanumeric characters: one below the minimum",
            "AAAAAAAAA".to_string(),
            "too-short".to_string(),
            false,
        ),
        (
            "four alphanumeric characters",
            "ABCD".to_string(),
            "far-too-short".to_string(),
            false,
        ),
        (
            "ten characters, one of them a hyphen",
            "ABCD-56789".to_string(),
            "hyphenated".to_string(),
            false,
        ),
        (
            "ten characters, the last an underscore",
            "ABCD56789_".to_string(),
            "underscored".to_string(),
            false,
        ),
        (
            "ten characters, the first a dot",
            ".ABCD56789".to_string(),
            "dotted".to_string(),
            false,
        ),
        (
            "a malformed bot identifier and a malformed session id",
            "AAAAAAAAAAA".to_string(),
            "a".to_string(),
            false,
        ),
    ];

    for (case, bot_id, session_id, well_formed) in &rows {
        for call in session_calls() {
            let path = format!(
                "/bots/{bot_id}/botAliases/{}/botLocales/{}/sessions/{session_id}",
                bot.alias_id,
                bot.locale()
            );
            let reply = call.send(&emulator, &path).await;
            if *well_formed {
                // A well-formed identifier is read as one: the call is served, or
                // refused for what it addresses, never for how it is spelled.
                assert!(
                    reply.status == 200 || reply.status == 404,
                    "case {case:?} on {}: a well-formed botId is not a refused value, so the \
                     call is served or its resource absent, got {} {:?}",
                    call.name(),
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                );
                if reply.status == 404 {
                    // What is absent is the resource the labels address — the
                    // session, or the bot the pair names — which is rule 1's
                    // refusal, not a refused value.
                    assert!(
                        reply.error_type().ends_with("ResourceNotFoundException"),
                        "case {case:?} on {}: an absent resource is the modelled 404, got {:?}",
                        call.name(),
                        reply.error_type()
                    );
                }
                continue;
            }
            assert_eq!(
                reply.status,
                400,
                "case {case:?} on {}: expected HTTP 400, got {} {:?}",
                call.name(),
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            assert!(
                reply.error_type().ends_with("ValidationException"),
                "case {case:?} on {}: expected ValidationException, got {:?}",
                call.name(),
                reply.error_type()
            );
            let message = reply.error_message();
            assert_eq!(
                message, INVALID_BOT_OR_ALIAS,
                "case {case:?} on {}: expected the runtime's fixed sentence, got {message:?}",
                call.name()
            );
            // The sentence is fixed, so nothing of the request is in it: neither
            // the value the caller sent nor the member it was bound to.
            assert!(
                !message.contains(bot_id.as_str()) && !message.contains("botId"),
                "case {case:?} on {}: the sentence interpolates nothing, got {message:?}",
                call.name()
            );
        }
    }
}

/// Rule 4, the other side: the reword is confined to the bot identifier, so a
/// refusal that blames another label still reports THAT label — a client fixing
/// a session id is told which value it was.
#[tokio::test]
async fn a_refusal_that_blames_another_label_keeps_its_own_report() {
    let (emulator, bot) = Emulator::with_bot().await;

    // (case, sessionId) — the rows span what the modelled `SessionId`
    // (length 2..100, pattern `^[0-9a-zA-Z._:-]+$`) can fail, with the bot
    // identifier well formed in every one.
    let rows: Vec<(&str, String)> = vec![
        (
            "one character: one below the minimum of 2",
            "a".to_string(),
        ),
        (
            "101 characters: one past the maximum of 100",
            "a".repeat(101),
        ),
        (
            "a character the pattern excludes",
            "session~id".to_string(),
        ),
    ];

    for (case, session_id) in &rows {
        for call in session_calls() {
            let path = format!(
                "/bots/{}/botAliases/{}/botLocales/{}/sessions/{session_id}",
                bot.bot_id,
                bot.alias_id,
                bot.locale()
            );
            let reply = call.send(&emulator, &path).await;
            assert_eq!(
                reply.status,
                400,
                "case {case:?} on {}: expected HTTP 400, got {} {:?}",
                call.name(),
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            let message = reply.error_message();
            assert_ne!(
                message, INVALID_BOT_OR_ALIAS,
                "case {case:?} on {}: the bot address is well formed, so this is not its \
                 refusal, got {message:?}",
                call.name()
            );
            assert!(
                message.contains("sessionId"),
                "case {case:?} on {}: expected the refused label to be named, got {message:?}",
                call.name()
            );
        }
    }
}
