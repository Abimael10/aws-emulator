// SPDX-License-Identifier: CC0-1.0

//! A user pool's SMS delivery path: what it takes for this service to deliver a text
//! message at all, and where the message goes when the pool routes it to a function.
//!
//! Two rules, over the wire.
//!
//! RULE A (a medium needs a path): a request that asks a message to be SENT over a delivery
//! medium is refused `InvalidParameterException` / HTTP 400, writing nothing, unless the pool
//! configures a path for that medium. A text message leaves a pool either through the caller
//! its `SmsConfiguration` names — "the SMS configuration with the settings for your Amazon
//! Cognito user pool to send SMS message with Amazon Simple Notification Service"
//! (`SmsConfigurationType`) — or by being handed to the function its `CustomSMSSender` names,
//! that trigger "rout[ing] all SMS notifications from a user pool to a Lambda function that
//! delivers the message using custom logic" (`LambdaConfigType$CustomSMSSender`). A pool that
//! configures neither can deliver nothing by SMS. A request that sends NO message
//! (`MessageAction` `SUPPRESS`) needs no path, and the email medium has its own.
//!
//! RULE B (the sender is the channel): on a pool that configures a `CustomSMSSender`, the
//! operation that issues a text message invokes that `LambdaArn` with the custom-SMS-sender
//! event of the stage that issued it — `triggerSource` naming the trigger and that stage, the
//! `request` being the `customSMSSenderRequestV1` document, `userAttributes` carrying the
//! user's own values and `request.code` being the issued code encrypted under
//! `LambdaConfig.KMSKeyID`, "the ARN of an KMS key. Amazon Cognito uses the key to encrypt
//! codes and temporary passwords sent to CustomEmailSender and CustomSMSSender"
//! (`LambdaConfigType$KMSKeyID`). The value the function receives is the credential the pool
//! itself accepts.
//!
//! Nothing here asserts a value baked into the service: the key ARN, the queue URL, the
//! function ARN, the pool id, the username and the delivered credential all come back from
//! the state the setup calls created.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_email_sender_node_handler::{
    cognito, create_function, kms, ok, opened_code, refusal, router, sqs, GOOD_PASSWORD, REGION,
};

/// A number satisfying the `phone_number` form every writer of that attribute is held to.
const PHONE_NUMBER: &str = "+15555550123";

/// A second one, for the user a row changes the number of.
const CHANGED_PHONE_NUMBER: &str = "+15555550124";

/// A temporary password satisfying a default pool's password policy, for the rows whose
/// request submits one.
const SUBMITTED_PASSWORD: &str = "Temp0rary!2024";

/// The Amazon SNS caller role an `SmsConfiguration` names: "the ARN of the IAM role in your
/// Amazon Web Services account that Amazon Cognito will use to send SMS messages"
/// (`SmsConfigurationType.SnsCallerArn`).
const SNS_CALLER_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/cognito-sms";

/// The sentence a request for an SMS delivery earns from a pool with no SMS path.
const NO_SMS_PATH_MESSAGE: &str = "User pool does not have SMS configuration to send messages.";

/// A Lambda function ARN that names a function nothing created, so an invocation of it can
/// only be answered by Lambda itself.
fn absent_function(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:000000000000:function:{name}")
}

/// What Lambda answers for a function it does not hold, as this service's trigger failure
/// states it: the trigger's name, then the error the invocation produced.
fn trigger_failure(trigger: &str, function_arn: &str) -> String {
    format!("{trigger} failed with error Function not found: {function_arn}:$LATEST.")
}

/// Create a symmetric key and return the ARN the key service minted for it.
async fn create_key(router: &MultiServiceRouter) -> String {
    let created = kms(router, "CreateKey", &json!({ "Description": "custom sender" })).await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey must mint a key ARN, got {created}"))
        .to_string()
}

/// Create a queue and return the URL Amazon SQS minted for it.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> String {
    let queue = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue must mint a queue URL, got {queue}"))
        .to_string()
}

