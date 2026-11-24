// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListBotAliases`.
//!
//! Every request goes through the same `MultiServiceRouter::dispatch` the emulator
//! serves on its socket, so the restJson1 routing, the modelled constraints and the
//! error envelopes are exercised exactly as a client sees them. The harness and the bot
//! setup are the other suites', reused rather than copied.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one case:
//!
//! * a listing is built from the aliases the bot the URI names holds, and every bot
//!   holds the alias Amazon Lex keeps for testing it from the moment it is created, so
//!   the listing of a bot that was only created already reports that alias — read back
//!   off the stored record, the same values on every call;
//! * the listing is scoped to that bot: an alias of another bot is not in it, a bot id
//!   nothing backs lists nothing, and an alias does not outlive its bot;
//! * the listing is ordered and paged: a page holds no more than `maxResults` entries
//!   and `nextToken` resumes the same total order without losing or repeating an entry;
//! * a page size or token the model or this operation does not accept is refused.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_lexmodelsv2_lexmodelbuildingservicev2::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::bot_lifecycle::{call, create_bot, create_body, emulator, error_code, member, seconds, text};

/// `ListBotAliases`'s URI: `POST /bots/{botId}/botaliases`.
fn aliases_uri(bot_id: &str) -> String {
    format!("/bots/{bot_id}/botaliases")
}

/// List one bot's aliases; `body` carries the paging members.
async fn list(router: &MultiServiceRouter, bot_id: &str, body: Value) -> (u16, Value) {
    call(router, "POST", &aliases_uri(bot_id), Some(body)).await
}

/// The summaries of a listing, in the order the response put them.
fn summaries(response: &Value) -> Vec<Value> {
    member(response, "botAliasSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `botAliasId`s of a listing, in the order the response put them.
fn listed_ids(response: &Value) -> Vec<String> {
    summaries(response)
        .iter()
        .map(|entry| text(entry, "botAliasId"))
        .collect()
}

/// The bots every listing assertion is run over: rows differ in which optional members
/// the bot was created with, in its type, in the session TTL and in the shape of the
/// name — none of which changes the alias the bot is created with.
fn bot_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "only the required members",
            create_body("AliasMinimalBot", 300, json!({})),
        ),
        (
            "a bot carrying tags for its test alias",
            create_body(
                "AliasTaggedBot",
                1_800,
                json!({
                    "description": "a bot whose test alias was tagged as it was created",
                    "botTags": { "team": "voice" },
                    "testBotAliasTags": { "env": "test" },
                }),
            ),
        ),
        (
            "a bot network",
            create_body(
                "AliasNetworkBot",
                600,
                json!({
                    "botType": "BotNetwork",
                    "botMembers": [{
                        "botMemberId": "ABCDEFGHIJ",
                        "botMemberName": "MemberOne",
                        "botMemberAliasId": state::TEST_BOT_ALIAS_ID,
                        "botMemberAliasName": state::TEST_BOT_ALIAS_NAME,
                        "botMemberVersion": state::DRAFT_BOT_VERSION,
                    }],
                }),
            ),
        ),
        (
            "a name using the separators the modelled pattern allows",
            create_body("Alias_Bot-Name_2", 120, json!({})),
        ),
    ]
}

/// Assert `summary` is the alias Amazon Lex keeps for testing the bot, naming `case` on
/// every failure.
fn assert_test_alias(case: &str, summary: &Value) {
    assert_eq!(
        text(summary, "botAliasId"),
        state::TEST_BOT_ALIAS_ID,
        "case {case:?}: expected the test alias id, got {summary:?}"
    );
    assert_eq!(
        text(summary, "botAliasName"),
        state::TEST_BOT_ALIAS_NAME,
        "case {case:?}: expected the test alias name, got {summary:?}"
    );
    assert_eq!(
        text(summary, "botVersion"),
        state::DRAFT_BOT_VERSION,
        "case {case:?}: the test alias points at the bot's working copy, got {summary:?}"
    );
    assert_eq!(
        text(summary, "botAliasStatus"),
        state::BOT_ALIAS_STATUS_AVAILABLE,
        "case {case:?}: expected the alias to be ready for use, got {summary:?}"
    );
    assert!(
        seconds(summary, "creationDateTime").is_finite(),
        "case {case:?}: the summary must report when the alias was created, got {summary:?}"
    );
    assert!(
        seconds(summary, "lastUpdatedDateTime").is_finite(),
        "case {case:?}: the summary must report when the alias was last updated, got {summary:?}"
    );
}

