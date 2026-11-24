// SPDX-License-Identifier: CC0-1.0

//! `DescribeAppInstanceBot` and `UpdateAppInstanceBot`, end to end over HTTP — the bot's
//! read-and-write round trip, so what an update persists is asserted through the reader
//! that reports it.
//!
//! The RULE pinned here: `DescribeAppInstanceBot` REPORTS the bot persisted under the
//! `AppInstanceBotArn` it is asked about — that ARN, the `Name`, `Metadata` and
//! `Configuration` the bot was created with, and the instants it was created and last
//! written at — and changes nothing; an `AppInstanceBotArn` this service holds no bot for
//! is an ARN the caller may not read, so it is refused with the modelled 403
//! `ForbiddenException` rather than answered or told that the resource is absent, and
//! that ONE refusal is the answer whatever left the ARN unresolvable, so it discloses
//! nothing about whether such a resource exists.
//!
//! The tables span what the rule generalises over: which optional members a bot carries
//! and at which sizes (absent, empty, the model's length maxima, non-ASCII), the shape of
//! its `Configuration` (the deprecated `RespondsTo` against each `InvokedBy` combination,
//! several locales, with and without a welcome intent), and every way an ARN can fail to
//! resolve to a bot (never minted, under an absent parent, deleted, cascaded away, or
//! naming another resource kind entirely).
//!
//! The second RULE pinned here: `UpdateAppInstanceBot` REPLACES the stored `Name` and
//! `Metadata` of the bot its `AppInstanceBotArn` names with the required pair the request
//! carries, replaces the bot's `Configuration` only when the request carries one — leaving
//! the stored configuration in place when it does not — re-stamps the bot's last-updated
//! instant without moving its created instant, and writes to no other bot; an
//! `AppInstanceBotArn` this service holds no bot for persists nothing and is refused with
//! the modelled client error, whose table of cases is the same one the describe's refusal
//! spans.

use serde_json::{json, Value};

use crate::app_instance_lifecycle::{uri_encode, Emulator};

/// The one sentence this service refuses an ARN it will not resolve with.
const FORBIDDEN_MESSAGE: &str = "You are not allowed to make this request.";

/// A well-formed Amazon Lex V2 bot-alias ARN for `bot_id`/`alias_id`.
///
/// Source: `smithy_query(query="shape",shape="LexConfiguration")` — `LexBotAliasArn` is
/// `@required`, len 15..2048 and
/// `^arn:aws:lex:[a-z]{2}-[a-z]+-\d{1}:\d{12}:bot-alias/[A-Z0-9]{10}/[A-Z0-9]{10}$`.
fn lex_bot_alias_arn(bot_id: &str, alias_id: &str) -> String {
    format!("arn:aws:lex:us-west-2:123456789012:bot-alias/{bot_id}/{alias_id}")
}

/// A `Configuration` driven by the deprecated `RespondsTo` member.
///
/// Source: `smithy_query(query="shape",shape="RespondsTo")` — the enum's only value is
/// `STANDARD_MESSAGES`. `LocaleId` is `@required` and modelled as a free string the Lex V2
/// service resolves, so a locale is a value this table varies rather than a catalog.
fn responds_to_configuration(bot_id: &str, locale_id: &str) -> Value {
    json!({
        "Lex": {
            "LexBotAliasArn": lex_bot_alias_arn(bot_id, "ALIASAAAAA"),
            "LocaleId": locale_id,
            "RespondsTo": "STANDARD_MESSAGES",
        }
    })
}

/// A `Configuration` driven by `InvokedBy`, the member that replaces `RespondsTo`.
///
/// Source: `smithy_query(query="shape",shape="InvokedBy")` — both members are
/// `@required`; `StandardMessages` is one of `AUTO`/`ALL`/`MENTIONS`/`NONE` and
/// `TargetedMessages` one of `ALL`/`NONE`. `WelcomeIntent` is optional, len 1..100,
/// pattern `^([A-Za-z]_?)+$`.
fn invoked_by_configuration(
    bot_id: &str,
    locale_id: &str,
    standard: &str,
    targeted: &str,
    welcome_intent: Option<&str>,
) -> Value {
    let mut lex = json!({
        "LexBotAliasArn": lex_bot_alias_arn(bot_id, "ALIASBBBBB"),
        "LocaleId": locale_id,
        "InvokedBy": { "StandardMessages": standard, "TargetedMessages": targeted },
    });
    if let Some(intent) = welcome_intent {
        lex["WelcomeIntent"] = json!(intent);
    }
    json!({ "Lex": lex })
}

