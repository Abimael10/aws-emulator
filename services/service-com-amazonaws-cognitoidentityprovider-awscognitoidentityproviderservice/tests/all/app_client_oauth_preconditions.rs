// SPDX-License-Identifier: CC0-1.0

//! The app-client OAuth preconditions, over the wire, driven through the router
//! exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! Two rules are under test, each held by `CreateUserPoolClient` and
//! `UpdateUserPoolClient` alike, decided over the configuration the write would
//! LEAVE the app client with, and before anything is stored — so a refused create
//! stores no app client and a refused update leaves the stored one exactly as it
//! was.
//!
//! * An app client that turns the OAuth authorization server on carries both the
//!   grants it allows and the scopes it may authorize: a write that sets
//!   `AllowedOAuthFlowsUserPoolClient` while leaving either `AllowedOAuthFlows`
//!   or `AllowedOAuthScopes` empty is refused with `InvalidParameterException`.
//! * Every URL an app client would redirect a browser to is reached over HTTPS,
//!   with cleartext admitted for the loopback interface alone: a `CallbackURLs`,
//!   `LogoutURLs` or `DefaultRedirectURI` entry that is `http://` to any other
//!   host is refused with `InvalidParameterException`, naming that URL.
//!
//! Each table row names the case it asserts and every assertion reports the
//! actual value.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The message a write that turns the OAuth authorization server on without the
/// configuration it rests on is refused with.
const OAUTH_CONFIGURATION_REQUIRED_MESSAGE: &str =
    "AllowedOAuthFlows and AllowedOAuthScopes are required if user pool client is \
     allowed to use OAuth flows.";

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

/// Create a pool named `name` and return its minted id.
async fn pool(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, "CreateUserPool", &json!({ "PoolName": name })).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string()
}

/// The app clients of `user_pool_id`, as `ListUserPoolClients` renders them.
async fn clients(router: &MultiServiceRouter, user_pool_id: &str) -> Vec<Value> {
    ok(
        router,
        "ListUserPoolClients",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await["UserPoolClients"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The stored app client, as `DescribeUserPoolClient` renders it.
async fn described(router: &MultiServiceRouter, user_pool_id: &str, client_id: &str) -> Value {
    ok(
        router,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await["UserPoolClient"]
        .clone()
}

/// `members` as a `CreateUserPoolClient` body for `user_pool_id`.
fn create_body(user_pool_id: &str, client_name: &str, members: &Value) -> Value {
    let mut body = members
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the row's members are a JSON object: {members}"));
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientName".to_string(), json!(client_name));
    Value::Object(body)
}

/// `members` as an `UpdateUserPoolClient` body for the stored `client_id`.
fn update_body(user_pool_id: &str, client_id: &str, members: &Value) -> Value {
    let mut body = members
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the row's members are a JSON object: {members}"));
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientId".to_string(), json!(client_id));
    Value::Object(body)
}

/// Create one app client carrying a complete OAuth configuration, to apply every
/// update row to, and return its id.
async fn standing_client(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "standing",
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": ["https://app.example.com/cb"],
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string()
}

/// Drive `members` through the create and the update path, asserting that both
/// are refused with `message` and that neither wrote anything — or that both are
/// served, when `message` is `None`.
async fn assert_both_paths(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    standing_id: &str,
    case: &str,
    members: &Value,
    message: Option<&str>,
) {
    let before = clients(router, user_pool_id).await.len();
    let (status, payload) = call(
        router,
        "CreateUserPoolClient",
        &create_body(user_pool_id, "row-client", members),
    )
    .await;
    let after = clients(router, user_pool_id).await.len();

    let Some(message) = message else {
        assert_eq!(
            status, 200,
            "case {case:?}: the configuration is one an app client can hold, got {status} \
             with {payload}"
        );
        assert_eq!(
            after,
            before + 1,
            "case {case:?}: an accepted create adds one app client, got {after}"
        );
        let (status, payload) = call(
            router,
            "UpdateUserPoolClient",
            &update_body(user_pool_id, standing_id, members),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update owes the same answer, got {status} with {payload}"
        );
        return;
    };

    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, "InvalidParameterException", message),
        "case {case:?}: expected a 400 InvalidParameterException {message:?}, got {status} \
         with {payload}"
    );
    assert_eq!(
        after, before,
        "case {case:?}: a refused create stores no app client, got {after}"
    );

    let was = described(router, user_pool_id, standing_id).await;
    let (status, payload) = call(
        router,
        "UpdateUserPoolClient",
        &update_body(user_pool_id, standing_id, members),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, "InvalidParameterException", message),
        "case {case:?}: the update owes the same refusal, got {status} with {payload}"
    );
    let now = described(router, user_pool_id, standing_id).await;
    assert_eq!(
        now, was,
        "case {case:?}: a refused update must leave the app client alone, got {now}"
    );
}

#[tokio::test]
async fn an_oauth_app_client_carries_both_its_flows_and_its_scopes() {
    let router = router();
    let user_pool_id = pool(&router, "oauth-configuration-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    // The rows differ along which of the two members the write configures, along
    // whether each is absent or an empty list, and along whether the
    // authorization server is turned on at all.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "the server on, with flows and scopes",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code", "implicit"],
                "AllowedOAuthScopes": ["openid", "email"],
                // A redirect-based grant is delivered to a registered callback
                // URL, so a client that allows one carries at least one
                // (`CreateUserPoolClient.CallbackURLs`).
                "CallbackURLs": ["https://app.example.com/cb"],
            }),
            false,
        ),
        (
            "the server on, with flows and no scopes member",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code", "implicit"],
            }),
            true,
        ),
        (
            "the server on, with flows and an empty scope list",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": [],
            }),
            true,
        ),
        (
            "the server on, with scopes and no flows member",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthScopes": ["openid"],
            }),
            true,
        ),
        (
            "the server on, with scopes and an empty flow list",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": [],
                "AllowedOAuthScopes": ["openid"],
            }),
            true,
        ),
        (
            "the server on, with neither member",
            json!({ "AllowedOAuthFlowsUserPoolClient": true }),
            true,
        ),
        (
            "the server on, with the machine-to-machine grant and a standard scope",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["client_credentials"],
                "AllowedOAuthScopes": ["openid"],
            }),
            false,
        ),
        (
            "the server off, with flows and no scopes",
            json!({
                "AllowedOAuthFlowsUserPoolClient": false,
                "AllowedOAuthFlows": ["code"],
            }),
            false,
        ),
        (
            "the server off, with scopes and no flows",
            json!({
                "AllowedOAuthFlowsUserPoolClient": false,
                "AllowedOAuthScopes": ["openid"],
            }),
            false,
        ),
        (
            "no OAuth members at all, so the server is off by default",
            json!({}),
            false,
        ),
    ];

    for (case, members, refused) in rows {
        assert_both_paths(
            &router,
            &user_pool_id,
            &standing_id,
            case,
            &members,
            refused.then_some(OAUTH_CONFIGURATION_REQUIRED_MESSAGE),
        )
        .await;
    }
}

