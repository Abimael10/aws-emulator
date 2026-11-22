// SPDX-License-Identifier: CC0-1.0

//! The app client's own grant bounds what the machine-to-machine token may authorize.
//!
//! The rule under test. `AllowedOAuthScopes` is "the OAuth, OpenID Connect (OIDC), and
//! custom scopes that you want to permit your app client to authorize access with"
//! (`CreateUserPoolClient` in this service's model), and `GetClientToken` "issues an
//! access token for machine-to-machine (M2M) authorization ... that authorizes requests
//! to your resource servers" whose `Scopes` are "the custom scopes to authorize in the
//! access token ... If you don't specify any scopes, Amazon Cognito authorizes the scopes
//! that are configured for the app client". So the app client's configured scopes are the
//! whole of what it may be authorized for: a request naming a scope outside that grant is
//! refused with `NotAuthorizedException` and no token at all — not served a token narrowed
//! to the granted part — whichever resource server of the pool defines the scope.
//!
//! The rows span what the rule generalises over: how many scopes are asked for (one,
//! several) and how many of them are granted (all, some, none), whether the app client is
//! granted a subset of its pool's scopes or nothing at all, which of two resource servers
//! defines the scope, a resource server identifier that carries the `/` separator itself,
//! and the order the granted scopes are asked for in. A scope NO resource server of the
//! pool defines is a different refusal, which `get_client_token.rs` and
//! `authentication_refusal_classes.rs` pin, so the rows here keep every named scope one
//! the pool defines.
//!
//! Each table row names the case it asserts.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// This service's SigV4 signing name, the 4th field of a credential scope.
const SIGNING_NAME: &str = "cognito-idp";

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// This service's real regional endpoint host.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The region the suite's calls are signed for.
const DEFAULT_REGION: &str = "us-east-1";

/// The operation under test.
const GRANT: &str = "GetClientToken";

/// The `ExplicitAuthFlows` value that configures an app client for machine-to-machine
/// authorization, which this operation's own doc names.
const CLIENT_TOKEN_AUTH_FLOW: &str = "ALLOW_CLIENT_TOKEN_AUTH";

/// A `ClientSecretType`-shaped secret (pattern `^[\w+]+$`, length 24..64) the suite's app
/// clients are created with, so a request can present the right one.
const CLIENT_SECRET: &str = "m2msecret00000000000000001";

/// The sentence a request naming a scope its app client was never granted is refused
/// with: fixed text, which names neither the app client nor the scope.
const SCOPES_NOT_GRANTED: &str = "Client is not allowed to request these scopes";

/// The two resource servers the suite's pool defines, each with its scope names. The
/// second identifier carries the `/` that separates an identifier from a scope name, so a
/// row can ask for a scope whose text holds two of them.
const RESOURCE_SERVERS: [(&str, [&str; 2]); 2] = [
    ("https://api.example.test", ["read", "write"]),
    ("https://api.example.test/v2", ["read", "admin"]),
];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router
}

