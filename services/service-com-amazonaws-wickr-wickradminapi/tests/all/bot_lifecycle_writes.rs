// SPDX-License-Identifier: CC0-1.0

//! The two Wickr operations that MUTATE an existing bot, driven over HTTP against one
//! running emulator: what they write, and what a later read of that bot reports.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response
//! serialization — so what it asserts is the wire behaviour a client sees.
//!
//! The rules under test, and where each comes from:
//!
//! * A bot is ONE stored record, and the operations addressed to it by
//!   `/networks/{networkId}/bots/{botId}` share it: `UpdateBot` (PATCH) "updates the
//!   properties of an existing bot ... allows you to modify the bot's display name,
//!   security group, password, or suspension status", `DeleteBot` (DELETE) "permanently
//!   removes the bot account and its associated data from the network", and `GetBot`
//!   reports that record. So a read after an accepted update reports exactly what the
//!   update set, every property the request omitted unchanged, and a read after a delete
//!   finds no bot at all.
//! * A deleted bot is gone for every later caller, with nothing of it kept: no read, no
//!   listing, and the `username` it held — which "must be unique within the network" —
//!   is available to a new account again. Only its `botId` is not reused.
//! * `UpdateBot$suspend` is "set to true to suspend the bot or false to unsuspend it.
//!   Omit this field for standard updates that don't affect suspension status", which is
//!   the omitted-member rule of the whole PATCH stated for one member.
//! * `UpdateBot$challenge` is "the new password for the bot account" and
//!   `Bot$hasChallenge` "indicates whether the bot has a password set", so a password is
//!   recorded as the bot HAVING one and is never reported back.
//! * The security group a bot is assigned to carries "the password complexity
//!   requirements that users must follow when creating or changing passwords"
//!   (`SecurityGroupSettings$passwordRequirements`), so a new password is measured
//!   against the group the bot ends up in, exactly as the password it was created with
//!   was.
//! * A network, user, bot or security group "that doesn't exist or has been deleted" is
//!   answered `ResourceNotFoundError` (HTTP 404), and the resource a refusal names is the
//!   one the request named: these two operations address one BOT.
//! * A refused update writes nothing: the bot is left exactly as it was.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_wickr_wickradminapi::state::{
    BOT_GONE_MESSAGE, BOT_STATUS_PENDING, DEFAULT_PASSWORD_MIN_LENGTH, PASSWORD_POLICY_FIELD,
    SECURITY_GROUP_GONE_MESSAGE,
};

/// The status the model declares for `ResourceNotFoundError` (`@httpError(404)`).
const NOT_FOUND_STATUS: u16 = 404;

/// The status the model declares for `ValidationError` (`@httpError(422)`).
const VALIDATION_STATUS: u16 = 422;

/// The status `CreateNetwork` answers a create it carried out with.
const CREATE_NETWORK_STATUS: u16 = 201;

/// A password meeting the default policy of a network's seeded security group
/// (a minimum length and no required character mix), so every create and every
/// accepted rotation below is decided by what the case is about rather than by the
/// policy.
const CHALLENGE: &str = "longenoughpassword1";

/// The display name every bot in these cases is created with, so what an update changed
/// is visible against it.
const BASELINE_DISPLAY_NAME: &str = "Baseline Bot";

/// One answer as a client sees it.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The `message` member of the body, or `""` when there is none.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }

    /// The member a `ValidationError` names as the one that was refused.
    fn refused_field(&self) -> &str {
        self.body["reasons"][0]["field"].as_str().unwrap_or_default()
    }
}

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the write path is.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// One request/response round trip.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
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
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Answer { status, body }
}

/// Create a network and return its minted id.
async fn create_network(router: &MultiServiceRouter, name: &str) -> String {
    let answer = call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await;
    assert_eq!(
        answer.status, CREATE_NETWORK_STATUS,
        "CreateNetwork {name:?}: expected {CREATE_NETWORK_STATUS}, got {} with {}",
        answer.status, answer.body
    );
    answer.body["networkId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNetwork {name:?}: no networkId in {}", answer.body))
        .to_string()
}

