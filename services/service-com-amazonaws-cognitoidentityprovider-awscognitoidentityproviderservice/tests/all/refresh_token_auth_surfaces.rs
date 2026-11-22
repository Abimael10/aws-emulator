// SPDX-License-Identifier: CC0-1.0

//! The refresh-token exchange across the THREE surfaces that serve one, and the refusal
//! each presented token earns on all of them alike.
//!
//! RULE: a refresh token signs its user in again on every surface that takes one — the
//! client-side sign-in `AuthFlow=REFRESH_TOKEN_AUTH` (and its `REFRESH_TOKEN` alias), the
//! administrative sign-in's same two flows, and the token exchange — and a token that does
//! not authorize the refresh is refused by the FIRST of these that holds, with its own
//! fixed sentence, the same on each surface:
//!
//! 1. the value resolves to no refresh token this service issued — "Invalid Refresh Token";
//! 2. it is a refresh token this service issued for ANOTHER app client than the request
//!    names — "Refresh Token has different Client";
//! 3. the user it was issued to may not sign in — "User is disabled." for a deactivated
//!    user, which is reported ahead of the state of the token itself, because deactivating
//!    a user also revokes its tokens and a caller told only that would re-authenticate,
//!    which the same state refuses;
//! 4. the token's own state: one this service revoked — "Refresh Token has been revoked".
//!
//! Every refusal is `NotAuthorizedException`, and no refusal is a 501: the flow is served.
//!
//! The rows span what the rule generalises over: which surface answers (three operations,
//! and both spellings of the flow on each sign-in), and WHY the presented token does not
//! authorize the refresh (nothing issued it, a token of another kind, a token of a sibling
//! app client, a token of an app client of another pool, a deactivated user, a revoked
//! token). The live token of each surface's own app client is a row too, so the refusals
//! above are each row's own and not a flow that refuses everything.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// A credential scope naming this service, which is what routes a call here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The endpoint of THIS emulator the suite's calls are addressed at, which is the address
/// a client configured with the emulator's own endpoint reaches it on — and therefore the
/// address the pools it creates publish their token issuer on.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The error class every refusal of a presented refresh token belongs to.
const NOT_AUTHORIZED: &str = "NotAuthorizedException";

/// The sentence a value that resolves to no refresh token of this service is refused with.
/// It ends in NO period, as the sentence Amazon Cognito denies such a value with does not.
const INVALID_REFRESH_TOKEN: &str = "Invalid Refresh Token";

/// The sentence a refresh token issued for another app client than the request names is
/// refused with.
const DIFFERENT_CLIENT: &str = "Refresh Token has different Client";

/// The sentence a refresh for a DEACTIVATED user is refused with.
const USER_DISABLED: &str = "User is disabled.";

/// The sentence a refresh with a token this service revoked is refused with.
const REFRESH_TOKEN_REVOKED: &str = "Refresh Token has been revoked";

/// The flows an app client of this suite admits.
const FLOWS: [&str; 3] = [
    "ALLOW_ADMIN_USER_PASSWORD_AUTH",
    "ALLOW_USER_PASSWORD_AUTH",
    "ALLOW_REFRESH_TOKEN_AUTH",
];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    router
}

/// Dispatch `operation` with `body` at this emulator's own endpoint, and answer the status
/// and the JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", EMULATOR_ENDPOINT)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
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

/// [`call`], requiring success.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The wire error code of an error body.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The message of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// The string member `name` of `document`.
fn member(case: &str, document: &Value, name: &str) -> String {
    document[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} in {document}"))
        .to_string()
}

/// The claim set of a compact JWS, decoded from its second segment.
fn claims(case: &str, token: &str) -> Value {
    let segment = token
        .split('.')
        .nth(1)
        .unwrap_or_else(|| panic!("case {case:?}: a token is a compact JWS, got {token:?}"));
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    let bytes = aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|error| panic!("case {case:?}: the claims must be base64url ({error})"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: the claims must be JSON ({error})"))
}

/// Create a pool, and answer its id.
async fn create_pool(router: &MultiServiceRouter, name: &str) -> String {
    member(
        name,
        &ok(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"],
        "Id",
    )
}

/// Create an app client of `user_pool_id`, and answer its id.
async fn create_client(router: &MultiServiceRouter, user_pool_id: &str, name: &str) -> String {
    member(
        name,
        &ok(
            router,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientName": name,
                "ExplicitAuthFlows": FLOWS,
            }),
        )
        .await["UserPoolClient"],
        "ClientId",
    )
}

/// Create a `CONFIRMED` user of `user_pool_id` holding [`GOOD_PASSWORD`].
async fn create_user(router: &MultiServiceRouter, user_pool_id: &str, username: &str) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
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
}