/// Create a pool with `extra` merged into its configuration, plus an app client of it that
/// admits the sign-ins and self-service calls these suites make.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    extra: &Value,
) -> (String, String) {
    let mut body = json!({ "PoolName": name });
    if let Value::Object(members) = extra.clone() {
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
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{name}-app"),
            "ExplicitAuthFlows": [
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    (user_pool_id, client_id)
}

/// The handler the trigger function runs: it forwards what the custom sender event carried to
/// the queue, which is how a test observes what reached the function. The code is forwarded
/// as it arrived, so the test opens it the way any client of the key service would.
fn handler_source(queue_url: &str) -> String {
    format!(
        r#"
import {{ SQSClient, SendMessageCommand }} from "@aws-sdk/client-sqs";

const QUEUE_URL = "{queue_url}";
const sqs = new SQSClient({{ region: "{REGION}" }});

export const handler = async (event) => {{
    await sqs.send(new SendMessageCommand({{
        QueueUrl: QUEUE_URL,
        MessageBody: JSON.stringify({{
            triggerSource: event.triggerSource,
            userName: event.userName,
            userPoolId: event.userPoolId,
            type: event.request.type,
            code: event.request.code,
            phoneNumber: event.request.userAttributes.phone_number,
            hasResponse: Object.keys(event).includes("response"),
        }}),
    }}));
    return {{}};
}};
"#
    )
}

/// How long these suites wait for a message the trigger function forwarded. Generous: the
/// wait is for the message, and a loaded host takes far longer than an idle one.
const DELIVERY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a poll of the queue waits for a message to appear.
const RECEIVE_WAIT_SECONDS: u64 = 1;

/// How often a wait looks again.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(50);

/// The message the trigger function forwarded for `username`, waited for until
/// [`DELIVERY_WAIT`] is spent.
async fn next_delivery(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    username: &str,
) -> Value {
    let deadline = std::time::Instant::now() + DELIVERY_WAIT;
    loop {
        let received = sqs(
            router,
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": RECEIVE_WAIT_SECONDS,
            }),
        )
        .await;
        let bodies: Vec<Value> = received["Messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["Body"].as_str())
            .map(|body| serde_json::from_str(body).unwrap_or(Value::Null))
            .collect();
        if let Some(delivery) = bodies
            .into_iter()
            .find(|delivery| delivery["userName"].as_str() == Some(username))
        {
            return delivery;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: no custom SMS sender delivery for {username:?} reached the queue \
             within {DELIVERY_WAIT:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What a row of the delivery-path table expects of its create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    /// The create is served, by this service alone.
    Served,
    /// The create is refused for the SMS path the pool does not have, and writes no user.
    NoSmsPath,
    /// The pool's custom SMS sender was invoked and Lambda answered that it holds no such
    /// function: the invitation reached the trigger, which is the pool's SMS path.
    SenderInvoked,
}

/// One row of the delivery-path table: which pool, which request, which outcome.
#[derive(Clone, Copy, Debug)]
struct Row {
    /// What this row asserts.
    case: &'static str,
    /// Which of the three pools the create is made in.
    pool: usize,
    /// The `DesiredDeliveryMediums` the create asks for.
    mediums: &'static [&'static str],
    /// Whether the create suppresses the message (`MessageAction` `SUPPRESS`).
    suppressed: bool,
    /// What the create must answer.
    expect: Expect,
}

/// RULE A, over `AdminCreateUser`: rows differ along the SMS path the pool configures (none,
/// an Amazon SNS caller, a custom SMS sender), the mediums the request asks for and whether
/// it sends a message at all.
#[tokio::test]
async fn an_sms_delivery_needs_a_path_the_pool_configures() {
    const ROWS: &[Row] = &[
        Row {
            case: "SMS alone, on a pool with no SMS path",
            pool: 0,
            mediums: &["SMS"],
            suppressed: false,
            expect: Expect::NoSmsPath,
        },
        Row {
            case: "both mediums, on a pool with no SMS path",
            pool: 0,
            mediums: &["EMAIL", "SMS"],
            suppressed: false,
            expect: Expect::NoSmsPath,
        },
        Row {
            case: "SMS alone with the message suppressed, on a pool with no SMS path",
            pool: 0,
            mediums: &["SMS"],
            suppressed: true,
            expect: Expect::Served,
        },
        Row {
            case: "EMAIL alone, on a pool with no SMS path",
            pool: 0,
            mediums: &["EMAIL"],
            suppressed: false,
            expect: Expect::Served,
        },
        Row {
            case: "no medium asked for at all, on a pool with no SMS path",
            pool: 0,
            mediums: &[],
            suppressed: false,
            expect: Expect::Served,
        },
        Row {
            case: "SMS alone, on a pool whose SmsConfiguration names a caller",
            pool: 1,
            mediums: &["SMS"],
            suppressed: false,
            expect: Expect::Served,
        },
        Row {
            case: "both mediums, on a pool whose SmsConfiguration names a caller",
            pool: 1,
            mediums: &["SMS", "EMAIL"],
            suppressed: false,
            expect: Expect::Served,
        },
        Row {
            case: "SMS alone, on a pool that routes its SMS to a custom sender",
            pool: 2,
            mediums: &["SMS"],
            suppressed: false,
            expect: Expect::SenderInvoked,
        },
        Row {
            case: "both mediums, on a pool that routes its SMS to a custom sender",
            pool: 2,
            mediums: &["EMAIL", "SMS"],
            suppressed: false,
            expect: Expect::SenderInvoked,
        },
        Row {
            case: "SMS alone with the message suppressed, on a custom-sender pool",
            pool: 2,
            mediums: &["SMS"],
            suppressed: true,
            expect: Expect::Served,
        },
        Row {
            case: "EMAIL alone, on a pool that routes only its SMS to a custom sender",
            pool: 2,
            mediums: &["EMAIL"],
            suppressed: false,
            expect: Expect::Served,
        },
    ];

    let router = router();
    let key_arn = create_key(&router).await;
    let absent = absent_function("never-created-sms-sender");
    // The three pools a row picks from: no SMS path at all, the Amazon SNS caller path, and
    // the custom-sender path.
    let pools = [
        pool_with_client(&router, "no-sms-path", &json!({})).await,
        pool_with_client(
            &router,
            "sns-caller",
            &json!({ "SmsConfiguration": { "SnsCallerArn": SNS_CALLER_ROLE } }),
        )
        .await,
        pool_with_client(
            &router,
            "custom-sms-sender",
            &json!({
                "LambdaConfig": {
                    "CustomSMSSender": { "LambdaVersion": "V1_0", "LambdaArn": absent },
                    "KMSKeyID": key_arn,
                },
            }),
        )
        .await,
    ];

    for (index, row) in ROWS.iter().enumerate() {
        let Row {
            case,
            pool,
            mediums,
            suppressed,
            expect,
        } = *row;
        let (user_pool_id, _) = &pools[pool];
        let username = format!("invited{index}");
        let mut body = json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [
                { "Name": "email", "Value": format!("{username}@example.test") },
                { "Name": "phone_number", "Value": PHONE_NUMBER },
            ],
        });
        if !mediums.is_empty() {
            body["DesiredDeliveryMediums"] = json!(mediums);
        }
        if suppressed {
            body["MessageAction"] = json!("SUPPRESS");
        }
        let (status, payload) = cognito(&router, "AdminCreateUser", &body).await;
        let (code, message) = refusal(&payload);

        match expect {
            Expect::Served => assert_eq!(
                status, 200,
                "case {case:?}: expected this service's own answer, got {status} with {payload}"
            ),
            Expect::NoSmsPath => assert_eq!(
                (status, code.as_str(), message.as_str()),
                (400, "InvalidParameterException", NO_SMS_PATH_MESSAGE),
                "case {case:?}: expected the missing-SMS-path refusal, got {status} with \
                 {payload}"
            ),
            Expect::SenderInvoked => assert_eq!(
                (status, code.as_str(), message.as_str()),
                (
                    400,
                    "UnexpectedLambdaException",
                    trigger_failure("CustomSMSSender", &absent).as_str()
                ),
                "case {case:?}: expected the custom SMS sender's own failure, got {status} \
                 with {payload}"
            ),
        }

        // A create refused for the path the pool does not have writes NO user; one that
        // reached the pool's sender wrote the account before the message was handed over, so
        // that account reads back.
        let (read_status, read_back) = cognito(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": user_pool_id, "Username": username }),
        )
        .await;
        if expect == Expect::NoSmsPath {
            assert_eq!(
                (read_status, refusal(&read_back).0.as_str()),
                (400, "UserNotFoundException"),
                "case {case:?}: a refused create writes no user, got {read_status} with \
                 {read_back}"
            );
        } else {
            assert_eq!(
                (read_status, read_back["Username"].as_str()),
                (200, Some(username.as_str())),
                "case {case:?}: the created user must read back, got {read_status} with \
                 {read_back}"
            );
        }
    }
}

/// Where the temporary password one row's invitation comes from.
#[derive(Clone, Copy, Debug)]
enum Credential {
    /// The request submitted it: "the user's temporary password"
    /// (`AdminCreateUser.TemporaryPassword`).
    Submitted,
    /// The request submitted none, so the pool generated one: "if you don't specify a value,
    /// Amazon Cognito generates one for you" (`AdminCreateUser.TemporaryPassword`).
    Generated,
}

/// RULE B, end to end: the invitation of a create asking for SMS delivery reaches the pool's
/// custom SMS sender, carrying the temporary password encrypted under the pool's own key —
/// and that password is the credential the pool itself accepts.
#[tokio::test]
async fn the_custom_sms_sender_receives_the_password_the_sign_in_accepts() {
    let router = router();
    let key_arn = create_key(&router).await;
    let queue_url = create_queue(&router, "sms-sender-sink").await;
    let function_arn =
        create_function(&router, "custom-sms-sender", &handler_source(&queue_url)).await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "sms-routed",
        &json!({
            "LambdaConfig": {
                "CustomSMSSender": { "LambdaVersion": "V1_0", "LambdaArn": function_arn },
                "KMSKeyID": key_arn,
            },
        }),
    )
    .await;

    // One row per place the invitation's credential comes from: the rule is the trigger's
    // and not one request's, so a password the pool generated travels exactly as a submitted
    // one does.
    for credential in [Credential::Submitted, Credential::Generated] {
        let case = format!("{credential:?}");
        let username = format!("sms-invited-{}", case.to_lowercase());
        let mut body = json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "DesiredDeliveryMediums": ["SMS"],
            "UserAttributes": [{ "Name": "phone_number", "Value": PHONE_NUMBER }],
            "ClientMetadata": { "tenant": "acme" },
        });
        if matches!(credential, Credential::Submitted) {
            body["TemporaryPassword"] = json!(SUBMITTED_PASSWORD);
        }
        let created = ok(&router, "AdminCreateUser", &body).await;
        assert_eq!(
            created["User"]["UserStatus"].as_str(),
            Some("FORCE_CHANGE_PASSWORD"),
            "case {case:?}: an invited user owes its invitation a password change, got {created}"
        );

        let delivery = next_delivery(&router, &case, &queue_url, &username).await;
        assert_eq!(
            (
                delivery["triggerSource"].as_str(),
                delivery["userPoolId"].as_str(),
                delivery["type"].as_str(),
                delivery["phoneNumber"].as_str(),
                delivery["hasResponse"].as_bool(),
            ),
            (
                Some("CustomSMSSender_AdminCreateUser"),
                Some(user_pool_id.as_str()),
                Some("customSMSSenderRequestV1"),
                Some(PHONE_NUMBER),
                Some(false),
            ),
            "case {case:?}: expected the create's own SMS invitation event, got {delivery}"
        );

        let carried = delivery["code"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the delivery carries no code: {delivery}"));
        let delivered = opened_code(&router, &case, carried, &key_arn).await;
        match credential {
            Credential::Submitted => assert_eq!(
                delivered, SUBMITTED_PASSWORD,
                "case {case:?}: expected the submitted password, got {delivered:?}"
            ),
            Credential::Generated => assert!(
                !delivered.is_empty() && !delivered.chars().any(char::is_whitespace),
                "case {case:?}: expected a generated password with no whitespace, got \
                 {delivered:?}"
            ),
        }

        // The delivered password is the account's real credential: it is what the sign-in
        // accepts, and what it accepts is the challenge to replace it.
        let answered = ok(
            &router,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": delivered },
            }),
        )
        .await;
        assert_eq!(
            answered["ChallengeName"].as_str(),
            Some("NEW_PASSWORD_REQUIRED"),
            "case {case:?}: the delivered password must sign the user in, got {answered}"
        );
    }
}

