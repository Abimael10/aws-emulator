// SPDX-License-Identifier: CC0-1.0

//! The sentence `ConfirmForgotPassword` refuses a policy-missing `Password`
//! with, over the wire.
//!
//! RULE under test: the prefix of an `InvalidPasswordException`
//! (`smithy_query(query="operation",operation="ConfirmForgotPassword")`: a
//! client error of this operation) is a property of the API that was CALLED,
//! not of who supplied the password — so the confirmation of a
//! forgotten-password reset renders its own fixed sentence, "Password does not
//! conform to policy: ", ahead of the clause for the requirement the password
//! missed, while the self-service sign-up of the SAME pool renders the sentence
//! ITS API answers, "Password did not conform with policy: ", ahead of the very
//! same clause. The clause itself is fixed per requirement of
//! `PasswordPolicyType` (`MinimumLength`, `RequireUppercase`,
//! `RequireLowercase`, `RequireNumbers`, `RequireSymbols` —
//! `smithy_query(query="shape",shape="PasswordPolicyType")`), is chosen from
//! the pool's configured policy and the submitted password, and never reports
//! the value the pool configured for that requirement.
//!
//! The rows differ along the policy the pool configures (none at all, the
//! documented `MinimumLength` default, both published bounds of its 6..99 range,
//! and each `Require*` requirement on its own), along the length and character
//! classes of the submitted password, and along whether that password conforms
//! at all. Nothing is pinned to one recorded password: each row's own
//! replacement password is derived from the policy under test.
//!
//! Every pool, app client, user and reset code here is real: the code comes from
//! a `ForgotPassword` of the row's own user, read out of the emulator's state
//! the way the service delivers it out of band, and every refused row proves the
//! code was not spent by completing the reset with a conforming password and
//! signing the user in with it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state;

/// A request's SigV4 credential scope is what routes an administrative call to
/// this service; the signature itself is never checked here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The fixed sentence `ConfirmForgotPassword` words a password-policy refusal
/// with, ahead of the requirement's clause.
const CONFIRM_RESET_PREFIX: &str = "Password does not conform to policy: ";

/// The fixed sentence the self-service `SignUp` words the SAME refusal with,
/// which is what makes the prefix a property of the API rather than of the
/// policy or of the password.
const SIGN_UP_PREFIX: &str = "Password did not conform with policy: ";

/// The documented default `MinimumLength` of a pool that configures no password
/// policy, used to derive each row's replacement password where the row
/// configures none itself.
const DEFAULT_MINIMUM_LENGTH: u64 = 8;

/// The stem of the password a user holds before its reset. Each row pads it to
/// the length its own policy asks for, so the credential the user starts with is
/// one that policy admits.
const ORIGINAL_STEM: &str = "Or1ginalPassw0rd!";

/// The stem of the password a refused row's reset finally settles on, distinct
/// from [`ORIGINAL_STEM`] so a sign-in tells the two apart.
const REPLACEMENT_STEM: &str = "Res3tPassw0rd!";

/// One row of the table: the policy the pool configures, the password the reset
/// confirmation submits, and the clause the refusal must carry (`None` where the
/// password conforms and the reset completes).
struct Row {
    case: &'static str,
    /// The `PasswordPolicy` member of `CreateUserPool`, or `Value::Null` for a
    /// pool that configures no policy at all.
    policy: Value,
    password: String,
    requirement: Option<&'static str>,
}

