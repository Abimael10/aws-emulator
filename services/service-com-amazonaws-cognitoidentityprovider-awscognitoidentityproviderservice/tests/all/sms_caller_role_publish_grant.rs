// SPDX-License-Identifier: CC0-1.0

//! The Amazon SNS caller role a user pool is bound to, probed as the pool would
//! really use it.
//!
//! RULE: a pool sends a text message by ASSUMING the role its `SmsConfiguration`
//! names and PUBLISHING as it — `SnsCallerArn` is "the ARN of the IAM role in
//! your Amazon Web Services account that Amazon Cognito will use to send SMS
//! messages" (`SmsConfigurationType` in this service's model) — so a write that
//! binds one is served only where this account's IAM answers that BOTH hold, and
//! the two defects are reported apart:
//!
//! * the assume Amazon Cognito would make is the one it really makes, presenting
//!   the configuration's `ExternalId` (`sts:ExternalId`) for THIS pool
//!   (`aws:SourceArn`) in its account (`aws:SourceAccount`), so a trust policy
//!   narrowed by any of those keys is read as the grant it is; a document that
//!   withholds that assume is `InvalidSmsRoleTrustRelationshipException`;
//! * a role that CAN be assumed still sends nothing unless the permissions in
//!   force on it allow the publish — which names no resource of its own, an SMS
//!   message going to a phone number rather than to a topic — and a role that
//!   withholds it is `InvalidSmsRoleAccessPolicyException`.
//!
//! A refused write stores nothing: the pool it would have created never appears
//! in `ListUserPools`, and a refused update leaves the pool's configuration as it
//! was.
//!
//! The roles, their trust documents and their permission policies are IAM's OWN
//! records, written through IAM's own `CreateRole` and `PutRolePolicy` over the
//! same engine, so what this service reads is what IAM persisted.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use crate::iam_authorization::{form_encode, xml_element};

/// The Region every call below is made in.
const REGION: &str = "us-east-1";

/// A request's SigV4 credential scope is what routes it to this service. Nothing
/// is signed here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-target, Signature=0000";

/// The service principal a user pool takes its SMS caller role on as.
const COGNITO_PRINCIPAL: &str = "cognito-idp.amazonaws.com";

/// The sentence a write naming a role Amazon Cognito cannot assume is refused
/// with.
const TRUST_RELATIONSHIP_MESSAGE: &str =
    "Role does not have a trust relationship allowing Cognito to assume the role";

/// The sentence a write naming a role that cannot publish is refused with.
const ACCESS_POLICY_MESSAGE: &str = "Role does not have permission to publish with SNS";

/// The error code of the first refusal.
const TRUST_RELATIONSHIP_CODE: &str = "InvalidSmsRoleTrustRelationshipException";

/// The error code of the second.
const ACCESS_POLICY_CODE: &str = "InvalidSmsRoleAccessPolicyException";

/// The external id every configuration of the trust table presents, so each row
/// differs only in what the grant is narrowed to.
const PRESENTED_EXTERNAL_ID: &str = "tenant-one";

/// This service over the SAME engine as IAM, so a caller role resolves in the
/// service that owns it — exactly what `core/router` assembles.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One Cognito call: `awsJson1_1`, addressed by `X-Amz-Target`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
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
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// A Cognito call that must succeed, with its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// One awsQuery call to IAM, answered with its status and XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Create a role carrying `trust` and, when one is given, the inline permissions
/// policy `permissions`; answer the ARN IAM persisted for it.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    trust: &Value,
    permissions: Option<&Value>,
) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be created in IAM first, got {status} {created}"
    );
    if let Some(permissions) = permissions {
        let (status, written) = iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "sms-caller-permissions"),
                ("PolicyDocument", &permissions.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the role's permissions must be stored, got {status} {written}"
        );
    }
    xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole answers the role's Arn, got {created}"))
}

/// This account's id, as IAM mints every ARN with.
fn account() -> &'static str {
    ::engine::emulator_account_id()
}

