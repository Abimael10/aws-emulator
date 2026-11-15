// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListBotVersions`.
//!
//! Every request goes through the same `MultiServiceRouter::dispatch` the emulator serves
//! on its socket, so the restJson1 routing, the modelled constraints and the error
//! envelopes are exercised exactly as a client sees them. The harness, the bot setup and
//! the locale helpers are the other suites', reused rather than copied.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one case:
//!
//! * a listing of one bot reports its `DRAFT` working copy plus one entry per numbered
//!   version the store holds for it, so the listing of a bot nothing else was done to
//!   already carries the draft, and every entry is read back off the stored record — the
//!   draft off the bot, a numbered version off the snapshot it was created as;
//! * the listing is ordered by the version: the working copy first, then the numbered
//!   versions BY NUMBER, and `Descending` reverses that order;
//! * the listing is paged: a page holds no more than `maxResults` entries and `nextToken`
//!   resumes the same total order without losing or repeating an entry;
//! * the listing is scoped to that bot: a version of another bot is not in it, a bot id
//!   nothing backs lists nothing, and a version does not outlive its bot;
//! * a page size, token or sort the model or this operation does not accept is refused.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lexmodelsv2_lexmodelbuildingservicev2::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::bot_lifecycle::{
    call, create_bot, create_body, emulator, error_code, member, seconds, text,
};
use crate::bot_locale::{locale_body, locales_uri};

/// The locale every bot in this suite carries, so a version has locale data to snapshot.
const LOCALE_ID: &str = "en_US";

/// How long a poll for a status the clock owes may run before the test gives up. Generous
/// on purpose: the test waits for the CONDITION, never for a fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `ListBotVersions`'s URI, which `CreateBotVersion` shares:
/// `POST /bots/{botId}/botversions` lists, `PUT` creates.
fn versions_uri(bot_id: &str) -> String {
    format!("/bots/{bot_id}/botversions")
}

/// `DescribeBotVersion`'s URI, which adds the version to [`versions_uri`].
fn version_uri(bot_id: &str, bot_version: &str) -> String {
    format!("{}/{bot_version}", versions_uri(bot_id))
}

/// List one bot's versions; `body` carries the paging and sorting members.
async fn list(router: &MultiServiceRouter, bot_id: &str, body: Value) -> (u16, Value) {
    call(router, "POST", &versions_uri(bot_id), Some(body)).await
}

