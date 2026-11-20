// SPDX-License-Identifier: CC0-1.0

//! Folders: the create and the read that reports it, over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the route, the status, and a body the handler computed from the shared store.
//!
//! The rule under test: a folder is identified by `(AwsAccountId, FolderId)`;
//! `CreateFolder` persists what the call submitted — reporting the folder's ARN and id —
//! defaulting the folder type and the sharing model the model gives defaults for,
//! nesting the folder under the folder its `ParentFolderArn` names so that its ancestor
//! path is the parent's path followed by the parent, and refusing a second create of the
//! same id; `DescribeFolder` answers from that stored record, and `DeleteFolder` removes
//! it — only while it is empty — so that every later read reports the miss. So the rows
//! below vary what the rule generalises over (the id's form, whether a name is given and
//! how long it is, the folder type, the sharing model, the tags and permissions
//! submitted, the nesting depth and the account) rather than re-running one recorded
//! call.
//!
//! The update cases at the end of the suite are the same rule read through a write that
//! is not the create: an accepted `UpdateFolder` writes the `Name` it submitted to that
//! stored record, so the next `DescribeFolder` reports it and nothing else about the
//! folder moves.

use serde_json::{json, Value};

use service_com_amazonaws_quicksight_quicksight_20180401::state as qs_state;

use super::data_source_data_set_lifecycle::Harness;

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// The region the test client signs for; it is the region in every ARN below.
const REGION: &str = "us-west-2";

fn folder_arn(account: &str, folder_id: &str) -> String {
    format!("arn:aws:quicksight:{REGION}:{account}:folder/{folder_id}")
}

async fn create_folder(
    harness: &Harness,
    account: &str,
    folder_id: &str,
    body: Value,
) -> (u16, Value) {
    harness
        .call(
            "POST",
            &format!("/accounts/{account}/folders/{folder_id}"),
            Some(body),
        )
        .await
}

async fn describe_folder(harness: &Harness, account: &str, folder_id: &str) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!("/accounts/{account}/folders/{folder_id}"),
            None,
        )
        .await
}

/// `UpdateFolder`: `PUT /accounts/{AwsAccountId}/folders/{FolderId}`.
/// Source: smithy_query(query="operation", operation="UpdateFolder").
async fn update_folder(
    harness: &Harness,
    account: &str,
    folder_id: &str,
    body: Value,
) -> (u16, Value) {
    harness
        .call(
            "PUT",
            &format!("/accounts/{account}/folders/{folder_id}"),
            Some(body),
        )
        .await
}

/// Wait until the clock the handlers read is strictly past `reported_time` (a
/// `CreatedTime` or `LastUpdatedTime` a read reported, in epoch seconds), so an update
/// made after this call has a strictly later instant available to record.
///
/// The condition is polled rather than waited out: the service's timestamps are
/// millisecond-resolution, so this returns as soon as the millisecond ticks over, and the
/// deadline is only there so a stopped clock fails the case instead of hanging.
fn wait_for_clock_past(case: &str, reported_time: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while service_clock_seconds() <= reported_time {
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the clock never moved past {reported_time}"
        );
        std::hint::spin_loop();
    }
}

/// The instant the handlers would record now, in the units a read reports it in: epoch
/// seconds with the millisecond resolution `CreatedTime` and `LastUpdatedTime` carry.
fn service_clock_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64
        / 1000.0
}

/// The names the update rows submit, one per row of [`rows`], so the table varies what
/// the read-your-write rule generalises over: the shortest and the longest `FolderName`
/// the model admits (len 1..200), a non-ASCII one, one holding punctuation and
/// separators, and a plain rename.
/// Source: smithy_query(query="operation", operation="UpdateFolder") — `Name` targets
/// `FolderName`, len 1..200.
fn updated_names() -> Vec<(&'static str, String)> {
    vec![
        ("plain-rename", "Reports (renamed)".to_string()),
        ("shortest-name", "n".to_string()),
        ("longest-name", "r".repeat(200)),
        ("non-ascii-name", "Rapports Café".to_string()),
        ("punctuated-name", "finance/eu-west-2: readers".to_string()),
        ("digits-and-spaces", "Q1 2026 draft".to_string()),
    ]
}

