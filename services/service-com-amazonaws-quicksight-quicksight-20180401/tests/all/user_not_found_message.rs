// SPDX-License-Identifier: CC0-1.0

//! What QuickSight's user path says about a `UserName` it holds no user under, end to
//! end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the status, the error code and the message the handler rendered.
//!
//! The rule under test: a user operation that finds no user at the address it was given
//! refuses with `ResourceNotFoundException` (HTTP 404) whose message is the identity
//! path's locator over the three path labels the call carried — `"AWS account id:
//! <AwsAccountId>, Namespace: <Namespace>, Username: <UserName>. "`, ending in a period
//! and a space — preceded by the sentence `"User <UserName> does not exist."`, and only
//! that sentence, when the service deleted a user at that address. One template per
//! case, whichever user operation refuses: a name with no record reports the locator
//! alone, because nothing is known to have existed to say anything else about.
//!
//! So the rows below vary what the rule generalises over — the account, the namespace,
//! the form and length of the name, the identity type the deleted user was registered
//! with, the operation that refuses, and whether the user went by name or with its
//! namespace — rather than re-running one recorded call.

use serde_json::{json, Value};

use service_com_amazonaws_quicksight_quicksight_20180401::state as qs_state;

use super::data_source_data_set_lifecycle::Harness;
use super::group_lifecycle::group_label as identity_label;
use super::namespace_lifecycle::{create_namespace_in, settle_creation};
use super::user_lifecycle::{describe_user, register_user, sign_up_for_quicksight_identities};

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, so a row shows the locator reports the
/// account the call addressed.
const OTHER_ACCOUNT: &str = "210987654321";
/// A third 12-digit account id, for a row addressing an account that holds nothing.
const THIRD_ACCOUNT: &str = "123456789012";
/// The region the harness signs for.
const REGION: &str = "us-west-2";
/// The namespace every QuickSight account already has.
const DEFAULT_NAMESPACE: &str = "default";
/// A namespace this suite creates, so a row shows the namespace travels in the locator
/// rather than a fixed one.
const CUSTOM_NAMESPACE: &str = "miss-ns";
/// A second namespace this suite creates and then deletes, for the users that go with
/// it.
const DOOMED_NAMESPACE: &str = "doomed-ns";

/// The locator a user refusal closes with: the three path labels the call carried,
/// ending in a period and a space.
///
/// `pub(crate)` because every suite that asserts a user miss renders the locator the
/// same way; a copy that drifted would hide a defect rather than show it.
pub(crate) fn locator(account: &str, namespace: &str, user_name: &str) -> String {
    format!("AWS account id: {account}, Namespace: {namespace}, Username: {user_name}. ")
}

/// What a user operation reports for a name the service holds no record of: the locator
/// alone.
fn never_registered_message(account: &str, namespace: &str, user_name: &str) -> String {
    locator(account, namespace, user_name)
}

/// What a user operation reports for a name whose user the service deleted: the
/// existence sentence, then the locator directly after it.
///
/// `pub(crate)` for the same reason [`locator`] is.
pub(crate) fn deleted_message(account: &str, namespace: &str, user_name: &str) -> String {
    format!(
        "User {user_name} does not exist.{}",
        locator(account, namespace, user_name)
    )
}

/// `DeleteUser`:
/// `DELETE /accounts/{AwsAccountId}/namespaces/{Namespace}/users/{UserName}`.
///
/// `pub(crate)` because every suite that deletes a user addresses it the same way.
/// Source: smithy_query(query="operation", operation="DeleteUser").
pub(crate) async fn delete_user(
    harness: &Harness,
    account: &str,
    namespace: &str,
    user_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "DELETE",
            &format!(
                "/accounts/{account}/namespaces/{namespace}/users/{}",
                identity_label(user_name)
            ),
            None,
        )
        .await
}

/// `DeleteNamespace`: `DELETE /accounts/{AwsAccountId}/namespaces/{Namespace}`.
///
/// `pub(crate)` for the same reason [`delete_user`] is.
/// Source: smithy_query(query="operation", operation="DeleteNamespace").
pub(crate) async fn delete_namespace(
    harness: &Harness,
    account: &str,
    namespace: &str,
) -> (u16, Value) {
    harness
        .call(
            "DELETE",
            &format!("/accounts/{account}/namespaces/{namespace}"),
            None,
        )
        .await
}

