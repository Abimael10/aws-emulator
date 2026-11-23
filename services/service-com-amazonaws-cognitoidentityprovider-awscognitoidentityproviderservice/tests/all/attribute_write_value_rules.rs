// SPDX-License-Identifier: CC0-1.0

//! What a request that WRITES a user's attributes is held to before anything is
//! stored, driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! Four rules, each asserted over a table that spans the class it generalises
//! over rather than the one value a recording carried:
//!
//! * A user attribute whose values have a FORM is checked against that form
//!   before it is stored, whichever operation submitted it: an `email` holds an
//!   email ADDRESS, so a value that is not one — judged by the workspace's one
//!   reading of that form, `service_common::email_address` — is refused with 400
//!   `InvalidParameterException` and the sentence Amazon Cognito words that
//!   refusal with, exactly as a `phone_number` outside E.164 form already is.
//! * A value longer than the maximum an attribute value may carry
//!   (`AttributeType.Value` targets `AttributeValueType`, `@length` max 2048,
//!   `smithy_query(query="shape",shape="AttributeType,AttributeValueType")`) is
//!   no value an attribute can hold, and an UPDATE of a stored user states that
//!   ceiling per ATTRIBUTE: the attribute the request named and the maximum its
//!   value may have, with the rejected value not quoted back. A length is
//!   counted in CHARACTERS.
//! * A verified flag (`email_verified`, `phone_number_verified` — the flags of
//!   `VerifiedAttributeType`) records this service's own confirmation of the
//!   value beside it, so it is not an attribute its own holder may write: the
//!   self-service write is refused with the non-mutable-attribute sentence,
//!   while the administrative write — whose `UserAttributes` documents the flag
//!   as the way to "skip the verification message" — still takes it.
//! * A pool whose `AliasAttributes` include an attribute whose values have a
//!   format of their own keeps that format out of its USERNAME namespace, since
//!   an alias value names one user of the pool: a name of that format is no name
//!   a user of such a pool may be given — neither the `Username` a registration
//!   stores a user under nor the `preferred_username` a write stores for one —
//!   and is refused with the sentence naming the format and the alias the pool is
//!   configured for. A pool that aliases nothing of that format, and one that
//!   signs its users in BY the attribute (`UsernameAttributes`), take the same
//!   name; and a name that merely ADDRESSES a stored user still resolves, since a
//!   `Username` "can be any of their alias attributes".
//! * A value submitted for an attribute earns the finding the pool's schema
//!   states about it: a value that is not of the declared `AttributeDataType`
//!   (`Boolean`), a blank value for an attribute the schema marks required —
//!   which is the same finding a DELETION of that attribute earns, whichever
//!   operation asks for the removal — a
//!   value that is no unix timestamp for an attribute whose values are points in
//!   time, and a verified flag set where the attribute it vouches for would hold
//!   no value at all.
//!
//! And a refused write stores NOTHING: every refusal here is read back through
//! `AdminGetUser`, which must still report the value the user held.
//!
//! Every pool, app client, user and access token is real: they come from
//! `CreateUserPool`, `CreateUserPoolClient`, `AdminCreateUser`,
//! `AdminSetUserPassword` and `InitiateAuth`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes an administrative call to
/// this service. The signature is never checked here: the suite turns request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// This service's regional endpoint host, which a signed call is addressed to.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The emulator's own endpoint: an address, so an unsigned request addressed to
/// it names no service in its `Host` header and `X-Amz-Target` is the only
/// routing signal left. That is how a signed-in user's own client calls the
/// self-service operations, which the model marks `@optionalAuth`.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The error code every refusal of a bad parameter carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The maximum an attribute value's length is held to, from the model:
/// `AttributeValueType` is `@length` max 2048.
const MAX_ATTRIBUTE_VALUE_LENGTH: usize = 2048;

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// Dispatch `operation` with `body` and return its status and JSON response
/// body. A `signed` call carries a credential scope, as an SDK sends every
/// administrative operation; an unsigned one is addressed to the emulator's own
/// address, which is how a user's own client calls a self-service operation.
async fn dispatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    signed: bool,
) -> (u16, Value) {
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

/// Call the signed `operation` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    dispatch(router, operation, body, true).await
}

/// Call the signed `operation`, requiring success, and return its response body.
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

