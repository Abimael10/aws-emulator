// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeletePermission` and `DeletePermissionVersion` and what
//! they make the reads and writes of a managed permission report afterwards.
//!
//! The rule the rows below pin: deleting a customer managed permission is performed
//! asynchronously and deletes the permission with every version written for it — the
//! permission leaves the attachable statuses for `DELETING` when the call is answered
//! and reports `DELETED` once that move falls due — and every read of the permission,
//! at its default version or at an earlier one by number, reports the status the delete
//! has reached while still reporting the policy document and version number that were
//! stored. From the delete on the permission is no longer one a resource share can be
//! associated with nor one a new version can be written for, and once the delete has
//! completed its name is free for another permission. A delete of a permission a caller
//! may not delete — one Amazon Web Services manages, or one a live resource share is
//! associated with — is refused with the modelled error that words that refusal and
//! changes nothing.
//!
//! So the rows vary along what the rule generalises over:
//!
//! * HOW MANY versions the permission carries when it is deleted: one, two and the
//!   maximum a permission may hold — and WHICH resource type it is for, so the rule is
//!   not read off one type's spelling;
//! * WHICH window the next call falls in: while the delete is in flight (`DELETING`)
//!   and after it has completed (`DELETED`);
//! * WHICH operation the deleted permission is then named to: a read of it, a read of
//!   one of its earlier versions, a version write, an attach to a share, and a create
//!   of a permission of the same name;
//! * WHAT a `clientToken` answers for: a repeat of the same delete, and a token reused
//!   for a different permission;
//! * WHICH rule a refused delete breaks: a permission Amazon Web Services manages, one
//!   a live share is associated with, an ARN no permission answers to, a value that is
//!   not a managed permission ARN, and an empty `clientToken`.
//!
//! The second rule, which the version rows at the end of this file pin: ONE version of a
//! customer managed permission may be deleted only when that version is neither the
//! permission's default version nor one a live resource share is governed by; such a
//! delete is performed asynchronously, so that version leaves the attachable statuses
//! for `DELETING` when the call is answered and reports `DELETED` once that move falls
//! due, every other version is left exactly as it was, and the deleted version no longer
//! counts against the versions one permission may carry. Every other version delete is
//! refused with the modelled error that words that refusal and writes nothing.
//!
//! So those rows vary along HOW MANY versions the permission carries, WHICH of them is
//! deleted, WHICH resource type it is for, WHAT a `clientToken` answers for, and WHICH
//! rule a refused version delete breaks.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    aws_managed_permission_arn, FIRST_PERMISSION_VERSION, MAX_PERMISSION_VERSIONS,
    PERMISSION_DELETED_AFTER_MILLIS, PERMISSION_STATUS_ATTACHABLE, PERMISSION_STATUS_DELETED,
    PERMISSION_STATUS_DELETING, PERMISSION_TYPE_CUSTOMER_MANAGED,
};
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

/// How long a poll for the delete to complete keeps looking. Nothing sleeps
/// server-side: the owed move is materialised by the first call at or after it falls
/// due, so the loop waits for the CONDITION and this is only the point at which it
/// gives up — deliberately far beyond the span itself, so a loaded host still passes.
const DELETE_POLL_DEADLINE: Duration =
    Duration::from_millis((PERMISSION_DELETED_AFTER_MILLIS as u64) * 20);

/// How long the poll loop waits between looks.
const DELETE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Actions of the AWS Glue namespace, one more of which each version of the row at the
/// version limit allows.
const GLUE_ACTIONS: [&str; 5] = [
    "glue:GetDatabase",
    "glue:GetDatabases",
    "glue:GetTable",
    "glue:GetTables",
    "glue:GetPartitions",
];

async fn create_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermission", Some(body)).await
}

async fn create_permission_version(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermissionversion", Some(body)).await
}

async fn get_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getpermission", Some(body)).await
}

async fn attach(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        "/associateresourcesharepermission",
        Some(body),
    )
    .await
}

/// `DeletePermission`, whose members are query parameters
/// (`smithy_query(query="operation",operation="DeletePermission")`).
async fn delete_permission(
    router: &MultiServiceRouter,
    permission_arn: &str,
    client_token: Option<&str>,
) -> (u16, Value) {
    let mut uri = format!(
        "/deletepermission?permissionArn={}",
        query_escape(permission_arn)
    );
    if let Some(token) = client_token {
        uri.push_str(&format!("&clientToken={}", query_escape(token)));
    }
    call(router, "DELETE", &uri, None).await
}

/// A policy template allowing `actions`, the shape `CreatePermission$policyTemplate`
/// documents: an `Effect` of `ALLOW` and an `Action` list.
fn policy_template(actions: &[&str]) -> String {
    json!({"Effect": "ALLOW", "Action": actions}).to_string()
}

/// The ARN a customer managed permission of this name is minted under.
fn customer(name: &str) -> String {
    format!("arn:aws:ram:{REGION}:{ACCOUNT}:permission/{name}")
}

/// A string member of a reported permission.
fn field(case: &str, permission: &Value, member: &str) -> String {
    permission
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
        .to_string()
}

/// A boolean member of a reported permission.
fn flag(case: &str, permission: &Value, member: &str) -> bool {
    permission
        .get(member)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
}

/// Read the permission (its default version, or `version` when named), asserting HTTP
/// 200, and answer the `ResourceSharePermissionDetail` it reported.
async fn read(
    router: &MultiServiceRouter,
    case: &str,
    permission_arn: &str,
    version: Option<i32>,
) -> Value {
    let mut request = json!({"permissionArn": permission_arn});
    if let Some(version) = version {
        request["permissionVersion"] = json!(version);
    }
    let (status, body) = get_permission(router, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading {permission_arn} version {version:?}, got \
         {status} with {body:?}"
    );
    body.get("permission")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the read reported no permission, got {body:?}"))
}

