// SPDX-License-Identifier: CC0-1.0

//! How the two threat-protection FEEDBACK submissions refuse a request whose user,
//! event or token they cannot bind, over the wire (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rules under test:
//!
//! * `AdminUpdateAuthEventFeedback` resolves the user it leaves feedback for as the
//!   CREDENTIAL the request names a user by, so a `Username` the named pool holds no
//!   user for is refused with the credential class — HTTP 400,
//!   `NotAuthorizedException` in the body's `__type` and in the `x-amzn-errortype`
//!   header, carrying the fixed sentence that surface answers a credential it did not
//!   accept with — and never with a refusal that reports whether that user exists;
//! * `UpdateAuthEventFeedback` is authorized by the `FeedbackToken` Amazon Cognito
//!   minted for one event of one user, so it judges the request as ONE question and
//!   answers a request that binds to no such token with its session class — HTTP 400,
//!   `NotAuthorizedException`, the fixed invalid-session sentence — whichever part of
//!   the triple failed: the pool holds no such user, the user's history holds no such
//!   event, or the token is not the one minted for it;
//! * so the refusal a caller sees carries no information about pool membership: every
//!   row of each table is answered by the SAME status, error class, header and
//!   sentence, and none of them records feedback on anything;
//! * and `AdminUpdateAuthEventFeedback` resolves its `EventId` INSIDE the history of
//!   the user the same request names rather than addressing an event as a resource of
//!   its own, so an id that resolves to no event of that history — including one
//!   logged for another user — is refused as an invalid parameter value naming the
//!   member it arrived in (HTTP 400, `InvalidParameterException`).
//!
//! The pool-level gates keep their own classes, because they are properties of the
//! POOL rather than of the user asked about: a pool nothing created is still reported
//! as the absent resource it is, and a pool running no threat protection still reports
//! that its add-ons are off. The READER of the history (`AdminListUserAuthEvents`)
//! keeps its own user-existence refusal: it is not a submission authorized by a
//! credential or a token.
//!
//! The rows span what the rules generalise over rather than one request: the FORM of
//! the username that resolves to nothing (`UsernameType`, length 1..128 and pattern
//! `^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$` from
//! `smithy_query(query="operation",operation="AdminUpdateAuthEventFeedback")`) at both
//! published length bounds, the WAY it resolves to nothing (never created, created in
//! another pool), which `FeedbackValueType` verdict was submitted
//! (`smithy_query(query="shape",shape="FeedbackValueType")`), whether the event named
//! beside it exists, and — for the user's own submission — which part of the triple
//! failed, including a token minted for another event of the same user and one minted
//! for another user's event.
//!
//! Every pool, app client, user, event and feedback token here is real: they come from
//! `CreateUserPool`, `CreateUserPoolClient`, `AdminCreateUser`,
//! `AdminSetUserPassword` and `InitiateAuth`, and the stored feedback is read back only
//! through `AdminListUserAuthEvents`.
//!
//! Every assertion names its case and reports the value it saw.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state;

/// This service's SigV4 signing name, the 4th field of a credential scope, which is
/// what routes a signed request to it.
const SIGNING_NAME: &str = "cognito-idp";

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// This service's regional endpoint host, which a signed call is addressed to, and the
/// emulator's own address, which is what an unsigned client is pointed at.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The response header an AWS SDK resolves the concrete exception type from.
const ERRORTYPE: &str = "x-amzn-errortype";

/// The status this endpoint answers every refusal of the caller with.
pub const REFUSED: u16 = 400;

/// A password satisfying a default pool's password policy, so a created user can hold a
/// permanent one and sign in.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The users these tests create: two of the pool under test, and one of a second pool.
pub const USERNAME: &str = "feedback-user";
pub const OTHER_USERNAME: &str = "other-feedback-user";
pub const FOREIGN_POOL_USERNAME: &str = "user-of-another-pool";

/// An `EventIdType` value (pattern `^[\w+-]+$`) that names no logged event.
pub const ABSENT_EVENT_ID: &str = "n0-such-event";

/// A `TokenModelType` value (pattern `^[A-Za-z0-9-_=.]+$`) this emulator minted for no
/// event, so it authorizes feedback on none.
pub const FOREIGN_FEEDBACK_TOKEN: &str = "n0t-a-feedback-token";