/// One row of the create-then-read table.
struct Row {
    /// The case label every assertion names.
    case: &'static str,
    account: &'static str,
    /// The `FolderId` the create names; the model admits len 1..2048 over the pattern
    /// `^[\w\-]+$`.
    /// Source: smithy_query(query="operation", operation="CreateFolder").
    folder_id: String,
    /// The `Name` the create submits, if any — `FolderName` is len 1..200 and the
    /// member is optional.
    name: Option<String>,
    /// The `FolderType` the create submits, if any. The model's `FolderType` enum
    /// admits SHARED and RESTRICTED, and documents SHARED as the default.
    /// Source: smithy_query(query="shape", shape="FolderType").
    folder_type: Option<&'static str>,
    /// The `SharingModel` the create submits, if any. The model's `SharingModel` enum
    /// admits ACCOUNT and NAMESPACE, and documents ACCOUNT as the default.
    /// Source: smithy_query(query="shape", shape="SharingModel").
    sharing_model: Option<&'static str>,
    tags: Option<Value>,
    permissions: Option<Value>,
    /// What the folder's reported `FolderType` and `SharingModel` must be: the
    /// submitted value, or the model's documented default when none was submitted.
    expected_folder_type: &'static str,
    expected_sharing_model: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // Nothing beyond the id: both enums fall back to their documented default
            // and the folder has no display name to report.
            case: "defaults-only",
            account: ACCOUNT,
            folder_id: "reports".to_string(),
            name: None,
            folder_type: None,
            sharing_model: None,
            tags: None,
            permissions: None,
            expected_folder_type: "SHARED",
            expected_sharing_model: "ACCOUNT",
        },
        Row {
            case: "restricted-with-tags-and-permissions",
            account: ACCOUNT,
            folder_id: "finance_eu-west-2".to_string(),
            name: Some("Finance (EU)".to_string()),
            folder_type: Some("RESTRICTED"),
            sharing_model: Some("ACCOUNT"),
            tags: Some(json!([
                { "Key": "team", "Value": "finance" },
                { "Key": "cost-centre", "Value": "4711" },
            ])),
            permissions: Some(json!([{
                "Principal": format!("arn:aws:quicksight:{REGION}:{ACCOUNT}:user/default/analyst"),
                "Actions": ["quicksight:DescribeFolder", "quicksight:CreateFolder"],
            }])),
            expected_folder_type: "RESTRICTED",
            expected_sharing_model: "ACCOUNT",
        },
        Row {
            // The other member of the `SharingModel` enum, which the issue's own call
            // did not exercise.
            case: "namespace-sharing-model",
            account: ACCOUNT,
            folder_id: "shared-with-namespace".to_string(),
            name: Some("Namespace scoped".to_string()),
            folder_type: Some("SHARED"),
            sharing_model: Some("NAMESPACE"),
            tags: None,
            permissions: None,
            expected_folder_type: "SHARED",
            expected_sharing_model: "NAMESPACE",
        },
        Row {
            // The shortest id the pattern admits, and the longest name the model does:
            // `FolderName` is len 1..200.
            case: "single-character-id-and-longest-name",
            account: ACCOUNT,
            folder_id: "f".to_string(),
            name: Some("n".repeat(200)),
            folder_type: None,
            sharing_model: Some("ACCOUNT"),
            tags: Some(json!([{ "Key": "edge", "Value": "max-length" }])),
            permissions: None,
            expected_folder_type: "SHARED",
            expected_sharing_model: "ACCOUNT",
        },
        Row {
            // A long id, well past the one the issue cited, still inside len 1..2048.
            case: "long-id",
            account: ACCOUNT,
            folder_id: format!("folder-{}", "a".repeat(300)),
            name: Some("Long id".to_string()),
            folder_type: None,
            sharing_model: None,
            tags: None,
            permissions: None,
            expected_folder_type: "SHARED",
            expected_sharing_model: "ACCOUNT",
        },
        Row {
            // The same id a row above used, in another account: the account is part of
            // the folder's identity.
            case: "other-account",
            account: OTHER_ACCOUNT,
            folder_id: "reports".to_string(),
            name: Some("Reports".to_string()),
            folder_type: Some("RESTRICTED"),
            sharing_model: None,
            tags: None,
            permissions: None,
            expected_folder_type: "RESTRICTED",
            expected_sharing_model: "ACCOUNT",
        },
    ]
}

impl Row {
    fn body(&self) -> Value {
        let mut body = json!({});
        if let Some(name) = &self.name {
            body["Name"] = json!(name);
        }
        if let Some(folder_type) = self.folder_type {
            body["FolderType"] = json!(folder_type);
        }
        if let Some(sharing_model) = self.sharing_model {
            body["SharingModel"] = json!(sharing_model);
        }
        if let Some(tags) = &self.tags {
            body["Tags"] = tags.clone();
        }
        if let Some(permissions) = &self.permissions {
            body["Permissions"] = permissions.clone();
        }
        body
    }
}