/// The `message` of an error body, under either casing the protocol admits.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// A pool named `pool_name` carrying `configuration`, and its minted id.
async fn pool(router: &MultiServiceRouter, pool_name: &str, configuration: Value) -> String {
    let mut body: Map<String, Value> = configuration
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("a pool configuration is a JSON object: {configuration}"));
    body.insert("PoolName".to_string(), json!(pool_name));
    ok(router, "CreateUserPool", &Value::Object(body)).await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string()
}

/// An app client of `user_pool_id` that allows the password sign-in this suite
/// uses, and its minted `ClientId`.
async fn client(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "attribute-writer",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string()
}

/// `attributes` as the modelled `AttributeListType` an SDK sends.
fn attribute_list(attributes: &[(&str, &str)]) -> Value {
    Value::Array(
        attributes
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect(),
    )
}

/// Create `username` holding `attributes` with a PERMANENT password, so the user
/// is `CONFIRMED` and can sign itself in.
async fn confirmed_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": attribute_list(attributes),
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

/// Sign `username` in through an unsigned `InitiateAuth` and return its access
/// token.
async fn access_token(router: &MultiServiceRouter, client_id: &str, username: &str) -> String {
    let (status, payload) = dispatch(
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
        status, 200,
        "signing {username} in must succeed, got {status} with {payload}"
    );
    payload["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a signed-in user holds an access token: {payload}"))
        .to_string()
}

/// The value `username` holds for `name`, read back through `AdminGetUser`, or
/// `None` when the user holds none.
async fn stored_attribute(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    name: &str,
) -> Option<String> {
    let read = ok(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    read.get("UserAttributes")
        .and_then(Value::as_array)?
        .iter()
        .find(|attribute| attribute.get("Name").and_then(Value::as_str) == Some(name))
        .and_then(|attribute| attribute.get("Value").and_then(Value::as_str))
        .map(ToString::to_string)
}

/// The two operations that WRITE the attributes of a user the pool already
/// holds: the administrative one, addressed by `Username`, and the self-service
/// one, addressed by the signed-in user's own access token.
const ADMIN_WRITE: &str = "AdminUpdateUserAttributes";
const SELF_WRITE: &str = "UpdateUserAttributes";

/// Submit `attributes` through `stage` — one of the two writes above — and
/// return its status and JSON body.
async fn write_attributes(
    router: &MultiServiceRouter,
    stage: &str,
    user_pool_id: &str,
    username: &str,
    token: &str,
    attributes: &[(&str, &str)],
) -> (u16, Value) {
    if stage == ADMIN_WRITE {
        call(
            router,
            ADMIN_WRITE,
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "UserAttributes": attribute_list(attributes),
            }),
        )
        .await
    } else {
        dispatch(
            router,
            SELF_WRITE,
            &json!({
                "AccessToken": token,
                "UserAttributes": attribute_list(attributes),
            }),
            false,
        )
        .await
    }
}

/// One row of the email-form table: a value submitted for `email` and whether
/// the address grammar admits it.
struct EmailRow {
    case: &'static str,
    value: &'static str,
    /// `true` for a value that is an address, so every write stores it.
    stored: bool,
}

/// The rows span the forms a submitted `email` can take: addresses with a
/// two-label and a four-label domain, one carrying the `+` of a tagged mailbox,
/// one in capitals, and values missing each part an address is made of — the
/// local part, the `@`, the domain, the domain's suffix — plus one carrying a
/// space inside it and one that is no address at all, which is the form the
/// recorded refusal carried. The form itself is the workspace's one reading of
/// an address (`service_common::email_address`:
/// `UserName@[SubDomain.]Domain.TopLevelDomain`).
const EMAIL_ROWS: &[EmailRow] = &[
    EmailRow {
        case: "an address with a dotted domain",
        value: "ada@lovelace.example",
        stored: true,
    },
    EmailRow {
        case: "an address with a four-label domain",
        value: "ada@mail.lovelace.co.uk",
        stored: true,
    },
    EmailRow {
        case: "an address with a tagged mailbox",
        value: "ada+billing@lovelace.example",
        stored: true,
    },
    EmailRow {
        case: "an address written in capitals",
        value: "ADA@LOVELACE.EXAMPLE",
        stored: true,
    },
    EmailRow {
        case: "a value with no at-sign at all",
        value: "not-an-email",
        stored: false,
    },
    EmailRow {
        case: "a value with no local part",
        value: "@lovelace.example",
        stored: false,
    },
    EmailRow {
        case: "a value with no domain",
        value: "ada@",
        stored: false,
    },
    EmailRow {
        case: "a value whose domain is a single label, so it names no top-level domain",
        value: "ada@localhost",
        stored: false,
    },
    EmailRow {
        case: "a value carrying a space inside it",
        value: "ada lovelace@example.test",
        stored: false,
    },
    EmailRow {
        case: "two at-signs, so no single domain",
        value: "ada@lovelace@example.test",
        stored: false,
    },
];