/// Dispatch `operation` with `body` and return the status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", ENDPOINT_HOST)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{DEFAULT_REGION}/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        )
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
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// The decoded claims of the compact JWS `token`: the payload segment, base64url as
/// RFC 7515 §2 leaves it, decoded by the one JOSE codec this workspace holds
/// (`service_common::jwt`).
fn claims(case: &str, token: &str) -> Value {
    let segment = token
        .split('.')
        .nth(1)
        .unwrap_or_else(|| panic!("case {case:?}: a compact JWS carries three segments: {token:?}"));
    let bytes = service_common::jwt::decode_base64url(segment)
        .unwrap_or_else(|| panic!("case {case:?}: {segment:?} must be base64url"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: the claims must be JSON ({error})"))
}

/// Create the pool with [`RESOURCE_SERVERS`] defined on it and return its id.
async fn pool(router: &MultiServiceRouter, pool_name: &str) -> String {
    let created = ok(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    for (at, (identifier, scope_names)) in RESOURCE_SERVERS.iter().enumerate() {
        let scopes: Vec<Value> = scope_names
            .iter()
            .map(|name| json!({ "ScopeName": name, "ScopeDescription": format!("the {name} scope") }))
            .collect();
        ok(
            router,
            "CreateResourceServer",
            &json!({
                "UserPoolId": user_pool_id,
                "Identifier": identifier,
                // `ResourceServerNameType`'s pattern is `^[\w\s+=,.@-]+$`, which an
                // identifier's own characters need not satisfy.
                "Name": format!("resource server {}", at + 1),
                "Scopes": scopes,
            }),
        )
        .await;
    }
    user_pool_id
}

/// Create a machine-to-machine app client of `user_pool_id` permitted `granted`, and
/// return its minted `ClientId`.
async fn m2m_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_name: &str,
    granted: &[&str],
) -> String {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientName".to_string(), json!(client_name));
    body.insert("ClientSecret".to_string(), json!(CLIENT_SECRET));
    body.insert(
        "ExplicitAuthFlows".to_string(),
        json!([CLIENT_TOKEN_AUTH_FLOW]),
    );
    body.insert("AllowedOAuthScopes".to_string(), json!(granted));
    // "AllowedOAuthFlows and AllowedOAuthScopes are required if user pool client is
    // allowed to use OAuth flows", so the authorization server is turned on only on a
    // client that is permitted a scope for it to authorize.
    if !granted.is_empty() {
        body.insert("AllowedOAuthFlows".to_string(), json!(["client_credentials"]));
        body.insert("AllowedOAuthFlowsUserPoolClient".to_string(), json!(true));
    }
    let created = ok(router, "CreateUserPoolClient", &Value::Object(body)).await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string()
}

/// One row of the grant table: what the app client is permitted, what the request asks
/// for, and the scopes the token must authorize — or `None` for a row the grant refuses.
struct ScopeCase {
    case: &'static str,
    /// The app client's `AllowedOAuthScopes`.
    granted: &'static [&'static str],
    /// The `Scopes` the request names, and `None` for a request that sends no `Scopes`
    /// member at all.
    requested: Option<&'static [&'static str]>,
    /// The scopes the issued token must authorize in order, or `None` when the request is
    /// refused and no token is issued.
    authorized: Option<&'static [&'static str]>,
}

