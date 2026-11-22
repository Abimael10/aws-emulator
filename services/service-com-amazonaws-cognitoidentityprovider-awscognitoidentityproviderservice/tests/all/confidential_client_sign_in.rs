// SPDX-License-Identifier: CC0-1.0

//! The `SECRET_HASH` an app client that holds a client secret requires of every sign-in.
//!
//! The rule under test. "Add a `SECRET_HASH` parameter if your app client has a client
//! secret" (`InitiateAuth.AuthParameters` and `AdminInitiateAuth.AuthParameters` in this
//! service's model), stated for the parameter map as a whole rather than for one
//! `AuthFlow`. `SecretHash` is "a keyed-hash message authentication code (HMAC)
//! calculated using the secret key of a user pool client and username plus the client ID
//! in the message" (`SignUp.SecretHash`), so an app client that holds a secret accepts
//! only the tag THAT secret produces over the username the sign-in names plus the app
//! client, and refuses the sign-in otherwise with `NotAuthorizedException` — one sentence
//! for a parameter that was not received and one for a hash that does not verify. An app
//! client that holds no secret has nothing to prove, so it is judged by neither.
//!
//! The rows span what the rule generalises over: the sign-in API and `AuthFlow` asked
//! for (the client-side password flow, the choice-based flow that delivers a one-time
//! password, both refresh flows, whose username comes from the presented refresh token
//! rather than from the request, and the administrative password flow), and the hash the
//! request presents (none at all, the app client's own secret over the right username,
//! another app client's secret, the right secret over another username, and the right
//! secret over another app client). Each refusal row runs BEFORE the row that presents
//! the right hash, so a served sign-in after it shows the refusals moved no state.
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

/// A password that satisfies the default password policy of a user pool.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The user every row signs in, which carries a verified email so the choice-based flow
/// has somewhere to deliver its one-time password.
const USERNAME: &str = "alice";

/// A second user of the same pool, so a row can compute the right secret's hash over a
/// username that is not the one it signs in.
const OTHER_USERNAME: &str = "bob";

/// The `ExplicitAuthFlows` every app client of this suite declares, so one client serves
/// each flow the table asks for.
const EVERY_FLOW: [&str; 4] = [
    "ALLOW_USER_PASSWORD_AUTH",
    "ALLOW_ADMIN_USER_PASSWORD_AUTH",
    "ALLOW_REFRESH_TOKEN_AUTH",
    "ALLOW_USER_AUTH",
];

/// The email one-time-password challenge (`ChallengeNameType`), which is also the
/// `SignInPolicyType.AllowedFirstAuthFactors` value that admits it.
const EMAIL_OTP: &str = "EMAIL_OTP";

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

/// The `SECRET_HASH` a request for `username` to the app client `client_id` carries when
/// that client's secret is `client_secret`: base64 of HMAC-SHA256, keyed with the secret,
/// over the username followed by the client id
/// (`smithy_query(query="doc",shape="SignUpRequest",member="SecretHash")`).
///
/// Shared with the suites whose sign-in fixtures act as a confidential app client, so one
/// definition of the tag this service accepts serves every one of them.
pub fn secret_hash(client_secret: &str, username: &str, client_id: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, client_secret.as_bytes());
    let tag = ring::hmac::sign(&key, format!("{username}{client_id}").as_bytes());
    aws_smithy_types::base64::encode(tag.as_ref())
}

/// One created app client: its id and the secret it holds, for a confidential one.
struct Client {
    client_id: String,
    client_secret: Option<String>,
}

/// The pool the suite's rows are served by, with three app clients of it: one holding a
/// secret, a second holding a DIFFERENT secret, and a public one holding none.
struct Fixture {
    user_pool_id: String,
    confidential: Client,
    other_confidential: Client,
    public: Client,
}

/// Create an app client of `user_pool_id` declaring every flow this suite asks for, with
/// a generated secret when `with_secret`.
async fn create_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_name: &str,
    with_secret: bool,
) -> Client {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientName".to_string(), json!(client_name));
    body.insert("ExplicitAuthFlows".to_string(), json!(EVERY_FLOW));
    if with_secret {
        body.insert("GenerateSecret".to_string(), json!(true));
    }
    let created = ok(router, "CreateUserPoolClient", &Value::Object(body)).await;
    let client = &created["UserPoolClient"];
    Client {
        client_id: client["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("a minted client id: {created}"))
            .to_string(),
        client_secret: client["ClientSecret"].as_str().map(str::to_string),
    }
}

/// Create `username` in `user_pool_id` with a permanent password and a verified email, so
/// it is `CONFIRMED` and every flow of the table can sign it in.
async fn create_confirmed_user(router: &MultiServiceRouter, user_pool_id: &str, username: &str) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "email", "Value": format!("{username}@example.test") },
                { "Name": "email_verified", "Value": "true" },
            ],
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