/// A trust policy allowing `cognito-idp.amazonaws.com` the assume, narrowed by
/// `condition` when one is given.
fn trust(condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": COGNITO_PRINCIPAL },
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A permissions policy with one statement: `effect` on `action` over
/// `resource`.
fn permissions(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

/// The grant AWS documents for an Amazon SNS caller role: publishing, which
/// names no resource of its own.
fn publish_grant() -> Value {
    permissions("Allow", "sns:Publish", "*")
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

/// The value at a dotted path of `body`.
fn at<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = body;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

/// Whether `ListUserPools` reports a pool named `pool_name`.
async fn listed(router: &MultiServiceRouter, pool_name: &str) -> bool {
    let listing = ok(router, "ListUserPools", &json!({ "MaxResults": 60 })).await;
    listing["UserPools"]
        .as_array()
        .expect("a listing of pools")
        .iter()
        .any(|entry| entry["Name"] == json!(pool_name))
}

/// What a row expects of the write: service, or one of the two refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The configuration is stored.
    Served,
    /// `InvalidSmsRoleTrustRelationshipException`.
    Unassumable,
    /// `InvalidSmsRoleAccessPolicyException`.
    NoPublish,
}

impl Expected {
    /// The wire error code and message a refusal carries, and `None` for a row
    /// that is served.
    fn refusal(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Served => None,
            Self::Unassumable => Some((TRUST_RELATIONSHIP_CODE, TRUST_RELATIONSHIP_MESSAGE)),
            Self::NoPublish => Some((ACCESS_POLICY_CODE, ACCESS_POLICY_MESSAGE)),
        }
    }
}

/// Assert that `payload` is the answer `expected` describes for `operation`.
fn assert_answer(case: &str, operation: &str, expected: Expected, status: u16, payload: &Value) {
    match expected.refusal() {
        None => assert_eq!(
            status, 200,
            "case {case:?}: {operation} must serve the role, got {status} with {payload}"
        ),
        Some((code, message)) => {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse the role, got {status} with {payload}"
            );
            assert_eq!(
                error_code(payload),
                code,
                "case {case:?}: {operation} must name {code}, got {payload}"
            );
            assert_eq!(
                message_of(payload),
                message,
                "case {case:?}: {operation} must carry the refusal sentence, got {payload}"
            );
        }
    }
}

/// Bind the caller `role_arn` with `external_id` through `CreateUserPool` and
/// hold the answer to `expected`, checking what the account holds afterwards.
async fn create_binding(
    router: &MultiServiceRouter,
    case: &str,
    pool_name: &str,
    role_arn: &str,
    external_id: &str,
    expected: Expected,
) {
    let (status, payload) = call(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": pool_name,
            "SmsConfiguration": { "SnsCallerArn": role_arn, "ExternalId": external_id },
        }),
    )
    .await;
    assert_answer(case, "CreateUserPool", expected, status, &payload);
    if expected == Expected::Served {
        let user_pool_id = payload["UserPool"]["Id"]
            .as_str()
            .expect("a minted pool id")
            .to_string();
        let described = ok(
            router,
            "DescribeUserPool",
            &json!({ "UserPoolId": user_pool_id }),
        )
        .await["UserPool"]
            .clone();
        assert_eq!(
            at(&described, "SmsConfiguration.SnsCallerArn"),
            Some(&json!(role_arn)),
            "case {case:?}: the stored pool must carry the caller it was created with, got \
             {described}"
        );
        assert!(
            listed(router, pool_name).await,
            "case {case:?}: a served create stores the pool"
        );
    } else {
        assert!(
            !listed(router, pool_name).await,
            "case {case:?}: a refused create stores no pool"
        );
    }
}

