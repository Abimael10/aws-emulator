// SPDX-License-Identifier: CC0-1.0

//! Who OWNS an alias value of a user pool, over the wire, driven exactly as an
//! SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`).
//!
//! An alias value names ONE user of the pool, so every write that would hand a
//! second user a value another account already answers to is decided by that
//! one rule. The three ways it is decided:
//!
//! * `AdminCreateUser` MIGRATES the value when the request asks it to and the
//!   attribute is one whose verified flag is what makes the value its holder's:
//!   "if this parameter is set to `True` and the phone number or email address
//!   specified in the `UserAttributes` parameter already exists as an alias with
//!   a different user, the API call will migrate the alias from the previous user
//!   to the newly created user. The previous user will no longer be able to log
//!   in using that alias. If this parameter is set to `False`, the API throws an
//!   `AliasExistsException` error if the alias already exists"
//!   (`AdminCreateUser.ForceAliasCreation`) — and this operation, which models no
//!   `AliasExistsException`, reports the account that exists as
//!   `UsernameExistsException`. The migration takes the flag from the previous
//!   holder, which keeps its value; a value of a `UsernameAttributes` attribute
//!   is the other user's sign-in NAME rather than an alias of it, and an alias
//!   attribute that carries no verified flag (`preferred_username`) has no flag
//!   to take away, so neither moves however the request asks.
//! * A write of `preferred_username` — an alias attribute with no verified flag
//!   — to a value another account already answers to is refused
//!   `AliasExistsException` with the fixed sentence AWS states such a refusal in,
//!   whichever of the two attribute writers submitted it, and the user is left
//!   exactly as it was.
//! * `SignUp` may not carry a `preferred_username` at all in a pool that aliases
//!   it: the account a registration creates is not confirmed yet, so it cannot
//!   be handed a name the pool aliases, and the refusal is
//!   `InvalidParameterException` with a fixed sentence naming that stage and the
//!   pool's configuration. No user is created.
//!
//! Attribute-format validation is judged BEFORE any of this, so a malformed
//! `phone_number` or `email` is refused for its form even where the request asks
//! for a migration.
//!
//! Every pool, user and alias here is real: they come from `CreateUserPool`,
//! `AdminCreateUser`, `SignUp` and the two attribute writers, and every write is
//! read back through `AdminGetUser`. Each table row names the case it asserts and
//! every assertion reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service; the
/// signature itself is never checked here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The sentence `AdminCreateUser` answers an account that already exists with,
/// which names no username.
const USERNAME_EXISTS_MESSAGE: &str = "User account already exists";

/// The sentence a write refused because the alias value it would store already
/// names another account carries: fixed text with no variable part.
const ALIAS_EXISTS_MESSAGE: &str = "User account already exists for the alias.";

/// The sentence a self-service registration that carries a `preferred_username`
/// into a pool that aliases that attribute is refused with: fixed text naming
/// the stage and the pool's configuration.
const SIGN_UP_PREFERRED_USERNAME_MESSAGE: &str =
    "Preferred username cannot be provided for unconfirmed account, since user pool is \
     configured for preferred username alias.";

/// The sentence a `phone_number` that is not a phone number is refused with.
const INVALID_PHONE_MESSAGE: &str = "Invalid phone number format.";

/// The sentence an `email` that is not an email address is refused with.
const INVALID_EMAIL_MESSAGE: &str = "Invalid email address format.";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
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
        .or_else(|| payload.get("Message").and_then(Value::as_str))
        .unwrap_or("<no message>")
}

/// A pool named `name` with `configuration` merged into its create, returned as
/// its minted `UserPoolId`.
async fn pool(router: &MultiServiceRouter, name: &str, configuration: Value) -> String {
    let mut body = json!({ "PoolName": name });
    if let Some(members) = configuration.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    let created = ok(router, "CreateUserPool", &body).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id, got {created}"))
        .to_string()
}

/// An app client of `user_pool_id` that admits the administrative password flow,
/// returned as its minted `ClientId`.
async fn client(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "alias-suite-client",
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id, got {created}"))
        .to_string()
}

/// `(name, value)` pairs as the wire's `AttributeListType`.
fn attribute_list(attributes: &[(&str, &str)]) -> Value {
    Value::Array(
        attributes
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect(),
    )
}

