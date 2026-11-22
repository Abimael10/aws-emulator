// SPDX-License-Identifier: CC0-1.0

//! The threat-protection always-block address ranges, over the wire, driven
//! through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a pool whose threat protection takes automatic action
//! refuses an authentication request whose presented client address lies in a
//! range of the `RiskExceptionConfiguration.BlockedIPRangeList` that applies to
//! the request's app client, with `NotAuthorizedException` and before the
//! credential the request carries is evaluated at all; every other
//! authentication request is served. Three facts decide it — the pool's add-on
//! mode, the configuration level the ranges are stored at, and whether the
//! request's address reaches threat protection at all, which for an
//! app-client-facing call means an app client that propagates additional user
//! context data.
//!
//! Each table row names the case it asserts and every assertion reports the
//! actual value.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

// The `SECRET_HASH` a confidential app client requires of a request, read through the one
// definition of that tag this test target holds.
use super::confidential_client_sign_in::secret_hash;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The user every sign-in below is attempted for.
const USERNAME: &str = "blocked-range-user";

/// The feature plan a pool that carries `UserPoolAddOns` is put on: "to activate
/// this setting, your user pool must be on the Plus tier"
/// (`UserPoolAddOnsType` in this service's model), one of the modelled
/// `UserPoolTierType` values.
const THREAT_PROTECTION_TIER: &str = "PLUS";

/// The add-on mode in which the pool takes automatic action on what its threat
/// protection decides, and the one in which it only records it
/// (`smithy_query(query="shape",shape="AdvancedSecurityModeType")`:
/// OFF | AUDIT | ENFORCED).
const ENFORCED: &str = "ENFORCED";
const AUDIT: &str = "AUDIT";
const OFF: &str = "OFF";

/// The message an authentication request the pool blocks is refused with.
const BLOCKED_MESSAGE: &str = "Request not allowed due to security reasons.";

/// The message a request that sends a source address its app client may not
/// propagate is refused with.
const ADDRESS_FORBIDDEN_MESSAGE: &str = "Cannot use IP Address passed into UserContextData";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// A pool created in the add-on mode `mode` names — or with no `UserPoolAddOns`
/// member at all when `mode` is `None` — with one app client that allows both
/// password flows, and one confirmed user holding [`GOOD_PASSWORD`].
struct Fixture {
    user_pool_id: String,
    client_id: String,
    /// A second app client of the same pool, created with a secret and
    /// configured to propagate additional user context data, beside that secret.
    propagating_client_id: String,
    propagating_client_secret: String,
}