/// `POST /app-instance-bots`, with only the members the row carries, returning the ARN
/// the create minted.
async fn create_bot(
    emulator: &Emulator,
    app_instance_arn: &str,
    token: &str,
    name: Option<&str>,
    metadata: Option<&str>,
    configuration: &Value,
) -> String {
    let mut body = json!({
        "AppInstanceArn": app_instance_arn,
        "ClientRequestToken": token,
        "Configuration": configuration,
    });
    if let Some(name) = name {
        body["Name"] = json!(name);
    }
    if let Some(metadata) = metadata {
        body["Metadata"] = json!(metadata);
    }
    let (status, body) = emulator.call("POST", "/app-instance-bots", Some(body)).await;
    assert_eq!(
        status, 201,
        "case {token:?}: expected CreateAppInstanceBot to answer 201, got {status} {body:?}"
    );
    body["AppInstanceBotArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {token:?}: CreateAppInstanceBot returned no ARN: {body:?}"))
        .to_string()
}

/// `GET /app-instance-bots/{AppInstanceBotArn}`.
async fn describe_bot(emulator: &Emulator, app_instance_bot_arn: &str) -> (u16, Value) {
    emulator
        .call(
            "GET",
            &format!(
                "/app-instance-bots/{}",
                uri_encode(app_instance_bot_arn)
            ),
            None,
        )
        .await
}

/// `DELETE /app-instance-bots/{AppInstanceBotArn}`, failing the test if it was refused.
async fn delete_bot(emulator: &Emulator, app_instance_bot_arn: &str) {
    let (status, body) = emulator
        .call(
            "DELETE",
            &format!(
                "/app-instance-bots/{}",
                uri_encode(app_instance_bot_arn)
            ),
            None,
        )
        .await;
    assert_eq!(
        status, 204,
        "expected DeleteAppInstanceBot to answer 204, got {status} {body:?}"
    );
}

/// `DELETE /app-instances/{AppInstanceArn}`, failing the test if it was refused.
async fn delete_app_instance(emulator: &Emulator, app_instance_arn: &str) {
    let (status, body) = emulator
        .call(
            "DELETE",
            &format!("/app-instances/{}", uri_encode(app_instance_arn)),
            None,
        )
        .await;
    assert_eq!(
        status, 204,
        "expected DeleteAppInstance to answer 204, got {status} {body:?}"
    );
}

/// `arn` with its REGION segment (the 4th) replaced, so a row can name the same resource
/// in another region without hardcoding the region the emulator was built in.
fn in_another_region(arn: &str) -> String {
    let mut segments: Vec<&str> = arn.splitn(6, ':').collect();
    assert_eq!(segments.len(), 6, "expected a six-segment ARN, got {arn:?}");
    let other = if segments[3] == "eu-west-1" {
        "us-east-2"
    } else {
        "eu-west-1"
    };
    segments[3] = other;
    segments.join(":")
}

/// `arn` with its ACCOUNT segment (the 5th) replaced, so a row can name the same
/// resource in another account without hardcoding the emulator's account id.
fn in_another_account(arn: &str) -> String {
    let mut segments: Vec<&str> = arn.splitn(6, ':').collect();
    assert_eq!(segments.len(), 6, "expected a six-segment ARN, got {arn:?}");
    let other = if segments[4] == "210987654321" {
        "123456789012"
    } else {
        "210987654321"
    };
    segments[4] = other;
    segments.join(":")
}

/// The reported member, or `Value::Null` when the response omits it.
fn member(described: &Value, name: &str) -> Value {
    described["AppInstanceBot"]
        .get(name)
        .cloned()
        .unwrap_or(Value::Null)
}