/// The id of the network's seeded security group, as `ListSecurityGroups` reports it.
async fn default_group_id(router: &MultiServiceRouter, network_id: &str) -> String {
    let answer = call(
        router,
        "GET",
        &format!("/networks/{network_id}/security-groups"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "ListSecurityGroups: expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["securityGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .and_then(|group| group["id"].as_str())
        .unwrap_or_else(|| panic!("ListSecurityGroups: no group id in {}", answer.body))
        .to_string()
}

/// Create one more security group of `network_id` and return its minted id.
async fn create_group(router: &MultiServiceRouter, network_id: &str, name: &str) -> String {
    let answer = call(
        router,
        "POST",
        &format!("/networks/{network_id}/security-groups"),
        Some(json!({ "name": name, "securityGroupSettings": {} })),
    )
    .await;
    assert!(
        (200..300).contains(&answer.status),
        "CreateSecurityGroup {name:?}: expected a success, got {} with {}",
        answer.status,
        answer.body
    );
    answer.body["securityGroup"]["id"]
        .as_str()
        .or_else(|| answer.body["id"].as_str())
        .unwrap_or_else(|| {
            panic!("CreateSecurityGroup {name:?}: no group id in {}", answer.body)
        })
        .to_string()
}

/// Create one bot of `network_id` in `group_id` and return its minted `botId`.
async fn create_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    group_id: &str,
    username: &str,
) -> String {
    let answer = call(
        router,
        "POST",
        &format!("/networks/{network_id}/bots"),
        Some(json!({
            "username": username,
            "displayName": BASELINE_DISPLAY_NAME,
            "groupId": group_id,
            "challenge": CHALLENGE,
        })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "CreateBot {username:?}: expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["botId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateBot {username:?}: no botId in {}", answer.body))
        .to_string()
}

/// Read one bot back.
async fn get_bot(router: &MultiServiceRouter, network_id: &str, bot_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/networks/{network_id}/bots/{bot_id}"),
        None,
    )
    .await
}

/// Apply one update to a bot.
async fn update_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    bot_id: &str,
    patch: Value,
) -> Answer {
    call(
        router,
        "PATCH",
        &format!("/networks/{network_id}/bots/{bot_id}"),
        Some(patch),
    )
    .await
}

/// Delete one bot.
async fn delete_bot(router: &MultiServiceRouter, network_id: &str, bot_id: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/networks/{network_id}/bots/{bot_id}"),
        None,
    )
    .await
}

