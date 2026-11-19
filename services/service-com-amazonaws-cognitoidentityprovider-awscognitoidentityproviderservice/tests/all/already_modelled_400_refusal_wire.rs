// SPDX-License-Identifier: CC0-1.0

//! How this endpoint answers a refusal whose error class the model ALREADY annotates with
//! HTTP 400, over the wire (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test is the one [`super::error_wire`] states for this endpoint, asserted
//! here over the refusals where the status was never in question, so the header is the only
//! thing that can be missing: every error response names its error shape in the
//! `x-amzn-errortype` response header, as the shape's simple name followed by a colon. An
//! AWS SDK resolves the concrete exception type from that header FIRST and reads the body's
//! `__type` only when it is absent, and so does everything between a client and this
//! endpoint that classifies a failure without parsing the body — a proxy, a request log, a
//! retry classifier. Without the header those see an unclassified 400, so a refusal a
//! client is meant to branch on arrives as a generic bad request.
//!
//! The rows span what the rule generalises over, so no row is the rule: the error CLASS the
//! refusal is raised with (`InvalidUserPoolConfigurationException`, `AliasExistsException`,
//! `InvalidParameterException`), the OPERATION that raises it (the token-addressed device
//! calls and the administrative ones, the anonymous sign-up confirmation, the attribute
//! verification code, and the pool listing), the LAYER it is raised at (a handler's own
//! modelled error, and the framework's refusal of a value outside a modelled constraint,
//! which no handler sees), and the KIND of refusal (a pool that activated no device
//! remembering, an alias value another user already holds, an attribute no message can be
//! sent to, an attribute the user registered no destination for, a page size outside its
//! modelled range, an absent required member, and a page token nothing minted). Each group
//! carries its own control: the same call, made where the state or the value permits it, is
//! served and names no error shape.
//!
//! Every assertion names its case and reports the value it saw.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state;

use super::error_wire::{call, error_code, ok, ERRORTYPE, REFUSED};

/// A password satisfying a default pool's password policy, so a created user can be given a
/// permanent one and sign in.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// A device key no sign-in minted, in the form `DeviceKeyType`'s pattern demands
/// (`^[\w-]+_[0-9a-f-]+$`, from
/// `smithy_query(query="operation",operation="ForgetDevice")`): a pool that remembers no
/// device is refused on its configuration before any device key is resolved.
const UNMINTED_DEVICE: &str = "us-east-1_00000000-0000-4000-8000-000000000000";

/// The IAM role a pool's `SmsConfiguration` names as the Amazon SNS caller SMS is sent
/// through ("the ARN of the Amazon SNS caller", `SmsConfigurationType.SnsCallerArn`), which
/// a pool that auto-verifies `phone_number` needs.
const SNS_CALLER_ARN: &str = "arn:aws:iam::000000000000:role/sms-caller";

/// The user that holds an alias value until another sign-up claims it.
const FIRST_USER: &str = "already-400-one";

/// The user whose confirmed sign-up claims the alias value the first user holds.
const SECOND_USER: &str = "already-400-two";

/// One emulator with just this service mounted over `engine`, so the sign-up confirmation
/// code — which the real service delivers out of band, so no response carries it — can be
/// read out of this service's own state.
fn router(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(engine)));
    router.set_request_authentication(false);
    router
}

/// The shared engine one test's router is mounted over.
fn engine() -> Arc<engine::InMemoryEngine> {
    Arc::new(engine::InMemoryEngine::new())
}

/// One row: a call this service refuses, and the error class it refuses it with.
struct Refusal {
    case: String,
    operation: &'static str,
    body: Value,
    class: &'static str,
}

/// Assert the rule over `rows`: each is refused with HTTP 400, its body names its error
/// class, and the `x-amzn-errortype` header names that same class followed by a colon.
async fn each_refusal_names_its_error_shape(router: &MultiServiceRouter, rows: Vec<Refusal>) {
    for Refusal {
        case,
        operation,
        body,
        class,
    } in rows
    {
        let answered = call(router, operation, &body).await;
        assert_eq!(
            answered.status, REFUSED,
            "case {case:?}: a refusal of the caller is HTTP {REFUSED}, got {} with {}",
            answered.status, answered.payload
        );
        assert_eq!(
            error_code(&answered.payload),
            class,
            "case {case:?}: expected the body to name {class}, got {}",
            answered.payload
        );
        let expected = format!("{class}:");
        assert_eq!(
            answered.errortype.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {ERRORTYPE} {expected:?}, got {:?}",
            answered.errortype
        );
    }
}

