// SPDX-License-Identifier: CC0-1.0

//! The pre-sign-up Lambda trigger of a user pool, read by the function it is written for: a
//! `python3.12` handler that evaluates the registration it is handed and answers with the
//! members this trigger reads back.
//!
//! The rule pinned here: an operation that registers an account in a pool whose
//! `LambdaConfig` names a `PreSignUp` function INVOKES that function before anything is
//! stored — "this trigger evaluates new users and can bypass confirmation, link a federated
//! user profile, or block sign-up requests" (`LambdaConfigType$PreSignUp`, this service's
//! model) — and the account is stored as the function left it: a function that bypassed the
//! registrant's own confirmation leaves it `CONFIRMED` with no code to answer, one that marked
//! a value verified leaves that value's flag set, and a function that failed registers nobody
//! and refuses the operation with its modelled Lambda error.
//!
//! The function is the ASSERTION on the event: the caller states in `ValidationData` — which
//! this trigger is handed as `validationData` — what the event must say about the registration
//! it was fired for (its `triggerSource`, its `version`, the pool, the proposed username, the
//! app client, the submitted email address, the assigned client metadata, and that every
//! member it reads back was delivered unset), and the handler RAISES when the event says
//! anything else. So a row that is answered 200 is a row whose event said all of it, and a row
//! whose event did not comes back as the trigger's own refusal carrying what the function saw.
//!
//! Rows span what the rule generalises over: which member the function asks for (none, each
//! one alone, all three), whether the registration submitted the value a verified flag is
//! about, whether client metadata was assigned, which operation reached the stage (the
//! self-service registration and the administrative create, each with its own
//! `triggerSource`), and a pool that configures no trigger at all. Nothing here asserts a
//! value baked into the service: the pool id, the app client id, the function ARN, the
//! username and the email address all come back from the state the setup calls created.

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

/// A phone number in the E.164 form `phone_number` values take.
const PHONE_NUMBER: &str = "+15550100";

/// The event `version` this trigger has only one of.
const EVENT_VERSION: &str = "1";

/// `callerContext.clientId` of an event fired by an operation that names no app client, which
/// is every `Admin*` one.
const CLIENT_ID_NOT_APPLICABLE: &str = "CLIENT_ID_NOT_APPLICABLE";

/// How long this suite waits for the trigger function to become `Active`, and how often it
/// looks again. Generous: the wait is for the condition, and a loaded host takes far longer
/// than an idle one.
const ACTIVE_BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a row that judges a function's ANSWER keeps asking while the invocation does not
/// reach the function at all inside Amazon Cognito's own trigger budget. Generous, and a
/// deadline rather than a sleep: what the row waits for is an answer to judge.
const ANSWERED_BY: Duration = Duration::from_secs(120);

/// How many times such a row asks at most, so a function that answers nothing fails the row
/// on the answer it gave rather than keeping it asking to the deadline.
const ASK_AGAIN_AT_MOST: usize = 4;

/// Cognito and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them, so
/// Cognito's in-process callback reaches this very Lambda.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
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
/// `index.handler` names, and nothing else.
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

/// Create the `python3.12` trigger function and return the ARN Lambda minted for it.
///
/// The function is created and then waited FOR: an invocation only reaches a function Lambda
/// reports as `Active`, and that transition is the clock's, so the wait is for that condition
/// with a generous deadline.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri(FUNCTIONS_URI)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(
            serde_json::to_vec(&json!({
                "FunctionName": name,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Timeout": 60,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
                },
            }))
            .expect("the create body serializes"),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let created: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 201,
        "CreateFunction must answer 201, got {status} with {created}"
    );
    let deadline = Instant::now() + ACTIVE_BUDGET;
    loop {
        let read = lambda_get(router, &format!("{FUNCTIONS_URI}/{name}/configuration")).await;
        if read["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the function {name:?} never became Active within {ACTIVE_BUDGET:?}, got {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    // Amazon Cognito invokes a trigger as its OWN service principal, so the function's
    // resource-based policy has to admit it — the grant AWS asks for on every function a
    // pool's `LambdaConfig` names, written through the one spelling of it this crate has
    // (`crate::trigger_invoke_permission`, the suite that owns that rule).
    crate::trigger_invoke_permission::grant_cognito_trigger_invoke(router, REGION, name).await;
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFunction must mint an ARN, got {created}"))
        .to_string()
}

/// One Lambda read, as its parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null)
}