#[tokio::test]
async fn an_email_attribute_holds_an_address_whichever_operation_submits_it() {
    let router = router();
    let user_pool_id = pool(&router, "email-form", json!({})).await;
    let client_id = client(&router, &user_pool_id).await;
    let held = "held@example.test";
    confirmed_user(&router, &user_pool_id, "ada", &[("email", held)]).await;
    let token = access_token(&router, &client_id, "ada").await;

    for row in EMAIL_ROWS {
        let case = row.case;
        // The administrative write of a stored user's attributes, and the
        // self-service write of the signed-in user's own: both submit a value
        // for the same attribute, so both hold it to the same form.
        for stage in [ADMIN_WRITE, SELF_WRITE] {
            let (status, payload) = write_attributes(
                &router,
                stage,
                &user_pool_id,
                "ada",
                &token,
                &[("email", row.value)],
            )
            .await;
            assert_eq!(
                (status == 200, status),
                (row.stored, status),
                "case {case:?}: {stage} owes the same answer for {:?}, got {status} with \
                 {payload}",
                row.value
            );
            if !row.stored {
                assert_eq!(
                    (error_code(&payload), error_message(&payload)),
                    (INVALID_PARAMETER, "Invalid email address format."),
                    "case {case:?}: {stage} must word the refusal the same way, got {payload}"
                );
            }
            // A refused write stores nothing, so the user keeps the value it
            // held; a served one is readable at once.
            let expected = if row.stored { row.value } else { held };
            let stored = stored_attribute(&router, &user_pool_id, "ada", "email").await;
            assert_eq!(
                stored.as_deref(),
                Some(expected),
                "case {case:?}: {stage} must leave {expected:?} stored, got {stored:?}"
            );
            // Put the held address back, so the next row starts where this one
            // did.
            if row.stored {
                ok(
                    &router,
                    "AdminUpdateUserAttributes",
                    &json!({
                        "UserPoolId": user_pool_id.clone(),
                        "Username": "ada",
                        "UserAttributes": attribute_list(&[("email", held)]),
                    }),
                )
                .await;
            }
        }
    }

    // A REGISTRATION submits the same attribute, so it is held to the same form:
    // the value that is no address at all is refused there too, and no user is
    // left behind.
    let (status, payload) = call(
        &router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id.clone(),
            "Username": "registered",
            "UserAttributes": attribute_list(&[("email", "not-an-email")]),
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, INVALID_PARAMETER, "Invalid email address format."),
        "a registration carrying no address must be refused the same way, got {status} with \
         {payload}"
    );
    let (status, payload) = call(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": "registered" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, "UserNotFoundException"),
        "the refused registration must have stored no user, got {status} with {payload}"
    );
}

/// One row of the length table: the attribute a value is submitted for, how many
/// characters it carries and the character it is written with.
struct LengthRow {
    case: &'static str,
    attribute: &'static str,
    characters: usize,
    /// The character the value repeats, so a row can state a length in
    /// characters that is not a length in bytes.
    character: char,
    /// `true` for a value within the ceiling, which every write stores.
    stored: bool,
}