/// Create `username` in `user_pool_id` holding `attributes`, requiring success,
/// and return its generated `sub`.
async fn user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) -> String {
    let created = ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": attribute_list(attributes),
        }),
    )
    .await;
    stored_attribute_of(&created["User"], "sub")
        .unwrap_or_else(|| panic!("a created user carries its sub, got {created}"))
}

/// The value `rendered` (a `UserType` or an `AdminGetUser` answer) holds for the
/// attribute `name`, or `None` when it holds none.
fn stored_attribute_of(rendered: &Value, name: &str) -> Option<String> {
    rendered["Attributes"]
        .as_array()
        .or_else(|| rendered["UserAttributes"].as_array())?
        .iter()
        .find(|attribute| attribute["Name"] == json!(name))
        .and_then(|attribute| attribute["Value"].as_str())
        .map(str::to_string)
}

/// `AdminGetUser` for `named`, as `(status, payload)`.
async fn read(router: &MultiServiceRouter, user_pool_id: &str, named: &str) -> (u16, Value) {
    call(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": named }),
    )
    .await
}

/// The stored username `named` resolves to, requiring the read to be served.
async fn username_of(router: &MultiServiceRouter, user_pool_id: &str, named: &str) -> String {
    let (status, payload) = read(router, user_pool_id, named).await;
    assert_eq!(
        status, 200,
        "{named:?} must resolve a stored user, got {status} with {payload}"
    );
    payload["Username"]
        .as_str()
        .unwrap_or_else(|| panic!("a read names the stored user, got {payload}"))
        .to_string()
}

/// An access token for `username`, taken from the administrative password flow.
async fn access_token(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    username: &str,
) -> String {
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
        .unwrap_or_else(|| panic!("a sign-in issues an access token, got {signed_in}"))
        .to_string()
}

/// One row of the alias-claim table on `AdminCreateUser`: how the pool is
/// configured, the attribute the claimed value belongs to, and whether the
/// create asks for the migration.
struct ClaimRow {
    case: &'static str,
    /// The pool's own configuration, as `CreateUserPool` takes it.
    configuration: Value,
    /// The attribute whose value both users submit.
    attribute: &'static str,
    /// The flag that vouches for that attribute's value, where it has one.
    flag: Option<&'static str>,
    /// The value both users submit for it.
    value: &'static str,
    /// Whether the pool signs its users in BY that value, so the value is what
    /// each create submits as its `Username` and the pool mints the username.
    signs_in_by_value: bool,
    /// What the second create asks for.
    force: bool,
    /// Whether the alias moves to the second account.
    migrates: bool,
}

/// The rows span the class the rule generalises over: both attributes whose
/// verified flag makes the value an alias (`email`, `phone_number`), each with
/// and without the request asking for the migration; the alias attribute that
/// carries no such flag (`preferred_username`); and a pool that signs its users
/// in BY the value (`UsernameAttributes`), where the value is the other user's
/// own sign-in name.
fn claim_rows() -> Vec<ClaimRow> {
    vec![
        ClaimRow {
            case: "a verified email alias, with the migration asked for",
            configuration: json!({ "AliasAttributes": ["email"] }),
            attribute: "email",
            flag: Some("email_verified"),
            value: "shared@example.test",
            signs_in_by_value: false,
            force: true,
            migrates: true,
        },
        ClaimRow {
            case: "a verified email alias, with the migration not asked for",
            configuration: json!({ "AliasAttributes": ["email"] }),
            attribute: "email",
            flag: Some("email_verified"),
            value: "shared@example.test",
            signs_in_by_value: false,
            force: false,
            migrates: false,
        },
        ClaimRow {
            case: "a verified phone alias, with the migration asked for",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            attribute: "phone_number",
            flag: Some("phone_number_verified"),
            value: "+15550100099",
            signs_in_by_value: false,
            force: true,
            migrates: true,
        },
        ClaimRow {
            case: "a verified phone alias, with the migration not asked for",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            attribute: "phone_number",
            flag: Some("phone_number_verified"),
            value: "+15550100099",
            signs_in_by_value: false,
            force: false,
            migrates: false,
        },
        ClaimRow {
            case: "a preferred_username alias, which carries no flag to take away",
            configuration: json!({ "AliasAttributes": ["preferred_username"] }),
            attribute: "preferred_username",
            flag: None,
            value: "shared-name",
            signs_in_by_value: false,
            force: true,
            migrates: false,
        },
        ClaimRow {
            case: "the value a pool signs its users in BY, which is a name rather than an alias",
            configuration: json!({ "UsernameAttributes": ["email"] }),
            attribute: "email",
            flag: Some("email_verified"),
            value: "signs-in@example.test",
            signs_in_by_value: true,
            force: true,
            migrates: false,
        },
    ]
}