/// Create `namespace` under `account` and settle the creation the clock owes it, so it
/// is a place a user can be registered in.
async fn create_settled_namespace(harness: &Harness, account: &str, namespace: &str) {
    let (status, body) = create_namespace_in(
        harness,
        REGION,
        account,
        json!({ "Namespace": namespace, "IdentityStore": "QUICKSIGHT" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture namespace {namespace:?} in {account}: got {status} with {body}"
    );
    settle_creation(harness, account, namespace);
}

/// The three user operations keyed on a `UserName`, as the model routes them: the read,
/// the update and the delete. Each reports the same miss, so the message does not depend
/// on which one a client called.
/// Source: smithy_query(query="operation", operation="DescribeUser,UpdateUser,DeleteUser").
const USER_CALLS: [&str; 3] = ["DescribeUser", "UpdateUser", "DeleteUser"];

/// One of [`USER_CALLS`], addressed to `(account, namespace, user_name)`. The update
/// carries the two members its request requires, so the miss is what refuses it rather
/// than a missing member.
async fn call_user(
    harness: &Harness,
    operation: &str,
    account: &str,
    namespace: &str,
    user_name: &str,
) -> (u16, Value) {
    match operation {
        "DescribeUser" => describe_user(harness, account, namespace, user_name).await,
        "UpdateUser" => {
            harness
                .call(
                    "PUT",
                    &format!(
                        "/accounts/{account}/namespaces/{namespace}/users/{}",
                        identity_label(user_name)
                    ),
                    Some(json!({ "Email": "whoever@example.com", "Role": "READER" })),
                )
                .await
        }
        _ => delete_user(harness, account, namespace, user_name).await,
    }
}

/// Assert that every user operation refuses `(account, namespace, user_name)` with the
/// 404 `ResourceNotFoundException` carrying `expected`.
async fn assert_user_miss(
    harness: &Harness,
    case: &str,
    account: &str,
    namespace: &str,
    user_name: &str,
    expected: &str,
) {
    for operation in USER_CALLS {
        let (status, body) = call_user(harness, operation, account, namespace, user_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}/{operation}: expected 404 for a name no user is held under, \
             got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}/{operation}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("USER"),
            "case {case:?}/{operation}: the refusal is about a user, got {body}"
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}/{operation}: expected Message {expected:?}, got {body}"
        );
    }
}

#[tokio::test]
async fn a_name_with_no_record_is_refused_with_the_locator_alone() {
    let harness = Harness::new();
    create_settled_namespace(&harness, ACCOUNT, CUSTOM_NAMESPACE).await;
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;

    // One user IS registered, so the rows below are about names that address no user
    // rather than about an empty store.
    let held = "Held Reader";
    let (status, body) = register_user(
        &harness,
        ACCOUNT,
        DEFAULT_NAMESPACE,
        json!({
            "IdentityType": "QUICKSIGHT",
            "Email": "held@example.com",
            "UserRole": "READER",
            "UserName": held,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "fixture registration expected 201, got {status} with {body}"
    );

    // Rows: the name's form and length (`UserName` is len 1.. over the pattern
    // `^[\u0020-\u00FF]+$`, so it can hold a space or a Latin-1 letter), the namespace,
    // and the account — including a name held by another account only.
    // Source: smithy_query(query="operation", operation="DescribeUser").
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "a name nothing registered",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "no-such-user".to_string(),
        ),
        (
            "a name holding a space",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "No Such Reader".to_string(),
        ),
        (
            "a Latin-1 name the pattern admits",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Lecteur Générée".to_string(),
        ),
        (
            "the shortest name the model admits",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "a".to_string(),
        ),
        (
            "a long name",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "u".repeat(200),
        ),
        (
            "a name differing from a held one only in case",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            held.to_uppercase(),
        ),
        (
            "a held name, in a namespace that does not hold it",
            ACCOUNT,
            CUSTOM_NAMESPACE,
            held.to_string(),
        ),
        (
            "a held name, in an account that does not hold it",
            OTHER_ACCOUNT,
            DEFAULT_NAMESPACE,
            held.to_string(),
        ),
        (
            "an account holding nothing at all",
            THIRD_ACCOUNT,
            DEFAULT_NAMESPACE,
            held.to_string(),
        ),
    ];

    for (case, account, namespace, user_name) in rows {
        let expected = never_registered_message(account, namespace, &user_name);
        assert_user_miss(&harness, case, account, namespace, &user_name, &expected).await;
    }

    // None of the refusals disturbed the user that IS registered.
    let (status, body) = describe_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, held).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored user readable, got {status} with {body}"
    );
}

/// One row of the delete-then-read table: a user that is registered, then deleted.
struct DeletedRow {
    case: &'static str,
    account: &'static str,
    namespace: &'static str,
    /// The registration body, without the two path labels.
    body: Value,
    /// The name the registered user is known by: the `UserName` the request named, or
    /// the IAM principal's own name for an identity type that federates through IAM.
    user_name: String,
}

