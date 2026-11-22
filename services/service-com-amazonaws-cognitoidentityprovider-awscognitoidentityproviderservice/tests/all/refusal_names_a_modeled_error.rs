// SPDX-License-Identifier: CC0-1.0

//! What a request this service does NOT serve is answered with, over the awsJson1_1 wire
//! (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! RULE: every refusal names an exception the REFUSED operation's own model declares —
//! the AWS-common `AccessDeniedException` for a caller whose identity-based IAM policies
//! do not allow that operation's action on the resource the request addresses, and that
//! operation's own modelled precondition exception for a request the stored state forbids
//! — carrying the sentence AWS words that refusal with. No refusal is reported as a
//! server-side gap and none is reported as an empty success, so a client can tell "you may
//! not read this pool", "this pool is not configured for this surface" and "this token
//! names nobody" apart from one another, and from "there is nothing here".
//!
//! The rows span what the rule generalises over, so no row is the rule: the PRECONDITION
//! that refuses (a caller outside its IAM resource scope, an access token that names no
//! signed-in user, a pool that activated no threat protection, a pool that remembers no
//! device, a user holding no destination an MFA factor is delivered to), the OPERATION it
//! refuses (the administrative half and the access-token half of each of those surfaces),
//! and the VALUE presented (a token nothing issued, the identity and refresh tokens of a
//! live sign-in, each of the three deliverable MFA factors). Every group carries its
//! CONTROL: the same call, against a pool or a user whose state permits it, is served or
//! refused for a LATER reason, so no row's refusal is the operation refusing everything.
//!
//! A last rule of the same surface, where the answer is a CLAIM rather than an error: the
//! roles a user's groups grant reach the identity token of a SELF-SERVICE sign-in, and the
//! `cognito:preferred_role` claim is withheld exactly when the groups tied on the best
//! `Precedence` name different roles — answered by the sign-in itself, so a client reads
//! the tie off the first response rather than waiting for one.
//!
//! The IAM principals of the first test are made with IAM's own operations, through the
//! harness [`super::iam_authorization`] drives its own rows with; the identity token's
//! claims are read with [`super::group_role_claims::claims_of`]. Every assertion names its
//! case and reports the value it saw.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use super::group_role_claims::claims_of;
use super::iam_authorization::{
    action_of, allow, code_of, message_of, Emulator, ACCESS_DENIED, ACCOUNT_KEY,
};

/// A request's SigV4 credential scope is what routes an administrative call to this
/// service. The signature is never checked here: the suite turns request authentication
/// off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// This service's regional endpoint host, which a signed call is addressed to.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The emulator's own address: an unsigned request addressed to it names no service in its
/// `Host` header, which is how a signed-in user's client calls the access-token surface.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The response header an AWS SDK resolves the concrete exception type from.
const ERRORTYPE: &str = "x-amzn-errortype";

/// The status this endpoint answers every refusal of the caller under.
const REFUSED: u16 = 400;

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// A value that is no token this service ever issued, in the form `TokenModelType`'s
/// pattern admits (`^[A-Za-z0-9-_=.]+$`, from
/// `smithy_query(query="operation",operation="ListWebAuthnCredentials")`).
const UNISSUED_TOKEN: &str = "not-a-token-at-all";

/// A device key nothing minted, in the form `DeviceKeyType`'s pattern demands
/// (`^[\w-]+_[0-9a-f-]+$`, from
/// `smithy_query(query="operation",operation="AdminGetDevice")`).
const UNMINTED_DEVICE: &str = "us-east-1_00000000-0000-4000-8000-000000000000";

/// An `EventId` no sign-in recorded, in the form `EventIdType`'s pattern demands
/// (`^[\w+-]+$`, from
/// `smithy_query(query="operation",operation="AdminUpdateAuthEventFeedback")`).
const UNRECORDED_EVENT: &str = "unrecorded-auth-event";

/// What a refusal of an `AccessToken` that resolves to no signed-in user reports.
const INVALID_ACCESS_TOKEN: &str = "Invalid Access Token";

