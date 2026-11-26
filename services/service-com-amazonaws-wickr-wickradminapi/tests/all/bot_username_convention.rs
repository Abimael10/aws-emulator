// SPDX-License-Identifier: CC0-1.0

//! The naming convention a bot account's `username` follows, and what a create that
//! served reports, driven over HTTP against one running emulator.
//!
//! The rules under test:
//!
//! * `CreateBotRequest$username` "must be unique within the network and follow the
//!   network's naming conventions"
//!   (`smithy_query(query="operation",operation="CreateBot")`). The convention a BOT
//!   account's name follows is its suffix: the name ends with `bot`. The model expresses
//!   no part of it — `username` is a `GenericString` whose `^[\S\s]*$` matches every
//!   string — so the service owns the whole rule, and a name that does not follow it is
//!   refused with the modelled `ValidationError` (`@httpError(422)`) whose `reasons`
//!   carries exactly one `ErrorDetail`: `field` names the refused member and `reason` is
//!   the one FIXED sentence this service states for a name that is not a bot name,
//!   transcribed below from the recorded wire. `ValidationError$message` stays unset, so
//!   the body carries no envelope sentence beside the per-field detail.
//! * `CreateBotResponse$message` is "a message indicating the result of the bot creation
//!   operation" (`smithy_query(query="operation",operation="CreateBot")`), and the result
//!   this service indicates for a create it served is one fixed word — it names neither
//!   the bot, its id nor the network, so every served create reports the same text.
//! * A refused create stores nothing: the network's bots, its listing and its group
//!   member tallies are what they were before the refusal.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response
//! serialization — so what it asserts is the wire behaviour a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{
    bot_username_follows_convention, BOT_USERNAME_INVALID_REASON, BOT_USERNAME_SUFFIX,
    USERNAME_FIELD,
};
use smithy_server_runtime::MultiServiceRouter;

/// The status the model declares for `ValidationError` (`@httpError(422)`).
const VALIDATION_ERROR_STATUS: u16 = 422;

/// The error shape a restJson1 client resolves from `x-amzn-errortype` and from the
/// body's `__type`.
const VALIDATION_ERROR_SHAPE: &str = "ValidationError";

/// The sentence this service states for a `username` that does not follow the bot-naming
/// convention, transcribed from the recorded wire and stated here independently of the
/// emulator's own constant, which the last assertion below pins to it.
const EXPECTED_REASON: &str = "Bot Username must end with bot";

/// The member such a refusal names.
const EXPECTED_FIELD: &str = "username";

/// The result a served create reports, transcribed from the recorded wire the same way.
const EXPECTED_CREATED_MESSAGE: &str = "success";

/// The suffix the convention is, as the refusal sentence states it.
const EXPECTED_SUFFIX: &str = "bot";

/// A password meeting the default policy of a network's seeded security group, so every
/// create below is decided by the username rather than by the password policy.
const CHALLENGE: &str = "longenoughpassword1";

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the naming rule and the wording are.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// What one call answered: the status, the `x-amzn-errortype` header (empty when the
/// answer carried none) and the parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

/// One request/response round trip.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Answer {
        status,
        error_type,
        body: parsed,
    }
}

/// Create a network and return its minted id, failing the test if it was refused.
async fn create_network(router: &MultiServiceRouter, name: &str) -> String {
    let answer = call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await;
    assert!(
        (200..300).contains(&answer.status),
        "CreateNetwork {name:?}: expected a success, got {} with {}",
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

/// One `CreateBot` call.
async fn create_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    username: &str,
    group_id: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/networks/{network_id}/bots"),
        Some(json!({
            "username": username,
            "displayName": "Bot",
            "groupId": group_id,
            "challenge": CHALLENGE,
        })),
    )
    .await
}

