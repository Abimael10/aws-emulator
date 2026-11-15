// SPDX-License-Identifier: CC0-1.0

//! `UpdateUser`: what an update writes to a stored user, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the route, the status, and a body the handler computed from the shared store.
//!
//! The rule under test: an update writes the members it carries onto the user held at
//! `(AwsAccountId, Namespace, UserName)` and reports that user as stored — the email and
//! the role it always carries, the custom permissions profile it applies or removes, and
//! the external login it sets or clears — leaving what it does not name as it was, so a
//! later `DescribeUser` answers from the updated record. A request whose parameters do
//! not go together is refused and writes nothing.
//!
//! So the rows below vary what the rule generalises over — the role, the email, the
//! identity type the user was registered with, which optional members the update
//! carries, and the external login provider — rather than re-running one recorded call.

use serde_json::{json, Value};

use super::data_source_data_set_lifecycle::Harness;
use super::group_lifecycle::group_label as identity_label;
use super::user_lifecycle::{describe_user, register_user, sign_up_for_quicksight_identities};

/// The account every case uses.
const ACCOUNT: &str = "000000000000";
/// The namespace every QuickSight account already has.
const DEFAULT_NAMESPACE: &str = "default";
/// The URL `RegisterUser` and `UpdateUser` document for the `COGNITO` provider type.
/// Source: smithy_query(query="doc", shape="UpdateUserRequest",
/// member="ExternalLoginFederationProviderType").
const COGNITO_PROVIDER_URL: &str = "cognito-identity.amazonaws.com";

/// The members of the model's `UserRole` enum an update can assign: every member except
/// the two the model documents as "This role isn't currently available for use".
/// Source: smithy_query(query="shape", shape="UserRole") and
/// smithy_query(query="doc", shape="User", member="Role").
const ASSIGNABLE_ROLES: [&str; 6] = [
    "READER",
    "AUTHOR",
    "ADMIN",
    "READER_PRO",
    "AUTHOR_PRO",
    "ADMIN_PRO",
];

/// The two `UserRole` members the model documents as unavailable for use.
/// Source: smithy_query(query="doc", shape="User", member="Role").
const UNAVAILABLE_ROLES: [&str; 2] = ["RESTRICTED_READER", "RESTRICTED_AUTHOR"];

/// `UpdateUser`:
/// `PUT /accounts/{AwsAccountId}/namespaces/{Namespace}/users/{UserName}`.
/// Source: smithy_query(query="operation", operation="UpdateUser").
async fn update_user(
    harness: &Harness,
    account: &str,
    namespace: &str,
    user_name: &str,
    body: Value,
) -> (u16, Value) {
    harness
        .call(
            "PUT",
            &format!(
                "/accounts/{account}/namespaces/{namespace}/users/{}",
                identity_label(user_name)
            ),
            Some(body),
        )
        .await
}

/// Register the user `case` names, by the body the row carries, and answer the name it
/// is known by.
async fn registered(harness: &Harness, case: &str, body: Value, user_name: &str) {
    let (status, response) = register_user(harness, ACCOUNT, DEFAULT_NAMESPACE, body).await;
    assert_eq!(
        status, 201,
        "case {case:?}: the fixture registration expected 201, got {status} with {response}"
    );
    assert_eq!(
        response["User"]["UserName"],
        json!(user_name),
        "case {case:?}: the registration must store the name the row expects: {response}"
    );
}

/// A registration of a user QuickSight manages itself, named `user_name`.
fn quicksight_registration(user_name: &str, extra: Value) -> Value {
    let mut body = json!({
        "IdentityType": "QUICKSIGHT",
        "Email": format!("{user_name}@example.com"),
        "UserRole": "READER",
        "UserName": user_name,
    });
    for (member, value) in extra.as_object().expect("an object of extra members") {
        body[member] = value.clone();
    }
    body
}

/// A registration of a user that federates through an IAM principal of `user_name`.
fn iam_registration(user_name: &str, extra: Value) -> Value {
    let mut body = json!({
        "IdentityType": "IAM",
        "Email": format!("{user_name}@example.com"),
        "UserRole": "READER",
        "IamArn": format!("arn:aws:iam::{ACCOUNT}:user/{user_name}"),
    });
    for (member, value) in extra.as_object().expect("an object of extra members") {
        body[member] = value.clone();
    }
    body
}

/// One row of the update table.
struct Row {
    case: &'static str,
    /// The registration that puts the user in the store.
    registration: Value,
    /// The name the registered user is known by.
    user_name: &'static str,
    /// The update's body.
    update: Value,
    /// The members the read must report afterwards, by name.
    reported: Value,
}