/// What a threat-protection call on a pool that activated no add-ons reports: the state of
/// the pool and the member to change to leave it.
const ADD_ONS_NOT_ENABLED: &str = "This is an add on feature. Please update \
     AdvancedSecurityMode for your user pool to access this API.";

/// What a device call on a pool that stores no `DeviceConfiguration` reports.
const DEVICE_TRACKING_OFF: &str = "Device tracking not currently enabled for this pool.";

/// What a `FeedbackToken` that authorizes feedback on no event of the user it names is
/// refused with, on a pool whose add-ons ARE on.
const INVALID_FEEDBACK_SESSION: &str = "Invalid session provided";

/// What an `EventId` that names no event of the user's history is refused with.
const INVALID_EVENT_ID: &str = "EventId is invalid";

/// What a `DeviceKey` the user holds no device for is refused with, on a pool that DOES
/// remember devices.
const DEVICE_NOT_FOUND: &str = "Device does not exist.";

/// The three `UserMFASettingList` settings a user's MFA preference is held as
/// (`smithy_query(query="doc",shape="GetUserResponse",member="UserMFASettingList")`), each
/// delivered to the user by something the user has to hold first.
const SMS_MFA: &str = "SMS_MFA";
const EMAIL_OTP_MFA: &str = "EMAIL_OTP";
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// What activating the MFA setting `setting` for a user holding no delivery configuration
/// for it is refused with.
fn no_delivery_config(setting: &str) -> String {
    format!("User does not have delivery config set to turn on {setting}")
}

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// What one call answered: its status, its `x-amzn-errortype` header and its JSON body.
struct Answered {
    status: u16,
    errortype: Option<String>,
    payload: Value,
}

/// Dispatch `operation` with `body`. A `signed` call carries a credential scope, as an SDK
/// sends every administrative operation; an unsigned one is addressed to the emulator's own
/// address, which is how a signed-in user's client calls the operations the model marks
/// `@optionalAuth`.
async fn dispatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    signed: bool,
) -> Answered {
    let host = if signed { ENDPOINT_HOST } else { EMULATOR_HOST };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        );
    if signed {
        request = request.header("authorization", AUTHORIZATION);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let errortype = response
        .headers()
        .get(ERRORTYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
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
    Answered {
        status,
        errortype,
        payload,
    }
}

/// Call the signed `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = dispatch(router, operation, body, true).await;
    assert_eq!(
        answered.status, 200,
        "{operation} must succeed, got {} with {}",
        answered.status, answered.payload
    );
    answered.payload
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

/// Create a pool named `name` carrying `members` beside its name, and answer its id.
async fn create_pool(router: &MultiServiceRouter, name: &str, members: Value) -> String {
    let mut body = json!({ "PoolName": name });
    if let Some(extra) = members.as_object() {
        let target = body.as_object_mut().expect("the create body is an object");
        for (member, value) in extra {
            target.insert(member.clone(), value.clone());
        }
    }
    let created = ok(router, "CreateUserPool", &body).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Id, got {created}"))
        .to_string()
}

/// Create one app client of `user_pool_id` that admits password sign-in, and answer its id.
async fn create_client(router: &MultiServiceRouter, user_pool_id: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{name}-client"),
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient answers a ClientId, got {created}"))
        .to_string()
}

/// Create `username` in `user_pool_id` holding `attributes`, with a permanent password so
/// it can sign in.
async fn create_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
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

/// Sign `username` in through the self-service `InitiateAuth` and answer that sign-in's
/// `AuthenticationResult`.
async fn sign_in(router: &MultiServiceRouter, client_id: &str, username: &str) -> Value {
    let answered = dispatch(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
        }),
        false,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "signing {username} in must succeed, got {} with {}",
        answered.status, answered.payload
    );
    answered.payload["AuthenticationResult"].clone()
}

/// A pool with one app client and one signed-in user: the state the refusal rows below are
/// driven against, and the tokens of a LIVE sign-in to present beside the values that name
/// nobody.
struct Fixture {
    user_pool_id: String,
    username: &'static str,
    access_token: String,
    id_token: String,
    refresh_token: String,
}

