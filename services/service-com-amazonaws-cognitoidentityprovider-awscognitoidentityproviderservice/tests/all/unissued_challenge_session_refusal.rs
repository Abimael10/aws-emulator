// SPDX-License-Identifier: CC0-1.0

//! How the CLIENT-SIDE challenge response refuses a `Session` this emulator is
//! holding no outstanding challenge under, driven through the router exactly as
//! an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.RespondToAuthChallenge`).
//!
//! The rule under test: a challenge response authenticates with the PAIR of its
//! `Session` and the parameters its challenge requires, so a session half that
//! resolves no outstanding challenge of the request is refused as that pair
//! failing — `CodeMismatchException`, which is "thrown if the provided code
//! doesn't match what the server was expecting" and is modelled HTTP 400
//! (`smithy_query(query="operation",operation="RespondToAuthChallenge")`) —
//! carrying one sentence that quotes neither half. The classification is a fact
//! about the pair alone: it does not depend on the `ChallengeName` the response
//! claims, on the user it names, or on the state the pool's own users are in,
//! and the refusal moves nothing, so every session this emulator DID hand out
//! stays answerable.
//!
//! Each table row names the case it asserts and every assertion reports the
//! value it actually saw.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The temporary password every fixture user is created with, which satisfies a
/// default pool's password policy.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2024";

/// The password every fixture response chooses, which satisfies that policy too.
const CHOSEN_PASSWORD: &str = "Ch0senPassw0rd!";

/// The challenge a user created with a temporary password owes.
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";

/// The status the user of a pool that is waiting for that answer carries.
const FORCE_CHANGE_PASSWORD: &str = "FORCE_CHANGE_PASSWORD";

/// The status a user whose answer completed the sign-in carries.
const CONFIRMED: &str = "CONFIRMED";

/// The status the modelled `CodeMismatchException` is refused with.
const REFUSED: u16 = 400;

/// The error code a session half that resolves no outstanding challenge earns.
const CODE_MISMATCH: &str = "CodeMismatchException";

/// The sentence that refusal carries, which quotes nothing of the request.
const CHALLENGE_SESSION_MISMATCH: &str = "Invalid code/session provided";

/// The modelled minimum length of a `Session`
/// (`smithy_query(query="shape",shape="SessionType")`: `@length` 20..4096).
const SESSION_MIN_LENGTH: usize = 20;

/// The modelled maximum length of a `Session` (same source).
const SESSION_MAX_LENGTH: usize = 4096;

/// Challenge names a response can claim, from `ChallengeNameType`
/// (`smithy_query(query="shape",shape="ChallengeNameType")`): the one the
/// fixture's own challenge is waiting on, the factor challenges, the two
/// selection challenges and the custom one.
const CLAIMED_CHALLENGES: [&str; 7] = [
    NEW_PASSWORD_REQUIRED,
    "EMAIL_OTP",
    "SOFTWARE_TOKEN_MFA",
    "SMS_MFA",
    "SELECT_MFA_TYPE",
    "MFA_SETUP",
    "CUSTOM_CHALLENGE",
];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
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

/// One user pool with one app client that admits the client-side password flow.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Create a pool and an app client for `name`.
async fn fixture(router: &MultiServiceRouter, name: &str) -> Fixture {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({ "PoolName": format!("{name}-pool") }),
    )
    .await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{name}-client"),
            "ExplicitAuthFlows": [
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
                "ALLOW_USER_SRP_AUTH",
            ],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// The `UserStatus` the pool reports for `username`.
async fn user_status(router: &MultiServiceRouter, fixture: &Fixture, username: &str) -> String {
    let read = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": fixture.user_pool_id, "Username": username }),
    )
    .await;
    read["UserStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("AdminGetUser reports a status, got {read}"))
        .to_string()
}

/// Create `username` owing a password change and sign it in through the
/// client-side password flow, returning the `Session` of the challenge the
/// sign-in raised.
async fn challenge_session(
    router: &MultiServiceRouter,
    fixture: &Fixture,
    username: &str,
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
    assert_eq!(
        user_status(router, fixture, username).await,
        FORCE_CHANGE_PASSWORD,
        "case {username:?}: a user created with a temporary password owes the change"
    );
    let started = ok(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": fixture.client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": TEMPORARY_PASSWORD },
        }),
    )
    .await;
    assert_eq!(
        started["ChallengeName"].as_str(),
        Some(NEW_PASSWORD_REQUIRED),
        "case {username:?}: the sign-in of a user owing a password change raises its challenge, \
         got {started}"
    );
    started["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("a raised challenge hands out a session, got {started}"))
        .to_string()
}

/// A `RespondToAuthChallenge` body: this operation models no `UserPoolId`, so
/// the app client is what names the pool.
fn response_body(client_id: &str, challenge_name: &str, session: &str, username: &str) -> Value {
    json!({
        "ClientId": client_id,
        "ChallengeName": challenge_name,
        "Session": session,
        "ChallengeResponses": { "USERNAME": username, "NEW_PASSWORD": CHOSEN_PASSWORD },
    })
}

