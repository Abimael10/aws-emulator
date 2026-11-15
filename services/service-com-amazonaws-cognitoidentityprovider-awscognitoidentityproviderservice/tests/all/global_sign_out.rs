// SPDX-License-Identifier: CC0-1.0

//! `GlobalSignOut` over the wire, driven exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.GlobalSignOut`).
//!
//! One rule is under test, in both directions.
//!
//! * An `AccessToken` that names a signed-in user signs THAT user out: it
//!   "invalidates the identity, access, and refresh tokens that Amazon Cognito issued
//!   to a user" (`GlobalSignOut` in this service's model), so every token issued to
//!   that user — whichever app client issued it, whichever flow issued it, and
//!   including the one that authorized the call — stops authorizing anything, while
//!   the stored profile is left exactly as it was and signs in again.
//! * An `AccessToken` that names no signed-in user signs NOBODY out and is refused
//!   with the error class that says why it was not accepted.
//!
//! The rows of the first table differ along what the rule generalises over: how many
//! sign-ins the user has outstanding, which app client of the pool each one went
//! through, whether it took the self-service or the administrative flow, and the case
//! the sign-in spelled the username in. Each row also keeps two sign-ins the sign-out
//! must leave alone: another user of the same pool, and a NAMESAKE of the same name in
//! another pool.
//!
//! The rows of the second table span the rejection classes the operation tells apart:
//! a value this service never issued, a token of the sign-in that is not the access
//! token, a token a sign-out already revoked, a token whose user or pool is gone, a
//! user Amazon Cognito is waiting on a new password from, and a machine-to-machine
//! token of an app client.
//!
//! Every pool, app client, user and token is real: they come from `CreateUserPool`,
//! `CreateUserPoolClient`, `CreateResourceServer`, `AdminCreateUser`,
//! `AdminSetUserPassword`, `InitiateAuth`, `AdminInitiateAuth` and `GetClientToken`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::state;

/// A request's SigV4 credential scope is what routes an administrative call to this
/// service. The signature itself is never checked here: the suite turns request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// This service's regional endpoint host, which a signed call is addressed to.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The emulator's own address, which a signed-in user's client sends the self-service
/// half of this API to.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The error class every rejection of the token itself carries.
const NOT_AUTHORIZED: &str = "NotAuthorizedException";

/// The sentence a call authorized by a token a sign-out invalidated is refused with:
/// "Amazon Cognito returns an `Access Token has been revoked` error when your app
/// attempts to authorize a user pools API request with a revoked access token"
/// (`GlobalSignOut` in this service's model).
const ACCESS_TOKEN_REVOKED: &str = "Access Token has been revoked";

/// The sentence a refresh with a refresh token a sign-out invalidated is refused with.
const REFRESH_TOKEN_REVOKED: &str = "Refresh Token has been revoked";

/// The sentence a value that resolves to no access token of this service is refused
/// with.
const INVALID_ACCESS_TOKEN: &str = "Invalid Access Token";

/// The sentence a live access token whose scopes do not authorize the user
/// self-service surface is refused with: each of those operations "must include the
/// scope `aws.cognito.signin.user.admin`" (this service's model).
const MISSING_REQUIRED_SCOPES: &str = "Access Token does not have required scopes";

/// The error class and sentence a live access token of a user Amazon Cognito is
/// waiting on a new password from is refused with.
const PASSWORD_RESET_REQUIRED_EXCEPTION: &str = "PasswordResetRequiredException";
const PASSWORD_RESET_REQUIRED: &str = "Password reset required for the user";

/// A value shaped like a `TokenModelType` (pattern `^[A-Za-z0-9-_=.]+$`) that this
/// service never issued.
const NEVER_ISSUED_TOKEN: &str = "not-a-token-this-service-ever-issued";

/// A `ClientSecretType`-shaped secret (pattern `^[\w+]+$`, length 24..64) the
/// machine-to-machine app client of this suite holds.
const CLIENT_SECRET: &str = "m2msecret00000000000000001";

/// The `ExplicitAuthFlows` value that configures an app client for
/// machine-to-machine authorization (`GetClientToken` in this service's model).
const CLIENT_TOKEN_AUTH_FLOW: &str = "ALLOW_CLIENT_TOKEN_AUTH";