/// RULE: a describe reports the bot PERSISTED under the ARN it names — that ARN and the
/// `Name`, `Metadata` and `Configuration` the create stored, member for member, with an
/// absent optional member still absent — plus the instants it was created and last
/// written at.
#[tokio::test]
async fn a_describe_reports_the_bot_that_was_created() {
    let emulator = Emulator::new();
    let instance = emulator.app_instance("described-bots", "tok-describe").await;

    // Rows differ in which optional members the bot carries and at which size (absent,
    // empty, the modelled maxima of `Name` len 0..256 and `Metadata` len 0..1024,
    // non-ASCII), and in the shape of the configuration: the deprecated `RespondsTo`
    // against each documented `InvokedBy` combination, a different locale each time, and
    // a welcome intent present or not.
    let max_name = "n".repeat(256);
    let max_metadata = "m".repeat(1024);
    let cases: Vec<(&str, Option<&str>, Option<&str>, Value)> = vec![
        (
            "no Name, no Metadata, RespondsTo",
            None,
            None,
            responds_to_configuration("BOTIDAAAAA", "en_US"),
        ),
        (
            "empty Name and Metadata",
            Some(""),
            Some(""),
            responds_to_configuration("BOTIDBBBBB", "de_DE"),
        ),
        (
            "Name at the modelled maximum",
            Some(max_name.as_str()),
            None,
            invoked_by_configuration("BOTIDCCCCC", "ja_JP", "ALL", "NONE", None),
        ),
        (
            "Metadata at the modelled maximum",
            Some("sized"),
            Some(max_metadata.as_str()),
            invoked_by_configuration("BOTIDDDDDD", "fr_FR", "AUTO", "ALL", Some("Welcome")),
        ),
        (
            "non-ASCII Name and Metadata",
            Some("ボット ⚙"),
            Some("メタデータ — ©"),
            invoked_by_configuration(
                "BOTIDEEEEE",
                "es_US",
                "MENTIONS",
                "NONE",
                Some("Greet_User"),
            ),
        ),
        (
            "a bot nothing triggers",
            Some("silent"),
            Some("triggers nothing"),
            invoked_by_configuration("BOTIDFFFFF", "it_IT", "NONE", "NONE", None),
        ),
    ];

    let mut minted: Vec<(&str, String)> = Vec::new();
    for (index, (label, name, metadata, configuration)) in cases.iter().enumerate() {
        let arn = create_bot(
            &emulator,
            &instance,
            &format!("tok-described-{index}"),
            *name,
            *metadata,
            configuration,
        )
        .await;

        let (status, body) = describe_bot(&emulator, &arn).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected DescribeAppInstanceBot to answer 200, got {status} {body:?}"
        );
        assert_eq!(
            member(&body, "AppInstanceBotArn").as_str(),
            Some(arn.as_str()),
            "case {label:?}: expected the described ARN {arn:?}, got {body:?}"
        );
        assert_eq!(
            member(&body, "Name"),
            name.map(|name| json!(name)).unwrap_or(Value::Null),
            "case {label:?}: expected the Name the create stored ({name:?}), got {body:?}"
        );
        assert_eq!(
            member(&body, "Metadata"),
            metadata.map(|value| json!(value)).unwrap_or(Value::Null),
            "case {label:?}: expected the Metadata the create stored ({metadata:?}), got {body:?}"
        );
        assert_eq!(
            &member(&body, "Configuration"),
            configuration,
            "case {label:?}: expected the Configuration the create stored, got {body:?}"
        );

        let created = member(&body, "CreatedTimestamp").as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: expected a numeric CreatedTimestamp, got {body:?}")
        });
        let updated = member(&body, "LastUpdatedTimestamp")
            .as_f64()
            .unwrap_or_else(|| {
                panic!("case {label:?}: expected a numeric LastUpdatedTimestamp, got {body:?}")
            });
        assert!(
            created > 0.0,
            "case {label:?}: expected a CreatedTimestamp after the epoch, got {created}"
        );
        assert!(
            updated >= created,
            "case {label:?}: expected LastUpdatedTimestamp {updated} at or after CreatedTimestamp {created}"
        );

        minted.push((label, arn));
    }

    // Every ARN still describes ITS OWN bot once all of them exist: a describe resolves
    // the ARN it was given rather than the last bot written.
    for (index, (label, arn)) in minted.iter().enumerate() {
        let (_, _, expected_metadata, expected_configuration) = &cases[index];
        let (status, body) = describe_bot(&emulator, arn).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the describe to still answer 200, got {status} {body:?}"
        );
        assert_eq!(
            member(&body, "AppInstanceBotArn").as_str(),
            Some(arn.as_str()),
            "case {label:?}: expected the describe to report {arn:?}, got {body:?}"
        );
        assert_eq!(
            member(&body, "Metadata"),
            expected_metadata
                .map(|value| json!(value))
                .unwrap_or(Value::Null),
            "case {label:?}: expected its own Metadata, got {body:?}"
        );
        assert_eq!(
            &member(&body, "Configuration"),
            expected_configuration,
            "case {label:?}: expected its own Configuration, got {body:?}"
        );
    }
}

