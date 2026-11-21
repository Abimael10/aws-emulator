// SPDX-License-Identifier: CC0-1.0

//! How a challenge response is refused when its `Session` half resolves no
//! outstanding challenge, driven through the router exactly as an SDK drives it
//! (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a challenge response authenticates with the PAIR of its
//! `Session` and the parameters its challenge requires, so a session half that
//! resolves no outstanding challenge of the request — a value this service never
//! handed out, one started for another pool or app client, or one started for
//! another user than the response names — is refused as that pair failing:
//! `CodeMismatchException`, HTTP 400, "thrown if the provided code doesn't match
//! what the server was expecting"
//! (`smithy_query(query="operation",operation="AdminRespondToAuthChallenge,RespondToAuthChallenge")`),
//! with one sentence that names neither half. A session this service DID hand
//! out for the request and has since spent is a state of that session and keeps
//! its own `NotAuthorizedException` wording, and a refused response consumes
//! nothing: the session it named is still answerable afterwards.
//!
//! Each table row names the case it asserts and every assertion reports the
//! value it actually saw.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
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

/// The challenge an administrator-created user has to answer.
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";

/// The status every refusal of the caller carries on this endpoint.
const REFUSED: u16 = 400;

/// The sentence a session half that resolves no outstanding challenge earns.
const CHALLENGE_SESSION_MISMATCH: &str = "Invalid code/session provided";

/// The sentence a session this request DID hand out and already answered earns,
/// whose single-use clause is what tells the two apart.
const SESSION_ALREADY_USED: &str = "Invalid session for the user, session can only be used once.";

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

/// One user pool with one app client that admits the administrative and the
/// client-side password flows.
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
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_ADMIN_USER_PASSWORD_AUTH"],
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

/// Create `username` in `fixture` owing a password change, sign it in and answer
/// the `Session` of the `NEW_PASSWORD_REQUIRED` challenge it is waiting on.
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
    let started = ok(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "ClientId": fixture.client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": TEMPORARY_PASSWORD },
        }),
    )
    .await;
    assert_eq!(
        started["ChallengeName"].as_str(),
        Some(NEW_PASSWORD_REQUIRED),
        "the sign-in of a user owing a password change raises its challenge, got {started}"
    );
    started["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("a raised challenge hands out a session, got {started}"))
        .to_string()
}

/// A challenge response body for `operation`: the administrative operation names
/// the pool, the client-side one does not.
fn response_body(
    operation: &str,
    user_pool_id: &str,
    client_id: &str,
    session: &str,
    username: &str,
) -> Value {
    let mut body = Map::new();
    if operation == "AdminRespondToAuthChallenge" {
        body.insert("UserPoolId".to_string(), json!(user_pool_id));
    }
    body.insert("ClientId".to_string(), json!(client_id));
    body.insert("ChallengeName".to_string(), json!(NEW_PASSWORD_REQUIRED));
    body.insert("Session".to_string(), json!(session));
    body.insert(
        "ChallengeResponses".to_string(),
        json!({ "USERNAME": username, "NEW_PASSWORD": CHOSEN_PASSWORD }),
    );
    Value::Object(body)
}

/// Where the session a row presents comes from.
enum Presented {
    /// A value this service never handed out, of the given shape.
    Unissued(String),
    /// The session of a challenge raised on ANOTHER app client of the pool.
    OtherClient,
    /// The session of a challenge raised in ANOTHER user pool.
    OtherPool,
    /// This request's own session, with the response naming another user.
    AnotherUser,
}

/// One row of the table: the session it presents and the refusal it earns.
struct SessionCase {
    case: &'static str,
    presented: Presented,
}

