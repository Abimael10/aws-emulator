// SPDX-License-Identifier: CC0-1.0

//! WHEN a challenge response's required entries are checked, over the wire, driven exactly
//! as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.RespondToAuthChallenge` /
//! `.AdminRespondToAuthChallenge`).
//!
//! The rules under test:
//!
//! * **A challenge response names the user it answers for — "all of the following
//!   challenges require USERNAME and, when the app client has a client secret,
//!   SECRET_HASH in the parameters"
//!   (`smithy_query(query="doc",shape="RespondToAuthChallengeRequest",member="ChallengeName")`)
//!   — and the presence of that entry is decided on the REQUEST, before the stored session,
//!   the user it was started for or that user's own MFA state is looked at: a response
//!   carrying no `USERNAME` is refused `InvalidParameterException` (400) with "Missing
//!   required parameter USERNAME", whichever challenge is outstanding.**
//! * **The `MFA_SETUP` challenge is answered with the setup itself — "the session ID from
//!   `VerifySoftwareToken`"
//!   (`smithy_query(query="doc",shape="RespondToAuthChallengeRequest",member="ChallengeResponses")`)
//!   — carried as the request's own `Session` or named in the response's `SESSION` entry: a
//!   response carrying neither answers no setup and is refused as a session that is not this
//!   request's (`NotAuthorizedException`, "Invalid session for the user."), again before the
//!   user's own factors are read.**
//!
//! Only the ORDER is these rules': a response that satisfies both still meets every check
//! behind them, including the one that refuses a setup nothing registered.
//!
//! The rows span what the rules generalise over: the challenge outstanding on the session
//! (`MFA_SETUP`, which is answered from the user's own MFA state, and
//! `NEW_PASSWORD_REQUIRED`, which is answered from the response's own entries), which
//! operation answers it (the self-service `RespondToAuthChallenge` and the administrative
//! `AdminRespondToAuthChallenge`), and what the response carries instead of the username (no
//! `ChallengeResponses` member at all, an empty map, and a map carrying the challenge's other
//! entries).
//!
//! The pools, app clients, users and sessions are real: they come from `CreateUserPool`,
//! `SetUserPoolMfaConfig`, `CreateUserPoolClient`, `AdminCreateUser`,
//! `AdminSetUserPassword` and `AdminInitiateAuth`.
//!
//! Each table row names the case it asserts and every assertion reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service. The signature is
/// never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The temporary password a user that owes a password change is created with.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2024";

/// A password a default pool's policy accepts.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The challenge a user of a pool that requires MFA with no factor registered owes.
const MFA_SETUP: &str = "MFA_SETUP";

/// The challenge an administrator-created user owes.
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";

/// The refusal of a response that names no user: the error class, and the sentence naming
/// the entry the response has to carry.
const INVALID_PARAMETER: &str = "InvalidParameterException";
const MISSING_USERNAME: &str = "Missing required parameter USERNAME";

/// The refusal an `MFA_SETUP` answer earns once the response DOES name the user but carries
/// no setup session: the session is not one this request may answer the challenge with.
const NOT_AUTHORIZED: &str = "NotAuthorizedException";
const INVALID_SESSION: &str = "Invalid session for the user.";

/// The refusal an `MFA_SETUP` answer earns once the response carries both, and the user
/// registered no software token: the check behind the two above, still reached.
const SOFTWARE_TOKEN_NOT_FOUND: &str = "SoftwareTokenMFANotFoundException";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
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

/// The `message` member of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// A pool and the app client every row signs in through.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Create a pool named `name`, requiring MFA with the software token offered when
/// `requires_mfa`, with one app client that allows the password flows.
async fn fixture(router: &MultiServiceRouter, name: &str, requires_mfa: bool) -> Fixture {
    let created = ok(router, "CreateUserPool", &json!({ "PoolName": name })).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    if requires_mfa {
        ok(
            router,
            "SetUserPoolMfaConfig",
            &json!({
                "UserPoolId": user_pool_id,
                "MfaConfiguration": "ON",
                "SoftwareTokenMfaConfiguration": { "Enabled": true },
            }),
        )
        .await;
    }
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "challenge-entries-client",
            "ExplicitAuthFlows": [
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH"
            ],
        }),
    )
    .await["UserPoolClient"]
        .clone();
    Fixture {
        user_pool_id,
        client_id: client["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("a minted client id: {client}"))
            .to_string(),
    }
}