/// Which code-issuing stage a row of the stage table drives, each one sending its message to
/// a phone number.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// `AdminCreateUser`, which sends an invitation's temporary password.
    AdminCreateUser,
    /// `SignUp`, which sends a new user's confirmation code.
    SignUp,
    /// `ResendConfirmationCode`, which sends it again.
    ResendCode,
    /// `ForgotPassword`, which sends a password-reset code.
    ForgotPassword,
    /// `AdminResetUserPassword`, which sends the same reset code as an administrator.
    AdminResetPassword,
    /// `UpdateUserAttributes`, which sends the code of the number it changed.
    UpdateAttributes,
    /// `AdminUpdateUserAttributes`, which sends that code as an administrator.
    AdminUpdateAttributes,
    /// `GetUserAttributeVerificationCode`, which sends one attribute's code.
    VerificationCode,
}

impl Stage {
    /// The operation a row calls last, which is the one that issues the message.
    fn operation(self) -> &'static str {
        match self {
            Self::AdminCreateUser => "AdminCreateUser",
            Self::SignUp => "SignUp",
            Self::ResendCode => "ResendConfirmationCode",
            Self::ForgotPassword => "ForgotPassword",
            Self::AdminResetPassword => "AdminResetUserPassword",
            Self::UpdateAttributes => "UpdateUserAttributes",
            Self::AdminUpdateAttributes => "AdminUpdateUserAttributes",
            Self::VerificationCode => "GetUserAttributeVerificationCode",
        }
    }
}