/// Build [`Fixture`] over a pool carrying `pool_members` and a user holding `attributes`.
async fn fixture(
    router: &MultiServiceRouter,
    name: &str,
    pool_members: Value,
    username: &'static str,
    attributes: &[(&str, &str)],
) -> Fixture {
    let user_pool_id = create_pool(router, name, pool_members).await;
    let client_id = create_client(router, &user_pool_id, name).await;
    create_user(router, &user_pool_id, username, attributes).await;
    let result = sign_in(router, &client_id, username).await;
    Fixture {
        user_pool_id,
        username,
        access_token: result["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in answers an AccessToken, got {result}"))
            .to_string(),
        id_token: result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in answers an IdToken, got {result}"))
            .to_string(),
        refresh_token: result["RefreshToken"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in answers a RefreshToken, got {result}"))
            .to_string(),
    }
}

/// One refused call: the case it stands for, the call itself, and the error the refusal has
/// to carry.
struct Refusal {
    case: String,
    operation: &'static str,
    body: Value,
    /// Whether the call carries a credential scope (the administrative half) or is
    /// addressed to the emulator's own address (the access-token half).
    signed: bool,
    class: &'static str,
    message: String,
}

/// The access-token operations one presented value is driven over: a passkey listing, an
/// MFA preference write, a profile read and a device listing — two reads and two writes,
/// each naming its user by the token alone.
fn access_token_calls(token: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("ListWebAuthnCredentials", json!({ "AccessToken": token })),
        (
            "SetUserMFAPreference",
            json!({
                "AccessToken": token,
                "SoftwareTokenMfaSettings": { "Enabled": false, "PreferredMfa": false },
            }),
        ),
        ("GetUser", json!({ "AccessToken": token })),
        ("ListDevices", json!({ "AccessToken": token })),
    ]
}

