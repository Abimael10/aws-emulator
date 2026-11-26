// SPDX-License-Identifier: CC0-1.0

//! The delivery PATH a self-service account recovery needs, over the awsJson1_1 wire.
//!
//! The rule: `ForgotPassword` REPORTS the destination it chose (`CodeDeliveryDetails`
//! carrying the attribute, the medium and the masked address), so it chooses among the
//! mechanisms the pool's `AccountRecoverySetting` ranks that the pool can actually deliver
//! over — "if `verified_email` has a priority of 1 and `verified_phone_number` has a priority
//! of 2, your user pool sends account-recovery messages to a verified email address but falls
//! back to an SMS message if the user has a verified phone number"
//! (smithy_query(query="shape", shape="RecoveryOptionType")). A text message leaves a pool
//! only through the caller its `SmsConfiguration` names or the function its `CustomSMSSender`
//! names (smithy_query(query="shape", shape="SmsConfigurationType") and
//! `LambdaConfigType$CustomSMSSender`), so on a pool that configures neither, the
//! `verified_phone_number` mechanism does not qualify and the pool falls through to the next
//! mechanism by priority; a user left with no qualifying mechanism is refused
//! `InvalidParameterException` (HTTP 400), the class this operation models for it.
//!
//! The ADMINISTRATIVE reset (`AdminResetUserPassword`) reports no delivery of its own and is
//! not held to that path: it is unchanged here, which the last test pins.
//!
//! The table spans what the rule generalises over: which mechanism the pool ranks first and
//! how many it ranks, which delivery path the pool stores (none, an Amazon SNS caller, a
//! custom SMS sender), and which of the user's contact values are verified.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state::masked_destination;

/// This service's SigV4 signing name, the 4th field of a credential scope.
const SIGNING_NAME: &str = "cognito-idp";

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// A password satisfying a default pool's password policy, so the user owns one and a
/// self-service reset applies to it.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The sentence a reset is refused with when no mechanism the pool ranks can reach the user.
const NO_RECOVERY_DESTINATION: &str =
    "Cannot reset password for the user as there is no registered/verified email or phone_number";

/// The Amazon SNS caller an `SmsConfiguration` names, which is a path a text message leaves
/// the pool through.
const SNS_CALLER_ARN: &str = "arn:aws:iam::000000000000:role/service-role/cognito-sms";

/// A function ARN a `CustomSMSSender` names, the other such path. The suite
/// `tests/all/sms_delivery_path.rs` drives that trigger end to end, over the key service and
/// the function service it needs; here it only has to be a path the pool NAMES.
const CUSTOM_SMS_SENDER: &str = "arn:aws:lambda:us-east-1:000000000000:function:sms-sender";

/// A key ARN the pool encrypts a custom sender's codes under, which that trigger needs.
const KMS_KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/00000000-0000-0000-0000-000000000000";

/// This service alone, over one engine, with request authentication off: the suite calls
/// both the administrative and the self-service doors.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router
}

/// Call `operation` with `body` and return the status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{SIGNING_NAME}.us-east-1.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its body.
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

/// The `message` an error body carries.
fn message_of(payload: &Value) -> &str {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Create a pool carrying `configuration`, an app client of it, and a user holding
/// `attributes` with a permanent password. Answers `(user_pool_id, client_id)`.
async fn pool_with_user(
    router: &MultiServiceRouter,
    name: &str,
    configuration: &Value,
    attributes: &[(&str, &str)],
) -> (String, String) {
    let mut body = json!({ "PoolName": name });
    if let Value::Object(members) = configuration.clone() {
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
        &json!({ "UserPoolId": user_pool_id, "ClientName": "app" }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    let user_attributes: Vec<Value> = attributes
        .iter()
        .map(|(attribute, value)| json!({ "Name": attribute, "Value": value }))
        .collect();
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": name,
            "MessageAction": "SUPPRESS",
            "UserAttributes": user_attributes,
        }),
    )
    .await;
    // A self-service reset replaces the password the user OWNS, so the user is given a
    // permanent one first.
    ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": name,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    (user_pool_id, client_id)
}

/// One row: the case, the pool's configuration, the user's attributes, and the attribute the
/// reset code must go to — `""` for the refusal.
struct Row {
    case: &'static str,
    configuration: Value,
    attributes: Vec<(&'static str, &'static str)>,
    expected: &'static str,
}