/// The table: every requirement a `PasswordPolicyType` can set, plus the two
/// published bounds of `MinimumLength` and the pool that configures nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no configured policy, a password below the documented default minimum",
            policy: Value::Null,
            password: "Ab1!".to_string(),
            requirement: Some("Password not long enough"),
        },
        Row {
            case: "no configured policy, a password of exactly the default minimum length \
                   carrying every class the default policy requires",
            policy: Value::Null,
            password: "Passw0r!".to_string(),
            requirement: None,
        },
        Row {
            case: "the 6-character minimum of the MinimumLength range, a 5-character password",
            policy: json!({ "MinimumLength": 6 }),
            password: "Ab1!c".to_string(),
            requirement: Some("Password not long enough"),
        },
        Row {
            case: "the 6-character minimum of the MinimumLength range, a 6-character password",
            policy: json!({ "MinimumLength": 6 }),
            password: "Ab1!cd".to_string(),
            requirement: None,
        },
        Row {
            case: "the 99-character maximum of the MinimumLength range, a 98-character password",
            policy: json!({ "MinimumLength": 99 }),
            password: "L0ngPassword!".to_string() + &"x".repeat(85),
            requirement: Some("Password not long enough"),
        },
        Row {
            case: "a password with no uppercase character where the policy requires one",
            policy: json!({ "MinimumLength": 6, "RequireUppercase": true }),
            password: "res3tpass!2024".to_string(),
            requirement: Some("Password must have uppercase characters"),
        },
        Row {
            case: "a password with no lowercase character where the policy requires one",
            policy: json!({ "MinimumLength": 6, "RequireLowercase": true }),
            password: "RES3TPASS!2024".to_string(),
            requirement: Some("Password must have lowercase characters"),
        },
        Row {
            case: "a password with no numeric character where the policy requires one",
            policy: json!({ "MinimumLength": 6, "RequireNumbers": true }),
            password: "ResetPassword!".to_string(),
            requirement: Some("Password must have numeric characters"),
        },
        Row {
            case: "a password with no symbol character where the policy requires one",
            policy: json!({ "MinimumLength": 6, "RequireSymbols": true }),
            password: "Res3tPassword".to_string(),
            requirement: Some("Password must have symbol characters"),
        },
        Row {
            case: "a password satisfying every requirement the policy sets",
            policy: json!({
                "MinimumLength": 12,
                "RequireUppercase": true,
                "RequireLowercase": true,
                "RequireNumbers": true,
                "RequireSymbols": true,
            }),
            password: "Res3tPassw0rd!".to_string(),
            requirement: None,
        },
    ]
}

/// A password that satisfies the policy `policy` configures: `stem`, which
/// carries one character of every class a policy can require, padded to the
/// length the policy asks for. Derived from the policy under test, so no row
/// carries a conforming literal of its own.
fn conforming_password(policy: &Value, stem: &str) -> String {
    let minimum = usize::try_from(
        policy
            .get("MinimumLength")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MINIMUM_LENGTH),
    )
    .expect("a MinimumLength inside the modelled 6..99 range fits a usize");
    let mut password = stem.to_string();
    while password.chars().count() < minimum {
        password.push('x');
    }
    password
}

/// The shared engine every router in one test mounts.
fn engine() -> Arc<engine::InMemoryEngine> {
    Arc::new(engine::InMemoryEngine::new())
}

/// One emulator with just this service mounted over `engine`.
fn router(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(engine)));
    router.set_request_authentication(false);
    router
}

/// Dispatch `operation` with `body` and return its status and JSON response
/// body.
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
    let bytes = response.into_body();
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(&bytes)
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

/// Create a pool carrying `policy` (when the row configures one) and return its
/// minted `UserPoolId`.
async fn create_pool(router: &MultiServiceRouter, pool_name: &str, policy: &Value) -> String {
    let mut body = json!({ "PoolName": pool_name });
    if !policy.is_null() {
        body["Policies"] = json!({ "PasswordPolicy": policy.clone() });
    }
    ok(router, "CreateUserPool", &body).await["UserPool"]["Id"]
        .as_str()
        .expect("CreateUserPool mints a pool id")
        .to_string()
}

/// Create a secretless app client of `user_pool_id` that admits the
/// administrative password sign-in, and return its minted `ClientId`.
async fn create_client(router: &MultiServiceRouter, user_pool_id: &str, name: &str) -> String {
    ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": name,
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("CreateUserPoolClient mints a client id")
        .to_string()
}

/// Create `username` in `user_pool_id` with a verified email and `password` as
/// its permanent one, which is what makes it a confirmed user a reset can be
/// asked for.
async fn create_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    password: &str,
) {
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
            "Password": password,
            "Permanent": true,
        }),
    )
    .await;
}

/// The code outstanding for a password reset of `username`, read out of the
/// emulator's own state: the service delivers it out of band, so no response
/// carries it.
fn outstanding_reset_code(
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    username: &str,
) -> Option<String> {
    let service_state = state::service_state(&**engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .and_then(|user| user.outstanding_code(&state::CodePurpose::PasswordReset))
        .map(|code| code.code.clone())
}

/// Ask for a reset of `username` through `client_id` and return the code it
/// minted.
async fn reset_code(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> String {
    ok(
        router,
        "ForgotPassword",
        &json!({ "ClientId": client_id, "Username": username }),
    )
    .await;
    outstanding_reset_code(engine, user_pool_id, username)
        .expect("a served ForgotPassword leaves a reset code outstanding")
}

/// Sign `username` in with `password` through the administrative password flow,
/// which is the only reader of a stored password this API has.
async fn signs_in(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
    password: &str,
) -> (u16, Value) {
    call(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": password },
        }),
    )
    .await
}