/// A `UserPoolIdType` value (pattern `^[\w-]+_[0-9a-zA-Z]+$`) that names no pool.
const ABSENT_USER_POOL_ID: &str = "us-east-1_n0suchpool";

/// The `AdvancedSecurityModeType` value threat protection is ON under, and the one it
/// is off under (`smithy_query(query="shape",shape="AdvancedSecurityModeType")`).
const ENFORCED: &str = "ENFORCED";
const OFF: &str = "OFF";

/// The sentences the refusals under test carry.
const INCORRECT_CREDENTIALS_MESSAGE: &str = "Incorrect username or password.";
const EVENT_ID_INVALID_MESSAGE: &str = "EventId is invalid";
pub const FEEDBACK_TOKEN_INVALID_MESSAGE: &str = "Invalid session provided";
const USER_NOT_FOUND_MESSAGE: &str = "User does not exist.";
pub const USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE: &str = "This is an add on feature. Please update \
     AdvancedSecurityMode for your user pool to access this API.";

/// The published bounds of `UsernameType`'s modelled length (1..128), which the rows
/// name a user at both of.
const USERNAME_MIN_LENGTH: usize = 1;
const USERNAME_MAX_LENGTH: usize = 128;

/// The published bounds of `EventIdType`'s modelled length (1..50, pattern `^[\w+-]+$`
/// from `smithy_query(query="operation",operation="AdminUpdateAuthEventFeedback")`),
/// which the rows name an event at both of.
const EVENT_ID_MIN_LENGTH: usize = 1;
const EVENT_ID_MAX_LENGTH: usize = 50;

/// One emulator with just this service mounted, beside the engine it is mounted on —
/// which is where a test reads an event's `FeedbackToken` from, because no response
/// carries it.
pub fn router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.set_request_authentication(false);
    (router, engine)
}

/// What one call answered: its HTTP status, its `x-amzn-errortype` header and its JSON
/// body — the three values the rules are stated over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub status: u16,
    pub errortype: Option<String>,
    pub payload: Value,
}

/// Dispatch `operation` with `body`. A `signed` call carries a credential scope, as the
/// SDK sends every administrative operation; an unsigned one is addressed to the
/// emulator's own address, which is how the holder of a notification link calls the
/// user's own submission.
pub async fn dispatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    signed: bool,
) -> Answered {
    let host = if signed { ENDPOINT_HOST } else { EMULATOR_HOST };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"));
    if signed {
        request = request.header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        );
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let errortype = response
        .headers()
        .get(ERRORTYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    Answered {
        status,
        errortype,
        payload,
    }
}

/// Call the signed `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = dispatch(router, operation, body, true).await;
    assert_eq!(
        answered.status, 200,
        "{operation} must succeed, got {} with {}",
        answered.status, answered.payload
    );
    answered.payload
}

/// Call the unsigned `operation`, requiring success, and return its response body.
pub async fn ok_anonymous(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = dispatch(router, operation, body, false).await;
    assert_eq!(
        answered.status, 200,
        "{operation} must succeed, got {} with {}",
        answered.status, answered.payload
    );
    answered.payload
}

/// The `__type` of an error body, which is the wire error code.
pub fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
pub fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// A created pool and its app client.
pub struct Fixture {
    pub user_pool_id: String,
    pub client_id: String,
}

