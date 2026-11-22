// SPDX-License-Identifier: CC0-1.0

//! The message text QuickSight's conflict and not-found refusals carry.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the status, the error code, and the message body the handler rendered.
//!
//! The rule under test: a refusal reports the resource it is about the way the service
//! does. A resource addressed by an ARN is named by that ARN — `"<Kind> <arn> already
//! exists"` when a create collides with it and `"<Kind> <arn> is not found"` when an
//! operation is keyed on one that is absent, neither ending in a period (a folder's
//! miss, which the service words its own way, is `"Folder: <arn> not found"`) — with
//! the ARN assembled from the region the request was signed for and the request's own
//! path labels; a group, which the identity path serves, is named by its name together
//! with the account that holds it; a namespace is named by the name the request carried,
//! and a namespace WRITE closes its refusal with a clause naming the call that failed,
//! `"Failed to <verb> namespace <Namespace> for account <AwsAccountId>. "`. The same
//! template serves every operation of a resource, so the wording does not depend on
//! which call a client made.
//!
//! So the rows below vary what the rule generalises over — the resource kind, the
//! operation that refuses, the account, the region, and the form and length of the id
//! or name — rather than re-running one recorded call.
//!
//! The last two suites are about WHERE a refusal states the request id of the call,
//! which is the same rule read across error classes: a failure reports it in the
//! `x-amzn-RequestId` response header and its document carries the error shape's other
//! members only, while a SUCCESS reports it as the body member its output shape declares.

use serde_json::{json, Value};

use super::data_source_data_set_lifecycle::Harness;
use super::delete_namespace::{delete_namespace, settle_deletion};
use super::namespace_lifecycle::{create_namespace_in, describe_namespace_in};

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// A third 12-digit account id, so a row can hold a resource no other account does.
const THIRD_ACCOUNT: &str = "123456789012";
/// The region most rows sign for.
const WEST: &str = "us-west-2";
/// A second region, so a row shows the ARN in a refusal is scoped to the region the
/// request was signed for rather than to a fixed one.
const EAST: &str = "eu-central-1";
/// The namespace every account already has.
/// Source: smithy_query(query="operation", operation="CreateGroup") — `Namespace` is a
/// required label.
const DEFAULT_NAMESPACE: &str = "default";
/// A namespace this suite creates, so a row can show the group conflict's wording does
/// not vary with the namespace the group lives in.
const CUSTOM_NAMESPACE: &str = "refusal-ns";

/// A QuickSight ARN as the service builds it: `arn:aws:quicksight:<region>:<account>:<resource>`.
fn quicksight_arn(region: &str, account: &str, resource: &str) -> String {
    format!("arn:aws:quicksight:{region}:{account}:{resource}")
}

/// `CreateGroup`: `POST /accounts/{AwsAccountId}/namespaces/{Namespace}/groups`.
/// Source: smithy_query(query="operation", operation="CreateGroup").
async fn create_group(
    harness: &Harness,
    region: &str,
    account: &str,
    namespace: &str,
    group_name: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "POST",
            &format!("/accounts/{account}/namespaces/{namespace}/groups"),
            Some(json!({ "GroupName": group_name })),
        )
        .await
}

/// `CreateFolder`: `POST /accounts/{AwsAccountId}/folders/{FolderId}`.
/// Source: smithy_query(query="operation", operation="CreateFolder").
async fn create_folder(
    harness: &Harness,
    region: &str,
    account: &str,
    folder_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "POST",
            &format!("/accounts/{account}/folders/{folder_id}"),
            Some(json!({ "Name": "a folder" })),
        )
        .await
}

/// `DescribeFolder`: `GET /accounts/{AwsAccountId}/folders/{FolderId}`.
/// Source: smithy_query(query="operation", operation="DescribeFolder").
async fn describe_folder(
    harness: &Harness,
    region: &str,
    account: &str,
    folder_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "GET",
            &format!("/accounts/{account}/folders/{folder_id}"),
            None,
        )
        .await
}

/// `CreateDataSource`: `POST /accounts/{AwsAccountId}/data-sources`.
/// Source: smithy_query(query="operation", operation="CreateDataSource").
async fn create_data_source(
    harness: &Harness,
    region: &str,
    account: &str,
    data_source_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "POST",
            &format!("/accounts/{account}/data-sources"),
            Some(json!({
                "DataSourceId": data_source_id,
                "Name": "a source",
                "Type": "ATHENA",
            })),
        )
        .await
}

