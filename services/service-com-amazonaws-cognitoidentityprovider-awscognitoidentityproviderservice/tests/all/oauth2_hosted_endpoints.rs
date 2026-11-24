// SPDX-License-Identifier: CC0-1.0

//! The OAuth2 endpoints of a pool's DOMAIN, over the wire: the token endpoint, the user-info
//! endpoint and the revocation endpoint of its hosted authorization server.
//!
//! RULE under test: **a pool that holds a domain serves that domain's OAuth2 endpoints at the
//! address its own discovery document announces** — so the document a relying party bootstraps
//! from names an address on THIS emulator (the endpoint the document was fetched on, with the
//! domain as the first path segment), and each endpoint named there answers the OAuth2 wire:
//!
//! * `POST .../oauth2/token` with `grant_type=refresh_token` answers a new access and id token
//!   for the sign-in the presented refresh token names and NO new refresh token, and with
//!   `grant_type=client_credentials` answers the app client's own machine-to-machine token;
//! * `GET .../oauth2/userInfo` with that access token as a bearer credential answers the
//!   user's claims, and refuses a credential it does not accept `invalid_token`;
//! * `POST .../oauth2/revoke` revokes the sign-in the presented refresh token belongs to, after
//!   which that token grants nothing.
//!
//! The rows differ along what each rule generalises over: for the document, whether the pool
//! holds a domain at all and the endpoint it is fetched on; for the grants, the grant named and
//! the field left out; for the refusals, which half of the credential is wrong.
//!
//! Each row names the case it asserts and every assertion reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes the modelled calls of this suite to the
/// service. The signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The published regional endpoint host of the region this suite signs for.
const PUBLISHED_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// A local address the emulator is reached at, which serves the same pools: an operator binds
/// the emulator wherever they like, and a pool answers on the endpoint each caller used.
const LOCAL_HOST: &str = "127.0.0.1:9297";

/// The password every fixture user holds.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The media type an OAuth2 request's form-encoded body travels in.
const FORM_MEDIA_TYPE: &str = "application/x-www-form-urlencoded";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    router.set_request_authentication(false);
    router
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", PUBLISHED_HOST)
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
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or_else(|error| {
        panic!(
            "{operation} answered a body that is not JSON ({error}): {:?}",
            String::from_utf8_lossy(response.body())
        )
    });
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// What one request to an OAuth2 endpoint answered.
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answered {
    /// The answer's JSON document, or `Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "an OAuth2 endpoint answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// The `error` code the answer carries, or `<none>` when it carries none.
    fn error(&self) -> String {
        self.json()
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("<none>")
            .to_string()
    }

    /// The one value `name` carries, or `None` when the answer states no such header.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(stated, _)| stated.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

/// Send one request to an OAuth2 endpoint of this emulator: `method path` on `host`, with
/// `headers` and `body`.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answered {
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    Answered {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: response.body().to_vec(),
    }
}

/// POST a form to an OAuth2 endpoint.
async fn post_form(
    router: &MultiServiceRouter,
    path: &str,
    headers: &[(&str, &str)],
    fields: &[(&str, &str)],
) -> Answered {
    let body: String = fields
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                ::url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
                ::url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<String>>()
        .join("&");
    let mut all: Vec<(&str, &str)> = vec![("content-type", FORM_MEDIA_TYPE)];
    all.extend_from_slice(headers);
    send(router, "POST", LOCAL_HOST, path, &all, &body).await
}

/// A pool with a domain, an app client and a signed-in user.
struct Hosted {
    user_pool_id: String,
    domain: String,
    client_id: String,
    refresh_token: String,
    username: String,
}