/// Mint a customer managed permission for `resource_type`, write one further version for
/// every template after the first, and answer its ARN.
async fn mint(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    resource_type: &str,
    templates: &[String],
) -> String {
    let (first, later) = templates
        .split_first()
        .unwrap_or_else(|| panic!("case {case:?}: a permission is minted with a template"));
    let (status, minted) = create_permission(
        router,
        &json!({
            "name": name,
            "resourceType": resource_type,
            "policyTemplate": first,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 minting {name}, got {status} with {minted:?}"
    );
    let arn = customer(name);
    for (index, template) in later.iter().enumerate() {
        let (status, written) = create_permission_version(
            router,
            &json!({"permissionArn": arn, "policyTemplate": template}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 writing version {}, got {status} with {written:?}",
            index + 2
        );
    }
    arn
}

/// Delete the permission, asserting the delete was served and answers the status the
/// permission has reached, and answer that status.
async fn delete_ok(
    router: &MultiServiceRouter,
    case: &str,
    permission_arn: &str,
    client_token: Option<&str>,
) -> String {
    let (status, answered) = delete_permission(router, permission_arn, client_token).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 deleting {permission_arn}, got {status} with \
         {answered:?}"
    );
    assert_eq!(
        answered.get("returnValue").and_then(Value::as_bool),
        Some(true),
        "case {case:?}: a served delete reports returnValue true, got {answered:?}"
    );
    let token = answered
        .get("clientToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a delete reports a clientToken, got {answered:?}"));
    match client_token {
        Some(supplied) => assert_eq!(
            token, supplied,
            "case {case:?}: a delete reports back the clientToken it was given, got {answered:?}"
        ),
        None => assert!(
            !token.is_empty(),
            "case {case:?}: a delete that was given no clientToken generates one, got {answered:?}"
        ),
    }
    answered
        .get("permissionStatus")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a delete reports a permissionStatus, got {answered:?}")
        })
        .to_string()
}

/// Look at the permission until it reports `PERMISSION_STATUS_DELETED`, or the deadline
/// passes, and answer the detail that reported it.
///
/// Waits for the CONDITION rather than for a span: the move to `DELETED` is one the
/// clock owes and the first read at or after it falls due is what materialises it.
async fn poll_until_deleted(router: &MultiServiceRouter, case: &str, permission_arn: &str) -> Value {
    let give_up_at = Instant::now() + DELETE_POLL_DEADLINE;
    loop {
        let detail = read(router, case, permission_arn, None).await;
        let status = field(case, &detail, "status");
        if status == PERMISSION_STATUS_DELETED {
            return detail;
        }
        assert_eq!(
            status, PERMISSION_STATUS_DELETING,
            "case {case:?}: a permission being deleted reports DELETING until it is DELETED, got \
             {detail:?}"
        );
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: expected {permission_arn} to report {PERMISSION_STATUS_DELETED}, still \
             {status:?} after {DELETE_POLL_DEADLINE:?}"
        );
        std::thread::sleep(DELETE_POLL_INTERVAL);
    }
}

/// One row of the delete table: a permission of some resource type carrying some number
/// of versions.
struct DeleteRow {
    case: &'static str,
    name: &'static str,
    resource_type: &'static str,
    /// One template per version, the first one the create wrote.
    templates: Vec<String>,
    client_token: Option<&'static str>,
}