/// Create the pool, its three app clients and its two users.
async fn fixture(router: &MultiServiceRouter, pool_name: &str) -> Fixture {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": pool_name,
            // The choice-based flow is offered only by a pool that admits the factor it
            // asks for (`SignInPolicyType.AllowedFirstAuthFactors`).
            "Policies": {
                "SignInPolicy": { "AllowedFirstAuthFactors": ["PASSWORD", EMAIL_OTP] },
            },
        }),
    )
    .await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    let confidential = create_client(router, &user_pool_id, "confidential", true).await;
    let other_confidential = create_client(router, &user_pool_id, "other-confidential", true).await;
    let public = create_client(router, &user_pool_id, "public", false).await;
    create_confirmed_user(router, &user_pool_id, USERNAME).await;
    create_confirmed_user(router, &user_pool_id, OTHER_USERNAME).await;
    Fixture {
        user_pool_id,
        confidential,
        other_confidential,
        public,
    }
}

/// Which sign-in a row asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// `InitiateAuth` with `AuthFlow=USER_PASSWORD_AUTH`.
    Password,
    /// `InitiateAuth` with `AuthFlow=USER_AUTH` and `PREFERRED_CHALLENGE=EMAIL_OTP`.
    ChoiceBased,
    /// `InitiateAuth` with `AuthFlow=REFRESH_TOKEN_AUTH`, whose username is the one the
    /// presented refresh token was issued to.
    Refresh,
    /// `AdminInitiateAuth` with `AuthFlow=ADMIN_USER_PASSWORD_AUTH`.
    AdminPassword,
    /// `AdminInitiateAuth` with `AuthFlow=REFRESH_TOKEN_AUTH`, whose username is also the
    /// one the presented refresh token was issued to.
    AdminRefresh,
}

impl Flow {
    /// The operation this flow is served by.
    fn operation(self) -> &'static str {
        match self {
            Self::AdminPassword | Self::AdminRefresh => "AdminInitiateAuth",
            _ => "InitiateAuth",
        }
    }

    /// The request body this flow sends for `client_id`, carrying `hash` as its
    /// `SECRET_HASH` entry when the row presents one.
    fn body(
        self,
        user_pool_id: &str,
        client_id: &str,
        refresh_token: &str,
        hash: Option<String>,
    ) -> Value {
        let mut parameters = Map::new();
        match self {
            Self::Password | Self::AdminPassword => {
                parameters.insert("USERNAME".to_string(), json!(USERNAME));
                parameters.insert("PASSWORD".to_string(), json!(GOOD_PASSWORD));
            }
            Self::ChoiceBased => {
                parameters.insert("USERNAME".to_string(), json!(USERNAME));
                parameters.insert("PREFERRED_CHALLENGE".to_string(), json!(EMAIL_OTP));
            }
            Self::Refresh | Self::AdminRefresh => {
                parameters.insert("REFRESH_TOKEN".to_string(), json!(refresh_token));
            }
        }
        if let Some(hash) = hash {
            parameters.insert("SECRET_HASH".to_string(), json!(hash));
        }
        let mut body = Map::new();
        if matches!(self, Self::AdminPassword | Self::AdminRefresh) {
            body.insert("UserPoolId".to_string(), json!(user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(client_id));
        body.insert(
            "AuthFlow".to_string(),
            json!(match self {
                Self::Password => "USER_PASSWORD_AUTH",
                Self::ChoiceBased => "USER_AUTH",
                Self::Refresh | Self::AdminRefresh => "REFRESH_TOKEN_AUTH",
                Self::AdminPassword => "ADMIN_USER_PASSWORD_AUTH",
            }),
        );
        body.insert("AuthParameters".to_string(), Value::Object(parameters));
        Value::Object(body)
    }
}

/// Which `SECRET_HASH` entry a row presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// No `SECRET_HASH` entry at all.
    Nothing,
    /// The tag the app client's OWN secret produces over the username it signs in.
    OwnSecret,
    /// The tag ANOTHER app client's secret produces over the same username.
    AnotherSecret,
    /// The tag the right secret produces over another username of the same pool.
    AnotherUsername,
    /// The tag the right secret produces over another app client's id.
    AnotherClient,
}

/// Sign [`USERNAME`] in through `client` and return the refresh token it was issued, so a
/// refresh row has one to present.
async fn issued_refresh_token(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client: &Client,
) -> String {
    let hash = client
        .client_secret
        .as_deref()
        .map(|secret| secret_hash(secret, USERNAME, &client.client_id));
    let signed_in = ok(
        router,
        "InitiateAuth",
        &Flow::Password.body(user_pool_id, &client.client_id, "", hash),
    )
    .await;
    signed_in["AuthenticationResult"]["RefreshToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in issues a refresh token: {signed_in}"))
        .to_string()
}