#[tokio::test]
async fn a_held_alias_value_moves_to_a_create_that_asks_for_it_and_is_otherwise_refused() {
    let router = router();

    for (index, row) in claim_rows().into_iter().enumerate() {
        let case = row.case;
        // One pool per row, so no row sees another's users.
        let user_pool_id = pool(&router, &format!("alias-claim-{index}"), row.configuration).await;

        // The first account holds the value, vouched for where the attribute
        // carries a flag — which is what makes the value its alias.
        let mut held: Vec<(&str, &str)> = vec![(row.attribute, row.value)];
        if let Some(flag) = row.flag {
            held.push((flag, "true"));
        }
        // A pool that signs its users in by the value takes that value as the
        // `Username`; everywhere else the name is the user's own.
        let first_name = if row.signs_in_by_value {
            row.value.to_string()
        } else {
            format!("first-{index}")
        };
        let first_user = user(&router, &user_pool_id, &first_name, &held).await;
        let first_username = username_of(&router, &user_pool_id, row.value).await;

        // The second create submits the same value, asking for the migration or
        // not.
        let second_name = if row.signs_in_by_value {
            row.value.to_string()
        } else {
            format!("second-{index}")
        };
        let (status, payload) = call(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": second_name,
                "UserAttributes": attribute_list(&held),
                "ForceAliasCreation": row.force,
            }),
        )
        .await;

        if !row.migrates {
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, "UsernameExistsException", USERNAME_EXISTS_MESSAGE),
                "case {case:?}: the duplicate must be refused, got {status} with {payload}"
            );
            // The refused create stored nothing: the value still names the first
            // account, which still holds the flag that vouches for it.
            let still = username_of(&router, &user_pool_id, row.value).await;
            assert_eq!(
                still, first_username,
                "case {case:?}: the value must still name the first account, got {still:?}"
            );
            if let Some(flag) = row.flag {
                let (_, first) = read(&router, &user_pool_id, &first_username).await;
                let vouched = stored_attribute_of(&first, flag);
                assert_eq!(
                    vouched.as_deref(),
                    Some("true"),
                    "case {case:?}: the first account keeps its flag, got {vouched:?}"
                );
            }
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: the create must be served, got {status} with {payload}"
        );
        let second_username = payload["User"]["Username"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the answer names the stored user: {payload}"))
            .to_string();
        assert_ne!(
            second_username, first_username,
            "case {case:?}: the create stored a second account, got one"
        );

        // The alias is the new account's: the value resolves to it, and its own
        // flag vouches for the value.
        let owner = username_of(&router, &user_pool_id, row.value).await;
        assert_eq!(
            owner, second_username,
            "case {case:?}: the migrated value must name the new account, got {owner:?}"
        );
        let (_, second) = read(&router, &user_pool_id, &second_username).await;
        assert_eq!(
            stored_attribute_of(&second, row.attribute).as_deref(),
            Some(row.value),
            "case {case:?}: the new account holds the value, got {second}"
        );
        let flag = row.flag.expect("a migrated alias carries a verified flag");
        assert_eq!(
            stored_attribute_of(&second, flag).as_deref(),
            Some("true"),
            "case {case:?}: the new account's flag vouches for the value, got {second}"
        );

        // The previous holder keeps the VALUE and loses the flag that made it an
        // alias, so it no longer answers to it — and it is still readable under
        // its own name, with its own id.
        let (status, previous) = read(&router, &user_pool_id, &first_username).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the previous holder stays readable, got {status} with {previous}"
        );
        assert_eq!(
            stored_attribute_of(&previous, row.attribute).as_deref(),
            Some(row.value),
            "case {case:?}: the previous holder keeps its value, got {previous}"
        );
        assert_eq!(
            stored_attribute_of(&previous, flag).as_deref(),
            Some("false"),
            "case {case:?}: the previous holder's flag is demoted, got {previous}"
        );
        assert_eq!(
            stored_attribute_of(&previous, "sub").as_deref(),
            Some(first_user.as_str()),
            "case {case:?}: the previous holder keeps its own id, got {previous}"
        );
    }
}