/// Answer the challenge `session` is waiting on for `username`, requiring the
/// tokens a completed sign-in mints.
async fn answer(
    router: &MultiServiceRouter,
    fixture: &Fixture,
    session: &str,
    username: &str,
) -> Value {
    let answered = ok(
        router,
        "RespondToAuthChallenge",
        &response_body(
            &fixture.client_id,
            NEW_PASSWORD_REQUIRED,
            session,
            username,
        ),
    )
    .await;
    assert!(
        answered["AuthenticationResult"]["AccessToken"].is_string(),
        "case {username:?}: answering the challenge completes the sign-in, got {answered}"
    );
    answered
}

/// Assert `session` named beside `challenge_name` and `username` is refused as
/// the one code/session mismatch, quoting no value the request carried.
async fn assert_mismatch(
    router: &MultiServiceRouter,
    fixture: &Fixture,
    case: &str,
    challenge_name: &str,
    session: &str,
    username: &str,
) {
    let (status, payload) = call(
        router,
        "RespondToAuthChallenge",
        &response_body(&fixture.client_id, challenge_name, session, username),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, CODE_MISMATCH, CHALLENGE_SESSION_MISMATCH),
        "case {case:?}: expected the code/session mismatch class, got {status} with {payload}"
    );
    let message = error_message(&payload);
    assert!(
        !message.contains(session)
            && !message.contains(username)
            && !message.contains(challenge_name)
            && !message.contains(fixture.client_id.as_str()),
        "case {case:?}: the refusal must quote no value of the request, got {payload}"
    );
}

#[tokio::test]
async fn an_unissued_session_is_refused_as_a_code_session_mismatch_whatever_it_claims() {
    let router = router();
    let here = fixture(&router, "unissued-session").await;
    let username = "unissued-session-user";

    // The recorded flow first: the challenge is raised, answered, and the answer
    // is read back off the pool through `AdminGetUser`.
    let issued = challenge_session(&router, &here, username).await;
    answer(&router, &here, &issued, username).await;
    assert_eq!(
        user_status(&router, &here, username).await,
        CONFIRMED,
        "the answered challenge leaves the user confirmed"
    );

    // The rows differ along the FORM of a value that resolves no outstanding
    // challenge: each modelled `SessionType` bound, a length between them, an
    // opaque blob of the shape a token vendor produces, and the session this
    // pool did hand out either truncated to the modelled minimum or with a
    // character appended — stale, truncated and tampered alike.
    let truncated = issued
        .get(..SESSION_MIN_LENGTH)
        .expect("a minted session is longer than the modelled minimum")
        .to_string();
    let tampered = format!("{issued}x");
    let sessions = [
        ("a value at the modelled minimum length", "n".repeat(SESSION_MIN_LENGTH)),
        ("a value of no particular shape", "not-a-real-session-token".to_string()),
        ("a value of the shape a token vendor produces", format!("AYABe{}", "Q".repeat(120))),
        ("a value at the modelled maximum length", "s".repeat(SESSION_MAX_LENGTH)),
        ("the issued session truncated to the modelled minimum", truncated),
        ("the issued session with a character appended", tampered),
    ];

    for (shape, session) in &sessions {
        // The classification is a fact about the pair, so it holds whichever
        // challenge the response claims to be answering.
        for claimed in CLAIMED_CHALLENGES {
            assert_mismatch(
                &router,
                &here,
                &format!("{shape} claiming {claimed}"),
                claimed,
                session,
                username,
            )
            .await;
        }
    }

    // Every refusal moved nothing: the user the responses named is still the
    // confirmed user whose password the answered challenge stored.
    assert_eq!(
        user_status(&router, &here, username).await,
        CONFIRMED,
        "the refused responses leave the user confirmed"
    );
    let signed_in = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": here.client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": CHOSEN_PASSWORD },
        }),
    )
    .await;
    assert!(
        signed_in["AuthenticationResult"]["AccessToken"].is_string(),
        "the refused responses leave the stored password untouched, got {signed_in}"
    );
}

#[tokio::test]
async fn an_unissued_session_is_refused_whichever_user_the_response_names() {
    let router = router();
    let here = fixture(&router, "unissued-session-users").await;

    // A user that answered its challenge, and a user still owing one, whose own
    // session every refusal has to leave answerable.
    let confirmed = "unissued-session-users-confirmed";
    let issued = challenge_session(&router, &here, confirmed).await;
    answer(&router, &here, &issued, confirmed).await;
    let waiting = "unissued-session-users-waiting";
    let outstanding = challenge_session(&router, &here, waiting).await;

    // The rows differ along the user the response names: one the pool holds in
    // each of the two states, and one it holds no user under at all — a value
    // this emulator never handed out resolves no challenge for any of them, and
    // no row is told which.
    let named = [
        ("a user whose challenge is answered", confirmed),
        ("a user still owing its challenge", waiting),
        ("a username the pool holds no user under", "unissued-session-users-nobody"),
    ];
    for (case, username) in named {
        assert_mismatch(
            &router,
            &here,
            case,
            NEW_PASSWORD_REQUIRED,
            "not-a-real-session-token",
            username,
        )
        .await;
    }

    // The refusals consumed nothing: the outstanding challenge still answers.
    answer(&router, &here, &outstanding, waiting).await;
    assert_eq!(
        user_status(&router, &here, waiting).await,
        CONFIRMED,
        "the challenge the refusals left standing still confirms its user"
    );
}
