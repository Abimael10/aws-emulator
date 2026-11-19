// SPDX-License-Identifier: CC0-1.0

//! The attribute-verification stages of a pool that routes its email to a custom sender: each
//! answers its OWN modelled success and hands the code to the configured function.
//!
//! The rule pinned here: on a pool whose `LambdaConfig` carries a `CustomEmailSender` — "this
//! trigger routes all email notifications from a user pool to a Lambda function"
//! (`LambdaConfigType$CustomEmailSender`, this service's model) — beside a `KMSKeyID`, an
//! operation that issues a verification code answers its own modelled 200 carrying the
//! delivery it made (`UpdateUserAttributes` a `CodeDeliveryDetailsList`,
//! `GetUserAttributeVerificationCode` a `CodeDeliveryDetails`) and invokes that function with
//! the issuing stage's own `triggerSource`; the Lambda refusals those operations model
//! (`UserLambdaValidationException`, `UnexpectedLambdaException`,
//! `InvalidLambdaResponseException`, all HTTP 400 — `smithy_query` operation=
//! UpdateUserAttributes,GetUserAttributeVerificationCode) are answered only when the
//! CONFIGURED FUNCTION itself fails, and such a refusal's variable part is that function's own
//! error text.
//!
//! The function is a `nodejs22.x` ES module that binds, by NAME, the classes the managed
//! runtime's bundled client package exports for a service: an ES module's named imports are
//! resolved when the module is LINKED, before a line of the handler runs, so a package that
//! exported its client alone would fail every one of these stages at load. The names bound
//! below are the client, the `<Operation>Command` of operations of the key service's own model
//! — `Decrypt`, `Encrypt`, `GenerateDataKey`, `DescribeKey`, `ListKeys` (`smithy_query`
//! model_path=services/service-com-amazonaws-kms-trentservice/smithy-model/
//! com-amazonaws-kms-trentservice.json operation=…) — an error class of a shape that model
//! declares (`NotFoundException`) and the base exception the package names its own refusals
//! with, so what is pinned is the package's whole exported surface and not the one command the
//! flow happens to need. One of those commands then makes a REAL call, whose answer the
//! function reports, which is what says the bound names are the working classes rather than
//! stand-ins.
//!
//! The rows differ along the stage that issued the code — the update that changed the
//! attribute, and the later request for that attribute's code — because the rule is the
//! pool's and not one operation's. Nothing here asserts a value baked into a service: the key
//! ARN, the queue URL, the function ARN, the pool id, the app client id and the code itself
//! all come back from the state these calls created, and the code is only ever read out of the
//! message the function was handed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_email_sender_node_handler::{
    cognito, create_function, kms, next_report, ok, opened_code, refusal, router, sqs,
    GOOD_PASSWORD, REGION,
};

/// How many decimal digits a verification code carries.
const VERIFICATION_CODE_DIGITS: usize = 6;

/// The names the trigger function binds out of the runtime's bundled client package for the
/// key service, every one of which has to be there when the module is LINKED.
///
/// Source: the key service's own model (`smithy_query`
/// model_path=services/service-com-amazonaws-kms-trentservice/smithy-model/
/// com-amazonaws-kms-trentservice.json) — the SDK's package for a service exports its client,
/// a `<Operation>Command` per operation that model binds and a class per error shape it
/// declares, beside the base exception its refusals carry.
const BOUND_KEY_SERVICE_NAMES: &[&str] = &[
    "KMSClient",
    "DecryptCommand",
    "EncryptCommand",
    "GenerateDataKeyCommand",
    "DescribeKeyCommand",
    "ListKeysCommand",
    "NotFoundException",
    "KMSServiceException",
];

/// The names it binds out of the queue service's package, which is how it reports at all.
const BOUND_QUEUE_SERVICE_NAMES: &[&str] = &["SQSClient", "SendMessageCommand"];

/// One stage that issues an attribute's verification code.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// The update that changed the verifiable attribute, which "sends a message to [the]
    /// user with a code" for the new value.
    Update,
    /// The later request for that attribute's own code.
    Request,
}

/// One row of the table: the stage, the `triggerSource` it delivers under, and the output
/// member its own model states the delivery in.
struct Row {
    /// The case every assertion of this row names.
    case: &'static str,
    /// The stage this row drives.
    stage: Stage,
    /// The `triggerSource` the function must be handed.
    trigger_source: &'static str,
}