#[tokio::test]
async fn create_folder_persists_it_and_describe_reports_it() {
    let harness = Harness::new();

    for row in rows() {
        let case = row.case;
        let folder_id = row.folder_id.as_str();
        let arn = folder_arn(row.account, folder_id);

        let (status, body) = create_folder(&harness, row.account, folder_id, row.body()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: create expected 200, got {status} with body {body}"
        );
        assert_eq!(
            body["Arn"],
            json!(arn),
            "case {case:?}: create reported the wrong Arn: {body}"
        );
        assert_eq!(
            body["FolderId"],
            json!(folder_id),
            "case {case:?}: create reported the wrong FolderId: {body}"
        );
        assert!(
            body["RequestId"]
                .as_str()
                .is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: create reported no RequestId: {body}"
        );

        let (status, body) = describe_folder(&harness, row.account, folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with body {body}"
        );
        let reported = &body["Folder"];
        assert_eq!(
            reported["Arn"],
            json!(arn),
            "case {case:?}: describe reported the wrong Arn: {body}"
        );
        assert_eq!(
            reported["FolderId"],
            json!(folder_id),
            "case {case:?}: describe reported the wrong FolderId: {body}"
        );
        assert_eq!(
            reported["Name"],
            row.name.as_ref().map_or(Value::Null, |name| json!(name)),
            "case {case:?}: describe must report the submitted Name, and none when the \
             create submitted none: {body}"
        );
        assert_eq!(
            reported["FolderType"],
            json!(row.expected_folder_type),
            "case {case:?}: describe reported the wrong FolderType: {body}"
        );
        assert_eq!(
            reported["SharingModel"],
            json!(row.expected_sharing_model),
            "case {case:?}: describe reported the wrong SharingModel: {body}"
        );
        assert_eq!(
            reported["FolderPath"],
            json!([]),
            "case {case:?}: a folder created with no ParentFolderArn is root-level, so \
             its ancestor path is reported as the empty list: {body}"
        );
        let created_time = reported["CreatedTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: describe reported no CreatedTime: {body}");
        });
        assert!(
            created_time > 0.0,
            "case {case:?}: CreatedTime must be the instant the folder was created, got \
             {created_time}"
        );
        assert_eq!(
            reported["LastUpdatedTime"].as_f64(),
            Some(created_time),
            "case {case:?}: a folder nothing has updated was last updated when it was \
             created: {body}"
        );

        // What the create submitted beside the folder itself is stored too, so the
        // reads that report it have something to report.
        {
            let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
            let store = state.lock();
            let record = store
                .folder(row.account, folder_id)
                .unwrap_or_else(|| panic!("case {case:?}: the created folder is not stored"));
            let stored_tags: Vec<Value> = record
                .tags
                .iter()
                .map(|tag| json!({ "Key": tag.key, "Value": tag.value }))
                .collect();
            assert_eq!(
                json!(stored_tags),
                row.tags.clone().unwrap_or_else(|| json!([])),
                "case {case:?}: the submitted Tags are not stored as given"
            );
            let stored_permissions: Vec<Value> = record
                .permissions
                .iter()
                .map(|permission| {
                    json!({ "Principal": permission.principal, "Actions": permission.actions })
                })
                .collect();
            assert_eq!(
                json!(stored_permissions),
                row.permissions.clone().unwrap_or_else(|| json!([])),
                "case {case:?}: the submitted Permissions are not stored as given"
            );
        }

        // A second create of the same id in the same account is refused, whatever it
        // submits, and the folder it refused to overwrite is untouched.
        let (status, body) = create_folder(&harness, row.account, folder_id, json!({})).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating the same folder expected 409, got {status} with \
             {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceExistsException"),
            "case {case:?}: an id already in use is an existing resource: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Folder {} already exists",
                folder_arn(row.account, folder_id)
            )),
            "case {case:?}: the refusal must name the folder by its ARN: {body}"
        );
        let (status, body) = describe_folder(&harness, row.account, folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refused create must leave the folder readable, got \
             {status} with {body}"
        );
        assert_eq!(
            body["Folder"]["FolderType"],
            json!(row.expected_folder_type),
            "case {case:?}: the refused create must not have changed the folder: {body}"
        );
    }
}