#[tokio::test]
async fn a_reset_confirmation_words_its_policy_refusal_as_its_own_api_does() {
    let engine = engine();
    let router = router(&engine);

    for (index, row) in rows().iter().enumerate() {
        let user_pool_id = create_pool(&router, &format!("reset-wording-{index}"), &row.policy).await;
        let client_id = create_client(&router, &user_pool_id, &format!("reset-app-{index}")).await;
        let username = format!("resetter{index}");
        let original = conforming_password(&row.policy, ORIGINAL_STEM);
        create_user(&router, &user_pool_id, &username, &original).await;
        let code = reset_code(&router, &engine, &user_pool_id, &client_id, &username).await;

        let (status, payload) = call(
            &router,
            "ConfirmForgotPassword",
            &json!({
                "ClientId": client_id,
                "Username": username,
                "ConfirmationCode": code,
                "Password": row.password,
            }),
        )
        .await;

        let Some(requirement) = row.requirement else {
            assert_eq!(
                status, 200,
                "case {:?}: a conforming password completes the reset, got {status} with \
                 {payload}",
                row.case
            );
            let (signed_in, result) =
                signs_in(&router, &user_pool_id, &client_id, &username, &row.password).await;
            assert_eq!(
                signed_in, 200,
                "case {:?}: the confirmed password must sign the user in, got {signed_in} \
                 with {result}",
                row.case
            );
            continue;
        };

        assert_eq!(
            (status, error_code(&payload)),
            (400, "InvalidPasswordException"),
            "case {:?}: expected a 400 InvalidPasswordException, got {status} with {payload}",
            row.case
        );
        let message = error_message(&payload);
        let expected = format!("{CONFIRM_RESET_PREFIX}{requirement}");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
        assert!(
            !message.starts_with(SIGN_UP_PREFIX),
            "case {:?}: this API does not answer the sign-up sentence, got {message:?}",
            row.case
        );
        // The clause reports WHICH requirement was missed, never the value the
        // pool configured for it: the only policy value that could leak into
        // this sentence is the configured minimum length.
        if let Some(minimum) = row.policy.get("MinimumLength").and_then(Value::as_i64) {
            assert!(
                !message.contains(&minimum.to_string()),
                "case {:?}: the refusal must not report the configured minimum {minimum}, \
                 got {message:?}",
                row.case
            );
        }

        // The SAME pool, policy and password through the self-service sign-up:
        // the clause is the same, the prefix is the one THAT API answers.
        let (signed_up, sign_up_payload) = call(
            &router,
            "SignUp",
            &json!({
                "ClientId": client_id,
                "Username": format!("signedup{index}"),
                "Password": row.password,
            }),
        )
        .await;
        assert_eq!(
            (signed_up, error_code(&sign_up_payload)),
            (400, "InvalidPasswordException"),
            "case {:?}: SignUp refuses the same password, got {signed_up} with \
             {sign_up_payload}",
            row.case
        );
        let sign_up_message = error_message(&sign_up_payload);
        let sign_up_expected = format!("{SIGN_UP_PREFIX}{requirement}");
        assert_eq!(
            sign_up_message, sign_up_expected,
            "case {:?}: expected {sign_up_expected:?}, got {sign_up_message:?}",
            row.case
        );

        // The refusal spent nothing: the code the reset minted still completes
        // it, and the password it settles on is the one the user signs in with.
        let replacement = conforming_password(&row.policy, REPLACEMENT_STEM);
        let completed = ok(
            &router,
            "ConfirmForgotPassword",
            &json!({
                "ClientId": client_id,
                "Username": username,
                "ConfirmationCode": code,
                "Password": replacement,
            }),
        )
        .await;
        assert_eq!(
            completed,
            json!({}),
            "case {:?}: the completed reset answers an empty body, got {completed}",
            row.case
        );
        let (signed_in, result) =
            signs_in(&router, &user_pool_id, &client_id, &username, &replacement).await;
        assert_eq!(
            signed_in, 200,
            "case {:?}: the replacement password must sign the user in, got {signed_in} with \
             {result}",
            row.case
        );
        let (refused, refusal) =
            signs_in(&router, &user_pool_id, &client_id, &username, &original).await;
        assert_ne!(
            refused, 200,
            "case {:?}: the password the reset replaced no longer signs the user in, got \
             {refused} with {refusal}",
            row.case
        );
        assert!(
            error_code(&refusal).ends_with("NotAuthorizedException"),
            "case {:?}: expected NotAuthorizedException for the replaced password, got \
             {refusal}",
            row.case
        );
    }
}
