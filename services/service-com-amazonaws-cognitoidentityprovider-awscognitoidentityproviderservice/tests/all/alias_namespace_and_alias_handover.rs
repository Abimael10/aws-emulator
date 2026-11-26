// SPDX-License-Identifier: CC0-1.0

//! The two rules a pool that ALIASES an attribute holds every registration to, over
//! the wire (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! * The NAMESPACE rule: in a pool whose `AliasAttributes` name an attribute whose
//!   values have a format of their own, a submitted `Username` of that format is no
//!   username of the pool at all — it would shadow the alias of whichever user holds
//!   that value — so every operation that stores a user UNDER the submitted name
//!   refuses it as a bad parameter, whether the registration is self-service
//!   (`SignUp`) or administrative (`AdminCreateUser`), and stores nobody: the name
//!   still resolves no user afterwards. A pool that aliases an attribute whose values
//!   carry no format of their own (`preferred_username`), a pool that aliases nothing,
//!   and a pool that signs its users in BY the attribute (`UsernameAttributes`, where
//!   the submitted value IS that attribute's value rather than a name shadowing it)
//!   admit the name.
//! * The HANDOVER rule: an alias value names ONE user of the pool, and a create that
//!   asks for the migration takes the value from its previous holder — "if this
//!   parameter is set to `True` and the phone number or email address specified in the
//!   `UserAttributes` parameter already exists as an alias with a different user, the
//!   API call will migrate the alias from the previous user to the newly created user.
//!   The previous user will no longer be able to log in using that alias"
//!   (`AdminCreateUser.ForceAliasCreation`) — so the create is SERVED, a lookup by the
//!   value answers with the new account, and the previous holder loses the flag that
//!   vouched for the value it kept. The handover is per VALUE: another alias the
//!   previous holder still has a vouched-for value for stays its own.
//!
//! The rows span what each rule generalises over rather than one recorded case: the
//! alias configurations (`AliasAttributeType` is `phone_number`, `email`,
//! `preferred_username` — `smithy_query(query="shape",shape="AliasAttributeType")`),
//! the format of the submitted name, the registration path, and which of two aliased
//! attributes the handover moves.
//!
//! Every pool, user and alias here is real: they come from `CreateUserPool`,
//! `CreateUserPoolClient`, `SignUp` and `AdminCreateUser`, and every write is read back
//! through `AdminGetUser`. Each assertion names its case and reports the value it saw.
//!
//! The wire harness (the router, the one call, the pieces of an answer) is the sibling
//! suite's `crate::error_wire`, so this suite drives the same endpoint rather than a
//! second copy of it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_wire::{call, error_code, ok, router, REFUSED};

/// A password satisfying a default pool's password policy, so a registration is never
/// refused for the credential it carries.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The error class a request refused for the NAME it submitted carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The error class a lookup that resolves no user of the pool carries.
const USER_NOT_FOUND: &str = "UserNotFoundException";

/// The sentence a lookup that resolves no user carries: fixed text with no variable
/// part.
const USER_NOT_FOUND_MESSAGE: &str = "User does not exist.";

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("Message").and_then(Value::as_str))
        .unwrap_or("<no message>")
}

/// How the `x-amzn-errortype` header names the error class `class`: the shape's simple
/// name followed by a colon, which is what this endpoint answers every refusal with.
fn errortype_of(class: &str) -> String {
    format!("{class}:")
}