#[tokio::test]
async fn folder_identity_is_the_account_and_the_id() {
    let harness = Harness::new();
    let shared = "shared-folder-id";

    for (case, account) in [("first-account", ACCOUNT), ("second-account", OTHER_ACCOUNT)] {
        let (status, body) =
            create_folder(&harness, account, shared, json!({ "Name": "Shared" })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: one folder id per account, so this create expected 200, got \
             {status} with {body}"
        );
        assert_eq!(
            body["Arn"],
            json!(folder_arn(account, shared)),
            "case {case:?}: the ARN must carry the owning account: {body}"
        );
    }

    for (case, account) in [("first-account", ACCOUNT), ("second-account", OTHER_ACCOUNT)] {
        let (status, body) = describe_folder(&harness, account, shared).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Folder"]["Arn"],
            json!(folder_arn(account, shared)),
            "case {case:?}: describe must report that account's own folder: {body}"
        );
    }

    // An id no folder is held under is not found, in an account that created folders
    // and in one that created none.
    for (case, account, folder_id) in [
        ("account-with-folders", ACCOUNT, "never-created"),
        ("account-with-nothing", "123456789012", shared),
    ] {
        let (status, body) = describe_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: describe of a folder that does not exist expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: an id no folder is held under is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Folder: {} not found",
                folder_arn(account, folder_id)
            )),
            "case {case:?}: the refusal must name the folder that was not found by its \
             ARN: {body}"
        );
    }
}

#[tokio::test]
async fn a_parent_folder_arn_nests_the_new_folder_under_it() {
    let harness = Harness::new();

    // A chain of folders, each created under the one before it: the ancestor path of
    // the folder at depth n is the n ARNs above it, outermost first.
    let depth = qs_state::MAX_FOLDER_ANCESTORS;
    let mut ancestors: Vec<String> = Vec::new();
    for level in 0..=depth {
        let folder_id = format!("level-{level}");
        let case = folder_id.clone();
        let mut body = json!({ "Name": format!("Level {level}") });
        if let Some(parent) = ancestors.last() {
            body["ParentFolderArn"] = json!(parent);
        }
        let (status, response) = create_folder(&harness, ACCOUNT, &folder_id, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a folder at depth {level} expected 200, got {status} with \
             {response}"
        );

        let (status, response) = describe_folder(&harness, ACCOUNT, &folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {response}"
        );
        // The ancestor path is always reported, so a client walking the hierarchy reads
        // the empty list at the root rather than a missing member.
        assert_eq!(
            response["Folder"]["FolderPath"],
            json!(ancestors),
            "case {case:?}: the ancestor path must be every folder above this one, \
             outermost first: {response}"
        );

        ancestors.push(folder_arn(ACCOUNT, &folder_id));
    }

    // One level further would need more ancestors than the modelled `FolderPath` can
    // carry, so it is refused rather than reported short.
    let (status, body) = create_folder(
        &harness,
        ACCOUNT,
        "one-level-too-deep",
        json!({ "ParentFolderArn": ancestors.last().expect("the deepest folder") }),
    )
    .await;
    assert_eq!(
        status, 409,
        "a folder deeper than the modelled path can carry expected 409, got {status} \
         with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("LimitExceededException"),
        "nesting past the modelled depth exceeds a limit: {body}"
    );
    assert_eq!(
        body["Message"],
        json!(format!(
            "A folder can have at most {depth} ancestor folders."
        )),
        "the refusal must name the limit: {body}"
    );

    // And the refused folder was not persisted.
    let (status, body) = describe_folder(&harness, ACCOUNT, "one-level-too-deep").await;
    assert_eq!(
        status, 404,
        "a refused create must persist nothing, got {status} with {body}"
    );
}