/// The rows span what an update writes: the role and the email it always carries, the
/// custom permissions profile it applies or removes, and the external login it sets,
/// replaces, clears or leaves alone.
/// Source: smithy_query(query="operation", operation="UpdateUser").
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the email and the role the update always carries",
            registration: quicksight_registration("plain-user", json!({})),
            user_name: "plain-user",
            update: json!({ "Email": "moved@example.com", "Role": "ADMIN" }),
            reported: json!({
                "Email": "moved@example.com",
                "Role": "ADMIN",
                "CustomPermissionsName": Value::Null,
            }),
        },
        Row {
            case: "a custom permissions profile applied by the update",
            registration: quicksight_registration("profile-user", json!({})),
            user_name: "profile-user",
            update: json!({
                "Email": "profile-user@example.com",
                "Role": "AUTHOR",
                "CustomPermissionsName": "tight-profile",
            }),
            reported: json!({
                "Role": "AUTHOR",
                "CustomPermissionsName": "tight-profile",
            }),
        },
        Row {
            case: "a custom permissions profile the update removes",
            registration: quicksight_registration(
                "unapply-user",
                json!({ "CustomPermissionsName": "base-profile" }),
            ),
            user_name: "unapply-user",
            update: json!({
                "Email": "unapply-user@example.com",
                "Role": "READER",
                "UnapplyCustomPermissions": true,
            }),
            reported: json!({ "CustomPermissionsName": Value::Null }),
        },
        Row {
            case: "a profile the update neither names nor removes",
            registration: quicksight_registration(
                "kept-profile-user",
                json!({ "CustomPermissionsName": "base-profile" }),
            ),
            user_name: "kept-profile-user",
            update: json!({ "Email": "kept@example.com", "Role": "READER_PRO" }),
            reported: json!({
                "Role": "READER_PRO",
                "CustomPermissionsName": "base-profile",
            }),
        },
        Row {
            case: "an external login the update sets to Amazon Cognito",
            registration: iam_registration("cognito-user", json!({})),
            user_name: "cognito-user",
            update: json!({
                "Email": "cognito-user@example.com",
                "Role": "READER",
                "ExternalLoginFederationProviderType": "COGNITO",
                "ExternalLoginId": "cognito-identity-1",
            }),
            reported: json!({
                "ExternalLoginFederationProviderType": "COGNITO",
                "ExternalLoginFederationProviderUrl": COGNITO_PROVIDER_URL,
                "ExternalLoginId": "cognito-identity-1",
            }),
        },
        Row {
            case: "an external login the update moves to a custom OIDC provider",
            registration: iam_registration(
                "oidc-user",
                json!({
                    "ExternalLoginFederationProviderType": "COGNITO",
                    "ExternalLoginId": "cognito-identity-2",
                }),
            ),
            user_name: "oidc-user",
            update: json!({
                "Email": "oidc-user@example.com",
                "Role": "AUTHOR_PRO",
                "ExternalLoginFederationProviderType": "CUSTOM_OIDC",
                "CustomFederationProviderUrl": "https://oidc.example.com/.well-known",
                "ExternalLoginId": "oidc-identity-1",
            }),
            reported: json!({
                "Role": "AUTHOR_PRO",
                "ExternalLoginFederationProviderType": "CUSTOM_OIDC",
                "ExternalLoginFederationProviderUrl": "https://oidc.example.com/.well-known",
                "ExternalLoginId": "oidc-identity-1",
            }),
        },
        Row {
            case: "an external login the update clears",
            registration: iam_registration(
                "cleared-user",
                json!({
                    "ExternalLoginFederationProviderType": "COGNITO",
                    "ExternalLoginId": "cognito-identity-3",
                }),
            ),
            user_name: "cleared-user",
            update: json!({
                "Email": "cleared-user@example.com",
                "Role": "READER",
                "ExternalLoginFederationProviderType": "NONE",
            }),
            reported: json!({
                "ExternalLoginFederationProviderType": Value::Null,
                "ExternalLoginFederationProviderUrl": Value::Null,
                "ExternalLoginId": Value::Null,
            }),
        },
        Row {
            case: "an external login the update does not name at all",
            registration: iam_registration(
                "untouched-login-user",
                json!({
                    "ExternalLoginFederationProviderType": "COGNITO",
                    "ExternalLoginId": "cognito-identity-4",
                }),
            ),
            user_name: "untouched-login-user",
            update: json!({ "Email": "untouched@example.com", "Role": "ADMIN_PRO" }),
            reported: json!({
                "Role": "ADMIN_PRO",
                "ExternalLoginFederationProviderType": "COGNITO",
                "ExternalLoginFederationProviderUrl": COGNITO_PROVIDER_URL,
                "ExternalLoginId": "cognito-identity-4",
            }),
        },
    ]
}

#[tokio::test]
async fn an_update_writes_what_it_carries_and_the_read_answers_from_it() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;

    for row in rows() {
        let Row {
            case,
            registration,
            user_name,
            update,
            reported,
        } = row;
        registered(&harness, case, registration, user_name).await;

        let (status, response) =
            update_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, user_name, update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update expected 200, got {status} with {response}"
        );
        let updated = response["User"].clone();

        // The update reports the user as stored, and the read answers from that same
        // record: both carry every member the row expects.
        let (status, read) = describe_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, user_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read after the update expected 200, got {status} with {read}"
        );
        for (member, expected) in reported.as_object().expect("an object of members") {
            assert_eq!(
                &updated[member], expected,
                "case {case:?}: the update must report {member} as {expected}: {response}"
            );
            assert_eq!(
                &read["User"][member], expected,
                "case {case:?}: the read must report {member} as {expected}: {read}"
            );
        }
        // Nothing about the user's identity changes: an update is not a re-registration.
        assert_eq!(
            read["User"]["PrincipalId"], updated["PrincipalId"],
            "case {case:?}: the update must keep the user's own handle: {read}"
        );
        assert_eq!(
            read["User"]["Arn"], updated["Arn"],
            "case {case:?}: the update must keep the user's ARN: {read}"
        );
    }
}