/// Create the fixture of a pool in `mode`.
async fn fixture(router: &MultiServiceRouter, name: &str, mode: Option<&str>) -> Fixture {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(name));
    if let Some(mode) = mode {
        body.insert(
            "UserPoolAddOns".to_string(),
            json!({ "AdvancedSecurityMode": mode }),
        );
        body.insert("UserPoolTier".to_string(), json!(THREAT_PROTECTION_TIER));
    }
    let created = ok(router, "CreateUserPool", &Value::Object(body)).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();

    let flows = json!(["ALLOW_USER_PASSWORD_AUTH", "ALLOW_ADMIN_USER_PASSWORD_AUTH"]);
    let plain = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "plain-client",
            "ExplicitAuthFlows": flows,
        }),
    )
    .await;
    let propagating = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "propagating-client",
            "ExplicitAuthFlows": flows,
            "GenerateSecret": true,
            "EnablePropagateAdditionalUserContextData": true,
        }),
    )
    .await;

    ok(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;

    Fixture {
        user_pool_id,
        client_id: plain["UserPoolClient"]["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("a minted client id: {plain}"))
            .to_string(),
        propagating_client_id: propagating["UserPoolClient"]["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("a minted client id: {propagating}"))
            .to_string(),
        propagating_client_secret: propagating["UserPoolClient"]["ClientSecret"]
            .as_str()
            .unwrap_or_else(|| panic!("a generated client secret: {propagating}"))
            .to_string(),
    }
}

/// Store `ranges` as the always-block list of the level `client_id` names — the
/// pool when it is `None`.
async fn block_ranges(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: Option<&str>,
    ranges: &[&str],
) {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    if let Some(client_id) = client_id {
        body.insert("ClientId".to_string(), json!(client_id));
    }
    body.insert(
        "RiskExceptionConfiguration".to_string(),
        json!({ "BlockedIPRangeList": ranges }),
    );
    ok(router, "SetRiskConfiguration", &Value::Object(body)).await;
}

/// An `AdminInitiateAuth` request for [`USERNAME`] with the correct password,
/// stating `ip_address` as the device's source address when one is given, and
/// carrying the `SECRET_HASH` of `client_secret` when the app client holds one
/// ("add a `SECRET_HASH` parameter if your app client has a client secret",
/// `AdminInitiateAuth.AuthParameters` in this service's model).
fn admin_initiate_auth(
    user_pool_id: &str,
    client_id: &str,
    client_secret: Option<&str>,
    ip_address: Option<&str>,
) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientId".to_string(), json!(client_id));
    body.insert("AuthFlow".to_string(), json!("ADMIN_USER_PASSWORD_AUTH"));
    let mut parameters = Map::new();
    parameters.insert("USERNAME".to_string(), json!(USERNAME));
    parameters.insert("PASSWORD".to_string(), json!(GOOD_PASSWORD));
    if let Some(secret) = client_secret {
        parameters.insert(
            "SECRET_HASH".to_string(),
            json!(secret_hash(secret, USERNAME, client_id)),
        );
    }
    body.insert("AuthParameters".to_string(), Value::Object(parameters));
    if let Some(ip_address) = ip_address {
        // Every member of `ContextDataType` but `EncodedData` is `@required`.
        body.insert(
            "ContextData".to_string(),
            json!({
                "IpAddress": ip_address,
                "ServerName": "app.example.com",
                "ServerPath": "/signin",
                "HttpHeaders": [],
            }),
        );
    }
    Value::Object(body)
}

/// The status this endpoint answers every refusal of the caller with, whatever
/// `@httpError` the error shape declares (`src/service/error_wire.rs`): the error
/// class travels in the body's `__type`.
const CLIENT_REFUSAL_STATUS: u16 = 400;

/// Whether a call's answer is the pool's block: `NotAuthorizedException` with the
/// one sentence such a refusal carries.
fn is_blocked(status: u16, payload: &Value) -> bool {
    status == CLIENT_REFUSAL_STATUS
        && error_code(payload) == "NotAuthorizedException"
        && error_message(payload) == BLOCKED_MESSAGE
}

/// Assert that `status`/`payload` is the block, or is not, as `blocked` says.
fn assert_block(case: &str, blocked: bool, status: u16, payload: &Value) {
    if blocked {
        assert!(
            is_blocked(status, payload),
            "case {case:?}: expected {CLIENT_REFUSAL_STATUS} NotAuthorizedException \
             {BLOCKED_MESSAGE:?}, got {status} with {payload}"
        );
    } else {
        assert_eq!(
            status, 200,
            "case {case:?}: the request is not blocked, so it must be served, got {status} \
             with {payload}"
        );
    }
}

/// One address-matching row: the ranges configured and the address a request
/// states, with whether the pool blocks that request.
struct RangeCase {
    case: &'static str,
    ranges: &'static [&'static str],
    address: Option<&'static str>,
    blocked: bool,
}