/// The pool's OpenID Connect discovery document, as served on `host`.
async fn discovery(router: &MultiServiceRouter, host: &str, user_pool_id: &str) -> Value {
    let answered = send(
        router,
        "GET",
        host,
        &format!("/{user_pool_id}/.well-known/openid-configuration"),
        &[],
        "",
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "the discovery document must be served, got {} with {:?}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    answered.json()
}

/// Create a pool with `domain`, one public app client that signs in with a password and
/// refreshes, and one confirmed user signed in through it.
async fn hosted_pool(router: &MultiServiceRouter, name: &str, domain: &str) -> Hosted {
    let user_pool_id = ok(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]
        ["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client_id = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "hosted-client",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    ok(
        router,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": user_pool_id, "Domain": domain }),
    )
    .await;
    let username = "alice".to_string();
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "email", "Value": "alice@example.com" },
                { "Name": "email_verified", "Value": "true" },
            ],
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let signed_in = ok(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    let refresh_token = signed_in["AuthenticationResult"]["RefreshToken"]
        .as_str()
        .expect("a sign-in carries a refresh token")
        .to_string();
    Hosted {
        user_pool_id,
        domain: domain.to_string(),
        client_id,
        refresh_token,
        username,
    }
}

/// A pool that holds a domain announces its hosted endpoints on the address this emulator
/// serves them at; a pool that holds none has no such server and names its own issuer.
#[tokio::test]
async fn the_discovery_document_announces_the_endpoints_this_emulator_serves() {
    let router = router();
    let hosted = hosted_pool(&router, "oauth2-discovery-pool", "oauth2-discovery").await;
    let plain = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "oauth2-domainless-pool" }),
    )
    .await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();

    // The rows differ along the endpoint the document is fetched on: a pool answers on every
    // address its callers use, and the document names THAT address.
    for host in [PUBLISHED_HOST, LOCAL_HOST] {
        let document = discovery(&router, host, &hosted.user_pool_id).await;
        let issuer = document["issuer"]
            .as_str()
            .expect("the document names an issuer")
            .to_string();
        let origin = issuer
            .rsplit_once('/')
            .map(|(origin, _)| origin.to_string())
            .expect("the issuer carries the pool as its path");
        let base = format!("{origin}/{}", hosted.domain);
        for (member, expected) in [
            ("authorization_endpoint", format!("{base}/oauth2/authorize")),
            ("token_endpoint", format!("{base}/oauth2/token")),
            ("userinfo_endpoint", format!("{base}/oauth2/userInfo")),
            ("revocation_endpoint", format!("{base}/oauth2/revoke")),
            ("end_session_endpoint", format!("{base}/logout")),
        ] {
            assert_eq!(
                document[member].as_str(),
                Some(expected.as_str()),
                "case {host:?}: expected {member} to be announced as {expected:?}, got {:?}",
                document[member]
            );
        }
    }

    // A pool with no domain hosts no authorization server, so the document names the
    // locators it does have, under its own issuer.
    let document = discovery(&router, PUBLISHED_HOST, &plain).await;
    let issuer = document["issuer"]
        .as_str()
        .expect("the document names an issuer")
        .to_string();
    for (member, expected) in [
        ("token_endpoint", format!("{issuer}/token")),
        ("userinfo_endpoint", format!("{issuer}/userInfo")),
        ("revocation_endpoint", format!("{issuer}/revoke")),
    ] {
        assert_eq!(
            document[member].as_str(),
            Some(expected.as_str()),
            "case \"a pool with no domain\": expected {member} to be {expected:?}, got {:?}",
            document[member]
        );
    }
}

