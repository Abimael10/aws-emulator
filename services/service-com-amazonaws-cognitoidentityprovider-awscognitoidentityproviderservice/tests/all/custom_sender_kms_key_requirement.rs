// SPDX-License-Identifier: CC0-1.0

//! The key a custom sender Lambda trigger's messages are encrypted under, over the
//! wire, driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`) and read back through
//! the operation that reports the stored configuration.
//!
//! The rule under test: a user-pool write whose `LambdaConfig` configures a custom
//! sender trigger — `CustomSMSSender` or `CustomEmailSender`, each of which
//! "routes all ... notifications from a user pool to a Lambda function that
//! delivers the message using custom logic" (this service's model) — is refused
//! `400 InvalidParameterException` unless the same `LambdaConfig` names the key
//! those messages travel under: `LambdaConfigType$KMSKeyID` is "the ARN of an KMS
//! key. Amazon Cognito uses the key to encrypt codes and temporary passwords sent
//! to custom sender Lambda triggers". A refused write stores nothing — no pool for
//! a create, and the configuration the pool already held for an update — and a
//! write that names the key is served with the `LambdaConfig` stored unchanged.
//!
//! The table's rows differ along which custom sender members the configuration
//! carries (neither, one, the other, both) and whether the key is named, together
//! with a configuration of trigger members that are NOT custom senders, which the
//! rule does not reach. Every assertion names the case and reports the actual
//! value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The sentence a user-pool write is refused with when its `LambdaConfig`
/// configures a custom sender trigger and names no key for it.
const CUSTOM_SENDER_KMS_KEY_REQUIRED: &str =
    "KMSKeyId parameter is required when Custom SMS/Email Lambda trigger is used.";

/// The function a custom SMS sender trigger names
/// (`CustomSMSLambdaVersionConfigType.LambdaArn`).
const SMS_FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:000000000000:function:custom-sms-sender";

/// The function a custom email sender trigger names
/// (`CustomEmailLambdaVersionConfigType.LambdaArn`).
const EMAIL_FUNCTION_ARN: &str =
    "arn:aws:lambda:us-east-1:000000000000:function:custom-email-sender";

/// The function a trigger that is NOT a custom sender names (`LambdaConfigType`'s
/// `PreSignUp` and `CustomMessage` members take a plain function ARN).
const PLAIN_FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:000000000000:function:pre-sign-up";

/// The key a custom sender's messages are encrypted under
/// (`LambdaConfigType.KMSKeyID`).
const KMS_KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555";

/// The ONE trigger version a custom sender is configured on: "you must use a
/// `LambdaVersion` of `V1_0` with a custom sender function", and it is the single
/// value of both enumerations
/// (`smithy_query(query="shape",shape="CustomSMSSenderLambdaVersionType,CustomEmailSenderLambdaVersionType")`).
const CUSTOM_SENDER_LAMBDA_VERSION: &str = "V1_0";

/// `QueryLimitType`'s range maximum, the most pools one `ListUserPools` page
/// serves (`smithy_query(query="operation",operation="ListUserPools")`).
const MAX_RESULTS: usize = 60;

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
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
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
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