/// Assert the control over `rows`: each `(case, operation, body)` is served, so it keeps its
/// own status and names no error shape.
async fn each_served_call_names_no_error_shape(
    router: &MultiServiceRouter,
    rows: Vec<(String, &'static str, Value)>,
) {
    for (case, operation, body) in rows {
        let answered = call(router, operation, &body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {}",
            answered.status, answered.payload
        );
        assert_eq!(
            answered.errortype, None,
            "case {case:?}: a served call names no error shape, got {:?}",
            answered.errortype
        );
    }
}

/// A pool created with `configuration`, plus one app client that allows the password
/// sign-in these tests drive: the minted pool id and app client id.
async fn pool_with_client(
    router: &MultiServiceRouter,
    configuration: Value,
) -> (String, String) {
    let created = ok(router, "CreateUserPool", &configuration).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id: {created}"))
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": &user_pool_id,
            "ClientName": "already-400-client",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted app client id: {client}"))
        .to_string();
    (user_pool_id, client_id)
}

/// Create `username` with `attributes` and a permanent password, so it is confirmed, then
/// sign it in and return the access token its sign-in issued.
async fn signed_in_token(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
    attributes: Value,
) -> String {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": attributes,
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
    let authenticated = ok(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    authenticated["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in issues an access token: {authenticated}"))
        .to_string()
}

/// The sign-up confirmation code outstanding for `username`, read out of this service's own
/// state: the real service delivers it out of band, so no response carries it.
fn outstanding_code(
    engine: &Arc<engine::InMemoryEngine>,
    user_pool_id: &str,
    username: &str,
) -> String {
    let service_state = state::service_state(&**engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .pool(user_pool_id)
        .and_then(|pool| pool.user(username))
        .and_then(|user| user.outstanding_code(&state::CodePurpose::SignUpConfirmation))
        .map(|code| code.code.clone())
        .unwrap_or_else(|| panic!("a sign-up code must be outstanding for {username:?}"))
}

/// RULE over the DEVICE surface: a pool that provided no `DeviceConfiguration` activated no
/// device remembering, so every device call against it is refused on the pool's
/// configuration — a class the model annotates 400 already — and that refusal names its
/// error shape in the header.
///
/// The rows are one refusal per device operation that resolves a device of a pool, spanning
/// the token-addressed half (addressed by a signed-in user's access token) and the
/// administrative half (addressed by a pool id and a username).
#[tokio::test]
async fn every_device_call_on_a_pool_that_remembers_no_device_names_its_error_shape() {
    let engine = engine();
    let router = router(&engine);
    // No `DeviceConfiguration`: "when you provide a value for any property of
    // DeviceConfiguration, you activate the device remembering for the user pool" (this
    // service's model), so a pool that provides none remembers nothing.
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        json!({ "PoolName": "already-400-no-device-remembering" }),
    )
    .await;
    let token = signed_in_token(&router, &user_pool_id, &client_id, "alice", json!([])).await;

    let class = "InvalidUserPoolConfigurationException";
    let rows = vec![
        Refusal {
            case: "ForgetDevice through the signed-in user's own token".to_string(),
            operation: "ForgetDevice",
            body: json!({ "AccessToken": &token, "DeviceKey": UNMINTED_DEVICE }),
            class,
        },
        Refusal {
            case: "UpdateDeviceStatus through the signed-in user's own token".to_string(),
            operation: "UpdateDeviceStatus",
            body: json!({
                "AccessToken": &token,
                "DeviceKey": UNMINTED_DEVICE,
                "DeviceRememberedStatus": "remembered",
            }),
            class,
        },
        Refusal {
            case: "GetDevice through the signed-in user's own token".to_string(),
            operation: "GetDevice",
            body: json!({ "AccessToken": &token, "DeviceKey": UNMINTED_DEVICE }),
            class,
        },
        Refusal {
            case: "AdminForgetDevice addressed by pool id and username".to_string(),
            operation: "AdminForgetDevice",
            body: json!({
                "UserPoolId": &user_pool_id,
                "Username": "alice",
                "DeviceKey": UNMINTED_DEVICE,
            }),
            class,
        },
        Refusal {
            case: "AdminUpdateDeviceStatus addressed by pool id and username".to_string(),
            operation: "AdminUpdateDeviceStatus",
            body: json!({
                "UserPoolId": &user_pool_id,
                "Username": "alice",
                "DeviceKey": UNMINTED_DEVICE,
                "DeviceRememberedStatus": "not_remembered",
            }),
            class,
        },
        Refusal {
            case: "AdminGetDevice addressed by pool id and username".to_string(),
            operation: "AdminGetDevice",
            body: json!({
                "UserPoolId": &user_pool_id,
                "Username": "alice",
                "DeviceKey": UNMINTED_DEVICE,
            }),
            class,
        },
        Refusal {
            case: "AdminListDevices addressed by pool id and username".to_string(),
            operation: "AdminListDevices",
            body: json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
            class,
        },
    ];

    each_refusal_names_its_error_shape(&router, rows).await;
}

/// The control for the device group: the same device calls against a pool that DID activate
/// device remembering are served for the device its own sign-in minted and confirmed, so
/// the refusals above are computed from the pool's configuration rather than answered to
/// every device call.
#[tokio::test]
async fn the_same_device_calls_are_served_by_a_pool_that_remembers_devices() {
    let engine = engine();
    let router = router(&engine);
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        json!({
            "PoolName": "already-400-device-remembering",
            "DeviceConfiguration": {
                "ChallengeRequiredOnNewDevice": true,
                "DeviceOnlyRememberedOnUserPrompt": false,
            },
        }),
    )
    .await;
    ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
    )
    .await;
    ok(
        &router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": &user_pool_id,
            "Username": "alice",
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let authenticated = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": &client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": "alice", "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    let token = authenticated["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in issues an access token: {authenticated}"))
        .to_string();
    let device = authenticated["AuthenticationResult"]["NewDeviceMetadata"]["DeviceKey"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("a sign-in against such a pool mints a device key: {authenticated}")
        })
        .to_string();
    // The device credentials a confirm REGISTERS: "a Secure Remote Password (SRP) value
    // that your application generates when you register a user's device"
    // (`smithy_query(query="shape",shape="DeviceSecretVerifierConfigType")`).
    ok(
        &router,
        "ConfirmDevice",
        &json!({
            "AccessToken": &token,
            "DeviceKey": &device,
            "DeviceName": "a-laptop",
            "DeviceSecretVerifierConfig": {
                "PasswordVerifier": "ZGV2aWNlLXBhc3N3b3JkLXZlcmlmaWVy",
                "Salt": "ZGV2aWNlLXNhbHQ",
            },
        }),
    )
    .await;

    let rows = vec![
        (
            "the remembered device read through its own token".to_string(),
            "GetDevice",
            json!({ "AccessToken": &token, "DeviceKey": &device }),
        ),
        (
            "the remembered device read administratively".to_string(),
            "AdminGetDevice",
            json!({
                "UserPoolId": &user_pool_id,
                "Username": "alice",
                "DeviceKey": &device,
            }),
        ),
        (
            "the user's devices listed administratively".to_string(),
            "AdminListDevices",
            json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
        ),
        (
            "the device's remembered status set administratively".to_string(),
            "AdminUpdateDeviceStatus",
            json!({
                "UserPoolId": &user_pool_id,
                "Username": "alice",
                "DeviceKey": &device,
                "DeviceRememberedStatus": "not_remembered",
            }),
        ),
        (
            "the device's remembered status set through its own token".to_string(),
            "UpdateDeviceStatus",
            json!({
                "AccessToken": &token,
                "DeviceKey": &device,
                "DeviceRememberedStatus": "remembered",
            }),
        ),
        (
            "the device forgotten through its own token".to_string(),
            "ForgetDevice",
            json!({ "AccessToken": &token, "DeviceKey": &device }),
        ),
    ];

    each_served_call_names_no_error_shape(&router, rows).await;
}