/// The whole hosted round trip: the refresh grant answers tokens without a new refresh token,
/// the access token it answers reads the user's claims at the user-info endpoint, and the
/// revocation endpoint ends the sign-in those tokens belong to.
#[tokio::test]
async fn the_hosted_endpoints_refresh_read_and_revoke_a_sign_in() {
    let router = router();
    let hosted = hosted_pool(&router, "oauth2-round-trip-pool", "oauth2-round-trip").await;
    let token_path = format!("/{}/oauth2/token", hosted.domain);
    let user_info_path = format!("/{}/oauth2/userInfo", hosted.domain);
    let revoke_path = format!("/{}/oauth2/revoke", hosted.domain);

    let refreshed = post_form(
        &router,
        &token_path,
        &[],
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &hosted.client_id),
            ("refresh_token", &hosted.refresh_token),
        ],
    )
    .await;
    assert_eq!(
        refreshed.status,
        200,
        "the refresh grant must be served, got {} with {}",
        refreshed.status,
        refreshed.json()
    );
    let granted = refreshed.json();
    assert_eq!(
        granted["token_type"].as_str(),
        Some("Bearer"),
        "the grant must state the type its token is presented as, got {granted}"
    );
    assert!(
        granted["expires_in"].as_i64().is_some_and(|seconds| seconds > 0),
        "the grant must state how long its access token lives, got {granted}"
    );
    assert_eq!(
        granted.get("refresh_token"),
        None,
        "a refresh grant issues no new refresh token, got {granted}"
    );
    let access_token = granted["access_token"]
        .as_str()
        .unwrap_or_else(|| panic!("the grant must answer an access token, got {granted}"))
        .to_string();
    assert!(
        granted["id_token"].as_str().is_some(),
        "the grant must answer an id token, got {granted}"
    );

    // The access token the grant answered reads the user's claims, and the claims are the
    // stored user's.
    let bearer = format!("Bearer {access_token}");
    for method in ["GET", "POST"] {
        let claims = send(
            &router,
            method,
            LOCAL_HOST,
            &user_info_path,
            &[("authorization", bearer.as_str())],
            "",
        )
        .await;
        assert_eq!(
            claims.status,
            200,
            "case {method:?}: the user-info endpoint must be served, got {} with {}",
            claims.status,
            claims.json()
        );
        let document = claims.json();
        assert_eq!(
            document["username"].as_str(),
            Some(hosted.username.as_str()),
            "case {method:?}: the claims must name the signed-in user, got {document}"
        );
        assert_eq!(
            document["email"].as_str(),
            Some("alice@example.com"),
            "case {method:?}: the claims must carry the stored attributes, got {document}"
        );
        assert!(
            document["sub"].as_str().is_some_and(|sub| !sub.is_empty()),
            "case {method:?}: the claims must carry the subject the pool minted, got {document}"
        );
    }

    // The revocation endpoint ends the sign-in: the refresh token that was granting tokens
    // grants none afterwards, and the access token it issued reads nothing.
    let revoked = post_form(
        &router,
        &revoke_path,
        &[],
        &[
            ("token", &hosted.refresh_token),
            ("client_id", &hosted.client_id),
        ],
    )
    .await;
    assert_eq!(
        revoked.status,
        200,
        "the revocation must be served, got {} with {}",
        revoked.status,
        revoked.json()
    );
    assert!(
        revoked.body.is_empty(),
        "a served revocation carries no body, got {:?}",
        String::from_utf8_lossy(&revoked.body)
    );

    let after = post_form(
        &router,
        &token_path,
        &[],
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &hosted.client_id),
            ("refresh_token", &hosted.refresh_token),
        ],
    )
    .await;
    assert_eq!(
        (after.status, after.error().as_str()),
        (400, "invalid_grant"),
        "a revoked refresh token must grant nothing, got {} with {}",
        after.status,
        after.json()
    );
    let read_after = send(
        &router,
        "GET",
        LOCAL_HOST,
        &user_info_path,
        &[("authorization", bearer.as_str())],
        "",
    )
    .await;
    assert_eq!(
        (read_after.status, read_after.error().as_str()),
        (401, "invalid_token"),
        "a revoked access token must read nothing, got {} with {}",
        read_after.status,
        read_after.json()
    );
}

