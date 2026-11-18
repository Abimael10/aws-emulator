// SPDX-License-Identifier: CC0-1.0

//! The code a pool's custom email sender was handed, as the ONE live credential of its flow.
//!
//! The rule pinned here: on a pool whose `LambdaConfig` carries a `CustomEmailSender` — "this
//! trigger routes all email notifications from a user pool to a Lambda function"
//! (`LambdaConfigType$CustomEmailSender`, this service's model) — the code store the trigger
//! writes is the same one the consuming operation validates against, and it holds exactly ONE
//! live code per user and purpose: a later issue of that flow's code supersedes the code
//! already delivered, the consumer accepts only the most recently delivered one and spends
//! it, and every other value is refused. So a value the trigger never delivered and the
//! superseded one are both `CodeMismatchException` — "this exception is thrown if the provided
//! code doesn't match what the server was expecting" — while a code the consumer already
//! accepted leaves nothing outstanding to compare against.
//!
//! That matters on such a pool because the function is the only place the code exists: no
//! message is mailed and no response carries it, so every code below is read out of what the
//! function itself reported.
//!
//! The rows differ along the flow the code belongs to — a sign-up confirmation superseded by
//! a resend, a password reset superseded by a second request, and an attribute verification
//! superseded by a fresh request for the attribute's code — because the rule is the code
//! store's and not one operation's. Each flow is driven through its own issuing stages and
//! its own consumer, and what the accepted code CHANGED is read back through this service's
//! own reads.
//!
//! The harness is the one `crate::custom_email_sender_node_handler` builds for this trigger:
//! a key, a queue and a `nodejs22.x` function that opens the encrypted code with the
//! runtime's own SDK and `crypto` and forwards the plaintext.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_email_sender_node_handler::{
    cognito, create_function, handler_source, kms, next_report, ok, refusal, reported_code,
    router, sqs, GOOD_PASSWORD, NEW_PASSWORD,
};

/// How many decimal digits the verification code of these stages carries. The email one-time
/// password of a sign-in challenge is a code of its own length, pinned beside the challenge
/// itself in `crate::custom_email_sender_node_handler`.
const VERIFICATION_CODE_DIGITS: usize = 6;

/// The one sentence this API refuses a value that is not the outstanding code with
/// (`CodeMismatchException`).
const MISMATCH_MESSAGE: &str = "Invalid verification code provided, please try again.";

/// Which code-bearing flow one row drives.
#[derive(Clone, Copy, Debug)]
enum Flow {
    /// A sign-up's confirmation code, resent by `ResendConfirmationCode` and consumed by
    /// `ConfirmSignUp`.
    SignUpConfirmation,
    /// A password-reset code, reissued by a second `ForgotPassword` and consumed by
    /// `ConfirmForgotPassword`.
    PasswordReset,
    /// An attribute's verification code, first sent by the update that changed the
    /// attribute, reissued by `GetUserAttributeVerificationCode` and consumed by
    /// `VerifyUserAttribute`.
    AttributeVerification,
}

/// One row: the flow, the two stages that deliver its code, and what a code the consumer
/// already accepted earns.
struct Row {
    /// The case every assertion of this row names.
    case: &'static str,
    /// The flow this row drives.
    flow: Flow,
    /// The `triggerSource` of the stage that delivered the FIRST code.
    first_source: &'static str,
    /// The `triggerSource` of the stage whose code supersedes it.
    second_source: &'static str,
    /// The error code the consumer answers once the delivered code has been spent: the
    /// account this flow already settled refuses a second settlement, and a flow whose code
    /// is simply gone has nothing live to compare a presented value against.
    spent_error: &'static str,
}

/// A value of the shape a code carries that neither delivery carried, so a refusal of it can
/// only be the store's answer about a code it never held.
fn never_delivered(first: &str, second: &str) -> String {
    (1..=9)
        .map(|step| shifted(second, step))
        .find(|candidate| candidate != first && candidate != second)
        .unwrap_or_else(|| panic!("a six-digit code has a neighbour that is neither {first} nor {second}"))
}

/// `code` with every digit moved on by `step`, which is a value of the same length.
fn shifted(code: &str, step: u32) -> String {
    code.chars()
        .map(|digit| {
            let moved = (digit.to_digit(10).unwrap_or_default() + step) % 10;
            char::from_digit(moved, 10).unwrap_or('0')
        })
        .collect()
}

/// What `AdminGetUser` reports for `username`.
async fn admin_read(router: &MultiServiceRouter, user_pool_id: &str, username: &str) -> Value {
    ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await
}