/// Invoke `name` DIRECTLY once, with an event shaped as this trigger's, so the rows that
/// follow do not pay the runtime's first-call cost inside Amazon Cognito's own trigger
/// budget. The answer is not read: what this buys is a runtime that has already run, so a
/// warm-up that does not come back inside a generous bound is simply left behind — the rows
/// ask for their own answer regardless.
async fn warm(router: &MultiServiceRouter, name: &str) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{FUNCTIONS_URI}/{name}/invocations"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(
            json!({
                "triggerSource": "PreSignUp_SignUp",
                "version": EVENT_VERSION,
                "userName": "warm-up",
                "request": { "userAttributes": {}, "validationData": {} },
                "response": {
                    "autoConfirmUser": false,
                    "autoVerifyEmail": false,
                    "autoVerifyPhone": false,
                },
            })
            .to_string(),
        ))
        .expect("the test request is well formed");
    let _ = tokio::time::timeout(ACTIVE_BUDGET, router.dispatch(request)).await;
}

/// The handler the trigger function runs: it CHECKS the event against what the caller stated
/// in the request's validation data, asks for the members that data named, and answers the
/// event.
///
/// It imports nothing: every name it uses is the language's own, so what the row measures is
/// the trigger and not a package's load.
const EVALUATING_HANDLER: &str = r#"
EXPECTED = (
    ("triggerSource", "expectSource"),
    ("version", "expectVersion"),
    ("userPoolId", "expectPool"),
    ("userName", "expectUser"),
)

ASKABLE = ("autoConfirmUser", "autoVerifyEmail", "autoVerifyPhone")


def handler(event, context):
    asked = event["request"].get("validationData", {})
    for member, statement in EXPECTED:
        expected = asked.get(statement)
        if expected is not None and event.get(member) != expected:
            raise Exception(
                "expected " + member + " " + repr(expected) + ", got " + repr(event.get(member))
            )
    expected_client = asked.get("expectClient")
    if expected_client is not None:
        seen_client = event["callerContext"].get("clientId")
        if seen_client != expected_client:
            raise Exception(
                "expected clientId " + repr(expected_client) + ", got " + repr(seen_client)
            )
    expected_email = asked.get("expectEmail")
    if expected_email is not None:
        seen_email = event["request"]["userAttributes"].get("email", "")
        if seen_email != expected_email:
            raise Exception(
                "expected email " + repr(expected_email) + ", got " + repr(seen_email)
            )
    expected_tenant = asked.get("expectTenant")
    if expected_tenant is not None:
        seen_tenant = event["request"].get("clientMetadata", {}).get("tenant", "")
        if seen_tenant != expected_tenant:
            raise Exception(
                "expected clientMetadata tenant " + repr(expected_tenant) + ", got "
                + repr(seen_tenant)
            )
    for member in ASKABLE:
        delivered = event["response"][member]
        if delivered is not False:
            raise Exception(
                "expected " + member + " delivered unset, got " + repr(delivered)
            )
        if asked.get(member) == "yes":
            event["response"][member] = True
    if asked.get("raise") is not None:
        raise Exception(asked["raise"])
    return event
"#;

