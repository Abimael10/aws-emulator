// SPDX-License-Identifier: CC0-1.0

//! What a stored backup selection REPORTS, and what bars the deletion of the
//! plan that holds one — both driven as real HTTP through the router.
//!
//! The rules under test:
//!
//! * a selection is reported in FULL: it names the resources to back up in the
//!   published list grammars — `Resources`, `NotResources` and `ListOfTags` — and
//!   in the `Conditions` structure of four operator lists (`StringEquals`,
//!   `StringNotEquals`, `StringLike`, `StringNotLike`;
//!   `smithy_query(query="shape",shape="BackupSelection")` and `Conditions`), and
//!   a grammar or an operator the create did not use reads back as an EMPTY list
//!   rather than as an absent member, while every value it did declare reads back
//!   value-for-value under the operator it was declared in;
//! * "a backup plan can only be deleted after all associated selections of
//!   resources have been deleted": the delete of a plan that still holds one is
//!   refused `InvalidRequestException` with the condition's own token and its
//!   fixed sentence — which names neither the plan nor how many selections it
//!   holds — and the plan is deleted once they are gone.
//!
//! Each rule is asserted over a table rather than one request: the selection rows
//! differ in WHICH grammar the create uses and how much of it, and the deletion
//! rows in how many selections stand in the delete's way.
//!
//! The IAM-mounted router and the `CreateRole` sender are the ones
//! `plan_and_selection_refusals.rs` exposes, because a selection is stored only
//! for a role this account's IAM shows AWS Backup can assume.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::plan_and_selection_refusals::{create_role, put_role_policy, router_with_iam};
use super::vault_and_backup_job::call;

/// The sentence AWS states for a plan delete its own selections bar
/// (`crate::state`'s `MESSAGE_PLAN_SELECTIONS_EXIST`), with the token it carries
/// for it (`ERROR_CODE_PLAN_SELECTIONS_EXIST`).
const SELECTIONS_EXIST_MESSAGE: &str =
    "Related backup plan selections must be deleted prior to backup plan deletion";
const SELECTIONS_EXIST_CODE: &str = "ERROR_9201";

/// A trust document admitting this service's principal, so IAM answers that the
/// role the selections name is one AWS Backup can assume.
fn backup_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "backup.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing the tagging read AWS Backup makes under a
/// selection's role to resolve a TAG-based selection, so a row that selects by
/// tag gets as far as the rule under test (the read AWS Backup's own managed
/// service-role policy grants: `tag:GetResources`).
fn tag_read_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "tag:GetResources",
            "Resource": "*",
        }],
    })
    .to_string()
}

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "BackupPlan": {
            "BackupPlanName": name,
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "selection-vault"}],
        }
    });
    let (status, created) =
        call(router, "PUT", "/backup/plans", Some(&body.to_string())).await;
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
) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/backup/plans/{plan_id}/selections"),
        Some(&json!({"BackupSelection": selection}).to_string()),
    )
    .await
}

/// `GET /backup/plans/{BackupPlanId}/selections/{SelectionId}` —
/// `GetBackupSelection`'s.
async fn get_selection(
    router: &MultiServiceRouter,
    plan_id: &str,
    selection_id: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/backup/plans/{plan_id}/selections/{selection_id}"),
        None,
    )
    .await
}

/// One condition parameter under an operator of `Conditions`.
fn condition(key: &str, value: &str) -> Value {
    json!({"ConditionKey": key, "ConditionValue": value})
}

/// The selection bodies the completion rule is asserted over: each row uses a
/// different amount of a different grammar, so what varies between two rows is
/// which members the create declared.
fn selection_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the smallest selection, one resource ARN",
            json!({"Resources": ["arn:aws:s3:::probe-only"]}),
        ),
        (
            "two resource ARNs only",
            json!({"Resources": ["arn:aws:s3:::probe-one", "arn:aws:s3:::probe-two"]}),
        ),
        (
            "resources included and excluded",
            json!({
                "Resources": ["arn:aws:s3:::probe-*"],
                "NotResources": ["arn:aws:s3:::probe-logs"],
            }),
        ),
        (
            "the legacy tag grammar only",
            json!({
                "ListOfTags": [{
                    "ConditionType": "STRINGEQUALS",
                    "ConditionKey": "tier",
                    "ConditionValue": "gold",
                }],
            }),
        ),
        (
            "one operator of Conditions",
            json!({"Conditions": {"StringEquals": [condition("aws:ResourceTag/tier", "gold")]}}),
        ),
        (
            "an operator the wildcard grammar uses",
            json!({"Conditions": {"StringLike": [condition("aws:ResourceTag/app", "prod*")]}}),
        ),
        (
            "a negated operator with two parameters",
            json!({
                "Conditions": {
                    "StringNotEquals": [
                        condition("aws:ResourceTag/tier", "scratch"),
                        condition("aws:ResourceTag/env", "test"),
                    ],
                },
            }),
        ),
        (
            "every operator at once, beside resources",
            json!({
                "Resources": ["arn:aws:s3:::probe-all"],
                "Conditions": {
                    "StringEquals": [condition("aws:ResourceTag/tier", "gold")],
                    "StringNotEquals": [condition("aws:ResourceTag/tier", "scratch")],
                    "StringLike": [condition("aws:ResourceTag/app", "prod*")],
                    "StringNotLike": [condition("aws:ResourceTag/app", "*test")],
                },
            }),
        ),
    ]
}

