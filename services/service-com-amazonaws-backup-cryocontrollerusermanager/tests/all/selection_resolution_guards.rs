// SPDX-License-Identifier: CC0-1.0

//! What a resource selection has to BE for AWS Backup to store it, driven as
//! real HTTP through the router.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over rather than on one recorded request:
//!
//! * a selection is how a plan's rules find the resources they protect, so one
//!   that selects nothing — no `Resources`, no `NotResources`, no `ListOfTags`
//!   condition and no `Conditions` operator
//!   (`smithy_query(query="shape",shape="BackupSelection")` and `Conditions`) —
//!   is refused `InvalidParameterValueException` (HTTP 400) with the condition's
//!   fixed sentence, and nothing is stored; a selection that states ANY of those
//!   grammars is stored and reads back;
//! * a selection that assigns resources BY TAG is resolved by reading this
//!   account's resource tags under the role it names, so the role has to be
//!   allowed that tagging read: one whose identity policies do not allow it is
//!   refused with that condition's own token and fixed sentence before anything
//!   is stored, while a selection that names its resources by ARN alone needs no
//!   such read and is stored whatever the role may do about tags.
//!
//! The Backup wire is the shared harness in `vault_and_backup_job.rs`; IAM is
//! mounted beside Backup on one engine (`plan_and_selection_refusals.rs`'s
//! `router_with_iam`) wherever the rule turns on what a role may do, because
//! that question is put to IAM over the emulator's own in-process callback and
//! the documents it is decided from are the ones `CreateRole` and
//! `PutRolePolicy` wrote through IAM's own wire.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::plan_and_selection_refusals::{create_role, put_role_policy, router_with_iam};
use super::vault_and_backup_job::{call_reported, ACCOUNT};

/// The sentence AWS states for a selection that selects nothing
/// (`crate::state`'s `MESSAGE_SELECTION_SELECTS_NOTHING`). It names the two
/// member names the model gives the grammars a selection is built from.
const SELECTS_NOTHING_MESSAGE: &str =
    "Invalid backup selection. Either 'ListOfTags' or 'Resources' section must be non-empty.";

/// The token and sentence a tag-based selection whose role may not read resource
/// tags is refused with (`ERROR_CODE_SELECTION_ROLE_CANNOT_READ_TAGS` /
/// `MESSAGE_SELECTION_ROLE_CANNOT_READ_TAGS`).
const ROLE_CANNOT_READ_TAGS_CODE: &str = "ERROR_3018";
const ROLE_CANNOT_READ_TAGS_MESSAGE: &str = "IAM Role is not authorized to call tag:GetResources";

/// The tagging-API read AWS Backup makes under the selection's role to resolve a
/// tag-based selection. SOURCE: the action the recorded refusal names, and the
/// read AWS Backup's own managed service-role policy grants for it
/// (`tag:GetResources`).
const TAG_READ_ACTION: &str = "tag:GetResources";

/// The service principal a role has to admit for AWS Backup to assume it, so
/// every row below gets as far as the rule under test.
const SERVICE_PRINCIPAL: &str = "backup.amazonaws.com";

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// A trust document admitting this service's principal.
fn backup_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy of the statements given.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One `Allow` statement of `action` on every resource.
fn allow(action: &str) -> Value {
    json!({"Effect": "Allow", "Action": action, "Resource": "*"})
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s binding — answering the plan id
/// this service minted.
async fn plan_id_of(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "BackupPlan": {
            "BackupPlanName": name,
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "selection-vault"}],
        }
    });
    let (status, _, created) =
        call_reported(router, "PUT", "/backup/plans", Some(&body.to_string())).await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan {name:?} expected 200, got {status} {created:?}"
    );
    member(&created, "BackupPlanId")
}

/// `PUT /backup/plans/{BackupPlanId}/selections` — `CreateBackupSelection`'s.
async fn create_selection(
    router: &MultiServiceRouter,
    plan_id: &str,
    selection: &Value,
) -> (u16, String, Value) {
    call_reported(
        router,
        "PUT",
        &format!("/backup/plans/{plan_id}/selections"),
        Some(&json!({"BackupSelection": selection}).to_string()),
    )
    .await
}