/// The URL members an app client registers for a browser redirect: the two lists
/// and the single URI
/// (`smithy_query(query="operation",operation="CreateUserPoolClient")`).
const REDIRECT_MEMBERS: [&str; 3] = ["CallbackURLs", "LogoutURLs", "DefaultRedirectURI"];

/// One URL row: the URL a redirect member carries, and whether the write is
/// refused for it.
struct UrlCase {
    case: &'static str,
    url: &'static str,
    refused: bool,
}

/// The rows differ along the scheme, the host and the port of the URL — the
/// loopback hosts the model names ("Amazon Cognito requires HTTPS over HTTP
/// except for callback URLs to `http://localhost`, `http://127.0.0.1` and
/// `http://[::1]`. ... You can specify custom TCP ports for your callback URLs",
/// `smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="CallbackURLs")`),
/// hosts that merely look like one, the application schemes the same member
/// admits, and the spelling of the scheme itself.
const URL_CASES: &[UrlCase] = &[
    UrlCase {
        case: "an HTTPS URL",
        url: "https://app.example.com/cb",
        refused: false,
    },
    UrlCase {
        case: "a cleartext URL to a named host",
        url: "http://vpc-endpoint.example.com/_dashboards/app/home",
        refused: true,
    },
    UrlCase {
        case: "a cleartext URL to a named host on a custom port",
        url: "http://app.example.com:8443/cb",
        refused: true,
    },
    UrlCase {
        case: "a cleartext URL whose scheme is spelled in capitals",
        url: "HTTP://app.example.com/cb",
        refused: true,
    },
    UrlCase {
        case: "a cleartext URL to localhost",
        url: "http://localhost/cb",
        refused: false,
    },
    UrlCase {
        case: "a cleartext URL to localhost on a custom port",
        url: "http://localhost:3000/cb",
        refused: false,
    },
    UrlCase {
        case: "a cleartext URL to the IPv4 loopback address",
        url: "http://127.0.0.1:8080/cb",
        refused: false,
    },
    UrlCase {
        case: "a cleartext URL to the IPv6 loopback address",
        url: "http://[::1]:8080/cb",
        refused: false,
    },
    UrlCase {
        case: "a cleartext URL to a host that merely starts with the loopback name",
        url: "http://localhost.attacker.example.com/cb",
        refused: true,
    },
    UrlCase {
        case: "a cleartext URL to an address outside the loopback block",
        url: "http://10.0.0.1/cb",
        refused: true,
    },
    UrlCase {
        case: "an HTTPS URL to a named host on a custom port",
        url: "https://app.example.com:8443/cb",
        refused: false,
    },
    UrlCase {
        case: "an application scheme",
        url: "myapp://example",
        refused: false,
    },
];

