// SPDX-License-Identifier: CC0-1.0

//! How a read of ONE bot answers a bot it cannot report, driven over HTTP against one
//! running emulator.
//!
//! The rules under test:
//!
//! * `GetBot` is `GET /networks/{networkId}/bots/{botId}` and the resource it reports on is
//!   the BOT the request named, so a `botId` this store holds no bot for under that
//!   `networkId` is refused as that bot — whether no bot of the network ever carried the
//!   id, the id names a bot of a DIFFERENT network, or the network that held it has been
//!   deleted and its bots went with it. The parent network never stands in for it:
//!   `ResourceNotFoundError` is "a network, user, bot, security group, or other resource
//!   that doesn't exist or has been deleted" and its `message` is "a message identifying
//!   which resource was not found" (`smithy_query(query="shape",shape="ResourceNotFoundError")`),
//!   so the sentence a bot read states is this API's fixed gone-BOT sentence.
//! * That one refusal names its error SHAPE nowhere: HTTP 404 with the sentence and no
//!   `x-amzn-errortype` header and no discriminator member in the body, so a client
//!   classifies it by status. Everything else this service refuses still names its shape —
//!   a network read of a deleted network is the typed `ForbiddenError` this API states for a
//!   network that is gone.
//! * The sentence is entirely fixed text: it names neither the `botId` nor the `networkId`
//!   the request carried.
//! * A bot that IS there is still reported, and deleting one network leaves another
//!   network's bots readable, so the refusal follows the state of the bot that was asked
//!   for rather than the state of the store.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response serialization
//! — so what it asserts is the wire behaviour a client sees.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{BOT_GONE_MESSAGE, NETWORK_GONE_MESSAGE};
use smithy_server_runtime::MultiServiceRouter;

/// The status the model declares for `ResourceNotFoundError` (`@httpError(404)`).
const NOT_FOUND_STATUS: u16 = 404;

/// The status the model declares for `ForbiddenError` (`@httpError(403)`), which is what a
/// read of a DELETED NETWORK is refused with — the control showing this service does name
/// the shape of its other refusals.
const FORBIDDEN_STATUS: u16 = 403;

/// The body members a JSON failure can name its error shape in, in the order a client
/// consults them: `__type` is what this emulator's restJson1 renderer writes, and
/// `code` / `Code` are the other spellings the same parsers accept.
const DISCRIMINATOR_MEMBERS: [&str; 3] = ["__type", "code", "Code"];

/// A password meeting the default policy of a network's seeded security group, so every
/// create below is decided by what the test is about rather than by the policy.
const CHALLENGE: &str = "longenoughpassword1";