/// The pool `user_pool_id` names, as `DescribeUserPool` renders it.
async fn described(router: &MultiServiceRouter, user_pool_id: &str) -> Value {
    ok(
        router,
        "DescribeUserPool",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await["UserPool"]
        .clone()
}

/// Whether any pool this account holds carries `pool_name`.
async fn pool_is_listed(router: &MultiServiceRouter, pool_name: &str) -> (bool, Value) {
    let listed = ok(router, "ListUserPools", &json!({ "MaxResults": MAX_RESULTS })).await;
    let named = listed["UserPools"]
        .as_array()
        .expect("a listing of pools")
        .iter()
        .any(|entry| entry["Name"] == json!(pool_name));
    (named, listed)
}

/// Assert `payload` is a 400 `InvalidParameterException` carrying `message`.
fn assert_refusal(case: &str, status: u16, payload: &Value, message: &str) {
    let code = payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    let reported = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>");
    assert_eq!(
        (status, code, reported),
        (400, "InvalidParameterException", message),
        "case {case:?}: expected a 400 InvalidParameterException carrying {message:?}, got \
         {status} with {payload}"
    );
}

/// One row: the custom sender members a `LambdaConfig` carries, whether it names
/// the key, and whether the write is served.
struct LambdaConfigCase {
    case: &'static str,
    custom_sms_sender: bool,
    custom_email_sender: bool,
    /// A trigger member that is not a custom sender, which the rule does not
    /// reach: a configuration carrying one needs no key.
    other_triggers: bool,
    kms_key: bool,
    served: bool,
}

/// The table: each row differs along which custom sender members the
/// configuration carries and whether the key those messages travel under is
/// named.
fn lambda_config_cases() -> Vec<LambdaConfigCase> {
    vec![
        LambdaConfigCase {
            case: "a custom SMS sender with no key",
            custom_sms_sender: true,
            custom_email_sender: false,
            other_triggers: false,
            kms_key: false,
            served: false,
        },
        LambdaConfigCase {
            case: "a custom email sender with no key",
            custom_sms_sender: false,
            custom_email_sender: true,
            other_triggers: false,
            kms_key: false,
            served: false,
        },
        LambdaConfigCase {
            case: "both custom senders with no key",
            custom_sms_sender: true,
            custom_email_sender: true,
            other_triggers: false,
            kms_key: false,
            served: false,
        },
        LambdaConfigCase {
            case: "a custom SMS sender beside other triggers, with no key",
            custom_sms_sender: true,
            custom_email_sender: false,
            other_triggers: true,
            kms_key: false,
            served: false,
        },
        LambdaConfigCase {
            case: "a custom SMS sender with the key",
            custom_sms_sender: true,
            custom_email_sender: false,
            other_triggers: false,
            kms_key: true,
            served: true,
        },
        LambdaConfigCase {
            case: "a custom email sender with the key",
            custom_sms_sender: false,
            custom_email_sender: true,
            other_triggers: false,
            kms_key: true,
            served: true,
        },
        LambdaConfigCase {
            case: "both custom senders with the key",
            custom_sms_sender: true,
            custom_email_sender: true,
            other_triggers: false,
            kms_key: true,
            served: true,
        },
        LambdaConfigCase {
            case: "triggers that are not custom senders, with no key",
            custom_sms_sender: false,
            custom_email_sender: false,
            other_triggers: true,
            kms_key: false,
            served: true,
        },
        LambdaConfigCase {
            case: "a key and no trigger at all",
            custom_sms_sender: false,
            custom_email_sender: false,
            other_triggers: false,
            kms_key: true,
            served: true,
        },
        LambdaConfigCase {
            case: "an empty trigger configuration",
            custom_sms_sender: false,
            custom_email_sender: false,
            other_triggers: false,
            kms_key: false,
            served: true,
        },
    ]
}

impl LambdaConfigCase {
    /// This row's `LambdaConfig`, as a write carries it.
    fn lambda_config(&self) -> Value {
        let mut members = Map::new();
        if self.custom_sms_sender {
            members.insert(
                "CustomSMSSender".to_string(),
                json!({
                    "LambdaVersion": CUSTOM_SENDER_LAMBDA_VERSION,
                    "LambdaArn": SMS_FUNCTION_ARN,
                }),
            );
        }
        if self.custom_email_sender {
            members.insert(
                "CustomEmailSender".to_string(),
                json!({
                    "LambdaVersion": CUSTOM_SENDER_LAMBDA_VERSION,
                    "LambdaArn": EMAIL_FUNCTION_ARN,
                }),
            );
        }
        if self.other_triggers {
            members.insert("PreSignUp".to_string(), json!(PLAIN_FUNCTION_ARN));
            members.insert("CustomMessage".to_string(), json!(PLAIN_FUNCTION_ARN));
        }
        if self.kms_key {
            members.insert("KMSKeyID".to_string(), json!(KMS_KEY_ARN));
        }
        Value::Object(members)
    }
}

/// Assert the pool `stored` reports exactly the `LambdaConfig` of `expected`,
/// member for member.
fn assert_lambda_config(case: &str, stored: &Value, expected: &Value) {
    let reported = stored
        .get("LambdaConfig")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    assert_eq!(
        &reported, expected,
        "case {case:?}: expected the stored LambdaConfig to be {expected}, got {reported}"
    );
}

#[tokio::test]
async fn a_custom_sender_trigger_takes_the_key_its_messages_are_encrypted_under_on_a_create() {
    let router = router();

    for (index, case) in lambda_config_cases().iter().enumerate() {
        let label = case.case;
        let pool_name = format!("custom-sender-create-{index}");
        let lambda_config = case.lambda_config();
        let (status, payload) = call(
            &router,
            "CreateUserPool",
            &json!({ "PoolName": pool_name, "LambdaConfig": lambda_config }),
        )
        .await;

        if case.served {
            assert_eq!(
                status, 200,
                "case {label:?}: CreateUserPool must serve this configuration, got {status} with \
                 {payload}"
            );
            // The configuration is stored UNCHANGED: what the create answered is
            // what a later read reports.
            assert_lambda_config(label, &payload["UserPool"], &lambda_config);
            let user_pool_id = payload["UserPool"]["Id"].as_str().expect("a minted id");
            let stored = described(&router, user_pool_id).await;
            assert_lambda_config(label, &stored, &lambda_config);
        } else {
            assert_refusal(label, status, &payload, CUSTOM_SENDER_KMS_KEY_REQUIRED);
            // A refused create leaves no pool behind.
            let (listed, listing) = pool_is_listed(&router, &pool_name).await;
            assert!(
                !listed,
                "case {label:?}: a refused create stores no pool, got {listing}"
            );
        }
    }
}

#[tokio::test]
async fn a_custom_sender_trigger_takes_the_key_its_messages_are_encrypted_under_on_an_update() {
    let router = router();

    // The configuration every row's pool starts from: a custom SMS sender WITH
    // the key, so a refused update is visible as the configuration still being
    // this one.
    let starting_config = json!({
        "CustomSMSSender": {
            "LambdaVersion": CUSTOM_SENDER_LAMBDA_VERSION,
            "LambdaArn": SMS_FUNCTION_ARN,
        },
        "KMSKeyID": KMS_KEY_ARN,
    });

    for (index, case) in lambda_config_cases().iter().enumerate() {
        let label = case.case;
        let created = ok(
            &router,
            "CreateUserPool",
            &json!({
                "PoolName": format!("custom-sender-update-{index}"),
                "LambdaConfig": starting_config.clone(),
            }),
        )
        .await;
        let user_pool_id = created["UserPool"]["Id"]
            .as_str()
            .expect("a minted id")
            .to_string();

        let lambda_config = case.lambda_config();
        let (status, payload) = call(
            &router,
            "UpdateUserPool",
            &json!({ "UserPoolId": user_pool_id, "LambdaConfig": lambda_config }),
        )
        .await;

        if case.served {
            assert_eq!(
                status, 200,
                "case {label:?}: UpdateUserPool must serve this configuration, got {status} with \
                 {payload}"
            );
            let stored = described(&router, &user_pool_id).await;
            assert_lambda_config(label, &stored, &lambda_config);
        } else {
            assert_refusal(label, status, &payload, CUSTOM_SENDER_KMS_KEY_REQUIRED);
            // A refused update leaves the pool exactly as it was.
            let stored = described(&router, &user_pool_id).await;
            assert_lambda_config(label, &stored, &starting_config);
        }
    }
}