/// Every stage a row drives, which is every stage this service sends a text message at.
const EVERY_STAGE: [Stage; 8] = [
    Stage::AdminCreateUser,
    Stage::SignUp,
    Stage::ResendCode,
    Stage::ForgotPassword,
    Stage::AdminResetPassword,
    Stage::UpdateAttributes,
    Stage::AdminUpdateAttributes,
    Stage::VerificationCode,
];

/// Create `username` with a verified number and a permanent password, and return the access
/// token an administrative sign-in issues for it.
///
/// None of these calls sends a message: the create suppresses its invitation and an
/// administrator that vouches for the number with `phone_number_verified` skips the
/// verification message, so the pool's sender is not reached by the setup.
async fn signed_in_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> String {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": [
                { "Name": "phone_number", "Value": PHONE_NUMBER },
                { "Name": "phone_number_verified", "Value": "true" },
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
    let signed_in = ok(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in must issue an access token, got {signed_in}"))
        .to_string()
}

/// Register `username` with a phone number, and return what the registration answered.
async fn sign_up(router: &MultiServiceRouter, client_id: &str, username: &str) -> (u16, Value) {
    cognito(
        router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "UserAttributes": [{ "Name": "phone_number", "Value": PHONE_NUMBER }],
        }),
    )
    .await
}