/// The rows differ along the form of the configured range (a network block in
/// either address family, a bare address, a host block, several ranges, none at
/// all), along where the stated address lies relative to it, and along whether
/// the request states an address at all.
const RANGE_CASES: &[RangeCase] = &[
    RangeCase {
        case: "an IPv4 block covering the stated address",
        ranges: &["203.0.113.0/24"],
        address: Some("203.0.113.77"),
        blocked: true,
    },
    RangeCase {
        case: "an IPv4 block the stated address lies outside",
        ranges: &["203.0.113.0/24"],
        address: Some("198.51.100.7"),
        blocked: false,
    },
    RangeCase {
        case: "the block's own network address",
        ranges: &["203.0.113.0/24"],
        address: Some("203.0.113.0"),
        blocked: true,
    },
    RangeCase {
        case: "the last address of the block",
        ranges: &["203.0.113.0/24"],
        address: Some("203.0.113.255"),
        blocked: true,
    },
    RangeCase {
        case: "the address one past the block",
        ranges: &["203.0.113.0/24"],
        address: Some("203.0.114.0"),
        blocked: false,
    },
    RangeCase {
        case: "a single address, spelled with no prefix length",
        ranges: &["198.51.100.7"],
        address: Some("198.51.100.7"),
        blocked: true,
    },
    RangeCase {
        case: "a host block, matched",
        ranges: &["198.51.100.7/32"],
        address: Some("198.51.100.7"),
        blocked: true,
    },
    RangeCase {
        case: "a host block, a neighbouring address",
        ranges: &["198.51.100.7/32"],
        address: Some("198.51.100.8"),
        blocked: false,
    },
    RangeCase {
        case: "an IPv6 block covering the stated address",
        ranges: &["2001:db8::/64"],
        address: Some("2001:db8::1"),
        blocked: true,
    },
    RangeCase {
        case: "an IPv6 block the stated address lies outside",
        ranges: &["2001:db8::/64"],
        address: Some("2001:db8:1::1"),
        blocked: false,
    },
    RangeCase {
        case: "the block that covers every IPv4 address",
        ranges: &["0.0.0.0/0"],
        address: Some("192.0.2.1"),
        blocked: true,
    },
    RangeCase {
        case: "the last of three configured ranges",
        ranges: &["10.0.0.0/8", "192.0.2.0/24", "203.0.113.0/24"],
        address: Some("203.0.113.5"),
        blocked: true,
    },
    RangeCase {
        case: "three configured ranges, none of them covering the address",
        ranges: &["10.0.0.0/8", "192.0.2.0/24", "203.0.113.0/24"],
        address: Some("172.16.0.1"),
        blocked: false,
    },
    RangeCase {
        case: "an empty always-block list",
        ranges: &[],
        address: Some("203.0.113.77"),
        blocked: false,
    },
    RangeCase {
        case: "a blocked range and a request that states no address",
        ranges: &["0.0.0.0/0", "::/0"],
        address: None,
        blocked: false,
    },
];

#[tokio::test]
async fn an_address_inside_a_blocked_range_is_refused_whatever_the_range_names() {
    let router = router();
    let fixture = fixture(&router, "blocked-range-pool", Some(ENFORCED)).await;

    for RangeCase {
        case,
        ranges,
        address,
        blocked,
    } in RANGE_CASES
    {
        block_ranges(&router, &fixture.user_pool_id, None, ranges).await;
        let (status, payload) = call(
            &router,
            "AdminInitiateAuth",
            &admin_initiate_auth(&fixture.user_pool_id, &fixture.client_id, None, *address),
        )
        .await;
        assert_block(case, *blocked, status, &payload);
    }
}

#[tokio::test]
async fn only_a_pool_that_acts_on_its_threat_protection_blocks_the_request() {
    // The same configured range and the same stated address at each add-on
    // state: the pool blocks the request only where it is configured to take
    // automatic action on what its threat protection decides.
    for (mode, blocked) in [
        (Some(ENFORCED), true),
        (Some(AUDIT), false),
        (Some(OFF), false),
        (None, false),
    ] {
        let case = format!("the add-on mode {mode:?}");
        let router = router();
        let fixture = fixture(&router, "add-on-mode-pool", mode).await;
        block_ranges(
            &router,
            &fixture.user_pool_id,
            None,
            &["203.0.113.0/24"],
        )
        .await;
        let (status, payload) = call(
            &router,
            "AdminInitiateAuth",
            &admin_initiate_auth(
                &fixture.user_pool_id,
                &fixture.client_id,
                None,
                Some("203.0.113.77"),
            ),
        )
        .await;
        assert_block(&case, blocked, status, &payload);
    }
}

/// What a call must be answered with, for a row that is not simply served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The sign-in is served.
    Served,
    /// The pool prevented the attempt ([`BLOCKED_MESSAGE`]).
    Blocked,
    /// The request sent a source address its app client may not propagate.
    AddressForbidden,
}

/// One propagation row: the app client the request is carried by, that client's
/// secret when it holds one, the address the request's `UserContextData` carries,
/// and the answer the pool owes it.
struct PropagationCase<'a> {
    case: &'a str,
    client_id: &'a str,
    client_secret: Option<&'a str>,
    address: Option<&'a str>,
    expected: Expected,
}

