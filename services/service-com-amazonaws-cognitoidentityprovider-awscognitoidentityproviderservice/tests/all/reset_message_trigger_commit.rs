// SPDX-License-Identifier: CC0-1.0

//! What a password reset leaves behind when the trigger that composes its message refuses
//! it, and which of a user's values that message is addressed to.
//!
//! TWO RULES, each asserted over a TABLE whose rows differ along what the rule generalises
//! over rather than on one request:
//!
//! * RULE A — a custom message trigger "returns a runtime-customized message subject and
//!   body for your user pool to deliver to a user" (`LambdaConfigType$CustomMessage`, this
//!   service's model), so the message a password reset sends is COMPOSED before the reset
//!   it announces is recorded: a reset refused because that function answered unusably, or
//!   reported an error, leaves the account exactly as the refusal found it — the status it
//!   held and the reset code it held — while the identical call on a pool that configures
//!   no such trigger is served and records both. The rows span the two operations that
//!   send that code (`AdminResetUserPassword` and `ForgotPassword`) and the answers the
//!   function can give: output that is not the event it was handed, an event naming
//!   another user than the one it was delivered for, wording the code cannot be delivered
//!   in, and a function that fails instead of wording anything — the first three being the
//!   400 `InvalidLambdaResponseException` of "an invalid Lambda response" and the last the
//!   400 `UserLambdaValidationException` of "a user validation exception with the Lambda
//!   service" (this service's model).
//! * RULE B — which mechanism a reset is delivered over is decided by the values the USER
//!   holds verified and the order the pool's `AccountRecoverySetting` ranks its mechanisms
//!   in: "if `verified_email` has a priority of 1 and `verified_phone_number` has a
//!   priority of 2, your user pool sends account-recovery messages to a verified email
//!   address but falls back to an SMS message if the user has a verified phone number"
//!   (`RecoveryOptionType`, this service's model). What the pool configured for SENDING
//!   over a medium is no part of that decision, so the rows pair the same ranking and the
//!   same verified values with a pool that names an Amazon SNS caller, a pool that names
//!   none and a configuration that names nobody. The ONE documented disqualification —
//!   SMS for a user with SMS MFA activated — is pinned by
//!   `all/user_pool_request_preconditions.rs`.
//!
//! RULE A runs a REAL deployment package: Cognito and Lambda are mounted on one engine, as
//! the assembled emulator mounts them, and the pool's trigger names a `python3.12` function
//! whose handler answers from the branch the call's own `ClientMetadata` selected. Nothing
//! here asserts a value baked into the service: the pool ids, the client ids, the function
//! ARN and every username come back from the state the setup calls created.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use crate::trigger_source_branching::{cognito, ok, GOOD_PASSWORD};

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

/// The `ClientMetadata` key a call selects the handler's branch with.
const MODE_KEY: &str = "mode";

/// The error the handler's failing branch raises, which the refusal states back.
const REPORTED_ERROR: &str = "the trigger refused to word this message";

/// A well-formed Amazon SNS caller a pool's `SmsConfiguration` can name.
const SNS_CALLER_ARN: &str = "arn:aws:iam::000000000000:role/sms-caller";

/// The handler the trigger function runs: ONE function serving every row of RULE A, which
/// branches on the `mode` the invoking call put in its `ClientMetadata` — a member both
/// operations model and the event carries through as `request.clientMetadata`.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    request = event.get("request") or {}
    mode = (request.get("clientMetadata") or {}).get("mode", "words")
    code = request.get("codeParameter", "<none>")
    if mode == "report":
        raise Exception("the trigger refused to word this message")
    if mode == "elsewhere":
        return {"wording": "this is not the event"}
    response = event.get("response") or {}
    response["emailSubject"] = "Your verification code"
    if mode == "drops":
        response["emailMessage"] = "Welcome aboard, and nothing more"
    else:
        response["emailMessage"] = "Your code is " + code
    event["response"] = response
    if mode == "renames":
        event["userName"] = 12345
    return event