/// Drive `stage` against `user_pool_id`, each row on a user of its own, and return what the
/// message-issuing operation answered.
async fn drive(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
    stage: Stage,
) -> (u16, Value) {
    match stage {
        Stage::AdminCreateUser => {
            cognito(
                router,
                stage.operation(),
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    "DesiredDeliveryMediums": ["SMS"],
                    "UserAttributes": [{ "Name": "phone_number", "Value": PHONE_NUMBER }],
                }),
            )
            .await
        }
        Stage::SignUp => sign_up(router, client_id, username).await,
        Stage::ResendCode => {
            // The sign-up that registers the user is the SignUp row's own subject; what it
            // answered says nothing about this row, and the user it created is there either
            // way.
            let _ = sign_up(router, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({ "ClientId": client_id, "Username": username }),
            )
            .await
        }
        Stage::ForgotPassword => {
            signed_in_user(router, user_pool_id, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({ "ClientId": client_id, "Username": username }),
            )
            .await
        }
        Stage::AdminResetPassword => {
            signed_in_user(router, user_pool_id, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({ "UserPoolId": user_pool_id, "Username": username }),
            )
            .await
        }
        Stage::UpdateAttributes => {
            let token = signed_in_user(router, user_pool_id, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({
                    "AccessToken": token,
                    "UserAttributes": [
                        { "Name": "phone_number", "Value": CHANGED_PHONE_NUMBER },
                    ],
                }),
            )
            .await
        }
        Stage::AdminUpdateAttributes => {
            signed_in_user(router, user_pool_id, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    "UserAttributes": [
                        { "Name": "phone_number", "Value": CHANGED_PHONE_NUMBER },
                    ],
                }),
            )
            .await
        }
        Stage::VerificationCode => {
            let token = signed_in_user(router, user_pool_id, client_id, username).await;
            cognito(
                router,
                stage.operation(),
                &json!({ "AccessToken": token, "AttributeName": "phone_number" }),
            )
            .await
        }
    }
}