#[tokio::test]
async fn a_parent_folder_arn_naming_no_folder_is_not_found() {
    let harness = Harness::new();

    let (status, body) = create_folder(&harness, ACCOUNT, "parent", json!({})).await;
    assert_eq!(status, 200, "the parent create expected 200, got {status} with {body}");
    // A folder of the same id in another account, to show the ARN is resolved in the
    // account the call addresses.
    let (status, body) = create_folder(&harness, OTHER_ACCOUNT, "parent", json!({})).await;
    assert_eq!(
        status, 200,
        "the other account's parent create expected 200, got {status} with {body}"
    );

    // Each row is an ARN that names no folder of this account: an unknown id, another
    // account's folder, another Region's folder, another service's resource, and a
    // string that is no ARN at all.
    let cases = [
        ("unknown-id", folder_arn(ACCOUNT, "no-such-folder")),
        ("other-account", folder_arn(OTHER_ACCOUNT, "parent")),
        (
            "other-region",
            format!("arn:aws:quicksight:eu-west-1:{ACCOUNT}:folder/parent"),
        ),
        (
            "other-resource-type",
            format!("arn:aws:quicksight:{REGION}:{ACCOUNT}:dataset/parent"),
        ),
        ("not-an-arn", "parent".to_string()),
    ];
    for (case, parent_folder_arn) in cases {
        let folder_id = format!("child-of-{case}");
        let (status, body) = create_folder(
            &harness,
            ACCOUNT,
            &folder_id,
            json!({ "ParentFolderArn": parent_folder_arn }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a ParentFolderArn naming no folder expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: an ARN no folder is held under is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "The parent folder {parent_folder_arn} was not found."
            )),
            "case {case:?}: the refusal must name the parent that was not found: {body}"
        );

        let (status, body) = describe_folder(&harness, ACCOUNT, &folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the refused create must persist nothing, got {status} with \
             {body}"
        );
    }

    // An empty ParentFolderArn names no parent at all: the model documents it as
    // creating a root-level folder, not as a parent that cannot be found.
    // Source: smithy_query(query="doc", shape="CreateFolderRequest",
    // member="ParentFolderArn").
    let (status, body) = create_folder(
        &harness,
        ACCOUNT,
        "empty-parent-arn",
        json!({ "ParentFolderArn": "" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an empty ParentFolderArn creates a root-level folder, got {status} with {body}"
    );
    let (status, body) = describe_folder(&harness, ACCOUNT, "empty-parent-arn").await;
    assert_eq!(status, 200, "describe expected 200, got {status} with {body}");
    assert_eq!(
        body["Folder"]["FolderPath"],
        json!([]),
        "a root-level folder reports the empty ancestor path: {body}"
    );
}

/// `DeleteFolder`: `DELETE /accounts/{AwsAccountId}/folders/{FolderId}`.
/// Source: smithy_query(query="operation", operation="DeleteFolder").
async fn delete_folder(harness: &Harness, account: &str, folder_id: &str) -> (u16, Value) {
    harness
        .call(
            "DELETE",
            &format!("/accounts/{account}/folders/{folder_id}"),
            None,
        )
        .await
}

/// Whether the store still holds the folder `(account, folder_id)`.
fn holds_folder(harness: &Harness, account: &str, folder_id: &str) -> bool {
    let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
    let store = state.lock();
    store.folder(account, folder_id).is_some()
}

#[tokio::test]
async fn deleting_a_folder_removes_it_and_every_later_read_reports_the_miss() {
    let harness = Harness::new();

    // Rows vary what the rule generalises over: the form and length of the `FolderId`
    // (len 1..2048 over the pattern `^[\w\-]+$`), whether the folder was created with a
    // name, whether it sits at the root or inside another folder, and the account.
    // Source: smithy_query(query="operation", operation="DeleteFolder").
    let nested_parent = "delete-parent";
    let (status, body) = create_folder(&harness, ACCOUNT, nested_parent, json!({})).await;
    assert_eq!(
        status, 200,
        "the parent of the nested row expected 200, got {status} with {body}"
    );
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "named-root-folder",
            ACCOUNT,
            "reports".to_string(),
            json!({ "Name": "Reports" }),
        ),
        ("unnamed-folder", ACCOUNT, "no_name-1".to_string(), json!({})),
        (
            "nested-folder",
            ACCOUNT,
            "nested-child".to_string(),
            json!({ "ParentFolderArn": folder_arn(ACCOUNT, nested_parent) }),
        ),
        (
            "long-id",
            ACCOUNT,
            "d".repeat(2048),
            json!({ "Name": "Long id" }),
        ),
        (
            "another-account",
            OTHER_ACCOUNT,
            "reports".to_string(),
            json!({ "Name": "Reports" }),
        ),
    ];

    for (case, account, folder_id, create_body) in &rows {
        let folder_id = folder_id.as_str();
        let (status, body) = create_folder(&harness, account, folder_id, create_body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture create expected 200, got {status} with {body}"
        );

        let (status, body) = delete_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: deleting the folder expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Arn"],
            json!(folder_arn(account, folder_id)),
            "case {case:?}: the delete reports the deleted folder's ARN: {body}"
        );
        assert_eq!(
            body["FolderId"],
            json!(folder_id),
            "case {case:?}: the delete reports the deleted folder's id: {body}"
        );
        assert!(
            body["RequestId"]
                .as_str()
                .is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: the delete reported no RequestId: {body}"
        );
        assert!(
            !holds_folder(&harness, account, folder_id),
            "case {case:?}: the deleted folder is still in the store"
        );

        // Reading the deleted folder is the miss every folder read reports, twice over:
        // the refusal changes nothing.
        for attempt in ["first-read", "second-read"] {
            let (status, body) = describe_folder(&harness, account, folder_id).await;
            assert_eq!(
                status, 404,
                "case {case:?}/{attempt}: describing a deleted folder expected 404, got \
                 {status} with {body}"
            );
            assert_eq!(
                body["__type"],
                json!("ResourceNotFoundException"),
                "case {case:?}/{attempt}: a deleted folder is not found: {body}"
            );
            assert_eq!(
                body["Message"],
                json!(format!("Folder: {} not found", folder_arn(account, folder_id))),
                "case {case:?}/{attempt}: the refusal must name the folder by its ARN: \
                 {body}"
            );
        }

        // A second delete has nothing to delete, so it is the same miss.
        let (status, body) = delete_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: deleting a folder twice expected 404, got {status} with {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!("Folder: {} not found", folder_arn(account, folder_id))),
            "case {case:?}: the repeated delete must name the folder by its ARN: {body}"
        );

        // The id is free again: the create that would have collided now succeeds.
        let (status, body) = create_folder(&harness, account, folder_id, create_body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: re-creating the deleted folder expected 200, got {status} \
             with {body}"
        );
    }

    // An id no folder is held under is nothing to delete.
    for (case, account, folder_id) in [
        ("never-created", ACCOUNT, "no-such-folder"),
        ("held-by-another-account-only", "123456789012", "reports"),
    ] {
        let (status, body) = delete_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: deleting a folder that does not exist expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!("Folder: {} not found", folder_arn(account, folder_id))),
            "case {case:?}: the refusal must name the folder by its ARN: {body}"
        );
    }

    // The folders the rows did not delete are still there: one delete removes one
    // folder.
    let (status, body) = describe_folder(&harness, ACCOUNT, nested_parent).await;
    assert_eq!(
        status, 200,
        "the parent folder must still answer, got {status} with {body}"
    );
}