/// The rows span the ceiling from both sides, on attributes of each kind — a
/// standard string attribute the recorded case cited (`gender`), one it did not
/// (`nickname`) and a custom attribute this suite's pool declares — with a value
/// at the maximum, one character past it, far past it, and a non-ASCII value
/// whose characters and bytes differ.
const LENGTH_ROWS: &[LengthRow] = &[
    LengthRow {
        case: "a value at the maximum",
        attribute: "gender",
        characters: MAX_ATTRIBUTE_VALUE_LENGTH,
        character: 'g',
        stored: true,
    },
    LengthRow {
        case: "a value one character past the maximum",
        attribute: "gender",
        characters: MAX_ATTRIBUTE_VALUE_LENGTH + 1,
        character: 'g',
        stored: false,
    },
    LengthRow {
        case: "a value far past the maximum",
        attribute: "nickname",
        characters: 4 * MAX_ATTRIBUTE_VALUE_LENGTH,
        character: 'n',
        stored: false,
    },
    LengthRow {
        case: "a non-ASCII value at the maximum, whose bytes are twice its characters",
        attribute: "nickname",
        characters: MAX_ATTRIBUTE_VALUE_LENGTH,
        character: 'é',
        stored: true,
    },
    LengthRow {
        case: "a non-ASCII value one character past the maximum",
        attribute: "nickname",
        characters: MAX_ATTRIBUTE_VALUE_LENGTH + 1,
        character: 'é',
        stored: false,
    },
    LengthRow {
        case: "a custom attribute's value past the maximum",
        attribute: "custom:note",
        characters: MAX_ATTRIBUTE_VALUE_LENGTH + 64,
        character: 'c',
        stored: false,
    },
];

#[tokio::test]
async fn an_update_states_the_attribute_value_ceiling_per_attribute() {
    let router = router();
    let user_pool_id = pool(
        &router,
        "value-ceiling",
        json!({
            "Schema": [{ "Name": "note", "AttributeDataType": "String", "Mutable": true }],
        }),
    )
    .await;
    let client_id = client(&router, &user_pool_id).await;
    confirmed_user(&router, &user_pool_id, "ada", &[]).await;
    let token = access_token(&router, &client_id, "ada").await;

    for row in LENGTH_ROWS {
        let case = row.case;
        let value: String = std::iter::repeat_n(row.character, row.characters).collect();
        for stage in [ADMIN_WRITE, SELF_WRITE] {
            let (status, payload) = write_attributes(
                &router,
                stage,
                &user_pool_id,
                "ada",
                &token,
                &[(row.attribute, value.as_str())],
            )
            .await;
            assert_eq!(
                (status == 200, status),
                (row.stored, status),
                "case {case:?}: {stage} owes the same answer for {} characters, got {status} \
                 with {payload}",
                row.characters
            );
            if row.stored {
                let stored = stored_attribute(&router, &user_pool_id, "ada", row.attribute).await;
                assert_eq!(
                    stored.as_deref(),
                    Some(value.as_str()),
                    "case {case:?}: {stage} must store a value within the ceiling"
                );
                // Blank the attribute again, so the refusals below are read
                // against a user holding nothing for it.
                ok(
                    &router,
                    ADMIN_WRITE,
                    &json!({
                        "UserPoolId": user_pool_id.clone(),
                        "Username": "ada",
                        "UserAttributes": attribute_list(&[(row.attribute, "")]),
                    }),
                )
                .await;
                continue;
            }
            let expected = format!(
                "The value of the attribute {} must have a length less than or equal to {} \
                 characters",
                row.attribute, MAX_ATTRIBUTE_VALUE_LENGTH
            );
            assert_eq!(
                (error_code(&payload), error_message(&payload)),
                (INVALID_PARAMETER, expected.as_str()),
                "case {case:?}: {stage} must state the ceiling for the attribute it named, got \
                 {payload}"
            );
            // The sentence states the rule and not the payload: the rejected
            // value is not quoted back.
            let message = error_message(&payload);
            assert!(
                !message.contains(&value[..64]),
                "case {case:?}: {stage} must not quote the rejected value back, got {message:?}"
            );
            let stored = stored_attribute(&router, &user_pool_id, "ada", row.attribute).await;
            assert_eq!(
                stored, None,
                "case {case:?}: {stage} must have stored nothing, got {stored:?}"
            );
        }
    }
}

/// One row of the verified-flag table: the flag a request names and the value it
/// submits for it.
struct FlagRow {
    case: &'static str,
    /// What the request submits, the flag among it.
    submitted: &'static [(&'static str, &'static str)],
    /// The flag the refusal must name.
    flag: &'static str,
}