/// RULE: a request the stored state forbids is refused with an exception the refused
/// operation's own model declares, carrying the sentence AWS words that refusal with — over
/// the preconditions, the operations and the presented values of this surface.
#[tokio::test]
async fn every_refused_precondition_answers_its_operations_own_modeled_error() {
    let router = router();
    // A pool that activated nothing beyond its defaults: no threat-protection add-ons and
    // no `DeviceConfiguration`, holding a user with no verified destination and no
    // registered authenticator.
    let plain = fixture(&router, "modeled-refusal", json!({}), "ada", &[]).await;
    let user_pool_id = plain.user_pool_id.as_str();
    let username = plain.username;

    let mut rows: Vec<Refusal> = Vec::new();

    // An `AccessToken` that names no signed-in user, over the values a client can present
    // in its place: a value nothing issued, and the two OTHER tokens of a live sign-in,
    // which are real tokens that authorize nothing here.
    for (presented, token) in [
        ("a value nothing issued", UNISSUED_TOKEN.to_string()),
        ("the identity token of a live sign-in", plain.id_token.clone()),
        (
            "the refresh token of the same sign-in",
            plain.refresh_token.clone(),
        ),
    ] {
        for (operation, body) in access_token_calls(&token) {
            rows.push(Refusal {
                case: format!("{operation} presented {presented}"),
                operation,
                body,
                signed: false,
                class: "NotAuthorizedException",
                message: INVALID_ACCESS_TOKEN.to_string(),
            });
        }
    }

    // A pool that runs no threat protection logged no authentication event, so every call
    // of that surface is refused on the POOL — the self-service feedback submission, the
    // administrative one, and the history listing.
    for (operation, body, signed) in [
        (
            "UpdateAuthEventFeedback",
            json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "EventId": UNRECORDED_EVENT,
                "FeedbackToken": UNISSUED_TOKEN,
                "FeedbackValue": "Valid",
            }),
            false,
        ),
        (
            "AdminUpdateAuthEventFeedback",
            json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "EventId": UNRECORDED_EVENT,
                "FeedbackValue": "Invalid",
            }),
            true,
        ),
        (
            "AdminListUserAuthEvents",
            json!({ "UserPoolId": user_pool_id, "Username": username }),
            true,
        ),
    ] {
        rows.push(Refusal {
            case: format!("{operation} on a pool that activated no threat protection"),
            operation,
            body,
            signed,
            class: "UserPoolAddOnNotEnabledException",
            message: ADD_ONS_NOT_ENABLED.to_string(),
        });
    }

    // A pool that stores no `DeviceConfiguration` remembers no device, so every device call
    // addressed to it is refused on the pool's configuration — before the device key, and
    // before the user, is resolved.
    for (operation, body, signed) in [
        (
            "AdminListDevices",
            json!({ "UserPoolId": user_pool_id, "Username": username }),
            true,
        ),
        (
            "AdminGetDevice",
            json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "DeviceKey": UNMINTED_DEVICE,
            }),
            true,
        ),
        (
            "AdminForgetDevice",
            json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "DeviceKey": UNMINTED_DEVICE,
            }),
            true,
        ),
        (
            "AdminUpdateDeviceStatus",
            json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "DeviceKey": UNMINTED_DEVICE,
                "DeviceRememberedStatus": "remembered",
            }),
            true,
        ),
        (
            "GetDevice",
            json!({ "AccessToken": &plain.access_token, "DeviceKey": UNMINTED_DEVICE }),
            false,
        ),
        (
            "ForgetDevice",
            json!({ "AccessToken": &plain.access_token, "DeviceKey": UNMINTED_DEVICE }),
            false,
        ),
    ] {
        rows.push(Refusal {
            case: format!("{operation} on a pool that remembers no device"),
            operation,
            body,
            signed,
            class: "InvalidUserPoolConfigurationException",
            message: DEVICE_TRACKING_OFF.to_string(),
        });
    }

    // Activating an MFA factor needs the delivery configuration that factor reaches the
    // user through, so each of the three settings is refused for the user that holds none
    // — over both halves of the preference surface, the token-addressed one and the
    // administrative one.
    for (setting, settings_member) in [
        (SMS_MFA, "SMSMfaSettings"),
        (EMAIL_OTP_MFA, "EmailMfaSettings"),
        (SOFTWARE_TOKEN_MFA, "SoftwareTokenMfaSettings"),
    ] {
        let settings = json!({ "Enabled": true, "PreferredMfa": true });
        rows.push(Refusal {
            case: format!("SetUserMFAPreference activating {setting} for a user holding none"),
            operation: "SetUserMFAPreference",
            body: json!({
                "AccessToken": &plain.access_token,
                settings_member: settings.clone(),
            }),
            signed: false,
            class: "InvalidParameterException",
            message: no_delivery_config(setting),
        });
        rows.push(Refusal {
            case: format!("AdminSetUserMFAPreference activating {setting} for a user holding none"),
            operation: "AdminSetUserMFAPreference",
            body: json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                settings_member: settings,
            }),
            signed: true,
            class: "InvalidParameterException",
            message: no_delivery_config(setting),
        });
    }

    for Refusal {
        case,
        operation,
        body,
        signed,
        class,
        message,
    } in rows
    {
        let answered = dispatch(&router, operation, &body, signed).await;
        assert_eq!(
            (answered.status, error_code(&answered.payload)),
            (REFUSED, class),
            "case {case:?}: expected a {REFUSED} {class}, got {} with {}",
            answered.status,
            answered.payload
        );
        assert_eq!(
            error_message(&answered.payload),
            message,
            "case {case:?}: expected {message:?}, got {}",
            answered.payload
        );
        let expected_header = format!("{class}:");
        assert_eq!(
            answered.errortype.as_deref(),
            Some(expected_header.as_str()),
            "case {case:?}: expected {ERRORTYPE} {expected_header:?}, got {:?}",
            answered.errortype
        );
    }
}

