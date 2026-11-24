// SPDX-License-Identifier: CC0-1.0

//! The custom MESSAGE trigger of a user pool: the function a pool's `LambdaConfig.CustomMessage`
//! names, asked for the wording of every message that pool delivers.
//!
//! THE RULE UNDER TEST, one level above any single operation:
//!
//! > "A custom message Lambda trigger. This trigger is an opportunity to customize all SMS and
//! > email messages from your user pool. When a custom message trigger is active, your user pool
//! > routes all messages to a Lambda function that returns a runtime-customized message subject
//! > and body for your user pool to deliver to a user" (`LambdaConfigType$CustomMessage`, this
//! > service's model). So EVERY operation of such a pool that issues a message invokes that
//! > function first, with the event of the stage that issued it — `triggerSource` naming the
//! > trigger and that stage, `request.codeParameter` carrying the placeholder the code is
//! > delivered in (the code itself is never handed over), and `request.usernameParameter`
//! > carrying the one for the recipient's name on the stage whose message names it — and the
//! > issuing operation is GATED on that invocation: it continues only on a function that
//! > RETURNED wording the code can be delivered in, and otherwise answers the modelled Lambda
//! > error for what went wrong instead of delivering a message no trigger worded.
//!
//! The two suites below span what the rule generalises over. The first spans the STAGES: five
//! operations that each issue a message, driven on one pool and one function, with the event the
//! function was handed read back out of the refusal it chose to raise — the only channel a
//! trigger's own view of the event reaches a client through. The second spans what the function
//! ANSWERS: wording that keeps the placeholder, wording that drops it, wording for the other
//! medium, output that is not the event at all, a function that fails, a function whose policy
//! admits this service nothing, and a pool that configures no such trigger.
//!
//! Both run a REAL deployment package: Cognito and Lambda are mounted on one engine, as the
//! assembled emulator mounts them, and the pool's trigger names a `python3.12` function whose
//! handler reads the event it was handed and answers from the branch the call's own
//! `ClientMetadata` selected. Nothing here asserts a value baked into the service: the pool id,
//! the client id, the function ARN and every username come back from the state the setup calls
//! created.

use std::io::Write as _;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::trigger_source_branching::{cognito, ok, router, GOOD_PASSWORD};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where Lambda's `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying Lambda's `Role` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the trigger function's package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long a wait for a function to report `Active` is given before it is a failure.
/// Generous: the span is the emulator's to choose and nothing here tests it.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `ClientMetadata` key one call selects the handler's branch with, and the branch that
/// reports the event back by failing.
const MODE_KEY: &str = "mode";
const REPORT_MODE: &str = "report";

/// The placeholder a custom message's wording carries the code in, which is what the event hands
/// the function (`request.codeParameter`).
const CODE_PARAMETER: &str = "{####}";

/// The placeholder for the recipient's name, which only the invitation an administrative create
/// sends carries (`request.usernameParameter`).
const USERNAME_PARAMETER: &str = "{username}";

/// What the handler reports in place of a member the event did not carry at all.
const ABSENT: &str = "<none>";

/// The handler the trigger function runs: ONE function serving every stage and every answer
/// below, which branches on the `mode` the invoking call put in its `ClientMetadata` — a member
/// every operation here models and the event carries through as `request.clientMetadata`.
///
/// The `report` branch RAISES, quoting the event's own `triggerSource`, `codeParameter`,
/// `usernameParameter` and `userName` back: a trigger's error reaches the client inside the
/// issuing operation's refusal, so that refusal is where this suite reads the event the function
/// was actually handed.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    request = event.get("request") or {}
    mode = (request.get("clientMetadata") or {}).get("mode", "words")
    code = request.get("codeParameter", "<none>")
    named = request.get("usernameParameter", "<none>")
    if mode == "report":
        raise Exception(
            "source=" + event["triggerSource"]
            + " code=" + code
            + " name=" + named
            + " user=" + event["userName"]
        )
    if mode == "elsewhere":
        return {"wording": "this is not the event"}
    response = event.get("response") or {}
    if mode == "drops":
        response["emailSubject"] = "Welcome"
        response["emailMessage"] = "Welcome aboard, and nothing more"
    elif mode == "texts":
        response["smsMessage"] = "A text with no placeholder in it"
    else:
        response["emailSubject"] = "Your verification code"
        response["emailMessage"] = "Your code is " + code
    event["response"] = response
    return event