/// The value `read` holds for `attribute`, or `<absent>` when it holds none.
fn attribute_of(read: &Value, attribute: &str) -> String {
    read["UserAttributes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|held| held["Name"].as_str() == Some(attribute))
        .and_then(|held| held["Value"].as_str())
        .unwrap_or("<absent>")
        .to_string()
}

/// What this flow needs in place before any code is issued, as the access token its
/// self-service operations are authorized by (`None` for the flow whose first call creates
/// the user).
///
/// An administrator that vouches for the address with `email_verified` "skip[s] the
/// verification message" (`AdminCreateUser$UserAttributes`), so the setup issues no code and
/// the sender is reached only by the stages a row drives.
async fn given_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    flow: Flow,
    username: &str,
    address: &str,
) -> Option<String> {
    if matches!(flow, Flow::SignUpConfirmation) {
        return None;
    }
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
    let signed_in = signed_in(router, client_id, username, GOOD_PASSWORD).await;
    Some(
        signed_in["AuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in must issue an access token, got {signed_in}"))
            .to_string(),
    )
}

/// The access token a flow whose operations are authorized by one was signed in with.
fn access_token(token: Option<&str>) -> &str {
    token.expect("the self-service attribute flow signs its user in before it issues a code")
}

/// Sign `username` in with `password`, which must succeed.
async fn signed_in(
    router: &MultiServiceRouter,
    client_id: &str,
    username: &str,
    password: &str,
) -> Value {
    ok(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": password },
        }),
    )
    .await
}

/// Drive the stage that issues this flow's FIRST code.
async fn issue(
    router: &MultiServiceRouter,
    client_id: &str,
    flow: Flow,
    username: &str,
    address: &str,
    changed: &str,
    token: Option<&str>,
) {
    match flow {
        Flow::SignUpConfirmation => {
            ok(
                router,
                "SignUp",
                &json!({
                    "ClientId": client_id,
                    "Username": username,
                    "Password": GOOD_PASSWORD,
                    "UserAttributes": [{ "Name": "email", "Value": address }],
                }),
            )
            .await;
        }
        Flow::PasswordReset => {
            ok(
                router,
                "ForgotPassword",
                &json!({ "ClientId": client_id, "Username": username }),
            )
            .await;
        }
        Flow::AttributeVerification => {
            ok(
                router,
                "UpdateUserAttributes",
                &json!({
                    "AccessToken": access_token(token),
                    "UserAttributes": [{ "Name": "email", "Value": changed }],
                }),
            )
            .await;
        }
    }
}

/// Drive the stage whose code SUPERSEDES the one already delivered.
async fn reissue(
    router: &MultiServiceRouter,
    client_id: &str,
    flow: Flow,
    username: &str,
    token: Option<&str>,
) {
    match flow {
        Flow::SignUpConfirmation => {
            ok(
                router,
                "ResendConfirmationCode",
                &json!({ "ClientId": client_id, "Username": username }),
            )
            .await;
        }
        Flow::PasswordReset => {
            ok(
                router,
                "ForgotPassword",
                &json!({ "ClientId": client_id, "Username": username }),
            )
            .await;
        }
        Flow::AttributeVerification => {
            ok(
                router,
                "GetUserAttributeVerificationCode",
                &json!({ "AccessToken": access_token(token), "AttributeName": "email" }),
            )
            .await;
        }
    }
}

/// Present `code` to the operation that CONSUMES this flow's code.
async fn consume(
    router: &MultiServiceRouter,
    client_id: &str,
    flow: Flow,
    username: &str,
    token: Option<&str>,
    code: &str,
) -> (u16, Value) {
    match flow {
        Flow::SignUpConfirmation => {
            cognito(
                router,
                "ConfirmSignUp",
                &json!({
                    "ClientId": client_id,
                    "Username": username,
                    "ConfirmationCode": code,
                }),
            )
            .await
        }
        Flow::PasswordReset => {
            cognito(
                router,
                "ConfirmForgotPassword",
                &json!({
                    "ClientId": client_id,
                    "Username": username,
                    "ConfirmationCode": code,
                    "Password": NEW_PASSWORD,
                }),
            )
            .await
        }
        Flow::AttributeVerification => {
            cognito(
                router,
                "VerifyUserAttribute",
                &json!({
                    "AccessToken": access_token(token),
                    "AttributeName": "email",
                    "Code": code,
                }),
            )
            .await
        }
    }
}

/// What the accepted code CHANGED, read back through this service's own reads.
async fn assert_accepted_outcome(
    router: &MultiServiceRouter,
    case: &str,
    user_pool_id: &str,
    client_id: &str,
    flow: Flow,
    username: &str,
    changed: &str,
) {
    match flow {
        Flow::SignUpConfirmation => {
            let read = admin_read(router, user_pool_id, username).await;
            assert_eq!(
                read["UserStatus"].as_str(),
                Some("CONFIRMED"),
                "case {case:?}: expected the delivered code to confirm the account, got {read}"
            );
        }
        Flow::PasswordReset => {
            // The reset settled on the password the accepted call carried, which a sign-in
            // with it is what proves.
            let result = signed_in(router, client_id, username, NEW_PASSWORD).await;
            assert!(
                result["AuthenticationResult"]["AccessToken"]
                    .as_str()
                    .is_some_and(|token| !token.is_empty()),
                "case {case:?}: expected the reset password to sign in, got {result}"
            );
        }
        Flow::AttributeVerification => {
            let read = admin_read(router, user_pool_id, username).await;
            assert_eq!(
                (
                    attribute_of(&read, "email"),
                    attribute_of(&read, "email_verified")
                ),
                (changed.to_string(), "true".to_string()),
                "case {case:?}: expected the verified address to read back, got {read}"
            );
        }
    }
}