/// Create `username` in `fixture`, sign it in, and return the `Session` of the challenge
/// the sign-in is answered with — `MFA_SETUP` for a user of a pool that requires MFA, and
/// `NEW_PASSWORD_REQUIRED` for one that still owes its password change.
async fn challenged(
    router: &MultiServiceRouter,
    fixture: &Fixture,
    username: &str,
    challenge: &str,
) -> String {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "Username": username,
            "TemporaryPassword": TEMPORARY_PASSWORD,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    let password = if challenge == NEW_PASSWORD_REQUIRED {
        TEMPORARY_PASSWORD
    } else {
        ok(
            router,
            "AdminSetUserPassword",
            &json!({
                "UserPoolId": fixture.user_pool_id,
                "Username": username,
                "Password": GOOD_PASSWORD,
                "Permanent": true,
            }),
        )
        .await;
        GOOD_PASSWORD
    };
    let started = ok(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "ClientId": fixture.client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": password },
        }),
    )
    .await;
    assert_eq!(
        started["ChallengeName"], json!(challenge),
        "the sign-in must leave the {challenge} challenge outstanding, got {started}"
    );
    started["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("the {challenge} challenge carries a session: {started}"))
        .to_string()
}

/// The body one of the two response operations takes for `challenge` on `session`, carrying
/// `responses` as its `ChallengeResponses` — or no such member at all when `responses` is
/// `None`.
fn response_body(
    operation: &str,
    fixture: &Fixture,
    challenge: &str,
    session: &str,
    responses: Option<Value>,
) -> Value {
    let mut body = Map::new();
    if operation == "AdminRespondToAuthChallenge" {
        body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
    }
    body.insert("ClientId".to_string(), json!(fixture.client_id));
    body.insert("ChallengeName".to_string(), json!(challenge));
    body.insert("Session".to_string(), json!(session));
    if let Some(responses) = responses {
        body.insert("ChallengeResponses".to_string(), responses);
    }
    Value::Object(body)
}

/// One row: the challenge outstanding, the operation answering it, and the responses the
/// answer carries instead of a username.
struct Row {
    case: &'static str,
    challenge: &'static str,
    operation: &'static str,
    responses: Option<Value>,
}

#[tokio::test]
async fn a_response_naming_no_user_is_refused_before_any_stored_state_is_read() {
    let router = router();

    let rows = vec![
        Row {
            case: "an MFA setup answer with no ChallengeResponses member at all",
            challenge: MFA_SETUP,
            operation: "RespondToAuthChallenge",
            responses: None,
        },
        Row {
            case: "an MFA setup answer carrying an empty response map",
            challenge: MFA_SETUP,
            operation: "RespondToAuthChallenge",
            responses: Some(json!({})),
        },
        Row {
            case: "an MFA setup answer carrying another entry instead of the username",
            challenge: MFA_SETUP,
            operation: "RespondToAuthChallenge",
            responses: Some(json!({ "SOFTWARE_TOKEN_MFA_CODE": "123456" })),
        },
        Row {
            case: "an administrative MFA setup answer with no ChallengeResponses member",
            challenge: MFA_SETUP,
            operation: "AdminRespondToAuthChallenge",
            responses: None,
        },
        Row {
            case: "an administrative MFA setup answer carrying another entry",
            challenge: MFA_SETUP,
            operation: "AdminRespondToAuthChallenge",
            responses: Some(json!({ "DEVICE_KEY": "us-east-1_device" })),
        },
        Row {
            case: "a password-change answer carrying the new password alone",
            challenge: NEW_PASSWORD_REQUIRED,
            operation: "RespondToAuthChallenge",
            responses: Some(json!({ "NEW_PASSWORD": GOOD_PASSWORD })),
        },
        Row {
            case: "an administrative password-change answer carrying the new password alone",
            challenge: NEW_PASSWORD_REQUIRED,
            operation: "AdminRespondToAuthChallenge",
            responses: Some(json!({ "NEW_PASSWORD": GOOD_PASSWORD })),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let requires_mfa = row.challenge == MFA_SETUP;
        let fixture = fixture(&router, &format!("entries-pool-{index}"), requires_mfa).await;
        let username = format!("unnamed-user-{index}");
        let session = challenged(&router, &fixture, &username, row.challenge).await;
        let body = response_body(
            row.operation,
            &fixture,
            row.challenge,
            &session,
            row.responses.clone(),
        );
        let (status, payload) = call(&router, row.operation, &body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, INVALID_PARAMETER),
            "case {:?}: expected a 400 {INVALID_PARAMETER}, got {status} with {payload}",
            row.case
        );
        let message = error_message(&payload);
        assert_eq!(
            message, MISSING_USERNAME,
            "case {:?}: expected {MISSING_USERNAME:?}, got {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_setup_challenge_is_answered_only_with_a_response_carrying_a_setup_session() {
    let router = router();

    // The rows differ along WHAT the response carries as the setup it completes — nothing,
    // an entry with no text in it, and an entry naming a session — and along which
    // operation answers the challenge. The session a `VerifySoftwareToken` really handed
    // out is the served case, which `mfa_sign_in_challenge.rs` drives end to end.
    let carried: &[(&str, Option<&str>, &str, &str)] = &[
        (
            "no setup session at all",
            None,
            NOT_AUTHORIZED,
            INVALID_SESSION,
        ),
        (
            "a SESSION entry with no text in it",
            Some(""),
            NOT_AUTHORIZED,
            INVALID_SESSION,
        ),
        (
            "a SESSION entry naming a session",
            Some("{session}"),
            SOFTWARE_TOKEN_NOT_FOUND,
            "Software token MFA is not registered for the user.",
        ),
    ];

    for operation in ["RespondToAuthChallenge", "AdminRespondToAuthChallenge"] {
        for (index, (case, entry, expected_code, expected_message)) in
            carried.iter().enumerate()
        {
            let fixture = fixture(
                &router,
                &format!("setup-session-pool-{operation}-{index}"),
                true,
            )
            .await;
            let username = format!("setup-user-{index}");
            let session = challenged(&router, &fixture, &username, MFA_SETUP).await;
            let mut responses = Map::new();
            responses.insert("USERNAME".to_string(), json!(username));
            if let Some(entry) = entry {
                let value = if *entry == "{session}" {
                    session.clone()
                } else {
                    (*entry).to_string()
                };
                responses.insert("SESSION".to_string(), json!(value));
            }
            let body = response_body(
                operation,
                &fixture,
                MFA_SETUP,
                &session,
                Some(Value::Object(responses)),
            );
            let (status, payload) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, *expected_code, *expected_message),
                "case {case:?} on {operation}: expected a 400 {expected_code} \
                 {expected_message:?}, got {status} with {payload}"
            );
        }
    }
}