/// The `SelectionId`s the plan holds, as `ListBackupSelections` reports them —
/// what a refused create must leave untouched.
async fn listed_selection_ids(router: &MultiServiceRouter, plan_id: &str) -> Vec<String> {
    let (status, _, listed) = call_reported(
        router,
        "GET",
        &format!("/backup/plans/{plan_id}/selections"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected ListBackupSelections 200, got {status} {listed:?}"
    );
    listed["BackupSelectionsList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| member(entry, "SelectionId"))
        .collect()
}

/// One tag condition under the legacy `ListOfTags` grammar.
fn list_of_tags() -> Value {
    json!([{
        "ConditionType": "STRINGEQUALS",
        "ConditionKey": "backup",
        "ConditionValue": "daily",
    }])
}

/// One parameter under an operator of `Conditions`.
fn condition(key: &str, value: &str) -> Value {
    json!({"ConditionKey": key, "ConditionValue": value})
}

/// The selection documents the "selects something" rule generalises over, with
/// whether each one selects anything at all.
///
/// The rows walk every grammar the model gives a selection and every way one can
/// be stated as empty: absent, an empty list, and — for `Conditions` — the
/// structure with no operator and the structure with every operator empty.
fn selection_rows() -> Vec<(&'static str, Value, bool)> {
    vec![
        ("no grammar at all", json!({}), false),
        ("an empty Resources list", json!({"Resources": []}), false),
        (
            "an empty ListOfTags list",
            json!({"ListOfTags": []}),
            false,
        ),
        (
            "both published grammars stated empty",
            json!({"Resources": [], "ListOfTags": []}),
            false,
        ),
        (
            "an empty NotResources list beside them",
            json!({"Resources": [], "ListOfTags": [], "NotResources": []}),
            false,
        ),
        (
            "a Conditions structure with no operator",
            json!({"Conditions": {}}),
            false,
        ),
        (
            "a Conditions structure with every operator empty",
            json!({
                "Conditions": {
                    "StringEquals": [],
                    "StringNotEquals": [],
                    "StringLike": [],
                    "StringNotLike": [],
                },
            }),
            false,
        ),
        (
            "one resource ARN",
            json!({"Resources": ["arn:aws:s3:::probe-bucket"]}),
            true,
        ),
        (
            "two resource ARNs and an exclusion",
            json!({
                "Resources": ["arn:aws:s3:::probe-one", "arn:aws:s3:::probe-two"],
                "NotResources": ["arn:aws:s3:::probe-logs"],
            }),
            true,
        ),
        (
            "an exclusion alone, which selects everything else",
            json!({"NotResources": ["arn:aws:s3:::probe-logs"]}),
            true,
        ),
        ("one tag condition", json!({"ListOfTags": list_of_tags()}), true),
        (
            "one Conditions operator",
            json!({"Conditions": {"StringLike": [condition("aws:ResourceTag/app", "prod*")]}}),
            true,
        ),
        (
            "a negated Conditions operator beside an empty one",
            json!({
                "Conditions": {
                    "StringEquals": [],
                    "StringNotEquals": [condition("aws:ResourceTag/tier", "scratch")],
                },
            }),
            true,
        ),
    ]
}

/// A selection is stored exactly when it selects something, and a refused create
/// leaves the plan holding nothing.
#[tokio::test]
async fn a_selection_is_stored_only_when_it_selects_something() {
    for (index, (case, declared, selects)) in selection_rows().into_iter().enumerate() {
        // One emulator per row, so a row can assert what the plan holds without
        // seeing what another row stored. IAM is mounted because the tag-based
        // rows are decided on what their role may do.
        let router = router_with_iam();
        let role = create_role(&router, "/", &format!("tagging-role-{index}"), &backup_trust())
            .await;
        put_role_policy(
            &router,
            &format!("tagging-role-{index}"),
            &policy(vec![allow(TAG_READ_ACTION)]),
        )
        .await;
        let plan_id = plan_id_of(&router, "selection-plan").await;

        let mut sent = declared.clone();
        sent["SelectionName"] = json!(format!("row-{index}"));
        sent["IamRoleArn"] = json!(role);
        let (status, errortype, answered) = create_selection(&router, &plan_id, &sent).await;

        if !selects {
            assert_eq!(
                (
                    status,
                    errortype.clone(),
                    member(&answered, "Message"),
                ),
                (
                    400,
                    "InvalidParameterValueException".to_string(),
                    SELECTS_NOTHING_MESSAGE.to_string(),
                ),
                "case {case:?}: expected the selects-nothing refusal, got {status} {errortype} \
                 {answered:?}"
            );
            let held = listed_selection_ids(&router, &plan_id).await;
            assert_eq!(
                held,
                Vec::<String>::new(),
                "case {case:?}: a refused create must store no selection, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupSelection 200, got {status} {answered:?}"
        );
        let selection_id = member(&answered, "SelectionId");
        let held = listed_selection_ids(&router, &plan_id).await;
        assert_eq!(
            held,
            vec![selection_id],
            "case {case:?}: expected the plan to hold exactly the selection it stored, got \
             {held:?}"
        );
    }
}

/// One row of the tagging-permission table: what the role's identity policy
/// says, and whether a TAG-based selection naming that role is stored.
struct RoleRow {
    case: &'static str,
    /// The statements of the role's inline identity policy; empty for a role
    /// that carries no policy at all.
    statements: Vec<Value>,
    stored: bool,
}

/// The policies the tagging rule generalises over.
///
/// The rows differ along what the policy says about the tagging read: the action
/// itself, a wildcard over the tagging namespace, every action of every service,
/// the action beside others, a policy covering other services only, an explicit
/// `Deny` beside a broad `Allow`, and no policy at all.
fn role_rows() -> Vec<RoleRow> {
    vec![
        RoleRow {
            case: "the tagging read itself",
            statements: vec![allow(TAG_READ_ACTION)],
            stored: true,
        },
        RoleRow {
            case: "a wildcard over the tagging namespace",
            statements: vec![allow("tag:*")],
            stored: true,
        },
        RoleRow {
            case: "every action of every service",
            statements: vec![allow("*")],
            stored: true,
        },
        RoleRow {
            case: "the tagging read beside the backup actions",
            statements: vec![allow("backup:*"), allow(TAG_READ_ACTION)],
            stored: true,
        },
        RoleRow {
            case: "other services only",
            statements: vec![allow("s3:GetObject"), allow("dynamodb:CreateBackup")],
            stored: false,
        },
        RoleRow {
            case: "another action of the tagging namespace only",
            statements: vec![allow("tag:GetTagKeys")],
            stored: false,
        },
        RoleRow {
            case: "an explicit Deny of the tagging read beside a broad Allow",
            statements: vec![
                allow("*"),
                json!({"Effect": "Deny", "Action": TAG_READ_ACTION, "Resource": "*"}),
            ],
            stored: false,
        },
        RoleRow {
            case: "no identity policy at all",
            statements: Vec::new(),
            stored: false,
        },
    ]
}

/// The tag-based selection grammars the rule applies to, each stated as the
/// members a create sends.
fn tag_grammars() -> Vec<(&'static str, Value)> {
    vec![
        ("the legacy ListOfTags grammar", json!({"ListOfTags": list_of_tags()})),
        (
            "the Conditions grammar",
            json!({
                "Conditions": {
                    "StringEquals": [condition("aws:ResourceTag/backup", "daily")],
                },
            }),
        ),
    ]
}

/// A TAG-based selection is stored only for a role this account's IAM shows may
/// read resource tags, and a refused create stores nothing.
#[tokio::test]
async fn a_tag_based_selection_is_stored_only_for_a_role_that_may_read_tags() {
    for (grammar, declared) in tag_grammars() {
        for (index, row) in role_rows().into_iter().enumerate() {
            let router = router_with_iam();
            let role_name = format!("tag-role-{index}");
            let role = create_role(&router, "/", &role_name, &backup_trust()).await;
            if !row.statements.is_empty() {
                put_role_policy(&router, &role_name, &policy(row.statements.clone())).await;
            }
            let plan_id = plan_id_of(&router, "tagging-plan").await;

            let mut sent = declared.clone();
            sent["SelectionName"] = json!("by-tag");
            sent["IamRoleArn"] = json!(role);
            let (status, errortype, answered) = create_selection(&router, &plan_id, &sent).await;

            if row.stored {
                assert_eq!(
                    status, 200,
                    "case {grammar}/{:?}: expected CreateBackupSelection 200, got {status} \
                     {answered:?}",
                    row.case
                );
                continue;
            }
            assert_eq!(
                (
                    status,
                    errortype.clone(),
                    member(&answered, "Code"),
                    member(&answered, "Message"),
                ),
                (
                    400,
                    "InvalidParameterValueException".to_string(),
                    ROLE_CANNOT_READ_TAGS_CODE.to_string(),
                    ROLE_CANNOT_READ_TAGS_MESSAGE.to_string(),
                ),
                "case {grammar}/{:?}: expected the tagging-permission refusal, got {status} \
                 {errortype} {answered:?}",
                row.case
            );
            assert!(
                !member(&answered, "Message").contains(&role),
                "case {grammar}/{:?}: the sentence must not name the role, got {answered:?}",
                row.case
            );
            let held = listed_selection_ids(&router, &plan_id).await;
            assert_eq!(
                held,
                Vec::<String>::new(),
                "case {grammar}/{:?}: a refused create must store no selection, got {held:?}",
                row.case
            );
        }
    }
}

/// A selection that names its resources by ARN alone is stored whatever the
/// role may do about tags: no tagging read resolves it, so none is required.
#[tokio::test]
async fn a_resource_selection_needs_no_tagging_permission() {
    for (index, row) in role_rows().into_iter().enumerate() {
        let router = router_with_iam();
        let role_name = format!("arn-role-{index}");
        let role = create_role(&router, "/", &role_name, &backup_trust()).await;
        if !row.statements.is_empty() {
            put_role_policy(&router, &role_name, &policy(row.statements.clone())).await;
        }
        let plan_id = plan_id_of(&router, "arn-plan").await;

        let (status, errortype, answered) = create_selection(
            &router,
            &plan_id,
            &json!({
                "SelectionName": "by-arn",
                "IamRoleArn": role,
                "Resources": [format!("arn:aws:dynamodb:us-west-2:{ACCOUNT}:table/orders")],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected a resource-ARN selection to be stored, got {status} \
             {errortype} {answered:?}",
            row.case
        );
    }
}