#[tokio::test]
async fn a_deleted_permission_reports_deleting_then_deleted_at_every_version() {
    // Each resource type below is one `ListResourceTypes` reports as taking a customer
    // managed permission (`RESOURCE_TYPES`), and each row's actions are of that type's
    // own service namespace.
    let rows = vec![
        DeleteRow {
            case: "one version, no clientToken",
            name: "DeletedComponentReader",
            resource_type: "imagebuilder:Component",
            templates: vec![policy_template(&["imagebuilder:ListComponents"])],
            client_token: None,
        },
        DeleteRow {
            case: "a second version added before the delete",
            name: "DeletedProjectReader",
            resource_type: "codebuild:Project",
            templates: vec![
                policy_template(&["codebuild:BatchGetProjects"]),
                policy_template(&["codebuild:BatchGetProjects", "codebuild:ListBuildsForProject"]),
            ],
            client_token: Some("delete-with-two-versions"),
        },
        DeleteRow {
            case: "the maximum number of versions a permission may carry",
            name: "DeletedDatabaseReader",
            resource_type: "glue:Database",
            // One version per action, each adding the next one: five versions, which
            // is the most a permission may carry (`MAX_PERMISSION_VERSIONS`).
            templates: (0..GLUE_ACTIONS.len().min(MAX_PERMISSION_VERSIONS))
                .map(|index| policy_template(&GLUE_ACTIONS[..=index]))
                .collect(),
            client_token: Some("delete-at-the-version-limit"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, _engine) = emulator_with_engine();
        let arn = mint(&router, case, row.name, row.resource_type, &row.templates).await;
        let latest = i32::try_from(row.templates.len()).expect("a small version count")
            + FIRST_PERMISSION_VERSION
            - 1;
        let latest_template = row
            .templates
            .last()
            .expect("every row carries at least one template")
            .clone();

        // Before the delete the permission is attachable at the version last written,
        // which is the version a read naming none reports.
        let before = read(&router, case, &arn, None).await;
        assert_eq!(
            (
                field(case, &before, "version"),
                field(case, &before, "permission"),
                field(case, &before, "status")
            ),
            (
                latest.to_string(),
                latest_template.clone(),
                PERMISSION_STATUS_ATTACHABLE.to_string()
            ),
            "case {case:?}: a live permission reports its latest version, that version's policy \
             and ATTACHABLE, got {before:?}"
        );

        // The delete is answered with the permission in the non-terminal status.
        let answered = delete_ok(&router, case, &arn, row.client_token).await;
        assert_eq!(
            answered, PERMISSION_STATUS_DELETING,
            "case {case:?}: the delete reports the status the permission has reached, got \
             {answered:?}"
        );
        let deleting = read(&router, case, &arn, None).await;
        assert_eq!(
            (
                field(case, &deleting, "status"),
                field(case, &deleting, "version"),
                field(case, &deleting, "permission")
            ),
            (
                PERMISSION_STATUS_DELETING.to_string(),
                latest.to_string(),
                latest_template.clone()
            ),
            "case {case:?}: a read straight after the delete reports DELETING at the version that \
             was stored, got {deleting:?}"
        );

        // The move to DELETED is the clock's, and the permission keeps reporting what
        // was stored: the delete changes its status, not its versions.
        let deleted = poll_until_deleted(&router, case, &arn).await;
        assert_eq!(
            (
                field(case, &deleted, "version"),
                field(case, &deleted, "permission"),
                field(case, &deleted, "permissionType"),
                flag(case, &deleted, "defaultVersion")
            ),
            (
                latest.to_string(),
                latest_template,
                PERMISSION_TYPE_CUSTOMER_MANAGED.to_string(),
                true
            ),
            "case {case:?}: a deleted permission still reports the version and policy that were \
             stored, got {deleted:?}"
        );

        // "The operation deletes all versions associated with the customer managed
        // permission", so every version reports the delete, each with its own policy.
        for (index, template) in row.templates.iter().enumerate() {
            let version = i32::try_from(index).expect("a small version count") + FIRST_PERMISSION_VERSION;
            let detail = read(&router, case, &arn, Some(version)).await;
            assert_eq!(
                (
                    field(case, &detail, "status"),
                    field(case, &detail, "version"),
                    field(case, &detail, "permission"),
                    flag(case, &detail, "defaultVersion")
                ),
                (
                    PERMISSION_STATUS_DELETED.to_string(),
                    version.to_string(),
                    template.clone(),
                    version == latest
                ),
                "case {case:?}: version {version} of a deleted permission reports DELETED and its \
                 own policy, got {detail:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_permission_on_the_delete_path_is_neither_attachable_nor_versionable() {
    // Both windows of the delete: the call that starts it and the state it reaches.
    for (case, wait_for_deleted) in [
        ("while the delete is in flight", false),
        ("after the delete has completed", true),
    ] {
        let (router, _engine) = emulator_with_engine();
        let arn = mint(
            &router,
            case,
            "AttachedAfterDelete",
            "imagebuilder:Component",
            &[policy_template(&["imagebuilder:ListComponents"])],
        )
        .await;
        let share_arn = arn_of(
            case,
            &create_ok(
                &router,
                case,
                &json!({"name": "share-for-a-deleted-permission", "resourceArns": [LOCAL_RESOURCE]}),
            )
            .await,
        );

        delete_ok(&router, case, &arn, None).await;
        if wait_for_deleted {
            poll_until_deleted(&router, case, &arn).await;
        }

        // A permission being deleted is not one "that can be associated with resource
        // shares", so the attach finds no permission to attach.
        let (status, refused) = attach(
            &router,
            &json!({"resourceShareArn": share_arn, "permissionArn": arn}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "UnknownResourceException".to_string()),
            "case {case:?}: attaching a deleted permission is refused, got {status} with \
             {refused:?}"
        );
        assert!(
            error_message(&refused).contains(&arn),
            "case {case:?}: the refusal names the permission that was asked for, got {refused:?}"
        );

        // Neither is it one there is a version left to add to.
        let (status, refused) = create_permission_version(
            &router,
            &json!({
                "permissionArn": arn,
                "policyTemplate": policy_template(&["imagebuilder:GetComponent"]),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "UnknownResourceException".to_string()),
            "case {case:?}: writing a version of a deleted permission is refused, got {status} \
             with {refused:?}"
        );
        assert!(
            error_message(&refused).contains(&arn),
            "case {case:?}: the refusal names the permission that was asked for, got {refused:?}"
        );

        // The refused calls wrote nothing: the permission still reports its own delete.
        let detail = read(&router, case, &arn, None).await;
        let status = field(case, &detail, "status");
        assert!(
            status == PERMISSION_STATUS_DELETING || status == PERMISSION_STATUS_DELETED,
            "case {case:?}: a deleted permission stays on the delete path, got {detail:?}"
        );
        assert_eq!(
            field(case, &detail, "version"),
            FIRST_PERMISSION_VERSION.to_string(),
            "case {case:?}: the refused version write added no version, got {detail:?}"
        );
    }
}

#[tokio::test]
async fn a_delete_a_caller_may_not_make_is_refused_and_changes_nothing() {
    let (router, _engine) = emulator_with_engine();

    // A permission a live resource share is associated with: "you can delete a customer
    // managed permission only if it isn't attached to any resource share".
    let attached_arn = mint(
        &router,
        "a permission a share is associated with",
        "AttachedComponentReader",
        "imagebuilder:Component",
        &[policy_template(&["imagebuilder:ListComponents"])],
    )
    .await;
    let share_arn = arn_of(
        "a permission a share is associated with",
        &create_ok(
            &router,
            "a permission a share is associated with",
            &json!({
                "name": "share-holding-a-permission",
                "resourceArns": [LOCAL_RESOURCE],
                "permissionArns": [attached_arn],
            }),
        )
        .await,
    );
    // A permission nothing is associated with, so the refusals below are about what
    // they name rather than about the store being empty.
    let free_arn = mint(
        &router,
        "a permission nothing holds",
        "FreeComponentReader",
        "imagebuilder:Component",
        &[policy_template(&["imagebuilder:ListComponents"])],
    )
    .await;

    // (case, the permissionArn the delete names, the clientToken it carries, the
    // modelled error that refusal is worded with, and what the message must name)
    let rows: Vec<(&str, String, Option<&str>, &str, String)> = vec![
        (
            "a permission Amazon Web Services manages",
            aws_managed_permission_arn("AWSRAMDefaultPermissionCodeBuildProject"),
            None,
            "OperationNotPermittedException",
            aws_managed_permission_arn("AWSRAMDefaultPermissionCodeBuildProject"),
        ),
        (
            "another permission Amazon Web Services manages",
            aws_managed_permission_arn("AWSRAMDefaultPermissionGlueDatabase"),
            Some("delete-an-aws-managed-permission"),
            "OperationNotPermittedException",
            aws_managed_permission_arn("AWSRAMDefaultPermissionGlueDatabase"),
        ),
        (
            "a permission a live resource share is associated with",
            attached_arn.clone(),
            None,
            "OperationNotPermittedException",
            share_arn.clone(),
        ),
        (
            "an ARN no permission answers to",
            customer("NeverCreated"),
            None,
            "UnknownResourceException",
            customer("NeverCreated"),
        ),
        (
            "a value that is not a managed permission ARN",
            format!("arn:aws:ram:{REGION}:{ACCOUNT}:resource-share/not-a-permission"),
            None,
            "MalformedArnException",
            "permissionArn".to_string(),
        ),
        (
            "a value that is not an ARN at all",
            "ComponentReader".to_string(),
            None,
            "MalformedArnException",
            "permissionArn".to_string(),
        ),
        (
            "an empty clientToken",
            free_arn.clone(),
            Some(""),
            "InvalidClientTokenException",
            "clientToken".to_string(),
        ),
    ];

    for (case, permission_arn, client_token, expected_code, must_name) in &rows {
        let (status, refused) = delete_permission(&router, permission_arn, *client_token).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, (*expected_code).to_string()),
            "case {case:?}: expected a 400 {expected_code}, got {status} with {refused:?}"
        );
        assert!(
            error_message(&refused).contains(must_name.as_str()),
            "case {case:?}: expected the message to name {must_name:?}, got {refused:?}"
        );
    }

    // Nothing above deleted anything: both permissions are still attachable.
    for (case, arn) in [
        ("the permission a share holds", &attached_arn),
        ("the permission nothing holds", &free_arn),
    ] {
        let detail = read(&router, case, arn, None).await;
        assert_eq!(
            field(case, &detail, "status"),
            PERMISSION_STATUS_ATTACHABLE,
            "case {case:?}: a refused delete leaves the permission attachable, got {detail:?}"
        );
    }
}

#[tokio::test]
async fn repeating_a_delete_deletes_nothing_further() {
    let case = "a repeated delete";
    let (router, _engine) = emulator_with_engine();
    let first = mint(
        &router,
        case,
        "RepeatedlyDeleted",
        "imagebuilder:Component",
        &[policy_template(&["imagebuilder:ListComponents"])],
    )
    .await;
    let second = mint(
        &router,
        case,
        "DeletedUnderAnotherToken",
        "codebuild:Project",
        &[policy_template(&["codebuild:BatchGetProjects"])],
    )
    .await;

    let token = "one-delete-token";
    assert_eq!(
        delete_ok(&router, case, &first, Some(token)).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: the delete starts the move"
    );
    // The same request under the same token: the first call's delete is the answer, and
    // the status is the one the permission has reached since.
    assert_eq!(
        delete_ok(&router, case, &first, Some(token)).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: a retry under the same token reports the same delete"
    );
    // And without a token: deleting is idempotent for as long as the record exists.
    assert_eq!(
        delete_ok(&router, case, &first, None).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: a second delete of the same permission deletes nothing further"
    );

    // The same token for a DIFFERENT permission is a different request.
    let (status, refused) = delete_permission(&router, &second, Some(token)).await;
    assert_eq!(
        (status, error_code(&refused)),
        (400, "IdempotentParameterMismatchException".to_string()),
        "case {case:?}: a token reused for another permission is refused, got {status} with \
         {refused:?}"
    );
    let detail = read(&router, case, &second, None).await;
    assert_eq!(
        field(case, &detail, "status"),
        PERMISSION_STATUS_ATTACHABLE,
        "case {case:?}: the refused delete left the other permission attachable, got {detail:?}"
    );

    // Once the delete has completed, repeating it still reports the delete that
    // happened rather than a client error.
    poll_until_deleted(&router, case, &first).await;
    assert_eq!(
        delete_ok(&router, case, &first, None).await,
        PERMISSION_STATUS_DELETED,
        "case {case:?}: a delete of an already deleted permission reports DELETED"
    );
}

#[tokio::test]
async fn the_name_of_a_deleted_permission_is_free_again() {
    let case = "a permission created again under a deleted name";
    let (router, _engine) = emulator_with_engine();
    let name = "ReusedComponentReader";
    let first_template = policy_template(&["imagebuilder:ListComponents"]);
    let arn = mint(
        &router,
        case,
        name,
        "imagebuilder:Component",
        std::slice::from_ref(&first_template),
    )
    .await;

    delete_ok(&router, case, &arn, None).await;

    // While the delete is in flight the name is still that permission's. The modelled
    // `PermissionAlreadyExistsException` carries HTTP 409
    // (`smithy_query(query="operation",operation="CreatePermission")`).
    let (status, refused) = create_permission(
        &router,
        &json!({
            "name": name,
            "resourceType": "imagebuilder:Component",
            "policyTemplate": first_template,
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        (409, "PermissionAlreadyExistsException".to_string()),
        "case {case:?}: a name a permission still being deleted holds is taken, got {status} with \
         {refused:?}"
    );

    poll_until_deleted(&router, case, &arn).await;

    // Once it is deleted the name is free, and what the create stores is what the read
    // reports: a permission at its first version, attachable, with the new policy.
    let reused_template = policy_template(&["imagebuilder:GetComponent"]);
    let (status, created) = create_permission(
        &router,
        &json!({
            "name": name,
            "resourceType": "imagebuilder:Component",
            "policyTemplate": reused_template,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a permission under a deleted name, got {status} \
         with {created:?}"
    );
    let detail = read(&router, case, &arn, None).await;
    assert_eq!(
        (
            field(case, &detail, "version"),
            field(case, &detail, "permission"),
            field(case, &detail, "status")
        ),
        (
            FIRST_PERMISSION_VERSION.to_string(),
            reused_template,
            PERMISSION_STATUS_ATTACHABLE.to_string()
        ),
        "case {case:?}: the permission created under the freed name reads back as a live one, got \
         {detail:?}"
    );
}

/// `DeletePermissionVersion`, whose members are query parameters
/// (`smithy_query(query="operation",operation="DeletePermissionVersion")`).
async fn delete_permission_version(
    router: &MultiServiceRouter,
    permission_arn: &str,
    permission_version: i32,
    client_token: Option<&str>,
) -> (u16, Value) {
    let mut uri = format!(
        "/deletepermissionversion?permissionArn={}&permissionVersion={permission_version}",
        query_escape(permission_arn)
    );
    if let Some(token) = client_token {
        uri.push_str(&format!("&clientToken={}", query_escape(token)));
    }
    call(router, "DELETE", &uri, None).await
}

/// Delete one version, asserting the delete was served the way the model describes the
/// response, and answer the `permissionStatus` it reported.
async fn delete_version_ok(
    router: &MultiServiceRouter,
    case: &str,
    permission_arn: &str,
    permission_version: i32,
    client_token: Option<&str>,
) -> String {
    let (status, answered) =
        delete_permission_version(router, permission_arn, permission_version, client_token).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 deleting version {permission_version} of \
         {permission_arn}, got {status} with {answered:?}"
    );
    assert_eq!(
        answered.get("returnValue").and_then(Value::as_bool),
        Some(true),
        "case {case:?}: a served version delete reports returnValue true, got {answered:?}"
    );
    let token = answered
        .get("clientToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a version delete reports a clientToken, got {answered:?}")
        });
    match client_token {
        Some(supplied) => assert_eq!(
            token, supplied,
            "case {case:?}: a version delete reports back the clientToken it was given, got \
             {answered:?}"
        ),
        None => assert!(
            !token.is_empty(),
            "case {case:?}: a version delete given no clientToken generates one, got {answered:?}"
        ),
    }
    answered
        .get("permissionStatus")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a version delete reports a permissionStatus, got {answered:?}")
        })
        .to_string()
}

/// The `(version, status)` pair of every version `ListPermissionVersions` reports for
/// the permission.
async fn version_statuses(
    router: &MultiServiceRouter,
    case: &str,
    permission_arn: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        "POST",
        "/listpermissionversions",
        Some(&json!({"permissionArn": permission_arn})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 listing the versions of {permission_arn}, got {status} \
         with {body:?}"
    );
    body.get("permissions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the list reported no permissions, got {body:?}"))
        .iter()
        .map(|permission| {
            (
                field(case, permission, "version"),
                field(case, permission, "status"),
            )
        })
        .collect()
}

/// Look at one version of the permission until it reports `PERMISSION_STATUS_DELETED`,
/// or the deadline passes, and answer the detail that reported it.
///
/// Waits for the CONDITION rather than for a span, exactly as [`poll_until_deleted`]
/// does for a whole permission.
async fn poll_until_version_deleted(
    router: &MultiServiceRouter,
    case: &str,
    permission_arn: &str,
    version: i32,
) -> Value {
    let give_up_at = Instant::now() + DELETE_POLL_DEADLINE;
    loop {
        let detail = read(router, case, permission_arn, Some(version)).await;
        let status = field(case, &detail, "status");
        if status == PERMISSION_STATUS_DELETED {
            return detail;
        }
        assert_eq!(
            status, PERMISSION_STATUS_DELETING,
            "case {case:?}: a version being deleted reports DELETING until it is DELETED, got \
             {detail:?}"
        );
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: expected version {version} of {permission_arn} to report \
             {PERMISSION_STATUS_DELETED}, still {status:?} after {DELETE_POLL_DEADLINE:?}"
        );
        std::thread::sleep(DELETE_POLL_INTERVAL);
    }
}