#[tokio::test]
async fn a_redirect_url_reaches_a_non_loopback_host_over_https_only() {
    let router = router();
    let user_pool_id = pool(&router, "redirect-url-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    for UrlCase { case, url, refused } in URL_CASES {
        for member in REDIRECT_MEMBERS {
            let mut members = Map::new();
            // The OAuth members a registered redirect URL belongs beside, so
            // every row carries a configuration the other rule admits.
            members.insert(
                "AllowedOAuthFlowsUserPoolClient".to_string(),
                json!(true),
            );
            members.insert("AllowedOAuthFlows".to_string(), json!(["code"]));
            members.insert("AllowedOAuthScopes".to_string(), json!(["openid"]));
            // A client allowing a redirect-based grant registers a callback URL,
            // so every row carries one the other rules admit; the row's own URL
            // replaces it where the member under test IS the callback list.
            members.insert(
                "CallbackURLs".to_string(),
                json!(["https://app.example.com/cb"]),
            );
            if member == "DefaultRedirectURI" {
                // "The default redirect URI ... must be in the CallbackURLs
                // list" (`DefaultRedirectURI` in this service's model).
                members.insert("CallbackURLs".to_string(), json!([url]));
                members.insert(member.to_string(), json!(url));
            } else {
                members.insert(member.to_string(), json!([url]));
            }
            let case = format!("{case} in {member}");
            assert_both_paths(
                &router,
                &user_pool_id,
                &standing_id,
                &case,
                &Value::Object(members),
                refused.then(|| format!("{url} cannot use the HTTP protocol.")).as_deref(),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn an_accepted_redirect_url_is_what_a_later_read_renders() {
    let router = router();
    let user_pool_id = pool(&router, "redirect-round-trip-pool").await;

    // The round trip: the loopback URLs an app client may register are stored as
    // submitted and read back unchanged, and a later update to a cleartext URL
    // leaves those stored values alone.
    let created = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "round-trip",
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": ["http://localhost:3000/cb", "https://app.example.com/cb"],
            "LogoutURLs": ["http://[::1]:3000/out"],
            "DefaultRedirectURI": "https://app.example.com/cb",
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string();
    let read = described(&router, &user_pool_id, &client_id).await;
    assert_eq!(
        (
            read["CallbackURLs"].clone(),
            read["LogoutURLs"].clone(),
            read["DefaultRedirectURI"].clone()
        ),
        (
            json!(["http://localhost:3000/cb", "https://app.example.com/cb"]),
            json!(["http://[::1]:3000/out"]),
            json!("https://app.example.com/cb")
        ),
        "case {:?}: the stored client must carry the URLs the create set, got {read}",
        "the loopback round trip"
    );

    let (status, payload) = call(
        &router,
        "UpdateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": ["http://vpc-endpoint.example.com/_dashboards/app/home"],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (
            400,
            "InvalidParameterException",
            "http://vpc-endpoint.example.com/_dashboards/app/home cannot use the HTTP protocol."
        ),
        "case {:?}: expected the cleartext refusal, got {status} with {payload}",
        "a cleartext update of a stored client"
    );
    let after = described(&router, &user_pool_id, &client_id).await;
    assert_eq!(
        after, read,
        "case {:?}: a refused update must leave the stored URLs alone, got {after}",
        "a cleartext update of a stored client"
    );

    // The HTTPS form of the same URL is accepted, and is what the read renders.
    let updated = ok(
        &router,
        "UpdateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": ["https://vpc-endpoint.example.com/_dashboards/app/home"],
        }),
    )
    .await;
    let stored = described(&router, &user_pool_id, &client_id).await;
    assert_eq!(
        (
            updated["UserPoolClient"]["CallbackURLs"].clone(),
            stored["CallbackURLs"].clone()
        ),
        (
            json!(["https://vpc-endpoint.example.com/_dashboards/app/home"]),
            json!(["https://vpc-endpoint.example.com/_dashboards/app/home"])
        ),
        "case {:?}: the HTTPS form is stored and read back, got {stored}",
        "the HTTPS form of the refused URL"
    );
}