/// One answer as a client sees it: the status, the `x-amzn-errortype` header restJson1
/// resolves an error shape from, and the parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The `message` member of the body, or `""` when there is none.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }

    /// The error shape the body names, or `""` when it names none.
    fn body_shape(&self) -> String {
        DISCRIMINATOR_MEMBERS
            .iter()
            .find_map(|member| self.body[*member].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// A fresh emulator holding only this service, over its own engine, so one test's networks
/// are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the refusal path is.
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Answer {
        status,
        error_type,
        body,
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
        answer.status,
        answer.body
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

/// Create one bot of `network_id` and return its minted `botId`.
async fn create_bot(router: &MultiServiceRouter, network_id: &str, username: &str) -> String {
    let group_id = default_group_id(router, network_id).await;
    let answer = call(
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

/// Delete one network and wait out the deletion the way a client does: by asking for the
/// network until the read is refused.
///
/// The deadline bounds the loop and is not what the test measures; the assertion after it
/// is the check.
async fn delete_network(router: &MultiServiceRouter, network_id: &str) {
    let answer = call(router, "DELETE", &format!("/networks/{network_id}"), None).await;
    assert!(
        (200..300).contains(&answer.status),
        "DeleteNetwork: expected a success, got {} with {}",
        answer.status,
        answer.body
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut answer = call(router, "GET", &format!("/networks/{network_id}"), None).await;
    while answer.status < 400 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        answer = call(router, "GET", &format!("/networks/{network_id}"), None).await;
    }
    assert_eq!(
        answer.status, FORBIDDEN_STATUS,
        "DeleteNetwork: expected the network read to end up refused {FORBIDDEN_STATUS}, got {} \
         with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.message(),
        NETWORK_GONE_MESSAGE,
        "DeleteNetwork: expected the gone-network sentence, got {}",
        answer.body
    );
}

/// Assert `answer` is the refusal this API states for a bot it cannot report: the status,
/// the fixed sentence, no error shape in either place, and nothing of the ids that were
/// asked about.
fn assert_gone_bot(case: &str, answer: &Answer, network_id: &str, bot_id: &str) {
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
    assert_eq!(
        answer.error_type, "",
        "case {case:?}: the refusal must name no error shape in the header, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.body_shape(),
        "",
        "case {case:?}: the refusal must name no error shape in the body, got {}",
        answer.body
    );
    let rendered = answer.body.to_string();
    assert!(
        !rendered.contains(bot_id),
        "case {case:?}: the refusal must not disclose the botId it was asked about, got {}",
        answer.body
    );
    assert!(
        !rendered.contains(network_id),
        "case {case:?}: the refusal must not disclose the networkId it was asked about, got {}",
        answer.body
    );
}

/// Every bot a read cannot resolve is refused as that BOT, in the one untyped form.
///
/// The rows differ along what the rule generalises over: whether the network behind the
/// label exists, was deleted or was never minted, and whether the `botId` was never minted,
/// belongs to another network, or sits at either end of the modelled `BotId` space
/// (`@length(1, 10)`, `@pattern ^[0-9]+$`).
#[tokio::test]
async fn a_bot_a_read_cannot_resolve_is_refused_as_that_bot_and_names_no_error_shape() {
    let router = emulator();
    let live = create_network(&router, "live-bot-net").await;
    let live_bot = create_bot(&router, &live, "live-bot").await;
    let other = create_network(&router, "other-bot-net").await;
    let other_bot = create_bot(&router, &other, "other-bot").await;
    let doomed = create_network(&router, "doomed-bot-net").await;
    let doomed_bot = create_bot(&router, &doomed, "doomed-bot").await;
    delete_network(&router, &doomed).await;

    // A well-formed `NetworkId` (`^[0-9]{8}$`) this store never minted: ids are minted from
    // the low end of the 8-digit range upwards, so neither end of it is a network here.
    let unminted_network = "99999999";
    assert!(
        ![live.as_str(), other.as_str(), doomed.as_str()].contains(&unminted_network),
        "the unminted network id must not be one this store minted"
    );

    // (case, the networkId asked under, the botId asked for)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a botId no bot of the network ever carried",
            live.clone(),
            "999999".to_string(),
        ),
        (
            "the shortest botId the model allows",
            live.clone(),
            "0".to_string(),
        ),
        (
            "the longest botId the model allows",
            live.clone(),
            "9999999999".to_string(),
        ),
        (
            "a botId that names a bot of another network",
            live.clone(),
            other_bot.clone(),
        ),
        (
            "the bot of a network that has been deleted",
            doomed.clone(),
            doomed_bot.clone(),
        ),
        (
            "a bot of a deleted network read under a live one",
            live.clone(),
            doomed_bot.clone(),
        ),
        (
            "a networkId this store never minted",
            unminted_network.to_string(),
            live_bot.clone(),
        ),
        (
            "a bot of a network that never existed",
            unminted_network.to_string(),
            "1".to_string(),
        ),
    ];

    for (case, network_id, bot_id) in rows {
        let answer = get_bot(&router, &network_id, &bot_id).await;
        assert_gone_bot(case, &answer, &network_id, &bot_id);
    }

    // The bots that ARE there are still reported, so the refusals above follow the bot the
    // request named rather than the state of the store.
    for (case, network_id, bot_id, username) in [
        ("the live network's bot", &live, &live_bot, "live-bot"),
        ("the other network's bot", &other, &other_bot, "other-bot"),
    ] {
        let answer = get_bot(&router, network_id, bot_id).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["botId"].as_str(),
            Some(bot_id.as_str()),
            "case {case:?}: expected the bot that was asked for, got {}",
            answer.body
        );
        assert_eq!(
            answer.body["username"].as_str(),
            Some(username),
            "case {case:?}: expected the stored username, got {}",
            answer.body
        );
    }
}

/// A read inside a deleted network reports the BOT, while a read OF that network reports
/// the network — and only the first of the two is untyped.
///
/// The rows are the two resources one cascade leaves behind, so the two refusals cannot be
/// confused for each other: each states its own sentence, and the network's names its shape
/// where the bot's names none.
#[tokio::test]
async fn a_cascade_is_reported_at_the_resource_the_request_named() {
    let router = emulator();
    let kept = create_network(&router, "kept-bot-net").await;
    let kept_bot = create_bot(&router, &kept, "kept-bot").await;
    let gone = create_network(&router, "gone-bot-net").await;
    let gone_bot = create_bot(&router, &gone, "gone-bot").await;
    let second_bot = create_bot(&router, &gone, "second-gone-bot").await;
    delete_network(&router, &gone).await;

    for (case, bot_id) in [
        ("the first bot the network held", &gone_bot),
        ("the second bot the network held", &second_bot),
    ] {
        let answer = get_bot(&router, &gone, bot_id).await;
        assert_gone_bot(case, &answer, &gone, bot_id);
    }

    // The network itself is the resource a network read names, and that refusal keeps the
    // shape this API states it in.
    let answer = call(&router, "GET", &format!("/networks/{gone}"), None).await;
    assert_eq!(
        answer.status, FORBIDDEN_STATUS,
        "case \"the deleted network itself\": expected {FORBIDDEN_STATUS}, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.message(),
        NETWORK_GONE_MESSAGE,
        "case \"the deleted network itself\": expected the gone-network sentence, got {}",
        answer.body
    );
    assert_eq!(
        answer.body_shape(),
        "ForbiddenError",
        "case \"the deleted network itself\": expected the typed shape, got {}",
        answer.body
    );
    assert_eq!(
        answer.error_type, "ForbiddenError",
        "case \"the deleted network itself\": expected x-amzn-errortype ForbiddenError, got {:?}",
        answer.error_type
    );

    // A neighbour's bot is untouched by the cascade.
    let answer = get_bot(&router, &kept, &kept_bot).await;
    assert_eq!(
        answer.status, 200,
        "case \"a neighbour network's bot\": expected 200, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["username"].as_str(),
        Some("kept-bot"),
        "case \"a neighbour network's bot\": expected its own record, got {}",
        answer.body
    );
}

/// The refusal body carries the sentence and nothing else: no shape, and none of the
/// members another refusal class of this API would carry.
#[tokio::test]
async fn the_refusal_body_carries_the_sentence_alone() {
    let router = emulator();
    let network_id = create_network(&router, "body-bot-net").await;
    let answer = get_bot(&router, &network_id, "4242").await;
    assert_eq!(
        answer.status, NOT_FOUND_STATUS,
        "expected {NOT_FOUND_STATUS}, got {} with {}",
        answer.status, answer.body
    );
    let object = answer
        .body
        .as_object()
        .unwrap_or_else(|| panic!("the refusal body must be a JSON object, got {}", answer.body));
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(
        members,
        vec!["message"],
        "expected the sentence alone, got {}",
        answer.body
    );
}