/// RULE over the anonymous SIGN-UP CONFIRMATION: a confirmation that would claim an alias
/// value another user of the pool already holds, without asking for the migration, is
/// refused with `AliasExistsException` ("if `ForceAliasCreation` is `false`, the API throws
/// an `AliasExistsException` error", `ConfirmSignUp.ForceAliasCreation`) — a class the model
/// annotates 400 already — and that refusal names its error shape in the header. Asking for
/// the migration is the control: the same confirmation is then served and names no error
/// shape.
///
/// The rows differ along which alias attribute of the pool carries the shared value, which
/// is what the verification message is addressed to: the two members of
/// `AliasAttributeType` that carry a verified flag
/// (`smithy_query(query="shape",shape="AliasAttributeType")`).
#[tokio::test]
async fn a_duplicate_alias_confirmation_names_its_error_shape() {
    // (case, alias attribute, the shared value)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("an email alias", "email", "already-400@example.com"),
        ("a phone-number alias", "phone_number", "+15550700800"),
    ];

    for (case, attribute, value) in rows {
        let engine = engine();
        let router = router(&engine);
        let mut configuration = serde_json::Map::new();
        configuration.insert(
            "PoolName".to_string(),
            json!(format!("already-400-alias-{attribute}")),
        );
        configuration.insert("AliasAttributes".to_string(), json!([attribute]));
        configuration.insert("AutoVerifiedAttributes".to_string(), json!([attribute]));
        // A pool that auto-verifies `phone_number` sends that verification over SMS, so it
        // needs the Amazon SNS caller SMS is sent through.
        if attribute == "phone_number" {
            configuration.insert(
                "SmsConfiguration".to_string(),
                json!({ "SnsCallerArn": SNS_CALLER_ARN }),
            );
        }
        let (user_pool_id, client_id) =
            pool_with_client(&router, Value::Object(configuration)).await;

        // The first user signs up with the value and confirms it, which is what makes the
        // value its alias.
        let sign_up = |username: &'static str| {
            json!({
                "ClientId": &client_id,
                "Username": username,
                "Password": GOOD_PASSWORD,
                "UserAttributes": [{ "Name": attribute, "Value": value }],
            })
        };
        ok(&router, "SignUp", &sign_up(FIRST_USER)).await;
        let first_code = outstanding_code(&engine, &user_pool_id, FIRST_USER);
        ok(
            &router,
            "ConfirmSignUp",
            &json!({
                "ClientId": &client_id,
                "Username": FIRST_USER,
                "ConfirmationCode": &first_code,
            }),
        )
        .await;

        // A second sign-up for the SAME value claims nothing yet: the claim is the
        // confirmation, and without the migration it is refused.
        ok(&router, "SignUp", &sign_up(SECOND_USER)).await;
        let second_code = outstanding_code(&engine, &user_pool_id, SECOND_USER);
        each_refusal_names_its_error_shape(
            &router,
            vec![Refusal {
                case: format!("{case}, confirmed without asking for the migration"),
                operation: "ConfirmSignUp",
                body: json!({
                    "ClientId": &client_id,
                    "Username": SECOND_USER,
                    "ConfirmationCode": &second_code,
                    "ForceAliasCreation": false,
                }),
                class: "AliasExistsException",
            }],
        )
        .await;

        // The control: the same confirmation, asking for the migration, is served.
        each_served_call_names_no_error_shape(
            &router,
            vec![(
                format!("{case}, confirmed asking for the migration"),
                "ConfirmSignUp",
                json!({
                    "ClientId": &client_id,
                    "Username": SECOND_USER,
                    "ConfirmationCode": &second_code,
                    "ForceAliasCreation": true,
                }),
            )],
        )
        .await;
    }
}

