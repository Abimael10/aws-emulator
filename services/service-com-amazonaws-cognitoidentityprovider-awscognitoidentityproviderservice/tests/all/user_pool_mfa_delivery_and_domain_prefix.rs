// SPDX-License-Identifier: CC0-1.0

//! The configuration a user pool and its hosted prefix domain may be left with,
//! over the wire, driven through the router exactly as an SDK drives it (`POST /`
//! with `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`) and read back
//! through the operations that report the stored resource.
//!
//! Two rules are under test:
//!
//! * a pool whose `MfaConfiguration` is not `OFF` carries the delivery path of
//!   the factor this surface configures — "the `CreateUserPool` operation
//!   supports only SMS MFA configuration. If you set `MfaConfiguration` to either
//!   of these values, include an `SmsConfiguration` in the same request … If you
//!   omit `SmsConfiguration`, the operation returns an
//!   `InvalidParameterException`"
//!   (`smithy_query(query="doc",shape="CreateUserPoolRequest",member="MfaConfiguration")`)
//!   — which is an SMS caller to send the message through and the phone number
//!   auto-verified, since a code reaches only a number the pool verifies; the
//!   same question is asked of a `CreateUserPool` and of an `UpdateUserPool`,
//!   over the configuration each would leave the pool with;
//! * an Amazon Cognito PREFIX domain is a label in a namespace Amazon Web
//!   Services owns — "a prefix value of `myprefix` for a user pool in the
//!   `us-east-1` Region results in a domain of
//!   `myprefix.auth.us-east-1.amazoncognito.com`"
//!   (`smithy_query(query="doc",shape="CreateUserPoolDomainRequest",member="Domain")`)
//!   — so a prefix containing one of the words reserved there is refused with a
//!   400 `InvalidParameterException` naming that word.
//!
//! Each table's rows differ along what the rule generalises over rather than on
//! one recorded request: every `UserPoolMfaType` mode against every combination
//! of the two halves of the delivery path (and an SMS caller named through either
//! of the two configurations that name one), and each reserved word in a prefix
//! alone, embedded, repeated and merely resembled. Every refusal is checked to
//! have stored nothing.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The Amazon SNS caller a pool's SMS messages are sent through
/// (`SmsConfigurationType.SnsCallerArn`).
const SNS_CALLER_ARN: &str = "arn:aws:iam::000000000000:role/sms-caller";

/// The Amazon Web Services End User Messaging caller a pool may send through
/// instead: "in a user pool, provide either the Amazon SNS configuration
/// (`SnsCallerArn`) or this configuration, but not both"
/// (`SmsConfigurationType.EumsSms`).
const EUMS_CALLER_ARN: &str = "arn:aws:iam::000000000000:role/eums-caller";

/// The sentence a user-pool write is refused with when it leaves MFA on without
/// the SMS delivery path that mode rests on.
const MFA_SMS_CONFIGURATION_REQUIRED: &str =
    "SMS configuration and Auto verification for phone_number are required when MFA is \
     required/optional";

/// The sentence a user-pool write is refused with when it selects `phone_number`
/// for auto-verification with no SMS delivery configuration at all, which is the
/// refusal a request that asks for both at once meets first.
const SMS_CONFIGURATION_REQUIRED: &str =
    "SMS configuration is required when phone_number is selected for auto verification";

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

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
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