/// The handler the trigger function runs: it binds the bundled package's classes by name,
/// makes one real call through a command it bound, and forwards what the event carried.
///
/// The decryption of the code is the reader's, not this handler's: the encrypted message is
/// forwarded verbatim and opened by the test the way any client of the key service opens one
/// ([`opened_code`]), so what this handler pins is the module's LINK and the call it makes.
fn handler_source(queue_url: &str, key_arn: &str) -> String {
    format!(
        r#"
import {{
    KMSClient,
    DecryptCommand,
    EncryptCommand,
    GenerateDataKeyCommand,
    DescribeKeyCommand,
    ListKeysCommand,
    NotFoundException,
    KMSServiceException,
}} from "@aws-sdk/client-kms";
import {{ SQSClient, SendMessageCommand }} from "@aws-sdk/client-sqs";

const QUEUE_URL = "{queue_url}";
const KEY_ID = "{key_arn}";
const REGION = "{REGION}";

// Every name this module bound, so the report says what each one IS rather than only that
// the module linked.
const BOUND = {{
    KMSClient,
    DecryptCommand,
    EncryptCommand,
    GenerateDataKeyCommand,
    DescribeKeyCommand,
    ListKeysCommand,
    NotFoundException,
    KMSServiceException,
    SQSClient,
    SendMessageCommand,
}};

const kms = new KMSClient({{ region: REGION }});
const sqs = new SQSClient({{ region: REGION }});

export const handler = async (event) => {{
    const bound = {{}};
    for (const name of Object.keys(BOUND)) {{
        bound[name] = typeof BOUND[name];
    }}
    let report;
    try {{
        // A command bound by name, sent: the answer is the key service's own.
        const described = await kms.send(new DescribeKeyCommand({{ KeyId: KEY_ID }}));
        report = {{
            ok: true,
            bound: bound,
            describedKeyArn: described.KeyMetadata.Arn,
            triggerSource: event.triggerSource,
            userName: event.userName,
            type: event.request.type,
            code: event.request.code,
        }};
    }} catch (error) {{
        report = {{ ok: false, bound: bound, name: error.name, message: error.message }};
    }}
    await sqs.send(new SendMessageCommand({{
        QueueUrl: QUEUE_URL,
        MessageBody: JSON.stringify(report),
    }}));
    return {{}};
}};
"#
    )
}

/// The handler of a function whose OWN code fails: the one case the stages above model a
/// Lambda validation refusal for.
fn failing_handler_source() -> String {
    format!(
        r#"
export const handler = async () => {{
    throw new Error("{SENDER_ERROR}");
}};
"#
    )
}

/// The error the failing function reports, which is the variable part of the refusal its
/// pool's stages answer.
const SENDER_ERROR: &str = "the sender could not deliver this message";

/// The code the function was handed, read out of the message it was handed it in: the
/// opening is the crate's one spelling of it (`crate::custom_email_sender_node_handler`), and
/// what is asserted here is the FORM of the code that comes out.
async fn delivered_code(
    router: &MultiServiceRouter,
    case: &str,
    report: &Value,
    key_arn: &str,
) -> String {
    let carried = report["code"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the delivery carries no code: {report}"));
    let code = opened_code(router, case, carried, key_arn).await;
    assert!(
        code.len() == VERIFICATION_CODE_DIGITS && code.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected the {VERIFICATION_CODE_DIGITS} digits a verification code \
         carries, got {code:?}"
    );
    code
}

/// A pool routing its email to `function_arn` under `key_arn`, with an app client, and the id
/// of each.
async fn pool_with_client(
    router: &MultiServiceRouter,
    name: &str,
    function_arn: &str,
    key_arn: &str,
) -> (String, String) {
    let pool = ok(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": name,
            "AutoVerifiedAttributes": ["email"],
            "LambdaConfig": {
                "CustomEmailSender": { "LambdaVersion": "V1_0", "LambdaArn": function_arn },
                "KMSKeyID": key_arn,
            },
        }),
    )
    .await;
    let user_pool_id = pool["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {pool}"))
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{name}-app"),
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    (user_pool_id, client_id)
}