/// RULE over the ATTRIBUTE VERIFICATION CODE: a code is a message, so the attribute it is
/// asked for has to be an address this pool can send to and the user has to hold a value
/// for it; neither is a bad request the model can express, so each is refused with
/// `InvalidParameterException` — a class the model annotates 400 already — and each refusal
/// names its error shape in the header.
///
/// The rows differ along WHY no code can be sent (an attribute no message is addressed to,
/// a custom attribute of the pool's own schema, and an address attribute the user holds no
/// value for) and so along the attribute asked for; the control is the one attribute the
/// user does hold a value for.
#[tokio::test]
async fn an_unsendable_attribute_verification_code_names_its_error_shape() {
    let engine = engine();
    let router = router(&engine);
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        json!({
            "PoolName": "already-400-verification-code",
            "Schema": [{ "Name": "department", "AttributeDataType": "String" }],
        }),
    )
    .await;
    // The user holds an email and no phone number, so `email` is the one attribute a code
    // can be sent to.
    let token = signed_in_token(
        &router,
        &user_pool_id,
        &client_id,
        "alice",
        json!([{ "Name": "email", "Value": "already-400-alice@example.com" }]),
    )
    .await;

    let class = "InvalidParameterException";
    let rows = vec![
        Refusal {
            case: "a standard attribute no message is addressed to".to_string(),
            operation: "GetUserAttributeVerificationCode",
            body: json!({ "AccessToken": &token, "AttributeName": "given_name" }),
            class,
        },
        Refusal {
            case: "a custom attribute of the pool's own schema".to_string(),
            operation: "GetUserAttributeVerificationCode",
            body: json!({ "AccessToken": &token, "AttributeName": "custom:department" }),
            class,
        },
        Refusal {
            case: "an address attribute the user holds no value for".to_string(),
            operation: "GetUserAttributeVerificationCode",
            body: json!({ "AccessToken": &token, "AttributeName": "phone_number" }),
            class,
        },
    ];

    each_refusal_names_its_error_shape(&router, rows).await;

    each_served_call_names_no_error_shape(
        &router,
        vec![(
            "the address attribute the user does hold a value for".to_string(),
            "GetUserAttributeVerificationCode",
            json!({ "AccessToken": &token, "AttributeName": "email" }),
        )],
    )
    .await;
}