#[tokio::test]
async fn update_folder_persists_the_new_name_and_describe_reports_it() {
    let harness = Harness::new();
    let names = updated_names();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let folder_id = row.folder_id.as_str();
        let arn = folder_arn(row.account, folder_id);

        let (status, body) = create_folder(&harness, row.account, folder_id, row.body()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: create expected 200, got {status} with body {body}"
        );
        let (status, body) = describe_folder(&harness, row.account, folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with body {body}"
        );
        let created_time = body["Folder"]["CreatedTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: describe reported no CreatedTime: {body}"));

        // Two updates in a row, each naming the folder something else: whatever the
        // latest accepted update submitted is what the folder is called. `previous_time`
        // is the instant the folder was last written at before each update, so each one
        // must move `LastUpdatedTime` strictly past it.
        let mut previous_time = created_time;
        for step in ["first-update", "second-update"] {
            let (label, name) = &names[if step == "first-update" {
                index % names.len()
            } else {
                (index + 1) % names.len()
            }];
            // With the clock strictly past the folder's last write, an update that does
            // not touch `LastUpdatedTime` cannot pass the assertion below.
            wait_for_clock_past(case, previous_time);
            let (status, body) = update_folder(
                &harness,
                row.account,
                folder_id,
                json!({ "Name": name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}/{step}/{label}: update expected 200, got {status} with {body}"
            );
            assert_eq!(
                body["Arn"],
                json!(arn),
                "case {case:?}/{step}/{label}: the update must report the folder's Arn: {body}"
            );
            assert_eq!(
                body["FolderId"],
                json!(folder_id),
                "case {case:?}/{step}/{label}: the update must report the FolderId it \
                 addressed: {body}"
            );
            assert!(
                body["RequestId"]
                    .as_str()
                    .is_some_and(|request_id| !request_id.is_empty()),
                "case {case:?}/{step}/{label}: the update reported no RequestId: {body}"
            );

            // The read that follows answers from the record the update wrote, and twice
            // over: a read changes nothing.
            for attempt in ["first-read", "second-read"] {
                let (status, body) = describe_folder(&harness, row.account, folder_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}/{step}/{label}/{attempt}: describe expected 200, got \
                     {status} with {body}"
                );
                let reported = &body["Folder"];
                assert_eq!(
                    reported["Name"],
                    json!(name),
                    "case {case:?}/{step}/{label}/{attempt}: describe must report the \
                     Name the update submitted, not the one the folder was created \
                     with: {body}"
                );
                // The update names the folder and nothing else, so the rest of the
                // record is the one the create left.
                assert_eq!(
                    reported["Arn"],
                    json!(arn),
                    "case {case:?}/{step}/{label}/{attempt}: an update must not change \
                     the folder's Arn: {body}"
                );
                assert_eq!(
                    reported["FolderType"],
                    json!(row.expected_folder_type),
                    "case {case:?}/{step}/{label}/{attempt}: an update must not change \
                     the FolderType: {body}"
                );
                assert_eq!(
                    reported["SharingModel"],
                    json!(row.expected_sharing_model),
                    "case {case:?}/{step}/{label}/{attempt}: an update must not change \
                     the SharingModel: {body}"
                );
                assert_eq!(
                    reported["CreatedTime"].as_f64(),
                    Some(created_time),
                    "case {case:?}/{step}/{label}/{attempt}: an update must not change \
                     the CreatedTime: {body}"
                );
                let last_updated_time = reported["LastUpdatedTime"].as_f64().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}/{step}/{label}/{attempt}: describe reported no \
                         LastUpdatedTime: {body}"
                    )
                });
                assert!(
                    last_updated_time > previous_time,
                    "case {case:?}/{step}/{label}/{attempt}: the update is what last \
                     addressed the folder, so LastUpdatedTime must be past the folder's \
                     previous write at {previous_time}, got {last_updated_time}"
                );
                assert!(
                    last_updated_time >= created_time,
                    "case {case:?}/{step}/{label}/{attempt}: LastUpdatedTime must not be \
                     before CreatedTime {created_time}, got {last_updated_time}"
                );
            }

            // And the stored record is what those reads answered from: both the name the
            // update submitted and the instant it recorded.
            let stored_time = {
                let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
                let store = state.lock();
                let record = store.folder(row.account, folder_id).unwrap_or_else(|| {
                    panic!("case {case:?}/{step}/{label}: the folder is not stored")
                });
                assert_eq!(
                    record.name.as_deref(),
                    Some(name.as_str()),
                    "case {case:?}/{step}/{label}: the submitted Name did not reach the \
                     stored record"
                );
                let stored_time = record.last_updated_time.as_secs_f64();
                assert!(
                    stored_time > previous_time,
                    "case {case:?}/{step}/{label}: the update must write the instant it \
                     ran at to the stored record, past the folder's previous write at \
                     {previous_time}, got {stored_time}"
                );
                stored_time
            };
            // The next update has to move the instant past this one.
            previous_time = stored_time;
        }
    }
}