/// One row of the form table: the attribute, a value that is not of its form,
/// and the sentence the refusal carries.
const MALFORMED_ROWS: [(&str, &str, &str, &str); 4] = [
    (
        "a phone number with no country calling code",
        "phone_number",
        "5550100099",
        INVALID_PHONE_MESSAGE,
    ),
    (
        "a phone number carrying letters",
        "phone_number",
        "+1555letters",
        INVALID_PHONE_MESSAGE,
    ),
    (
        "an email address with no domain",
        "email",
        "shared@",
        INVALID_EMAIL_MESSAGE,
    ),
    (
        "an email address with no at sign",
        "email",
        "shared.example.test",
        INVALID_EMAIL_MESSAGE,
    ),
];

#[tokio::test]
async fn asking_for_the_migration_does_not_bypass_the_form_an_attribute_value_must_have() {
    let router = router();
    let user_pool_id = pool(
        &router,
        "forced-form",
        json!({ "AliasAttributes": ["email", "phone_number"] }),
    )
    .await;

    for (index, (case, attribute, value, expected)) in MALFORMED_ROWS.into_iter().enumerate() {
        let username = format!("malformed-{index}");
        let (status, payload) = call(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "UserAttributes": attribute_list(&[(attribute, value)]),
                "ForceAliasCreation": true,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, "InvalidParameterException", expected),
            "case {case:?}: the form is judged whatever the request asks for, got {status} with \
             {payload}"
        );
        let (status, payload) = read(&router, &user_pool_id, &username).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, "UserNotFoundException"),
            "case {case:?}: the refused create stored no user, got {status} with {payload}"
        );
    }
}

/// How the value a `preferred_username` write submits is already another
/// account's, or that it is nobody's.
#[derive(Clone, Copy, Debug)]
enum Claimed {
    /// It is another account's stored username.
    OtherUsername,
    /// It is another account's `preferred_username`.
    OtherPreferredUsername,
    /// It is the id another account carries as its `sub`.
    OtherSub,
    /// It is the writer's OWN username, so no second account answers to it.
    OwnUsername,
    /// No account answers to it at all.
    Nobody,
}

/// The rows span every way the pool already resolves the submitted value to
/// ANOTHER account — the three names one of its users answers to or is
/// identified by — beside the two values that take nothing: the writer's own
/// name and a free value.
const PREFERRED_USERNAME_ROWS: [(&str, Claimed, bool); 5] = [
    (
        "a value that is another account's username",
        Claimed::OtherUsername,
        true,
    ),
    (
        "a value that is another account's preferred_username",
        Claimed::OtherPreferredUsername,
        true,
    ),
    (
        "a value that is another account's own id",
        Claimed::OtherSub,
        true,
    ),
    (
        "the writer's own username, which no second account answers to",
        Claimed::OwnUsername,
        false,
    ),
    (
        "a value no account answers to",
        Claimed::Nobody,
        false,
    ),
];

#[tokio::test]
async fn a_preferred_username_another_account_answers_to_is_refused_for_either_writer() {
    let router = router();

    // Both attribute writers are held to the one rule: the administrative write
    // and the self-service one.
    for administrative in [true, false] {
        for (index, (case, claimed, refused)) in PREFERRED_USERNAME_ROWS.into_iter().enumerate() {
            let case = format!(
                "{case} ({} writer)",
                if administrative {
                    "administrative"
                } else {
                    "self-service"
                }
            );
            // One pool per row, so no row sees another's users.
            let user_pool_id = pool(
                &router,
                &format!("preferred-{administrative}-{index}"),
                json!({ "AliasAttributes": ["preferred_username"] }),
            )
            .await;
            let client_id = client(&router, &user_pool_id).await;

            let other_sub = user(&router, &user_pool_id, "other-account", &[]).await;
            ok(
                &router,
                "AdminUpdateUserAttributes",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": "other-account",
                    "UserAttributes": attribute_list(&[("preferred_username", "other-chosen")]),
                }),
            )
            .await;
            let writer = "writing-account";
            user(&router, &user_pool_id, writer, &[]).await;

            let submitted = match claimed {
                Claimed::OtherUsername => "other-account".to_string(),
                Claimed::OtherPreferredUsername => "other-chosen".to_string(),
                Claimed::OtherSub => other_sub.clone(),
                Claimed::OwnUsername => writer.to_string(),
                Claimed::Nobody => "nobody-answers-to-this".to_string(),
            };

            let (status, payload) = if administrative {
                call(
                    &router,
                    "AdminUpdateUserAttributes",
                    &json!({
                        "UserPoolId": user_pool_id,
                        "Username": writer,
                        "UserAttributes": attribute_list(&[
                            ("preferred_username", submitted.as_str()),
                        ]),
                    }),
                )
                .await
            } else {
                let token = access_token(&router, &user_pool_id, &client_id, writer).await;
                call(
                    &router,
                    "UpdateUserAttributes",
                    &json!({
                        "AccessToken": token,
                        "UserAttributes": attribute_list(&[
                            ("preferred_username", submitted.as_str()),
                        ]),
                    }),
                )
                .await
            };

            let (_, stored) = read(&router, &user_pool_id, writer).await;
            let written = stored_attribute_of(&stored, "preferred_username");
            if refused {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (400, "AliasExistsException", ALIAS_EXISTS_MESSAGE),
                    "case {case:?}: the taken alias must be refused, got {status} with {payload}"
                );
                assert_eq!(
                    written, None,
                    "case {case:?}: the refused write stored nothing, got {written:?}"
                );
                continue;
            }
            assert_eq!(
                status, 200,
                "case {case:?}: the write must be served, got {status} with {payload}"
            );
            assert_eq!(
                written.as_deref(),
                Some(submitted.as_str()),
                "case {case:?}: the served write stored the name, got {written:?}"
            );
        }
    }
}