/// The bots `ListBots` reports for one network.
async fn list_bots(router: &MultiServiceRouter, case: &str, network_id: &str) -> Vec<Value> {
    let answer = call(router, "GET", &format!("/networks/{network_id}/bots"), None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: ListBots expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["bots"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: bots is required, got {}", answer.body))
}

/// Every name that does not follow the convention is refused the same way, and the
/// refusal creates nothing.
///
/// The rows differ along what the rule generalises over: whether the name carries the
/// suffix at all, where in the name it sits, what follows it, the length of the name and
/// the script it is built from. None of them is a name a constraint trait could refuse,
/// so every row reaches the handler and is decided by the naming rule alone.
#[tokio::test]
async fn a_username_that_does_not_follow_the_convention_is_refused_and_creates_no_bot() {
    let router = emulator();
    let network_id = create_network(&router, "bot-naming-net").await;
    let group_id = default_group_id(&router, &network_id).await;
    let long_name = "n".repeat(64);
    let suffix_inside = format!("{BOT_USERNAME_SUFFIX}-ops-service");

    let rows: &[(&str, &str)] = &[
        ("no suffix at all", "deploy"),
        ("no name at all", ""),
        ("the suffix at the start", suffix_inside.as_str()),
        ("the suffix followed by a letter", "deploybots"),
        ("the suffix followed by a space", "deploybot "),
        ("the suffix spelt with a digit", "deployb0t"),
        ("a name of 64 characters without the suffix", long_name.as_str()),
        ("a name built from another script", "ロボット"),
        ("an address-shaped name", "ann.lee@example.com"),
        ("a name shorter than the suffix", "b"),
    ];

    for (case, username) in rows {
        assert!(
            !bot_username_follows_convention(username),
            "case {case:?}: the row is only a refused name while it does not follow the \
             convention, got {username:?}"
        );
        let answer = create_bot(&router, &network_id, username, &group_id).await;
        assert_eq!(
            answer.status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected status {VALIDATION_ERROR_STATUS}, got {} with {}",
            answer.status, answer.body
        );
        assert!(
            answer.error_type.ends_with(VALIDATION_ERROR_SHAPE),
            "case {case:?}: expected x-amzn-errortype to name {VALIDATION_ERROR_SHAPE}, got \
             {:?}",
            answer.error_type
        );
        assert!(
            answer.body["__type"]
                .as_str()
                .is_some_and(|shape| shape.ends_with(VALIDATION_ERROR_SHAPE)),
            "case {case:?}: expected the body to name {VALIDATION_ERROR_SHAPE}, got {}",
            answer.body
        );
        assert!(
            answer.body["message"].is_null(),
            "case {case:?}: the diagnosis lives in reasons[], so the body carries no \
             envelope sentence, got {}",
            answer.body
        );
        let reasons = answer.body["reasons"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a reasons list, got {}", answer.body));
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: expected exactly one detail, got {}",
            answer.body
        );
        assert_eq!(
            reasons[0]["field"],
            json!(EXPECTED_FIELD),
            "case {case:?}: expected the detail to name {EXPECTED_FIELD}, got {}",
            answer.body
        );
        assert_eq!(
            reasons[0]["reason"],
            json!(EXPECTED_REASON),
            "case {case:?}: expected the fixed reason {EXPECTED_REASON:?}, got {}",
            reasons[0]["reason"]
        );
        assert!(
            answer.body["botId"].is_null(),
            "case {case:?}: a refused create reports no bot, got {}",
            answer.body
        );
        assert_eq!(
            list_bots(&router, case, &network_id).await,
            Vec::<Value>::new(),
            "case {case:?}: the refusal must create nothing"
        );
    }

    // The sentence is the same for every row, so it carries no part of the request, and
    // it is the one the service holds as its fixed text for this refusal.
    assert_eq!(
        BOT_USERNAME_INVALID_REASON, EXPECTED_REASON,
        "the service's fixed reason must be the recorded sentence, got \
         {BOT_USERNAME_INVALID_REASON:?}"
    );
    assert_eq!(
        USERNAME_FIELD, EXPECTED_FIELD,
        "the member such a refusal names must be the submitted one, got {USERNAME_FIELD:?}"
    );
    assert_eq!(
        BOT_USERNAME_SUFFIX, EXPECTED_SUFFIX,
        "the convention must be the suffix the refusal states, got {BOT_USERNAME_SUFFIX:?}"
    );
}

/// A name that does follow the convention is created, reports the service's one result
/// word, and reads back.
///
/// The rows differ along the same dimensions as the refused ones — the length of the
/// name, the script of the part before the suffix, the case of the suffix and how many
/// times the suffix appears — and each is created in the same network, so the create is
/// decided by the name's shape alone.
#[tokio::test]
async fn a_username_that_follows_the_convention_is_created_and_reports_the_fixed_result() {
    let router = emulator();
    let network_id = create_network(&router, "good-bot-net").await;
    let group_id = default_group_id(&router, &network_id).await;
    let long_name = format!("{}{BOT_USERNAME_SUFFIX}", "n".repeat(61));
    let other_script = format!("ロボット{BOT_USERNAME_SUFFIX}");
    let twice = format!("{BOT_USERNAME_SUFFIX}-ops-{BOT_USERNAME_SUFFIX}");

    let rows: &[(&str, &str)] = &[
        ("the suffix alone", BOT_USERNAME_SUFFIX),
        ("a word and the suffix", "deploybot"),
        ("a hyphenated name", "integration-bot"),
        ("the suffix in upper case", "MixedCaseBOT"),
        ("a name built from another script", other_script.as_str()),
        ("a name of 64 characters", long_name.as_str()),
        ("the suffix twice", twice.as_str()),
        ("an address-shaped name ending in the suffix", "ann.lee.bot"),
    ];

    for (index, (case, username)) in rows.iter().enumerate() {
        assert!(
            bot_username_follows_convention(username),
            "case {case:?}: the row is only an accepted name while it follows the \
             convention, got {username:?}"
        );
        let answer = create_bot(&router, &network_id, username, &group_id).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["message"],
            json!(EXPECTED_CREATED_MESSAGE),
            "case {case:?}: expected the fixed result {EXPECTED_CREATED_MESSAGE:?}, got {}",
            answer.body["message"]
        );
        assert_eq!(
            answer.body["username"],
            json!(username),
            "case {case:?}: expected the submitted name echoed back, got {}",
            answer.body
        );
        let bot_id = answer.body["botId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: botId is required, got {}", answer.body))
            .to_string();

        // Round trip: the create is backed by a record `GetBot` reads back.
        let read = call(
            &router,
            "GET",
            &format!("/networks/{network_id}/bots/{bot_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetBot expected 200, got {} with {}",
            read.status, read.body
        );
        assert_eq!(
            read.body["username"],
            json!(username),
            "case {case:?}: GetBot must report the stored name, got {}",
            read.body
        );
        assert_eq!(
            list_bots(&router, case, &network_id).await.len(),
            index + 1,
            "case {case:?}: expected every bot created so far"
        );
    }
}

/// The convention is the network's, not one network's: the same name is refused in every
/// network and in every group of a network, and a refusal leaves what the network already
/// holds alone.
#[tokio::test]
async fn the_convention_holds_for_every_network_and_group() {
    let router = emulator();
    let first = create_network(&router, "first-naming-net").await;
    let second = create_network(&router, "second-naming-net").await;
    let first_group = default_group_id(&router, &first).await;
    let second_group = default_group_id(&router, &second).await;

    // A second group of the first network, so the row below covers a group this API
    // created as well as the one the network was seeded with.
    let created = call(
        &router,
        "POST",
        &format!("/networks/{first}/security-groups"),
        Some(json!({ "name": "another group", "securityGroupSettings": {} })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "CreateSecurityGroup: expected 200, got {} with {}",
        created.status, created.body
    );
    let other_group = created.body["securityGroup"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateSecurityGroup: no group id in {}", created.body))
        .to_string();

    // One bot the first network holds, so every refusal below can be checked against a
    // network that is not empty.
    let answer = create_bot(&router, &first, "resident-bot", &first_group).await;
    assert_eq!(
        answer.status, 200,
        "the resident bot must be created, got {} with {}",
        answer.status, answer.body
    );

    // (case, the network, the group)
    let rows: &[(&str, &str, &str)] = &[
        ("the seeded group of a network holding a bot", first.as_str(), first_group.as_str()),
        ("a group this API created", first.as_str(), other_group.as_str()),
        ("the seeded group of an empty network", second.as_str(), second_group.as_str()),
    ];

    for (case, network_id, group_id) in rows {
        let answer = create_bot(&router, network_id, "no-suffix-here", group_id).await;
        assert_eq!(
            answer.status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected status {VALIDATION_ERROR_STATUS}, got {} with {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["reasons"][0]["reason"],
            json!(EXPECTED_REASON),
            "case {case:?}: expected the fixed reason {EXPECTED_REASON:?}, got {}",
            answer.body
        );
    }

    assert_eq!(
        list_bots(&router, "the first network after the refusals", &first)
            .await
            .len(),
        1,
        "the refusals must leave the one stored bot alone"
    );
    assert_eq!(
        list_bots(&router, "the second network after the refusals", &second).await,
        Vec::<Value>::new(),
        "the refusals must create nothing in the empty network"
    );
}