/// One emulator with just this service mounted, beside the engine holding its state.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.set_request_authentication(false);
    Emulator { router, engine }
}

/// Dispatch `operation` with `body` and return its status and JSON response body. A
/// `signed` call carries a credential scope, as the SDK sends every administrative
/// operation; an unsigned one is addressed to the emulator's own address, which is how
/// a signed-in user's client calls the self-service half of this API.
async fn dispatch(
    emulator: &Emulator,
    operation: &str,
    body: &Value,
    signed: bool,
) -> (u16, Value) {
    let host = if signed { ENDPOINT_HOST } else { EMULATOR_HOST };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        );
    if signed {
        request = request.header("authorization", AUTHORIZATION);
    }
    let response = emulator
        .router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
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

/// Call the signed `operation`, requiring success, and return its response body.
async fn ok(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(emulator, operation, body, true).await;
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

/// The stored record of `token`, as this service wrote it when it issued the token.
fn token_record(emulator: &Emulator, token: &str) -> Option<state::TokenRecord> {
    let service_state = state::service_state(&*emulator.engine).expect("this service's state");
    let guard = service_state.lock();
    guard.token(token).cloned()
}

/// Create a pool named `name`, with `extra` merged into the request, and return its
/// minted id.
async fn pool(emulator: &Emulator, name: &str, extra: Value) -> String {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(name));
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body.insert(member.clone(), value.clone());
        }
    }
    let created = ok(emulator, "CreateUserPool", &Value::Object(body)).await;
    created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string()
}

/// Create an app client of `user_pool_id` that admits both password sign-in flows and
/// the refresh flow, with `extra` merged in, and return its minted id.
async fn app_client(emulator: &Emulator, user_pool_id: &str, name: &str, extra: Value) -> String {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientName".to_string(), json!(name));
    body.insert(
        "ExplicitAuthFlows".to_string(),
        json!([
            "ALLOW_USER_PASSWORD_AUTH",
            "ALLOW_ADMIN_USER_PASSWORD_AUTH",
            "ALLOW_REFRESH_TOKEN_AUTH"
        ]),
    );
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body.insert(member.clone(), value.clone());
        }
    }
    let created = ok(emulator, "CreateUserPoolClient", &Value::Object(body)).await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string()
}

/// Create `username` in `user_pool_id` with a permanent password, and return the
/// username as the pool STORED it — which a pool whose usernames are case-insensitive
/// may spell differently from the request.
async fn create_user(emulator: &Emulator, user_pool_id: &str, username: &str) -> String {
    create_user_with(emulator, user_pool_id, username, &[]).await
}

/// Create `username` in `user_pool_id` carrying `attributes`, with a permanent
/// password, and return the username as the pool stored it.
async fn create_user_with(
    emulator: &Emulator,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) -> String {
    let created = ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": Value::Array(
                attributes
                    .iter()
                    .map(|(name, value)| json!({ "Name": name, "Value": value }))
                    .collect(),
            ),
        }),
    )
    .await;
    let stored = created["User"]["Username"]
        .as_str()
        .expect("the stored username")
        .to_string();
    ok(
        emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    stored
}

/// Which sign-in flow a lineage took: the user's own, or an administrator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    SelfService,
    Admin,
}

/// The tokens one completed sign-in issued, and the app client that issued them.
struct Tokens {
    client_id: String,
    access_token: String,
    id_token: String,
    refresh_token: String,
}

/// Sign `username` in through `client_id` of `user_pool_id` on `route`, and return the
/// tokens, requiring a completed sign-in.
async fn sign_in(
    emulator: &Emulator,
    route: Route,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> Tokens {
    let (operation, body, signed) = match route {
        Route::SelfService => (
            "InitiateAuth",
            json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            }),
            false,
        ),
        Route::Admin => (
            "AdminInitiateAuth",
            json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            }),
            true,
        ),
    };
    let (status, payload) = dispatch(emulator, operation, &body, signed).await;
    assert_eq!(
        status, 200,
        "signing {username} in through {operation} must succeed, got {status} with {payload}"
    );
    let result = &payload["AuthenticationResult"];
    let member = |name: &str| {
        result[name]
            .as_str()
            .unwrap_or_else(|| {
                panic!("a completed sign-in issues {name}, got {payload}")
            })
            .to_string()
    };
    Tokens {
        client_id: client_id.to_string(),
        access_token: member("AccessToken"),
        id_token: member("IdToken"),
        refresh_token: member("RefreshToken"),
    }
}