/// Assert that `payload` is the success `flow` answers a completed gate with: the token
/// triple for the flows that finish a sign-in, and the outstanding challenge for the
/// choice-based flow that delivers a one-time password.
fn assert_served(case: &str, flow: Flow, status: u16, payload: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: the proven secret must be served, got {status} with {payload}"
    );
    match flow {
        Flow::ChoiceBased => assert_eq!(
            (
                payload["ChallengeName"].as_str(),
                payload["Session"].as_str().is_some()
            ),
            (Some(EMAIL_OTP), true),
            "case {case:?}: the choice-based sign-in starts the one-time-password \
             challenge, got {payload}"
        ),
        _ => assert!(
            payload["AuthenticationResult"]["AccessToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            "case {case:?}: the sign-in issues an access token, got {payload}"
        ),
    }
}

/// A confidential app client refuses every sign-in that does not prove it holds the
/// client secret, and serves the one that does — on each flow, whatever the flow takes
/// its username from.
#[tokio::test]
async fn a_confidential_app_client_requires_the_secret_hash_of_every_sign_in() {
    let router = router();
    let fixture = fixture(&router, "confidential-sign-in-pool").await;
    let secret = fixture
        .confidential
        .client_secret
        .clone()
        .expect("the confidential app client holds a generated secret");
    let other_secret = fixture
        .other_confidential
        .client_secret
        .clone()
        .expect("the second confidential app client holds its own generated secret");
    let refresh_token =
        issued_refresh_token(&router, &fixture.user_pool_id, &fixture.confidential).await;

    // The two sentences this service words the defect with, each naming the app client
    // the request named and nothing else.
    let not_received = format!(
        "Client {} is configured with secret but SECRET_HASH was not received",
        fixture.confidential.client_id
    );
    let unverified = format!(
        "Unable to verify secret hash for client {}",
        fixture.confidential.client_id
    );

    for flow in [
        Flow::Password,
        Flow::ChoiceBased,
        Flow::Refresh,
        Flow::AdminPassword,
        Flow::AdminRefresh,
    ] {
        // The refusals come first, so the served row that follows them shows that none of
        // them moved the stored state the sign-in reads.
        for presented in [
            Presented::Nothing,
            Presented::AnotherSecret,
            Presented::AnotherUsername,
            Presented::AnotherClient,
            Presented::OwnSecret,
        ] {
            let case = format!("{flow:?} presenting {presented:?}");
            let hash = match presented {
                Presented::Nothing => None,
                Presented::OwnSecret => Some(secret_hash(
                    &secret,
                    USERNAME,
                    &fixture.confidential.client_id,
                )),
                Presented::AnotherSecret => Some(secret_hash(
                    &other_secret,
                    USERNAME,
                    &fixture.confidential.client_id,
                )),
                Presented::AnotherUsername => Some(secret_hash(
                    &secret,
                    OTHER_USERNAME,
                    &fixture.confidential.client_id,
                )),
                Presented::AnotherClient => {
                    Some(secret_hash(&secret, USERNAME, &fixture.public.client_id))
                }
            };
            let body = flow.body(
                &fixture.user_pool_id,
                &fixture.confidential.client_id,
                &refresh_token,
                hash,
            );
            let (status, payload) = call(&router, flow.operation(), &body).await;
            let expected = match presented {
                Presented::OwnSecret => None,
                Presented::Nothing => Some(not_received.as_str()),
                _ => Some(unverified.as_str()),
            };
            match expected {
                None => assert_served(&case, flow, status, &payload),
                Some(message) => assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, "NotAuthorizedException", message),
                    "case {case:?}: expected the refusal {message:?}, got {status} with \
                     {payload}"
                ),
            }
        }
    }
}

/// A PUBLIC app client holds no secret, so there is nothing for a request to prove: every
/// flow is served whether the request carries a `SECRET_HASH` entry or not, and a stray
/// entry is not a credential the client could reject.
#[tokio::test]
async fn a_public_app_client_asks_for_no_secret_hash() {
    let router = router();
    let fixture = fixture(&router, "public-sign-in-pool").await;
    let refresh_token = issued_refresh_token(&router, &fixture.user_pool_id, &fixture.public).await;
    let stray_secret = fixture
        .confidential
        .client_secret
        .clone()
        .expect("the confidential app client holds a generated secret");

    for flow in [
        Flow::Password,
        Flow::ChoiceBased,
        Flow::Refresh,
        Flow::AdminPassword,
        Flow::AdminRefresh,
    ] {
        for (presented, hash) in [
            ("no SECRET_HASH entry", None),
            (
                "a stray SECRET_HASH entry of another client's secret",
                Some(secret_hash(
                    &stray_secret,
                    USERNAME,
                    &fixture.public.client_id,
                )),
            ),
        ] {
            let case = format!("{flow:?} on a public client with {presented}");
            let body = flow.body(
                &fixture.user_pool_id,
                &fixture.public.client_id,
                &refresh_token,
                hash,
            );
            let (status, payload) = call(&router, flow.operation(), &body).await;
            assert_served(&case, flow, status, &payload);
        }
    }
}