/// RULE: the code the custom sender delivered LAST is the only live one — a value the trigger
/// never delivered and a superseded delivery are both refused, the latest delivery is
/// accepted once, and presenting it again finds nothing outstanding.
#[tokio::test]
async fn only_the_latest_delivered_code_completes_the_flow() {
    let rows = [
        Row {
            case: "a sign-up confirmation code, resent",
            flow: Flow::SignUpConfirmation,
            first_source: "CustomEmailSender_SignUp",
            second_source: "CustomEmailSender_ResendCode",
            // "User cannot be confirmed" — the account the accepted code confirmed is not
            // confirmed a second time.
            spent_error: "NotAuthorizedException",
        },
        Row {
            case: "a password-reset code, reissued",
            flow: Flow::PasswordReset,
            first_source: "CustomEmailSender_ForgotPassword",
            second_source: "CustomEmailSender_ForgotPassword",
            spent_error: "ExpiredCodeException",
        },
        Row {
            case: "an attribute's verification code, requested again",
            flow: Flow::AttributeVerification,
            first_source: "CustomEmailSender_UpdateUserAttribute",
            second_source: "CustomEmailSender_VerifyUserAttribute",
            spent_error: "ExpiredCodeException",
        },
    ];

    let router = router();
    let queue = sqs(
        &router,
        "CreateQueue",
        &json!({ "QueueName": "live-code-reports" }),
    )
    .await;
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue must mint a URL, got {queue}"))
        .to_string();
    let created = kms(
        &router,
        "CreateKey",
        &json!({ "Description": "the custom sender's key" }),
    )
    .await;
    let key_arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey must mint a key ARN, got {created}"))
        .to_string();
    let function_arn =
        create_function(&router, "live-code-sender", &handler_source(&queue_url)).await;
    let pool = ok(
        &router,
        "CreateUserPool",
        &json!({
            "PoolName": "live-code-pool",
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
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "live-code-app",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let username = format!("livecode{index}");
        let address = format!("{username}@example.test");
        let changed = format!("{username}-changed@example.test");
        let token = given_user(
            &router,
            &user_pool_id,
            &client_id,
            row.flow,
            &username,
            &address,
        )
        .await;

        // The first delivery: the code reaches the function, under the issuing stage's own
        // source, and nothing else ever reports it.
        issue(
            &router,
            &client_id,
            row.flow,
            &username,
            &address,
            &changed,
            token.as_deref(),
        )
        .await;
        let first = reported_code(
            case,
            &next_report(&router, case, &queue_url).await,
            row.first_source,
            VERIFICATION_CODE_DIGITS,
        );

        // The second delivery supersedes it: one user and one purpose hold ONE live code.
        reissue(&router, &client_id, row.flow, &username, token.as_deref()).await;
        let second = reported_code(
            case,
            &next_report(&router, case, &queue_url).await,
            row.second_source,
            VERIFICATION_CODE_DIGITS,
        );
        assert_ne!(
            second, first,
            "case {case:?}: expected the reissue to deliver a fresh code, got {first:?} twice"
        );

        let never = never_delivered(&first, &second);
        let refused = [
            (
                "a value of the right shape the trigger never delivered",
                never.as_str(),
            ),
            ("the earlier delivery, now superseded", first.as_str()),
        ];
        for (presented, code) in refused {
            let (status, body) = consume(
                &router,
                &client_id,
                row.flow,
                &username,
                token.as_deref(),
                code,
            )
            .await;
            assert_eq!(
                (status, refusal(&body)),
                (
                    400,
                    (
                        "CodeMismatchException".to_string(),
                        MISMATCH_MESSAGE.to_string()
                    )
                ),
                "case {case:?} presenting {presented:?}: expected the mismatch refusal, got \
                 {status} with {body}"
            );
        }

        // The latest delivery is the live credential: it completes the flow, and what it
        // changed reads back.
        let (status, body) = consume(
            &router,
            &client_id,
            row.flow,
            &username,
            token.as_deref(),
            &second,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the latest delivered code to be accepted, got {status} \
             with {body}"
        );
        assert_accepted_outcome(
            &router,
            case,
            &user_pool_id,
            &client_id,
            row.flow,
            &username,
            &changed,
        )
        .await;

        // And it is spent: the same delivered code does not complete the flow twice.
        let (status, body) = consume(
            &router,
            &client_id,
            row.flow,
            &username,
            token.as_deref(),
            &second,
        )
        .await;
        assert_eq!(
            (status, refusal(&body).0),
            (400, row.spent_error.to_string()),
            "case {case:?}: expected an accepted code to be spent, got {status} with {body}"
        );
    }
}