#[tokio::test]
async fn an_app_client_facing_request_presents_its_address_only_when_the_client_propagates_it() {
    let router = router();
    let fixture = fixture(&router, "propagation-pool", Some(ENFORCED)).await;
    block_ranges(
        &router,
        &fixture.user_pool_id,
        None,
        &["203.0.113.0/24"],
    )
    .await;

    // The rows differ along which app client carries the request and along what
    // the request says about the device's address.
    let secret = fixture.propagating_client_secret.as_str();
    let rows = [
        PropagationCase {
            case: "a propagating app client, an address inside the blocked range",
            client_id: &fixture.propagating_client_id,
            client_secret: Some(secret),
            address: Some("203.0.113.77"),
            expected: Expected::Blocked,
        },
        PropagationCase {
            case: "a propagating app client, an address outside it",
            client_id: &fixture.propagating_client_id,
            client_secret: Some(secret),
            address: Some("198.51.100.7"),
            expected: Expected::Served,
        },
        PropagationCase {
            case: "a propagating app client that sends no user context data",
            client_id: &fixture.propagating_client_id,
            client_secret: Some(secret),
            address: None,
            expected: Expected::Served,
        },
        PropagationCase {
            case: "an app client that does not propagate user context data",
            client_id: &fixture.client_id,
            client_secret: None,
            address: Some("203.0.113.77"),
            expected: Expected::AddressForbidden,
        },
        PropagationCase {
            case: "an app client that does not propagate it, sending a permitted address",
            client_id: &fixture.client_id,
            client_secret: None,
            address: Some("198.51.100.7"),
            expected: Expected::AddressForbidden,
        },
        PropagationCase {
            case: "an app client that does not propagate it, sending none",
            client_id: &fixture.client_id,
            client_secret: None,
            address: None,
            expected: Expected::Served,
        },
    ];

    for PropagationCase {
        case,
        client_id,
        client_secret,
        address,
        expected,
    } in rows
    {
        let mut parameters = Map::new();
        parameters.insert("USERNAME".to_string(), json!(USERNAME));
        parameters.insert("PASSWORD".to_string(), json!(GOOD_PASSWORD));
        if let Some(secret) = client_secret {
            parameters.insert(
                "SECRET_HASH".to_string(),
                json!(secret_hash(secret, USERNAME, client_id)),
            );
        }
        let mut body = Map::new();
        body.insert("ClientId".to_string(), json!(client_id));
        body.insert("AuthFlow".to_string(), json!("USER_PASSWORD_AUTH"));
        body.insert("AuthParameters".to_string(), Value::Object(parameters));
        if let Some(address) = address {
            body.insert(
                "UserContextData".to_string(),
                json!({ "IpAddress": address }),
            );
        }
        let (status, payload) = call(&router, "InitiateAuth", &Value::Object(body)).await;
        match expected {
            Expected::Served => assert_block(case, false, status, &payload),
            Expected::Blocked => assert_block(case, true, status, &payload),
            Expected::AddressForbidden => assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (
                    CLIENT_REFUSAL_STATUS,
                    "InvalidParameterException",
                    ADDRESS_FORBIDDEN_MESSAGE
                ),
                "case {case:?}: expected the refusal of an address this app client may \
                 not propagate, got {status} with {payload}"
            ),
        }
    }
}

#[tokio::test]
async fn the_ranges_that_apply_are_the_ones_stored_at_the_app_clients_own_level() {
    let router = router();
    let fixture = fixture(&router, "configuration-level-pool", Some(ENFORCED)).await;
    // The pool blocks one range; one of its app clients stores exceptions of its
    // own, which block another.
    block_ranges(
        &router,
        &fixture.user_pool_id,
        None,
        &["203.0.113.0/24"],
    )
    .await;
    block_ranges(
        &router,
        &fixture.user_pool_id,
        Some(&fixture.client_id),
        &["198.51.100.0/24"],
    )
    .await;

    // The rows differ along which level configured the app client the request
    // carries, and along which of the two ranges the stated address lies in. The
    // second app client holds a secret, so its rows carry that secret's
    // `SECRET_HASH`.
    for (case, client_id, client_secret, address, blocked) in [
        (
            "the app client's own range, on the client that configured one",
            &fixture.client_id,
            None,
            "198.51.100.7",
            true,
        ),
        (
            "the pool's range, on the client that configured its own",
            &fixture.client_id,
            None,
            "203.0.113.77",
            false,
        ),
        (
            "the pool's range, on a client that configured none",
            &fixture.propagating_client_id,
            Some(fixture.propagating_client_secret.as_str()),
            "203.0.113.77",
            true,
        ),
        (
            "the other client's range, on a client that configured none",
            &fixture.propagating_client_id,
            Some(fixture.propagating_client_secret.as_str()),
            "198.51.100.7",
            false,
        ),
    ] {
        let (status, payload) = call(
            &router,
            "AdminInitiateAuth",
            &admin_initiate_auth(
                &fixture.user_pool_id,
                client_id,
                client_secret,
                Some(address),
            ),
        )
        .await;
        assert_block(case, blocked, status, &payload);
    }
}