/// Assert that `access_token` still authorizes the self-service surface, as the user
/// `username`.
async fn still_authorizes(
    emulator: &Emulator,
    case: &str,
    who: &str,
    tokens: &Tokens,
    username: &str,
) {
    let (status, payload) = dispatch(
        emulator,
        "GetUser",
        &json!({ "AccessToken": tokens.access_token }),
        false,
    )
    .await;
    assert_eq!(
        (status, payload["Username"].as_str()),
        (200, Some(username)),
        "case {case:?}: {who} must still be served as {username}, got {status} with {payload}"
    );
}

/// The sign-in routes and app clients one row of the sign-out table signs its user in
/// through, with the pool's username casing and the spelling the sign-in uses.
struct SignOutCase {
    case: &'static str,
    /// `UsernameConfiguration.CaseSensitive` of the pools this row creates.
    case_sensitive: bool,
    /// The username `AdminCreateUser` is called with.
    username: &'static str,
    /// The spelling every sign-in request of this row uses for that user.
    sign_in_as: &'static str,
    /// One entry per outstanding sign-in: the flow it takes, and WHICH app client of
    /// the pool it goes through.
    lineages: &'static [(Route, usize)],
}

/// The rows differ along the number of outstanding sign-ins, the app client each one
/// went through, the flow it took, and the case the username was spelled in.
const SIGN_OUT_CASES: &[SignOutCase] = &[
    SignOutCase {
        case: "one self-service sign-in",
        case_sensitive: true,
        username: "ada",
        sign_in_as: "ada",
        lineages: &[(Route::SelfService, 0)],
    },
    SignOutCase {
        case: "two self-service sign-ins through one app client",
        case_sensitive: true,
        username: "ada",
        sign_in_as: "ada",
        lineages: &[(Route::SelfService, 0), (Route::SelfService, 0)],
    },
    SignOutCase {
        case: "self-service sign-ins through two app clients of the pool",
        case_sensitive: true,
        username: "ada",
        sign_in_as: "ada",
        lineages: &[(Route::SelfService, 0), (Route::SelfService, 1)],
    },
    SignOutCase {
        case: "a self-service sign-in beside an administrative one",
        case_sensitive: true,
        username: "ada",
        sign_in_as: "ada",
        lineages: &[(Route::SelfService, 0), (Route::Admin, 0)],
    },
    SignOutCase {
        case: "three sign-ins across both app clients and both flows",
        case_sensitive: true,
        username: "grace.hopper@example.com",
        sign_in_as: "grace.hopper@example.com",
        lineages: &[
            (Route::SelfService, 0),
            (Route::Admin, 1),
            (Route::SelfService, 1),
        ],
    },
    SignOutCase {
        case: "a sign-in spelling the username in another case, in a case-insensitive pool",
        case_sensitive: false,
        username: "Grace-Hopper",
        sign_in_as: "GRACE-HOPPER",
        lineages: &[(Route::SelfService, 0), (Route::Admin, 0)],
    },
];