/// The `client_credentials` grant: the app client's own token, carrying the scopes the grant
/// authorizes, authenticated with the two methods the pool advertises.
#[tokio::test]
async fn the_token_endpoint_serves_the_client_credentials_grant() {
    let router = router();
    let user_pool_id = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "oauth2-m2m-pool" }),
    )
    .await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    ok(
        &router,
        "CreateResourceServer",
        &json!({
            "UserPoolId": user_pool_id,
            "Identifier": "orders",
            "Name": "Orders",
            "Scopes": [{ "ScopeName": "read", "ScopeDescription": "Read orders" }],
        }),
    )
    .await;
    let created = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "m2m-client",
            "GenerateSecret": true,
            "AllowedOAuthFlows": ["client_credentials"],
            "AllowedOAuthScopes": ["orders/read"],
            "AllowedOAuthFlowsUserPoolClient": true,
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    let client_secret = created["UserPoolClient"]["ClientSecret"]
        .as_str()
        .expect("a generated client secret")
        .to_string();
    ok(
        &router,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": user_pool_id, "Domain": "oauth2-m2m" }),
    )
    .await;
    let token_path = "/oauth2-m2m/oauth2/token";
    let basic = format!(
        "Basic {}",
        aws_smithy_types::base64::encode(format!("{client_id}:{client_secret}"))
    );

    // The rows differ along how the app client authenticates and which scopes it asks for.
    let rows: [(&str, bool, Option<&str>, Option<&str>); 4] = [
        (
            "basic credentials asking for the scope it was permitted",
            true,
            Some("orders/read"),
            Some("orders/read"),
        ),
        (
            "form fields asking for the scope it was permitted",
            false,
            Some("orders/read"),
            Some("orders/read"),
        ),
        (
            "basic credentials asking for no scope at all",
            true,
            None,
            Some("orders/read"),
        ),
        (
            "basic credentials asking for a scope it was not permitted",
            true,
            Some("orders/write"),
            None,
        ),
    ];
    for (case, uses_basic, scope, expected_scope) in rows {
        let mut fields: Vec<(&str, &str)> = vec![("grant_type", "client_credentials")];
        if !uses_basic {
            fields.push(("client_id", &client_id));
            fields.push(("client_secret", &client_secret));
        }
        if let Some(scope) = scope {
            fields.push(("scope", scope));
        }
        let headers: Vec<(&str, &str)> = if uses_basic {
            vec![("authorization", basic.as_str())]
        } else {
            Vec::new()
        };
        let answered = post_form(&router, token_path, &headers, &fields).await;
        match expected_scope {
            Some(expected) => {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: the grant must be served, got {} with {}",
                    answered.status,
                    answered.json()
                );
                let granted = answered.json();
                assert!(
                    granted["access_token"].as_str().is_some(),
                    "case {case:?}: the grant must answer an access token, got {granted}"
                );
                assert_eq!(
                    granted["scope"].as_str(),
                    Some(expected),
                    "case {case:?}: the grant must state the scopes it authorized, got {granted}"
                );
                assert_eq!(
                    granted.get("refresh_token"),
                    None,
                    "case {case:?}: a machine-to-machine grant issues no refresh token, got \
                     {granted}"
                );
            }
            None => assert_eq!(
                (answered.status, answered.error().as_str()),
                (400, "invalid_scope"),
                "case {case:?}: a scope the client was not permitted must be refused, got {} \
                 with {}",
                answered.status,
                answered.json()
            ),
        }
    }
}

/// One row of [`the_hosted_endpoints_refuse_a_request_they_cannot_serve`]: the case, the
/// endpoint's path, the form fields the request carries, and the status and OAuth2 error code
/// it earns.
type RefusalRow<'r> = (&'r str, &'r str, Vec<(&'r str, &'r str)>, u16, &'r str);

/// One row of the bearer table: the case and the `Authorization` header the request carries.
type BearerRow<'r> = (&'r str, Option<&'r str>);