/// A request is served exactly the scopes the app client's own grant permits, and refused
/// outright when it names one the grant does not.
#[tokio::test]
async fn the_grant_serves_only_the_scopes_the_app_client_was_permitted() {
    let router = router();
    let user_pool_id = pool(&router, "scope-grant-pool").await;

    let cases = [
        ScopeCase {
            case: "the one scope the client is permitted",
            granted: &["https://api.example.test/read"],
            requested: Some(&["https://api.example.test/read"]),
            authorized: Some(&["https://api.example.test/read"]),
        },
        ScopeCase {
            case: "another scope of the same resource server, which the client is not \
                   permitted",
            granted: &["https://api.example.test/read"],
            requested: Some(&["https://api.example.test/write"]),
            authorized: None,
        },
        ScopeCase {
            case: "a scope of the other resource server, which the client is not permitted",
            granted: &["https://api.example.test/read"],
            requested: Some(&["https://api.example.test/v2/admin"]),
            authorized: None,
        },
        ScopeCase {
            case: "every permitted scope at once, in the order asked for",
            granted: &[
                "https://api.example.test/read",
                "https://api.example.test/v2/read",
            ],
            requested: Some(&[
                "https://api.example.test/v2/read",
                "https://api.example.test/read",
            ]),
            authorized: Some(&[
                "https://api.example.test/v2/read",
                "https://api.example.test/read",
            ]),
        },
        ScopeCase {
            case: "one permitted scope beside one the client is not permitted",
            granted: &["https://api.example.test/read"],
            requested: Some(&[
                "https://api.example.test/read",
                "https://api.example.test/write",
            ]),
            authorized: None,
        },
        ScopeCase {
            case: "a subset of what the client is permitted",
            granted: &[
                "https://api.example.test/read",
                "https://api.example.test/write",
            ],
            requested: Some(&["https://api.example.test/write"]),
            authorized: Some(&["https://api.example.test/write"]),
        },
        ScopeCase {
            case: "a scope of a resource server whose identifier carries the separator",
            granted: &["https://api.example.test/v2/admin"],
            requested: Some(&["https://api.example.test/v2/admin"]),
            authorized: Some(&["https://api.example.test/v2/admin"]),
        },
        ScopeCase {
            case: "an app client permitted no scope at all, asking for one the pool defines",
            granted: &[],
            requested: Some(&["https://api.example.test/read"]),
            authorized: None,
        },
        ScopeCase {
            case: "no Scopes member, which falls back to the whole of the client's grant",
            granted: &[
                "https://api.example.test/read",
                "https://api.example.test/v2/read",
            ],
            requested: None,
            // Read back in the order the pool holds a permitted scope SET in.
            authorized: Some(&[
                "https://api.example.test/read",
                "https://api.example.test/v2/read",
            ]),
        },
        ScopeCase {
            case: "an app client permitted no scope at all, asking for none",
            granted: &[],
            requested: None,
            authorized: Some(&[]),
        },
    ];

    for (at, row) in cases.iter().enumerate() {
        let case = row.case;
        let client_id =
            m2m_client(&router, &user_pool_id, &format!("m2m-{at}"), row.granted).await;
        let mut body = Map::new();
        body.insert("ClientId".to_string(), json!(client_id));
        body.insert("Secret".to_string(), json!(CLIENT_SECRET));
        if let Some(requested) = row.requested {
            body.insert("Scopes".to_string(), json!(requested));
        }
        let (status, payload) = call(&router, GRANT, &Value::Object(body)).await;

        let Some(authorized) = row.authorized else {
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, "NotAuthorizedException", SCOPES_NOT_GRANTED),
                "case {case:?}: an ungranted scope must refuse the whole request, got \
                 {status} with {payload}"
            );
            continue;
        };
        assert_eq!(
            status, 200,
            "case {case:?}: a request within the client's grant must be served, got \
             {status} with {payload}"
        );
        let access_token = payload["ClientAuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: an issued access token, got {payload}"));
        let claims = claims(case, access_token);
        assert_eq!(
            claims["scope"],
            json!(authorized.join(" ")),
            "case {case:?}: the token must authorize exactly {authorized:?}, got {claims}"
        );
    }
}

/// The refusal writes nothing: an app client refused an ungranted scope still serves the
/// scope it was permitted, and the grant it is permitted is what a read of the app client
/// still reports.
#[tokio::test]
async fn a_refused_scope_leaves_the_app_clients_grant_as_it_was() {
    let router = router();
    let user_pool_id = pool(&router, "scope-grant-persistence-pool").await;
    let granted = ["https://api.example.test/read"];
    let client_id = m2m_client(&router, &user_pool_id, "m2m-persistent", &granted).await;

    let refusal = call(
        &router,
        GRANT,
        &json!({
            "ClientId": client_id,
            "Secret": CLIENT_SECRET,
            "Scopes": ["https://api.example.test/write"],
        }),
    )
    .await;
    assert_eq!(
        (refusal.0, error_code(&refusal.1)),
        (400, "NotAuthorizedException"),
        "an ungranted scope is refused, got {} with {}",
        refusal.0,
        refusal.1
    );

    let served = ok(
        &router,
        GRANT,
        &json!({
            "ClientId": client_id,
            "Secret": CLIENT_SECRET,
            "Scopes": granted,
        }),
    )
    .await;
    let access_token = served["ClientAuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("an issued access token, got {served}"));
    assert_eq!(
        claims("after the refusal", access_token)["scope"],
        json!(granted.join(" ")),
        "the refusal must not change what the client's grant authorizes, got {served}"
    );

    let described = ok(
        &router,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await;
    assert_eq!(
        described["UserPoolClient"]["AllowedOAuthScopes"],
        json!(granted),
        "the refusal must not touch the app client's own configuration, got {described}"
    );
}