#[tokio::test]
async fn an_update_assigns_every_role_the_model_makes_available() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    let user_name = "role-user";
    registered(
        &harness,
        "every role",
        quicksight_registration(user_name, json!({})),
        user_name,
    )
    .await;

    // Every assignable member of the enum, applied in turn to the one user: each is
    // written and read back, so the update is not keyed to a single role.
    for role in ASSIGNABLE_ROLES {
        let (status, response) = update_user(
            &harness,
            ACCOUNT,
            DEFAULT_NAMESPACE,
            user_name,
            json!({ "Email": "role-user@example.com", "Role": role }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {role:?}: the update expected 200, got {status} with {response}"
        );
        let (status, read) = describe_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, user_name).await;
        assert_eq!(
            status, 200,
            "case {role:?}: the read expected 200, got {status} with {read}"
        );
        assert_eq!(
            read["User"]["Role"],
            json!(role),
            "case {role:?}: the read must report the role the update assigned: {read}"
        );
    }
}

#[tokio::test]
async fn an_update_whose_parameters_do_not_go_together_is_refused_and_writes_nothing() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    let user_name = "refusal-user";
    registered(
        &harness,
        "refusals",
        quicksight_registration(
            user_name,
            json!({ "CustomPermissionsName": "base-profile" }),
        ),
        user_name,
    )
    .await;
    let email = format!("{user_name}@example.com");

    // Rows: each way an update's members contradict the rule, and the message the
    // refusal reads.
    let mut rows: Vec<(String, Value, String)> = vec![
        (
            "an email that is no address".to_string(),
            json!({ "Email": "not-an-address", "Role": "READER" }),
            "Email not-an-address is not a valid email address.".to_string(),
        ),
        (
            "a profile named while every custom permission is removed".to_string(),
            json!({
                "Email": email,
                "Role": "READER",
                "UnapplyCustomPermissions": true,
                "CustomPermissionsName": "tight-profile",
            }),
            "CustomPermissionsName is not used when UnapplyCustomPermissions is set."
                .to_string(),
        ),
        (
            "a custom provider URL named for Amazon Cognito".to_string(),
            json!({
                "Email": email,
                "Role": "READER",
                "ExternalLoginFederationProviderType": "COGNITO",
                "CustomFederationProviderUrl": "https://oidc.example.com",
            }),
            "CustomFederationProviderUrl is only used when \
             ExternalLoginFederationProviderType is CUSTOM_OIDC."
                .to_string(),
        ),
        (
            "a custom OIDC provider with no URL of its own".to_string(),
            json!({
                "Email": email,
                "Role": "READER",
                "ExternalLoginFederationProviderType": "CUSTOM_OIDC",
            }),
            "CustomFederationProviderUrl is required when \
             ExternalLoginFederationProviderType is CUSTOM_OIDC."
                .to_string(),
        ),
        (
            "a provider type the service does not support".to_string(),
            json!({
                "Email": email,
                "Role": "READER",
                "ExternalLoginFederationProviderType": "SAML",
            }),
            "ExternalLoginFederationProviderType SAML is not supported. Supported types \
             are COGNITO, CUSTOM_OIDC and NONE."
                .to_string(),
        ),
    ];
    // Every role the model documents as unavailable is refused, not only one of them.
    for role in UNAVAILABLE_ROLES {
        rows.push((
            format!("the unavailable role {role}"),
            json!({ "Email": email, "Role": role }),
            format!("Role {role} isn't currently available for use."),
        ));
    }

    for (case, body, expected) in rows {
        let (status, response) =
            update_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, user_name, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {response}"
        );
        assert_eq!(
            response["__type"],
            json!("InvalidParameterValueException"),
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            response["Message"],
            json!(expected),
            "case {case:?}: expected Message {expected:?}, got {response}"
        );

        // The refused update wrote nothing: the user still reports what it was
        // registered with.
        let (status, read) = describe_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, user_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read expected 200, got {status} with {read}"
        );
        assert_eq!(
            read["User"]["Role"],
            json!("READER"),
            "case {case:?}: a refused update must not change the role: {read}"
        );
        assert_eq!(
            read["User"]["Email"],
            json!(email),
            "case {case:?}: a refused update must not change the email: {read}"
        );
        assert_eq!(
            read["User"]["CustomPermissionsName"],
            json!("base-profile"),
            "case {case:?}: a refused update must not change the profile: {read}"
        );
    }
}