#[tokio::test]
async fn a_session_that_resolves_no_outstanding_challenge_is_one_code_session_mismatch() {
    let router = router();
    let here = fixture(&router, "session-class-here").await;
    let elsewhere = fixture(&router, "session-class-elsewhere").await;
    let other_client = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": here.user_pool_id,
            "ClientName": "session-class-other-client",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_ADMIN_USER_PASSWORD_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();

    // The rows differ along what the rule generalises over: WHY the session half
    // resolves no outstanding challenge of this request — a value nothing handed
    // out, at each bound of the modelled `SessionType` length 20..4096 and
    // between them; a session of another app client; a session of another pool;
    // and a session of this request answered for another user.
    let rows = [
        SessionCase {
            case: "a session at the modelled minimum length",
            presented: Presented::Unissued("a".repeat(20)),
        },
        SessionCase {
            case: "a session of a shape a token generator might produce",
            presented: Presented::Unissued(format!("AYABe{}", "Z".repeat(60))),
        },
        SessionCase {
            case: "a session at the modelled maximum length",
            presented: Presented::Unissued("s".repeat(4096)),
        },
        SessionCase {
            case: "a session started on another app client of the pool",
            presented: Presented::OtherClient,
        },
        SessionCase {
            case: "a session started in another user pool",
            presented: Presented::OtherPool,
        },
        SessionCase {
            case: "a response naming another user than the session was started for",
            presented: Presented::AnotherUser,
        },
    ];

    for operation in ["AdminRespondToAuthChallenge", "RespondToAuthChallenge"] {
        for (index, row) in rows.iter().enumerate() {
            let case = format!("{operation} with {}", row.case);
            let username = format!("session-class-{operation}-{index}");
            // This request's OWN outstanding challenge, which every row leaves
            // answerable.
            let own = challenge_session(&router, &here, &username).await;
            let (session, responded_for) = match &row.presented {
                Presented::Unissued(value) => (value.clone(), username.clone()),
                Presented::OtherClient => (
                    challenge_session(
                        &router,
                        &Fixture {
                            user_pool_id: here.user_pool_id.clone(),
                            client_id: other_client.clone(),
                        },
                        &format!("{username}-other-client"),
                    )
                    .await,
                    format!("{username}-other-client"),
                ),
                Presented::OtherPool => (
                    challenge_session(&router, &elsewhere, &format!("{username}-abroad")).await,
                    format!("{username}-abroad"),
                ),
                Presented::AnotherUser => {
                    let stranger = format!("{username}-stranger");
                    challenge_session(&router, &here, &stranger).await;
                    (own.clone(), stranger)
                }
            };

            let (status, payload) = call(
                &router,
                operation,
                &response_body(
                    operation,
                    &here.user_pool_id,
                    &here.client_id,
                    &session,
                    &responded_for,
                ),
            )
            .await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (REFUSED, "CodeMismatchException", CHALLENGE_SESSION_MISMATCH),
                "case {case:?}: expected the code/session mismatch class, got {status} with \
                 {payload}"
            );
            // The sentence reports the pair, so it quotes neither half of it.
            assert!(
                !error_message(&payload).contains(session.as_str())
                    && !error_message(&payload).contains(responded_for.as_str()),
                "case {case:?}: the refusal must quote neither half of the pair, got {payload}"
            );

            // The refusal consumed nothing: this request's own session still
            // answers the challenge it is waiting on.
            let answered = ok(
                &router,
                operation,
                &response_body(
                    operation,
                    &here.user_pool_id,
                    &here.client_id,
                    &own,
                    &username,
                ),
            )
            .await;
            assert!(
                answered["AuthenticationResult"]["AccessToken"].is_string(),
                "case {case:?}: the refused response must leave the session answerable, got \
                 {answered}"
            );
        }
    }
}

#[tokio::test]
async fn a_session_this_request_already_answered_keeps_the_single_use_clause() {
    let router = router();
    let here = fixture(&router, "session-replay").await;

    // The rows differ along the operation that carries the replay, which is the
    // pair of operations that answer a challenge at all.
    for operation in ["AdminRespondToAuthChallenge", "RespondToAuthChallenge"] {
        let username = format!("session-replay-{operation}");
        let session = challenge_session(&router, &here, &username).await;
        let body = response_body(
            operation,
            &here.user_pool_id,
            &here.client_id,
            &session,
            &username,
        );
        let answered = ok(&router, operation, &body).await;
        assert!(
            answered["AuthenticationResult"]["AccessToken"].is_string(),
            "case {operation:?}: the first response completes the sign-in, got {answered}"
        );

        // The session this request DID hand out and has since spent is a state
        // of that session, not the response's pair failing.
        let (status, payload) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, "NotAuthorizedException", SESSION_ALREADY_USED),
            "case {operation:?}: expected the single-use clause, got {status} with {payload}"
        );
    }
}