/// The three data-source operations keyed on a `DataSourceId`, as the model routes
/// them: the read, the update and the delete.
/// Source: smithy_query(query="operation", operation="DescribeDataSource,UpdateDataSource,DeleteDataSource").
const DATA_SOURCE_CALLS: [(&str, &str); 3] = [
    ("DescribeDataSource", "GET"),
    ("UpdateDataSource", "PUT"),
    ("DeleteDataSource", "DELETE"),
];

/// One of [`DATA_SOURCE_CALLS`], addressed to `(account, data_source_id)`.
async fn call_data_source(
    harness: &Harness,
    method: &str,
    region: &str,
    account: &str,
    data_source_id: &str,
) -> (u16, Value) {
    let body = (method == "PUT").then(|| json!({ "Name": "whatever" }));
    harness
        .call_in_region(
            region,
            method,
            &format!("/accounts/{account}/data-sources/{data_source_id}"),
            body,
        )
        .await
}

/// One row of the duplicate-group table: a group created once, then created again.
struct GroupRow {
    case: &'static str,
    region: &'static str,
    account: &'static str,
    namespace: &'static str,
    /// The `GroupName` both creates name. `GroupName` is len 1.. over the pattern
    /// `^[\u0020-\u00FF]+$`, so a name can hold a space or a non-ASCII letter.
    /// Source: smithy_query(query="shape", shape="GroupName").
    group_name: String,
}

#[tokio::test]
async fn a_group_name_already_in_use_is_refused_with_the_identity_already_exists_text() {
    let harness = Harness::new();
    for account in [ACCOUNT, OTHER_ACCOUNT, THIRD_ACCOUNT] {
        let (status, body) = create_namespace_in(
            &harness,
            WEST,
            account,
            json!({ "Namespace": CUSTOM_NAMESPACE, "IdentityStore": "QUICKSIGHT" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "fixture namespace for {account}: got {status} with {body}"
        );
    }

    let rows = vec![
        GroupRow {
            case: "an ASCII name in the default namespace",
            region: WEST,
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "analysts".to_string(),
        },
        GroupRow {
            case: "a name holding a space",
            region: WEST,
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Field Ops".to_string(),
        },
        GroupRow {
            case: "a non-ASCII name the pattern admits",
            region: EAST,
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Groupe Générée".to_string(),
        },
        GroupRow {
            case: "the shortest name the model admits",
            region: WEST,
            account: OTHER_ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "a".to_string(),
        },
        GroupRow {
            case: "a long name, in another account",
            region: EAST,
            account: THIRD_ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "g".repeat(128),
        },
        GroupRow {
            case: "a namespace the account created, not the default one",
            region: WEST,
            account: ACCOUNT,
            namespace: CUSTOM_NAMESPACE,
            group_name: "analysts".to_string(),
        },
    ];

    for row in rows {
        let GroupRow {
            case,
            region,
            account,
            namespace,
            group_name,
        } = row;
        let (status, body) = create_group(&harness, region, account, namespace, &group_name).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the first create expected 201, got {status} with {body}"
        );

        let (status, body) = create_group(&harness, region, account, namespace, &group_name).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating the group expected 409, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceExistsException"),
            "case {case:?}: expected ResourceExistsException, got {body}"
        );
        // The name twice and the account once, and the sentence ends in a period
        // followed by a space.
        let expected = format!(
            "Group {group_name} already exists. AWS account id: {account}, groupName: \
             {group_name}. "
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected Message {expected:?}, got {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("GROUP"),
            "case {case:?}: the refusal is about a group, got {body}"
        );
    }
}

#[tokio::test]
async fn a_folder_id_already_in_use_is_refused_by_the_folders_arn() {
    let harness = Harness::new();

    // Rows: the id's form and length, the account that holds the folder, and the
    // region the call is signed for. `FolderId` is len 1..2048 over `^[\w\-]+$`.
    // Source: smithy_query(query="operation", operation="CreateFolder").
    let rows: Vec<(&str, &str, &str, String)> = vec![
        ("a plain id", WEST, ACCOUNT, "reports".to_string()),
        (
            "an id holding the separators the pattern admits",
            WEST,
            ACCOUNT,
            "team_reports-2026".to_string(),
        ),
        (
            "the shortest id the model admits",
            WEST,
            OTHER_ACCOUNT,
            "f".to_string(),
        ),
        (
            "a long id, in another account",
            EAST,
            THIRD_ACCOUNT,
            "f".repeat(200),
        ),
        (
            "the same id, in the other region",
            EAST,
            ACCOUNT,
            "reports-east".to_string(),
        ),
    ];

    for (case, region, account, folder_id) in rows {
        let (status, body) = create_folder(&harness, region, account, &folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first create expected 200, got {status} with {body}"
        );
        let created_arn = body["Arn"].clone();

        let (status, body) = create_folder(&harness, region, account, &folder_id).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating the folder expected 409, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceExistsException"),
            "case {case:?}: expected ResourceExistsException, got {body}"
        );
        let arn = quicksight_arn(region, account, &format!("folder/{folder_id}"));
        assert_eq!(
            created_arn,
            json!(arn),
            "case {case:?}: the fixture folder's own ARN must be the one the refusal \
             names, got {created_arn}"
        );
        let expected = format!("Folder {arn} already exists");
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected Message {expected:?}, got {body}"
        );
        assert_eq!(
            body["ResourceType"],
            Value::Null,
            "case {case:?}: the modelled enum has no folder member, so the refusal \
             reports none, got {body}"
        );
    }
}

