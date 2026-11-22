// SPDX-License-Identifier: CC0-1.0

//! What a post-confirmation trigger function's ANSWER decides, with Cognito and Lambda
//! mounted on one engine so the function the pool's `LambdaConfig` names really runs.
//!
//! RULE: a trigger that runs after the outcome it follows — "this trigger can take custom
//! actions after a user confirms their user account"
//! (`LambdaConfigType$PostConfirmation`) — is answered with the EVENT it was handed, which
//! names the account the stage is about, so a function whose answer does not name that same
//! account has not answered this invocation: Amazon Cognito "encounters an invalid Lambda
//! response" (this service's model for `InvalidLambdaResponseException`) and the confirming
//! operation reports that modelled error with the fixed sentence it states for an answer
//! naming another user. The confirmation itself was already settled when the function ran,
//! so the user is left `CONFIRMED` whatever the function answered, and a pool that configures
//! no such function is answered by Cognito alone.
//!
//! The rows span what the rule generalises over — the FORM of the answer: the event handed
//! back unchanged, the event with another account's name written into it, a document that is
//! not that event, a payload that is no document at all, and no payload at all — and both
//! operations that complete a sign-up confirmation drive the same rule (`AdminConfirmSignUp`
//! and the self-service `ConfirmSignUp`).
//!
//! Nothing here is asserted against a value baked into the service: the pool id, the client
//! id, the function ARN and the username all come back from the state the setup calls
//! created, and the confirmation code is read out of Cognito's own state, the real service
//! delivering it out of band.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The local name of Cognito's Smithy shape, which is the `X-Amz-Target` prefix.
const COGNITO_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// Where Lambda's `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying Lambda's `Role` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The modelled error a confirming operation answers an unreadable trigger answer with.
const INVALID_LAMBDA_RESPONSE: &str = "InvalidLambdaResponseException";

/// The sentence that error carries for an answer that does not name the account the event
/// was delivered for. Fixed text, quoting neither name.
const USERNAME_MISMATCH: &str =
    "Invalid user name. User name in Lambda mismatches the actual username.";

/// How long this suite waits for a created function to become `Active`, and how often it
/// looks again. Generous: the wait is for the condition, and a loaded host takes far longer
/// than an idle one.
const ACTIVE_BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Cognito and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them, so
/// Cognito's in-process callback reaches this very Lambda. The engine is handed back as
/// well: the confirmation code a self-service confirmation answers is read out of Cognito's
/// own state, no response carrying it.
fn router() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (engine, router)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    answer(router.dispatch(request).await)
}

/// One Cognito call: `awsJson1_1`, addressed by `X-Amz-Target`.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("{COGNITO_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("cognito-idp"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    answer(router.dispatch(request).await)
}