#[tokio::test]
async fn a_listing_reports_the_alias_every_created_bot_holds() {
    let router = emulator();

    for (case, request) in bot_cases() {
        let bot_id = create_bot(&router, &request).await;

        let (status, response) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListBotAliases to answer 200, got {status} {response:?}"
        );
        assert_eq!(
            text(&response, "botId"),
            bot_id,
            "case {case:?}: the listing must echo the bot it was scoped to, got {response:?}"
        );
        assert_eq!(
            member(&response, "nextToken"),
            Value::Null,
            "case {case:?}: a listing that fits one page carries no token, got {response:?}"
        );
        let listed = summaries(&response);
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: a bot that was only created holds the test alias alone, got {response:?}"
        );
        assert_test_alias(case, &listed[0]);

        // Reading the listing again reports the same stored record: the summary is read
        // back off state, not minted per call, so nothing a caller sees moves because it
        // looked.
        let (_, again) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            summaries(&again),
            listed,
            "case {case:?}: a second listing must report the same stored alias, got {again:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_holds_no_alias_of_another_bot() {
    let router = emulator();

    let mut bots: Vec<String> = Vec::new();
    for (case, request) in bot_cases() {
        let bot_id = create_bot(&router, &request).await;
        bots.push(bot_id.clone());

        let (status, response) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        for entry in summaries(&response) {
            assert_eq!(
                text(&entry, "botAliasId"),
                state::TEST_BOT_ALIAS_ID,
                "case {case:?}: the listing must hold only this bot's aliases, got {entry:?}"
            );
        }
    }
    assert_eq!(
        bots.iter().cloned().collect::<BTreeSet<_>>().len(),
        bots.len(),
        "each case must have created its own bot, got {bots:?}"
    );

    // A bot id nothing backs holds no alias; this operation models no not-found error for
    // it, so the listing is empty rather than refused.
    let (status, response) = list(&router, "ZZZZZZZZZZ", json!({})).await;
    assert_eq!(
        status, 200,
        "a bot id nothing backs must still answer 200, got {status} {response:?}"
    );
    assert_eq!(
        listed_ids(&response),
        Vec::<String>::new(),
        "a bot id nothing backs lists no alias, got {response:?}"
    );
    assert_eq!(
        text(&response, "botId"),
        "ZZZZZZZZZZ",
        "the listing must echo the bot it was scoped to, got {response:?}"
    );
}