/// RULE: a describe REPORTS, so repeating it answers exactly the same bot and leaves
/// every later caller — another describe, the listing — seeing what it saw before.
#[tokio::test]
async fn a_describe_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let instance = emulator.app_instance("stable-bots", "tok-stable").await;

    // Rows differ in how many times the same bot is read before the state is checked.
    for (label, reads) in [("read once", 1_usize), ("read twice", 2), ("read ten times", 10)] {
        let arn = create_bot(
            &emulator,
            &instance,
            &format!("tok-stable-{reads}"),
            Some(label),
            Some("unchanged by reading"),
            &responds_to_configuration("BOTIDSTABL", "en_GB"),
        )
        .await;

        let (status, first) = describe_bot(&emulator, &arn).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the first describe to answer 200, got {status} {first:?}"
        );
        for read in 1..reads {
            let (status, again) = describe_bot(&emulator, &arn).await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected read {read} to answer 200, got {status} {again:?}"
            );
            assert_eq!(
                again, first,
                "case {label:?}: expected read {read} to answer exactly what the first did, got {again:?}"
            );
        }

        // The bot is still listed, and still described, after the reads.
        let (status, listed) = emulator
            .call(
                "GET",
                &format!(
                    "/app-instance-bots?app-instance-arn={}",
                    uri_encode(&instance)
                ),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected ListAppInstanceBots to answer 200, got {status} {listed:?}"
        );
        let holds = listed["AppInstanceBots"]
            .as_array()
            .unwrap_or_else(|| panic!("case {label:?}: expected a bot list, got {listed:?}"))
            .iter()
            .any(|bot| bot["AppInstanceBotArn"].as_str() == Some(arn.as_str()));
        assert!(
            holds,
            "case {label:?}: expected the read bot {arn:?} to still be listed, got {listed:?}"
        );
    }
}

/// RULE: an `AppInstanceBotArn` this service holds no bot for is refused with the modelled
/// 403 `ForbiddenException` — `Code` `Forbidden` and the one forbidden-request sentence —
/// reporting no bot and disclosing nothing about whether such a resource exists.
#[tokio::test]
async fn an_unresolvable_bot_arn_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.app_instance("refused-bots", "tok-refused").await;
    let user = emulator.user(&instance, "refused-user", "tok-refused-user").await;
    let kept = create_bot(
        &emulator,
        &instance,
        "tok-refused-kept",
        Some("kept"),
        None,
        &responds_to_configuration("BOTIDKEPTA", "en_US"),
    )
    .await;

    // A bot created and then deleted, and a bot whose parent instance is deleted under
    // it: both ARNs were once real, so they test the refusal and not just a malformed id.
    let deleted = create_bot(
        &emulator,
        &instance,
        "tok-refused-deleted",
        Some("deleted"),
        None,
        &responds_to_configuration("BOTIDDELET", "en_US"),
    )
    .await;
    delete_bot(&emulator, &deleted).await;

    let doomed_instance = emulator.app_instance("doomed", "tok-refused-doomed").await;
    let cascaded = create_bot(
        &emulator,
        &doomed_instance,
        "tok-refused-cascaded",
        Some("cascaded"),
        None,
        &responds_to_configuration("BOTIDCASCA", "en_US"),
    )
    .await;
    delete_app_instance(&emulator, &doomed_instance).await;

    // Rows span every way a well-formed ChimeArn can fail to resolve to a bot.
    for (label, arn) in [
        ("an ARN under a live parent that was never minted", format!("{instance}/bot/never-minted")),
        ("a bot ARN under an AppInstance that does not exist", format!("{instance}x/bot/absent-parent")),
        ("a bot that was deleted", deleted.clone()),
        ("a bot whose parent AppInstance was deleted", cascaded.clone()),
        ("the parent AppInstance's own ARN", instance.clone()),
        ("an AppInstanceUser ARN", user.clone()),
        ("the same bot ARN in another account", in_another_account(&kept)),
    ] {
        let (status, body) = describe_bot(&emulator, &arn).await;
        assert_eq!(
            status, 403,
            "case {label:?}: expected {arn:?} to be refused with 403, got {status} {body:?}"
        );
        assert_eq!(
            body["Code"].as_str(),
            Some("Forbidden"),
            "case {label:?}: expected the refusal to carry the Forbidden code, got {body:?}"
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(FORBIDDEN_MESSAGE),
            "case {label:?}: expected the forbidden-request sentence, got {body:?}"
        );
        assert!(
            body.get("AppInstanceBot").is_none(),
            "case {label:?}: expected the refusal to report no bot, got {body:?}"
        );
    }

    // None of the refusals disturbed the bot that does exist.
    let (status, body) = describe_bot(&emulator, &kept).await;
    assert_eq!(
        status, 200,
        "expected the surviving bot to still be described, got {status} {body:?}"
    );
    assert_eq!(
        member(&body, "Name").as_str(),
        Some("kept"),
        "expected the surviving bot to report the Name it was created with, got {body:?}"
    );
}