/// The version numbers a list of `(version, status)` pairs reported, in the order they
/// were reported.
fn numbers_of(listed: &[(String, String)]) -> Vec<String> {
    listed
        .iter()
        .map(|(version, _)| version.clone())
        .collect()
}

/// Whether a reported `PermissionStatus` is one a delete puts a permission or a version
/// in: the non-terminal `DELETING` or the terminal `DELETED`.
fn on_the_delete_path(status: &str) -> bool {
    status == PERMISSION_STATUS_DELETING || status == PERMISSION_STATUS_DELETED
}

/// One row of the refused-version-delete table: the version delete a caller may not
/// make, the modelled error that refusal is worded with, and what its message must name
/// — so a refusal is about what the caller asked for.
struct VersionRefusalRow {
    case: &'static str,
    permission_arn: String,
    permission_version: i32,
    client_token: Option<&'static str>,
    expected_code: &'static str,
    must_name: String,
}

/// One row of the version-delete table: a permission of some resource type carrying some
/// number of versions, one of which the row deletes.
struct VersionDeleteRow {
    case: &'static str,
    name: &'static str,
    resource_type: &'static str,
    /// One template per version, the first one the create wrote.
    templates: Vec<String>,
    /// The version this row deletes. Never the last one written, which is the
    /// permission's default version.
    delete: i32,
    client_token: Option<&'static str>,
}