/// One row of the registration table: the pool's configuration, the value the
/// registration carries for `preferred_username`, and whether it is refused.
struct RegistrationRow {
    case: &'static str,
    /// The pool's own configuration, as `CreateUserPool` takes it.
    configuration: Value,
    /// What the registration submits for `preferred_username`.
    submitted: &'static str,
    /// Whether the registration is refused.
    refused: bool,
}

/// The rows span the pool configurations the rule turns on (the attribute
/// aliased, another attribute aliased, nothing aliased) and the value the
/// registration carries for it (a name, and the blank value that stores
/// nothing).
fn registration_rows() -> Vec<RegistrationRow> {
    vec![
        RegistrationRow {
            case: "a pool that aliases preferred_username, carrying a value for it",
            configuration: json!({ "AliasAttributes": ["preferred_username"] }),
            submitted: "chosen-name",
            refused: true,
        },
        RegistrationRow {
            case: "a pool that aliases preferred_username, carrying a blank value, which stores \
                   nothing",
            configuration: json!({ "AliasAttributes": ["preferred_username"] }),
            submitted: "",
            refused: false,
        },
        RegistrationRow {
            case: "a pool that aliases email instead",
            configuration: json!({ "AliasAttributes": ["email"] }),
            submitted: "chosen-name",
            refused: false,
        },
        RegistrationRow {
            case: "a pool that aliases nothing",
            configuration: json!({}),
            submitted: "chosen-name",
            refused: false,
        },
    ]
}

#[tokio::test]
async fn a_registration_may_not_carry_a_preferred_username_a_pool_aliases() {
    let router = router();

    for (index, row) in registration_rows().into_iter().enumerate() {
        let case = row.case;
        let submitted = row.submitted;
        let user_pool_id = pool(&router, &format!("registration-{index}"), row.configuration).await;
        let client_id = client(&router, &user_pool_id).await;
        let username = format!("registrant-{index}");

        let (status, payload) = call(
            &router,
            "SignUp",
            &json!({
                "ClientId": client_id,
                "Username": username,
                "Password": GOOD_PASSWORD,
                "UserAttributes": attribute_list(&[("preferred_username", submitted)]),
            }),
        )
        .await;

        if row.refused {
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (
                    400,
                    "InvalidParameterException",
                    SIGN_UP_PREFERRED_USERNAME_MESSAGE
                ),
                "case {case:?}: the registration must be refused, got {status} with {payload}"
            );
            // A refused registration creates nobody.
            let (status, payload) = read(&router, &user_pool_id, &username).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "UserNotFoundException"),
                "case {case:?}: the refused registration stored no user, got {status} with \
                 {payload}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: the registration must be served, got {status} with {payload}"
        );
        let (_, stored) = read(&router, &user_pool_id, &username).await;
        let written = stored_attribute_of(&stored, "preferred_username");
        let expected = (!submitted.is_empty()).then_some(submitted);
        assert_eq!(
            written.as_deref(),
            expected,
            "case {case:?}: the registration stored what it carried, got {written:?}"
        );
    }
}