/// The four operator members of `Conditions`
/// (`smithy_query(query="shape",shape="Conditions")`).
const OPERATORS: [&str; 4] = [
    "StringEquals",
    "StringNotEquals",
    "StringLike",
    "StringNotLike",
];

/// The list grammars a selection names its resources with
/// (`smithy_query(query="shape",shape="BackupSelection")`).
const GRAMMARS: [&str; 3] = ["Resources", "NotResources", "ListOfTags"];

/// A read of a selection reports every list grammar and every condition
/// operator, an undeclared one as an empty list and a declared one value for
/// value.
#[tokio::test]
async fn a_selection_reads_back_with_every_grammar_and_operator_reported() {
    for (index, (case, declared)) in selection_rows().into_iter().enumerate() {
        let router = router_with_iam();
        let role = create_role(&router, "/", "AWSBackupDefaultServiceRole", &backup_trust()).await;
        put_role_policy(&router, "AWSBackupDefaultServiceRole", &tag_read_policy()).await;
        let plan_id = create_plan(&router, "selection-plan").await;

        let mut sent = declared.clone();
        sent["SelectionName"] = json!(format!("row-{index}"));
        sent["IamRoleArn"] = json!(role);
        let (status, created) = create_selection(&router, &plan_id, &sent).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupSelection 200, got {status} {created:?}"
        );

        let (status, read) = get_selection(&router, &plan_id, &member(&created, "SelectionId")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBackupSelection 200, got {status} {read:?}"
        );
        let reported = &read["BackupSelection"];

        for grammar in GRAMMARS {
            let expected = declared
                .get(grammar)
                .cloned()
                .unwrap_or_else(|| json!([]));
            assert_eq!(
                reported.get(grammar),
                Some(&expected),
                "case {case:?}: expected {grammar} to read back as {expected}, got \
                 {reported:?}"
            );
        }
        for operator in OPERATORS {
            let expected = declared
                .get("Conditions")
                .and_then(|conditions| conditions.get(operator))
                .cloned()
                .unwrap_or_else(|| json!([]));
            assert_eq!(
                reported["Conditions"].get(operator),
                Some(&expected),
                "case {case:?}: expected Conditions.{operator} to read back as {expected}, \
                 got {reported:?}"
            );
        }
    }
}

/// The delete of a plan that still holds selections is refused with the
/// condition's own fixed sentence, and succeeds once the selections are gone.
///
/// The rows differ in HOW MANY selections stand in the way, which is what shows
/// the sentence carries no count — and none of them names the plan either.
#[tokio::test]
async fn a_plan_delete_is_barred_by_its_selections_with_one_fixed_sentence() {
    for (case, selections) in [("one selection", 1), ("three selections", 3)] {
        let router = router_with_iam();
        let role = create_role(&router, "/", "AWSBackupDefaultServiceRole", &backup_trust()).await;
        let plan_id = create_plan(&router, "deletion-plan").await;

        let mut ids = Vec::new();
        for index in 0..selections {
            let (status, created) = create_selection(
                &router,
                &plan_id,
                &json!({
                    "SelectionName": format!("assigned-{index}"),
                    "IamRoleArn": role,
                    "Resources": [format!("arn:aws:s3:::probe-{index}")],
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateBackupSelection 200, got {status} {created:?}"
            );
            ids.push(member(&created, "SelectionId"));
        }

        let (status, refused) =
            call(&router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await;
        assert_eq!(
            (
                status,
                member(&refused, "Code"),
                member(&refused, "Message"),
            ),
            (
                400,
                SELECTIONS_EXIST_CODE.to_string(),
                SELECTIONS_EXIST_MESSAGE.to_string(),
            ),
            "case {case:?}: expected the selections-exist token and fixed sentence, got {status} \
             {refused:?}"
        );
        let message = member(&refused, "Message");
        assert!(
            !message.contains(&plan_id) && !message.contains(&selections.to_string()),
            "case {case:?}: the sentence must name neither the plan nor the count, got \
             {message:?}"
        );

        // With every selection deleted the same delete is served, so the
        // refusal was the selections and nothing else.
        for id in &ids {
            let (status, deleted) = call(
                &router,
                "DELETE",
                &format!("/backup/plans/{plan_id}/selections/{id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteBackupSelection 200 for {id}, got {status} \
                 {deleted:?}"
            );
        }
        let (status, deleted) =
            call(&router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupPlan 200 once the selections are gone, got \
             {status} {deleted:?}"
        );
    }
}