"#;

/// Cognito and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them, so
/// the trigger dispatch's in-process invocation reaches this very Lambda — with the engine
/// handed back, because a reset code is delivered out of band and no response carries it.
fn router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One REST call to Lambda, answering its status and parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> (u16, Value) {
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

/// Create the trigger function `name`, wait until Lambda reports it `Active`, grant Cognito
/// the invocation and answer the ARN Lambda minted for it.
async fn trigger_function(router: &MultiServiceRouter, name: &str) -> String {
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

    // Amazon Cognito invokes a trigger as its OWN service principal, so the function's
    // resource-based policy has to admit it — written through the one spelling of that
    // grant this crate has.
    crate::trigger_invoke_permission::grant_cognito_trigger_invoke(router, REGION, name).await;
    arn
}

/// Create a pool configured by `extra`, with an app client of it, and answer their minted
/// ids.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    extra: Value,
) -> (String, String) {
    let mut request = json!({ "PoolName": name, "AutoVerifiedAttributes": ["email"] });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            request[member] = value.clone();
        }
    }
    let created = ok(router, "CreateUserPool", &request).await;
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

/// Store `username` as a CONFIRMED user holding its own password and the `attributes` given,
/// sending no message at all: "set to SUPPRESS to suppress sending the message"
/// (`AdminCreateUser.MessageAction`), so this setup reaches no trigger and every invocation
/// a row observes is the one its own call made.
async fn given_confirmed_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, String)],
) {
    let user_attributes: Vec<Value> = attributes
        .iter()
        .map(|(name, value)| json!({ "Name": name, "Value": value }))
        .collect();
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": user_attributes,
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

/// The attributes a user whose email address is vouched for holds.
fn verified_email(username: &str) -> Vec<(&'static str, String)> {
    vec![
        ("email", format!("{username}@example.test")),
        ("email_verified", "true".to_string()),
    ]
}

/// The `UserStatus` `AdminGetUser` reports for `username`.
async fn stored_status(router: &MultiServiceRouter, user_pool_id: &str, username: &str) -> String {
    let read_back = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    read_back["UserStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("AdminGetUser must report a UserStatus, got {read_back}"))
        .to_string()
}

/// The code outstanding for a password reset of `username`, read out of the emulator's own
/// state: the real service delivers a reset code out of band, so no response carries it.
fn outstanding_reset_code(
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    username: &str,
) -> Option<cognito_idp::state::VerificationCode> {
    let service_state =
        cognito_idp::state::service_state(&**engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .and_then(|user| {
            user.outstanding_code(&cognito_idp::state::CodePurpose::PasswordReset)
        })
        .cloned()
}

/// The wire error code of an error body (`__type`), without its namespace.
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

/// Which operation a row of RULE A sends the reset code through.
#[derive(Clone, Copy, Debug)]
enum Reset {
    /// `AdminResetUserPassword`, which an administrator calls and which also moves the
    /// account into `RESET_REQUIRED`.
    Administrative,
    /// `ForgotPassword`, which the user itself calls through an app client.
    SelfService,
}

impl Reset {
    /// The operation this reset calls.
    fn operation(self) -> &'static str {
        match self {
            Self::Administrative => "AdminResetUserPassword",
            Self::SelfService => "ForgotPassword",
        }
    }

    /// The request body that drives it in the mode `mode` selects.
    fn body(self, user_pool_id: &str, client_id: &str, username: &str, mode: &str) -> Value {
        let metadata = json!({ MODE_KEY: mode });
        match self {
            Self::Administrative => json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "ClientMetadata": metadata,
            }),
            Self::SelfService => json!({
                "ClientId": client_id,
                "Username": username,
                "ClientMetadata": metadata,
            }),
        }
    }

    /// The status the account is in once this reset IS recorded: "sets the requested user's
    /// account into a RESET_REQUIRED status" (`AdminResetUserPassword` in this service's
    /// model), while a self-service request moves no status at all.
    fn status_once_recorded(self) -> &'static str {
        match self {
            Self::Administrative => "RESET_REQUIRED",
            Self::SelfService => "CONFIRMED",
        }
    }
}