/// Sign `username` in administratively through `client_id`, and answer the
/// `AuthenticationResult`.
async fn sign_in(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> Value {
    ok(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await["AuthenticationResult"]
        .clone()
}

/// Which surface a row asks, and how it presents the token to it.
#[derive(Clone, Copy, Debug)]
enum Surface {
    /// The client-side sign-in, with the `AuthFlow` spelling this surface uses.
    SignIn(&'static str),
    /// The administrative sign-in, with the `AuthFlow` spelling this surface uses.
    AdminSignIn(&'static str),
    /// The token exchange, which models no `AuthFlow`.
    Exchange,
}

impl Surface {
    /// How a case naming this surface reads in a failure.
    fn label(self) -> String {
        match self {
            Self::SignIn(flow) => format!("InitiateAuth AuthFlow={flow}"),
            Self::AdminSignIn(flow) => format!("AdminInitiateAuth AuthFlow={flow}"),
            Self::Exchange => "GetTokensFromRefreshToken".to_string(),
        }
    }

    /// The operation and the request body that present `refresh_token` to this surface for
    /// the app client `client_id` of `user_pool_id`.
    fn request(
        self,
        user_pool_id: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> (&'static str, Value) {
        match self {
            Self::SignIn(flow) => (
                "InitiateAuth",
                json!({
                    "ClientId": client_id,
                    "AuthFlow": flow,
                    "AuthParameters": { "REFRESH_TOKEN": refresh_token },
                }),
            ),
            Self::AdminSignIn(flow) => (
                "AdminInitiateAuth",
                json!({
                    "UserPoolId": user_pool_id,
                    "ClientId": client_id,
                    "AuthFlow": flow,
                    "AuthParameters": { "REFRESH_TOKEN": refresh_token },
                }),
            ),
            Self::Exchange => (
                "GetTokensFromRefreshToken",
                json!({ "ClientId": client_id, "RefreshToken": refresh_token }),
            ),
        }
    }
}

/// Every surface that takes a refresh token, in both spellings of the flow where the
/// surface has one.
const SURFACES: [Surface; 5] = [
    Surface::SignIn("REFRESH_TOKEN_AUTH"),
    Surface::SignIn("REFRESH_TOKEN"),
    Surface::AdminSignIn("REFRESH_TOKEN_AUTH"),
    Surface::AdminSignIn("REFRESH_TOKEN"),
    Surface::Exchange,
];

/// RULE: every surface that takes a refresh token refuses one that does not authorize the
/// refresh with the same fixed sentence, chosen by WHY it does not — and serves the live
/// token of its own app client.
#[tokio::test]
async fn every_refresh_surface_refuses_a_presented_token_for_the_same_reason() {
    let router = router();
    let user_pool_id = create_pool(&router, "refresh-surfaces-pool").await;
    let other_pool_id = create_pool(&router, "refresh-surfaces-other-pool").await;
    let client_id = create_client(&router, &user_pool_id, "mine").await;
    let sibling = create_client(&router, &user_pool_id, "sibling").await;
    let abroad = create_client(&router, &other_pool_id, "abroad").await;
    for username in ["alice", "disabled", "signedout"] {
        create_user(&router, &user_pool_id, username).await;
    }
    create_user(&router, &other_pool_id, "alice").await;

    let live = sign_in(&router, &user_pool_id, &client_id, "alice").await;
    let siblings = sign_in(&router, &user_pool_id, &sibling, "alice").await;
    let abroads = sign_in(&router, &other_pool_id, &abroad, "alice").await;
    let deactivated = sign_in(&router, &user_pool_id, &client_id, "disabled").await;
    let revoked = sign_in(&router, &user_pool_id, &client_id, "signedout").await;

    // The two states a token of this app client can be in beside being live. Deactivating
    // a user revokes its tokens too, so this row is also where the ORDER of the two
    // refusals is pinned.
    ok(
        &router,
        "AdminDisableUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "disabled" }),
    )
    .await;
    ok(
        &router,
        "AdminUserGlobalSignOut",
        &json!({ "UserPoolId": user_pool_id, "Username": "signedout" }),
    )
    .await;

    // (case, the value presented, the sentence it earns — `None` for a refresh that is
    // served)
    let rows: [(&str, String, Option<&str>); 7] = [
        (
            "a value nothing issued",
            "not.a.token.this.emulator.issued".to_string(),
            Some(INVALID_REFRESH_TOKEN),
        ),
        (
            "the access token of the caller's own sign-in",
            member("the access token", &live, "AccessToken"),
            Some(INVALID_REFRESH_TOKEN),
        ),
        (
            "a refresh token of a sibling app client of the same pool",
            member("the sibling's token", &siblings, "RefreshToken"),
            Some(DIFFERENT_CLIENT),
        ),
        (
            "a refresh token of an app client of another pool",
            member("the other pool's token", &abroads, "RefreshToken"),
            Some(DIFFERENT_CLIENT),
        ),
        (
            "a refresh token whose user was deactivated",
            member("the deactivated user's token", &deactivated, "RefreshToken"),
            Some(USER_DISABLED),
        ),
        (
            "a refresh token an administrative sign-out revoked",
            member("the revoked token", &revoked, "RefreshToken"),
            Some(REFRESH_TOKEN_REVOKED),
        ),
        (
            "the live refresh token of this app client",
            member("the live token", &live, "RefreshToken"),
            None,
        ),
    ];

    for (case, presented, expected) in &rows {
        for surface in SURFACES {
            let label = surface.label();
            let case = format!("{case} / {label}");
            let (operation, body) = surface.request(&user_pool_id, &client_id, presented);
            let (status, payload) = call(&router, operation, &body).await;
            match expected {
                Some(message) => assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, NOT_AUTHORIZED, *message),
                    "case {case:?}: expected (400, {NOT_AUTHORIZED:?}, {message:?}), got \
                     {status} with {payload}"
                ),
                None => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the live token must refresh, got {status} with \
                         {payload}"
                    );
                    let result = &payload["AuthenticationResult"];
                    for minted in ["AccessToken", "IdToken"] {
                        assert!(
                            result[minted].as_str().is_some_and(|token| !token.is_empty()),
                            "case {case:?}: a refresh mints {minted}, got {payload}"
                        );
                    }
                }
            }
        }
    }
}

/// RULE, on the leg the recorded case drives: a refresh for a DEACTIVATED user is refused
/// `NotAuthorizedException` "User is disabled." on the administrative sign-in as on the
/// client-side one — a 4xx refusal a client can classify, never a 501 — and re-enabling the
/// user makes the sign-in and the refresh serve again.
#[tokio::test]
async fn a_deactivated_user_cannot_refresh_until_it_is_re_enabled() {
    let router = router();
    let user_pool_id = create_pool(&router, "refresh-disabled-pool").await;
    let client_id = create_client(&router, &user_pool_id, "disabled-pool-client").await;
    create_user(&router, &user_pool_id, "dave").await;
    let signed_in = sign_in(&router, &user_pool_id, &client_id, "dave").await;
    let refresh_token = member("the sign-in", &signed_in, "RefreshToken");

    ok(
        &router,
        "AdminDisableUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "dave" }),
    )
    .await;

    for surface in SURFACES {
        let case = format!("a deactivated user's refresh on {}", surface.label());
        let (operation, body) = surface.request(&user_pool_id, &client_id, &refresh_token);
        let (status, payload) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, NOT_AUTHORIZED, USER_DISABLED),
            "case {case:?}: expected (400, {NOT_AUTHORIZED:?}, {USER_DISABLED:?}), got \
             {status} with {payload}"
        );
        assert!(
            (400..500).contains(&status),
            "case {case:?}: a deactivated user is a refusal of the CALLER, got {status} \
             with {payload}"
        );
    }

    // The password sign-in is refused for the same state, which is why the refresh may not
    // report a token state the caller would re-authenticate over.
    let (status, payload) = call(
        &router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": "dave", "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, NOT_AUTHORIZED, USER_DISABLED),
        "case \"the password sign-in of a deactivated user\": expected it refused too, got \
         {status} with {payload}"
    );

    // Re-enabling the user lets it sign in again, and the refresh token THAT sign-in
    // issues refreshes: the refusal was the user's state and nothing else.
    ok(
        &router,
        "AdminEnableUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "dave" }),
    )
    .await;
    let re_signed_in = sign_in(&router, &user_pool_id, &client_id, "dave").await;
    let reissued = member("the re-enabled sign-in", &re_signed_in, "RefreshToken");
    let refreshed = ok(
        &router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "REFRESH_TOKEN_AUTH",
            "AuthParameters": { "REFRESH_TOKEN": reissued },
        }),
    )
    .await;
    let result = &refreshed["AuthenticationResult"];
    let case = "the re-enabled user's refresh";
    let refreshed_id = member(case, result, "IdToken");
    assert_ne!(
        refreshed_id,
        member(case, &re_signed_in, "IdToken"),
        "case {case:?}: a refresh mints a FRESH id token, got {result}"
    );
    // "Receive new ID and access tokens" (`AdminInitiateAuth.AuthFlow`): the refresh token
    // the caller presented stays the one in force, so none is minted beside them.
    assert!(
        result.get("RefreshToken").is_none(),
        "case {case:?}: this flow mints no new refresh token, got {result}"
    );

    // The refreshed token is the same pool's: it names the same subject and the same
    // issuer the sign-in's own token named, which is the issuer a verifier fetches the
    // pool's key set from.
    let signed_in_claims = claims(case, &member(case, &re_signed_in, "IdToken"));
    let refreshed_claims = claims(case, &refreshed_id);
    for claim in ["sub", "iss", "aud"] {
        assert_eq!(
            refreshed_claims[claim], signed_in_claims[claim],
            "case {case:?}: a refreshed token keeps the {claim} claim of the session, got \
             {refreshed_claims}"
        );
    }
    let issuer = member(case, &refreshed_claims, "iss");
    assert_eq!(
        issuer,
        format!("https://{EMULATOR_ENDPOINT}/{user_pool_id}"),
        "case {case:?}: the refreshed token announces the pool's own issuer, the endpoint of \
         this emulator the pool was created through, got {issuer:?}"
    );
}