/// The status and parsed body of a dispatched response.
fn answer(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Cognito call that must answer 200, with its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = cognito(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The error code of an error body (`__type`), without its namespace, and its message.
fn refusal(body: &Value) -> (String, String) {
    (
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        body.get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// A .zip deployment package holding `source` as `index.py`, the module a `Handler` of
/// `index.handler` names.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the `python3.12` function `name` running `source`, grant Cognito the invocation
/// its trigger needs, and answer the ARN Lambda minted for it.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateFunction({name:?}) must answer 201, got {status} with {created}"
    );
    // Amazon Cognito invokes a trigger as its OWN service principal, so the function's
    // resource-based policy has to admit it — written through the one spelling of that grant
    // this crate has (`crate::trigger_invoke_permission`, the suite that owns that rule).
    crate::trigger_invoke_permission::grant_cognito_trigger_invoke(router, REGION, name).await;
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFunction must mint an ARN, got {created}"))
        .to_string()
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs: the
/// transition is the CLOCK's, so this looks again until it holds rather than sleeping a
/// fixed span.
async fn await_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + ACTIVE_BUDGET;
    loop {
        let (_, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the function {name:?} never became Active within {ACTIVE_BUDGET:?}, got {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create a pool whose `LambdaConfig` is `lambda_config` and an app client of it, and
/// answer the pool id and the client id.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    lambda_config: Value,
) -> (String, String) {
    let mut body = json!({ "PoolName": name, "AutoVerifiedAttributes": ["email"] });
    if !lambda_config.is_null() {
        body["LambdaConfig"] = lambda_config;
    }
    let created = ok(router, "CreateUserPool", &body).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {created}"))
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientName": format!("{name}-app") }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    (user_pool_id, client_id)
}

/// Sign `username` up through `client_id`, leaving it unconfirmed.
async fn sign_up(router: &MultiServiceRouter, client_id: &str, username: &str) {
    ok(
        router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "UserAttributes": [{ "Name": "email", "Value": format!("{username}@example.test") }],
        }),
    )
    .await;
}

/// The `UserStatus` `AdminGetUser` reports for `username`.
async fn stored_status(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
) -> String {
    let read_back = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    read_back["UserStatus"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The sign-up confirmation code outstanding for `username`, read out of Cognito's own
/// state: the real service delivers it out of band, so no response carries it.
fn outstanding_code(
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    username: &str,
) -> String {
    let service_state = cognito_idp::state::service_state(&**engine).expect("Cognito's own state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .and_then(|user| {
            user.outstanding_code(&cognito_idp::state::CodePurpose::SignUpConfirmation)
        })
        .map(|code| code.code.clone())
        .unwrap_or_else(|| panic!("a sign-up code must be outstanding for {username}"))
}

/// A handler that answers the event it was handed, which is what a trigger is answered
/// with.
const ANSWERS_THE_EVENT: &str = r#"
def handler(event, context):
    return event
"#;

/// A handler that writes another account's name into the event before answering it.
const ANSWERS_ANOTHER_USERNAME: &str = r#"
def handler(event, context):
    event["userName"] = "somebody-else"
    return event
"#;

/// A handler that answers a document of its own, which is not the event.
const ANSWERS_ANOTHER_DOCUMENT: &str = r#"
def handler(event, context):
    return {"statusCode": 200, "body": "handled"}
"#;

/// A handler that answers something that is no document at all.
const ANSWERS_A_STRING: &str = r#"
def handler(event, context):
    return "handled"
"#;

/// A handler that answers nothing: it runs, and returns no payload.
const ANSWERS_NOTHING: &str = r#"
def handler(event, context):
    return None
"#;

#[tokio::test]
async fn a_confirmation_is_answered_by_what_its_post_confirmation_function_answered() {
    let (_engine, router) = router();

    // (case, the handler's source, expected status, expected error code, expected message)
    //
    // Every row drives the same sign-up and the same AdminConfirmSignUp against a pool
    // whose only difference is the function its PostConfirmation names, so what the
    // function ANSWERED is the only thing that can decide the answer.
    let rows: [(&str, &str, u16, &str, &str); 5] = [
        (
            "a function that answers the event it was handed",
            ANSWERS_THE_EVENT,
            200,
            "",
            "",
        ),
        (
            "a function that answers the event with another account's name",
            ANSWERS_ANOTHER_USERNAME,
            400,
            INVALID_LAMBDA_RESPONSE,
            USERNAME_MISMATCH,
        ),
        (
            "a function that answers a document that is not the event",
            ANSWERS_ANOTHER_DOCUMENT,
            400,
            INVALID_LAMBDA_RESPONSE,
            USERNAME_MISMATCH,
        ),
        (
            "a function that answers a value that is no document at all",
            ANSWERS_A_STRING,
            400,
            INVALID_LAMBDA_RESPONSE,
            USERNAME_MISMATCH,
        ),
        (
            "a function that answers nothing at all",
            ANSWERS_NOTHING,
            400,
            INVALID_LAMBDA_RESPONSE,
            USERNAME_MISMATCH,
        ),
    ];

    // Every function is created first and then waited for, so the rows share one wait
    // rather than each taking its own.
    let mut functions = Vec::new();
    for (index, (_, source, _, _, _)) in rows.iter().enumerate() {
        let name = format!("post-confirm-{index}");
        let arn = create_function(&router, &name, source).await;
        functions.push((name, arn));
    }
    for (name, _) in &functions {
        await_active(&router, name).await;
    }

    for (index, (case, _, expected_status, expected_code, expected_message)) in
        rows.into_iter().enumerate()
    {
        let (_, function_arn) = &functions[index];
        let (user_pool_id, client_id) = pool_with_client(
            &router,
            &format!("answered{index}"),
            json!({ "PostConfirmation": function_arn }),
        )
        .await;
        let username = format!("user{index}");
        sign_up(&router, &client_id, &username).await;

        let (status, payload) = cognito(
            &router,
            "AdminConfirmSignUp",
            &json!({ "UserPoolId": user_pool_id, "Username": username }),
        )
        .await;
        let (code, message) = refusal(&payload);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (expected_status, expected_code, expected_message),
            "case {case:?}: expected HTTP {expected_status} {expected_code:?} \
             {expected_message:?}, got {status} with {payload}"
        );

        // The confirmation was settled before the function ran, so the user is CONFIRMED
        // whatever the function answered: the trigger's answer decides what the call
        // reports, not what it wrote.
        let stored = stored_status(&router, &user_pool_id, &username).await;
        assert_eq!(
            stored, "CONFIRMED",
            "case {case:?}: expected the confirmed user to read back CONFIRMED, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn the_self_service_confirmation_reads_the_same_answer() {
    let (engine, router) = router();
    let name = "post-confirm-self-service";
    let function_arn = create_function(&router, name, ANSWERS_ANOTHER_DOCUMENT).await;
    await_active(&router, name).await;

    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "self-service-answered",
        json!({ "PostConfirmation": function_arn }),
    )
    .await;

    // `ConfirmSignUp`, the self-service confirmation, fires the same trigger and reads the
    // same answer: the stage is the pool's, not the operation's.
    let case = "a self-service confirmation whose function answers another document";
    let username = "selfservice";
    sign_up(&router, &client_id, username).await;
    let code = outstanding_code(&engine, &user_pool_id, username);
    let (status, payload) = cognito(
        &router,
        "ConfirmSignUp",
        &json!({
            "ClientId": client_id,
            "Username": username,
            "ConfirmationCode": code,
        }),
    )
    .await;
    assert_eq!(
        (status, refusal(&payload)),
        (
            400,
            (
                INVALID_LAMBDA_RESPONSE.to_string(),
                USERNAME_MISMATCH.to_string()
            )
        ),
        "case {case:?}: expected the trigger's answer to refuse the call, got {status} with \
         {payload}"
    );
    let stored = stored_status(&router, &user_pool_id, username).await;
    assert_eq!(
        stored, "CONFIRMED",
        "case {case:?}: expected the confirmed user to read back CONFIRMED, got {stored:?}"
    );

    // And a pool that configures the trigger NO LONGER has nothing to answer the call: the
    // identical confirmation of a fresh user is served by Cognito alone. `UpdateUserPool`
    // states the pool's whole configuration, so what the pool keeps is restated with it.
    let case = "the same pool with its LambdaConfig cleared";
    ok(
        &router,
        "UpdateUserPool",
        &json!({
            "UserPoolId": user_pool_id,
            "AutoVerifiedAttributes": ["email"],
            "LambdaConfig": {},
        }),
    )
    .await;
    let username = "untriggered";
    sign_up(&router, &client_id, username).await;
    let code = outstanding_code(&engine, &user_pool_id, username);
    let (status, payload) = cognito(
        &router,
        "ConfirmSignUp",
        &json!({
            "ClientId": client_id,
            "Username": username,
            "ConfirmationCode": code,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a pool that configures no function invokes nothing, got {status} with \
         {payload}"
    );
    let stored = stored_status(&router, &user_pool_id, username).await;
    assert_eq!(
        stored, "CONFIRMED",
        "case {case:?}: expected the confirmed user to read back CONFIRMED, got {stored:?}"
    );
}