/// The two contact values a row's user may hold, verified.
fn verified_email(address: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![("email", address), ("email_verified", "true")]
}

fn verified_phone(number: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![("phone_number", number), ("phone_number_verified", "true")]
}

fn rows() -> Vec<Row> {
    let sns_caller = json!({
        "SmsConfiguration": { "SnsCallerArn": SNS_CALLER_ARN, "ExternalId": "tenant" }
    });
    let phone_first = json!([
        { "Name": "verified_phone_number", "Priority": 1 },
        { "Name": "verified_email", "Priority": 2 },
    ]);
    let phone_only = json!([{ "Name": "verified_phone_number", "Priority": 1 }]);
    let email_first = json!([
        { "Name": "verified_email", "Priority": 1 },
        { "Name": "verified_phone_number", "Priority": 2 },
    ]);
    let ranked = |mechanisms: &Value, extra: &Value| -> Value {
        let mut configuration =
            json!({ "AccountRecoverySetting": { "RecoveryMechanisms": mechanisms } });
        if let Value::Object(members) = extra.clone() {
            for (member, value) in members {
                configuration[member] = value;
            }
        }
        configuration
    };
    let both = |email: &'static str, number: &'static str| {
        let mut attributes = verified_email(email);
        attributes.extend(verified_phone(number));
        attributes
    };
    vec![
        Row {
            case: "the phone mechanism ranked first, on a pool with an Amazon SNS caller",
            configuration: ranked(&phone_first, &sns_caller),
            attributes: both("a@example.test", "+15550100001"),
            expected: "phone_number",
        },
        Row {
            case: "the phone mechanism ranked first, on a pool with no SMS path",
            configuration: ranked(&phone_first, &json!({})),
            attributes: both("c@example.test", "+15550100003"),
            expected: "email",
        },
        Row {
            case: "the phone mechanism alone, on a pool with no SMS path and a verified email",
            configuration: ranked(&phone_only, &json!({})),
            attributes: both("d@example.test", "+15550100004"),
            expected: "",
        },
        Row {
            case: "the phone mechanism alone, on a pool with no SMS path and only a number",
            configuration: ranked(&phone_only, &json!({})),
            attributes: verified_phone("+15550100005"),
            expected: "",
        },
        Row {
            case: "the phone mechanism alone, on a pool with an Amazon SNS caller",
            configuration: ranked(&phone_only, &sns_caller),
            attributes: verified_phone("+15550100006"),
            expected: "phone_number",
        },
        Row {
            case: "the email mechanism ranked first, on a pool with no SMS path",
            configuration: ranked(&email_first, &json!({})),
            attributes: both("e@example.test", "+15550100007"),
            expected: "email",
        },
        Row {
            case: "the phone mechanism ranked first and only the email verified",
            configuration: ranked(&phone_first, &sns_caller),
            attributes: verified_email("f@example.test"),
            expected: "email",
        },
        Row {
            case: "a pool that configures no recovery at all, with no SMS path",
            configuration: json!({}),
            attributes: both("g@example.test", "+15550100008"),
            expected: "email",
        },
    ]
}

#[tokio::test]
async fn a_self_service_reset_only_recovers_over_a_medium_the_pool_can_deliver() {
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("recovery-path-{index}");
        let (_, client_id) =
            pool_with_user(&router, &name, &row.configuration, &row.attributes).await;

        let (status, payload) = call(
            &router,
            "ForgotPassword",
            &json!({ "ClientId": client_id, "Username": name }),
        )
        .await;

        if row.expected.is_empty() {
            assert_eq!(
                status, 400,
                "case {case:?}: a user no qualifying mechanism reaches must be refused, got \
                 {status} with {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "InvalidParameterException",
                "case {case:?}: got {payload}"
            );
            assert_eq!(
                message_of(&payload),
                NO_RECOVERY_DESTINATION,
                "case {case:?}: got {payload}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected a reported delivery, got {status} with {payload}"
        );
        let details = &payload["CodeDeliveryDetails"];
        assert_eq!(
            details["AttributeName"].as_str(),
            Some(row.expected),
            "case {case:?}: expected the code sent to {:?}, got {details}",
            row.expected
        );
        let medium = if row.expected == "phone_number" {
            "SMS"
        } else {
            "EMAIL"
        };
        assert_eq!(
            details["DeliveryMedium"].as_str(),
            Some(medium),
            "case {case:?}: expected DeliveryMedium {medium}, got {details}"
        );
        // The masked destination is the value the chosen attribute holds, reported as
        // Amazon Cognito masks it: an email keeps the first character of its local part and
        // of its domain, each followed by `***`, and a phone number keeps its leading `+`
        // and its last four characters. So the three reported members cannot disagree about
        // which contact was used, and an unmasked or unrelated address cannot pass.
        let address = row
            .attributes
            .iter()
            .find(|(attribute, _)| *attribute == row.expected)
            .map(|(_, value)| *value)
            .unwrap_or_default();
        let expected_destination = masked_destination(address);
        assert_eq!(
            details["Destination"].as_str(),
            Some(expected_destination.as_str()),
            "case {case:?}: expected the masked form {expected_destination:?} of {address:?}, \
             got {details}"
        );
    }
}