#[tokio::test]
async fn a_folder_the_account_holds_none_of_is_not_found_by_its_arn() {
    let harness = Harness::new();
    let held = "held-folder";
    let (status, body) = create_folder(&harness, WEST, ACCOUNT, held).await;
    assert_eq!(status, 200, "fixture create: got {status} with {body}");

    // Rows: an id nothing created, an id held only by another account, an id differing
    // only in case, and a read signed for the other region.
    let rows: [(&str, &str, &str, &str); 4] = [
        ("an id nothing created", WEST, ACCOUNT, "never-created"),
        ("an id held by another account", WEST, OTHER_ACCOUNT, held),
        ("an id differing only in case", WEST, ACCOUNT, "HELD-FOLDER"),
        ("an account holding nothing at all", EAST, THIRD_ACCOUNT, held),
    ];
    for (case, region, account, folder_id) in rows {
        let (status, body) = describe_folder(&harness, region, account, folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for a folder nothing holds, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
        let expected = format!(
            "Folder: {} not found",
            quicksight_arn(region, account, &format!("folder/{folder_id}"))
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected Message {expected:?}, got {body}"
        );
    }
}

#[tokio::test]
async fn every_data_source_operation_reports_a_source_it_cannot_find_by_its_arn() {
    let harness = Harness::new();
    let held = "held-source";
    let (status, body) = create_data_source(&harness, WEST, ACCOUNT, held).await;
    assert_eq!(status, 202, "fixture create: got {status} with {body}");

    // The recorded case generalised: a source that was created and then deleted is as
    // absent as one that never existed, so it is one row among the others.
    let deleted = "deleted-source";
    let (status, body) = create_data_source(&harness, WEST, ACCOUNT, deleted).await;
    assert_eq!(status, 202, "fixture create: got {status} with {body}");
    let (status, body) = call_data_source(&harness, "DELETE", WEST, ACCOUNT, deleted).await;
    assert_eq!(status, 200, "fixture delete: got {status} with {body}");

    // Rows: an id nothing created, the deleted id, an id held only by another account,
    // an id differing only in case, an id in the other region, and a long id.
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "an id nothing created",
            WEST,
            ACCOUNT,
            "absent-source".to_string(),
        ),
        ("a deleted id", WEST, ACCOUNT, deleted.to_string()),
        (
            "an id held by another account",
            WEST,
            OTHER_ACCOUNT,
            held.to_string(),
        ),
        (
            "an id differing only in case",
            WEST,
            ACCOUNT,
            "HELD-SOURCE".to_string(),
        ),
        (
            "an account holding nothing at all, in the other region",
            EAST,
            THIRD_ACCOUNT,
            held.to_string(),
        ),
        ("a long id", EAST, ACCOUNT, "s".repeat(200)),
    ];

    for (case, region, account, data_source_id) in rows {
        let expected = format!(
            "DataSource {} is not found",
            quicksight_arn(region, account, &format!("datasource/{data_source_id}"))
        );
        // One template per resource, not per operation: the read, the update and the
        // delete all report the same miss the same way.
        for (operation, method) in DATA_SOURCE_CALLS {
            let (status, body) =
                call_data_source(&harness, method, region, account, &data_source_id).await;
            assert_eq!(
                status, 404,
                "case {case:?}/{operation}: expected 404 for a source nothing holds, got \
                 {status} with {body}"
            );
            assert_eq!(
                body["__type"],
                json!("ResourceNotFoundException"),
                "case {case:?}/{operation}: expected ResourceNotFoundException, got {body}"
            );
            assert_eq!(
                body["Message"],
                json!(expected),
                "case {case:?}/{operation}: expected Message {expected:?}, got {body}"
            );
            assert_eq!(
                body["ResourceType"],
                Value::Null,
                "case {case:?}/{operation}: the data-source miss carries no ResourceType \
                 discriminator, got {body}"
            );
        }
    }

    // The source the refusals addressed by the wrong id is still readable, so none of
    // them disturbed the store.
    let (status, body) = call_data_source(&harness, "GET", WEST, ACCOUNT, held).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored source readable, got {status} with {body}"
    );
}

