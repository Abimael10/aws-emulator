// SPDX-License-Identifier: CC0-1.0

//! Why this API refuses a `username` an account of the network already holds, driven over
//! HTTP against one running emulator.
//!
//! The rules under test:
//!
//! * `CreateBotRequest$username` "must be unique within the network and follow the network's
//!   naming conventions"
//!   (`smithy_query(query="operation",operation="CreateBot")`), and a name the network is
//!   not free to give a new account is refused with the modelled `ValidationError`
//!   (`@httpError(422)`), whose `reasons` carries exactly one `ErrorDetail`: `field` names
//!   the refused member and `reason` is "a detailed description of the error"
//!   (`smithy_query(query="shape",shape="ErrorDetail")`).
//! * That reason is the one FIXED sentence this service states for a username that is not
//!   available — transcribed below from the recorded wire, independently of the emulator. It
//!   has no variable parts: whatever name was submitted and whichever network was asked, a
//!   client reads the same sentence, so the refusal discloses neither the name nor the
//!   network nor which account holds it. (`ValidationError` models no reason text, so this
//!   is recorded behaviour, like the fixed sentence the same service states for a network
//!   name it will not accept.)
//! * A name a bot may hold also follows the network's bot-naming convention, which is a
//!   rule of its own with its own fixed reason: every name below follows it, so what
//!   decides each case here is availability alone.
//! * A username names one account of ONE network, so the same name is free in another
//!   network, and both operations that create an account of a network — `CreateBot` and
//!   `BatchCreateUser`, whose per-item failure carries the same `field` / `reason` pair —
//!   state that one sentence.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response serialization
//! — so what it asserts is the wire behaviour a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{USERNAME_FIELD, USERNAME_UNAVAILABLE_REASON};
use smithy_server_runtime::MultiServiceRouter;

/// The status the model declares for `ValidationError` (`@httpError(422)`).
const VALIDATION_ERROR_STATUS: u16 = 422;

/// The sentence this service states for a username that is not available to a new account,
/// transcribed from the recorded wire and stated here independently of the emulator's own
/// constant, which the last assertion of the first test pins to it.
const EXPECTED_REASON: &str = "username is unavailable";

/// The member such a refusal names.
const EXPECTED_FIELD: &str = "username";

/// A password meeting the default policy of a network's seeded security group, so every
/// create below is decided by the username rather than by the password policy.
const CHALLENGE: &str = "longenoughpassword1";

/// A fresh emulator holding only this service, over its own engine, so one test's networks
/// are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the refusal wording is.
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
    assert!(
        (200..300).contains(&status),
        "CreateNetwork {name:?}: expected a success, got {status} with {body}"
    );
    body["networkId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNetwork {name:?}: no networkId in {body}"))
        .to_string()
}

/// The id of the network's seeded security group, as `ListSecurityGroups` reports it.
async fn default_group_id(router: &MultiServiceRouter, network_id: &str) -> String {
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
        .and_then(|group| group["id"].as_str())
        .unwrap_or_else(|| panic!("ListSecurityGroups: no group id in {body}"))
        .to_string()
}