/// The rows span both flags of `VerifiedAttributeType`, both values a flag can
/// carry, a flag submitted alone and one submitted beside the value it vouches
/// for, and a flag beside the deletion of that value.
const FLAG_ROWS: &[FlagRow] = &[
    FlagRow {
        case: "an email_verified of true, alone",
        submitted: &[("email_verified", "true")],
        flag: "email_verified",
    },
    FlagRow {
        case: "an email_verified of false, alone",
        submitted: &[("email_verified", "false")],
        flag: "email_verified",
    },
    FlagRow {
        case: "an email_verified beside the address it would vouch for",
        submitted: &[("email", "new@example.test"), ("email_verified", "true")],
        flag: "email_verified",
    },
    FlagRow {
        case: "an email_verified beside the deletion of the address it vouched for",
        submitted: &[("email", ""), ("email_verified", "true")],
        flag: "email_verified",
    },
    FlagRow {
        case: "a phone_number_verified of true",
        submitted: &[("phone_number_verified", "true")],
        flag: "phone_number_verified",
    },
];

#[tokio::test]
async fn a_verified_flag_is_not_the_signed_in_users_to_write() {
    let router = router();
    let user_pool_id = pool(&router, "verified-flags", json!({})).await;
    let client_id = client(&router, &user_pool_id).await;
    let held = "held@example.test";
    confirmed_user(
        &router,
        &user_pool_id,
        "ada",
        &[("email", held), ("phone_number", "+15550100")],
    )
    .await;
    let token = access_token(&router, &client_id, "ada").await;

    for row in FLAG_ROWS {
        let case = row.case;
        let (status, payload) = dispatch(
            &router,
            "UpdateUserAttributes",
            &json!({
                "AccessToken": token.clone(),
                "UserAttributes": attribute_list(row.submitted),
            }),
            false,
        )
        .await;
        let expected = format!("Cannot modify the non-mutable attribute {}", row.flag);
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, INVALID_PARAMETER, expected.as_str()),
            "case {case:?}: the self-service write must be refused, got {status} with {payload}"
        );
        // Nothing of the request was written, the value beside the flag
        // included.
        let stored = stored_attribute(&router, &user_pool_id, "ada", "email").await;
        assert_eq!(
            stored.as_deref(),
            Some(held),
            "case {case:?}: the refused write stores nothing, got {stored:?}"
        );
    }

    // The ADMINISTRATIVE write is the caller the flag's own documentation names
    // ("to skip the verification message ... include the email_verified ...
    // attribute, with a value of true"), so it still takes the flag — which is
    // what makes this a rule about the caller's authority rather than about the
    // attribute's name.
    ok(
        &router,
        "AdminUpdateUserAttributes",
        &json!({
            "UserPoolId": user_pool_id.clone(),
            "Username": "ada",
            "UserAttributes": attribute_list(&[
                ("email", "vouched@example.test"),
                ("email_verified", "true"),
            ]),
        }),
    )
    .await;
    let stored = stored_attribute(&router, &user_pool_id, "ada", "email_verified").await;
    assert_eq!(
        stored.as_deref(),
        Some("true"),
        "the administrative write stores the flag it asserted, got {stored:?}"
    );
}

/// One row of the username-namespace table: how the pool signs its users in, the
/// name a request submits for a user, and the format the refusal names — `None`
/// for a name the pool admits.
struct UsernameRow {
    case: &'static str,
    /// The pool's own configuration, as `CreateUserPool` takes it.
    configuration: Value,
    submitted: &'static str,
    /// The format the refusal names (`email`, `phone number`), or `None` where
    /// the name is one the pool admits.
    refused_format: Option<&'static str>,
}

/// The rows span each alias configuration against a submitted name of each
/// format: an aliased attribute whose values have a format of their own
/// (`email`, `phone_number`), an aliased attribute whose values have none
/// (`preferred_username`), a pool that aliases nothing, and a pool that signs
/// its users in BY the attribute instead (`UsernameAttributes`), where the
/// submitted value IS that attribute's value rather than a name shadowing
/// it.
fn username_rows() -> Vec<UsernameRow> {
    vec![
        UsernameRow {
            case: "an address in a pool that aliases email",
            configuration: json!({ "AliasAttributes": ["email"] }),
            submitted: "ada@lovelace.example",
            refused_format: Some("email"),
        },
        UsernameRow {
            case: "a plain name in a pool that aliases email",
            configuration: json!({ "AliasAttributes": ["email"] }),
            submitted: "ada",
            refused_format: None,
        },
        UsernameRow {
            case: "a number in a pool that aliases phone_number",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            submitted: "+15550100",
            refused_format: Some("phone number"),
        },
        UsernameRow {
            case: "an address in a pool that aliases phone_number only",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            submitted: "ada@lovelace.example",
            refused_format: None,
        },
        UsernameRow {
            case: "an address in a pool that aliases both",
            configuration: json!({ "AliasAttributes": ["email", "phone_number"] }),
            submitted: "ada@lovelace.example",
            refused_format: Some("email"),
        },
        UsernameRow {
            case: "an address in a pool that aliases preferred_username, which has no format",
            configuration: json!({ "AliasAttributes": ["preferred_username"] }),
            submitted: "ada@lovelace.example",
            refused_format: None,
        },
        UsernameRow {
            case: "an address in a pool that aliases nothing",
            configuration: json!({}),
            submitted: "ada@lovelace.example",
            refused_format: None,
        },
        UsernameRow {
            case: "an address in a pool that signs its users in by email",
            configuration: json!({ "UsernameAttributes": ["email"] }),
            submitted: "ada@lovelace.example",
            refused_format: None,
        },
    ]
}