"#;

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One REST call to Lambda, answering its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A .zip deployment package holding [`HANDLER_SOURCE`] as the module the handler names.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the trigger function `name`, wait until Lambda reports it `Active`, grant Cognito the
/// invocation when `granted`, and answer the ARN Lambda minted for it.
async fn trigger_function(router: &MultiServiceRouter, name: &str, granted: bool) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 10,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateFunction {name:?} must answer 201, got {status} with {created}"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFunction must mint an ARN, got {created}"))
        .to_string();

    // An invocation only reaches a function Lambda reports as active, so the condition is
    // waited FOR rather than waited out.
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the trigger function {name:?} never became Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    if granted {
        // Amazon Cognito invokes a trigger as its OWN service principal, so the function's
        // resource-based policy has to admit it — written through the one spelling of that grant
        // this crate has (`crate::trigger_invoke_permission`, the suite that owns the rule).
        crate::trigger_invoke_permission::grant_cognito_trigger_invoke(router, REGION, name).await;
    }
    arn
}

/// Create a pool whose `LambdaConfig` is `lambda_config`, auto-verifying the email address —
/// which is what makes a sign-up send a confirmation code and a reset message have a destination
/// to go to — with an app client of it, and answer their minted ids.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    lambda_config: Value,
) -> (String, String) {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": name,
            "AutoVerifiedAttributes": ["email"],
            "LambdaConfig": lambda_config,
        }),
    )
    .await;
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

/// The `ClientMetadata` one call carries, selecting the handler's branch.
fn metadata(mode: &str) -> Value {
    json!({ MODE_KEY: mode })
}

/// The wire error code of an error body (`__type`), without its namespace, or the empty string
/// for a body that carries none.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` member of a body.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The address a fixture user holds, which is where every message below is addressed.
fn address(username: &str) -> String {
    format!("{username}@example.test")
}

/// Sign `username` up through `client_id` in the mode `mode` selects, answering the call's
/// status and body.
async fn sign_up(
    router: &MultiServiceRouter,
    client_id: &str,
    username: &str,
    mode: &str,
) -> (u16, Value) {
    cognito(
        router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "UserAttributes": [{ "Name": "email", "Value": address(username) }],
            "ClientMetadata": metadata(mode),
        }),
    )
    .await
}

/// Store `username` as a CONFIRMED user whose address is already vouched for, sending no
/// message at all: "set to SUPPRESS to suppress sending the message"
/// (`AdminCreateUser.MessageAction`), so this setup reaches no trigger and every invocation a
/// row observes is the one its own stage made.
async fn given_confirmed_user(router: &MultiServiceRouter, user_pool_id: &str, username: &str) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": [
                { "Name": "email", "Value": address(username) },
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

/// Which stage one row of the first suite drives.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// A sign-up's confirmation code, sent by `SignUp`.
    SignUp,
    /// That same code sent again, by `ResendConfirmationCode`.
    ResendCode,
    /// A password-reset code, by `ForgotPassword`.
    ForgotPassword,
    /// The invitation an administrative create sends, by `AdminCreateUser`.
    AdminCreateUser,
    /// The code for an attribute an administrative update asked to verify, by
    /// `AdminUpdateUserAttributes`.
    UpdateUserAttribute,
}