/// Both resets, which is every operation of this service that sends a password-reset code.
const EVERY_RESET: [Reset; 2] = [Reset::Administrative, Reset::SelfService];

#[tokio::test]
async fn a_reset_its_message_trigger_refuses_records_nothing() {
    let (router, engine) = router_and_engine();
    let function = trigger_function(&router, "reset-message-trigger").await;
    let (triggered, triggered_client) = pool_with_client(
        &router,
        "reset-message-refused",
        json!({ "LambdaConfig": { "CustomMessage": function } }),
    )
    .await;

    // (case, the handler branch, expected error code, expected message)
    //
    // Every row is the same reset of the same kind of user over the same medium: what the
    // pool's trigger ANSWERS is the only thing that differs, so it is the only thing that
    // can decide the outcome.
    let rows: [(&str, &str, &str, String); 4] = [
        (
            "output that is not the event the trigger was handed",
            "elsewhere",
            "InvalidLambdaResponseException",
            "Unrecognizable lambda output".to_string(),
        ),
        (
            "an event naming another user than the one it was delivered for",
            "renames",
            "InvalidLambdaResponseException",
            "Invalid user name. User name in Lambda mismatches the actual username.".to_string(),
        ),
        (
            "wording the code cannot be delivered in",
            "drops",
            "InvalidLambdaResponseException",
            "The custom message does not contain the code parameter {####}".to_string(),
        ),
        (
            "a function that fails instead of wording anything",
            "report",
            "UserLambdaValidationException",
            format!("CustomMessage failed with error {REPORTED_ERROR}."),
        ),
    ];

    for (index, (case, mode, expected_code, expected_message)) in rows.into_iter().enumerate() {
        for reset in EVERY_RESET {
            let label = format!("{case} through {}", reset.operation());
            let username = format!("refused{index}{}", reset.operation().to_lowercase());
            given_confirmed_user(&router, &triggered, &username, &verified_email(&username)).await;

            let (status, answered) = cognito(
                &router,
                reset.operation(),
                &reset.body(&triggered, &triggered_client, &username, mode),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, expected_code),
                "case {label:?}: expected HTTP 400 {expected_code}, got {status} with {answered}"
            );
            assert_eq!(
                message(&answered),
                expected_message,
                "case {label:?}: expected the message {expected_message:?}, got {answered}"
            );

            // The message is composed before the reset it announces is recorded, so the
            // refusal left the account as it found it.
            let status_after = stored_status(&router, &triggered, &username).await;
            assert_eq!(
                status_after, "CONFIRMED",
                "case {label:?}: a refused reset leaves the account CONFIRMED, got \
                 {status_after:?}"
            );
            let code = outstanding_reset_code(&engine, &triggered, &username);
            assert!(
                code.is_none(),
                "case {label:?}: a refused reset leaves no code outstanding, got {code:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_same_reset_is_recorded_once_no_trigger_refuses_it() {
    let (router, engine) = router_and_engine();
    let (untriggered, untriggered_client) =
        pool_with_client(&router, "reset-message-served", json!({})).await;

    for reset in EVERY_RESET {
        let case = reset.operation();
        let username = format!("served-{}", case.to_lowercase());
        given_confirmed_user(&router, &untriggered, &username, &verified_email(&username)).await;

        let (status, answered) = cognito(
            &router,
            case,
            &reset.body(&untriggered, &untriggered_client, &username, "words"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a pool that configures no message trigger serves the reset, got \
             {status} with {answered}"
        );
        let code = outstanding_reset_code(&engine, &untriggered, &username);
        assert!(
            code.is_some(),
            "case {case:?}: a served reset records the code it sent, got {code:?}"
        );
        let status_after = stored_status(&router, &untriggered, &username).await;
        assert_eq!(
            status_after,
            reset.status_once_recorded(),
            "case {case:?}: expected the account in {:?}, got {status_after:?}",
            reset.status_once_recorded()
        );
    }
}

#[tokio::test]
async fn the_recovery_mechanism_is_chosen_from_what_the_user_holds_verified() {
    let (router, engine) = router_and_engine();
    // The pool ranks the number first and the address behind it, so a row where the number
    // does not qualify falls through to the address and a row where it does uses it.
    let phone_first = json!({
        "AccountRecoverySetting": {
            "RecoveryMechanisms": [
                { "Name": "verified_phone_number", "Priority": 1 },
                { "Name": "verified_email", "Priority": 2 },
            ]
        }
    });

    // (case, the pool's SmsConfiguration or `null` for a pool that stores none, whether the
    //  user's number is vouched for, whether its address is, the attribute the code goes to
    //  or "" for the refusal)
    type Row = (&'static str, Value, bool, bool, &'static str);
    let rows: Vec<Row> = vec![
        (
            "a verified number on a pool that names an Amazon SNS caller",
            json!({ "SnsCallerArn": SNS_CALLER_ARN }),
            true,
            true,
            "phone_number",
        ),
        (
            "the same user on a pool that stores no SmsConfiguration at all",
            Value::Null,
            true,
            true,
            "phone_number",
        ),
        (
            "the same user on a configuration that names nobody to send through",
            json!({ "SnsCallerArn": "" }),
            true,
            true,
            "phone_number",
        ),
        (
            "a verified number as the user's only vouched-for value, and no caller",
            Value::Null,
            true,
            false,
            "phone_number",
        ),
        (
            // The ranked mechanism the user holds verified is the address behind the
            // number, so that is where the code goes.
            "an unverified number and a verified address, on a pool with no caller",
            Value::Null,
            false,
            true,
            "email",
        ),
        (
            // Neither ranked mechanism has a verified value: "if neither a verified phone
            // number nor a verified email exists, Amazon Cognito responds with an
            // InvalidParameterException error" (`ForgotPassword` in this service's model).
            "neither value vouched for",
            Value::Null,
            false,
            false,
            "",
        ),
    ];

    for (index, (case, sms_configuration, number_verified, address_verified, expected)) in
        rows.into_iter().enumerate()
    {
        let mut configuration = phone_first.clone();
        if let Some(members) = sms_configuration.as_object() {
            configuration["SmsConfiguration"] = Value::Object(members.clone());
        }
        let (user_pool_id, _client_id) =
            pool_with_client(&router, &format!("recovery{index}"), configuration).await;
        let username = format!("recovery{index}");
        let number = format!("+1555010{index:04}");
        let mut attributes = vec![
            ("phone_number", number.clone()),
            ("email", format!("{username}@example.test")),
        ];
        if number_verified {
            attributes.push(("phone_number_verified", "true".to_string()));
        }
        if address_verified {
            attributes.push(("email_verified", "true".to_string()));
        }
        given_confirmed_user(&router, &user_pool_id, &username, &attributes).await;

        let (status, answered) = cognito(
            &router,
            "AdminResetUserPassword",
            &json!({ "UserPoolId": user_pool_id, "Username": username }),
        )
        .await;
        if expected.is_empty() {
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "InvalidParameterException"),
                "case {case:?}: a user holding no verified value a ranked mechanism recovers \
                 over is refused, got {status} with {answered}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected the reset to be served, got {status} with {answered}"
        );
        let code = outstanding_reset_code(&engine, &user_pool_id, &username)
            .unwrap_or_else(|| panic!("case {case:?}: a served reset records the code it sent"));
        assert_eq!(
            code.attribute, expected,
            "case {case:?}: expected the code sent to {expected}, got {code:?}"
        );
    }
}