/// The sentence a name of `format`'s format is refused with in a pool that
/// aliases the attribute of that format.
fn alias_namespace_refusal(format: &str) -> String {
    format!(
        "Username cannot be of {format} format, since user pool is configured for {format} alias."
    )
}

#[tokio::test]
async fn a_name_of_an_aliased_attributes_format_is_no_name_a_user_may_take() {
    let router = router();
    for (index, row) in username_rows().into_iter().enumerate() {
        let case = row.case;
        let signs_in_by_attribute = row.configuration.get("UsernameAttributes").is_some();
        let user_pool_id = pool(&router, &format!("namespace-{index}"), row.configuration).await;

        // A REGISTRATION stores the user UNDER the submitted name, so that name
        // has to be one the pool's namespaces admit.
        let (status, payload) = call(
            &router,
            "AdminCreateUser",
            &json!({ "UserPoolId": user_pool_id.clone(), "Username": row.submitted }),
        )
        .await;
        match row.refused_format {
            Some(format) => {
                let expected = alias_namespace_refusal(format);
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, INVALID_PARAMETER, expected.as_str()),
                    "case {case:?}: the creation must be refused for the name it submitted, got \
                     {status} with {payload}"
                );
                // The refused creation stored no user, so the name is still free.
                let (status, payload) = call(
                    &router,
                    "AdminGetUser",
                    &json!({ "UserPoolId": user_pool_id.clone(), "Username": row.submitted }),
                )
                .await;
                assert_eq!(
                    (status, error_code(&payload)),
                    (400, "UserNotFoundException"),
                    "case {case:?}: the refused creation stored no user, got {status} with \
                     {payload}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: the pool admits the name, so the creation is served, got \
                 {status} with {payload}"
            ),
        }

        // A `preferred_username` is a NAME its holder answers to as well, so a
        // value written for it is held to the same namespaces — whichever
        // operation writes it. A pool that signs its users in by an attribute
        // mints the username itself, so that row's user is reached by the value
        // it signed up with.
        let holder = format!("namespace-user-{index}");
        let addressed_by = if signs_in_by_attribute {
            let signed_up = format!("{holder}@example.test");
            ok(
                &router,
                "AdminCreateUser",
                &json!({ "UserPoolId": user_pool_id.clone(), "Username": signed_up.clone() }),
            )
            .await;
            signed_up
        } else {
            ok(
                &router,
                "AdminCreateUser",
                &json!({ "UserPoolId": user_pool_id.clone(), "Username": holder.clone() }),
            )
            .await;
            holder
        };
        let (status, payload) = call(
            &router,
            ADMIN_WRITE,
            &json!({
                "UserPoolId": user_pool_id.clone(),
                "Username": addressed_by.clone(),
                "UserAttributes": attribute_list(&[("preferred_username", row.submitted)]),
            }),
        )
        .await;
        match row.refused_format {
            Some(format) => {
                let expected = alias_namespace_refusal(format);
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, INVALID_PARAMETER, expected.as_str()),
                    "case {case:?}: the write must be refused for the name it would store, got \
                     {status} with {payload}"
                );
                let stored = stored_attribute(
                    &router,
                    &user_pool_id,
                    &addressed_by,
                    "preferred_username",
                )
                .await;
                assert_eq!(
                    stored, None,
                    "case {case:?}: the refused write stored nothing, got {stored:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the pool admits the name, so the write is served, got \
                     {status} with {payload}"
                );
                let stored = stored_attribute(
                    &router,
                    &user_pool_id,
                    &addressed_by,
                    "preferred_username",
                )
                .await;
                assert_eq!(
                    stored.as_deref(),
                    Some(row.submitted),
                    "case {case:?}: the served write stored the name, got {stored:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_write_addressed_by_an_alias_value_reaches_its_holder() {
    let router = router();
    // The namespace rule is about a name a user is given, not about one that
    // ADDRESSES a stored user: a `Username` "can be any of their alias
    // attributes" (`AdminGetUser.Username`), so an address that resolves a user
    // of the pool reaches it on a write exactly as it does on a read.
    let user_pool_id = pool(
        &router,
        "alias-addressed",
        json!({ "AliasAttributes": ["email"], "AutoVerifiedAttributes": ["email"] }),
    )
    .await;
    let address = "holder@example.test";
    confirmed_user(
        &router,
        &user_pool_id,
        "holder",
        &[("email", address), ("email_verified", "true")],
    )
    .await;
    let read = ok(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id.clone(), "Username": address }),
    )
    .await;
    assert_eq!(
        read["Username"],
        json!("holder"),
        "the alias must resolve its holder on a read, got {read}"
    );

    ok(
        &router,
        ADMIN_WRITE,
        &json!({
            "UserPoolId": user_pool_id.clone(),
            "Username": address,
            "UserAttributes": attribute_list(&[("nickname", "Ada")]),
        }),
    )
    .await;
    let stored = stored_attribute(&router, &user_pool_id, "holder", "nickname").await;
    assert_eq!(
        stored.as_deref(),
        Some("Ada"),
        "the write addressed by the alias must reach its holder, got {stored:?}"
    );
}

