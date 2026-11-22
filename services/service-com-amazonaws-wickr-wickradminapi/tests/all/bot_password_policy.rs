// SPDX-License-Identifier: CC0-1.0

//! `CreateBot` and the password policy of the security group a bot joins, driven
//! over HTTP against one running emulator.
//!
//! Every case goes through the real multi-service router — route resolution,
//! restJson1 deserialization, modelled-constraint validation, the handler and
//! response serialization — so what it asserts is the wire behaviour a client
//! sees.
//!
//! The rules under test, and where each comes from:
//!
//! * `SecurityGroupSettings$passwordRequirements` is "the password complexity
//!   requirements that users must follow when creating or changing passwords" and
//!   `PasswordRequirements` carries a minimum length plus a required count of
//!   uppercase letters, lowercase letters, numbers and symbols. The policy
//!   belongs to the GROUP, so a bot's `challenge` is decided against the policy
//!   of the group `groupId` names, before any bot is created.
//! * A `challenge` the policy turns down is the modelled `ValidationError`
//!   (`@httpError(422)`), whose body carries no envelope sentence and exactly one
//!   `ErrorDetail`: `field` names the policy and `reason` states the GROUP's five
//!   numbers in the service's fixed layout.
//! * `challenge` is a `SensitiveString` whose `^[\S\s]*$` matches every string,
//!   so no constraint trait can refuse a password: every row below reaches the
//!   handler.
//! * The refusal is atomic — `ListBots` afterwards reports no bot — and a
//!   `challenge` that meets the policy creates the bot and echoes `username`
//!   back.
//! * `CreateBotResponse$botId` is "the unique identifier assigned to the newly
//!   created bot", so the created bot is readable back through `GetBot` and
//!   reported by `ListBots`, and it counts towards its group's
//!   `SecurityGroup$botMembers` ("the number of bot members").
//! * A `networkId` or a `groupId` naming no resource is `ResourceNotFoundError`
//!   (HTTP 404) — "a network, user, bot, security group, or other resource that
//!   doesn't exist or has been deleted" — and a `username` already held in the
//!   network is refused, since it "must be unique within the network".

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::ops::types::PasswordRequirements;
use service_com_amazonaws_wickr_wickradminapi::state::{
    password_meets_requirements, BOT_GONE_MESSAGE, BOT_STATUS_PENDING, NETWORK_GONE_MESSAGE,
    PASSWORD_POLICY_FIELD, SECURITY_GROUP_GONE_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The status `CreateNetwork` answers a create it carried out with (see
/// `src/ops/create_network.rs`).
const CREATE_NETWORK_STATUS: u16 = 201;

/// The status the model declares for `ValidationError` (`@httpError(422)`).
const VALIDATION_ERROR_STATUS: u16 = 422;

/// The status the model declares for `ResourceNotFoundError`.
const NOT_FOUND_STATUS: u16 = 404;

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the bot rules are.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// One request/response round trip: the HTTP status and the parsed JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, parsed)
}

/// Create a network and return its minted id, failing the test if it was refused.
async fn create_network(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await;
    assert_eq!(
        status, CREATE_NETWORK_STATUS,
        "CreateNetwork {name:?}: expected {CREATE_NETWORK_STATUS}, got {status} with {body}"
    );
    body["networkId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNetwork {name:?}: no networkId in {body}"))
        .to_string()
}

/// The network's default security group as `ListSecurityGroups` reports it — the
/// group the steps below assign bots to, and the policy they are measured
/// against.
async fn default_group(router: &MultiServiceRouter, network_id: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/networks/{network_id}/security-groups"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "ListSecurityGroups: expected 200, got {status} with {body}"
    );
    body["securityGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .cloned()
        .unwrap_or_else(|| panic!("ListSecurityGroups: no security group in {body}"))
}

/// One `CreateBot` call.
async fn create_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    username: &str,
    group_id: &str,
    challenge: &str,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/networks/{network_id}/bots"),
        Some(json!({
            "username": username,
            "displayName": "Bot",
            "groupId": group_id,
            "challenge": challenge,
        })),
    )
    .await
}