/// How many pools the store holds, as `ListUserPools` reports.
async fn pool_count(router: &MultiServiceRouter) -> usize {
    ok(router, "ListUserPools", &json!({ "MaxResults": 60 })).await["UserPools"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// `members` as a request body for `operation`, with `extra` merged in.
fn body(members: &Value, extra: Vec<(&str, Value)>) -> Value {
    let mut body: Map<String, Value> = members
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("the row's members are a JSON object: {members}"));
    for (member, value) in extra {
        body.insert(member.to_string(), value);
    }
    Value::Object(body)
}

/// The SMS delivery configuration a row carries: an Amazon SNS caller, an End
/// User Messaging caller, or none.
#[derive(Clone, Copy, Debug)]
enum SmsCaller {
    /// No `SmsConfiguration` member at all.
    None,
    /// The Amazon SNS caller role (`SmsConfigurationType.SnsCallerArn`).
    Sns,
    /// The End User Messaging configuration (`SmsConfigurationType.EumsSms`).
    EndUserMessaging,
}

impl SmsCaller {
    /// This caller as the `SmsConfiguration` member of a request, or `None` where
    /// the row names no caller.
    fn member(self) -> Option<Value> {
        match self {
            Self::None => None,
            Self::Sns => Some(json!({ "SnsCallerArn": SNS_CALLER_ARN })),
            Self::EndUserMessaging => Some(json!({
                "EumsSms": { "CallerArn": EUMS_CALLER_ARN, "OriginationIdentity": "+15550100" }
            })),
        }
    }
}

/// One row: the MFA mode the write asks for, which SMS caller it names, whether
/// it auto-verifies the phone number, and the sentence it is refused with — or
/// `None` where the pool takes the configuration.
struct MfaCase {
    case: &'static str,
    mode: &'static str,
    caller: SmsCaller,
    verifies_phone: bool,
    refusal: Option<&'static str>,
}

/// The rows: every `UserPoolMfaType` value
/// (`smithy_query(query="shape",shape="UserPoolMfaType")` — `OFF`, `ON`,
/// `OPTIONAL`) against every combination of the two halves of the SMS delivery
/// path, with the caller named through either of the two configurations that name
/// one.
const MFA_CASES: &[MfaCase] = &[
    MfaCase {
        case: "MFA off with no delivery path at all",
        mode: "OFF",
        caller: SmsCaller::None,
        verifies_phone: false,
        refusal: None,
    },
    MfaCase {
        case: "MFA off with an SMS caller and no phone verification",
        mode: "OFF",
        caller: SmsCaller::Sns,
        verifies_phone: false,
        refusal: None,
    },
    MfaCase {
        case: "MFA off with the whole delivery path",
        mode: "OFF",
        caller: SmsCaller::Sns,
        verifies_phone: true,
        refusal: None,
    },
    MfaCase {
        case: "MFA required with no delivery path at all",
        mode: "ON",
        caller: SmsCaller::None,
        verifies_phone: false,
        refusal: Some(MFA_SMS_CONFIGURATION_REQUIRED),
    },
    MfaCase {
        case: "MFA required with an SMS caller but no phone verification",
        mode: "ON",
        caller: SmsCaller::Sns,
        verifies_phone: false,
        refusal: Some(MFA_SMS_CONFIGURATION_REQUIRED),
    },
    MfaCase {
        // The phone number cannot be auto-verified without a caller to send the
        // verification through, which is the refusal such a request meets first.
        case: "MFA required with phone verification but no caller",
        mode: "ON",
        caller: SmsCaller::None,
        verifies_phone: true,
        refusal: Some(SMS_CONFIGURATION_REQUIRED),
    },
    MfaCase {
        case: "MFA required with the whole delivery path",
        mode: "ON",
        caller: SmsCaller::Sns,
        verifies_phone: true,
        refusal: None,
    },
    MfaCase {
        case: "MFA required with the whole path through End User Messaging",
        mode: "ON",
        caller: SmsCaller::EndUserMessaging,
        verifies_phone: true,
        refusal: None,
    },
    MfaCase {
        case: "MFA optional with no delivery path at all",
        mode: "OPTIONAL",
        caller: SmsCaller::None,
        verifies_phone: false,
        refusal: Some(MFA_SMS_CONFIGURATION_REQUIRED),
    },
    MfaCase {
        case: "MFA optional with an SMS caller but no phone verification",
        mode: "OPTIONAL",
        caller: SmsCaller::Sns,
        verifies_phone: false,
        refusal: Some(MFA_SMS_CONFIGURATION_REQUIRED),
    },
    MfaCase {
        case: "MFA optional with the whole delivery path",
        mode: "OPTIONAL",
        caller: SmsCaller::Sns,
        verifies_phone: true,
        refusal: None,
    },
    MfaCase {
        case: "MFA optional with the whole path through End User Messaging",
        mode: "OPTIONAL",
        caller: SmsCaller::EndUserMessaging,
        verifies_phone: true,
        refusal: None,
    },
];

/// The row's configuration as the members of a user-pool write.
fn mfa_members(case: &MfaCase) -> Value {
    let mut members = Map::new();
    members.insert("MfaConfiguration".to_string(), json!(case.mode));
    if let Some(configuration) = case.caller.member() {
        members.insert("SmsConfiguration".to_string(), configuration);
    }
    if case.verifies_phone {
        members.insert(
            "AutoVerifiedAttributes".to_string(),
            json!(["phone_number"]),
        );
    }
    Value::Object(members)
}

#[tokio::test]
async fn a_pool_left_with_mfa_on_carries_the_sms_delivery_path() {
    let router = router();

    for (index, case) in MFA_CASES.iter().enumerate() {
        let label = case.case;
        let members = mfa_members(case);
        let before = pool_count(&router).await;
        let (status, payload) = call(
            &router,
            "CreateUserPool",
            &body(
                &members,
                vec![("PoolName", json!(format!("mfa-delivery-{index}")))],
            ),
        )
        .await;
        let after = pool_count(&router).await;

        match case.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {label:?}: the pool must take this configuration, got {status} with \
                     {payload}"
                );
                assert_eq!(
                    after,
                    before + 1,
                    "case {label:?}: an accepted create stores one pool, got {after}"
                );
                let user_pool_id = payload["UserPool"]["Id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {label:?}: a minted pool id: {payload}"))
                    .to_string();
                let stored = described(&router, &user_pool_id).await;
                assert_eq!(
                    stored["MfaConfiguration"],
                    json!(case.mode),
                    "case {label:?}: the stored pool must hold the mode the create set, got \
                     {stored}"
                );
            }
            Some(message) => {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, "InvalidParameterException", message),
                    "case {label:?}: expected a 400 InvalidParameterException carrying \
                     {message:?}, got {status} with {payload}"
                );
                assert_eq!(
                    after, before,
                    "case {label:?}: a refused create stores no pool, got {after}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_that_would_leave_mfa_on_carries_the_same_delivery_path() {
    let router = router();

    /// The configuration a pool each row is applied to is created with: MFA on and the
    /// whole delivery path, so a row is not refused for the mode change itself ("cannot
    /// turn MFA functionality ON, once the user pool has been created").
    fn starting_pool(name: String) -> Value {
        json!({
            "PoolName": name,
            "MfaConfiguration": "ON",
            "SmsConfiguration": { "SnsCallerArn": SNS_CALLER_ARN },
            "AutoVerifiedAttributes": ["phone_number"],
        })
    }

    // A refused update leaves the pool exactly as it was, which is what each refused row
    // asserts, so every one of them is applied to this ONE pool; a row the pool TAKES
    // changes the mode, so it gets a pool of its own.
    let refused_against = ok(&router, "CreateUserPool", &starting_pool("mfa-update".to_string()))
        .await["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id"))
        .to_string();

    for (index, case) in MFA_CASES.iter().enumerate() {
        let label = case.case;
        let user_pool_id = match case.refusal {
            Some(_) => refused_against.clone(),
            None => ok(
                &router,
                "CreateUserPool",
                &starting_pool(format!("mfa-update-{index}")),
            )
            .await["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {label:?}: a minted pool id"))
                .to_string(),
        };

        let was = described(&router, &user_pool_id).await;
        let (status, payload) = call(
            &router,
            "UpdateUserPool",
            &body(
                &mfa_members(case),
                vec![("UserPoolId", json!(user_pool_id.clone()))],
            ),
        )
        .await;
        let now = described(&router, &user_pool_id).await;

        match case.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {label:?}: the update must be served, got {status} with {payload}"
                );
                assert_eq!(
                    now["MfaConfiguration"],
                    json!(case.mode),
                    "case {label:?}: the updated pool must hold {:?}, got {now}",
                    case.mode
                );
            }
            Some(message) => {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, "InvalidParameterException", message),
                    "case {label:?}: expected a 400 InvalidParameterException carrying \
                     {message:?}, got {status} with {payload}"
                );
                assert_eq!(
                    now, was,
                    "case {label:?}: a refused update must leave the pool alone, got {now}"
                );
            }
        }
    }
}