/// RULE: the refusal of a `DescribeAppInstanceBot` addressed to an ARN this service holds
/// no bot for is ONE answer — the modelled 403 `ForbiddenException`, `Code` `Forbidden`
/// and the fixed forbidden-request sentence with nothing interpolated into it — whatever
/// left that ARN unresolvable, so the response body is identical across every class of
/// unresolvable ARN, never reports absence as a 404 or a 400, and never echoes the ARN it
/// was asked about. Resolving the addressed ARN is an authorization decision here, so
/// telling a caller apart "no such bot" from "a bot you may not read" is exactly what the
/// answer must not do.
#[tokio::test]
async fn one_refusal_answers_every_unresolvable_bot_arn_and_discloses_nothing() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("undisclosed-bots", "tok-undisclosed")
        .await;
    let user = emulator
        .user(&instance, "undisclosed-user", "tok-undisclosed-user")
        .await;
    let live = create_bot(
        &emulator,
        &instance,
        "tok-undisclosed-live",
        Some("live"),
        Some("still here"),
        &responds_to_configuration("BOTIDLIVEA", "en_US"),
    )
    .await;

    let deleted = create_bot(
        &emulator,
        &instance,
        "tok-undisclosed-deleted",
        Some("deleted"),
        None,
        &responds_to_configuration("BOTIDGONEA", "en_US"),
    )
    .await;
    delete_bot(&emulator, &deleted).await;

    let doomed_instance = emulator
        .app_instance("undisclosed-doomed", "tok-undisclosed-doomed")
        .await;
    let cascaded = create_bot(
        &emulator,
        &doomed_instance,
        "tok-undisclosed-cascaded",
        Some("cascaded"),
        None,
        &responds_to_configuration("BOTIDCASCB", "en_US"),
    )
    .await;
    delete_app_instance(&emulator, &doomed_instance).await;

    // Rows span the classes the rule generalises over: WHY the ARN resolves to no bot
    // (never minted, minted and deleted, cascaded away with its parent, belonging to
    // another account or region, or naming a live resource that is not a bot — a user,
    // the parent instance itself, or a resource of another service entirely), and the
    // SHAPE of the ARN (one segment longer than a bot ARN, one character off a live
    // bot's, a resource path of its own). Every `ChimeArn` here satisfies the modelled
    // pattern, so what is under test is the refusal and not input validation.
    //
    // Source for the ARN shape: `smithy_query(query="shape",shape="ChimeArn")` — len
    // 5..1600 and `^arn:[a-z0-9-\.]{1,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[^/].{0,1023}$`.
    let one_character_off = {
        let mut flipped = live.clone();
        flipped.pop();
        format!("{flipped}z")
    };
    let rows: Vec<(&str, String)> = vec![
        (
            "a bot never minted under a live AppInstance",
            format!("{instance}/bot/never-minted"),
        ),
        (
            "a bot under an AppInstance that never existed",
            format!("{instance}-absent/bot/orphan"),
        ),
        ("a bot that was deleted", deleted.clone()),
        (
            "a bot whose parent AppInstance was deleted",
            cascaded.clone(),
        ),
        (
            "a live bot's ARN in another account",
            in_another_account(&live),
        ),
        (
            "a live bot's ARN in another region",
            in_another_region(&live),
        ),
        ("a live AppInstanceUser's ARN", user.clone()),
        ("the live parent AppInstance's own ARN", instance.clone()),
        (
            "an ARN one character off a live bot's",
            one_character_off.clone(),
        ),
        (
            "a bot ARN with a sub-resource path of its own",
            format!("{live}/channel/extra"),
        ),
        (
            "an ARN of another AWS service entirely",
            "arn:aws:s3:us-west-2:123456789012:bucket-that-is-not-a-bot".to_string(),
        ),
    ];

    let mut canonical: Option<(&str, Value)> = None;
    for (label, arn) in &rows {
        let (status, body) = describe_bot(&emulator, arn).await;
        assert_eq!(
            status, 403,
            "case {label:?}: expected {arn:?} to be refused with the modelled 403, got {status} {body:?}"
        );
        assert_eq!(
            body["Code"].as_str(),
            Some("Forbidden"),
            "case {label:?}: expected the Forbidden code, got {body:?}"
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(FORBIDDEN_MESSAGE),
            "case {label:?}: expected the fixed forbidden-request sentence, got {body:?}"
        );
        assert!(
            body.get("AppInstanceBot").is_none(),
            "case {label:?}: expected the refusal to report no bot, got {body:?}"
        );
        // Nothing of the request is interpolated into the refusal, so the answer cannot
        // be read as a statement about the ARN that was asked about.
        let rendered = body.to_string();
        assert!(
            !rendered.contains(arn.as_str()),
            "case {label:?}: expected the refusal not to echo {arn:?}, got {rendered}"
        );
        match &canonical {
            None => canonical = Some((label, body)),
            Some((first_label, first_body)) => assert_eq!(
                &body, first_body,
                "case {label:?}: expected the same refusal as case {first_label:?} ({first_body:?}), got {body:?}"
            ),
        }
    }

    // The refusals are not a blanket one: the bot that does exist is still reported, with
    // everything it was created with, after every row above.
    let (status, body) = describe_bot(&emulator, &live).await;
    assert_eq!(
        status, 200,
        "expected the live bot to still be described, got {status} {body:?}"
    );
    assert_eq!(
        member(&body, "AppInstanceBotArn").as_str(),
        Some(live.as_str()),
        "expected the live bot to report its own ARN, got {body:?}"
    );
    assert_eq!(
        member(&body, "Metadata").as_str(),
        Some("still here"),
        "expected the live bot to report the Metadata it was created with, got {body:?}"
    );
}