/// One row of the schema-finding table: what a request submits for a user of a
/// pool whose schema declares `name` required, and the whole message the refusal
/// carries.
struct FindingRow {
    case: &'static str,
    /// What the request submits.
    submitted: &'static [(&'static str, &'static str)],
    /// The message the refusal must carry, exactly.
    expected: &'static str,
}

/// The rows span the findings a submitted VALUE earns from the pool's schema
/// beside the bounds it declares: a value that is not of the data type the
/// schema declares (`Boolean`), a blank value for an attribute the schema marks
/// required, a value that is no unix timestamp for the standard attribute whose
/// values are points in time, and a verified flag set where the attribute it
/// vouches for would hold no value — for each of the two attributes that carry
/// one.
const FINDING_ROWS: &[FindingRow] = &[
    FindingRow {
        case: "a value that is not a boolean for a Boolean attribute",
        submitted: &[("email", "ada@example.test"), ("email_verified", "maybe")],
        expected: "user.email_verified: The attribute is not a valid boolean.\n",
    },
    FindingRow {
        case: "a value that is not a boolean for the other Boolean attribute",
        submitted: &[
            ("phone_number", "+15550100"),
            ("phone_number_verified", "1"),
        ],
        expected: "user.phone_number_verified: The attribute is not a valid boolean.\n",
    },
    FindingRow {
        case: "a blank value for an attribute the schema marks required",
        submitted: &[("name", "")],
        expected: "user.name: Required attribute cannot be deleted.\n",
    },
    FindingRow {
        case: "a value that is no unix timestamp for the attribute whose values are timestamps",
        submitted: &[("updated_at", "yesterday")],
        expected: "Invalid updated_at timestamp format. It should be a unix timestamp.",
    },
    FindingRow {
        case: "a verified flag for an attribute the user holds nothing for",
        submitted: &[("phone_number_verified", "true")],
        expected: "Phone number is required to verify/un-verify a phone number",
    },
    FindingRow {
        case: "a verified flag of false for an attribute the user holds nothing for",
        submitted: &[("phone_number_verified", "false")],
        expected: "Phone number is required to verify/un-verify a phone number",
    },
    FindingRow {
        case: "a verified flag beside the deletion of the value it vouches for",
        submitted: &[("email", ""), ("email_verified", "true")],
        expected: "Email is required to verify/un-verify an email",
    },
];