/// Create a pool whose `UserPoolAddOns` run threat protection in `mode` (and none at
/// all where `mode` is `None`) and one app client that admits the password sign-in.
///
/// `UserPoolAddOns` is a Plus-tier feature of a pool, so a pool configuring it is
/// created on that tier.
pub async fn given_pool(
    router: &MultiServiceRouter,
    pool_name: &str,
    mode: Option<&str>,
) -> Fixture {
    let mut pool = json!({ "PoolName": pool_name, "AutoVerifiedAttributes": ["email"] });
    if let Some(mode) = mode {
        pool["UserPoolTier"] = json!("PLUS");
        pool["UserPoolAddOns"] = json!({ "AdvancedSecurityMode": mode });
    }
    let created = ok(router, "CreateUserPool", &pool).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": &user_pool_id,
            "ClientName": "feedback-client",
            "GenerateSecret": false,
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted app client id: {created}"))
        .to_string();
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// Create `username` in `user_pool_id` holding [`GOOD_PASSWORD`] permanently, so it can
/// sign in and so its sign-ins are logged.
pub async fn given_user(router: &MultiServiceRouter, user_pool_id: &str, username: &str) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": [
                { "Name": "email", "Value": format!("{username}@example.com") },
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

/// Sign `username` in through `client_id` with the password it holds, which is what logs
/// one authentication event in a pool whose threat protection is active.
pub async fn sign_in(router: &MultiServiceRouter, client_id: &str, username: &str) {
    let answered = ok_anonymous(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    assert!(
        answered["AuthenticationResult"]["AccessToken"].is_string(),
        "a completed sign-in must issue tokens, got {answered}"
    );
}

/// The events `AdminListUserAuthEvents` reports for `username`, newest first.
pub async fn history(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
) -> Vec<Value> {
    let answered = ok(
        router,
        "AdminListUserAuthEvents",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    answered
        .get("AuthEvents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("the listing must carry an AuthEvents member: {answered}"))
}

/// The ids of the events logged for `username`, newest first.
pub async fn event_ids(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
) -> Vec<String> {
    history(router, user_pool_id, username)
        .await
        .iter()
        .map(|event| {
            event["EventId"]
                .as_str()
                .unwrap_or_else(|| panic!("a logged event carries an id: {event}"))
                .to_string()
        })
        .collect()
}

/// The `FeedbackToken` the pool minted for the logged event `event_id`, read out of the
/// emulator's own state the way a user reads it out of the notification email: it is a
/// secret of that event, so no response carries it.
pub fn feedback_token(
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    username: &str,
    event_id: &str,
) -> String {
    let service_state = state::service_state(&**engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .and_then(|user| {
            user.auth_events
                .iter()
                .find(|event| event.event_id == event_id)
        })
        .map(|event| event.feedback_token.clone())
        .expect("a logged event carries the feedback token minted for it")
}

/// The usernames that resolve to no user of the pool under test, spanning the forms
/// `UsernameType` admits and the ways a name resolves to nothing. The length bounds are
/// the modelled ones ([`USERNAME_MIN_LENGTH`], [`USERNAME_MAX_LENGTH`]).
pub fn unresolved_usernames() -> Vec<(String, String)> {
    vec![
        ("a name nothing created".to_string(), "nobody-at-all".to_string()),
        (
            "a name of the shortest length the model admits".to_string(),
            "n".repeat(USERNAME_MIN_LENGTH),
        ),
        (
            "a name of the greatest length the model admits".to_string(),
            "n".repeat(USERNAME_MAX_LENGTH),
        ),
        (
            "a name shaped like a user's sub".to_string(),
            "7f1c2d80-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "a name shaped like an email alias".to_string(),
            "ghost@example.com".to_string(),
        ),
        (
            "a name of letters with combining marks".to_string(),
            "nïcht-vörhanden".to_string(),
        ),
        (
            "a name a different pool holds a user for".to_string(),
            FOREIGN_POOL_USERNAME.to_string(),
        ),
    ]
}

/// RULE: `AdminUpdateAuthEventFeedback` refuses a `Username` the named pool holds no
/// user for with the CREDENTIAL class — HTTP 400, `NotAuthorizedException` in the
/// body and in the `x-amzn-errortype` header, carrying the fixed sentence a credential
/// that was not accepted earns — so the refusal reports nothing about whether that user
/// exists, and every row is answered identically.
#[tokio::test]
async fn the_administrative_feedback_refuses_an_unresolved_username_on_the_credential() {
    let (router, _engine) = router_and_engine();
    let fixture = given_pool(&router, "admin-feedback-masking", Some(ENFORCED)).await;
    given_user(&router, &fixture.user_pool_id, USERNAME).await;
    sign_in(&router, &fixture.client_id, USERNAME).await;
    let logged = event_ids(&router, &fixture.user_pool_id, USERNAME).await;
    let event_id = logged
        .first()
        .expect("the sign-in logged one event")
        .to_string();

    // A user of ANOTHER pool, so one row names a username this service holds a user
    // for while the pool under test does not.
    let elsewhere = given_pool(&router, "admin-feedback-other-pool", Some(ENFORCED)).await;
    given_user(&router, &elsewhere.user_pool_id, FOREIGN_POOL_USERNAME).await;

    let mut answers = Vec::new();
    for (case, username) in unresolved_usernames() {
        // Both `FeedbackValueType` verdicts, and an event that exists beside one that
        // does not: neither is what decides this refusal.
        for verdict in ["Valid", "Invalid"] {
            for (named_event, event) in [("a logged event", event_id.as_str()), ("an event nothing logged", ABSENT_EVENT_ID)] {
                let case = format!("{case} submitting {verdict} on {named_event}");
                let answered = dispatch(
                    &router,
                    "AdminUpdateAuthEventFeedback",
                    &json!({
                        "UserPoolId": &fixture.user_pool_id,
                        "Username": &username,
                        "EventId": event,
                        "FeedbackValue": verdict,
                    }),
                    true,
                )
                .await;
                assert_eq!(
                    (
                        answered.status,
                        error_code(&answered.payload),
                        error_message(&answered.payload),
                        answered.errortype.as_deref(),
                    ),
                    (
                        REFUSED,
                        "NotAuthorizedException",
                        INCORRECT_CREDENTIALS_MESSAGE,
                        Some("NotAuthorizedException:"),
                    ),
                    "case {case:?}: expected the credential refusal, got {answered:?}"
                );
                answers.push((case, answered));
            }
        }
    }

    // The masking itself: nothing in the answer differs between rows, so the refusal is
    // no oracle for which usernames the pool holds.
    let (first_case, first) = answers.first().expect("the table has rows").clone();
    for (case, answered) in &answers {
        assert_eq!(
            *answered, first,
            "case {case:?}: expected the answer of case {first_case:?} ({first:?}), got \
             {answered:?}"
        );
    }

    // Nothing was recorded by any refused submission.
    let events = history(&router, &fixture.user_pool_id, USERNAME).await;
    assert!(
        events.iter().all(|event| event.get("EventFeedback").is_none()),
        "a refused submission must record no feedback, got {events:?}"
    );
}

/// RULE: `UpdateAuthEventFeedback` is authorized by the `FeedbackToken` minted for one
/// event of one user, so a request that binds to no such token is refused with its
/// SESSION class — HTTP 400, `NotAuthorizedException` in the body and in the
/// `x-amzn-errortype` header, carrying the fixed invalid-session sentence — whichever
/// part of the triple failed, and every row is answered identically.
#[tokio::test]
async fn the_user_s_own_feedback_refuses_everything_it_cannot_bind_on_the_session() {
    let (router, engine) = router_and_engine();
    let fixture = given_pool(&router, "user-feedback-masking", Some(ENFORCED)).await;
    given_user(&router, &fixture.user_pool_id, USERNAME).await;
    given_user(&router, &fixture.user_pool_id, OTHER_USERNAME).await;
    // Two events for the user under test, so one row can present the token of its OTHER
    // event, and one event for a second user, so another row can present that user's.
    sign_in(&router, &fixture.client_id, USERNAME).await;
    sign_in(&router, &fixture.client_id, USERNAME).await;
    sign_in(&router, &fixture.client_id, OTHER_USERNAME).await;

    let mine = event_ids(&router, &fixture.user_pool_id, USERNAME).await;
    assert_eq!(
        mine.len(),
        2,
        "the two sign-ins must have logged two events, got {mine:?}"
    );
    let subject = mine[0].clone();
    let sibling = mine[1].clone();
    let theirs = event_ids(&router, &fixture.user_pool_id, OTHER_USERNAME)
        .await
        .first()
        .expect("the other user's sign-in logged one event")
        .to_string();

    let my_token = feedback_token(&engine, &fixture.user_pool_id, USERNAME, &subject);
    let sibling_token = feedback_token(&engine, &fixture.user_pool_id, USERNAME, &sibling);
    let their_token = feedback_token(&engine, &fixture.user_pool_id, OTHER_USERNAME, &theirs);

    let elsewhere = given_pool(&router, "user-feedback-other-pool", Some(ENFORCED)).await;
    given_user(&router, &elsewhere.user_pool_id, FOREIGN_POOL_USERNAME).await;

    // (case, username, event id, token) — one row per way a request binds to no token
    // of the pool under test.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for (case, username) in unresolved_usernames() {
        // Only the username is unresolved: the event and the token are a real pair.
        rows.push((
            format!("{case}, with a real event and its own token"),
            username,
            subject.clone(),
            my_token.clone(),
        ));
    }
    rows.push((
        "an event the user's history does not hold".to_string(),
        USERNAME.to_string(),
        ABSENT_EVENT_ID.to_string(),
        my_token.clone(),
    ));
    rows.push((
        "a token this emulator minted for no event".to_string(),
        USERNAME.to_string(),
        subject.clone(),
        FOREIGN_FEEDBACK_TOKEN.to_string(),
    ));
    rows.push((
        "the token minted for another event of the same user".to_string(),
        USERNAME.to_string(),
        subject.clone(),
        sibling_token,
    ));
    rows.push((
        "the token minted for another user's event".to_string(),
        USERNAME.to_string(),
        subject.clone(),
        their_token,
    ));
    rows.push((
        "another user's event named under this user".to_string(),
        USERNAME.to_string(),
        theirs.clone(),
        my_token.clone(),
    ));

    let mut answers = Vec::new();
    for (case, username, event, token) in &rows {
        for verdict in ["Valid", "Invalid"] {
            let case = format!("{case}, submitting {verdict}");
            let answered = dispatch(
                &router,
                "UpdateAuthEventFeedback",
                &json!({
                    "UserPoolId": &fixture.user_pool_id,
                    "Username": username,
                    "EventId": event,
                    "FeedbackToken": token,
                    "FeedbackValue": verdict,
                }),
                false,
            )
            .await;
            assert_eq!(
                (
                    answered.status,
                    error_code(&answered.payload),
                    error_message(&answered.payload),
                    answered.errortype.as_deref(),
                ),
                (
                    REFUSED,
                    "NotAuthorizedException",
                    FEEDBACK_TOKEN_INVALID_MESSAGE,
                    Some("NotAuthorizedException:"),
                ),
                "case {case:?}: expected the invalid-session refusal, got {answered:?}"
            );
            answers.push((case, answered));
        }
    }

    // The masking itself: a username the pool holds no user for is answered exactly as a
    // token that was not the event's, so an anonymous caller learns nothing about who is
    // a member of the pool.
    let (first_case, first) = answers.first().expect("the table has rows").clone();
    for (case, answered) in &answers {
        assert_eq!(
            *answered, first,
            "case {case:?}: expected the answer of case {first_case:?} ({first:?}), got \
             {answered:?}"
        );
    }

    // No refused submission recorded anything, and the token that WAS minted for the
    // event still authorizes feedback on it.
    for username in [USERNAME, OTHER_USERNAME] {
        let events = history(&router, &fixture.user_pool_id, username).await;
        assert!(
            events.iter().all(|event| event.get("EventFeedback").is_none()),
            "a refused submission must record no feedback, got {events:?} for {username}"
        );
    }
    ok_anonymous(
        &router,
        "UpdateAuthEventFeedback",
        &json!({
            "UserPoolId": &fixture.user_pool_id,
            "Username": USERNAME,
            "EventId": &subject,
            "FeedbackToken": my_token,
            "FeedbackValue": "Valid",
        }),
    )
    .await;
    let events = history(&router, &fixture.user_pool_id, USERNAME).await;
    let recorded = events
        .iter()
        .find(|event| event["EventId"] == json!(subject))
        .unwrap_or_else(|| panic!("the event the feedback was left on: {events:?}"));
    assert_eq!(
        (
            recorded["EventFeedback"]["FeedbackValue"].as_str(),
            recorded["EventFeedback"]["Provider"].as_str(),
        ),
        (Some("Valid"), Some("User")),
        "the token minted for the event must still record the user's verdict, got \
         {recorded}"
    );
}

/// RULE: `AdminUpdateAuthEventFeedback` resolves its `EventId` inside the history of
/// the user the same request names, so an id that resolves to no event of that history
/// is refused as an invalid parameter value naming the member it arrived in — HTTP 400,
/// `InvalidParameterException` in the body and in the `x-amzn-errortype` header,
/// carrying one fixed sentence — rather than as an absent resource.
///
/// The rows span the forms `EventIdType` admits at both published length bounds, and
/// the ways an id resolves to no event of this user: never logged, and logged for
/// another user of the same pool.
#[tokio::test]
async fn the_administrative_feedback_reports_an_unresolved_event_id_as_an_invalid_value() {
    let (router, _engine) = router_and_engine();
    let fixture = given_pool(&router, "admin-feedback-event-id", Some(ENFORCED)).await;
    given_user(&router, &fixture.user_pool_id, USERNAME).await;
    given_user(&router, &fixture.user_pool_id, OTHER_USERNAME).await;
    sign_in(&router, &fixture.client_id, USERNAME).await;
    sign_in(&router, &fixture.client_id, OTHER_USERNAME).await;
    let theirs = event_ids(&router, &fixture.user_pool_id, OTHER_USERNAME)
        .await
        .first()
        .expect("the other user's sign-in logged one event")
        .to_string();

    // The values are `EventIdType`'s own (pattern `^[\w+-]+$`, length 1..50).
    let rows: Vec<(String, String)> = vec![
        ("an id nothing logged".to_string(), ABSENT_EVENT_ID.to_string()),
        (
            "an id of the shortest length the model admits".to_string(),
            "e".repeat(EVENT_ID_MIN_LENGTH),
        ),
        (
            "an id of the greatest length the model admits".to_string(),
            "e".repeat(EVENT_ID_MAX_LENGTH),
        ),
        (
            "an id shaped like the ids this emulator mints".to_string(),
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "an id carrying the other characters the pattern admits".to_string(),
            "event+id_7-0".to_string(),
        ),
        (
            "an event logged for another user of the same pool".to_string(),
            theirs,
        ),
    ];

    for (case, event_id) in &rows {
        for verdict in ["Valid", "Invalid"] {
            let case = format!("{case}, submitting {verdict}");
            let answered = dispatch(
                &router,
                "AdminUpdateAuthEventFeedback",
                &json!({
                    "UserPoolId": &fixture.user_pool_id,
                    "Username": USERNAME,
                    "EventId": event_id,
                    "FeedbackValue": verdict,
                }),
                true,
            )
            .await;
            assert_eq!(
                (
                    answered.status,
                    error_code(&answered.payload),
                    error_message(&answered.payload),
                    answered.errortype.as_deref(),
                ),
                (
                    REFUSED,
                    "InvalidParameterException",
                    EVENT_ID_INVALID_MESSAGE,
                    Some("InvalidParameterException:"),
                ),
                "case {case:?}: expected the invalid-event-id refusal, got {answered:?}"
            );
        }
    }

    // Nothing was recorded on either user's events by the refused submissions, and the
    // event the OTHER user owns still carries none after being named under this one.
    for username in [USERNAME, OTHER_USERNAME] {
        let events = history(&router, &fixture.user_pool_id, username).await;
        assert!(
            events.iter().all(|event| event.get("EventFeedback").is_none()),
            "a refused submission must record no feedback, got {events:?} for {username}"
        );
    }
}

/// RULE: the gates that are properties of the POOL rather than of the user asked about
/// keep their own classes and still answer first — an absent pool as the absent
/// resource it is, a pool running no threat protection as the add-on state it is in —
/// and the READER of the history keeps its own user-existence refusal, because it is no
/// submission authorized by a credential or a token.
#[tokio::test]
async fn the_pool_gates_and_the_history_reader_keep_their_own_refusals() {
    let (router, _engine) = router_and_engine();
    let live = given_pool(&router, "feedback-pool-gates", Some(ENFORCED)).await;
    given_user(&router, &live.user_pool_id, USERNAME).await;
    sign_in(&router, &live.client_id, USERNAME).await;
    let off = given_pool(&router, "feedback-pool-gates-off", Some(OFF)).await;
    given_user(&router, &off.user_pool_id, USERNAME).await;
    let plain = given_pool(&router, "feedback-pool-gates-plain", None).await;

    // (case, operation, pool, username, class, sentence) — the sentence is empty where
    // it names the pool the row made up.
    let rows: Vec<(&str, &str, &str, &str, &str, &str)> = vec![
        (
            "the administrative submission on a pool nothing created",
            "AdminUpdateAuthEventFeedback",
            ABSENT_USER_POOL_ID,
            "nobody-at-all",
            "ResourceNotFoundException",
            "",
        ),
        (
            "the user's own submission on a pool nothing created",
            "UpdateAuthEventFeedback",
            ABSENT_USER_POOL_ID,
            "nobody-at-all",
            "ResourceNotFoundException",
            "",
        ),
        (
            "the administrative submission on a pool whose add-ons are off",
            "AdminUpdateAuthEventFeedback",
            off.user_pool_id.as_str(),
            "nobody-at-all",
            "UserPoolAddOnNotEnabledException",
            USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE,
        ),
        (
            "the user's own submission on a pool whose add-ons are off",
            "UpdateAuthEventFeedback",
            off.user_pool_id.as_str(),
            "nobody-at-all",
            "UserPoolAddOnNotEnabledException",
            USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE,
        ),
        (
            "the administrative submission on a pool configuring no add-ons at all",
            "AdminUpdateAuthEventFeedback",
            plain.user_pool_id.as_str(),
            "nobody-at-all",
            "UserPoolAddOnNotEnabledException",
            USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE,
        ),
        (
            "the history reader for a user the pool does not hold",
            "AdminListUserAuthEvents",
            live.user_pool_id.as_str(),
            "nobody-at-all",
            "UserNotFoundException",
            USER_NOT_FOUND_MESSAGE,
        ),
    ];

    for (case, operation, user_pool_id, username, class, sentence) in rows {
        let mut body = json!({
            "UserPoolId": user_pool_id,
            "Username": username,
        });
        if operation != "AdminListUserAuthEvents" {
            body["EventId"] = json!(ABSENT_EVENT_ID);
            body["FeedbackValue"] = json!("Valid");
        }
        if operation == "UpdateAuthEventFeedback" {
            body["FeedbackToken"] = json!(FOREIGN_FEEDBACK_TOKEN);
        }
        let answered = dispatch(
            &router,
            operation,
            &body,
            operation != "UpdateAuthEventFeedback",
        )
        .await;
        assert_eq!(
            (answered.status, error_code(&answered.payload)),
            (REFUSED, class),
            "case {case:?}: expected {class}, got {answered:?}"
        );
        if !sentence.is_empty() {
            assert_eq!(
                error_message(&answered.payload),
                sentence,
                "case {case:?}: expected {sentence:?}, got {answered:?}"
            );
        }
    }

    // The administrative submission for a user the pool DOES hold, naming an event
    // nothing logged, reports the invalid parameter value it is: the id is resolved
    // inside that user's own history rather than addressed as a resource of its own.
    let answered = dispatch(
        &router,
        "AdminUpdateAuthEventFeedback",
        &json!({
            "UserPoolId": &live.user_pool_id,
            "Username": USERNAME,
            "EventId": ABSENT_EVENT_ID,
            "FeedbackValue": "Valid",
        }),
        true,
    )
    .await;
    assert_eq!(
        (
            answered.status,
            error_code(&answered.payload),
            error_message(&answered.payload),
        ),
        (
            REFUSED,
            "InvalidParameterException",
            EVENT_ID_INVALID_MESSAGE,
        ),
        "case \"an event nothing logged\": expected the invalid-event-id refusal, got \
         {answered:?}"
    );

    // And the administrative submission for the user and the event the pool holds is
    // served, so the refusals above are the gates they claim to be rather than a
    // blanket refusal.
    let logged = event_ids(&router, &live.user_pool_id, USERNAME).await;
    let event_id = logged.first().expect("the sign-in logged one event");
    ok(
        &router,
        "AdminUpdateAuthEventFeedback",
        &json!({
            "UserPoolId": &live.user_pool_id,
            "Username": USERNAME,
            "EventId": event_id,
            "FeedbackValue": "Invalid",
        }),
    )
    .await;
    let events = history(&router, &live.user_pool_id, USERNAME).await;
    let recorded = events
        .iter()
        .find(|event| event["EventId"] == json!(event_id))
        .unwrap_or_else(|| panic!("the event the feedback was left on: {events:?}"));
    assert_eq!(
        (
            recorded["EventFeedback"]["FeedbackValue"].as_str(),
            recorded["EventFeedback"]["Provider"].as_str(),
        ),
        (Some("Invalid"), Some("Admin")),
        "the administrative submission must record its verdict, got {recorded}"
    );
}