/// `PUT /app-instance-bots/{AppInstanceBotArn}` with the `Name`/`Metadata` pair the model
/// requires, and a `Configuration` only when the row carries one.
///
/// Source: `smithy_query(query="operation",operation="UpdateAppInstanceBot")` — `PUT
/// /app-instance-bots/{AppInstanceBotArn}`, `Name` (len 0..256) and `Metadata` (len
/// 0..1024) `@required`, `Configuration` optional, answering 200 with the bot's ARN.
async fn update_bot(
    emulator: &Emulator,
    app_instance_bot_arn: &str,
    name: &str,
    metadata: &str,
    configuration: Option<&Value>,
) -> (u16, Value) {
    let mut body = json!({ "Name": name, "Metadata": metadata });
    if let Some(configuration) = configuration {
        body["Configuration"] = configuration.clone();
    }
    emulator
        .call(
            "PUT",
            &format!("/app-instance-bots/{}", uri_encode(app_instance_bot_arn)),
            Some(body),
        )
        .await
}

/// The summary `ListAppInstanceBots` reports for `bot_arn` under `app_instance_arn`, or
/// `Value::Null` when the listing holds no row for it.
async fn listed_bot(emulator: &Emulator, app_instance_arn: &str, bot_arn: &str) -> Value {
    let (status, body) = emulator
        .call(
            "GET",
            &format!(
                "/app-instance-bots?app-instance-arn={}",
                uri_encode(app_instance_arn)
            ),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "expected ListAppInstanceBots to answer 200, got {status} {body:?}"
    );
    body["AppInstanceBots"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a bot list, got {body:?}"))
        .iter()
        .find(|bot| bot["AppInstanceBotArn"].as_str() == Some(bot_arn))
        .cloned()
        .unwrap_or(Value::Null)
}

/// RULE: an update REPLACES the stored `Name`/`Metadata` pair of the bot its ARN names,
/// replaces the `Configuration` only when the request carries one and otherwise leaves the
/// stored configuration in place, re-stamps the last-updated instant without moving the
/// created instant, answers with that bot's ARN, and writes to no other bot.
#[tokio::test]
async fn an_update_replaces_the_stored_pair_and_only_the_configuration_it_is_given() {
    let emulator = Emulator::new();
    let instance = emulator.app_instance("updated-bots", "tok-update").await;

    let created_configuration = responds_to_configuration("BOTIDUPDAT", "en_US");
    let target = create_bot(
        &emulator,
        &instance,
        "tok-update-target",
        Some("created-name"),
        Some("created-metadata"),
        &created_configuration,
    )
    .await;
    // A second bot under the same parent: every row below has to leave it exactly as its
    // create left it, so an update writes to the ONE bot its ARN names.
    let bystander = create_bot(
        &emulator,
        &instance,
        "tok-update-bystander",
        Some("bystander"),
        Some("untouched"),
        &invoked_by_configuration("BOTIDBYSTA", "en_GB", "ALL", "ALL", Some("Hello")),
    )
    .await;
    let (status, bystander_before) = describe_bot(&emulator, &bystander).await;
    assert_eq!(
        status, 200,
        "expected the bystander bot to describe, got {status} {bystander_before:?}"
    );

    let (status, before) = describe_bot(&emulator, &target).await;
    assert_eq!(
        status, 200,
        "expected the fresh bot to describe, got {status} {before:?}"
    );
    let created = member(&before, "CreatedTimestamp")
        .as_f64()
        .unwrap_or_else(|| panic!("expected a numeric CreatedTimestamp, got {before:?}"));
    let mut previous_update = member(&before, "LastUpdatedTimestamp")
        .as_f64()
        .unwrap_or_else(|| panic!("expected a numeric LastUpdatedTimestamp, got {before:?}"));

    // Rows differ along what the rule generalises over: the value and size of the pair
    // being written (ordinary text, the empty values the model's `min 0` lengths allow,
    // each member at its modelled maximum, non-ASCII text), and whether the request
    // carries a `Configuration` at all — one driven by the deprecated `RespondsTo`, one by
    // `InvokedBy`, and rows carrying none, which must keep whatever the bot already holds.
    //
    // Source: `smithy_query(query="operation",operation="UpdateAppInstanceBot")` — `Name`
    // is `ResourceName` (len 0..256) and `Metadata` is len 0..1024.
    let max_name = "n".repeat(256);
    let max_metadata = "m".repeat(1024);
    let rows: Vec<(&str, String, String, Option<Value>)> = vec![
        (
            "an ordinary rename, no Configuration",
            "renamed".to_string(),
            "metadata-1".to_string(),
            None,
        ),
        (
            "the empty Name and Metadata the model allows",
            String::new(),
            String::new(),
            None,
        ),
        (
            "Name at the modelled maximum",
            max_name.clone(),
            "metadata-2".to_string(),
            None,
        ),
        (
            "Metadata at the modelled maximum",
            "sized".to_string(),
            max_metadata.clone(),
            None,
        ),
        (
            "non-ASCII Name and Metadata",
            "ボット ⚙".to_string(),
            "メタデータ — ©".to_string(),
            None,
        ),
        (
            "a Configuration driven by InvokedBy",
            "reconfigured".to_string(),
            "metadata-3".to_string(),
            Some(invoked_by_configuration(
                "BOTIDNEWCF",
                "de_DE",
                "MENTIONS",
                "ALL",
                Some("Greet_User"),
            )),
        ),
        (
            "a Configuration driven by RespondsTo",
            "reconfigured-again".to_string(),
            String::new(),
            Some(responds_to_configuration("BOTIDOLDCF", "ja_JP")),
        ),
        (
            "no Configuration after one was replaced",
            "pair-only".to_string(),
            "metadata-4".to_string(),
            None,
        ),
    ];

    let mut expected_configuration = created_configuration.clone();
    for (label, name, metadata, configuration) in &rows {
        let (status, answered) =
            update_bot(&emulator, &target, name, metadata, configuration.as_ref()).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected UpdateAppInstanceBot to answer 200, got {status} \
             {answered:?}"
        );
        assert_eq!(
            answered["AppInstanceBotArn"].as_str(),
            Some(target.as_str()),
            "case {label:?}: expected the update to answer with {target:?}, got {answered:?}"
        );
        if let Some(configuration) = configuration {
            expected_configuration = configuration.clone();
        }

        let (status, after) = describe_bot(&emulator, &target).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the updated bot to describe, got {status} {after:?}"
        );
        assert_eq!(
            member(&after, "Name").as_str(),
            Some(name.as_str()),
            "case {label:?}: expected Name {name:?} after the update, got {after:?}"
        );
        assert_eq!(
            member(&after, "Metadata").as_str(),
            Some(metadata.as_str()),
            "case {label:?}: expected Metadata {metadata:?} after the update, got {after:?}"
        );
        assert_eq!(
            member(&after, "Configuration"),
            expected_configuration,
            "case {label:?}: expected the Configuration {expected_configuration:?}, got \
             {after:?}"
        );
        assert_eq!(
            member(&after, "CreatedTimestamp").as_f64(),
            Some(created),
            "case {label:?}: an update must not move the created instant {created}, got \
             {after:?}"
        );
        let updated = member(&after, "LastUpdatedTimestamp").as_f64();
        assert!(
            updated.is_some_and(|at| at >= created && at >= previous_update),
            "case {label:?}: expected a last-updated instant at or after both the created \
             instant {created} and the previous stamp {previous_update}, got {updated:?}"
        );
        previous_update = updated.unwrap_or(previous_update);
    }

    // The listing reports the pair the LAST update wrote, so the update persisted to the
    // one store every reader of this bot reads.
    let (_, final_name, final_metadata, _) = rows.last().expect("a non-empty table");
    let summary = listed_bot(&emulator, &instance, &target).await;
    assert_eq!(
        summary["Name"].as_str(),
        Some(final_name.as_str()),
        "expected the listing to report the last Name written, got {summary:?}"
    );
    assert_eq!(
        summary["Metadata"].as_str(),
        Some(final_metadata.as_str()),
        "expected the listing to report the last Metadata written, got {summary:?}"
    );

    // Not one of those updates touched the other bot of the same AppInstance.
    let (status, bystander_after) = describe_bot(&emulator, &bystander).await;
    assert_eq!(
        (status, &bystander_after),
        (200, &bystander_before),
        "expected the bystander bot to be exactly what its create left, got \
         {bystander_after:?}"
    );
}