/// Every `botId` one network's listing reports.
async fn listed_bot_ids(router: &MultiServiceRouter, network_id: &str) -> Vec<String> {
    let answer = call(router, "GET", &format!("/networks/{network_id}/bots"), None).await;
    assert_eq!(
        answer.status, 200,
        "ListBots: expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["bots"]
        .as_array()
        .unwrap_or_else(|| panic!("ListBots: no bots in {}", answer.body))
        .iter()
        .map(|bot| bot["botId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Assert `answer` is the refusal this API states for a bot it cannot resolve: the
/// modelled status and the fixed gone-bot sentence, which names no id.
fn assert_gone_bot(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, NOT_FOUND_STATUS,
        "case {case:?}: expected status {NOT_FOUND_STATUS}, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.message(),
        BOT_GONE_MESSAGE,
        "case {case:?}: expected the fixed gone-bot sentence {BOT_GONE_MESSAGE:?}, got {:?}",
        answer.message()
    );
}

/// The four mutable properties of a bot, as a read reports them, plus the three that
/// identify it.
async fn bot_members(router: &MultiServiceRouter, network_id: &str, bot_id: &str) -> Value {
    let answer = get_bot(router, network_id, bot_id).await;
    assert_eq!(
        answer.status, 200,
        "GetBot {bot_id}: expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body
}

/// RULE: an accepted update writes exactly the properties the request carries to the one
/// stored bot, leaves every property it omits as it stands, and the read after it reports
/// the result.
///
/// The rows differ along what the rule generalises over: which of the four modifiable
/// properties the request carries (one, several, all, none), and the form of the value
/// (a rename, a non-ASCII name, a name at no length at all, either boolean, a group
/// reassignment, a password rotation).
#[tokio::test]
async fn an_update_writes_the_members_it_carries_and_leaves_the_rest() {
    let router = emulator();
    let network_id = create_network(&router, "bot-update-net").await;
    let home = default_group_id(&router, &network_id).await;
    let elsewhere = create_group(&router, &network_id, "Elsewhere").await;
    assert_ne!(
        home, elsewhere,
        "the second group must be a different group, got {home} twice"
    );

    // (case, the PATCH body, the four modifiable members as a read must then report them:
    // displayName, groupId, hasChallenge, suspended)
    let rows: Vec<(&str, Value, Value, Value, Value, Value)> = vec![
        (
            "a rename",
            json!({ "displayName": "Renamed Bot" }),
            json!("Renamed Bot"),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "a rename to a non-ASCII display name",
            json!({ "displayName": "Ro\u{301}bot \u{1F916} \u{2014} \u{30DC}\u{30C3}\u{30C8}" }),
            json!("Ro\u{301}bot \u{1F916} \u{2014} \u{30DC}\u{30C3}\u{30C8}"),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "a rename to the empty display name",
            json!({ "displayName": "" }),
            json!(""),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "a suspension",
            json!({ "suspend": true }),
            json!(BASELINE_DISPLAY_NAME),
            json!(home),
            json!(true),
            json!(true),
        ),
        (
            "an unsuspension of a bot that was not suspended",
            json!({ "suspend": false }),
            json!(BASELINE_DISPLAY_NAME),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "a reassignment to another group of the network",
            json!({ "groupId": elsewhere }),
            json!(BASELINE_DISPLAY_NAME),
            json!(elsewhere),
            json!(true),
            json!(false),
        ),
        (
            "a reassignment to the group the bot is already in",
            json!({ "groupId": home }),
            json!(BASELINE_DISPLAY_NAME),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "a password rotation",
            json!({ "challenge": "anotherlongpassword2" }),
            json!(BASELINE_DISPLAY_NAME),
            json!(home),
            json!(true),
            json!(false),
        ),
        (
            "every modifiable property at once",
            json!({
                "displayName": "Everything Bot",
                "groupId": elsewhere,
                "challenge": "yetanotherpassword3",
                "suspend": true,
            }),
            json!("Everything Bot"),
            json!(elsewhere),
            json!(true),
            json!(true),
        ),
        (
            "an update carrying no property at all",
            json!({}),
            json!(BASELINE_DISPLAY_NAME),
            json!(home),
            json!(true),
            json!(false),
        ),
    ];

    for (index, (case, patch, display_name, group_id, has_challenge, suspended)) in
        rows.into_iter().enumerate()
    {
        let username = format!("update-{index}-bot");
        let bot_id = create_bot(&router, &network_id, &home, &username).await;
        let before = bot_members(&router, &network_id, &bot_id).await;

        let answer = update_bot(&router, &network_id, &bot_id, patch).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with {}",
            answer.status, answer.body
        );
        assert!(
            !answer.message().is_empty(),
            "case {case:?}: the success body must state the update in a message, got {}",
            answer.body
        );

        let after = bot_members(&router, &network_id, &bot_id).await;
        for (member, expected) in [
            ("displayName", &display_name),
            ("groupId", &group_id),
            ("hasChallenge", &has_challenge),
            ("suspended", &suspended),
        ] {
            assert_eq!(
                &after[member], expected,
                "case {case:?}: expected {member} {expected}, got {}",
                after[member]
            );
        }
        // What identifies the bot is not something an update can move, and a password is
        // never reported back.
        for member in ["botId", "username", "uname", "status"] {
            assert_eq!(
                after[member], before[member],
                "case {case:?}: {member} must be untouched by an update, expected {}, got {}",
                before[member], after[member]
            );
        }
        assert_eq!(
            after["status"],
            json!(BOT_STATUS_PENDING),
            "case {case:?}: expected the stored status {BOT_STATUS_PENDING}, got {}",
            after["status"]
        );
        assert!(
            after.get("challenge").is_none(),
            "case {case:?}: the read must report no password, got {after}"
        );
    }
}

/// RULE: the stored bot holds what the LAST accepted update set, so a client that updates
/// and reads never sees an earlier value.
///
/// The steps differ along the property each one moves and the direction it moves it in,
/// including a property set back to the value it was created with.
#[tokio::test]
async fn successive_updates_leave_the_bot_holding_the_last_accepted_value() {
    let router = emulator();
    let network_id = create_network(&router, "bot-resequence-net").await;
    let home = default_group_id(&router, &network_id).await;
    let bot_id = create_bot(&router, &network_id, &home, "resequenced-bot").await;

    // (case, the PATCH body, the member it moves, the value a read must then report)
    let steps: Vec<(&str, Value, &str, Value)> = vec![
        (
            "the first rename",
            json!({ "displayName": "First Name" }),
            "displayName",
            json!("First Name"),
        ),
        (
            "the second rename",
            json!({ "displayName": "Second Name" }),
            "displayName",
            json!("Second Name"),
        ),
        (
            "back to the name it was created with",
            json!({ "displayName": BASELINE_DISPLAY_NAME }),
            "displayName",
            json!(BASELINE_DISPLAY_NAME),
        ),
        (
            "suspended",
            json!({ "suspend": true }),
            "suspended",
            json!(true),
        ),
        (
            "suspended again",
            json!({ "suspend": true }),
            "suspended",
            json!(true),
        ),
        (
            "unsuspended",
            json!({ "suspend": false }),
            "suspended",
            json!(false),
        ),
    ];

    for (case, patch, member, expected) in steps {
        let answer = update_bot(&router, &network_id, &bot_id, patch).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with {}",
            answer.status, answer.body
        );
        let after = bot_members(&router, &network_id, &bot_id).await;
        assert_eq!(
            after[member], expected,
            "case {case:?}: expected {member} {expected}, got {}",
            after[member]
        );
    }
}

/// RULE: a delete removes the bot account, so every later caller finds no bot — the read
/// is the gone-bot refusal straight away, with nothing to wait out, the listing no longer
/// carries it, and a second delete and an update of it are refused the same way. Nothing
/// of it is kept: the `username` it held is available to a new account again, which then
/// carries a NEW `botId`.
///
/// The rows differ along what else the network holds when the bot goes: the only bot of
/// the network, one of several, and a bot of another group.
#[tokio::test]
async fn a_delete_removes_the_bot_account_for_every_later_caller() {
    let router = emulator();
    let network_id = create_network(&router, "bot-delete-net").await;
    let home = default_group_id(&router, &network_id).await;
    let elsewhere = create_group(&router, &network_id, "Elsewhere").await;

    // (case, the username of the bot that is deleted, the group it lives in, the
    // usernames of the bots that stay)
    let rows: Vec<(&str, &str, &str, Vec<&str>)> = vec![
        ("the only bot of the network", "solo-bot", &home, vec![]),
        (
            "one bot of several",
            "crowd-bot",
            &home,
            vec!["kept-bot", "other-kept-bot"],
        ),
        (
            "a bot of another group",
            "elsewhere-bot",
            &elsewhere,
            vec!["home-bot"],
        ),
    ];

    for (case, doomed_username, group_id, kept_usernames) in rows {
        let doomed = create_bot(&router, &network_id, group_id, doomed_username).await;
        let mut kept = Vec::new();
        for username in &kept_usernames {
            kept.push((
                *username,
                create_bot(&router, &network_id, &home, username).await,
            ));
        }

        let answer = delete_bot(&router, &network_id, &doomed).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from the delete, got {} with {}",
            answer.status, answer.body
        );
        assert!(
            !answer.message().is_empty(),
            "case {case:?}: the success body must state the deletion in a message, got {}",
            answer.body
        );

        // The very next read is already the refusal: this deletion has no later step.
        assert_gone_bot(
            &format!("{case}: the read after the delete"),
            &get_bot(&router, &network_id, &doomed).await,
        );
        // And so is every other call addressed to that bot.
        assert_gone_bot(
            &format!("{case}: a second delete"),
            &delete_bot(&router, &network_id, &doomed).await,
        );
        assert_gone_bot(
            &format!("{case}: an update of the deleted bot"),
            &update_bot(
                &router,
                &network_id,
                &doomed,
                json!({ "displayName": "Ghost" }),
            )
            .await,
        );

        let listed = listed_bot_ids(&router, &network_id).await;
        assert!(
            !listed.contains(&doomed),
            "case {case:?}: the listing must not carry the deleted bot {doomed}, got {listed:?}"
        );
        for (username, bot_id) in &kept {
            assert!(
                listed.contains(bot_id),
                "case {case:?}: the listing must still carry {username}, got {listed:?}"
            );
            let after = bot_members(&router, &network_id, bot_id).await;
            assert_eq!(
                after["username"],
                json!(username),
                "case {case:?}: {username} must be untouched by another bot's delete, got {after}"
            );
        }

        // The username the deleted bot held is available to a new account of the network
        // again, and that account is a new bot: ids are not handed out twice.
        let recreated = create_bot(&router, &network_id, group_id, doomed_username).await;
        assert_ne!(
            recreated, doomed,
            "case {case:?}: the recreated bot must carry a new botId, got {recreated} twice"
        );
        let after = bot_members(&router, &network_id, &recreated).await;
        assert_eq!(
            after["username"],
            json!(doomed_username),
            "case {case:?}: the recreated bot must hold the freed username, got {after}"
        );

        // Leave the network as the next row found it.
        delete_bot(&router, &network_id, &recreated).await;
        for (_, bot_id) in &kept {
            delete_bot(&router, &network_id, bot_id).await;
        }
    }
}

/// RULE: an update or a delete this service turns down writes nothing — the bot is left
/// exactly as it was — and names the resource the request named.
///
/// The rows differ along which part of the request is refused and by which rule: a bot
/// the store cannot resolve (by `botId`, and under a `networkId` that is no network), a
/// `groupId` naming no group of the network, and a password the group's policy turns
/// down (at the length just below its minimum, and at no length at all).
#[tokio::test]
async fn a_refused_update_leaves_the_bot_as_it_was() {
    let router = emulator();
    let network_id = create_network(&router, "bot-refusal-net").await;
    let home = default_group_id(&router, &network_id).await;
    let bot_id = create_bot(&router, &network_id, &home, "steady-bot").await;
    let before = bot_members(&router, &network_id, &bot_id).await;

    // A well-formed `NetworkId` (`^[0-9]{8}$`) this store never minted: ids are minted
    // from the low end of the 8-digit range upwards.
    let unminted_network = "99999999";
    assert_ne!(
        network_id, unminted_network,
        "the unminted network id must not be the one this store minted"
    );
    // A `groupId` naming no group of the network: the network is seeded with one group
    // and nothing else created one here.
    let unminted_group = "4242";
    assert_ne!(
        home, unminted_group,
        "the unminted group id must not be the network's own group"
    );
    // One character short of the minimum length the seeded group's default policy states.
    let short_password: String = "a".repeat(
        usize::try_from(DEFAULT_PASSWORD_MIN_LENGTH)
            .expect("the minimum length is not negative")
            .saturating_sub(1),
    );

    // (case, the networkId, the botId, the PATCH body, the status, the sentence or the
    // refused member)
    let rows: Vec<(&str, &str, &str, Value, u16, Refusal)> = vec![
        (
            "a botId no bot of the network ever carried",
            network_id.as_str(),
            "999999",
            json!({ "displayName": "Nobody" }),
            NOT_FOUND_STATUS,
            Refusal::Sentence(BOT_GONE_MESSAGE),
        ),
        (
            "a networkId this store never minted",
            unminted_network,
            bot_id.as_str(),
            json!({ "displayName": "Nobody" }),
            NOT_FOUND_STATUS,
            Refusal::Sentence(BOT_GONE_MESSAGE),
        ),
        (
            "a groupId naming no group of the network",
            network_id.as_str(),
            bot_id.as_str(),
            json!({ "groupId": unminted_group }),
            NOT_FOUND_STATUS,
            Refusal::Sentence(SECURITY_GROUP_GONE_MESSAGE),
        ),
        (
            "a password one character short of the policy's minimum",
            network_id.as_str(),
            bot_id.as_str(),
            json!({ "challenge": short_password }),
            VALIDATION_STATUS,
            Refusal::Field(PASSWORD_POLICY_FIELD),
        ),
        (
            "no password at all",
            network_id.as_str(),
            bot_id.as_str(),
            json!({ "challenge": "" }),
            VALIDATION_STATUS,
            Refusal::Field(PASSWORD_POLICY_FIELD),
        ),
        (
            "a refused password beside a rename that would have been accepted",
            network_id.as_str(),
            bot_id.as_str(),
            json!({ "displayName": "Half Applied", "challenge": short_password }),
            VALIDATION_STATUS,
            Refusal::Field(PASSWORD_POLICY_FIELD),
        ),
    ];

    for (case, network, bot, patch, status, refusal) in rows {
        let answer = update_bot(&router, network, bot, patch).await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected status {status}, got {} with {}",
            answer.status, answer.body
        );
        match refusal {
            Refusal::Sentence(sentence) => assert_eq!(
                answer.message(),
                sentence,
                "case {case:?}: expected the sentence {sentence:?}, got {}",
                answer.body
            ),
            Refusal::Field(field) => assert_eq!(
                answer.refused_field(),
                field,
                "case {case:?}: expected the refusal to name {field:?}, got {}",
                answer.body
            ),
        }
        let after = bot_members(&router, &network_id, &bot_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must write nothing, expected {before}, got {after}"
        );
    }

    // A delete of a bot the store cannot resolve is refused the same way, and leaves the
    // bot that IS there alone.
    for (case, network, bot) in [
        (
            "a delete of a botId no bot ever carried",
            network_id.as_str(),
            "999999",
        ),
        (
            "a delete under a networkId this store never minted",
            unminted_network,
            bot_id.as_str(),
        ),
    ] {
        assert_gone_bot(case, &delete_bot(&router, network, bot).await);
        let after = bot_members(&router, &network_id, &bot_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused delete must remove nothing, expected {before}, got {after}"
        );
    }
}

/// What a refusal is checked by: the fixed sentence a gone resource is stated in, or the
/// member a `ValidationError` names.
enum Refusal {
    Sentence(&'static str),
    Field(&'static str),
}