/// RULE (the other side of the same one): each of those refusals is the POOL's or the
/// USER's own state, so the same call is served — or refused for a later reason — where the
/// state permits it, and a refused preference leaves the user exactly as it was.
#[tokio::test]
async fn the_same_call_is_served_where_the_stored_state_permits_it() {
    let router = router();

    // The controls of the access-token group: the LIVE access token of the same sign-in,
    // over the same operations.
    let plain = fixture(&router, "modeled-control", json!({}), "ada", &[]).await;
    let passkeys = dispatch(
        &router,
        "ListWebAuthnCredentials",
        &json!({ "AccessToken": &plain.access_token }),
        false,
    )
    .await;
    assert_eq!(
        (passkeys.status, &passkeys.payload["Credentials"]),
        (200, &json!([])),
        "case \"ListWebAuthnCredentials on a live access token\": expected a 200 naming an \
         empty listing, got {} with {}",
        passkeys.status,
        passkeys.payload
    );
    let profile = dispatch(
        &router,
        "GetUser",
        &json!({ "AccessToken": &plain.access_token }),
        false,
    )
    .await;
    assert_eq!(
        (profile.status, profile.payload["Username"].as_str()),
        (200, Some(plain.username)),
        "case \"GetUser on a live access token\": expected a 200 naming {:?}, got {} with {}",
        plain.username,
        profile.status,
        profile.payload
    );
    let devices = dispatch(
        &router,
        "ListDevices",
        &json!({ "AccessToken": &plain.access_token }),
        false,
    )
    .await;
    assert_eq!(
        (devices.status, &devices.payload["Devices"]),
        (200, &json!([])),
        "case \"ListDevices on a live access token\": expected a 200 naming an empty listing, \
         got {} with {}",
        devices.status,
        devices.payload
    );

    // The pool that DOES run threat protection: the same three calls get past the add-on
    // precondition and are judged on what they asked for.
    let guarded = fixture(
        &router,
        "modeled-guarded",
        json!({ "UserPoolAddOns": { "AdvancedSecurityMode": "AUDIT" } }),
        "grace",
        &[],
    )
    .await;
    let feedback = dispatch(
        &router,
        "UpdateAuthEventFeedback",
        &json!({
            "UserPoolId": &guarded.user_pool_id,
            "Username": guarded.username,
            "EventId": UNRECORDED_EVENT,
            "FeedbackToken": UNISSUED_TOKEN,
            "FeedbackValue": "Valid",
        }),
        false,
    )
    .await;
    assert_eq!(
        (
            feedback.status,
            error_code(&feedback.payload),
            error_message(&feedback.payload)
        ),
        (REFUSED, "NotAuthorizedException", INVALID_FEEDBACK_SESSION),
        "case \"UpdateAuthEventFeedback on a pool with threat protection on\": expected the \
         refusal of the presented session, got {} with {}",
        feedback.status,
        feedback.payload
    );
    let admin_feedback = dispatch(
        &router,
        "AdminUpdateAuthEventFeedback",
        &json!({
            "UserPoolId": &guarded.user_pool_id,
            "Username": guarded.username,
            "EventId": UNRECORDED_EVENT,
            "FeedbackValue": "Invalid",
        }),
        true,
    )
    .await;
    assert_eq!(
        (
            admin_feedback.status,
            error_code(&admin_feedback.payload),
            error_message(&admin_feedback.payload)
        ),
        (REFUSED, "InvalidParameterException", INVALID_EVENT_ID),
        "case \"AdminUpdateAuthEventFeedback on a pool with threat protection on\": expected \
         the refusal of the named event, got {} with {}",
        admin_feedback.status,
        admin_feedback.payload
    );
    let history = dispatch(
        &router,
        "AdminListUserAuthEvents",
        &json!({ "UserPoolId": &guarded.user_pool_id, "Username": guarded.username }),
        true,
    )
    .await;
    assert_eq!(
        history.status, 200,
        "case \"AdminListUserAuthEvents on a pool with threat protection on\": expected a \
         200, got {} with {}",
        history.status, history.payload
    );

    // The pool that DOES remember devices: the listing is served and a key it holds no
    // device for is refused as the absent device rather than as the pool's configuration.
    let remembering = fixture(
        &router,
        "modeled-remembering",
        json!({
            "DeviceConfiguration": {
                "ChallengeRequiredOnNewDevice": true,
                "DeviceOnlyRememberedOnUserPrompt": false,
            },
        }),
        "erin",
        &[],
    )
    .await;
    let listed = dispatch(
        &router,
        "AdminListDevices",
        &json!({ "UserPoolId": &remembering.user_pool_id, "Username": remembering.username }),
        true,
    )
    .await;
    assert_eq!(
        (listed.status, &listed.payload["Devices"]),
        (200, &json!([])),
        "case \"AdminListDevices on a pool that remembers devices\": expected a 200 naming an \
         empty listing, got {} with {}",
        listed.status,
        listed.payload
    );
    let absent = dispatch(
        &router,
        "AdminGetDevice",
        &json!({
            "UserPoolId": &remembering.user_pool_id,
            "Username": remembering.username,
            "DeviceKey": UNMINTED_DEVICE,
        }),
        true,
    )
    .await;
    assert_eq!(
        (
            absent.status,
            error_code(&absent.payload),
            error_message(&absent.payload)
        ),
        (REFUSED, "ResourceNotFoundException", DEVICE_NOT_FOUND),
        "case \"AdminGetDevice for a key the user holds no device for\": expected the refusal \
         of the device, got {} with {}",
        absent.status,
        absent.payload
    );

    // The user that CAN receive an SMS message: the same setting the user without a
    // verified phone number was refused is stored, and read back through `GetUser`.
    let reachable = fixture(
        &router,
        "modeled-reachable",
        json!({}),
        "frida",
        &[
            ("phone_number", "+15550100001"),
            ("phone_number_verified", "true"),
        ],
    )
    .await;
    let activated = dispatch(
        &router,
        "SetUserMFAPreference",
        &json!({
            "AccessToken": &reachable.access_token,
            "SMSMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
        false,
    )
    .await;
    assert_eq!(
        activated.status, 200,
        "case \"SetUserMFAPreference activating SMS_MFA for a user with a verified phone \
         number\": expected a 200, got {} with {}",
        activated.status, activated.payload
    );
    let stored = dispatch(
        &router,
        "GetUser",
        &json!({ "AccessToken": &reachable.access_token }),
        false,
    )
    .await;
    assert_eq!(
        (
            stored.payload["UserMFASettingList"].clone(),
            stored.payload["PreferredMfaSetting"].clone()
        ),
        (json!([SMS_MFA]), json!(SMS_MFA)),
        "case \"the preference a served request stored\": expected {SMS_MFA} activated and \
         preferred, got {}",
        stored.payload
    );

    // And the refused requests of the first table changed nothing: the user that holds no
    // destination still holds no setting and no preference.
    let unchanged = dispatch(
        &router,
        "GetUser",
        &json!({ "AccessToken": &plain.access_token }),
        false,
    )
    .await;
    for member in ["UserMFASettingList", "PreferredMfaSetting"] {
        let reported = unchanged.payload.get(member).cloned();
        assert!(
            reported.is_none_or(|value| value == json!([]) || value.is_null()),
            "case \"a refused preference stores nothing\": expected no {member}, got {}",
            unchanged.payload
        );
    }
}

/// One group a row of the claim table creates: its name, the role it grants and its
/// `Precedence`.
struct GroupSpec {
    group_name: &'static str,
    role: &'static str,
    precedence: i64,
}

/// One row of the claim table: the user that signs in, the groups it belongs to, and the
/// claims the identity token of that sign-in has to carry.
struct ClaimRow {
    case: &'static str,
    username: &'static str,
    groups: Vec<GroupSpec>,
    roles: Vec<&'static str>,
    /// The `cognito:preferred_role` claim, and `None` for a claim that must be ABSENT.
    preferred: Option<&'static str>,
}

/// An IAM role ARN naming `role_name`, satisfying `CreateGroup`'s `RoleArn` (length 20..2048
/// and its pattern).
fn role_arn(role_name: &str) -> String {
    format!("arn:aws:iam::000000000000:role/{role_name}")
}

/// RULE: the identity token a SELF-SERVICE sign-in issues carries every role the user's
/// groups grant in `cognito:roles`, best `Precedence` first, and carries
/// `cognito:preferred_role` only when the groups tied on that best precedence agree about
/// the role — answered by the sign-in itself, so an unresolvable tie is read off the first
/// response rather than waited for.
#[tokio::test]
async fn a_precedence_tie_withholds_the_preferred_role_from_a_self_service_sign_in() {
    let router = router();
    let user_pool_id = create_pool(&router, "modeled-claims", json!({})).await;
    let client_id = create_client(&router, &user_pool_id, "modeled-claims").await;

    let rows = vec![
        ClaimRow {
            case: "two groups at EQUAL Precedence with different roles: no preference",
            username: "tied",
            groups: vec![
                GroupSpec {
                    group_name: "tied-a",
                    role: "role-tied-a",
                    precedence: 5,
                },
                GroupSpec {
                    group_name: "tied-b",
                    role: "role-tied-b",
                    precedence: 5,
                },
            ],
            roles: vec!["role-tied-a", "role-tied-b"],
            preferred: None,
        },
        ClaimRow {
            case: "two groups at EQUAL Precedence with the SAME role: that role is preferred",
            username: "agreed",
            groups: vec![
                GroupSpec {
                    group_name: "agreed-a",
                    role: "role-agreed",
                    precedence: 5,
                },
                GroupSpec {
                    group_name: "agreed-b",
                    role: "role-agreed",
                    precedence: 5,
                },
            ],
            roles: vec!["role-agreed"],
            preferred: Some("role-agreed"),
        },
        ClaimRow {
            case: "two groups at distinct Precedence: the lower value wins",
            username: "ordered",
            groups: vec![
                GroupSpec {
                    group_name: "ordered-high",
                    role: "role-ordered-high",
                    precedence: 9,
                },
                GroupSpec {
                    group_name: "ordered-low",
                    role: "role-ordered-low",
                    precedence: 1,
                },
            ],
            roles: vec!["role-ordered-low", "role-ordered-high"],
            preferred: Some("role-ordered-low"),
        },
    ];

    for ClaimRow {
        case,
        username,
        groups,
        roles,
        preferred,
    } in rows
    {
        create_user(&router, &user_pool_id, username, &[]).await;
        for group in &groups {
            ok(
                &router,
                "CreateGroup",
                &json!({
                    "UserPoolId": &user_pool_id,
                    "GroupName": group.group_name,
                    "RoleArn": role_arn(group.role),
                    "Precedence": group.precedence,
                }),
            )
            .await;
            ok(
                &router,
                "AdminAddUserToGroup",
                &json!({
                    "UserPoolId": &user_pool_id,
                    "Username": username,
                    "GroupName": group.group_name,
                }),
            )
            .await;
        }

        let result = sign_in(&router, &client_id, username).await;
        let id_token = result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the sign-in answers an IdToken: {result}"));
        let claims = claims_of(id_token);

        let expected_roles: Vec<Value> = roles.iter().map(|role| json!(role_arn(role))).collect();
        assert_eq!(
            claims["cognito:roles"],
            Value::Array(expected_roles),
            "case {case:?}: expected cognito:roles {roles:?}, got {}",
            claims["cognito:roles"]
        );
        let expected_preferred = preferred.map(|role| json!(role_arn(role)));
        assert_eq!(
            claims.get("cognito:preferred_role").cloned(),
            expected_preferred,
            "case {case:?}: expected cognito:preferred_role {preferred:?}, got {:?}",
            claims.get("cognito:preferred_role")
        );
    }
}

/// One pool-scoped call of the IAM table: the operation, and the body naming one pool.
struct ScopedCall {
    operation: &'static str,
    /// The body naming the pool the caller's policy covers.
    granted: Value,
    /// The body naming the pool it does not.
    ungranted: Value,
    /// What the GRANTED call answers: `None` for one that is served, and the class and
    /// message of the refusal the operation's own precondition raises otherwise.
    served: Option<(&'static str, &'static str)>,
}

/// RULE: a request a caller's identity-based policies do not allow for the resource it
/// addresses is refused with `AccessDeniedException`, stating IAM's own sentence, BEFORE
/// the operation looks at the pool — so the same call on the pool the caller IS granted is
/// served, or refused by that operation's own modelled precondition.
#[tokio::test]
async fn a_pool_scoped_call_outside_the_callers_grant_is_access_denied() {
    let emulator = Emulator::new();
    let (granted_pool, granted_arn) = emulator.pool("modeled-granted").await;
    let (closed_pool, closed_arn) = emulator.pool("modeled-closed").await;
    // A caller that may perform every action of this service — but only on one of the two
    // pools.
    let caller = emulator
        .user("modeled-scoped", Some(&allow("cognito-idp:*", &granted_arn)))
        .await;

    // The branding both pools store, written with the account's own credential, so a read
    // of the granted pool reports that pool's own CSS.
    let granted_css = ".label-customizable { color: #1b4332; }";
    for (pool, css) in [
        (&granted_pool, granted_css),
        (&closed_pool, ".label-customizable { color: #7f1d1d; }"),
    ] {
        let (status, payload) = emulator
            .call(
                ACCOUNT_KEY,
                "SetUICustomization",
                &json!({ "UserPoolId": pool, "CSS": css }),
            )
            .await;
        assert_eq!(
            status, 200,
            "the setup SetUICustomization must succeed, got {status} with {payload}"
        );
    }
    let username = "haskell";
    let (status, payload) = emulator
        .call(
            ACCOUNT_KEY,
            "AdminCreateUser",
            &json!({ "UserPoolId": &granted_pool, "Username": username }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the setup AdminCreateUser must succeed, got {status} with {payload}"
    );

    let calls = vec![
        ScopedCall {
            operation: "GetUICustomization",
            granted: json!({ "UserPoolId": &granted_pool }),
            ungranted: json!({ "UserPoolId": &closed_pool }),
            served: None,
        },
        ScopedCall {
            operation: "GetUserPoolMfaConfig",
            granted: json!({ "UserPoolId": &granted_pool }),
            ungranted: json!({ "UserPoolId": &closed_pool }),
            served: None,
        },
        // A call whose own precondition refuses it as well: the authorization decision is
        // taken FIRST, so the two pools answer two different modelled refusals.
        ScopedCall {
            operation: "AdminListDevices",
            granted: json!({ "UserPoolId": &granted_pool, "Username": username }),
            ungranted: json!({ "UserPoolId": &closed_pool, "Username": username }),
            served: Some((
                "InvalidUserPoolConfigurationException",
                DEVICE_TRACKING_OFF,
            )),
        },
    ];

    for ScopedCall {
        operation,
        granted,
        ungranted,
        served,
    } in calls
    {
        let action = action_of(operation);
        let (status, answered) = emulator.call(&caller.access_key_id, operation, &ungranted).await;
        assert_eq!(
            (status, code_of(&answered).as_str()),
            ACCESS_DENIED,
            "case {operation:?} on the pool the caller is not granted: expected \
             {ACCESS_DENIED:?}, got {status} with {answered}"
        );
        let expected = format!(
            "User: {} is not authorized to perform: {action} on resource: {closed_arn} because \
             no identity-based policy allows the {action} action",
            caller.arn
        );
        assert_eq!(
            message_of(&answered),
            expected,
            "case {operation:?} on the pool the caller is not granted: expected {expected:?}, \
             got {:?}",
            message_of(&answered)
        );

        let (status, answered) = emulator.call(&caller.access_key_id, operation, &granted).await;
        match served {
            None => assert_eq!(
                status, 200,
                "case {operation:?} on the pool the caller IS granted: expected a 200, got \
                 {status} with {answered}"
            ),
            Some((class, message)) => assert_eq!(
                (status, code_of(&answered).as_str(), message_of(&answered).as_str()),
                (REFUSED, class, message),
                "case {operation:?} on the pool the caller IS granted: expected the \
                 operation's own {class}, got {status} with {answered}"
            ),
        }
    }

    // The read the grant allows reports the GRANTED pool's own stored branding.
    let (status, answered) = emulator
        .call(
            &caller.access_key_id,
            "GetUICustomization",
            &json!({ "UserPoolId": &granted_pool }),
        )
        .await;
    assert_eq!(
        (status, answered["UICustomization"]["CSS"].as_str()),
        (200, Some(granted_css)),
        "case \"the granted read reports the granted pool's branding\": expected \
         {granted_css:?}, got {status} with {answered}"
    );
}