/// `CreateTheme`: `POST /accounts/{AwsAccountId}/themes/{ThemeId}`.
/// Source: smithy_query(query="operation", operation="CreateTheme").
async fn create_theme(
    harness: &Harness,
    region: &str,
    account: &str,
    theme_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "POST",
            &format!("/accounts/{account}/themes/{theme_id}"),
            Some(json!({
                "Name": "a theme",
                "BaseThemeId": "MIDNIGHT",
                "Configuration": { "DataColorPalette": { "Colors": ["#37BFF5", "#8E6E9B"] } },
            })),
        )
        .await
}

/// The implemented operations keyed on a `(AwsAccountId, ThemeId)` theme: the read, the
/// delete, the two alias calls addressed through the theme and the two permission calls.
/// The theme's remaining operations are still stubs and answer 501, so they report no
/// miss to assert.
/// Source: smithy_query(query="operation", operation="DescribeTheme,DeleteTheme,
/// CreateThemeAlias,DescribeThemeAlias,DescribeThemePermissions,UpdateThemePermissions").
const THEME_CALLS: [&str; 6] = [
    "DescribeTheme",
    "DeleteTheme",
    "CreateThemeAlias",
    "DescribeThemeAlias",
    "DescribeThemePermissions",
    "UpdateThemePermissions",
];

/// One of [`THEME_CALLS`], addressed to `(account, theme_id)` and signed for `region`.
/// Each request carries the members its own shape requires, so the miss is what refuses
/// it rather than a missing member.
async fn call_theme(
    harness: &Harness,
    operation: &str,
    region: &str,
    account: &str,
    theme_id: &str,
) -> (u16, Value) {
    let base = format!("/accounts/{account}/themes/{theme_id}");
    match operation {
        "DescribeTheme" => harness.call_in_region(region, "GET", &base, None).await,
        "DeleteTheme" => harness.call_in_region(region, "DELETE", &base, None).await,
        "CreateThemeAlias" => {
            harness
                .call_in_region(
                    region,
                    "POST",
                    &format!("{base}/aliases/POSTDELETE"),
                    Some(json!({ "ThemeVersionNumber": 1 })),
                )
                .await
        }
        "DescribeThemeAlias" => {
            harness
                .call_in_region(region, "GET", &format!("{base}/aliases/POSTDELETE"), None)
                .await
        }
        "DescribeThemePermissions" => {
            harness
                .call_in_region(region, "GET", &format!("{base}/permissions"), None)
                .await
        }
        _ => {
            harness
                .call_in_region(
                    region,
                    "PUT",
                    &format!("{base}/permissions"),
                    Some(json!({ "GrantPermissions": [] })),
                )
                .await
        }
    }
}