#[tokio::test]
async fn update_folder_touches_only_the_folder_it_addresses() {
    let harness = Harness::new();
    let shared = "shared-folder-id";
    let other_id = "untouched";

    // Three folders: the one the update addresses, a second one in the same account,
    // and one of the same id in another account.
    for (case, account, folder_id) in [
        ("addressed", ACCOUNT, shared),
        ("same-account-other-id", ACCOUNT, other_id),
        ("other-account-same-id", OTHER_ACCOUNT, shared),
    ] {
        let (status, body) =
            create_folder(&harness, account, folder_id, json!({ "Name": "Created" })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: create expected 200, got {status} with {body}"
        );
    }

    let (status, body) = update_folder(
        &harness,
        ACCOUNT,
        shared,
        json!({ "Name": "Only this one" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the update expected 200, got {status} with {body}"
    );

    for (case, account, folder_id, expected_name) in [
        ("addressed", ACCOUNT, shared, "Only this one"),
        ("same-account-other-id", ACCOUNT, other_id, "Created"),
        ("other-account-same-id", OTHER_ACCOUNT, shared, "Created"),
    ] {
        let (status, body) = describe_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Folder"]["Name"],
            json!(expected_name),
            "case {case:?}: an update addresses one folder of one account: {body}"
        );
    }
}

#[tokio::test]
async fn updating_a_nested_folder_leaves_its_ancestor_path_alone() {
    let harness = Harness::new();

    let (status, body) = create_folder(&harness, ACCOUNT, "parent", json!({ "Name": "Parent" })).await;
    assert_eq!(
        status, 200,
        "the parent create expected 200, got {status} with {body}"
    );
    let parent_arn = folder_arn(ACCOUNT, "parent");
    let (status, body) = create_folder(
        &harness,
        ACCOUNT,
        "child",
        json!({ "Name": "Child", "ParentFolderArn": parent_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the child create expected 200, got {status} with {body}"
    );

    let (status, body) = update_folder(&harness, ACCOUNT, "child", json!({ "Name": "Renamed" })).await;
    assert_eq!(
        status, 200,
        "the child update expected 200, got {status} with {body}"
    );

    let (status, body) = describe_folder(&harness, ACCOUNT, "child").await;
    assert_eq!(status, 200, "describe expected 200, got {status} with {body}");
    assert_eq!(
        body["Folder"]["Name"],
        json!("Renamed"),
        "the renamed child must report the Name the update submitted: {body}"
    );
    assert_eq!(
        body["Folder"]["FolderPath"],
        json!([parent_arn]),
        "renaming a folder must not move it: its ancestor path is still the folder \
         above it: {body}"
    );
    let (status, body) = describe_folder(&harness, ACCOUNT, "parent").await;
    assert_eq!(status, 200, "describe expected 200, got {status} with {body}");
    assert_eq!(
        body["Folder"]["Name"],
        json!("Parent"),
        "renaming the child must not rename the parent: {body}"
    );
}

#[tokio::test]
async fn only_an_empty_folder_is_deleted() {
    let harness = Harness::new();

    // A chain root -> middle -> leaf: every folder above the leaf holds the ones below
    // it, so only the leaf is empty.
    let mut parent: Option<String> = None;
    for folder_id in ["chain-root", "chain-middle", "chain-leaf"] {
        let mut body = json!({ "Name": folder_id });
        if let Some(parent_folder_arn) = &parent {
            body["ParentFolderArn"] = json!(parent_folder_arn);
        }
        let (status, response) = create_folder(&harness, ACCOUNT, folder_id, body).await;
        assert_eq!(
            status, 200,
            "fixture {folder_id:?}: expected 200, got {status} with {response}"
        );
        parent = Some(folder_arn(ACCOUNT, folder_id));
    }

    // Rows: the folder holding a folder directly, and the one holding it two levels
    // down. Each is refused, and the folder it holds is still there afterwards.
    for (case, folder_id) in [
        ("holds-a-folder-two-levels-down", "chain-root"),
        ("holds-a-folder-directly", "chain-middle"),
    ] {
        let (status, body) = delete_folder(&harness, ACCOUNT, folder_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: deleting a folder that holds folders expected 400, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("PreconditionNotMetException"),
            "case {case:?}: only an empty folder is deleted: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Folder: {} is not empty",
                folder_arn(ACCOUNT, folder_id)
            )),
            "case {case:?}: the refusal must name the folder that is not empty: {body}"
        );
        assert!(
            holds_folder(&harness, ACCOUNT, folder_id),
            "case {case:?}: the refused delete must leave the folder where it was"
        );
    }

    // The leaf holds nothing, so it goes — and its parent, now empty, goes next.
    for folder_id in ["chain-leaf", "chain-middle", "chain-root"] {
        let (status, body) = delete_folder(&harness, ACCOUNT, folder_id).await;
        assert_eq!(
            status, 200,
            "case {folder_id:?}: an empty folder is deleted, got {status} with {body}"
        );
    }
    assert_eq!(
        {
            let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
            let store = state.lock();
            store.folder_count(ACCOUNT)
        },
        0,
        "the whole chain was deleted from the leaf upwards"
    );
}

#[tokio::test]
async fn updating_a_folder_id_no_folder_is_held_under_is_not_found() {
    let harness = Harness::new();

    let (status, body) = create_folder(&harness, ACCOUNT, "exists", json!({ "Name": "Exists" })).await;
    assert_eq!(status, 200, "the create expected 200, got {status} with {body}");

    // An unknown id of an account that holds folders, the id of another account's
    // folder, and an id of an account that holds none.
    for (case, account, folder_id) in [
        ("unknown-id", ACCOUNT, "never-created"),
        ("other-account", OTHER_ACCOUNT, "exists"),
        ("account-with-nothing", "123456789012", "exists"),
    ] {
        let (status, body) =
            update_folder(&harness, account, folder_id, json!({ "Name": "New name" })).await;
        assert_eq!(
            status, 404,
            "case {case:?}: updating a folder that does not exist expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: an id no folder is held under is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!("Folder: {} not found", folder_arn(account, folder_id))),
            "case {case:?}: the refusal must name the folder that was not found by its \
             ARN: {body}"
        );

        // A refused update creates nothing.
        let (status, body) = describe_folder(&harness, account, folder_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused update must persist nothing, got {status} with \
             {body}"
        );
    }

    // And the folder the account does hold is untouched by the refusals.
    let (status, body) = describe_folder(&harness, ACCOUNT, "exists").await;
    assert_eq!(status, 200, "describe expected 200, got {status} with {body}");
    assert_eq!(
        body["Folder"]["Name"],
        json!("Exists"),
        "a refused update must not change another folder: {body}"
    );
}