/// What each endpoint refuses, and with which OAuth2 error: the rows differ along the field
/// the request leaves out, the grant it names, and which half of the credential is wrong.
#[tokio::test]
async fn the_hosted_endpoints_refuse_a_request_they_cannot_serve() {
    let router = router();
    let hosted = hosted_pool(&router, "oauth2-refusal-pool", "oauth2-refusal").await;
    let token_path = format!("/{}/oauth2/token", hosted.domain);
    let revoke_path = format!("/{}/oauth2/revoke", hosted.domain);
    let user_info_path = format!("/{}/oauth2/userInfo", hosted.domain);
    let client_id = hosted.client_id.clone();
    let refresh_token = hosted.refresh_token.clone();

    let rows: [RefusalRow<'_>; 7] = [
        (
            "a token request naming no grant",
            &token_path,
            vec![("client_id", &client_id)],
            400,
            "invalid_request",
        ),
        (
            "a token request naming no app client",
            &token_path,
            vec![("grant_type", "refresh_token")],
            400,
            "invalid_request",
        ),
        (
            "a token request naming an app client that does not exist",
            &token_path,
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", "nosuchclient"),
            ],
            401,
            "invalid_client",
        ),
        (
            "a refresh grant carrying no refresh token",
            &token_path,
            vec![("grant_type", "refresh_token"), ("client_id", &client_id)],
            400,
            "invalid_request",
        ),
        (
            "a refresh grant carrying a token this server never issued",
            &token_path,
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", &client_id),
                ("refresh_token", "not-a-token-this-server-issued"),
            ],
            400,
            "invalid_grant",
        ),
        (
            "a grant this authorization server does not serve",
            &token_path,
            vec![("grant_type", "password"), ("client_id", &client_id)],
            400,
            "unsupported_grant_type",
        ),
        (
            "a revocation naming no token",
            &revoke_path,
            vec![("client_id", &client_id)],
            400,
            "invalid_request",
        ),
    ];
    for (case, path, fields, status, code) in rows {
        let answered = post_form(&router, path, &[], &fields).await;
        assert_eq!(
            (answered.status, answered.error().as_str()),
            (status, code),
            "case {case:?}: expected ({status}, {code:?}), got ({}, {:?}) with {}",
            answered.status,
            answered.error(),
            answered.json()
        );
    }

    // The user-info endpoint refuses every credential it does not accept, and names the
    // scheme it expected.
    let bearer_rows: [BearerRow<'_>; 3] = [
        ("a request presenting no credential", None),
        ("a bearer value that is no token of this pool", Some("Bearer not-a-token")),
        (
            "a refresh token presented as a bearer access token",
            Some(&format!("Bearer {refresh_token}")),
        ),
    ];
    for (case, header) in bearer_rows {
        let headers: Vec<(&str, &str)> = header
            .map(|value| vec![("authorization", value)])
            .unwrap_or_default();
        let answered = send(&router, "GET", LOCAL_HOST, &user_info_path, &headers, "").await;
        assert_eq!(
            (answered.status, answered.error().as_str()),
            (401, "invalid_token"),
            "case {case:?}: expected the credential to be refused, got {} with {}",
            answered.status,
            answered.json()
        );
        assert_eq!(
            answered.header("www-authenticate").as_deref(),
            Some("Bearer"),
            "case {case:?}: expected the refusal to name the scheme it expected, got {:?}",
            answered.header("www-authenticate")
        );
    }

    // A domain no pool of this emulator holds serves no authorization server at all.
    let nowhere = post_form(
        &router,
        "/oauth2-no-such-domain/oauth2/token",
        &[],
        &[("grant_type", "refresh_token"), ("client_id", &client_id)],
    )
    .await;
    assert_ne!(
        nowhere.status, 200,
        "a domain this emulator holds no pool under must serve nothing, got {} with {:?}",
        nowhere.status,
        String::from_utf8_lossy(&nowhere.body)
    );
}