/// The summaries of a listing, in the order the response put them.
fn summaries(response: &Value) -> Vec<Value> {
    member(response, "botVersionSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `botVersion`s of a listing, in the order the response put them.
fn listed_versions(response: &Value) -> Vec<String> {
    summaries(response)
        .iter()
        .map(|entry| text(entry, "botVersion"))
        .collect()
}

/// Create a bot carrying one locale on its `DRAFT` version and return its minted id.
async fn draft_bot(router: &MultiServiceRouter, bot_name: &str, extra: Value) -> String {
    let bot_id = create_bot(router, &create_body(bot_name, 300, extra)).await;
    let (status, created) = call(
        router,
        "PUT",
        &locales_uri(&bot_id, state::DRAFT_BOT_VERSION),
        Some(locale_body(LOCALE_ID, 0.4, json!({}))),
    )
    .await;
    assert_eq!(
        status, 202,
        "bot {bot_name}: expected CreateBotLocale to answer 202, got {status} {created:?}"
    );
    bot_id
}

/// Create one version of the bot off its draft and return the number it was created
/// under.
async fn create_version(router: &MultiServiceRouter, bot_id: &str, extra: Value) -> String {
    let mut body = json!({
        "botVersionLocaleSpecification": {
            LOCALE_ID: { "sourceBotVersion": state::DRAFT_BOT_VERSION },
        },
    });
    let object = body.as_object_mut().expect("the body is a JSON object");
    for (key, value) in extra.as_object().expect("extras are a JSON object") {
        object.insert(key.clone(), value.clone());
    }
    let (status, created) = call(router, "PUT", &versions_uri(bot_id), Some(body.clone())).await;
    assert_eq!(
        status, 202,
        "CreateBotVersion {body:?}: expected 202, got {status} {created:?}"
    );
    text(&created, "botVersion")
}

/// The versions a bot carrying `count` numbered versions holds, in the ascending order
/// this listing returns: the working copy first, then the numbers.
fn expected_order(count: u32) -> Vec<String> {
    let mut expected = vec![state::DRAFT_BOT_VERSION.to_string()];
    expected.extend((1..=count).map(|number| number.to_string()));
    expected
}

/// Poll the listing until every entry reports the status the clock owes it, and answer the
/// settled listing. Waits for the CONDITION, with a deadline far longer than the
/// transition needs.
async fn settled_listing(router: &MultiServiceRouter, bot_id: &str, case: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, response) = list(router, bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListBotVersions to answer 200, got {status} {response:?}"
        );
        let entries = summaries(&response);
        let settled = !entries.is_empty()
            && entries
                .iter()
                .all(|entry| text(entry, "botStatus") == state::BOT_STATUS_AVAILABLE);
        if settled {
            return response;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: every version must reach {} once the clock owes it, got {response:?}",
            state::BOT_STATUS_AVAILABLE
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The table the listing assertions run over: rows differ in how many numbered versions
/// the bot holds — none, one, a few, and enough that a number of two digits has to sort
/// after a number of one.
fn version_counts() -> [(&'static str, u32); 4] {
    [
        ("a bot that has only its working copy", 0),
        ("one numbered version", 1),
        ("three numbered versions", 3),
        ("enough versions for a two-digit number", 11),
    ]
}

#[tokio::test]
async fn a_listing_reports_the_draft_and_every_numbered_version_of_the_bot() {
    let router = emulator();

    for (index, (case, count)) in version_counts().into_iter().enumerate() {
        let bot_id = draft_bot(
            &router,
            &format!("VersionListingBot{index}"),
            json!({ "description": "the bot the listing is scoped to" }),
        )
        .await;
        for number in 1..=count {
            let created = create_version(
                &router,
                &bot_id,
                json!({ "description": format!("version {number}") }),
            )
            .await;
            assert_eq!(
                created,
                number.to_string(),
                "case {case:?}: each create is numbered in sequence, got {created}"
            );
        }

        let response = settled_listing(&router, &bot_id, case).await;
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
        assert_eq!(
            listed_versions(&response),
            expected_order(count),
            "case {case:?}: the listing must report the draft and every numbered version, \
             got {response:?}"
        );

        // Every entry is read back off the record that owns it: the draft off the bot,
        // a numbered version off the snapshot `DescribeBotVersion` reports.
        let (_, bot) = call(&router, "GET", &format!("/bots/{bot_id}"), None).await;
        for entry in summaries(&response) {
            let version = text(&entry, "botVersion");
            let described = if version == state::DRAFT_BOT_VERSION {
                bot.clone()
            } else {
                let (status, described) =
                    call(&router, "GET", &version_uri(&bot_id, &version), None).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: version {version} must be readable, got {status} \
                     {described:?}"
                );
                described
            };
            for name in ["botName", "description", "botStatus"] {
                assert_eq!(
                    member(&entry, name),
                    member(&described, name),
                    "case {case:?}: version {version} must report the stored {name}, got \
                     {entry:?}"
                );
            }
            assert!(
                (seconds(&entry, "creationDateTime") - seconds(&described, "creationDateTime"))
                    .abs()
                    < 1e-3,
                "case {case:?}: version {version} must report the stored creationDateTime, \
                 got {entry:?}"
            );
        }

        // Listing again reports the same stored records: the summaries are read off
        // state, not minted per call, so nothing a caller sees moves because it looked.
        let (_, again) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            summaries(&again),
            summaries(&response),
            "case {case:?}: a second listing must report the same stored versions, got \
             {again:?}"
        );
    }
}

#[tokio::test]
async fn the_listing_is_ordered_by_the_version_and_the_order_is_reversed_on_request() {
    let router = emulator();
    let bot_id = draft_bot(&router, "VersionOrderBot", json!({})).await;
    for _ in 0..11 {
        create_version(&router, &bot_id, json!({})).await;
    }
    let ascending = expected_order(11);
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();

    // Rows differ in the sort the request asks for: none at all, and each published
    // member of the modelled `SortOrder` enum on the one modelled sort attribute
    // (`smithy_query(query="shape",shape="BotVersionSortAttribute")`: BotVersion).
    let rows: Vec<(&str, Value, &Vec<String>)> = vec![
        ("no sort at all", json!({}), &ascending),
        (
            "ascending by version",
            json!({ "sortBy": { "attribute": "BotVersion", "order": "Ascending" } }),
            &ascending,
        ),
        (
            "descending by version",
            json!({ "sortBy": { "attribute": "BotVersion", "order": "Descending" } }),
            &descending,
        ),
    ];

    for (case, body, expected) in rows {
        let (status, response) = list(&router, &bot_id, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        assert_eq!(
            &listed_versions(&response),
            expected,
            "case {case:?}: a version number sorts by its NUMBER, got {response:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_is_paged_by_the_page_size_the_request_asks_for() {
    let router = emulator();
    let bot_id = draft_bot(&router, "VersionPagedBot", json!({})).await;
    for _ in 0..3 {
        create_version(&router, &bot_id, json!({})).await;
    }
    let ascending = expected_order(3);
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();

    // Rows differ in the page size — one page per entry, a size that splits the listing,
    // a size larger than it and both published bounds of the modelled range — and in the
    // order the pages are walked in.
    for (order, expected) in [("Ascending", &ascending), ("Descending", &descending)] {
        for page_size in [1_usize, 2, 3, 5, state::MAX_LIST_PAGE] {
            let case = format!("{order} in pages of {page_size}");
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            let mut pages = 0;
            loop {
                let mut body = json!({
                    "maxResults": page_size,
                    "sortBy": { "attribute": "BotVersion", "order": order },
                });
                if let Some(token) = &token {
                    body["nextToken"] = json!(token);
                }
                let (status, response) = list(&router, &bot_id, body).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} {response:?}"
                );
                let page = listed_versions(&response);
                assert!(
                    page.len() <= page_size,
                    "case {case:?}: a page must hold at most {page_size} entries, got {page:?}"
                );
                walked.extend(page);
                pages += 1;
                assert!(
                    pages <= expected.len() + 1,
                    "case {case:?}: paging must terminate, walked {walked:?}"
                );
                match member(&response, "nextToken").as_str() {
                    Some(next) => token = Some(next.to_string()),
                    None => break,
                }
            }
            assert_eq!(
                &walked, expected,
                "case {case:?}: paging must walk the listing once, in order, got {walked:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_listing_holds_no_version_of_another_bot_and_none_outlives_its_bot() {
    let router = emulator();

    // Rows differ in how many versions the bot holds, so no two listings are the same.
    let mut bots: Vec<(&str, String, u32)> = Vec::new();
    for (index, (case, count)) in version_counts().into_iter().enumerate() {
        let bot_id = draft_bot(&router, &format!("VersionScopedBot{index}"), json!({})).await;
        for _ in 0..count {
            create_version(&router, &bot_id, json!({})).await;
        }
        bots.push((case, bot_id, count));
    }

    for (case, bot_id, count) in &bots {
        let (status, response) = list(&router, bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        assert_eq!(
            listed_versions(&response),
            expected_order(*count),
            "case {case:?}: the listing must hold only this bot's versions, got {response:?}"
        );
        for entry in summaries(&response) {
            let (_, bot) = call(&router, "GET", &format!("/bots/{bot_id}"), None).await;
            assert_eq!(
                member(&entry, "botName"),
                member(&bot, "botName"),
                "case {case:?}: every entry must belong to the bot listed, got {entry:?}"
            );
        }
    }

    // A bot id nothing backs holds no version; this operation models no not-found error
    // for it, so the listing is empty rather than refused.
    let (status, response) = list(&router, "ZZZZZZZZZZ", json!({})).await;
    assert_eq!(
        status, 200,
        "a bot id nothing backs must still answer 200, got {status} {response:?}"
    );
    assert_eq!(
        listed_versions(&response),
        Vec::<String>::new(),
        "a bot id nothing backs lists no version, got {response:?}"
    );
    assert_eq!(
        text(&response, "botId"),
        "ZZZZZZZZZZ",
        "the listing must echo the bot it was scoped to, got {response:?}"
    );

    // A version is a version OF a bot, so deleting the bot takes its listing with it —
    // the draft included.
    for (case, bot_id, _) in &bots {
        let (status, deleted) = call(&router, "DELETE", &format!("/bots/{bot_id}"), None).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected DeleteBot to answer 202, got {status} {deleted:?}"
        );
        let (status, response) = list(&router, bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        assert_eq!(
            listed_versions(&response),
            Vec::<String>::new(),
            "case {case:?}: no version may outlive its bot, got {response:?}"
        );
    }
}

#[tokio::test]
async fn the_draft_entry_follows_the_bot_while_a_numbered_entry_keeps_its_snapshot() {
    let router = emulator();
    let bot_id = draft_bot(
        &router,
        "VersionSnapshotBot",
        json!({ "description": "the bot as it was first created" }),
    )
    .await;
    let created = create_version(
        &router,
        &bot_id,
        json!({ "description": "the version's own description" }),
    )
    .await;

    // A version being created reports the transient status until the clock owes the one
    // it is ready for use in.
    let (status, response) = list(&router, &bot_id, json!({})).await;
    assert_eq!(
        status, 200,
        "expected 200, got {status} {response:?}"
    );
    let entry = summaries(&response)
        .into_iter()
        .find(|entry| text(entry, "botVersion") == created)
        .unwrap_or(Value::Null);
    assert_eq!(
        text(&entry, "botStatus"),
        state::BOT_STATUS_VERSIONING,
        "a version just created is still being created, got {entry:?}"
    );
    let settled = settled_listing(&router, &bot_id, "the created version settles").await;
    let entry = summaries(&settled)
        .into_iter()
        .find(|entry| text(entry, "botVersion") == created)
        .unwrap_or(Value::Null);
    assert_eq!(
        text(&entry, "botStatus"),
        state::BOT_STATUS_AVAILABLE,
        "a version past its due instant is ready for use, got {entry:?}"
    );

    // Rows differ in the description the bot is updated to, including none at all: the
    // draft entry reports the bot as it stands, while the numbered version keeps the
    // snapshot it was created as.
    let rows: Vec<(&str, Value)> = vec![
        ("a new description", json!("the bot as it stands now")),
        ("no description at all", Value::Null),
        (
            "a description at its published maximum length",
            json!("d".repeat(2_000)),
        ),
    ];

    for (case, description) in rows {
        let mut update = json!({
            "botName": "VersionSnapshotBot",
            "roleArn": "arn:aws:iam::123456789012:role/lex-demo",
            "dataPrivacy": { "childDirected": false },
            "idleSessionTTLInSeconds": 300,
        });
        if let Some(description) = description.as_str() {
            update["description"] = json!(description);
        }
        let (status, updated) =
            call(&router, "PUT", &format!("/bots/{bot_id}"), Some(update)).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected UpdateBot to answer 202, got {status} {updated:?}"
        );

        let (status, response) = list(&router, &bot_id, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {response:?}"
        );
        let entries = summaries(&response);
        let draft = entries
            .iter()
            .find(|entry| text(entry, "botVersion") == state::DRAFT_BOT_VERSION)
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            member(&draft, "description"),
            description,
            "case {case:?}: the draft entry must report the bot as it stands, got {draft:?}"
        );
        let numbered = entries
            .iter()
            .find(|entry| text(entry, "botVersion") == created)
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            text(&numbered, "description"),
            "the version's own description",
            "case {case:?}: a numbered version keeps the description it was created with, \
             got {numbered:?}"
        );
    }
}

#[tokio::test]
async fn a_listing_request_outside_what_the_model_accepts_is_refused() {
    let router = emulator();
    let bot_id = draft_bot(&router, "VersionRefusedBot", json!({})).await;
    create_version(&router, &bot_id, json!({})).await;

    // Rows differ in which member is outside what the model or this operation accepts:
    // the page size one past each published bound of the modelled range 1..1000, a token
    // this operation never issued, and a sort attribute or order outside the modelled
    // enums (`BotVersionSortAttribute`: BotVersion; `SortOrder`: Ascending | Descending).
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
        (
            "a token that decodes to a cursor of another listing",
            json!({ "nextToken": state::encode_page_token("BotName/ABCDEFGHIJ") }),
        ),
        (
            "a sort attribute the model does not declare",
            json!({ "sortBy": { "attribute": "BotName", "order": "Ascending" } }),
        ),
        (
            "a sort order the model does not declare",
            json!({ "sortBy": { "attribute": "BotVersion", "order": "Rising" } }),
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

    // Each published bound and each modelled enum member is accepted, which is what makes
    // the rows above a refusal of what lies outside them rather than of the bound itself.
    let accepted: Vec<(&str, Value)> = vec![
        (
            "the published minimum page size",
            json!({ "maxResults": 1 }),
        ),
        (
            "the published maximum page size",
            json!({ "maxResults": state::MAX_LIST_PAGE }),
        ),
        (
            "the one modelled attribute, ascending",
            json!({ "sortBy": { "attribute": "BotVersion", "order": "Ascending" } }),
        ),
        (
            "the one modelled attribute, descending",
            json!({ "sortBy": { "attribute": "BotVersion", "order": "Descending" } }),
        ),
    ];
    for (case, body) in accepted {
        let (status, response) = list(&router, &bot_id, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: must be accepted, got {status} {response:?}"
        );
    }
}