#[tokio::test]
async fn the_block_is_decided_before_the_credential_the_request_carries() {
    let router = router();
    let fixture = fixture(&router, "precedence-pool", Some(ENFORCED)).await;
    block_ranges(
        &router,
        &fixture.user_pool_id,
        None,
        &["203.0.113.0/24"],
    )
    .await;

    // A WRONG password from a blocked address is answered with the block rather
    // than with the refusal a bad credential owes, so nothing about the
    // credential was evaluated.
    let mut wrong_password =
        admin_initiate_auth(
            &fixture.user_pool_id,
            &fixture.client_id,
            None,
            Some("203.0.113.77"),
        );
    wrong_password["AuthParameters"]["PASSWORD"] = json!("not-the-password");
    let (status, payload) = call(&router, "AdminInitiateAuth", &wrong_password).await;
    assert_block(
        "a wrong password from a blocked address",
        true,
        status,
        &payload,
    );

    // A challenge response from a blocked address is answered the same way,
    // before the session it carries is resolved.
    let (status, payload) = call(
        &router,
        "AdminRespondToAuthChallenge",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "ClientId": fixture.client_id,
            "ChallengeName": "NEW_PASSWORD_REQUIRED",
            "Session": "no-such-session-token-at-all",
            "ChallengeResponses": { "USERNAME": USERNAME, "NEW_PASSWORD": GOOD_PASSWORD },
            "ContextData": {
                "IpAddress": "203.0.113.77",
                "ServerName": "app.example.com",
                "ServerPath": "/signin",
                "HttpHeaders": [],
            },
        }),
    )
    .await;
    assert_block(
        "a challenge response from a blocked address",
        true,
        status,
        &payload,
    );

    // The block moved no stored state: the same user signs in with the same
    // password from an address the list does not cover.
    let (status, payload) = call(
        &router,
        "AdminInitiateAuth",
        &admin_initiate_auth(
            &fixture.user_pool_id,
            &fixture.client_id,
            None,
            Some("198.51.100.7"),
        ),
    )
    .await;
    assert_block(
        "the same credential from a permitted address, after the blocks",
        false,
        status,
        &payload,
    );
    assert!(
        payload["AuthenticationResult"]["AccessToken"].is_string(),
        "case {:?}: the served sign-in issues tokens, got {payload}",
        "the same credential from a permitted address, after the blocks"
    );
}