/// Put in place whatever this stage needs before it can issue a message, and drive it in the
/// `report` mode. Answers the operation's status and body, and the username the message was
/// issued for.
async fn drive(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    stage: Stage,
    username: &str,
) -> (u16, Value) {
    match stage {
        Stage::SignUp => sign_up(router, client_id, username, REPORT_MODE).await,
        Stage::ResendCode => {
            // The code is resent to a user that still owes its confirmation, which is what a
            // sign-up leaves behind; that sign-up words its own message rather than reporting.
            let (status, answered) = sign_up(router, client_id, username, "words").await;
            assert_eq!(
                status, 200,
                "setup SignUp for the resend row must succeed, got {status} with {answered}"
            );
            cognito(
                router,
                "ResendConfirmationCode",
                &json!({
                    "ClientId": client_id,
                    "Username": username,
                    "ClientMetadata": metadata(REPORT_MODE),
                }),
            )
            .await
        }
        Stage::ForgotPassword => {
            given_confirmed_user(router, user_pool_id, username).await;
            cognito(
                router,
                "ForgotPassword",
                &json!({
                    "ClientId": client_id,
                    "Username": username,
                    "ClientMetadata": metadata(REPORT_MODE),
                }),
            )
            .await
        }
        Stage::AdminCreateUser => {
            cognito(
                router,
                "AdminCreateUser",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    // "Specify EMAIL if email will be used to send the welcome message"
                    // (`AdminCreateUser.DesiredDeliveryMediums`).
                    "DesiredDeliveryMediums": ["EMAIL"],
                    "UserAttributes": [{ "Name": "email", "Value": address(username) }],
                    "ClientMetadata": metadata(REPORT_MODE),
                }),
            )
            .await
        }
        Stage::UpdateUserAttribute => {
            given_confirmed_user(router, user_pool_id, username).await;
            cognito(
                router,
                "AdminUpdateUserAttributes",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    "UserAttributes": [
                        { "Name": "email", "Value": format!("moved-{}", address(username)) },
                    ],
                    "ClientMetadata": metadata(REPORT_MODE),
                }),
            )
            .await
        }
    }
}

#[tokio::test]
async fn every_stage_that_issues_a_message_hands_the_custom_message_trigger_its_own_event() {
    let router = router();
    let function = trigger_function(&router, "custom-message-stages", true).await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "custom-message-stages",
        json!({ "CustomMessage": function }),
    )
    .await;

    // (case, the stage, the username its message is issued for, the `triggerSource` its event
    // must carry, the `usernameParameter` it must carry)
    //
    // The sources are the ones Amazon Cognito documents for this trigger, one per stage that
    // issues a message; `usernameParameter` is the placeholder for the recipient's name, which
    // only the invitation an administrative create sends writes into its body.
    let rows: [(&str, Stage, &str, &str, &str); 5] = [
        (
            "a sign-up's own confirmation code",
            Stage::SignUp,
            "registrant",
            "CustomMessage_SignUp",
            ABSENT,
        ),
        (
            "that confirmation code sent again",
            Stage::ResendCode,
            "resender",
            "CustomMessage_ResendCode",
            ABSENT,
        ),
        (
            "a forgotten password's reset code",
            Stage::ForgotPassword,
            "forgetful",
            "CustomMessage_ForgotPassword",
            ABSENT,
        ),
        (
            "an administrative create's invitation",
            Stage::AdminCreateUser,
            "invited",
            "CustomMessage_AdminCreateUser",
            USERNAME_PARAMETER,
        ),
        (
            "the code for an attribute an update asked to verify",
            Stage::UpdateUserAttribute,
            "mover",
            "CustomMessage_UpdateUserAttribute",
            ABSENT,
        ),
    ];

    for (case, stage, username, source, named) in rows {
        let (status, answered) = drive(&router, &user_pool_id, &client_id, stage, username).await;
        // The function RAN and reported an error, which is "a user validation exception with
        // the Lambda service" (this service's model for `UserLambdaValidationException`), and
        // the refusal states the trigger and that error once.
        assert_eq!(
            (status, error_code(&answered)),
            (400, "UserLambdaValidationException".to_string()),
            "case {case:?}: expected the issuing operation to report the trigger's own failure, \
             got {status} with {answered}"
        );
        assert_eq!(
            message(&answered),
            format!(
                "CustomMessage failed with error source={source} code={CODE_PARAMETER} \
                 name={named} user={username}."
            ),
            "case {case:?}: expected the event the function was handed to name this stage, got \
             {answered}"
        );
    }
}