/// Create a pool with `extra` merged into its configuration, and an app client of it. Every
/// pool here auto-verifies the email address, which is what makes an unconfirmed sign-up send
/// a confirmation code at all.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    extra: Value,
) -> (String, String) {
    let mut body = json!({
        "PoolName": name,
        "AutoVerifiedAttributes": ["email"],
        "Schema": [
            { "Name": "email", "AttributeDataType": "String" },
            { "Name": "phone_number", "AttributeDataType": "String" },
        ],
    });
    if let Value::Object(members) = extra {
        for (member, value) in members {
            body[member] = value;
        }
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

/// `ValidationData` as the wire carries it: one `AttributeType` per statement.
fn validation_data(statements: &[(&str, &str)]) -> Value {
    Value::Array(
        statements
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect(),
    )
}

/// What `AdminGetUser` reports for `username`: its status and its attributes, flattened.
async fn read_back(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
) -> (String, std::collections::BTreeMap<String, String>) {
    let read = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    let attributes = read["UserAttributes"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|attribute| {
                    (
                        attribute["Name"].as_str().unwrap_or_default().to_string(),
                        attribute["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    (
        read["UserStatus"].as_str().unwrap_or_default().to_string(),
        attributes,
    )
}

/// RULE: a self-service registration in a pool naming a `PreSignUp` function is evaluated by
/// that function, and the account is stored as the function left it.
#[tokio::test]
async fn a_sign_up_is_evaluated_by_the_pools_pre_sign_up_function() {
    let router = router();
    let function_arn = create_function(&router, "pre-sign-up", EVALUATING_HANDLER).await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "evaluated",
        json!({ "LambdaConfig": { "PreSignUp": function_arn } }),
    )
    .await;
    // The pool that configures NO trigger, which nothing evaluates.
    let (plain_pool_id, plain_client_id) = pool_with_client(&router, "unevaluated", json!({})).await;

    // (case, what the function is asked for, whether a phone number is submitted, whether
    //  metadata is assigned, which pool and client, expected UserConfirmed, expected status,
    //  expected verified flags)
    type Row = (
        &'static str,
        Vec<(&'static str, &'static str)>,
        bool,
        bool,
        bool,
        bool,
        &'static str,
        Vec<(&'static str, &'static str)>,
    );
    let rows: Vec<Row> = vec![
        (
            "a function that asks for nothing",
            vec![],
            false,
            false,
            true,
            false,
            "UNCONFIRMED",
            vec![("email_verified", "false")],
        ),
        (
            "a function that bypasses the confirmation",
            vec![("autoConfirmUser", "yes")],
            false,
            false,
            true,
            true,
            "CONFIRMED",
            vec![("email_verified", "false")],
        ),
        (
            "a function that verifies the email address alone",
            vec![("autoVerifyEmail", "yes")],
            false,
            false,
            true,
            false,
            "UNCONFIRMED",
            vec![("email_verified", "true")],
        ),
        (
            "a function that verifies the phone number alone",
            vec![("autoVerifyPhone", "yes")],
            true,
            false,
            true,
            false,
            "UNCONFIRMED",
            vec![("email_verified", "false"), ("phone_number_verified", "true")],
        ),
        (
            "a function that asks for all three",
            vec![
                ("autoConfirmUser", "yes"),
                ("autoVerifyEmail", "yes"),
                ("autoVerifyPhone", "yes"),
            ],
            true,
            true,
            true,
            true,
            "CONFIRMED",
            vec![("email_verified", "true"), ("phone_number_verified", "true")],
        ),
        (
            "a function that verifies a value the registration did not submit",
            vec![("autoVerifyPhone", "yes")],
            false,
            false,
            true,
            false,
            "UNCONFIRMED",
            vec![("email_verified", "false")],
        ),
        (
            "a pool that configures no trigger at all",
            vec![("autoConfirmUser", "yes")],
            false,
            false,
            false,
            false,
            "UNCONFIRMED",
            vec![("email_verified", "false")],
        ),
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let (
            case,
            asked,
            with_phone,
            with_metadata,
            evaluated,
            expected_confirmed,
            expected_status,
            expected_flags,
        ) = row;
        let (pool, client) = if evaluated {
            (&user_pool_id, &client_id)
        } else {
            (&plain_pool_id, &plain_client_id)
        };
        let username = format!("registrant{index}");
        let email = format!("{username}@example.test");
        // What the event must say about THIS registration, stated in the request's own
        // validation data: the function raises on anything else.
        let mut statements: Vec<(&str, &str)> = asked.clone();
        statements.push(("expectSource", "PreSignUp_SignUp"));
        statements.push(("expectVersion", EVENT_VERSION));
        statements.push(("expectPool", pool.as_str()));
        statements.push(("expectUser", username.as_str()));
        statements.push(("expectClient", client.as_str()));
        statements.push(("expectEmail", email.as_str()));
        if with_metadata {
            statements.push(("expectTenant", "acme"));
        }

        let mut attributes = vec![json!({ "Name": "email", "Value": email })];
        if with_phone {
            attributes.push(json!({ "Name": "phone_number", "Value": PHONE_NUMBER }));
        }
        let mut request = json!({
            "ClientId": client,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "UserAttributes": attributes,
            "ValidationData": validation_data(&statements),
        });
        if with_metadata {
            request["ClientMetadata"] = json!({ "tenant": "acme" });
        }

        let (status, answered) = cognito(&router, "SignUp", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the registration to be served, got {status} ({answered})"
        );
        assert_eq!(
            answered["UserConfirmed"],
            json!(expected_confirmed),
            "case {case:?}: expected UserConfirmed {expected_confirmed}, got {answered}"
        );
        // A sign-up that still owes a confirmation reports where the code went; one the
        // function confirmed owes no code and reports no delivery.
        assert_eq!(
            answered.get("CodeDeliveryDetails").is_some(),
            !expected_confirmed,
            "case {case:?}: expected a reported delivery exactly where a confirmation is \
             still owed, got {answered}"
        );

        let (stored_status, stored_attributes) = read_back(&router, pool, &username).await;
        assert_eq!(
            stored_status, expected_status,
            "case {case:?}: expected the stored user to read back {expected_status}, got \
             {stored_status:?}"
        );
        for (flag, expected) in expected_flags {
            let actual = stored_attributes.get(flag).map(String::as_str);
            assert_eq!(
                actual,
                Some(expected),
                "case {case:?}: expected {flag} {expected:?}, got {actual:?} in \
                 {stored_attributes:?}"
            );
        }
    }
}

/// RULE: the same trigger, reached by the administrative create, which reports its own
/// `triggerSource` — and the confirmation it can bypass is the registrant's own, which an
/// administratively created account does not owe.
#[tokio::test]
async fn an_administrative_create_is_evaluated_by_the_same_trigger() {
    let router = router();
    let function_arn = create_function(&router, "pre-create", EVALUATING_HANDLER).await;
    let (user_pool_id, _client_id) = pool_with_client(
        &router,
        "evaluated-admin",
        json!({ "LambdaConfig": { "PreSignUp": function_arn } }),
    )
    .await;

    // (case, what the function is asked for, expected status, expected verified flags — a
    //  flag the create records nothing for at all reads back as absent)
    type AdminRow = (
        &'static str,
        Vec<(&'static str, &'static str)>,
        &'static str,
        Vec<(&'static str, Option<&'static str>)>,
    );
    let rows: [AdminRow; 3] = [
        (
            "a function that asks for nothing",
            vec![],
            "FORCE_CHANGE_PASSWORD",
            vec![("email_verified", None)],
        ),
        (
            "a function that verifies the email address",
            vec![("autoVerifyEmail", "yes")],
            "FORCE_CHANGE_PASSWORD",
            vec![("email_verified", Some("true"))],
        ),
        (
            // The account an administrator created owes no confirmation of its own: it
            // "must change their password to complete sign-in" (`UserStatusType`), which is
            // not a confirmation this trigger bypasses.
            "a function that bypasses the confirmation",
            vec![("autoConfirmUser", "yes")],
            "FORCE_CHANGE_PASSWORD",
            vec![("email_verified", None)],
        ),
    ];

    for (index, (case, asked, expected_status, expected_flags)) in rows.into_iter().enumerate() {
        let username = format!("invited{index}");
        let email = format!("{username}@example.test");
        let mut statements: Vec<(&str, &str)> = asked.clone();
        statements.push(("expectSource", "PreSignUp_AdminCreateUser"));
        statements.push(("expectVersion", EVENT_VERSION));
        statements.push(("expectPool", user_pool_id.as_str()));
        statements.push(("expectUser", username.as_str()));
        // An `Admin*` operation names no app client, so the event reports the placeholder
        // Amazon Cognito reports in that member's place.
        statements.push(("expectClient", CLIENT_ID_NOT_APPLICABLE));
        statements.push(("expectEmail", email.as_str()));

        let (status, answered) = cognito(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "TemporaryPassword": GOOD_PASSWORD,
                "UserAttributes": [{ "Name": "email", "Value": email }],
                "ValidationData": validation_data(&statements),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status} ({answered})"
        );

        let (stored_status, stored_attributes) = read_back(&router, &user_pool_id, &username).await;
        assert_eq!(
            stored_status, expected_status,
            "case {case:?}: expected the stored user to read back {expected_status}, got \
             {stored_status:?}"
        );
        for (flag, expected) in expected_flags {
            let actual = stored_attributes.get(flag).map(String::as_str);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {flag} {expected:?}, got {actual:?} in \
                 {stored_attributes:?}"
            );
        }
    }
}

/// The handler a row of [`an_answer_that_is_not_the_event_registers_nobody`] runs: it answers
/// with whatever the caller's validation data asked for, which is either the event it was
/// handed or one of the malformed answers that event cannot be read out of.
///
/// It imports nothing: every name it uses is the language's own.
const ANSWERING_HANDLER: &str = r#"
def handler(event, context):
    asked = event["request"].get("validationData", {}).get("answer", "the-event")
    if asked == "text-instead-of-a-document":
        return "not an event at all"
    if asked == "no-output":
        return None
    if asked == "a-list":
        return [event]
    if asked == "other-members":
        return {"ok": True, "statusCode": 200}
    if asked == "a-response-half-that-is-not-an-object":
        return {"response": "autoConfirmUser"}
    if asked == "the-confirmation-as-text":
        event["response"]["autoConfirmUser"] = "true"
    elif asked == "the-confirmation-as-a-number":
        event["response"]["autoConfirmUser"] = 1
    elif asked == "the-email-verification-as-an-object":
        event["response"]["autoVerifyEmail"] = {"asked": True}
    elif asked == "another-user":
        event["userName"] = 42
    return event
"#;

/// RULE: a function answers with the EVENT it was handed — the identity that event names and
/// every response member in the type it was delivered in — so an answer this service cannot
/// read a verdict out of is an invalid Lambda response: the registration is refused with that
/// modelled error and the pool holds no such user, exactly as a function that raised registers
/// nobody.
#[tokio::test]
async fn an_answer_that_is_not_the_event_registers_nobody() {
    let router = router();
    let function = create_function(&router, "pre-sign-up-answer", ANSWERING_HANDLER).await;
    // The runtime has already run once by the time a row fires the trigger, so no row
    // spends Amazon Cognito's trigger budget on the first call's start-up.
    warm(&router, "pre-sign-up-answer").await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "answering",
        json!({ "LambdaConfig": { "PreSignUp": function } }),
    )
    .await;

    // The one sentence an answer with no verdict in it earns, whatever shape it took.
    const UNRECOGNIZABLE: &str = "Unrecognizable lambda output";
    // The sentence an answer naming another user earns.
    const ANOTHER_USER: &str =
        "Invalid user name. User name in Lambda mismatches the actual username.";

    // The rows differ along WHAT the function answered: output that is no document at all
    // (text, nothing, a list), a document carrying no response half or one that is not an
    // object, a response member stated in a type the event did not deliver it in (each
    // member, each wrong type), the envelope's own identity rewritten, and — the control —
    // the event exactly as delivered.
    //
    // (case, what the function answers, the message the registration is refused with)
    let rows: [(&str, &str, Option<&str>); 10] = [
        (
            "output that is text rather than a document",
            "text-instead-of-a-document",
            Some(UNRECOGNIZABLE),
        ),
        ("no output at all", "no-output", Some(UNRECOGNIZABLE)),
        ("output that is a list", "a-list", Some(UNRECOGNIZABLE)),
        (
            "a document carrying other members entirely",
            "other-members",
            Some(UNRECOGNIZABLE),
        ),
        (
            "a response half that is not an object",
            "a-response-half-that-is-not-an-object",
            Some(UNRECOGNIZABLE),
        ),
        (
            "the confirmation answered as text",
            "the-confirmation-as-text",
            Some(UNRECOGNIZABLE),
        ),
        (
            "the confirmation answered as a number",
            "the-confirmation-as-a-number",
            Some(UNRECOGNIZABLE),
        ),
        (
            "the email verification answered as an object",
            "the-email-verification-as-an-object",
            Some(UNRECOGNIZABLE),
        ),
        (
            "an event naming another user than the one delivered",
            "another-user",
            Some(ANOTHER_USER),
        ),
        ("the event exactly as delivered", "the-event", None),
    ];

    for (index, (case, answer, refused_with)) in rows.into_iter().enumerate() {
        // Both registering stages reach this trigger, so every row is driven through each.
        for (stage, prefix) in [("SignUp", "signed"), ("AdminCreateUser", "invited")] {
            // What the row asks is about the ANSWER a function gave, so the row is asked
            // again — with a username of its own — while the invocation did not reach the
            // function at all inside Amazon Cognito's own trigger budget, which is a
            // refusal about the invocation and no answer to judge. Bounded by a generous
            // deadline rather than by how fast the host runs, and asked again immediately:
            // nothing here waits out a span.
            let deadline = Instant::now() + ANSWERED_BY;
            for attempt in 0.. {
                let username = format!("{prefix}{index}x{attempt}");
                let email = format!("{username}@example.test");
                let statements: Vec<(&str, &str)> = vec![("answer", answer)];
                let request = if stage == "SignUp" {
                    json!({
                        "ClientId": client_id,
                        "Username": username,
                        "Password": GOOD_PASSWORD,
                        "UserAttributes": [{ "Name": "email", "Value": email }],
                        "ValidationData": validation_data(&statements),
                    })
                } else {
                    json!({
                        "UserPoolId": user_pool_id,
                        "Username": username,
                        "TemporaryPassword": GOOD_PASSWORD,
                        "UserAttributes": [{ "Name": "email", "Value": email }],
                        "ValidationData": validation_data(&statements),
                    })
                };
                let (status, answered) = cognito(&router, stage, &request).await;
                if refusal(&answered).0 == "UnexpectedLambdaException"
                    && attempt + 1 < ASK_AGAIN_AT_MOST
                {
                    assert!(
                        Instant::now() < deadline,
                        "case {case:?} ({stage}): the trigger function never answered within \
                         {ANSWERED_BY:?}, got {status} ({answered})"
                    );
                    continue;
                }
                let (read_status, read_body) = cognito(
                    &router,
                    "AdminGetUser",
                    &json!({ "UserPoolId": user_pool_id, "Username": username }),
                )
                .await;

                let Some(message) = refused_with else {
                    assert_eq!(
                        status, 200,
                        "case {case:?} ({stage}): a function that answered the event registers \
                         the user, got {status} ({answered})"
                    );
                    assert_eq!(
                        read_status, 200,
                        "case {case:?} ({stage}): the registered user must read back, got \
                         {read_status} ({read_body})"
                    );
                    break;
                };

                let (code, stated) = refusal(&answered);
                assert_eq!(
                    (status, code.as_str(), stated.as_str()),
                    (400, "InvalidLambdaResponseException", message),
                    "case {case:?} ({stage}): expected HTTP 400 InvalidLambdaResponseException \
                     {message:?}, got {status} ({answered})"
                );
                // The trigger sits before the account exists, so a refused registration
                // leaves nothing behind.
                assert_eq!(
                    refusal(&read_body).0,
                    "UserNotFoundException",
                    "case {case:?} ({stage}): expected the pool to hold no such user, got \
                     {read_status} ({read_body})"
                );
                break;
            }
        }
    }
}

#[tokio::test]
async fn a_failing_pre_sign_up_function_registers_nobody() {
    let router = router();
    let present = create_function(&router, "pre-sign-up-gate", EVALUATING_HANDLER).await;
    let absent = present.replace("pre-sign-up-gate", "never-created");
    let (blocking_pool, blocking_client) = pool_with_client(
        &router,
        "blocking",
        json!({ "LambdaConfig": { "PreSignUp": present } }),
    )
    .await;
    let (absent_pool, absent_client) = pool_with_client(
        &router,
        "absent-function",
        json!({ "LambdaConfig": { "PreSignUp": absent.clone() } }),
    )
    .await;

    // The sentence a raised handler earns states the trigger and the error the function
    // reported; the one an unreachable function earns states what Lambda answered about it.
    let raised = "this address is not allowed to register";
    // (case, pool, client, validation statements, expected code, expected message)
    type RefusalRow<'row> = (
        &'static str,
        &'row String,
        &'row String,
        Vec<(&'static str, &'static str)>,
        &'static str,
        String,
    );
    let rows: [RefusalRow<'_>; 2] = [
        (
            "a function that raised",
            &blocking_pool,
            &blocking_client,
            vec![("raise", raised)],
            "UserLambdaValidationException",
            format!("PreSignUp failed with error {raised}."),
        ),
        (
            "a trigger naming a function Lambda does not hold",
            &absent_pool,
            &absent_client,
            vec![],
            "UnexpectedLambdaException",
            format!("PreSignUp failed with error Function not found: {absent}."),
        ),
    ];

    for (index, (case, pool, client, statements, expected_code, expected_message)) in
        rows.into_iter().enumerate()
    {
        let username = format!("refused{index}");
        let (status, answered) = cognito(
            &router,
            "SignUp",
            &json!({
                "ClientId": client,
                "Username": username,
                "Password": GOOD_PASSWORD,
                "UserAttributes": [
                    { "Name": "email", "Value": format!("{username}@example.test") },
                ],
                "ValidationData": validation_data(&statements),
            }),
        )
        .await;
        let (code, message) = refusal(&answered);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, expected_code, expected_message.as_str()),
            "case {case:?}: expected HTTP 400 {expected_code} {expected_message:?}, got \
             {status} ({answered})"
        );
        // Nothing was stored: the trigger sits before the account exists.
        let (read_status, read_body) = cognito(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": pool, "Username": username }),
        )
        .await;
        assert_eq!(
            refusal(&read_body).0,
            "UserNotFoundException",
            "case {case:?}: expected the pool to hold no such user, got {read_status} \
             ({read_body})"
        );

        // The same gate on the administrative create, which reaches the same stage.
        let invited = format!("refused-admin{index}");
        let (admin_status, admin_answer) = cognito(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": pool,
                "Username": invited,
                "TemporaryPassword": GOOD_PASSWORD,
                "UserAttributes": [
                    { "Name": "email", "Value": format!("{invited}@example.test") },
                ],
                "ValidationData": validation_data(&statements),
            }),
        )
        .await;
        let (admin_code, admin_message) = refusal(&admin_answer);
        assert_eq!(
            (admin_status, admin_code.as_str(), admin_message.as_str()),
            (400, expected_code, expected_message.as_str()),
            "case {case:?} (AdminCreateUser): expected HTTP 400 {expected_code} \
             {expected_message:?}, got {admin_status} ({admin_answer})"
        );
        let (read_status, read_body) = cognito(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": pool, "Username": invited }),
        )
        .await;
        assert_eq!(
            refusal(&read_body).0,
            "UserNotFoundException",
            "case {case:?} (AdminCreateUser): expected the pool to hold no such user, got \
             {read_status} ({read_body})"
        );
    }
}