/// A confirmed user of `user_pool_id` whose address is already vouched for — an administrator
/// that submits `email_verified` "skip[s] the verification message"
/// (`AdminCreateUser$UserAttributes`), so the setup issues no code — and the access token it
/// signs in with.
async fn signed_in_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
    address: &str,
) -> String {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": [
                { "Name": "email", "Value": address },
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
    let signed_in = ok(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in must issue an access token, got {signed_in}"))
        .to_string()
}

/// The queue the trigger function reports to, and the key the pool wraps its codes with.
async fn queue_and_key(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let queue = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue must mint a URL, got {queue}"))
        .to_string();
    let created = kms(
        router,
        "CreateKey",
        &json!({ "Description": "the custom sender's key" }),
    )
    .await;
    let key_arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey must mint a key ARN, got {created}"))
        .to_string();
    (queue_url, key_arn)
}

/// Every name the handler bound is a CLASS, and the call it made through one of them was
/// answered by the key service's own record of the pool's key.
fn assert_bound_surface(case: &str, report: &Value, key_arn: &str) {
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the function to run to completion, got {report}"
    );
    for name in BOUND_KEY_SERVICE_NAMES
        .iter()
        .chain(BOUND_QUEUE_SERVICE_NAMES)
    {
        let bound = report["bound"][*name].as_str();
        assert_eq!(
            bound,
            Some("function"),
            "case {case:?}: expected the bundled package to export {name:?} as a class, got \
             {bound:?} in {report}"
        );
    }
    assert_eq!(
        report["describedKeyArn"].as_str(),
        Some(key_arn),
        "case {case:?}: expected the call the bound command made to be answered with the \
         pool's own key, got {report}"
    );
}

/// The one delivery a reply states, as its attribute, medium and destination.
///
/// The destination is the address MASKED — how much of one is masked is pinned by
/// `crate::code_delivery_destination_masking`, so what is asserted here is that this reply
/// reports a masked destination at all and never the address itself.
fn assert_delivery(case: &str, delivery: &Value, address: &str) {
    assert_eq!(
        (
            delivery["AttributeName"].as_str(),
            delivery["DeliveryMedium"].as_str(),
        ),
        (Some("email"), Some("EMAIL")),
        "case {case:?}: expected the delivery of the address's own code, got {delivery}"
    );
    let destination = delivery["Destination"].as_str().unwrap_or_default();
    assert!(
        destination.contains('*') && destination != address,
        "case {case:?}: expected a masked destination, got {delivery}"
    );
}

/// Drive one row's stage and answer the delivery it reported.
///
/// `destination` is the address the message goes to: the value the update writes, which is
/// then the stored value the later request addresses its own code to.
async fn issue(router: &MultiServiceRouter, row: &Row, token: &str, destination: &str) {
    match row.stage {
        Stage::Update => {
            let updated = ok(
                router,
                "UpdateUserAttributes",
                &json!({
                    "AccessToken": token,
                    "UserAttributes": [{ "Name": "email", "Value": destination }],
                }),
            )
            .await;
            let deliveries = updated["CodeDeliveryDetailsList"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the update sent a code, so it states the delivery it made, \
                         got {updated}",
                        row.case
                    )
                });
            assert_eq!(
                deliveries.len(),
                1,
                "case {:?}: expected the one message this update sent, got {updated}",
                row.case
            );
            assert_delivery(row.case, &deliveries[0], destination);
        }
        Stage::Request => {
            let requested = ok(
                router,
                "GetUserAttributeVerificationCode",
                &json!({ "AccessToken": token, "AttributeName": "email" }),
            )
            .await;
            assert!(
                requested["CodeDeliveryDetailsList"].is_null(),
                "case {:?}: this operation states its delivery as ONE member, got {requested}",
                row.case
            );
            assert_delivery(row.case, &requested["CodeDeliveryDetails"], destination);
        }
    }
}