/// RULE: an `UpdateAppInstanceBot` addressed to an `AppInstanceBotArn` this service holds
/// no bot for persists nothing and is refused with the modelled client error — the
/// permanent 403 `ForbiddenException` once the bot's own `AppInstance` has been deleted,
/// and otherwise the modelled 400 `BadRequestException` carrying the `NotFound` code, this
/// operation declaring no 404 shape.
#[tokio::test]
async fn updating_an_arn_this_service_holds_no_bot_for_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let instance = emulator
        .app_instance("unwritable-bots", "tok-update-absent")
        .await;
    let user = emulator
        .user(&instance, "update-absent-user", "tok-update-absent-user")
        .await;
    let kept = create_bot(
        &emulator,
        &instance,
        "tok-update-kept",
        Some("kept"),
        Some("kept-metadata"),
        &responds_to_configuration("BOTIDKEPTU", "en_US"),
    )
    .await;
    let (status, kept_before) = describe_bot(&emulator, &kept).await;
    assert_eq!(
        status, 200,
        "expected the kept bot to describe, got {status} {kept_before:?}"
    );

    let deleted = create_bot(
        &emulator,
        &instance,
        "tok-update-deleted",
        Some("deleted"),
        None,
        &responds_to_configuration("BOTIDDELEU", "en_US"),
    )
    .await;
    delete_bot(&emulator, &deleted).await;

    let doomed_instance = emulator
        .app_instance("update-doomed", "tok-update-doomed")
        .await;
    let cascaded = create_bot(
        &emulator,
        &doomed_instance,
        "tok-update-cascaded",
        Some("cascaded"),
        None,
        &responds_to_configuration("BOTIDCASCU", "en_US"),
    )
    .await;
    delete_app_instance(&emulator, &doomed_instance).await;

    // Rows span every way a well-formed `ChimeArn` can fail to name a bot this service
    // holds, split by which refusal class the ARN falls in: one at or under a DELETED
    // `AppInstance` is a request the caller may no longer make, every other is reported
    // as a resource that was not found.
    let rows: Vec<(&str, String, u16, &str)> = vec![
        (
            "a bot never minted under a live AppInstance",
            format!("{instance}/bot/never-minted"),
            400,
            "NotFound",
        ),
        (
            "a bot under an AppInstance that never existed",
            format!("{instance}-absent/bot/orphan"),
            400,
            "NotFound",
        ),
        ("a bot that was deleted", deleted.clone(), 400, "NotFound"),
        (
            "a live AppInstanceUser's ARN in the bot space",
            user.clone(),
            400,
            "NotFound",
        ),
        (
            "the live parent AppInstance's own ARN",
            instance.clone(),
            400,
            "NotFound",
        ),
        (
            "a live bot's ARN in another account",
            in_another_account(&kept),
            400,
            "NotFound",
        ),
        (
            "a live bot's ARN in another region",
            in_another_region(&kept),
            400,
            "NotFound",
        ),
        (
            "a bot whose parent AppInstance was deleted",
            cascaded.clone(),
            403,
            "Forbidden",
        ),
        (
            "the deleted AppInstance's own ARN",
            doomed_instance.clone(),
            403,
            "Forbidden",
        ),
    ];

    for (label, arn, expected_status, expected_code) in &rows {
        let (status, body) = update_bot(&emulator, arn, "written", "written", None).await;
        assert_eq!(
            status, *expected_status,
            "case {label:?}: expected {arn:?} to be refused with {expected_status}, got \
             {status} {body:?}"
        );
        assert_eq!(
            body["Code"].as_str(),
            Some(*expected_code),
            "case {label:?}: expected the {expected_code:?} code, got {body:?}"
        );
        assert!(
            body.get("AppInstanceBotArn").is_none(),
            "case {label:?}: expected the refusal to report no ARN, got {body:?}"
        );
        if *expected_status == 403 {
            assert_eq!(
                body["Message"].as_str(),
                Some(FORBIDDEN_MESSAGE),
                "case {label:?}: expected the forbidden-request sentence, got {body:?}"
            );
        }
    }

    // None of the refused writes landed anywhere: the one live bot is byte for byte what
    // its create left, last-updated instant included, and the deleted bot is still gone.
    let (status, kept_after) = describe_bot(&emulator, &kept).await;
    assert_eq!(
        (status, &kept_after),
        (200, &kept_before),
        "expected the live bot to be untouched by the refusals, got {kept_after:?}"
    );
    let (status, body) = describe_bot(&emulator, &deleted).await;
    assert_eq!(
        status, 403,
        "expected the deleted bot to stay unresolvable after the refused update, got \
         {status} {body:?}"
    );
}