/// RULE, first half: the trust document is read against the assume Amazon Cognito
/// really makes, so each key that assume carries decides a grant narrowed by it.
///
/// Every role here holds the publish grant, so the trust document is what decides
/// the row. The rows differ along which condition keys the grant is narrowed by
/// and whether this pool's assume satisfies them.
#[tokio::test]
async fn the_sms_caller_trust_grant_is_read_against_the_keys_the_assume_carries() {
    let router = router();
    let account = account();

    // (case, the Condition the grant is narrowed by, the expected answer). The
    // external id the request presents is the row's own, below.
    let rows: Vec<(&str, Option<Value>, Expected)> = vec![
        ("an unconditioned grant", None, Expected::Served),
        (
            "a grant narrowed to the external id the configuration presents",
            Some(json!({ "StringEquals": { "sts:ExternalId": PRESENTED_EXTERNAL_ID } })),
            Expected::Served,
        ),
        (
            "a grant narrowed to the external id and the account that owns the pool",
            Some(json!({
                "StringEquals": {
                    "sts:ExternalId": PRESENTED_EXTERNAL_ID,
                    "aws:SourceAccount": account,
                }
            })),
            Expected::Served,
        ),
        (
            "a grant narrowed to every user pool of this account in this Region",
            Some(json!({
                "StringEquals": {
                    "sts:ExternalId": PRESENTED_EXTERNAL_ID,
                    "aws:SourceAccount": account,
                },
                "ArnLike": {
                    "aws:SourceArn": format!("arn:aws:cognito-idp:{REGION}:{account}:userpool/*")
                }
            })),
            Expected::Served,
        ),
        (
            "a grant narrowed to another external id",
            Some(json!({ "StringEquals": { "sts:ExternalId": "another-tenant" } })),
            Expected::Unassumable,
        ),
        (
            "a grant narrowed to another account",
            Some(json!({ "StringEquals": { "aws:SourceAccount": "123456789012" } })),
            Expected::Unassumable,
        ),
        (
            "a grant narrowed to this account's pools in another Region",
            Some(json!({
                "ArnLike": {
                    "aws:SourceArn": format!("arn:aws:cognito-idp:eu-west-1:{account}:userpool/*")
                }
            })),
            Expected::Unassumable,
        ),
        (
            "a grant narrowed to one other user pool",
            Some(json!({
                "ArnEquals": {
                    "aws:SourceArn":
                        format!("arn:aws:cognito-idp:{REGION}:{account}:userpool/{REGION}_other")
                }
            })),
            Expected::Unassumable,
        ),
    ];

    for (index, (case, condition, expected)) in rows.into_iter().enumerate() {
        let role_arn = given_role(
            &router,
            case,
            &format!("trusted-caller-{index}"),
            &trust(condition),
            Some(&publish_grant()),
        )
        .await;
        create_binding(
            &router,
            case,
            &format!("trust-{index}"),
            &role_arn,
            PRESENTED_EXTERNAL_ID,
            expected,
        )
        .await;
    }
}

/// RULE, second half: a role Amazon Cognito CAN assume still publishes nothing
/// unless the permissions in force on it allow the publish, and a role that
/// withholds it is refused with the access-policy error rather than the trust
/// one.
///
/// Every role here carries the trust document AWS documents for an SMS caller,
/// so the permissions document is what decides the row. The rows differ along
/// what that document grants: the action, the resource it is scoped to, the
/// effect, and a role carrying no permissions document at all.
#[tokio::test]
async fn an_sms_caller_role_is_bound_only_where_its_permissions_allow_the_publish() {
    let router = router();
    let account = account();
    let topic = format!("arn:aws:sns:{REGION}:{account}:verification");

    // (case, the inline permissions document the role holds, the expected answer)
    let rows: Vec<(&str, Option<Value>, Expected)> = vec![
        (
            "the publish grant AWS documents for the role",
            Some(publish_grant()),
            Expected::Served,
        ),
        (
            "every Amazon SNS action over every resource",
            Some(permissions("Allow", "sns:*", "*")),
            Expected::Served,
        ),
        (
            "every action over every resource",
            Some(permissions("Allow", "*", "*")),
            Expected::Served,
        ),
        ("no permissions document at all", None, Expected::NoPublish),
        (
            "another Amazon SNS action over every resource",
            Some(permissions("Allow", "sns:Subscribe", "*")),
            Expected::NoPublish,
        ),
        (
            "another service's action over every resource",
            Some(permissions("Allow", "sqs:SendMessage", "*")),
            Expected::NoPublish,
        ),
        (
            // A text message is published to a phone number rather than to a
            // topic, so a grant scoped to one topic covers no SMS the pool sends.
            "the publish scoped to one topic",
            Some(permissions("Allow", "sns:Publish", &topic)),
            Expected::NoPublish,
        ),
        (
            "the publish explicitly denied beside the grant",
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "sns:Publish", "Resource": "*" },
                    { "Effect": "Deny", "Action": "sns:Publish", "Resource": "*" },
                ],
            })),
            Expected::NoPublish,
        ),
    ];

    for (index, (case, document, expected)) in rows.into_iter().enumerate() {
        let external_id = format!("publish-tenant-{index}");
        let narrowed = json!({
            "StringEquals": { "sts:ExternalId": external_id, "aws:SourceAccount": account },
            "ArnLike": {
                "aws:SourceArn": format!("arn:aws:cognito-idp:{REGION}:{account}:userpool/*")
            }
        });
        let role_arn = given_role(
            &router,
            case,
            &format!("publishing-caller-{index}"),
            &trust(Some(narrowed)),
            document.as_ref(),
        )
        .await;
        create_binding(
            &router,
            case,
            &format!("publish-{index}"),
            &role_arn,
            &external_id,
            expected,
        )
        .await;
    }
}