/// One `CreateBot` call.
async fn create_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    username: &str,
    group_id: &str,
) -> (u16, Value) {
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

/// One `BatchCreateUser` call carrying one user.
async fn create_user(
    router: &MultiServiceRouter,
    network_id: &str,
    username: &str,
    group_id: &str,
) -> Value {
    let (status, body) = call(
        router,
        "POST",
        &format!("/networks/{network_id}/users"),
        Some(json!({ "users": [{ "username": username, "securityGroupIds": [group_id] }] })),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchCreateUser {username:?}: expected 200, got {status} with {body}"
    );
    body
}

/// The `reasons` list of a refusal body.
fn reasons(body: &Value) -> Vec<Value> {
    body["reasons"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("the refusal must carry its reasons list, got {body}"))
}

/// Assert `body` is the refusal this API states for a username that is not available: the
/// one detail, the member it names, and the fixed sentence with nothing of the request in
/// it.
fn assert_unavailable(case: &str, status: u16, body: &Value, network_id: &str) {
    assert_eq!(
        status, VALIDATION_ERROR_STATUS,
        "case {case:?}: expected status {VALIDATION_ERROR_STATUS}, got {status} with {body}"
    );
    let reasons = reasons(body);
    assert_eq!(
        reasons.len(),
        1,
        "case {case:?}: expected exactly one detail, got {body}"
    );
    assert_eq!(
        reasons[0]["field"].as_str(),
        Some(EXPECTED_FIELD),
        "case {case:?}: expected the detail to name {EXPECTED_FIELD}, got {body}"
    );
    assert_eq!(
        reasons[0]["reason"].as_str(),
        Some(EXPECTED_REASON),
        "case {case:?}: expected the fixed reason {EXPECTED_REASON:?}, got {}",
        reasons[0]["reason"]
    );
    assert!(
        !body.to_string().contains(network_id),
        "case {case:?}: the refusal must not disclose the network it was asked about, got {body}"
    );
}

/// Whatever the submitted name looks like and whichever account of the network holds it, the
/// refusal states the one fixed reason.
///
/// The rows differ along what the rule generalises over: the length of the name, the
/// characters and script it is built from, and whether the account already holding it is a
/// bot or a user. None of them is a name a constraint trait could refuse —
/// `CreateBotRequest$username` is a `GenericString` whose `^[\S\s]*$` matches every string —
/// so every row reaches the handler and is decided by the uniqueness rule alone.
#[tokio::test]
async fn a_username_the_network_already_holds_is_refused_with_one_fixed_reason() {
    let router = emulator();
    let network_id = create_network(&router, "taken-name-net").await;
    let group_id = default_group_id(&router, &network_id).await;
    let long_name = format!("{}bot", "n".repeat(61));

    // (case, the username, whether a BOT or a USER takes it first)
    //
    // Every row is a name a bot may hold — it follows the bot-naming convention — so what
    // decides each second create is the uniqueness rule and not the shape of the name.
    let rows: &[(&str, &str, bool)] = &[
        ("a short name a bot holds", "zzbot", true),
        ("a name a bot holds, hyphenated", "integration-bot", true),
        ("a dotted name a user holds", "ann.lee.bot", false),
        ("a mixed-case name a bot holds", "MixedCaseBot", true),
        ("a name of 64 characters", long_name.as_str(), true),
        ("a name built from another script", "ロボットbot", true),
        ("a name carrying a space", "two words bot", false),
        ("a name of digits and the suffix", "12345678bot", false),
    ];

    for (case, username, taken_by_a_bot) in rows {
        if *taken_by_a_bot {
            let (status, body) = create_bot(&router, &network_id, username, &group_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the first CreateBot must be served, got {status} with {body}"
            );
        } else {
            let body = create_user(&router, &network_id, username, &group_id).await;
            assert_eq!(
                body["successful"].as_array().map(Vec::len),
                Some(1),
                "case {case:?}: the first BatchCreateUser must create the user, got {body}"
            );
        }

        // The same name again, for a BOT this time whoever holds it: one username names one
        // account of the network, whether that account is a user or a bot.
        let (status, body) = create_bot(&router, &network_id, username, &group_id).await;
        assert_unavailable(case, status, &body, &network_id);
    }

    // The sentence is the same for every row, so it carries no part of the request, and it
    // is the one the service holds as its fixed text for this refusal.
    assert_eq!(
        USERNAME_UNAVAILABLE_REASON, EXPECTED_REASON,
        "the service's fixed reason must be the recorded sentence, got \
         {USERNAME_UNAVAILABLE_REASON:?}"
    );
    assert_eq!(
        USERNAME_FIELD, EXPECTED_FIELD,
        "the member such a refusal names must be the submitted one, got {USERNAME_FIELD:?}"
    );
}

/// The same name is free in another network, and the refused create stores nothing.
#[tokio::test]
async fn the_same_username_is_free_in_another_network() {
    let router = emulator();
    let first = create_network(&router, "first-name-net").await;
    let second = create_network(&router, "second-name-net").await;
    let first_group = default_group_id(&router, &first).await;
    let second_group = default_group_id(&router, &second).await;
    let username = "shared-bot";

    let (status, body) = create_bot(&router, &first, username, &first_group).await;
    assert_eq!(
        status, 200,
        "the first network's bot must be created, got {status} with {body}"
    );

    // The same name in the OTHER network names a different account and is created.
    let (status, body) = create_bot(&router, &second, username, &second_group).await;
    assert_eq!(
        status, 200,
        "case \"the same name in another network\": expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["username"].as_str(),
        Some(username),
        "case \"the same name in another network\": expected the name echoed back, got {body}"
    );

    // And in the first network it is refused, leaving that network with the one bot.
    let (status, body) = create_bot(&router, &first, username, &first_group).await;
    assert_unavailable("the name again in its own network", status, &body, &first);
    let (status, listing) = call(&router, "GET", &format!("/networks/{first}/bots"), None).await;
    assert_eq!(
        status, 200,
        "ListBots: expected 200, got {status} with {listing}"
    );
    assert_eq!(
        listing["bots"].as_array().map(Vec::len),
        Some(1),
        "the refused create must leave the one stored bot alone, got {listing}"
    );
}

/// The batch that creates users states the same sentence for the same refusal, in the
/// per-item failure its partial success reports.
///
/// The rows differ along the shape of the name, as above; each is first created and then
/// submitted again in a batch.
#[tokio::test]
async fn a_batch_refuses_a_taken_username_with_the_same_fixed_reason() {
    let router = emulator();
    let network_id = create_network(&router, "batch-name-net").await;
    let group_id = default_group_id(&router, &network_id).await;

    for (case, username) in [
        ("an address-shaped name", "bo.ng@example.com"),
        ("a name of two characters", "qq"),
        ("a name built from another script", "ユーザー"),
    ] {
        let body = create_user(&router, &network_id, username, &group_id).await;
        let created = body["successful"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the first create must succeed, got {body}"));

        let body = create_user(&router, &network_id, username, &group_id).await;
        assert_eq!(
            body["successful"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: the repeat must create nothing, got {body}"
        );
        let failed = body["failed"]
            .as_array()
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected one failed item, got {body}"));
        assert_eq!(
            failed["field"].as_str(),
            Some(EXPECTED_FIELD),
            "case {case:?}: expected the item to name {EXPECTED_FIELD}, got {failed}"
        );
        assert_eq!(
            failed["reason"].as_str(),
            Some(EXPECTED_REASON),
            "case {case:?}: expected the fixed reason {EXPECTED_REASON:?}, got {failed}"
        );
        assert_eq!(
            failed["userId"], created["userId"],
            "case {case:?}: the item must name the user that holds the name, got {failed}"
        );
    }
}