#[tokio::test]
async fn every_theme_operation_reports_a_theme_it_cannot_find_by_its_arn() {
    let harness = Harness::new();
    let held = "held-theme";
    let (status, body) = create_theme(&harness, WEST, ACCOUNT, held).await;
    assert_eq!(status, 200, "fixture create: got {status} with {body}");

    // The recorded case generalised: a theme that was created and then deleted is as
    // absent as one that never existed, so it is one row among the others.
    let deleted = "deleted-theme";
    let (status, body) = create_theme(&harness, WEST, ACCOUNT, deleted).await;
    assert_eq!(status, 200, "fixture create: got {status} with {body}");
    let (status, body) = call_theme(&harness, "DeleteTheme", WEST, ACCOUNT, deleted).await;
    assert_eq!(status, 200, "fixture delete: got {status} with {body}");

    // Rows: an id nothing created, the deleted id, an id held only by another account,
    // an id differing only in case, the id's form and length, and the region the call is
    // signed for. `ThemeId` is len 1..512 over `^[\w\-]+$`.
    // Source: smithy_query(query="operation", operation="DescribeTheme").
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "an id nothing created",
            WEST,
            ACCOUNT,
            "absent-theme".to_string(),
        ),
        ("a deleted id", WEST, ACCOUNT, deleted.to_string()),
        (
            "an id held by another account",
            WEST,
            OTHER_ACCOUNT,
            held.to_string(),
        ),
        (
            "an id differing only in case",
            WEST,
            ACCOUNT,
            "HELD-THEME".to_string(),
        ),
        (
            "an id holding the separators the pattern admits",
            WEST,
            ACCOUNT,
            "Theme_7-ID".to_string(),
        ),
        (
            "the shortest id the model admits",
            WEST,
            OTHER_ACCOUNT,
            "t".to_string(),
        ),
        (
            "the longest id the model admits",
            WEST,
            ACCOUNT,
            "t".repeat(512),
        ),
        (
            "an account holding nothing at all, in the other region",
            EAST,
            THIRD_ACCOUNT,
            held.to_string(),
        ),
        // The ARN a refusal names is scoped to the region the request was signed for,
        // so the same absent id read from the other region is named by another ARN.
        (
            "an id nothing created, in the other region",
            EAST,
            ACCOUNT,
            "absent-theme".to_string(),
        ),
    ];

    for (case, region, account, theme_id) in rows {
        let expected = format!(
            "Theme {} is not found",
            quicksight_arn(region, account, &format!("theme/{theme_id}"))
        );
        // One template per resource, not per operation: the read, the delete, the alias
        // calls and the permission calls all report the same miss the same way.
        for operation in THEME_CALLS {
            let (status, body) = call_theme(&harness, operation, region, account, &theme_id).await;
            assert_eq!(
                status, 404,
                "case {case:?}/{operation}: expected 404 for a theme nothing holds, got \
                 {status} with {body}"
            );
            assert_eq!(
                body["__type"],
                json!("ResourceNotFoundException"),
                "case {case:?}/{operation}: expected ResourceNotFoundException, got {body}"
            );
            assert_eq!(
                body["Message"],
                json!(expected),
                "case {case:?}/{operation}: expected Message {expected:?}, got {body}"
            );
            assert_eq!(
                body["ResourceType"],
                Value::Null,
                "case {case:?}/{operation}: the modelled enum has no theme member, so the \
                 refusal reports none, got {body}"
            );
        }
    }

    // The theme the refusals addressed by the wrong id is still readable, so none of
    // them disturbed the store.
    let (status, body) = call_theme(&harness, "DescribeTheme", WEST, ACCOUNT, held).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored theme readable, got {status} with {body}"
    );
}

/// The reason a namespace lookup fails: the namespace by the name the request carried,
/// with no trailing period.
fn namespace_miss_reason(namespace: &str) -> String {
    format!("Namespace {namespace} not found")
}