/// RULE over the POOL LISTING: a request this listing cannot serve is refused with
/// `InvalidParameterException` — a class the model annotates 400 already — and that refusal
/// names its error shape in the header whether the framework raised it from a modelled
/// constraint or the handler raised it itself.
///
/// `MaxResults` is required and ranges over 1..60 and `NextToken` is a page token this
/// service minted (`smithy_query(query="operation",operation="ListUserPools")`), so the
/// rows are: each published bound, one past each bound, the member absent, and a token
/// nothing minted. The control is the listing at its published maximum.
#[tokio::test]
async fn every_unservable_pool_listing_names_its_error_shape() {
    let engine = engine();
    let router = router(&engine);
    ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "already-400-listed" }),
    )
    .await;

    let class = "InvalidParameterException";
    let rows = vec![
        Refusal {
            case: "a page size one below the modelled minimum".to_string(),
            operation: "ListUserPools",
            body: json!({ "MaxResults": 0 }),
            class,
        },
        Refusal {
            case: "a page size one past the modelled maximum".to_string(),
            operation: "ListUserPools",
            body: json!({ "MaxResults": 61 }),
            class,
        },
        Refusal {
            case: "no page size at all, which the model requires".to_string(),
            operation: "ListUserPools",
            body: json!({}),
            class,
        },
        Refusal {
            case: "a page token nothing minted".to_string(),
            operation: "ListUserPools",
            body: json!({ "MaxResults": 10, "NextToken": "not-a-token" }),
            class,
        },
    ];

    each_refusal_names_its_error_shape(&router, rows).await;

    each_served_call_names_no_error_shape(
        &router,
        vec![
            (
                "the listing at its published minimum page size".to_string(),
                "ListUserPools",
                json!({ "MaxResults": 1 }),
            ),
            (
                "the listing at its published maximum page size".to_string(),
                "ListUserPools",
                json!({ "MaxResults": 60 }),
            ),
        ],
    )
    .await;
}