/// RULE B over every stage: each one invokes the pool's custom SMS sender.
///
/// The pool's trigger names a function Lambda does not hold, so the refusal each row earns
/// can only have been built from an answer Lambda gave: that is what says the invocation was
/// issued at that stage.
#[tokio::test]
async fn every_sms_sending_stage_invokes_the_pools_custom_sms_sender() {
    let router = router();
    let key_arn = create_key(&router).await;
    let absent = absent_function("never-created-stage-sender");
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "absent-sms-sender",
        &json!({
            "AutoVerifiedAttributes": ["phone_number"],
            "LambdaConfig": {
                "CustomSMSSender": { "LambdaVersion": "V1_0", "LambdaArn": absent },
                "KMSKeyID": key_arn,
            },
        }),
    )
    .await;

    let expected = trigger_failure("CustomSMSSender", &absent);
    for (index, stage) in EVERY_STAGE.into_iter().enumerate() {
        let case = stage.operation();
        let username = format!("smsstage{index}");
        let (status, answer) = drive(&router, &user_pool_id, &client_id, &username, stage).await;
        let (code, message) = refusal(&answer);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "UnexpectedLambdaException", expected.as_str()),
            "case {case:?}: expected the custom SMS sender's own failure, got {status} \
             ({answer})"
        );
    }
}

/// A pool that configures no custom SMS sender hands nothing to a function: the same stages
/// are answered by this service alone, over the Amazon SNS caller path.
#[tokio::test]
async fn a_pool_with_no_custom_sms_sender_invokes_nothing() {
    let router = router();
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "unrouted-sms",
        &json!({
            "AutoVerifiedAttributes": ["phone_number"],
            "SmsConfiguration": { "SnsCallerArn": SNS_CALLER_ROLE },
        }),
    )
    .await;

    for (index, stage) in EVERY_STAGE.into_iter().enumerate() {
        let case = stage.operation();
        let username = format!("unroutedstage{index}");
        let (status, answer) = drive(&router, &user_pool_id, &client_id, &username, stage).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected this service's own answer, got {status} ({answer})"
        );
    }
}