#[tokio::test]
async fn deleting_a_namespace_nothing_holds_names_the_delete_that_failed() {
    let harness = Harness::new();

    // One namespace IS held, so the rows below are about names that address no namespace
    // rather than about an empty store.
    let held = "held-namespace";
    let (status, body) = create_namespace_in(
        &harness,
        WEST,
        ACCOUNT,
        json!({ "Namespace": held, "IdentityStore": "QUICKSIGHT" }),
    )
    .await;
    assert_eq!(status, 200, "fixture create: got {status} with {body}");

    // The recorded case generalised: a namespace created and then deleted is as absent as
    // one that never existed, so it is one row among the others. Its removal is what the
    // delete asked for and the clock owes a span later: bring that instant forward rather
    // than wait for it.
    let deleted = "deleted.namespace";
    let (status, body) = create_namespace_in(
        &harness,
        WEST,
        ACCOUNT,
        json!({ "Namespace": deleted, "IdentityStore": "QUICKSIGHT" }),
    )
    .await;
    assert_eq!(status, 200, "fixture create: got {status} with {body}");
    let (status, body) = delete_namespace(&harness, ACCOUNT, deleted).await;
    assert_eq!(status, 200, "fixture delete: got {status} with {body}");
    settle_deletion(&harness, ACCOUNT, deleted);

    // Rows: a name nothing created, the deleted name, a name held only by another
    // account, a name differing only in case, and the name's form and length.
    // `Namespace` is len 0..64 over `^[a-zA-Z0-9._-]*$`.
    // Source: smithy_query(query="operation", operation="DeleteNamespace").
    let rows: Vec<(&str, &str, String)> = vec![
        ("a name nothing created", ACCOUNT, "absent-ns".to_string()),
        ("a deleted name", ACCOUNT, deleted.to_string()),
        ("a name held by another account", OTHER_ACCOUNT, held.to_string()),
        ("a name differing only in case", ACCOUNT, "HELD-NAMESPACE".to_string()),
        (
            "a name holding the punctuation the pattern admits",
            ACCOUNT,
            "sales.archive_2026-Q1".to_string(),
        ),
        ("the shortest name the model admits", THIRD_ACCOUNT, "n".to_string()),
        ("the longest name the model admits", ACCOUNT, "n".repeat(64)),
    ];

    for (case, account, namespace) in rows {
        // The delete names the namespace, then the call that failed and the account it
        // addressed: the namespace travels twice and the message ends in a period and a
        // space.
        let expected = format!(
            "{} Failed to delete namespace {namespace} for account {account}. ",
            namespace_miss_reason(&namespace)
        );
        let (status, body) = delete_namespace(&harness, account, &namespace).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for a namespace nothing holds, got {status} \
             with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected Message {expected:?}, got {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("NAMESPACE"),
            "case {case:?}: the refusal is about a namespace, got {body}"
        );

        // The read of the same namespace reports the reason alone: the clause belongs to
        // the write that failed, not to the lookup.
        let reason = namespace_miss_reason(&namespace);
        let (status, body) = describe_namespace_in(&harness, WEST, account, &namespace).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 reading a namespace nothing holds, got {status} \
             with {body}"
        );
        assert_eq!(
            body["Message"],
            json!(reason),
            "case {case:?}: expected Message {reason:?}, got {body}"
        );
    }

    // The namespace the refusals addressed by the wrong name is still readable, so none
    // of them disturbed the store.
    let (status, body) = describe_namespace_in(&harness, WEST, ACCOUNT, held).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored namespace readable, got {status} with {body}"
    );
}

/// The response header a QuickSight answer reports the request id of a failed call in.
/// HTTP header names are case-insensitive, and `HeaderMap` is keyed on the lowercase
/// spelling.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// Every member a refusal document of this service may carry: the shape discriminator the
/// JSON envelope names the error class in, the error shape's own `Message` and
/// `ResourceType`, and the lowercase `message` the framework's synthetic shapes word a
/// 501 in. `RequestId` is deliberately NOT one of them.
/// Source: smithy_query(query="shape", shape="ResourceNotFoundException,
/// ResourceExistsException,InvalidParameterValueException").
const REFUSAL_BODY_MEMBERS: [&str; 4] = ["__type", "Message", "ResourceType", "message"];

/// One row of the refusal-answer table: the call to make, and what its answer states.
struct RefusalRow {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The status the class is rendered under.
    status: u16,
    /// The error class, as `__type` and `x-amzn-errortype` name it.
    shape: &'static str,
    /// The `ResourceType` discriminator the class reports, where it reports one.
    resource_type: Option<&'static str>,
    /// Whether the rendered error shape declares a request-id member at all, and so
    /// whether the answer reports an id in its header. The framework's synthetic shapes
    /// (the 501 among them) declare none, so they report none anywhere.
    reports_a_request_id: bool,
}