#[tokio::test]
async fn what_the_custom_message_function_answers_decides_the_issuing_operation() {
    let router = router();
    let granted = trigger_function(&router, "custom-message-answers", true).await;
    let ungranted = trigger_function(&router, "custom-message-unadmitted", false).await;
    let (_, worded) = pool_with_client(
        &router,
        "custom-message-answers",
        json!({ "CustomMessage": granted }),
    )
    .await;
    let (_, unadmitted) = pool_with_client(
        &router,
        "custom-message-unadmitted",
        json!({ "CustomMessage": ungranted }),
    )
    .await;
    let (_, untriggered) = pool_with_client(&router, "custom-message-absent", json!({})).await;

    // (case, the app client the sign-up is made through, the handler branch, expected status,
    // expected error code, expected message)
    //
    // Every row is the same `SignUp` of a fresh user over the same medium: what the pool's
    // trigger ANSWERS is the only thing that differs, so it is the only thing that can decide
    // the outcome.
    let rows: [(&str, &str, &str, u16, &str, String); 7] = [
        (
            "wording that keeps the placeholder the code is delivered in",
            &worded,
            "words",
            200,
            "",
            String::new(),
        ),
        (
            "wording that drops that placeholder",
            &worded,
            "drops",
            400,
            "InvalidLambdaResponseException",
            "The custom message does not contain the code parameter {####}".to_string(),
        ),
        (
            // The message travels over EMAIL, so the email wording is the one read: a function
            // that worded only the OTHER medium left this message to the pool's own wording,
            // which carries the code itself.
            "wording for the other medium only",
            &worded,
            "texts",
            200,
            "",
            String::new(),
        ),
        (
            "output that is not the event the trigger was handed",
            &worded,
            "elsewhere",
            400,
            "InvalidLambdaResponseException",
            "Unrecognizable lambda output".to_string(),
        ),
        (
            "a function that fails instead of wording anything",
            &worded,
            REPORT_MODE,
            400,
            "UserLambdaValidationException",
            String::new(),
        ),
        (
            // Amazon Cognito invokes as its own service principal, so a function whose policy
            // admits it nothing is NOT invoked and the invocation could not be made at all.
            "a function whose policy admits this service nothing",
            &unadmitted,
            "words",
            400,
            "UnexpectedLambdaException",
            "CustomMessage invocation failed due to error AccessDeniedException.".to_string(),
        ),
        (
            "a pool that configures no custom message trigger at all",
            &untriggered,
            "words",
            200,
            "",
            String::new(),
        ),
    ];

    for (index, (case, client_id, mode, expected_status, expected_code, expected_message)) in
        rows.into_iter().enumerate()
    {
        let username = format!("answerer{index}");
        let (status, answered) = sign_up(&router, client_id, &username, mode).await;
        assert_eq!(
            (status, error_code(&answered)),
            (expected_status, expected_code.to_string()),
            "case {case:?}: expected HTTP {expected_status} {expected_code:?}, got {status} with \
             {answered}"
        );
        if !expected_message.is_empty() {
            assert_eq!(
                message(&answered),
                expected_message,
                "case {case:?}: expected that refusal's own sentence, got {answered}"
            );
        }
        if expected_status == 200 {
            // The registration this wording belongs to is reported the way every sign-up that
            // sent a code reports it: the delivery it made, to the address the user registered.
            assert_eq!(
                answered["CodeDeliveryDetails"]["DeliveryMedium"],
                json!("EMAIL"),
                "case {case:?}: expected the sign-up to report the delivery it made, got \
                 {answered}"
            );
        }
    }
}