#[tokio::test]
async fn a_submitted_value_earns_the_schema_finding_its_attribute_states() {
    let router = router();
    // The pool's schema marks `name` required, so one row can blank it; every
    // other attribute the rows name is one the standard schema declares.
    let user_pool_id = pool(
        &router,
        "schema-findings",
        json!({
            "Schema": [{
                "Name": "name",
                "AttributeDataType": "String",
                "Mutable": true,
                "Required": true,
            }],
        }),
    )
    .await;
    let client_id = client(&router, &user_pool_id).await;
    confirmed_user(
        &router,
        &user_pool_id,
        "ada",
        &[("name", "Ada"), ("email", "held@example.test")],
    )
    .await;
    let token = access_token(&router, &client_id, "ada").await;

    for row in FINDING_ROWS {
        let case = row.case;
        for stage in [ADMIN_WRITE, SELF_WRITE] {
            // The self-service write is refused for the verified flag it names
            // before any other finding of the same request is reached, so only
            // the rows that name none are driven through it.
            if stage == SELF_WRITE
                && row
                    .submitted
                    .iter()
                    .any(|(name, _)| name.ends_with("_verified"))
            {
                continue;
            }
            let (status, payload) = write_attributes(
                &router,
                stage,
                &user_pool_id,
                "ada",
                &token,
                row.submitted,
            )
            .await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, INVALID_PARAMETER, row.expected),
                "case {case:?}: {stage} must state the finding its schema reports, got {status} \
                 with {payload}"
            );
        }
        // Nothing of a refused request is written: the user keeps the values it
        // held.
        for (attribute, held) in [("name", Some("Ada")), ("email", Some("held@example.test"))] {
            let stored = stored_attribute(&router, &user_pool_id, "ada", attribute).await;
            assert_eq!(
                stored.as_deref(),
                held,
                "case {case:?}: the refused write must leave {attribute} as it was, got \
                 {stored:?}"
            );
        }
    }

    // The values each of those attributes DOES admit are stored, so the findings
    // above are the schema's answer and not every write's.
    ok(
        &router,
        ADMIN_WRITE,
        &json!({
            "UserPoolId": user_pool_id.clone(),
            "Username": "ada",
            "UserAttributes": attribute_list(&[
                ("email", "verified@example.test"),
                ("email_verified", "true"),
                ("updated_at", "1700000000"),
            ]),
        }),
    )
    .await;
    for (attribute, expected) in [
        ("email", "verified@example.test"),
        ("email_verified", "true"),
        ("updated_at", "1700000000"),
    ] {
        let stored = stored_attribute(&router, &user_pool_id, "ada", attribute).await;
        assert_eq!(
            stored.as_deref(),
            Some(expected),
            "the admitted write must store {attribute}, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn a_required_attribute_is_refused_the_same_way_however_a_request_would_remove_it() {
    let router = router();
    // Taking a required attribute's value away is the same finding as blanking
    // it: the pool's schema says it must hold one. The three ways a request can
    // ask for that removal — a blank value through either attribute write, and
    // the deletion that names the attribute — therefore state the same report
    // line.
    let user_pool_id = pool(
        &router,
        "required-removal",
        json!({
            "Schema": [{
                "Name": "name",
                "AttributeDataType": "String",
                "Mutable": true,
                "Required": true,
            }],
        }),
    )
    .await;
    let client_id = client(&router, &user_pool_id).await;
    confirmed_user(&router, &user_pool_id, "ada", &[("name", "Ada")]).await;
    let token = access_token(&router, &client_id, "ada").await;
    let expected = "user.name: Required attribute cannot be deleted.\n";

    for (case, (status, payload)) in [
        (
            "the administrative deletion",
            call(
                &router,
                "AdminDeleteUserAttributes",
                &json!({
                    "UserPoolId": user_pool_id.clone(),
                    "Username": "ada",
                    "UserAttributeNames": ["name"],
                }),
            )
            .await,
        ),
        (
            "the self-service deletion",
            dispatch(
                &router,
                "DeleteUserAttributes",
                &json!({
                    "AccessToken": token.clone(),
                    "UserAttributeNames": ["name"],
                }),
                false,
            )
            .await,
        ),
    ] {
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, INVALID_PARAMETER, expected),
            "case {case:?}: must state the schema finding the blank value earns, got {status} \
             with {payload}"
        );
    }
    // Every refusal above left the value where it was.
    let stored = stored_attribute(&router, &user_pool_id, "ada", "name").await;
    assert_eq!(
        stored.as_deref(),
        Some("Ada"),
        "the refused removals must leave the required value stored, got {stored:?}"
    );
}