#[tokio::test]
async fn a_refusal_reports_its_request_id_in_the_header_and_never_in_the_document() {
    let harness = Harness::new();

    // Fixtures the rows refuse against: a folder and a group whose ids are taken, a
    // readable data source, and one created and then deleted.
    let held_folder = "held-folder-id";
    let (status, body) = create_folder(&harness, WEST, ACCOUNT, held_folder).await;
    assert_eq!(status, 200, "fixture folder: got {status} with {body}");
    let held_group = "held-group";
    let (status, body) = create_group(&harness, WEST, ACCOUNT, DEFAULT_NAMESPACE, held_group).await;
    assert_eq!(status, 201, "fixture group: got {status} with {body}");
    let held_source = "held-source-id";
    let (status, body) = create_data_source(&harness, WEST, ACCOUNT, held_source).await;
    assert_eq!(status, 202, "fixture source: got {status} with {body}");
    let deleted_source = "deleted-source-id";
    let (status, body) = create_data_source(&harness, WEST, ACCOUNT, deleted_source).await;
    assert_eq!(status, 202, "fixture source: got {status} with {body}");
    let (status, body) = call_data_source(&harness, "DELETE", WEST, ACCOUNT, deleted_source).await;
    assert_eq!(status, 200, "fixture delete: got {status} with {body}");

    // Rows: the error class and the status it renders under, the operation that refuses,
    // whether the class reports a `ResourceType` beside its sentence, and — the last row
    // — a rendered shape that declares no request-id member at all.
    let rows: Vec<RefusalRow> = vec![
        RefusalRow {
            case: "a data source deleted before the read, 404",
            method: "GET",
            path: format!("/accounts/{ACCOUNT}/data-sources/{deleted_source}"),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            resource_type: None,
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a data source nothing created, on the update, 404",
            method: "PUT",
            path: format!("/accounts/{ACCOUNT}/data-sources/absent-source"),
            body: Some(json!({ "Name": "whatever" })),
            status: 404,
            shape: "ResourceNotFoundException",
            resource_type: None,
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a second create of the same folder id, 409",
            method: "POST",
            path: format!("/accounts/{ACCOUNT}/folders/{held_folder}"),
            body: Some(json!({ "Name": "a folder" })),
            status: 409,
            shape: "ResourceExistsException",
            resource_type: None,
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a second create of the same data source id, 409",
            method: "POST",
            path: format!("/accounts/{ACCOUNT}/data-sources"),
            body: Some(json!({
                "DataSourceId": held_source,
                "Name": "a source",
                "Type": "ATHENA",
            })),
            status: 409,
            shape: "ResourceExistsException",
            resource_type: Some("DATA_SOURCE"),
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a second create of the same group name, which reports a ResourceType",
            method: "POST",
            path: format!("/accounts/{ACCOUNT}/namespaces/{DEFAULT_NAMESPACE}/groups"),
            body: Some(json!({ "GroupName": held_group })),
            status: 409,
            shape: "ResourceExistsException",
            resource_type: Some("GROUP"),
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a folder the account holds none of, 404",
            method: "GET",
            path: format!("/accounts/{OTHER_ACCOUNT}/folders/{held_folder}"),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            resource_type: None,
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a namespace nothing holds, on the delete, 404",
            method: "DELETE",
            path: format!("/accounts/{ACCOUNT}/namespaces/absent-ns"),
            body: None,
            status: 404,
            shape: "ResourceNotFoundException",
            resource_type: Some("NAMESPACE"),
            reports_a_request_id: true,
        },
        RefusalRow {
            case: "a parameter value naming an address with no route, 400",
            method: "POST",
            path: format!("/accounts/{ACCOUNT}/data-sources"),
            body: Some(json!({
                "DataSourceId": "unroutable-source",
                "Name": "a source",
                "Type": "SPARK",
                "DataSourceParameters": { "SparkParameters": { "Host": "100::1", "Port": 10000 } },
            })),
            status: 400,
            shape: "InvalidParameterValueException",
            resource_type: None,
            reports_a_request_id: true,
        },
        RefusalRow {
            // `DescribeIpRestriction` is still a stub, so its 501 is rendered from the
            // framework's own synthetic shape, which declares no request-id member.
            // Source: smithy_query(query="operation", operation="DescribeIpRestriction").
            case: "an operation this emulator has not implemented, 501",
            method: "GET",
            path: format!("/accounts/{ACCOUNT}/ip-restriction"),
            body: None,
            status: 501,
            shape: "NotImplemented",
            resource_type: None,
            reports_a_request_id: false,
        },
    ];

    // Every id reported across the table, so no two answers can report the same one.
    let mut reported_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for row in rows {
        let RefusalRow {
            case,
            method,
            path,
            body,
            status: expected_status,
            shape,
            resource_type,
            reports_a_request_id,
        } = row;

        let (status, document, headers) = harness
            .call_reporting_headers(WEST, method, &path, body)
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} with {document}"
        );
        assert_eq!(
            document["__type"],
            json!(shape),
            "case {case:?}: expected the class {shape}, got {document}"
        );
        assert_eq!(
            headers.get("x-amzn-errortype").and_then(|v| v.to_str().ok()),
            Some(shape),
            "case {case:?}: the class also travels in x-amzn-errortype, got {headers:?}"
        );

        // The document carries the error shape's own members and NOT the request id.
        assert!(
            document.get("RequestId").is_none(),
            "case {case:?}: the document must carry no RequestId member, got {document}"
        );
        let unexpected: Vec<&String> = document
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .filter(|key| !REFUSAL_BODY_MEMBERS.contains(&key.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            unexpected.is_empty(),
            "case {case:?}: the document carries members the shape does not declare \
             {unexpected:?}: {document}"
        );
        assert_eq!(
            document["ResourceType"],
            resource_type.map_or(Value::Null, |kind| json!(kind)),
            "case {case:?}: expected the ResourceType {resource_type:?}, got {document}"
        );

        let reported = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if reports_a_request_id {
            // The sentence of the class is still the handler's, beside the moved id.
            assert!(
                document["Message"]
                    .as_str()
                    .is_some_and(|sentence| !sentence.is_empty()),
                "case {case:?}: the document must still carry the class's sentence, got \
                 {document}"
            );
            let reported = reported.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the request id in the {REQUEST_ID_HEADER} \
                     header, got {headers:?}"
                )
            });
            assert!(
                !reported.is_empty(),
                "case {case:?}: the reported request id is empty: {headers:?}"
            );
            assert!(
                reported_ids.insert(reported.clone()),
                "case {case:?}: the request id {reported:?} was reported for another call too"
            );
        } else {
            assert!(
                reported.is_none(),
                "case {case:?}: a shape declaring no request-id member reports none, got \
                 {reported:?}"
            );
        }
    }

    // The fixtures the refusals addressed are still readable, so none of them disturbed
    // the store.
    let (status, body) = call_data_source(&harness, "GET", WEST, ACCOUNT, held_source).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored source readable, got {status} with {body}"
    );
    let (status, body) = describe_folder(&harness, WEST, ACCOUNT, held_folder).await;
    assert_eq!(
        status, 200,
        "the refusals must leave the stored folder readable, got {status} with {body}"
    );
}