/// A pool named `name` with `configuration` merged into its create, returned as its
/// minted `UserPoolId`.
async fn pool(router: &MultiServiceRouter, name: &str, configuration: &Value) -> String {
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

/// An app client of `user_pool_id`, returned as its minted `ClientId`.
async fn client(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "alias-namespace-client",
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

/// Which registration path stores the user under the submitted name.
#[derive(Clone, Copy, Debug)]
enum Path {
    /// `SignUp`, the self-service registration.
    SelfService,
    /// `AdminCreateUser`, the administrative one.
    Administrative,
}

impl Path {
    /// The operation this path registers through.
    fn operation(self) -> &'static str {
        match self {
            Path::SelfService => "SignUp",
            Path::Administrative => "AdminCreateUser",
        }
    }
}

/// One row of the namespace table: how the pool is configured, the name the
/// registration submits, and the format its refusal names where the pool refuses it.
struct NameRow {
    case: &'static str,
    /// The pool's own configuration, as `CreateUserPool` takes it.
    configuration: Value,
    /// The `Username` the registration submits.
    submitted: &'static str,
    /// The format the refusal names (`email`, `phone number`), or `None` where the
    /// pool admits the name.
    refused_format: Option<&'static str>,
}

/// The rows span each alias configuration against a submitted name of each format: an
/// aliased attribute whose values have a format of their own (`email`, `phone_number`,
/// each aliased alone and together), an aliased attribute whose values have none
/// (`preferred_username`), a pool that aliases nothing, and a pool that signs its users
/// in BY the attribute (`UsernameAttributes`), where the submitted value IS that
/// attribute's value. The attribute catalog is `AliasAttributeType`'s own values
/// (`smithy_query(query="shape",shape="AliasAttributeType")`).
fn name_rows() -> Vec<NameRow> {
    vec![
        NameRow {
            case: "an address in a pool that aliases email",
            configuration: json!({ "AliasAttributes": ["email"] }),
            submitted: "grace@hopper.example",
            refused_format: Some("email"),
        },
        NameRow {
            case: "a plain name in a pool that aliases email",
            configuration: json!({ "AliasAttributes": ["email"] }),
            submitted: "grace",
            refused_format: None,
        },
        NameRow {
            case: "a number in a pool that aliases phone_number",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            submitted: "+15550100099",
            refused_format: Some("phone number"),
        },
        NameRow {
            case: "an address in a pool that aliases phone_number only",
            configuration: json!({ "AliasAttributes": ["phone_number"] }),
            submitted: "grace@hopper.example",
            refused_format: None,
        },
        NameRow {
            case: "an address in a pool that aliases both",
            configuration: json!({ "AliasAttributes": ["email", "phone_number"] }),
            submitted: "grace@hopper.example",
            refused_format: Some("email"),
        },
        NameRow {
            case: "a number in a pool that aliases both",
            configuration: json!({ "AliasAttributes": ["email", "phone_number"] }),
            submitted: "+15550100099",
            refused_format: Some("phone number"),
        },
        NameRow {
            case: "an address in a pool that aliases preferred_username, which has no format",
            configuration: json!({ "AliasAttributes": ["preferred_username"] }),
            submitted: "grace@hopper.example",
            refused_format: None,
        },
        NameRow {
            case: "an address in a pool that aliases nothing",
            configuration: json!({}),
            submitted: "grace@hopper.example",
            refused_format: None,
        },
        NameRow {
            case: "an address in a pool that signs its users in by email",
            configuration: json!({ "UsernameAttributes": ["email"] }),
            submitted: "grace@hopper.example",
            refused_format: None,
        },
    ]
}

/// The sentence a name of `format`'s format is refused with in a pool that aliases the
/// attribute of that format: fixed text built from the POOL's configuration, so it
/// echoes nothing the caller sent.
fn alias_namespace_refusal(format: &str) -> String {
    format!(
        "Username cannot be of {format} format, since user pool is configured for {format} alias."
    )
}

#[tokio::test]
async fn a_name_of_an_aliased_attributes_format_is_refused_by_every_registration_path() {
    let router = router();

    for path in [Path::SelfService, Path::Administrative] {
        for (index, row) in name_rows().into_iter().enumerate() {
            let case = format!("{} via {}", row.case, path.operation());
            // One pool per row, so no row sees another's users.
            let user_pool_id = pool(
                &router,
                &format!("namespace-{}-{index}", path.operation()),
                &row.configuration,
            )
            .await;
            let client_id = client(&router, &user_pool_id).await;

            let body = match path {
                Path::SelfService => json!({
                    "ClientId": client_id,
                    "Username": row.submitted,
                    "Password": GOOD_PASSWORD,
                }),
                Path::Administrative => json!({
                    "UserPoolId": user_pool_id.clone(),
                    "Username": row.submitted,
                }),
            };
            let answered = call(&router, path.operation(), &body).await;

            // Whether the name was admitted is read back through the pool: a refused
            // registration stored nobody, so the name resolves no user, while a served
            // one stored the account the lookup then reports.
            let read = call(
                &router,
                "AdminGetUser",
                &json!({ "UserPoolId": user_pool_id.clone(), "Username": row.submitted }),
            )
            .await;

            match row.refused_format {
                Some(format) => {
                    let expected = alias_namespace_refusal(format);
                    assert_eq!(
                        (
                            answered.status,
                            error_code(&answered.payload),
                            error_message(&answered.payload),
                            answered.errortype.as_deref(),
                        ),
                        (
                            REFUSED,
                            INVALID_PARAMETER,
                            expected.as_str(),
                            Some(errortype_of(INVALID_PARAMETER).as_str()),
                        ),
                        "case {case:?}: the registration must be refused for the name it \
                         submitted, got {} with {}",
                        answered.status,
                        answered.payload
                    );
                    assert_eq!(
                        (
                            read.status,
                            error_code(&read.payload),
                            error_message(&read.payload),
                            read.errortype.as_deref(),
                        ),
                        (
                            REFUSED,
                            USER_NOT_FOUND,
                            USER_NOT_FOUND_MESSAGE,
                            Some(errortype_of(USER_NOT_FOUND).as_str()),
                        ),
                        "case {case:?}: the refused registration stored no user, got {} with {}",
                        read.status,
                        read.payload
                    );
                }
                None => {
                    assert_eq!(
                        answered.status, 200,
                        "case {case:?}: the pool admits the name, so the registration is \
                         served, got {} with {}",
                        answered.status, answered.payload
                    );
                    assert_eq!(
                        read.status, 200,
                        "case {case:?}: the served registration stored the user the name \
                         resolves, got {} with {}",
                        read.status, read.payload
                    );
                    assert!(
                        read.payload["Username"].is_string(),
                        "case {case:?}: the read names the stored user, got {}",
                        read.payload
                    );
                }
            }
        }
    }
}

/// One row of the handover table: which aliased attribute the second create claims,
/// and which one the previous holder keeps.
struct HandoverRow {
    case: &'static str,
    /// The attribute whose value moves, with the flag that vouches for it and the
    /// value itself.
    moved: (&'static str, &'static str, &'static str),
    /// The other aliased attribute, which this create says nothing about, with its own
    /// flag and value.
    kept: (&'static str, &'static str, &'static str),
}

/// The rows span the two alias attributes whose verified flag is what makes their value
/// an alias (`email`, `phone_number`), each once as the value that moves and once as
/// the value the previous holder keeps.
const HANDOVER_ROWS: [HandoverRow; 2] = [
    HandoverRow {
        case: "the email alias moves and the phone alias stays",
        moved: ("email", "email_verified", "handover@example.test"),
        kept: (
            "phone_number",
            "phone_number_verified",
            "+15550100077",
        ),
    },
    HandoverRow {
        case: "the phone alias moves and the email alias stays",
        moved: (
            "phone_number",
            "phone_number_verified",
            "+15550100088",
        ),
        kept: ("email", "email_verified", "stays@example.test"),
    },
];

#[tokio::test]
async fn a_forced_create_takes_the_claimed_alias_and_leaves_the_holders_other_alias() {
    let router = router();

    for (index, row) in HANDOVER_ROWS.into_iter().enumerate() {
        let case = row.case;
        let (moved_attribute, moved_flag, moved_value) = row.moved;
        let (kept_attribute, kept_flag, kept_value) = row.kept;
        // One pool per row, so no row sees another's users.
        let user_pool_id = pool(
            &router,
            &format!("handover-{index}"),
            &json!({ "AliasAttributes": ["email", "phone_number"] }),
        )
        .await;

        // The first account holds BOTH aliases, each vouched for by its own flag.
        let first_name = format!("first-holder-{index}");
        let first = ok(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id.clone(),
                "Username": first_name.clone(),
                "UserAttributes": attribute_list(&[
                    (moved_attribute, moved_value),
                    (moved_flag, "true"),
                    (kept_attribute, kept_value),
                    (kept_flag, "true"),
                ]),
            }),
        )
        .await;
        let first_sub = stored_attribute_of(&first["User"], "sub")
            .unwrap_or_else(|| panic!("case {case:?}: a created user carries its sub: {first}"));

        // The second create claims ONE of those values, asking for the migration.
        let second_name = format!("second-holder-{index}");
        let answered = call(
            &router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id.clone(),
                "Username": second_name.clone(),
                "UserAttributes": attribute_list(&[
                    (moved_attribute, moved_value),
                    (moved_flag, "true"),
                ]),
                "ForceAliasCreation": true,
            }),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the create that asks for the migration must be served, got {} with {}",
            answered.status, answered.payload
        );
        let created = &answered.payload["User"];
        assert_eq!(
            created["Username"].as_str(),
            Some(second_name.as_str()),
            "case {case:?}: the answer names the account it stored, got {created}"
        );
        assert_eq!(
            created["Enabled"],
            json!(true),
            "case {case:?}: the stored account is activated for sign-in, got {created}"
        );
        assert_eq!(
            created["UserStatus"],
            json!("FORCE_CHANGE_PASSWORD"),
            "case {case:?}: an administrator-created account must change its password, \
             got {created}"
        );
        assert_eq!(
            stored_attribute_of(created, moved_attribute).as_deref(),
            Some(moved_value),
            "case {case:?}: the new account holds the claimed value, got {created}"
        );
        assert_eq!(
            stored_attribute_of(created, moved_flag).as_deref(),
            Some("true"),
            "case {case:?}: the new account's flag vouches for the claimed value, got {created}"
        );
        let second_sub = stored_attribute_of(created, "sub")
            .unwrap_or_else(|| panic!("case {case:?}: the stored account carries a sub: {created}"));
        assert_ne!(
            second_sub, first_sub,
            "case {case:?}: the create stored a second account with its own id, got {second_sub:?}"
        );

        // The claimed value names the NEW account now, with that account's own id.
        let by_moved = call(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": user_pool_id.clone(), "Username": moved_value }),
        )
        .await;
        assert_eq!(
            by_moved.status, 200,
            "case {case:?}: the migrated value still resolves a user, got {} with {}",
            by_moved.status, by_moved.payload
        );
        assert_eq!(
            by_moved.payload["Username"].as_str(),
            Some(second_name.as_str()),
            "case {case:?}: the migrated value must name the new account, got {}",
            by_moved.payload
        );
        assert_eq!(
            stored_attribute_of(&by_moved.payload, "sub").as_deref(),
            Some(second_sub.as_str()),
            "case {case:?}: the lookup reports the new account's own id, got {}",
            by_moved.payload
        );

        // The value this create said nothing about is still the first account's.
        let by_kept = call(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": user_pool_id.clone(), "Username": kept_value }),
        )
        .await;
        assert_eq!(
            by_kept.payload["Username"].as_str(),
            Some(first_name.as_str()),
            "case {case:?}: the untouched alias must still name the first account, got {}",
            by_kept.payload
        );

        // The previous holder keeps the value it lost the ALIAS to and loses the flag
        // that vouched for it, keeps the other alias vouched for, and keeps its own id.
        let previous = call(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": user_pool_id.clone(), "Username": first_name.clone() }),
        )
        .await;
        assert_eq!(
            previous.status, 200,
            "case {case:?}: the previous holder stays readable, got {} with {}",
            previous.status, previous.payload
        );
        assert_eq!(
            stored_attribute_of(&previous.payload, moved_attribute).as_deref(),
            Some(moved_value),
            "case {case:?}: the previous holder keeps the value it lost, got {}",
            previous.payload
        );
        assert_eq!(
            stored_attribute_of(&previous.payload, moved_flag).as_deref(),
            Some("false"),
            "case {case:?}: the previous holder's flag no longer vouches for it, got {}",
            previous.payload
        );
        assert_eq!(
            stored_attribute_of(&previous.payload, kept_flag).as_deref(),
            Some("true"),
            "case {case:?}: the previous holder's other alias is untouched, got {}",
            previous.payload
        );
        assert_eq!(
            stored_attribute_of(&previous.payload, "sub").as_deref(),
            Some(first_sub.as_str()),
            "case {case:?}: the previous holder keeps its own id, got {}",
            previous.payload
        );
    }
}