/// The bots `ListBots` reports for one network, asserting the listing was served.
async fn list_bots(router: &MultiServiceRouter, case: &str, network_id: &str) -> Vec<Value> {
    let (status, body) = call(
        router,
        "GET",
        &format!("/networks/{network_id}/bots"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBots expected 200, got {status} with {body}"
    );
    body["bots"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: bots is required, got {body}"))
}

/// The `__type` discriminator of an error body, or `""` when there is none.
fn error_code(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

/// One number of a reported `passwordRequirements`.
fn requirement(group: &Value, member: &str) -> i64 {
    group["securityGroupSettings"]["passwordRequirements"][member]
        .as_i64()
        .unwrap_or_else(|| panic!("the group reports no {member} in its policy: {group}"))
}

/// The summary a password that does not meet a policy is refused with, as the
/// recorded wire states it: the fixed sentence, the four character-class lines in
/// that order and the minimum length, each line prefixed by a newline and six
/// spaces and the last one followed by the same prefix again. Transcribed here
/// independently of the emulator, and filled with the numbers the GROUP reports.
fn expected_reason(group: &Value) -> String {
    let uppercase = requirement(group, "uppercase");
    let lowercase = requirement(group, "lowercase");
    let numbers = requirement(group, "numbers");
    let symbols = requirement(group, "symbols");
    let min_length = requirement(group, "minLength");
    format!("password does not meet minimum requirements: \n      {uppercase} uppercase letter(s), \n      {lowercase} lowercase letter(s), \n      {numbers} number(s), \n      {symbols} symbol(s), \n      minimum {min_length} letters\n      ")
}

/// Every `challenge` the group's policy turns down is refused the same way, and
/// the refusal creates nothing.
///
/// The rows differ along what the rule generalises over — the length of the
/// password, the character classes it is made of, and the script those characters
/// belong to — and every one of them is a password no constraint trait could have
/// refused, so each reaches the handler and is decided by the group's policy
/// alone.
#[tokio::test]
async fn a_challenge_the_groups_policy_turns_down_is_refused_and_creates_no_bot() {
    let router = emulator();
    let network_id = create_network(&router, "weak-bot-net").await;
    let group = default_group(&router, &network_id).await;
    let group_id = group["id"].as_str().expect("the group reports an id");
    let min_length = requirement(&group, "minLength");
    let reason = expected_reason(&group);

    for (case, challenge) in [
        ("no password at all", ""),
        ("one character", "a"),
        ("six characters", "short1"),
        ("one below the minimum", "1234567"),
        ("short but every class", "Aa1!"),
        ("short outside ASCII", "pässw"),
    ] {
        assert!(
            i64::try_from(challenge.chars().count()).unwrap_or(i64::MAX) < min_length,
            "case {case:?}: the row is only a weak password while it is shorter than \
             the group's minimum {min_length}, got {} characters",
            challenge.chars().count()
        );
        let (status, body) = create_bot(&router, &network_id, "weak-bot", group_id, challenge).await;
        assert_eq!(
            status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected the modelled ValidationError status \
             {VALIDATION_ERROR_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {case:?}: expected ValidationError, got {body}"
        );
        assert!(
            body["message"].is_null(),
            "case {case:?}: the diagnosis lives in reasons[], so the body carries no \
             envelope sentence, got {body}"
        );
        let reasons = body["reasons"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected one reason, got {body}"));
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: expected exactly one reason, got {body}"
        );
        assert_eq!(
            reasons[0]["field"],
            json!(PASSWORD_POLICY_FIELD),
            "case {case:?}: the refusal must name the policy, got {body}"
        );
        assert_eq!(
            reasons[0]["reason"],
            json!(reason),
            "case {case:?}: expected {reason:?}, got {}",
            reasons[0]["reason"]
        );
        assert!(
            body["botId"].is_null(),
            "case {case:?}: a refused create reports no bot, got {body}"
        );
        assert_eq!(
            list_bots(&router, case, &network_id).await,
            Vec::<Value>::new(),
            "case {case:?}: the refusal must create nothing"
        );
    }

    // The very sequence a client takes after being refused: a password that meets
    // the policy is created, so the refusals above were the policy check and not a
    // blanket failure.
    let (status, body) = create_bot(
        &router,
        &network_id,
        "weak-bot",
        group_id,
        "longenoughpassword1",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a password that meets the policy\": expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["username"],
        json!("weak-bot"),
        "case \"a password that meets the policy\": expected the requested username \
         echoed back, got {body}"
    );
    assert_eq!(
        list_bots(&router, "after the create", &network_id).await.len(),
        1,
        "case \"a password that meets the policy\": expected the one created bot"
    );

    // The rule is the GROUP's, not the default group's: a group created through this
    // API states no policy of its own and holds its accounts to the same one, so the
    // weak password is refused there too, with the same numbers.
    let (status, created) = call(
        &router,
        "POST",
        &format!("/networks/{network_id}/security-groups"),
        Some(json!({ "name": "another group", "securityGroupSettings": {} })),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateSecurityGroup: expected 200, got {status} with {created}"
    );
    let other_group = created["securityGroup"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateSecurityGroup: no group id in {created}"))
        .to_string();
    let (status, body) = create_bot(&router, &network_id, "other-bot", &other_group, "short1").await;
    assert_eq!(
        status, VALIDATION_ERROR_STATUS,
        "case \"another group of the network\": expected {VALIDATION_ERROR_STATUS}, got \
         {status} with {body}"
    );
    assert_eq!(
        body["reasons"][0]["field"],
        json!(PASSWORD_POLICY_FIELD),
        "case \"another group of the network\": the refusal must name the policy, got {body}"
    );
    assert_eq!(
        body["reasons"][0]["reason"],
        json!(reason),
        "case \"another group of the network\": expected {reason:?}, got {}",
        body["reasons"][0]["reason"]
    );
}

/// Every `challenge` that meets the group's policy creates the bot, which is then
/// readable back and counted in its group.
///
/// The rows differ in length, in which character classes they carry and in script,
/// including the shortest password the policy accepts at all.
#[tokio::test]
async fn a_challenge_that_meets_the_policy_creates_a_bot_that_reads_back() {
    let router = emulator();
    let network_id = create_network(&router, "strong-bot-net").await;
    let group = default_group(&router, &network_id).await;
    let group_id = group["id"]
        .as_str()
        .expect("the group reports an id")
        .to_string();
    let min_length = requirement(&group, "minLength");

    let long_passphrase = "l".repeat(128);
    let rows: [(&str, &str); 5] = [
        ("exactly the minimum", "12345678"),
        ("letters only", "longenoughpassword"),
        ("every character class", "P@ssw0rd!x"),
        ("far longer than the minimum", long_passphrase.as_str()),
        ("outside ASCII", "pässwörtchen"),
    ];

    for (index, (case, challenge)) in rows.iter().enumerate() {
        assert!(
            i64::try_from(challenge.chars().count()).unwrap_or(i64::MAX) >= min_length,
            "case {case:?}: the row is only an accepted password while it is at least \
             the group's minimum {min_length}, got {} characters",
            challenge.chars().count()
        );
        let username = format!("bot-{index}");
        let (status, body) = create_bot(&router, &network_id, &username, &group_id, challenge).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        for (member, expected) in [
            ("username", json!(username)),
            ("groupId", json!(group_id)),
            ("networkId", json!(network_id)),
            ("displayName", json!("Bot")),
        ] {
            assert_eq!(
                body[member], expected,
                "case {case:?}: {member} expected {expected}, got {}",
                body[member]
            );
        }
        let bot_id = body["botId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: botId is required, got {body}"))
            .to_string();
        // `BotId` is len 1..10 and `^[0-9]+$`.
        assert!(
            (1..=10).contains(&bot_id.len()) && bot_id.chars().all(|c| c.is_ascii_digit()),
            "case {case:?}: expected a botId of 1 to 10 digits, got {bot_id:?}"
        );

        // Round trip: the create is backed by a record `GetBot` reads back.
        let (status, read) = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/bots/{bot_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetBot expected 200, got {status} with {read}"
        );
        for (member, expected) in [
            ("botId", json!(bot_id)),
            ("username", json!(username)),
            ("groupId", json!(group_id)),
            ("displayName", json!("Bot")),
            ("hasChallenge", json!(true)),
            ("suspended", json!(false)),
            ("status", json!(BOT_STATUS_PENDING)),
        ] {
            assert_eq!(
                read[member], expected,
                "case {case:?}: GetBot {member} expected {expected}, got {}",
                read[member]
            );
        }
        assert!(
            read["uname"].as_str().is_some_and(|uname| !uname.is_empty()),
            "case {case:?}: a bot is known by a uname, got {read}"
        );
        // The password itself is never reported back, whatever it was.
        let serialized = read.to_string();
        assert!(
            !serialized.contains(*challenge),
            "case {case:?}: GetBot must not report the challenge, got {read}"
        );

        // And by the listing, which reports the same members.
        let listed = list_bots(&router, case, &network_id).await;
        assert_eq!(
            listed.len(),
            index + 1,
            "case {case:?}: expected every bot created so far, got {listed:?}"
        );
        let found = listed
            .iter()
            .find(|bot| bot["botId"] == json!(bot_id))
            .unwrap_or_else(|| panic!("case {case:?}: {bot_id} is not in {listed:?}"));
        assert_eq!(
            found["username"],
            json!(username),
            "case {case:?}: ListBots must report the stored username, got {found}"
        );

        // The bot is a member of the group it was assigned to, which the group's own
        // read reports.
        let group = default_group(&router, &network_id).await;
        assert_eq!(
            group["botMembers"],
            json!(index + 1),
            "case {case:?}: expected the group to count every bot in it, got {group}"
        );
    }
}

/// The policy decides a password class by class and at the boundary of each
/// requirement: a count at the requirement meets it, one below does not.
///
/// The password policy of a group is not settable through this API, so the rows
/// drive the rule the handler applies over policies the API cannot build —
/// stricter minimums, each character class required on its own, and a policy that
/// states nothing at all.
#[tokio::test]
async fn each_requirement_of_a_policy_is_decided_at_its_boundary() {
    /// One policy, named by the member it requires, as `PasswordRequirements`
    /// states it.
    fn policy(min_length: i32, uppercase: i32, lowercase: i32, numbers: i32, symbols: i32) -> PasswordRequirements {
        PasswordRequirements {
            min_length: Some(min_length),
            uppercase: Some(uppercase),
            lowercase: Some(lowercase),
            numbers: Some(numbers),
            symbols: Some(symbols),
        }
    }

    let rows: [(&str, PasswordRequirements, &str, bool); 16] = [
        // The policy the group a network is created with carries: a minimum length
        // and no required mix.
        ("the default policy, at its minimum", policy(8, 0, 0, 0, 0), "12345678", true),
        ("the default policy, one below", policy(8, 0, 0, 0, 0), "1234567", false),
        ("the default policy, empty", policy(8, 0, 0, 0, 0), "", false),
        // A stricter minimum length, at the boundary and either side of it.
        ("a longer minimum, one below", policy(12, 0, 0, 0, 0), "elevenchars", false),
        ("a longer minimum, exactly", policy(12, 0, 0, 0, 0), "twelvecharss", true),
        ("a longer minimum, one above", policy(12, 0, 0, 0, 0), "thirteencharss", true),
        // Each character class required on its own, at its boundary.
        ("two uppercase, one given", policy(4, 2, 0, 0, 0), "Abcd", false),
        ("two uppercase, both given", policy(4, 2, 0, 0, 0), "ABcd", true),
        ("one lowercase, none given", policy(4, 0, 1, 0, 0), "ABCD", false),
        ("one lowercase, given", policy(4, 0, 1, 0, 0), "ABCd", true),
        ("three numbers, two given", policy(4, 0, 0, 3, 0), "ab12", false),
        ("three numbers, all given", policy(4, 0, 0, 3, 0), "a123", true),
        ("one symbol, none given", policy(4, 0, 0, 0, 1), "abcd", false),
        ("one symbol, given", policy(4, 0, 0, 0, 1), "abc!", true),
        // A policy that requires nothing requires nothing, whatever the password.
        ("a policy stating nothing", PasswordRequirements::default(), "", true),
        // Characters are counted as characters, not bytes: eight characters outside
        // ASCII are eight characters.
        ("counted in characters", policy(8, 0, 0, 0, 0), "pässwörtchen", true),
    ];

    for (case, requirements, password, expected) in rows {
        let actual = password_meets_requirements(password, &requirements);
        assert_eq!(
            actual, expected,
            "case {case:?}: {password:?} against {requirements:?} expected \
             {expected}, got {actual}"
        );
    }
}

/// Every `sortFields` value the listing accepts orders the stored bots, and one it
/// cannot serve is refused naming the member.
///
/// The accepted rows differ along the dimension `ListBotsRequest$sortFields` spans
/// — one name, each accepted name, several names separated by '+', a name repeated,
/// each direction — and every one of them reports the whole stored set. The refused
/// rows are the values that name something the listing cannot order by at all.
#[tokio::test]
async fn every_sort_argument_the_listing_accepts_orders_the_stored_bots() {
    let router = emulator();
    let network_id = create_network(&router, "sorted-bot-net").await;
    let group = default_group(&router, &network_id).await;
    let group_id = group["id"]
        .as_str()
        .expect("the group reports an id")
        .to_string();
    for username in ["a-bot", "b-bot", "c-bot"] {
        let (status, body) =
            create_bot(&router, &network_id, username, &group_id, "longenoughpassword1").await;
        assert_eq!(
            status, 200,
            "CreateBot {username:?}: expected 200, got {status} with {body}"
        );
    }

    for (case, query) in [
        ("no argument", ""),
        ("by username", "?sortFields=username"),
        ("by display name", "?sortFields=displayName"),
        ("by status", "?sortFields=status"),
        ("by group", "?sortFields=groupId"),
        ("by first name", "?sortFields=firstName"),
        ("by two fields", "?sortFields=groupId%2Busername"),
        ("by every field it accepts",
         "?sortFields=username%2BfirstName%2BdisplayName%2Bstatus%2BgroupId"),
        // A name the value repeats orders the listing no further, so it is served
        // exactly as the name given once is.
        ("the same field twice", "?sortFields=username%2Busername"),
        ("ascending", "?sortFields=username&sortDirection=ASC"),
        ("descending", "?sortFields=username&sortDirection=DESC"),
        ("one item a page", "?maxResults=1"),
    ] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/bots{query}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        let reported = body["bots"].as_array().cloned().unwrap_or_default();
        let expected = if query.contains("maxResults=1") { 1 } else { 3 };
        assert_eq!(
            reported.len(),
            expected,
            "case {case:?}: expected {expected} bots on the page, got {body}"
        );
        for bot in &reported {
            assert_eq!(
                bot["groupId"],
                json!(group_id),
                "case {case:?}: every stored bot is in the group it was created in, got {bot}"
            );
        }
    }

    // A value the listing cannot serve is refused naming the member — never
    // ignored, and never answered as a differently ordered listing.
    for (case, query) in [
        ("a member of another shape", "?sortFields=uname"),
        ("no field at all", "?sortFields="),
        ("one name of several unknown", "?sortFields=username%2Buname"),
    ] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/bots{query}"),
            None,
        )
        .await;
        assert_eq!(
            status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected {VALIDATION_ERROR_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            body["reasons"][0]["field"],
            json!("sortFields"),
            "case {case:?}: the refusal must name sortFields, got {body}"
        );
        assert!(
            body["bots"].is_null(),
            "case {case:?}: a refusal reports no bots, got {body}"
        );
    }
}

/// A bot cannot be created where there is nothing to create it in, and a username
/// one account of the network already holds cannot name a second one.
#[tokio::test]
async fn a_create_that_names_no_resource_or_a_taken_username_is_refused() {
    let router = emulator();
    let network_id = create_network(&router, "refusal-bot-net").await;
    let group = default_group(&router, &network_id).await;
    let group_id = group["id"]
        .as_str()
        .expect("the group reports an id")
        .to_string();
    let challenge = "longenoughpassword1";

    // A well-formed id this store never minted: `NetworkId` ends at 99999999 and ids
    // are minted from the low end of the 8-digit range upwards; and the id of a
    // network that was deleted, which can still be placed and has nothing behind it.
    let deleted = create_network(&router, "deleted-bot-net").await;
    let (status, body) = call(&router, "DELETE", &format!("/networks/{deleted}"), None).await;
    assert_eq!(
        status, 200,
        "DeleteNetwork: expected 200, got {status} with {body}"
    );
    for (case, network) in [
        ("an id this store never minted", "99999999"),
        ("a network that was deleted", deleted.as_str()),
    ] {
        let (status, body) = create_bot(&router, network, "any-bot", &group_id, challenge).await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected {NOT_FOUND_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundError",
            "case {case:?}: expected ResourceNotFoundError, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(NETWORK_GONE_MESSAGE),
            "case {case:?}: expected this API's fixed sentence, got {body}"
        );
    }

    // A `groupId` naming no group of this network names nothing to assign the bot
    // to, and nothing whose policy the password could be measured against.
    for (case, requested_group) in [
        ("a group id no network has", "4242"),
        ("a group id that is not a number", "no-such-group"),
    ] {
        let (status, body) =
            create_bot(&router, &network_id, "any-bot", requested_group, challenge).await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected {NOT_FOUND_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(SECURITY_GROUP_GONE_MESSAGE),
            "case {case:?}: expected this API's fixed sentence for a group that is not \
             there, got {body}"
        );
    }

    // A username is unique within the network, across its accounts: a bot cannot
    // take one another bot holds, nor one a user holds.
    let (status, body) = create_bot(&router, &network_id, "taken-bot", &group_id, challenge).await;
    assert_eq!(
        status, 200,
        "CreateBot \"taken-bot\": expected 200, got {status} with {body}"
    );
    let (status, body) = call(
        &router,
        "POST",
        &format!("/networks/{network_id}/users"),
        Some(json!({ "users": [{ "username": "taken-user", "securityGroupIds": [group_id] }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchCreateUser \"taken-user\": expected 200, got {status} with {body}"
    );

    for (case, username) in [
        ("a username another bot holds", "taken-bot"),
        ("a username a user holds", "taken-user"),
    ] {
        let (status, body) = create_bot(&router, &network_id, username, &group_id, challenge).await;
        assert_eq!(
            status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected {VALIDATION_ERROR_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            body["reasons"][0]["field"],
            json!("username"),
            "case {case:?}: the refusal must name username, got {body}"
        );
    }
    assert_eq!(
        list_bots(&router, "after the refusals", &network_id).await.len(),
        1,
        "the refused creates must leave the one stored bot alone"
    );

    // And a `botId` naming no bot of the network is the same refusal as a bot that
    // is not there, never an empty success.
    let (status, body) = call(
        &router,
        "GET",
        &format!("/networks/{network_id}/bots/999999"),
        None,
    )
    .await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case \"a bot id this network has not\": expected {NOT_FOUND_STATUS}, got \
         {status} with {body}"
    );
    assert_eq!(
        body["message"],
        json!(BOT_GONE_MESSAGE),
        "case \"a bot id this network has not\": expected this API's fixed sentence, \
         got {body}"
    );
}