/// A sign-out invalidates every token issued to the user its access token names,
/// leaves the stored profile and every other user's tokens alone, and leaves that
/// profile able to sign in again.
#[tokio::test]
async fn a_sign_out_invalidates_every_token_issued_to_the_user_its_token_names() {
    for row in SIGN_OUT_CASES {
        let case = row.case;
        let emulator = emulator();
        let casing = json!({ "UsernameConfiguration": { "CaseSensitive": row.case_sensitive } });

        let user_pool_id = pool(&emulator, "sign-out-pool", casing.clone()).await;
        let client_count = row
            .lineages
            .iter()
            .map(|(_, client)| client + 1)
            .max()
            .expect("a row signs its user in at least once");
        let mut clients = Vec::new();
        for at in 0..client_count {
            clients.push(
                app_client(
                    &emulator,
                    &user_pool_id,
                    &format!("sign-out-client-{at}"),
                    json!({}),
                )
                .await,
            );
        }
        let stored_username = create_user(&emulator, &user_pool_id, row.username).await;

        // Another user of the SAME pool, and a NAMESAKE of the same name in another
        // pool: the sign-out names a user, so neither is signed out by it.
        let bystander = create_user(&emulator, &user_pool_id, "bystander").await;
        let bystander_tokens = sign_in(
            &emulator,
            Route::SelfService,
            &user_pool_id,
            &clients[0],
            &bystander,
        )
        .await;
        let other_pool_id = pool(&emulator, "namesake-pool", casing).await;
        let other_client = app_client(&emulator, &other_pool_id, "namesake-client", json!({})).await;
        let namesake = create_user(&emulator, &other_pool_id, row.username).await;
        let namesake_tokens = sign_in(
            &emulator,
            Route::SelfService,
            &other_pool_id,
            &other_client,
            row.sign_in_as,
        )
        .await;

        let mut lineages = Vec::new();
        for (route, client) in row.lineages {
            lineages.push(
                sign_in(
                    &emulator,
                    *route,
                    &user_pool_id,
                    &clients[*client],
                    row.sign_in_as,
                )
                .await,
            );
        }

        let profile_before = ok(
            &emulator,
            "AdminGetUser",
            &json!({ "UserPoolId": &user_pool_id, "Username": &stored_username }),
        )
        .await;

        // The sign-out is authorized by the FIRST sign-in's access token, and models no
        // output member at all: what it asserts is the revocation every later token
        // check reads.
        let (status, payload) = dispatch(
            &emulator,
            "GlobalSignOut",
            &json!({ "AccessToken": lineages[0].access_token }),
            false,
        )
        .await;
        assert_eq!(
            (status, &payload),
            (200, &json!({})),
            "case {case:?}: the sign-out must be served with no output member, got {status} \
             with {payload}"
        );

        for (at, tokens) in lineages.iter().enumerate() {
            // The ACCESS token of each sign-in, including the one that authorized the
            // call, no longer authorizes a self-service read.
            let (status, payload) = dispatch(
                &emulator,
                "GetUser",
                &json!({ "AccessToken": tokens.access_token }),
                false,
            )
            .await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, NOT_AUTHORIZED, ACCESS_TOKEN_REVOKED),
                "case {case:?}: sign-in {at}'s access token must be refused as revoked, got \
                 {status} with {payload}"
            );

            // The ID token of each sign-in is invalidated too: no operation takes one,
            // so the stored record this service wrote is what reports its state.
            let record = token_record(&emulator, &tokens.id_token).unwrap_or_else(|| {
                panic!("case {case:?}: sign-in {at}'s id token must be recorded")
            });
            assert_eq!(
                (record.kind, record.revoked),
                (state::TokenKind::Id, true),
                "case {case:?}: sign-in {at}'s id token must be recorded as a revoked id \
                 token, got {record:?}"
            );

            // The REFRESH token of each sign-in mints nothing further.
            let (status, payload) = dispatch(
                &emulator,
                "InitiateAuth",
                &json!({
                    "ClientId": tokens.client_id,
                    "AuthFlow": "REFRESH_TOKEN_AUTH",
                    "AuthParameters": { "REFRESH_TOKEN": tokens.refresh_token },
                }),
                false,
            )
            .await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, NOT_AUTHORIZED, REFRESH_TOKEN_REVOKED),
                "case {case:?}: sign-in {at}'s refresh token must be refused as revoked, got \
                 {status} with {payload}"
            );
        }

        // The stored profile is untouched: this call signs the user out, it neither
        // deactivates nor modifies it.
        let profile_after = ok(
            &emulator,
            "AdminGetUser",
            &json!({ "UserPoolId": &user_pool_id, "Username": &stored_username }),
        )
        .await;
        assert_eq!(
            profile_after, profile_before,
            "case {case:?}: expected the stored profile {profile_before} unchanged, got \
             {profile_after}"
        );

        // Nobody else was signed out, and the signed-out user signs in again.
        still_authorizes(
            &emulator,
            case,
            "another user of the same pool",
            &bystander_tokens,
            &bystander,
        )
        .await;
        still_authorizes(
            &emulator,
            case,
            "a namesake in another pool",
            &namesake_tokens,
            &namesake,
        )
        .await;
        let (route, client) = row.lineages[0];
        let again = sign_in(
            &emulator,
            route,
            &user_pool_id,
            &clients[client],
            row.sign_in_as,
        )
        .await;
        still_authorizes(
            &emulator,
            case,
            "a sign-in made after the sign-out",
            &again,
            &stored_username,
        )
        .await;
    }
}