#[tokio::test]
async fn a_successful_answer_still_reports_its_request_id_as_a_body_member() {
    let harness = Harness::new();
    let folder_id = "answered-folder";
    let source_id = "answered-source";
    let group_name = "answered-group";

    // Rows: the write and the read of two resources plus an identity create, so the
    // success contract is read through a 200, a 201 and a 202. Each output shape declares
    // `RequestId` as a body member, which is where a success reports it.
    // Source: smithy_query(query="operation", operation="CreateFolder,DescribeFolder,
    // CreateDataSource,DescribeDataSource,CreateGroup").
    let rows: Vec<(&str, u16, (u16, Value))> = vec![
        (
            "CreateFolder",
            200,
            create_folder(&harness, WEST, ACCOUNT, folder_id).await,
        ),
        (
            "DescribeFolder",
            200,
            describe_folder(&harness, WEST, ACCOUNT, folder_id).await,
        ),
        (
            "CreateDataSource",
            202,
            create_data_source(&harness, WEST, ACCOUNT, source_id).await,
        ),
        (
            "DescribeDataSource",
            200,
            call_data_source(&harness, "GET", WEST, ACCOUNT, source_id).await,
        ),
        (
            "CreateGroup",
            201,
            create_group(&harness, WEST, ACCOUNT, DEFAULT_NAMESPACE, group_name).await,
        ),
    ];

    for (case, expected_status, (status, body)) in rows {
        assert_eq!(
            status, expected_status,
            "case {case}: expected {expected_status}, got {status} with {body}"
        );
        assert!(
            body["RequestId"]
                .as_str()
                .is_some_and(|request_id| !request_id.is_empty()),
            "case {case}: a success reports its RequestId in the body, got {body}"
        );
    }
}