/// One domain row: the prefix the request asks for, and the reserved word it
/// contains — `None` for a prefix the service grants.
struct DomainCase {
    case: &'static str,
    domain: &'static str,
    reserved: Option<&'static str>,
}

/// The rows. The reserved words are the ones Amazon Cognito does not grant in the
/// `amazoncognito.com` namespace — `aws`, `amazon` and `cognito` — and the rows
/// span each of them alone, embedded in a longer label, at the front, in the
/// middle and at the end, beside a prefix that merely RESEMBLES one and plain
/// labels that contain none. Every value satisfies `DomainType`'s own pattern
/// (`smithy_query(query="shape",shape="DomainType")`), so what each row measures
/// is the value rule above the schema.
const DOMAIN_CASES: &[DomainCase] = &[
    DomainCase {
        case: "a prefix that is the cloud's own name",
        domain: "aws",
        reserved: Some("aws"),
    },
    DomainCase {
        case: "a prefix opening with the cloud's name",
        domain: "aws-login",
        reserved: Some("aws"),
    },
    DomainCase {
        case: "a prefix ending with the cloud's name",
        domain: "login-aws",
        reserved: Some("aws"),
    },
    DomainCase {
        case: "a prefix carrying the company's name in the middle",
        domain: "my-amazon-portal",
        reserved: Some("amazon"),
    },
    DomainCase {
        case: "a prefix carrying the service's own name",
        domain: "cognito-signin",
        reserved: Some("cognito"),
    },
    DomainCase {
        case: "a prefix carrying the service's name with no separator",
        domain: "mycognitoauth",
        reserved: Some("cognito"),
    },
    DomainCase {
        case: "a prefix carrying a reserved word twice",
        domain: "aws-aws-login",
        reserved: Some("aws"),
    },
    DomainCase {
        case: "a plain prefix",
        domain: "myprefix",
        reserved: None,
    },
    DomainCase {
        case: "a prefix that merely resembles the cloud's name",
        domain: "awesome-login",
        reserved: None,
    },
    DomainCase {
        case: "a prefix that merely resembles the company's name",
        domain: "amazing-portal",
        reserved: None,
    },
    DomainCase {
        case: "a prefix carrying the service's name with a letter missing",
        domain: "congito-signin",
        reserved: None,
    },
];