/// What the `AccessToken` of a refused sign-out is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// A value this service never issued.
    NeverIssued,
    /// The ID token of a live sign-in, which is not the token this member asks for.
    IdToken,
    /// The REFRESH token of a live sign-in.
    RefreshToken,
    /// An access token an earlier sign-out already invalidated.
    AlreadySignedOut,
    /// A live access token whose user has since been deleted.
    UserDeleted,
    /// A live access token whose pool has since been deleted. Deleting a pool
    /// forgets every token it issued, so the value presented resolves to no access
    /// token of this service at all — which is the same answer the rest of the
    /// self-service surface gives it.
    PoolDeleted,
    /// A live access token of a user Amazon Cognito is waiting on a new password from.
    PasswordResetRequired,
    /// The machine-to-machine access token of an app client, which is issued to no
    /// user and carries no self-service scope.
    MachineToMachine,
}

/// One row of the refusal table: the token presented, and the refusal it owes.
struct RefusalCase {
    case: &'static str,
    presented: Presented,
    error_code: &'static str,
    message: &'static str,
}

/// The rows span the rejection classes this operation tells apart, each with the error
/// class and sentence it reports.
const REFUSAL_CASES: &[RefusalCase] = &[
    RefusalCase {
        case: "a value this service never issued",
        presented: Presented::NeverIssued,
        error_code: NOT_AUTHORIZED,
        message: INVALID_ACCESS_TOKEN,
    },
    RefusalCase {
        case: "the id token of a live sign-in",
        presented: Presented::IdToken,
        error_code: NOT_AUTHORIZED,
        message: INVALID_ACCESS_TOKEN,
    },
    RefusalCase {
        case: "the refresh token of a live sign-in",
        presented: Presented::RefreshToken,
        error_code: NOT_AUTHORIZED,
        message: INVALID_ACCESS_TOKEN,
    },
    RefusalCase {
        case: "an access token an earlier sign-out already invalidated",
        presented: Presented::AlreadySignedOut,
        error_code: NOT_AUTHORIZED,
        message: ACCESS_TOKEN_REVOKED,
    },
    RefusalCase {
        case: "an access token whose user has been deleted",
        presented: Presented::UserDeleted,
        error_code: NOT_AUTHORIZED,
        message: INVALID_ACCESS_TOKEN,
    },
    RefusalCase {
        case: "an access token whose pool has been deleted",
        presented: Presented::PoolDeleted,
        error_code: NOT_AUTHORIZED,
        message: INVALID_ACCESS_TOKEN,
    },
    RefusalCase {
        case: "an access token of a user waiting on a new password",
        presented: Presented::PasswordResetRequired,
        error_code: PASSWORD_RESET_REQUIRED_EXCEPTION,
        message: PASSWORD_RESET_REQUIRED,
    },
    RefusalCase {
        case: "the machine-to-machine access token of an app client",
        presented: Presented::MachineToMachine,
        error_code: NOT_AUTHORIZED,
        message: MISSING_REQUIRED_SCOPES,
    },
];