/// RULE: each attribute-verification stage of a custom-sender pool answers its own modelled
/// 200 with the delivery it made, and hands the code to a function whose named imports of the
/// runtime's bundled client package link and run.
#[tokio::test]
async fn every_attribute_code_stage_answers_its_delivery_through_the_bundled_client() {
    let rows = [
        Row {
            case: "the update that changed the attribute",
            stage: Stage::Update,
            trigger_source: "CustomEmailSender_UpdateUserAttribute",
        },
        Row {
            case: "the later request for the attribute's code",
            stage: Stage::Request,
            trigger_source: "CustomEmailSender_VerifyUserAttribute",
        },
    ];

    let router = router();
    let (queue_url, key_arn) = queue_and_key(&router, "attribute-code-reports").await;
    let function_arn = create_function(
        &router,
        "attribute-code-sender",
        &handler_source(&queue_url, &key_arn),
    )
    .await;
    let (user_pool_id, client_id) =
        pool_with_client(&router, "attribute-code-pool", &function_arn, &key_arn).await;
    let username = "attributereader";
    let address = "attributereader@example.test";
    let changed = "attributereader-changed@example.test";
    let token = signed_in_user(&router, &user_pool_id, &client_id, username, address).await;

    let mut delivered = Vec::new();
    for row in &rows {
        let case = row.case;
        // The stage answers its OWN success, with the delivery it made.
        issue(&router, row, &token, changed).await;

        // And the code reached the configured function, under the issuing stage's own
        // source, with the module's whole bound surface in place.
        let report = next_report(&router, case, &queue_url).await;
        assert_bound_surface(case, &report, &key_arn);
        assert_eq!(
            (
                report["triggerSource"].as_str(),
                report["userName"].as_str(),
                report["type"].as_str(),
            ),
            (
                Some(row.trigger_source),
                Some(username),
                Some("customEmailSenderRequestV1"),
            ),
            "case {case:?}: expected the issuing stage's own custom sender event, got {report}"
        );
        delivered.push(delivered_code(&router, case, &report, &key_arn).await);
    }
    assert_eq!(
        delivered.len(),
        rows.len(),
        "expected one delivery per stage, got {delivered:?}"
    );
    // Which of the two deliveries is the live code is the code store's rule, pinned by
    // `crate::custom_email_sender_live_code`; the two values are minted per run, so nothing
    // here compares them.

    // The last delivered code is the live one, and what it verified reads back: the pending
    // value becomes the stored one and the attribute reads as verified.
    let case = "the delivered code";
    let last = delivered
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: the stages above delivered a code"));
    ok(
        &router,
        "VerifyUserAttribute",
        &json!({ "AccessToken": token, "AttributeName": "email", "Code": last }),
    )
    .await;
    let read = ok(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    let held = |attribute: &str| {
        read["UserAttributes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["Name"].as_str() == Some(attribute))
            .and_then(|entry| entry["Value"].as_str())
            .unwrap_or("<absent>")
            .to_string()
    };
    assert_eq!(
        (held("email"), held("email_verified")),
        (changed.to_string(), "true".to_string()),
        "case {case:?}: expected the verified address to read back, got {read}"
    );
}

/// RULE: the Lambda validation refusal of these stages is reserved for the CONFIGURED
/// function's own failure, and its variable part is that function's own error text.
#[tokio::test]
async fn only_the_configured_functions_own_failure_refuses_a_stage() {
    let router = router();
    // The pool is configured exactly as the one above but for the function it names, so the
    // only difference between the two outcomes is the function's own code.
    let (_queue_url, key_arn) = queue_and_key(&router, "failing-sender-reports").await;
    let function_arn =
        create_function(&router, "failing-sender", &failing_handler_source()).await;
    let (user_pool_id, client_id) =
        pool_with_client(&router, "failing-sender-pool", &function_arn, &key_arn).await;
    let username = "failingreader";
    let address = "failingreader@example.test";
    let token = signed_in_user(&router, &user_pool_id, &client_id, username, address).await;

    // Each stage, refused by the same sentence: the trigger's name, the function's own error,
    // terminated once.
    let expected = format!("CustomEmailSender failed with error {SENDER_ERROR}.");
    let stages = [
        (
            "the update that changed the attribute",
            "UpdateUserAttributes",
            json!({
                "AccessToken": token,
                "UserAttributes": [{ "Name": "email", "Value": "failing-changed@example.test" }],
            }),
        ),
        (
            "the later request for the attribute's code",
            "GetUserAttributeVerificationCode",
            json!({ "AccessToken": token, "AttributeName": "email" }),
        ),
    ];
    for (case, operation, body) in stages {
        let (status, refused) = cognito(&router, operation, &body).await;
        assert_eq!(
            (status, refusal(&refused)),
            (
                400,
                (
                    "UserLambdaValidationException".to_string(),
                    expected.clone(),
                )
            ),
            "case {case:?}: expected the configured function's own failure to be what refuses \
             {operation}, got {status} with {refused}"
        );
    }
}