#[tokio::test]
async fn a_response_that_names_the_user_still_meets_the_checks_behind_the_entry() {
    let router = router();

    // The boundary of the rules: they moved the entry checks ahead of the stored state,
    // they did not remove what is behind them. An MFA setup answer that names the user but
    // presents the sign-in's own session answers no setup, so it is refused as a session
    // that is not this request's.
    let mfa_fixture = fixture(&router, "named-user-mfa-pool", true).await;
    let mfa_user = "named-mfa-user";
    let mfa_session = challenged(&router, &mfa_fixture, mfa_user, MFA_SETUP).await;
    let (status, payload) = call(
        &router,
        "RespondToAuthChallenge",
        &response_body(
            "RespondToAuthChallenge",
            &mfa_fixture,
            MFA_SETUP,
            &mfa_session,
            Some(json!({ "USERNAME": mfa_user })),
        ),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, NOT_AUTHORIZED, INVALID_SESSION),
        "an answer carrying no setup session is refused as an invalid session, got {status} \
         with {payload}"
    );

    // A response that DOES carry a setup session reaches the user's own MFA state, which
    // refuses a setup nothing registered.
    let (status, payload) = call(
        &router,
        "RespondToAuthChallenge",
        &response_body(
            "RespondToAuthChallenge",
            &mfa_fixture,
            MFA_SETUP,
            &mfa_session,
            Some(json!({ "USERNAME": mfa_user, "SESSION": mfa_session.clone() })),
        ),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, SOFTWARE_TOKEN_NOT_FOUND),
        "an answer carrying a setup session is decided on that user's MFA state, got {status} \
         with {payload}"
    );

    // And a password-change answer that names the user and carries the password completes
    // the sign-in it was waiting on.
    let password_fixture = fixture(&router, "named-user-password-pool", false).await;
    let password_user = "named-password-user";
    let session = challenged(
        &router,
        &password_fixture,
        password_user,
        NEW_PASSWORD_REQUIRED,
    )
    .await;
    let completed = ok(
        &router,
        "RespondToAuthChallenge",
        &response_body(
            "RespondToAuthChallenge",
            &password_fixture,
            NEW_PASSWORD_REQUIRED,
            &session,
            Some(json!({ "USERNAME": password_user, "NEW_PASSWORD": GOOD_PASSWORD })),
        ),
    )
    .await;
    assert!(
        completed["AuthenticationResult"]["AccessToken"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "an answered password change completes the sign-in, got {completed}"
    );
}