/// RULE, over the OTHER writes that bind the same configuration: `UpdateUserPool`
/// and `SetUserPoolMfaConfig` probe the role exactly as the create does, and a
/// refused write leaves the pool holding no SMS configuration at all.
#[tokio::test]
async fn the_same_two_verdicts_decide_an_update_and_an_mfa_write() {
    let router = router();
    let account = account();
    let external_id = "update-tenant";
    let narrowed = json!({
        "StringEquals": { "sts:ExternalId": external_id, "aws:SourceAccount": account },
        "ArnLike": {
            "aws:SourceArn": format!("arn:aws:cognito-idp:{REGION}:{account}:userpool/*")
        }
    });

    // (case, the trust document, the permissions document, the expected answer)
    let rows: Vec<(&str, Value, Option<Value>, Expected)> = vec![
        (
            "a role that trusts Amazon Cognito and may publish",
            trust(Some(narrowed.clone())),
            Some(publish_grant()),
            Expected::Served,
        ),
        (
            "a role Amazon Cognito may not assume",
            trust(Some(
                json!({ "StringEquals": { "sts:ExternalId": "another-tenant" } }),
            )),
            Some(publish_grant()),
            Expected::Unassumable,
        ),
        (
            "a role Amazon Cognito may assume that may not publish",
            trust(Some(narrowed.clone())),
            None,
            Expected::NoPublish,
        ),
    ];

    for (index, (case, trust_document, document, expected)) in rows.into_iter().enumerate() {
        let role_arn = given_role(
            &router,
            case,
            &format!("written-caller-{index}"),
            &trust_document,
            document.as_ref(),
        )
        .await;
        let sms_configuration = json!({ "SnsCallerArn": role_arn, "ExternalId": external_id });
        let user_pool_id = ok(
            &router,
            "CreateUserPool",
            &json!({ "PoolName": format!("written-{index}") }),
        )
        .await["UserPool"]["Id"]
            .as_str()
            .expect("a minted pool id")
            .to_string();

        let (status, payload) = call(
            &router,
            "UpdateUserPool",
            &json!({
                "UserPoolId": user_pool_id,
                "SmsConfiguration": sms_configuration,
            }),
        )
        .await;
        assert_answer(case, "UpdateUserPool", expected, status, &payload);
        let described = ok(
            &router,
            "DescribeUserPool",
            &json!({ "UserPoolId": user_pool_id }),
        )
        .await["UserPool"]
            .clone();
        let stored = at(&described, "SmsConfiguration.SnsCallerArn");
        if expected == Expected::Served {
            assert_eq!(
                stored,
                Some(&json!(role_arn)),
                "case {case:?}: the updated pool must report the caller it was given, got \
                 {described}"
            );
        } else {
            assert_eq!(
                stored, None,
                "case {case:?}: a refused update stores no SMS configuration, got {described}"
            );
        }

        // The MFA write carries the same configuration, on a pool of its own so
        // the update above decides nothing for it.
        let mfa_pool_id = ok(
            &router,
            "CreateUserPool",
            &json!({ "PoolName": format!("mfa-written-{index}") }),
        )
        .await["UserPool"]["Id"]
            .as_str()
            .expect("a minted pool id")
            .to_string();
        let (status, payload) = call(
            &router,
            "SetUserPoolMfaConfig",
            &json!({
                "UserPoolId": mfa_pool_id,
                "MfaConfiguration": "OPTIONAL",
                "SmsMfaConfiguration": { "SmsConfiguration": sms_configuration },
            }),
        )
        .await;
        assert_answer(case, "SetUserPoolMfaConfig", expected, status, &payload);
        let read_back = ok(
            &router,
            "GetUserPoolMfaConfig",
            &json!({ "UserPoolId": mfa_pool_id }),
        )
        .await;
        let expected_mode = if expected == Expected::Served {
            "OPTIONAL"
        } else {
            "OFF"
        };
        assert_eq!(
            read_back["MfaConfiguration"].as_str(),
            Some(expected_mode),
            "case {case:?}: the pool's MFA mode must be {expected_mode:?}, got {read_back}"
        );
    }
}
