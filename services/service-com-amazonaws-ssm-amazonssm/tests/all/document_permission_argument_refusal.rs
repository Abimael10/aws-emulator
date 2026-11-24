// SPDX-License-Identifier: CC0-1.0

//! How `ModifyDocumentPermission` refuses a call that asks for no change.
//!
//! RULE: `AccountIdsToAdd` and `AccountIdsToRemove` are both optional
//! (`smithy_query(query="operation", operation="ModifyDocumentPermission")`), but a call
//! supplying neither names no account to share with or stop sharing with, which is a request
//! this operation cannot carry out. The fault is the PAIR rather than either member — neither
//! is outside anything the model declares — so it is refused with the class Systems Manager
//! answers an argument fault with, `InvalidParameterException`, carrying the fixed sentence
//! "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation", and
//! the sharing the document already holds is left exactly as it was.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and every assertion
//! about what the document still holds is read back through `DescribeDocumentPermission`.
//!
//! The rows differ along what the rule generalises over: whether the two members are absent
//! or present-but-empty, whether the call names a version to share, which document it is
//! about, and what that document was already shared with.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The `DocumentPermissionType` value the model declares (`smithy_query(query="shape",
/// shape="DocumentPermissionType")`: `Share`).
const SHARE: &str = "Share";

/// The class Systems Manager refuses the pair with, and the whole sentence it carries.
const ARGUMENT_REFUSAL: &str = "InvalidParameterException";

/// See [`ARGUMENT_REFUSAL`].
const NEITHER_NAMED: &str =
    "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation";

/// An account id of another account, to share a document with.
const OTHER_ACCOUNT: &str = "111122223333";

/// A second one, so a row can add two accounts and remove one of them.
const THIRD_ACCOUNT: &str = "444455556666";

/// The minimal content of a command document, so a create is served.
const CONTENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "a document the permission suite shares",
  "mainSteps": [
    {
      "action": "aws:runShellScript",
      "name": "run",
      "inputs": { "runCommand": ["echo hello"] }
    }
  ]
}"#;

/// Create a document of `name`, asserting the create succeeded.
async fn create(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({ "Name": name, "Content": CONTENT, "DocumentType": "Command" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the document {name} must be created first, got {status} with {created}"
    );
}

/// The accounts `DescribeDocumentPermission` reports `name` as shared with.
async fn shared_with(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let (status, response) = call(
        router,
        "DescribeDocumentPermission",
        &json!({ "Name": name, "PermissionType": SHARE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeDocumentPermission must be served, got {status} with {response}"
    );
    let mut reported: Vec<String> = response["AccountIds"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    reported.sort();
    reported
}

/// RULE: a call naming neither account list is refused with the argument class, and changes
/// nothing.
#[tokio::test]
async fn a_call_naming_neither_account_list_is_refused() {
    // (case, the document this row is about, what it is already shared with, the members the
    // refused call carries beyond `Name` and `PermissionType`)
    let rows: Vec<(&str, &str, Vec<&str>, Value)> = vec![
        (
            "neither member present, on a document shared with nobody",
            "Permission.Neither.Fresh",
            vec![],
            json!({}),
        ),
        (
            "neither member present, on a document already shared",
            "Permission.Neither.Shared",
            vec![OTHER_ACCOUNT],
            json!({}),
        ),
        (
            "both members present but empty",
            "Permission.Empty.Both",
            vec![OTHER_ACCOUNT],
            json!({ "AccountIdsToAdd": [], "AccountIdsToRemove": [] }),
        ),
        (
            "one member present but empty",
            "Permission.Empty.One",
            vec![],
            json!({ "AccountIdsToAdd": [] }),
        ),
        (
            "neither member present, beside the version a share would grant",
            "Permission.Neither.Versioned",
            vec![],
            json!({ "SharedDocumentVersion": "$DEFAULT" }),
        ),
    ];

    for (case, name, already, extra) in rows {
        let router = router();
        create(&router, name).await;
        if !already.is_empty() {
            let (status, shared) = call(
                &router,
                "ModifyDocumentPermission",
                &json!({
                    "Name": name,
                    "PermissionType": SHARE,
                    "AccountIdsToAdd": already,
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the share this row starts from must be granted, got {status} \
                 with {shared}"
            );
        }
        let before = shared_with(&router, case, name).await;

        let mut body = json!({ "Name": name, "PermissionType": SHARE });
        if let Some(extra) = extra.as_object() {
            for (member, value) in extra {
                body[member.as_str()] = value.clone();
            }
        }
        let (status, response) = call(&router, "ModifyDocumentPermission", &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, ARGUMENT_REFUSAL),
            "case {case:?}: expected the argument refusal, got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            NEITHER_NAMED,
            "case {case:?}: expected the fixed sentence, got {response}"
        );
        assert_eq!(
            shared_with(&router, case, name).await,
            before,
            "case {case:?}: a refused call changes no sharing"
        );
    }
}

/// RULE, the other side: a call naming EITHER list is served, so the refusal is about the
/// pair and nothing else.
#[tokio::test]
async fn a_call_naming_either_list_is_served() {
    let router = router();
    let name = "Permission.Either.Named";
    create(&router, name).await;

    // (case, the members the call carries, what the document is shared with afterwards)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "only AccountIdsToAdd",
            json!({ "AccountIdsToAdd": [OTHER_ACCOUNT] }),
            vec![OTHER_ACCOUNT],
        ),
        (
            "only AccountIdsToRemove",
            json!({ "AccountIdsToRemove": [OTHER_ACCOUNT] }),
            vec![],
        ),
        (
            "both, where the removal outranks the add",
            json!({
                "AccountIdsToAdd": [OTHER_ACCOUNT, THIRD_ACCOUNT],
                "AccountIdsToRemove": [THIRD_ACCOUNT],
            }),
            vec![OTHER_ACCOUNT],
        ),
    ];

    for (case, extra, expected) in rows {
        let mut body = json!({ "Name": name, "PermissionType": SHARE });
        if let Some(extra) = extra.as_object() {
            for (member, value) in extra {
                body[member.as_str()] = value.clone();
            }
        }
        let (status, response) = call(&router, "ModifyDocumentPermission", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a call naming either list is served, got {status} with {response}"
        );
        let mut expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        expected.sort();
        assert_eq!(
            shared_with(&router, case, name).await,
            expected,
            "case {case:?}: expected the sharing the call asked for"
        );
    }
}