/// Build the fixture `presented` describes, in its own pool, and return the value the
/// sign-out presents as its `AccessToken`.
async fn token_to_present(emulator: &Emulator, case: &str, presented: Presented) -> String {
    if presented == Presented::NeverIssued {
        return NEVER_ISSUED_TOKEN.to_string();
    }
    if presented == Presented::MachineToMachine {
        let user_pool_id = pool(emulator, "m2m-pool", json!({})).await;
        ok(
            emulator,
            "CreateResourceServer",
            &json!({
                "UserPoolId": &user_pool_id,
                "Identifier": "https://api.example.test",
                "Name": "the suite resource server",
                "Scopes": [{ "ScopeName": "read", "ScopeDescription": "the read scope" }],
            }),
        )
        .await;
        let client_id = app_client(
            emulator,
            &user_pool_id,
            "m2m-client",
            json!({
                "ClientSecret": CLIENT_SECRET,
                "ExplicitAuthFlows": [CLIENT_TOKEN_AUTH_FLOW],
                "AllowedOAuthFlows": ["client_credentials"],
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthScopes": ["https://api.example.test/read"],
            }),
        )
        .await;
        let granted = ok(
            emulator,
            "GetClientToken",
            &json!({ "ClientId": client_id, "Secret": CLIENT_SECRET }),
        )
        .await;
        return granted["ClientAuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the grant issues an access token"))
            .to_string();
    }

    let user_pool_id = pool(emulator, "refusal-pool", json!({})).await;
    let client_id = app_client(emulator, &user_pool_id, "refusal-client", json!({})).await;
    // A verified email, because a password reset is delivered to one: "cannot reset
    // password for the user as there is no registered/verified email or phone_number"
    // is what `AdminResetUserPassword` owes a profile holding neither.
    let username = create_user_with(
        emulator,
        &user_pool_id,
        "ada",
        &[("email", "ada@example.test"), ("email_verified", "true")],
    )
    .await;
    let tokens = sign_in(
        emulator,
        Route::SelfService,
        &user_pool_id,
        &client_id,
        &username,
    )
    .await;
    match presented {
        Presented::IdToken => tokens.id_token,
        Presented::RefreshToken => tokens.refresh_token,
        Presented::AlreadySignedOut => {
            let (status, payload) = dispatch(
                emulator,
                "GlobalSignOut",
                &json!({ "AccessToken": tokens.access_token }),
                false,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the first sign-out must be served, got {status} with {payload}"
            );
            tokens.access_token
        }
        Presented::UserDeleted => {
            ok(
                emulator,
                "AdminDeleteUser",
                &json!({ "UserPoolId": &user_pool_id, "Username": &username }),
            )
            .await;
            tokens.access_token
        }
        Presented::PoolDeleted => {
            ok(
                emulator,
                "DeleteUserPool",
                &json!({ "UserPoolId": &user_pool_id }),
            )
            .await;
            tokens.access_token
        }
        Presented::PasswordResetRequired => {
            ok(
                emulator,
                "AdminResetUserPassword",
                &json!({ "UserPoolId": &user_pool_id, "Username": &username }),
            )
            .await;
            tokens.access_token
        }
        Presented::NeverIssued | Presented::MachineToMachine => {
            unreachable!("case {case:?}: handled above")
        }
    }
}

/// A sign-out whose `AccessToken` names no signed-in user is refused with the class
/// that says why, and signs nobody out.
#[tokio::test]
async fn a_sign_out_whose_token_names_no_signed_in_user_is_refused_and_revokes_nothing() {
    for row in REFUSAL_CASES {
        let case = row.case;
        let emulator = emulator();

        // A sign-in of a user in a pool of its own, which no refused sign-out may
        // touch.
        let sentinel_pool_id = pool(&emulator, "sentinel-pool", json!({})).await;
        let sentinel_client = app_client(
            &emulator,
            &sentinel_pool_id,
            "sentinel-client",
            json!({}),
        )
        .await;
        let sentinel = create_user(&emulator, &sentinel_pool_id, "sentinel").await;
        let sentinel_tokens = sign_in(
            &emulator,
            Route::SelfService,
            &sentinel_pool_id,
            &sentinel_client,
            &sentinel,
        )
        .await;

        let token = token_to_present(&emulator, case, row.presented).await;
        let (status, payload) = dispatch(
            &emulator,
            "GlobalSignOut",
            &json!({ "AccessToken": token }),
            false,
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, row.error_code, row.message),
            "case {case:?}: expected a 400 {} saying {:?}, got {status} with {payload}",
            row.error_code,
            row.message
        );

        still_authorizes(
            &emulator,
            case,
            "the sign-in of another pool's user",
            &sentinel_tokens,
            &sentinel,
        )
        .await;

        // A token of a pool that is gone is the one row whose answer is decided by the
        // pool's deletion forgetting the tokens it issued rather than by this
        // operation's own error set, so it is also checked against a reader of the same
        // self-service surface: both resolve the same value to no access token.
        if row.presented == Presented::PoolDeleted {
            let (reader_status, reader_payload) = dispatch(
                &emulator,
                "GetUser",
                &json!({ "AccessToken": token }),
                false,
            )
            .await;
            assert_eq!(
                (
                    reader_status,
                    error_code(&reader_payload),
                    error_message(&reader_payload)
                ),
                (400, row.error_code, row.message),
                "case {case:?}: the self-service reader must refuse the same token the same \
                 way, got {reader_status} with {reader_payload}"
            );
        }
    }
}