#[tokio::test]
async fn an_alias_does_not_outlive_the_bot_it_belongs_to() {
    let router = emulator();

    // Rows differ in whether the delete asks to skip the in-use check, which is the only
    // member `DeleteBot` takes beyond the bot it names.
    for (case, skip) in [("with the in-use check", false), ("skipping it", true)] {
        let bot_id = create_bot(&router, &create_body(
            &format!("AliasDeleted{}", u8::from(skip)),
            300,
            json!({}),
        ))
        .await;
        let (_, response) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            listed_ids(&response),
            vec![state::TEST_BOT_ALIAS_ID.to_string()],
            "case {case:?}: the created bot must hold its test alias, got {response:?}"
        );

        let uri = format!("/bots/{bot_id}?skipResourceInUseCheck={skip}");
        let (status, deleted) = call(&router, "DELETE", &uri, None).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected DeleteBot to answer 202, got {status} {deleted:?}"
        );

        let (status, response) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        assert_eq!(
            listed_ids(&response),
            Vec::<String>::new(),
            "case {case:?}: an alias must not outlive its bot, got {response:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_is_paged_by_the_page_size_the_request_asks_for() {
    let router = emulator();
    let bot_id = create_bot(&router, &create_body("AliasPagedBot", 300, json!({}))).await;
    let held = vec![state::TEST_BOT_ALIAS_ID.to_string()];

    // Rows differ in the page size: one page per entry, a size larger than the listing,
    // and both published bounds of the modelled range.
    for page_size in [1_usize, 2, 5, state::MAX_LIST_PAGE] {
        let case = format!("maxResults {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({ "maxResults": page_size });
            if let Some(token) = &token {
                body["nextToken"] = json!(token);
            }
            let (status, response) = list(&router, &bot_id, body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {response:?}"
            );
            let ids = listed_ids(&response);
            assert!(
                ids.len() <= page_size,
                "case {case:?}: a page must hold at most {page_size} entries, got {ids:?}"
            );
            walked.extend(ids);
            pages += 1;
            assert!(
                pages <= held.len() + 1,
                "case {case:?}: paging must terminate, walked {walked:?}"
            );
            match member(&response, "nextToken").as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, held,
            "case {case:?}: paging must walk the listing once, in order, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_request_outside_what_the_model_accepts_is_refused() {
    let router = emulator();
    let bot_id = create_bot(&router, &create_body("AliasRefusedBot", 300, json!({}))).await;

    // Rows differ in which member is outside what the model or this operation accepts:
    // the page size one past each published bound of the modelled range 1..1000, and a
    // token this operation never issued.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a page size below the published minimum",
            json!({ "maxResults": 0 }),
        ),
        (
            "a page size above the published maximum",
            json!({ "maxResults": state::MAX_LIST_PAGE + 1 }),
        ),
        (
            "a token this operation never issued",
            json!({ "nextToken": "not-a-token" }),
        ),
    ];

    for (case, body) in &rows {
        let (status, response) = list(&router, &bot_id, body.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the request must be refused with 400, got {status} {response:?}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {response:?}"
        );
    }

    // Each published bound of the range is accepted, which is what makes the rows above a
    // refusal of what lies outside it rather than of the bound itself.
    for page_size in [1_usize, state::MAX_LIST_PAGE] {
        let (status, response) = list(&router, &bot_id, json!({ "maxResults": page_size })).await;
        assert_eq!(
            status, 200,
            "maxResults {page_size} is a published bound and must be accepted, got {status} {response:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The alias a caller CREATES, beside the one every bot is created with
// ---------------------------------------------------------------------------

/// `CreateBotAlias`'s URI: `PUT /bots/{botId}/botaliases`, which is [`aliases_uri`] under
/// another method; one alias is addressed under it by its own id.
fn alias_uri(bot_id: &str, bot_alias_id: &str) -> String {
    format!("{}/{bot_alias_id}", aliases_uri(bot_id))
}

/// Create an alias of a bot; the body carries every member but the bot, which the URI names.
async fn create_alias(router: &MultiServiceRouter, bot_id: &str, body: Value) -> (u16, Value) {
    call(router, "PUT", &aliases_uri(bot_id), Some(body)).await
}

/// Read one alias back: `DescribeBotAlias` is `GET /bots/{botId}/botaliases/{botAliasId}`.
async fn describe_alias(
    router: &MultiServiceRouter,
    bot_id: &str,
    bot_alias_id: &str,
) -> (u16, Value) {
    call(router, "GET", &alias_uri(bot_id, bot_alias_id), None).await
}

/// Delete one alias, with the resource-in-use check skipped where `skip` says so.
async fn delete_alias(
    router: &MultiServiceRouter,
    bot_id: &str,
    bot_alias_id: &str,
    skip: bool,
) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!(
            "{}?skipResourceInUseCheck={skip}",
            alias_uri(bot_id, bot_alias_id)
        ),
        None,
    )
    .await
}

/// A bot carrying one `en_US` locale with one intent on its `DRAFT` version, and one
/// NUMBERED version cut from that locale — which is the estate an alias points into.
///
/// Answers the minted `botId` and the number `CreateBotVersion` assigned, so a row that
/// points an alias at a version uses the number that service really handed out rather than
/// one written into the test.
async fn bot_with_a_version(router: &MultiServiceRouter, bot_name: &str) -> (String, String) {
    let bot_id = create_bot(router, &create_body(bot_name, 300, json!({}))).await;
    let (status, created) = call(
        router,
        "PUT",
        &crate::bot_locale::locales_uri(&bot_id, state::DRAFT_BOT_VERSION),
        Some(crate::bot_locale::locale_body("en_US", 0.4, json!({}))),
    )
    .await;
    assert_eq!(
        status, 202,
        "CreateBotLocale for {bot_name}: expected 202, got {status} {created:?}"
    );
    crate::intents_and_builds::create_intent(
        router,
        &bot_id,
        "en_US",
        &crate::intents_and_builds::intent_body(
            "OrderPizza",
            json!({ "sampleUtterances": [{ "utterance": "I want to order a pizza" }] }),
        ),
    )
    .await;
    let (status, version) = call(
        router,
        "PUT",
        &format!("/bots/{bot_id}/botversions"),
        Some(json!({
            "botVersionLocaleSpecification": {
                "en_US": { "sourceBotVersion": state::DRAFT_BOT_VERSION },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "CreateBotVersion for {bot_name}: expected 202, got {status} {version:?}"
    );
    (bot_id, text(&version, "botVersion"))
}

/// The alias the deadline waits for, read again until it carries `Available` — the status
/// `CreateBotAlias` documents an alias reaches "when the alias is ready to be used" — or the
/// generous deadline passes. Waits for the CONDITION rather than for a fixed time.
async fn settled_alias(router: &MultiServiceRouter, bot_id: &str, bot_alias_id: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (status, alias) = describe_alias(router, bot_id, bot_alias_id).await;
        assert_eq!(
            status, 200,
            "DescribeBotAlias {bot_alias_id}: expected 200, got {status} {alias:?}"
        );
        if text(&alias, "botAliasStatus") == state::BOT_ALIAS_STATUS_AVAILABLE
            || std::time::Instant::now() >= deadline
        {
            return alias;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// An alias a caller creates is the alias Amazon Lex reports back, points at the version the
/// create named, and stops resolving once it is deleted.
///
/// The rows differ along what the rule generalises over: whether the alias names a VERSION or
/// leaves it at the bot's working copy, whether it carries a description, per-locale settings,
/// conversation logging or tags, and the shape of its name.
#[tokio::test]
async fn an_alias_is_created_read_back_and_deleted() {
    // One row per shape of a create. The version placeholder is filled in with the number
    // `CreateBotVersion` really answered, so no row writes a version number of its own.
    let rows: Vec<(&str, bool, Value)> = vec![
        ("only the required members", false, json!({})),
        (
            "an alias pointing at a numbered version",
            true,
            json!({ "description": "the alias the stack publishes" }),
        ),
        (
            "an alias carrying per-locale settings and tags",
            true,
            json!({
                "botAliasLocaleSettings": { "en_US": { "enabled": true } },
                "tags": { "team": "voice" },
            }),
        ),
        (
            "an alias carrying conversation logging and a hyphenated name",
            false,
            json!({
                "description": "d".repeat(2_000),
                "conversationLogSettings": {
                    "textLogSettings": [{
                        "enabled": true,
                        "destination": {
                            "cloudWatch": {
                                "cloudWatchLogGroupArn":
                                    "arn:aws:logs:us-east-1:000000000000:log-group:/lex/alias",
                                "logPrefix": "aliases/",
                            },
                        },
                    }],
                },
            }),
        ),
    ];

    for (index, (case, points_at_version, extra)) in rows.iter().enumerate() {
        let router = emulator();
        let (bot_id, bot_version) =
            bot_with_a_version(&router, &format!("AliasCreateBot{index}")).await;
        let alias_name = format!("Prod-{index}");
        let mut body = json!({ "botAliasName": alias_name });
        let object = body.as_object_mut().expect("the body is a JSON object");
        for (key, value) in extra.as_object().expect("extras are a JSON object") {
            object.insert(key.clone(), value.clone());
        }
        let expected_version = if *points_at_version {
            object.insert("botVersion".to_string(), json!(bot_version));
            bot_version.clone()
        } else {
            // An alias created without a version points at the bot's working copy, which the
            // modelled `BotVersion` admits beside a number.
            state::DRAFT_BOT_VERSION.to_string()
        };

        let (status, created) = create_alias(&router, &bot_id, body.clone()).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected CreateBotAlias to answer 202, got {status} {created:?}"
        );
        let bot_alias_id = text(&created, "botAliasId");
        assert!(
            bot_alias_id.len() == 10
                && bot_alias_id.chars().all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: expected a minted ^[0-9a-zA-Z]{{10}}$ botAliasId, got \
             {bot_alias_id:?}"
        );
        assert_eq!(
            (
                text(&created, "botAliasStatus"),
                text(&created, "botVersion"),
                text(&created, "botId"),
            ),
            (
                state::BOT_ALIAS_STATUS_CREATING.to_string(),
                expected_version.clone(),
                bot_id.clone(),
            ),
            "case {case:?}: expected the create to report the alias it opened, got {created:?}"
        );

        // Read back off the stored record: every member the create was given is reported,
        // and the status the clock owed has been materialised.
        let alias = settled_alias(&router, &bot_id, &bot_alias_id).await;
        assert_eq!(
            (
                text(&alias, "botAliasId"),
                text(&alias, "botAliasName"),
                text(&alias, "botVersion"),
                text(&alias, "botAliasStatus"),
            ),
            (
                bot_alias_id.clone(),
                alias_name.clone(),
                expected_version.clone(),
                state::BOT_ALIAS_STATUS_AVAILABLE.to_string(),
            ),
            "case {case:?}: expected the alias to read back as created, got {alias:?}"
        );
        for optional in [
            "description",
            "botAliasLocaleSettings",
            "conversationLogSettings",
        ] {
            assert_eq!(
                member(&alias, optional),
                member(&created, optional),
                "case {case:?}: expected {optional} to read back as the create reported it, \
                 got {alias:?}"
            );
        }

        // The listing of the bot's aliases holds it beside the test alias every bot carries.
        let (status, listed) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListBotAliases to answer 200, got {status} {listed:?}"
        );
        let ids = listed_ids(&listed);
        assert!(
            ids.contains(&bot_alias_id) && ids.contains(&state::TEST_BOT_ALIAS_ID.to_string()),
            "case {case:?}: expected the listing to hold the created alias beside the test \
             alias, got {ids:?}"
        );

        // And the delete takes it away: the handle stops resolving, while the test alias the
        // bot was created with is untouched.
        let (status, deleted) = delete_alias(&router, &bot_id, &bot_alias_id, false).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected DeleteBotAlias to answer 202, got {status} {deleted:?}"
        );
        assert_eq!(
            text(&deleted, "botAliasStatus"),
            state::BOT_ALIAS_STATUS_DELETING,
            "case {case:?}: expected the delete to report the status it put the alias in, got \
             {deleted:?}"
        );
        let (status, gone) = describe_alias(&router, &bot_id, &bot_alias_id).await;
        assert_eq!(
            (status, error_code(&gone)),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected the deleted alias to stop resolving, got {status} {gone:?}"
        );
        let (status, test_alias) =
            describe_alias(&router, &bot_id, state::TEST_BOT_ALIAS_ID).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the bot's test alias to survive, got {status} \
             {test_alias:?}"
        );
    }
}

/// A create outside what the service accepts is refused, each with the error class that
/// refusal belongs to: a name the bot already carries conflicts, and a bot or a version
/// nothing backs is a precondition the request does not meet.
#[tokio::test]
async fn creating_an_alias_outside_what_the_service_accepts_is_refused() {
    let router = emulator();
    let (bot_id, bot_version) = bot_with_a_version(&router, "AliasRefusalBot").await;
    let (status, created) =
        create_alias(&router, &bot_id, json!({ "botAliasName": "Prod" })).await;
    assert_eq!(
        status, 202,
        "expected the first create to be accepted, got {status} {created:?}"
    );

    // Rows differ in WHICH precondition the request misses: the name, the bot, and the
    // version the alias would point at.
    let rows: Vec<(&str, String, Value, u16, &str)> = vec![
        (
            "a name the bot already carries",
            bot_id.clone(),
            json!({ "botAliasName": "Prod" }),
            409,
            "ConflictException",
        ),
        (
            "the name of the alias every bot is created with",
            bot_id.clone(),
            json!({ "botAliasName": state::TEST_BOT_ALIAS_NAME }),
            409,
            "ConflictException",
        ),
        (
            "a bot the store does not hold",
            "ZZZZZZZZZZ".to_string(),
            json!({ "botAliasName": "Prod2" }),
            412,
            "PreconditionFailedException",
        ),
        (
            "a version the bot does not hold",
            bot_id.clone(),
            json!({ "botAliasName": "Prod3", "botVersion": "99" }),
            412,
            "PreconditionFailedException",
        ),
    ];

    for (case, bot, body, expected_status, expected_code) in &rows {
        let (status, response) = create_alias(&router, bot, body.clone()).await;
        assert_eq!(
            (status, error_code(&response)),
            (*expected_status, *expected_code),
            "case {case:?}: expected {expected_status} {expected_code}, got {status} \
             {response:?}"
        );
    }

    // The version the first alias points at IS accepted, which is what makes the row above a
    // refusal of a version nothing backs rather than of the member itself.
    let (status, accepted) = create_alias(
        &router,
        &bot_id,
        json!({ "botAliasName": "Staging", "botVersion": bot_version }),
    )
    .await;
    assert_eq!(
        status, 202,
        "a version the bot holds must be accepted, got {status} {accepted:?}"
    );
}

/// A version an ALIAS points at is in use: its delete is refused until the request asks for
/// that check to be skipped, and once it is gone the version stops resolving.
#[tokio::test]
async fn a_version_an_alias_points_at_is_in_use_until_the_check_is_skipped() {
    let router = emulator();
    let (bot_id, bot_version) = bot_with_a_version(&router, "AliasVersionInUseBot").await;
    let version_uri = format!("/bots/{bot_id}/botversions/{bot_version}");
    let (status, created) = create_alias(
        &router,
        &bot_id,
        json!({ "botAliasName": "Prod", "botVersion": bot_version }),
    )
    .await;
    assert_eq!(
        status, 202,
        "expected CreateBotAlias to answer 202, got {status} {created:?}"
    );

    let (status, refused) = call(&router, "DELETE", &version_uri, None).await;
    assert_eq!(
        (status, error_code(&refused)),
        (409, "ConflictException"),
        "expected a version an alias points at to be in use, got {status} {refused:?}"
    );

    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("{version_uri}?skipResourceInUseCheck=true"),
        None,
    )
    .await;
    assert_eq!(
        status, 202,
        "expected the delete to be accepted once the check is skipped, got {status} \
         {deleted:?}"
    );
    assert_eq!(
        (
            text(&deleted, "botId"),
            text(&deleted, "botVersion"),
        ),
        (bot_id.clone(), bot_version.clone()),
        "expected the delete to report the version it removed, got {deleted:?}"
    );
    let (status, gone) = call(&router, "GET", &version_uri, None).await;
    assert_eq!(
        (status, error_code(&gone)),
        (404, "ResourceNotFoundException"),
        "expected the deleted version to stop resolving, got {status} {gone:?}"
    );
    // The locale the version snapshotted goes with it, while the bot's own working copy is
    // untouched: a resource of a version the store no longer holds cannot be read back.
    for (locale_uri, expected, case) in [
        (
            crate::bot_locale::locale_uri(&bot_id, &bot_version, "en_US"),
            404,
            "the locale the deleted version snapshotted",
        ),
        (
            crate::bot_locale::locale_uri(&bot_id, state::DRAFT_BOT_VERSION, "en_US"),
            200,
            "the locale of the bot's working copy",
        ),
    ] {
        let (status, response) = call(&router, "GET", &locale_uri, None).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected}, got {status} {response:?}"
        );
    }
}