/// The newest logged authentication event of [`USERNAME`], as
/// `AdminListUserAuthEvents` renders it — the reader serves the most recent
/// first.
async fn newest_auth_event(router: &MultiServiceRouter, user_pool_id: &str) -> Value {
    let history = ok(
        router,
        "AdminListUserAuthEvents",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    history["AuthEvents"]
        .as_array()
        .and_then(|events| events.first())
        .cloned()
        .unwrap_or_else(|| panic!("the user's event history holds an event: {history}"))
}

#[tokio::test]
async fn a_prevented_attempt_is_logged_with_the_pools_block_decision() {
    let router = router();
    let fixture = fixture(&router, "risk-decision-pool", Some(ENFORCED)).await;
    block_ranges(
        &router,
        &fixture.user_pool_id,
        None,
        &["203.0.113.0/24"],
    )
    .await;

    // The rows differ along whether the pool prevented the attempt, which is
    // what the event it logs reports as the pool's decision: "if `Block`, your
    // user pool prevented the attempt. If `NoRisk`, your user pool took no
    // action" (`smithy_query(query="shape",shape="EventRiskType")`).
    for (case, address, decision, response) in [
        (
            "an attempt from a blocked address",
            "203.0.113.77",
            "Block",
            "Fail",
        ),
        (
            "an attempt from a permitted address",
            "198.51.100.7",
            "NoRisk",
            "Pass",
        ),
    ] {
        call(
            &router,
            "AdminInitiateAuth",
            &admin_initiate_auth(&fixture.user_pool_id, &fixture.client_id, None, Some(address)),
        )
        .await;
        let event = newest_auth_event(&router, &fixture.user_pool_id).await;
        assert_eq!(
            (
                event["EventRisk"]["RiskDecision"].clone(),
                event["EventResponse"].clone(),
                event["EventContextData"]["IpAddress"].clone()
            ),
            (json!(decision), json!(response), json!(address)),
            "case {case:?}: expected the decision {decision:?} on a {response:?} event \
             from {address}, got {event}"
        );
    }
}

/// One operation that carries `UserContextData`, and a request body for it that
/// names [`USERNAME`] and the app client `client_id`.
fn user_context_request(operation: &str, client_id: &str, address: &str) -> Value {
    let mut body = Map::new();
    body.insert("ClientId".to_string(), json!(client_id));
    body.insert(
        "UserContextData".to_string(),
        json!({ "IpAddress": address }),
    );
    match operation {
        "InitiateAuth" => {
            body.insert("AuthFlow".to_string(), json!("USER_PASSWORD_AUTH"));
            body.insert(
                "AuthParameters".to_string(),
                json!({ "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD }),
            );
        }
        "RespondToAuthChallenge" => {
            body.insert("ChallengeName".to_string(), json!("NEW_PASSWORD_REQUIRED"));
            // `SessionType`'s modelled length is 20..2048, so a session shorter
            // than that is refused by the boundary validator rather than by the
            // handler.
            body.insert(
                "Session".to_string(),
                json!("no-such-session-token-at-all"),
            );
            body.insert(
                "ChallengeResponses".to_string(),
                json!({ "USERNAME": USERNAME, "NEW_PASSWORD": GOOD_PASSWORD }),
            );
        }
        "SignUp" => {
            body.insert("Username".to_string(), json!("another-user"));
            body.insert("Password".to_string(), json!(GOOD_PASSWORD));
        }
        "ConfirmSignUp" => {
            body.insert("Username".to_string(), json!(USERNAME));
            body.insert("ConfirmationCode".to_string(), json!("123456"));
        }
        "ConfirmForgotPassword" => {
            body.insert("Username".to_string(), json!(USERNAME));
            body.insert("ConfirmationCode".to_string(), json!("123456"));
            body.insert("Password".to_string(), json!(GOOD_PASSWORD));
        }
        _ => {
            body.insert("Username".to_string(), json!(USERNAME));
        }
    }
    Value::Object(body)
}

#[tokio::test]
async fn only_a_propagating_app_client_may_send_a_source_address() {
    let router = router();
    // The refusal rests on the app client's configuration alone, so the pool
    // here carries no threat-protection add-ons at all.
    let fixture = fixture(&router, "address-member-pool", None).await;

    // Every operation whose input models `UserContextData`
    // (`smithy_query(query="digest")`), each driven through an app client that
    // propagates additional user context data and one that does not.
    for operation in [
        "InitiateAuth",
        "RespondToAuthChallenge",
        "SignUp",
        "ConfirmSignUp",
        "ForgotPassword",
        "ConfirmForgotPassword",
        "ResendConfirmationCode",
    ] {
        let (status, payload) = call(
            &router,
            operation,
            &user_context_request(operation, &fixture.client_id, "203.0.113.77"),
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (
                CLIENT_REFUSAL_STATUS,
                "InvalidParameterException",
                ADDRESS_FORBIDDEN_MESSAGE
            ),
            "case {operation:?} on a non-propagating app client: expected the refusal of \
             an address it may not propagate, got {status} with {payload}"
        );

        let (status, payload) = call(
            &router,
            operation,
            &user_context_request(operation, &fixture.propagating_client_id, "203.0.113.77"),
        )
        .await;
        assert_ne!(
            (error_code(&payload), error_message(&payload)),
            ("InvalidParameterException", ADDRESS_FORBIDDEN_MESSAGE),
            "case {operation:?} on a propagating app client: the address is one it may \
             send, got {status} with {payload}"
        );
    }
}