/// The rows span the identity types a user can be registered with, the forms a name
/// takes, two namespaces and two accounts — the deletion is what the rule keys on, so
/// everything else about the user varies.
/// Source: smithy_query(query="shape", shape="IdentityType").
fn deleted_rows() -> Vec<DeletedRow> {
    vec![
        DeletedRow {
            case: "a user QuickSight manages itself",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "gone@example.com",
                "UserRole": "READER",
                "UserName": "Gone Reader",
            }),
            user_name: "Gone Reader".to_string(),
        },
        DeletedRow {
            case: "a Latin-1 name the pattern admits",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "cafe@example.com",
                "UserRole": "AUTHOR",
                "UserName": "Café Lecteur",
            }),
            user_name: "Café Lecteur".to_string(),
        },
        DeletedRow {
            case: "the shortest name the model admits",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "short@example.com",
                "UserRole": "READER",
                "UserName": "a",
            }),
            user_name: "a".to_string(),
        },
        DeletedRow {
            case: "a user that federates through an IAM user",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "IAM",
                "Email": "analyst@example.com",
                "UserRole": "READER",
                "IamArn": format!("arn:aws:iam::{ACCOUNT}:user/analyst"),
            }),
            user_name: "analyst".to_string(),
        },
        DeletedRow {
            case: "a user registered under an assumed role's session name",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "IAM",
                "Email": "nightly@example.com",
                "UserRole": "AUTHOR",
                "IamArn": format!("arn:aws:iam::{ACCOUNT}:role/report-role"),
                "SessionName": "nightly",
            }),
            user_name: "report-role/nightly".to_string(),
        },
        DeletedRow {
            case: "a namespace the account created, not the default one",
            account: ACCOUNT,
            namespace: CUSTOM_NAMESPACE,
            body: json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "ns@example.com",
                "UserRole": "READER",
                "UserName": "namespaced",
            }),
            user_name: "namespaced".to_string(),
        },
        DeletedRow {
            case: "another account entirely",
            account: OTHER_ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            body: json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "elsewhere@example.com",
                "UserRole": "READER",
                "UserName": "elsewhere",
            }),
            user_name: "elsewhere".to_string(),
        },
    ]
}

#[tokio::test]
async fn a_user_this_service_deleted_is_refused_with_the_existence_sentence_too() {
    let harness = Harness::new();
    create_settled_namespace(&harness, ACCOUNT, CUSTOM_NAMESPACE).await;
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    sign_up_for_quicksight_identities(&harness, OTHER_ACCOUNT).await;

    for row in deleted_rows() {
        let DeletedRow {
            case,
            account,
            namespace,
            body,
            user_name,
        } = row;

        let (status, response) = register_user(&harness, account, namespace, body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the fixture registration expected 201, got {status} with \
             {response}"
        );
        assert_eq!(
            response["User"]["UserName"],
            json!(user_name),
            "case {case:?}: the registration must store the name the row expects: \
             {response}"
        );

        let (status, response) = delete_user(&harness, account, namespace, &user_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: deleting the user expected 200, got {status} with {response}"
        );
        assert!(
            response["RequestId"].is_string(),
            "case {case:?}: the deletion reports its request id: {response}"
        );

        // The deletion removed the record, so the read answers from nothing.
        {
            let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
            let store = state.lock();
            assert!(
                store.user(account, namespace, &user_name).is_none(),
                "case {case:?}: the deleted user is still stored"
            );
        }

        let expected = deleted_message(account, namespace, &user_name);
        assert_user_miss(&harness, case, account, namespace, &user_name, &expected).await;

        // A name the service never held is still reported with the locator alone, in the
        // very namespace a user was just deleted from: the two misses are told apart.
        let never = format!("never-{user_name}");
        let expected = never_registered_message(account, namespace, &never);
        assert_user_miss(&harness, case, account, namespace, &never, &expected).await;
    }
}

#[tokio::test]
async fn a_user_deleted_with_its_namespace_is_reported_as_deleted() {
    let harness = Harness::new();
    create_settled_namespace(&harness, ACCOUNT, DOOMED_NAMESPACE).await;
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;

    // Two users, so the namespace's deletion is shown to take every user it holds: the
    // name's form varies between them.
    let users = ["doomed", "Doomed Café"];
    for user_name in users {
        let (status, body) = register_user(
            &harness,
            ACCOUNT,
            DOOMED_NAMESPACE,
            json!({
                "IdentityType": "QUICKSIGHT",
                "Email": "doomed@example.com",
                "UserRole": "READER",
                "UserName": user_name,
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {user_name:?}: the fixture registration expected 201, got {status} \
             with {body}"
        );
    }

    let (status, body) = delete_namespace(&harness, ACCOUNT, DOOMED_NAMESPACE).await;
    assert_eq!(
        status, 200,
        "deleting the namespace expected 200, got {status} with {body}"
    );

    // A user that went with its namespace was deleted by this service, so a later read
    // of it reports a user that is gone — the same template a deletion by name gets.
    for user_name in users {
        let expected = deleted_message(ACCOUNT, DOOMED_NAMESPACE, user_name);
        assert_user_miss(
            &harness,
            user_name,
            ACCOUNT,
            DOOMED_NAMESPACE,
            user_name,
            &expected,
        )
        .await;
    }

    // And a name that namespace never held reports the locator alone.
    let expected = never_registered_message(ACCOUNT, DOOMED_NAMESPACE, "never-held");
    assert_user_miss(
        &harness,
        "never-held",
        ACCOUNT,
        DOOMED_NAMESPACE,
        "never-held",
        &expected,
    )
    .await;
}