#[tokio::test]
async fn a_prefix_domain_carries_no_reserved_word() {
    let router = router();

    // A refused create claims no name and leaves the pool holding no prefix domain, which
    // is what each refused row asserts, so every one of them is asked of this ONE pool; a
    // prefix the service GRANTS fills the pool's one prefix-domain slot, so a row the
    // service grants gets a pool of its own.
    let refused_against = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "domain-prefix" }),
    )
    .await["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id"))
        .to_string();

    for (index, case) in DOMAIN_CASES.iter().enumerate() {
        let label = case.case;
        let user_pool_id = match case.reserved {
            Some(_) => refused_against.clone(),
            None => ok(
                &router,
                "CreateUserPool",
                &json!({ "PoolName": format!("domain-prefix-{index}") }),
            )
            .await["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {label:?}: a minted pool id"))
                .to_string(),
        };

        let (status, payload) = call(
            &router,
            "CreateUserPoolDomain",
            &json!({ "UserPoolId": user_pool_id, "Domain": case.domain }),
        )
        .await;
        let (read_status, read) = call(
            &router,
            "DescribeUserPoolDomain",
            &json!({ "Domain": case.domain }),
        )
        .await;

        match case.reserved {
            None => {
                assert_eq!(
                    status, 200,
                    "case {label:?}: the prefix {:?} is one the service grants, got {status} with \
                     {payload}",
                    case.domain
                );
                // The round trip: the granted prefix is the domain a later read
                // resolves to this very pool.
                assert_eq!(
                    (
                        read_status,
                        read["DomainDescription"]["Domain"].clone(),
                        read["DomainDescription"]["UserPoolId"].clone()
                    ),
                    (200, json!(case.domain), json!(user_pool_id.clone())),
                    "case {label:?}: the created domain must read back on this pool, got \
                     {read_status} with {read}"
                );
            }
            Some(reserved) => {
                let message = format!("Domain cannot contain reserved word: {reserved}");
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, "InvalidParameterException", message.as_str()),
                    "case {label:?}: expected a 400 InvalidParameterException carrying \
                     {message:?}, got {status} with {payload}"
                );
                // A refused create claimed nothing: the name resolves to no
                // domain, and the pool holds none.
                assert_eq!(
                    read["DomainDescription"].get("Domain"),
                    None,
                    "case {label:?}: a refused create stores no domain, got {read}"
                );
                let stored = described(&router, &user_pool_id).await;
                assert_eq!(
                    stored.get("Domain"),
                    None,
                    "case {label:?}: the pool must hold no prefix domain, got {stored}"
                );
            }
        }
    }
}