/// A pool whose SMS path is the FUNCTION its `CustomSMSSender` names qualifies the phone
/// mechanism too: the reset reaches that trigger instead of being refused for having no
/// destination at all.
///
/// The trigger itself is another suite's subject (`tests/all/sms_delivery_path.rs` drives it
/// over the key service and the function service it needs); what this pins is that the
/// mechanism QUALIFIED, which is visible in the class the reset is answered with.
#[tokio::test]
async fn a_custom_sms_sender_is_a_delivery_path_the_phone_mechanism_qualifies_on() {
    let router = router();
    let configuration = json!({
        "AccountRecoverySetting": {
            "RecoveryMechanisms": [{ "Name": "verified_phone_number", "Priority": 1 }]
        },
        "LambdaConfig": {
            "CustomSMSSender": { "LambdaVersion": "V1_0", "LambdaArn": CUSTOM_SMS_SENDER },
            "KMSKeyID": KMS_KEY_ARN,
        },
    });
    let attributes = verified_phone("+15550100010");
    let (_, client_id) =
        pool_with_user(&router, "custom-sender-path", &configuration, &attributes).await;

    let (status, payload) = call(
        &router,
        "ForgotPassword",
        &json!({ "ClientId": client_id, "Username": "custom-sender-path" }),
    )
    .await;
    assert_ne!(
        error_code(&payload),
        "InvalidParameterException",
        "the phone mechanism qualifies on a pool that routes SMS to a function, so the reset \
         is not refused for having no destination: got {status} with {payload}"
    );
    assert_ne!(
        message_of(&payload),
        NO_RECOVERY_DESTINATION,
        "got {status} with {payload}"
    );
}

/// The ADMINISTRATIVE reset is not held to the pool's delivery path: it reports no delivery
/// of its own, and succeeds for a user the self-service door cannot reach.
#[tokio::test]
async fn an_administrative_reset_is_served_whatever_path_the_pool_configures() {
    let router = router();
    let configuration = json!({
        "AccountRecoverySetting": {
            "RecoveryMechanisms": [{ "Name": "verified_phone_number", "Priority": 1 }]
        }
    });
    let attributes = verified_phone("+15550100009");
    let (user_pool_id, client_id) =
        pool_with_user(&router, "admin-reset-no-path", &configuration, &attributes).await;

    // The self-service door cannot reach this user: the one mechanism the pool ranks has no
    // path on it.
    let (status, payload) = call(
        &router,
        "ForgotPassword",
        &json!({ "ClientId": client_id, "Username": "admin-reset-no-path" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "InvalidParameterException"),
        "the self-service reset must be refused, got {status} with {payload}"
    );

    // The administrative one is served, and the user it reset is in the state that reset
    // leaves behind.
    let answered = ok(
        &router,
        "AdminResetUserPassword",
        &json!({ "UserPoolId": user_pool_id, "Username": "admin-reset-no-path" }),
    )
    .await;
    assert!(
        answered.get("CodeDeliveryDetails").is_none(),
        "an administrative reset reports no delivery of its own, got {answered}"
    );
    let described = ok(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "admin-reset-no-path" }),
    )
    .await;
    assert_eq!(
        described["UserStatus"].as_str(),
        Some("RESET_REQUIRED"),
        "the reset user must be left needing a new password, got {described}"
    );
}