#[tokio::test]
async fn a_deleted_permission_version_reports_deleting_then_deleted_and_frees_room() {
    // Each resource type below is one `ListResourceTypes` reports as taking a customer
    // managed permission (`RESOURCE_TYPES`), and each row's actions are of that type's
    // own service namespace.
    let rows = vec![
        VersionDeleteRow {
            case: "the first of two versions, no clientToken",
            name: "VersionedComponentReader",
            resource_type: "imagebuilder:Component",
            templates: vec![
                policy_template(&["imagebuilder:ListComponents"]),
                policy_template(&["imagebuilder:ListComponents", "imagebuilder:GetComponent"]),
            ],
            delete: FIRST_PERMISSION_VERSION,
            client_token: None,
        },
        VersionDeleteRow {
            case: "the middle of three versions",
            name: "VersionedProjectReader",
            resource_type: "codebuild:Project",
            templates: vec![
                policy_template(&["codebuild:BatchGetProjects"]),
                policy_template(&["codebuild:BatchGetProjects", "codebuild:ListBuildsForProject"]),
                policy_template(&[
                    "codebuild:BatchGetProjects",
                    "codebuild:ListBuildsForProject",
                    "codebuild:BatchGetBuilds",
                ]),
            ],
            delete: FIRST_PERMISSION_VERSION + 1,
            client_token: Some("delete-the-middle-version"),
        },
        VersionDeleteRow {
            case: "the first of the maximum number of versions a permission may carry",
            name: "VersionedDatabaseReader",
            resource_type: "glue:Database",
            // One version per action, each adding the next one: five versions, which is
            // the most a permission may carry (`MAX_PERMISSION_VERSIONS`).
            templates: (0..GLUE_ACTIONS.len().min(MAX_PERMISSION_VERSIONS))
                .map(|index| policy_template(&GLUE_ACTIONS[..=index]))
                .collect(),
            delete: FIRST_PERMISSION_VERSION,
            client_token: Some("delete-one-of-the-maximum-versions"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, _engine) = emulator_with_engine();
        let arn = mint(&router, case, row.name, row.resource_type, &row.templates).await;
        let count = i32::try_from(row.templates.len()).expect("a small version count");
        let latest = count + FIRST_PERMISSION_VERSION - 1;
        let template_of = |version: i32| {
            row.templates[usize::try_from(version - FIRST_PERMISSION_VERSION)
                .expect("a version at or above the first one")]
            .clone()
        };

        // Before the delete the permission carries every version written, and none of
        // them is on the delete path.
        let before = version_statuses(&router, case, &arn).await;
        assert_eq!(
            numbers_of(&before),
            (FIRST_PERMISSION_VERSION..=latest)
                .map(|version| version.to_string())
                .collect::<Vec<_>>(),
            "case {case:?}: the permission carries every version written, got {before:?}"
        );
        assert!(
            !before
                .iter()
                .any(|(_, status)| on_the_delete_path(status)),
            "case {case:?}: no version is on the delete path before the delete, got {before:?}"
        );
        // A permission already carrying the maximum has no room for another version
        // until one is deleted, which is what deleting one is for.
        if row.templates.len() == MAX_PERMISSION_VERSIONS {
            let (status, refused) = create_permission_version(
                &router,
                &json!({"permissionArn": arn, "policyTemplate": policy_template(&GLUE_ACTIONS)}),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                (
                    400,
                    "PermissionVersionsLimitExceededException".to_string()
                ),
                "case {case:?}: a version past the limit is refused before the delete, got \
                 {status} with {refused:?}"
            );
        }

        // The delete is answered with that version in the non-terminal status.
        let answered = delete_version_ok(&router, case, &arn, row.delete, row.client_token).await;
        assert_eq!(
            answered, PERMISSION_STATUS_DELETING,
            "case {case:?}: the version delete reports the status that version has reached, got \
             {answered:?}"
        );
        let deleting = read(&router, case, &arn, Some(row.delete)).await;
        assert_eq!(
            (
                field(case, &deleting, "status"),
                field(case, &deleting, "version"),
                field(case, &deleting, "permission")
            ),
            (
                PERMISSION_STATUS_DELETING.to_string(),
                row.delete.to_string(),
                template_of(row.delete)
            ),
            "case {case:?}: a read straight after the version delete reports DELETING at the \
             version that was stored, got {deleting:?}"
        );

        // The move to DELETED is the clock's.
        let deleted = poll_until_version_deleted(&router, case, &arn, row.delete).await;
        assert_eq!(
            (
                field(case, &deleted, "version"),
                field(case, &deleted, "permission"),
                flag(case, &deleted, "defaultVersion")
            ),
            (row.delete.to_string(), template_of(row.delete), false),
            "case {case:?}: the deleted version still reports the policy that was stored and is \
             not the default version, got {deleted:?}"
        );

        // Every OTHER version is exactly as it was: the delete takes away one version,
        // not the permission.
        for version in (FIRST_PERMISSION_VERSION..=latest).filter(|version| *version != row.delete)
        {
            let detail = read(&router, case, &arn, Some(version)).await;
            assert_eq!(
                (
                    field(case, &detail, "version"),
                    field(case, &detail, "permission"),
                    flag(case, &detail, "defaultVersion")
                ),
                (
                    version.to_string(),
                    template_of(version),
                    version == latest
                ),
                "case {case:?}: version {version} is untouched by the delete of version {}, got \
                 {detail:?}",
                row.delete
            );
            assert!(
                !on_the_delete_path(&field(case, &detail, "status")),
                "case {case:?}: version {version} is not on the delete path after the delete of \
                 version {}, got {detail:?}",
                row.delete
            );
        }
        // A read naming no version still reports the default one, which the delete did
        // not move.
        let default = read(&router, case, &arn, None).await;
        assert_eq!(
            (
                field(case, &default, "version"),
                field(case, &default, "status"),
                field(case, &default, "permissionType")
            ),
            (
                latest.to_string(),
                PERMISSION_STATUS_ATTACHABLE.to_string(),
                PERMISSION_TYPE_CUSTOMER_MANAGED.to_string()
            ),
            "case {case:?}: the delete of another version leaves the default version alone, got \
             {default:?}"
        );

        // Every read of the permission's versions reports the delete: the record
        // survives in its terminal status, which is what lets a caller poll the delete
        // to completion, and no other version is on the delete path.
        let listed = version_statuses(&router, case, &arn).await;
        for version in FIRST_PERMISSION_VERSION..=latest {
            let reported = listed
                .iter()
                .find(|(number, _)| *number == version.to_string())
                .map(|(_, status)| status.clone())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: version {version} is not reported at all, got {listed:?}")
                });
            if version == row.delete {
                assert_eq!(
                    reported, PERMISSION_STATUS_DELETED,
                    "case {case:?}: the deleted version is reported DELETED, got {listed:?}"
                );
            } else {
                assert!(
                    !on_the_delete_path(&reported),
                    "case {case:?}: version {version} is not on the delete path, got {listed:?}"
                );
            }
        }

        // "If a customer managed permission has the maximum of 5 versions, then you must
        // delete at least one version before you can create another": the deleted
        // version no longer counts, and the number it carried is not reused. The
        // template is one of this row's own, so it names actions of the permission's
        // resource type.
        let next_template = template_of(FIRST_PERMISSION_VERSION);
        let (status, written) = create_permission_version(
            &router,
            &json!({"permissionArn": arn, "policyTemplate": next_template}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a version written after the delete is served, got {status} with \
             {written:?}"
        );
        let after = read(&router, case, &arn, None).await;
        assert_eq!(
            (
                field(case, &after, "version"),
                field(case, &after, "permission"),
                flag(case, &after, "defaultVersion")
            ),
            (
                (latest + 1).to_string(),
                next_template,
                true
            ),
            "case {case:?}: the version written after the delete is numbered above every version \
             ever written and is the new default, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_version_delete_a_caller_may_not_make_is_refused_and_changes_nothing() {
    let case = "a refused version delete";
    let (router, _engine) = emulator_with_engine();

    // A permission carrying two versions, whose FIRST version a live resource share is
    // governed by: the share attached the permission while version 1 was the default,
    // and "existing resource shares continue to use their original permission versions".
    let attached_arn = mint(
        &router,
        case,
        "VersionHeldByAShare",
        "imagebuilder:Component",
        &[policy_template(&["imagebuilder:ListComponents"])],
    )
    .await;
    let share_arn = arn_of(
        case,
        &create_ok(
            &router,
            case,
            &json!({
                "name": "share-holding-a-permission-version",
                "resourceArns": [LOCAL_RESOURCE],
                "permissionArns": [attached_arn],
            }),
        )
        .await,
    );
    let (status, written) = create_permission_version(
        &router,
        &json!({
            "permissionArn": attached_arn,
            "policyTemplate": policy_template(&["imagebuilder:GetComponent"]),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 writing a second version, got {status} with {written:?}"
    );

    // A permission carrying a single version, which is therefore its default version.
    let single_arn = mint(
        &router,
        case,
        "TheOnlyVersion",
        "codebuild:Project",
        &[policy_template(&["codebuild:BatchGetProjects"])],
    )
    .await;
    // A permission whose own delete is in flight: `DeletePermission` "deletes all
    // versions associated with the customer managed permission", so it has no version
    // left for a caller to delete.
    let deleted_arn = mint(
        &router,
        case,
        "DeletedWholePermission",
        "glue:Database",
        &[policy_template(&["glue:GetDatabase"])],
    )
    .await;
    delete_ok(&router, case, &deleted_arn, None).await;

    let rows = vec![
        VersionRefusalRow {
            case: "the permission's default version",
            permission_arn: attached_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION + 1,
            client_token: None,
            expected_code: "OperationNotPermittedException",
            must_name: attached_arn.clone(),
        },
        VersionRefusalRow {
            case: "the only version a permission carries, which is its default",
            permission_arn: single_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: Some("delete-the-only-version"),
            expected_code: "OperationNotPermittedException",
            must_name: single_arn.clone(),
        },
        VersionRefusalRow {
            case: "a version a live resource share is governed by",
            permission_arn: attached_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "OperationNotPermittedException",
            must_name: share_arn.clone(),
        },
        VersionRefusalRow {
            case: "a version of a permission Amazon Web Services manages",
            permission_arn: aws_managed_permission_arn("AWSRAMDefaultPermissionCodeBuildProject"),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "OperationNotPermittedException",
            must_name: aws_managed_permission_arn("AWSRAMDefaultPermissionCodeBuildProject"),
        },
        VersionRefusalRow {
            case: "a version of another permission Amazon Web Services manages",
            permission_arn: aws_managed_permission_arn("AWSRAMDefaultPermissionGlueDatabase"),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: Some("delete-an-aws-managed-permission-version"),
            expected_code: "OperationNotPermittedException",
            must_name: aws_managed_permission_arn("AWSRAMDefaultPermissionGlueDatabase"),
        },
        VersionRefusalRow {
            case: "a version number the permission never carried",
            permission_arn: attached_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION + 5,
            client_token: None,
            expected_code: "UnknownResourceException",
            must_name: (FIRST_PERMISSION_VERSION + 5).to_string(),
        },
        VersionRefusalRow {
            case: "an ARN no permission answers to",
            permission_arn: customer("NeverCreatedVersioned"),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "UnknownResourceException",
            must_name: customer("NeverCreatedVersioned"),
        },
        VersionRefusalRow {
            case: "a version of a permission whose own delete is in flight",
            permission_arn: deleted_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "UnknownResourceException",
            must_name: deleted_arn.clone(),
        },
        VersionRefusalRow {
            case: "a version number below the first version any permission carries",
            permission_arn: attached_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION - 1,
            client_token: None,
            expected_code: "InvalidParameterException",
            must_name: "permissionVersion".to_string(),
        },
        VersionRefusalRow {
            case: "a negative version number",
            permission_arn: attached_arn.clone(),
            permission_version: -3,
            client_token: None,
            expected_code: "InvalidParameterException",
            must_name: "permissionVersion".to_string(),
        },
        VersionRefusalRow {
            case: "a value that is not a managed permission ARN",
            permission_arn: format!(
                "arn:aws:ram:{REGION}:{ACCOUNT}:resource-share/not-a-permission"
            ),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "MalformedArnException",
            must_name: "permissionArn".to_string(),
        },
        VersionRefusalRow {
            case: "a value that is not an ARN at all",
            permission_arn: "VersionHeldByAShare".to_string(),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: None,
            expected_code: "MalformedArnException",
            must_name: "permissionArn".to_string(),
        },
        VersionRefusalRow {
            case: "an empty clientToken",
            permission_arn: attached_arn.clone(),
            permission_version: FIRST_PERMISSION_VERSION,
            client_token: Some(""),
            expected_code: "InvalidClientTokenException",
            must_name: "clientToken".to_string(),
        },
    ];

    for row in &rows {
        let refused_case = row.case;
        let expected_code = row.expected_code;
        let (status, refused) = delete_permission_version(
            &router,
            &row.permission_arn,
            row.permission_version,
            row.client_token,
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, expected_code.to_string()),
            "case {refused_case:?}: expected a 400 {expected_code}, got {status} with {refused:?}"
        );
        assert!(
            error_message(&refused).contains(row.must_name.as_str()),
            "case {refused_case:?}: expected the message to name {:?}, got {refused:?}",
            row.must_name
        );
    }

    // Nothing above deleted anything: both live permissions still carry every version
    // they were written, none of them on the delete path.
    for (row, arn, versions) in [
        (
            "the permission a share holds a version of",
            &attached_arn,
            2,
        ),
        ("the permission carrying one version", &single_arn, 1),
    ] {
        let listed = version_statuses(&router, case, arn).await;
        assert_eq!(
            numbers_of(&listed),
            (FIRST_PERMISSION_VERSION..FIRST_PERMISSION_VERSION + versions)
                .map(|version| version.to_string())
                .collect::<Vec<_>>(),
            "case {row:?}: a refused version delete takes no version away, got {listed:?}"
        );
        assert!(
            !listed
                .iter()
                .any(|(_, status)| on_the_delete_path(status)),
            "case {row:?}: a refused version delete puts no version on the delete path, got \
             {listed:?}"
        );
    }
}

#[tokio::test]
async fn repeating_a_version_delete_deletes_nothing_further() {
    let case = "a repeated version delete";
    let (router, _engine) = emulator_with_engine();
    let arn = mint(
        &router,
        case,
        "RepeatedlyDeletedVersion",
        "imagebuilder:Component",
        &[
            policy_template(&["imagebuilder:ListComponents"]),
            policy_template(&["imagebuilder:ListComponents", "imagebuilder:GetComponent"]),
        ],
    )
    .await;

    let token = "one-version-delete-token";
    assert_eq!(
        delete_version_ok(&router, case, &arn, FIRST_PERMISSION_VERSION, Some(token)).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: the version delete starts the move"
    );
    // The same request under the same token: the first call's delete is the answer, and
    // the status is the one that version has reached since.
    assert_eq!(
        delete_version_ok(&router, case, &arn, FIRST_PERMISSION_VERSION, Some(token)).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: a retry under the same token reports the same delete"
    );
    // And without a token: deleting a version is idempotent for as long as its record
    // exists.
    assert_eq!(
        delete_version_ok(&router, case, &arn, FIRST_PERMISSION_VERSION, None).await,
        PERMISSION_STATUS_DELETING,
        "case {case:?}: a second delete of the same version deletes nothing further"
    );

    // The same token naming a DIFFERENT version is a different request.
    let (status, refused) =
        delete_permission_version(&router, &arn, FIRST_PERMISSION_VERSION + 1, Some(token)).await;
    assert_eq!(
        (status, error_code(&refused)),
        (400, "IdempotentParameterMismatchException".to_string()),
        "case {case:?}: a token reused for another version is refused, got {status} with \
         {refused:?}"
    );

    // Once the delete has completed, repeating it still reports the delete that happened
    // rather than a client error.
    poll_until_version_deleted(&router, case, &arn, FIRST_PERMISSION_VERSION).await;
    assert_eq!(
        delete_version_ok(&router, case, &arn, FIRST_PERMISSION_VERSION, None).await,
        PERMISSION_STATUS_DELETED,
        "case {case:?}: a delete of an already deleted version reports DELETED"
    );
    // And the version the refused retry named is untouched: still the default, still
    // attachable.
    let default = read(&router, case, &arn, None).await;
    assert_eq!(
        (
            field(case, &default, "version"),
            field(case, &default, "status")
        ),
        (
            (FIRST_PERMISSION_VERSION + 1).to_string(),
            PERMISSION_STATUS_ATTACHABLE.to_string()
        ),
        "case {case:?}: the default version is untouched by the delete of another version, got \
         {default:?}"
    );
}
