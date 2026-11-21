// SPDX-License-Identifier: CC0-1.0

//! The OPERATOR a tag condition of a backup selection is stated with, driven as
//! real HTTP through the router.
//!
//! The rule under test: AWS Backup reads a `ListOfTags` condition's
//! `ConditionType` BY NAME and reads that name without regard to letter case, so
//! a condition stating an admitted operator in ANY casing assigns resources by
//! it and the selection is stored — under the operator it names, which is what
//! every read of the selection reports — while a value that is no casing of an
//! admitted operator is refused `InvalidParameterValueException` (HTTP 400) and
//! nothing is stored.
//!
//! The rows differ along the casing of the operator and, for the refusals, along
//! HOW a value falls outside the set (another operator's name, a respelling, a
//! prefix of one, an extension of one, the empty value) and along WHICH condition
//! of a several-condition selection carries it.
//!
//! The consequence the rule has for a teardown is asserted too: a plan holding a
//! selection stated in any casing refuses its own delete while the selection
//! stands, and the SAME delete is served once the selection is deleted — so a
//! client that removed every selection can always delete the plan.
//!
//! The Backup wire is the shared harness in `vault_and_backup_job.rs`; IAM is
//! mounted beside Backup on one engine (`plan_and_selection_refusals.rs`'s
//! `router_with_iam`) because a TAG-based selection is resolved under the role it
//! names, so every row has to get a role IAM shows may read resource tags.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::plan_and_selection_refusals::{create_role, put_role_policy, router_with_iam};
use super::vault_and_backup_job::call_reported;

/// The operators a tag condition may be stated with. SOURCE: the model's own
/// value set, `smithy_query(query="shape",shape="ConditionType")` — the single
/// value `STRINGEQUALS`.
const ADMITTED_OPERATORS: [&str; 1] = ["STRINGEQUALS"];

/// The token and sentence a plan delete is refused with while the plan still
/// holds selections (`crate::state`'s `ERROR_CODE_PLAN_SELECTIONS_EXIST` /
/// `MESSAGE_PLAN_SELECTIONS_EXIST`).
const SELECTIONS_EXIST_CODE: &str = "ERROR_9201";
const SELECTIONS_EXIST_MESSAGE: &str =
    "Related backup plan selections must be deleted prior to backup plan deletion";

/// The service principal a role has to admit for AWS Backup to assume it.
const SERVICE_PRINCIPAL: &str = "backup.amazonaws.com";

/// The tagging-API read AWS Backup makes under the selection's role to resolve a
/// tag-based selection, so every row gets as far as the rule under test.
const TAG_READ_ACTION: &str = "tag:GetResources";

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

/// An identity policy allowing the tagging read on every resource.
fn tag_read_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": TAG_READ_ACTION, "Resource": "*"}],
    })
    .to_string()
}

/// One emulator with a role that may read resource tags, plus a backup plan to
/// assign selections to: the ARN of that role and the plan's id.
async fn plan_with_tagging_role(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let role = create_role(router, "/", name, &backup_trust()).await;
    put_role_policy(router, name, &tag_read_policy()).await;
    let body = json!({
        "BackupPlan": {
            "BackupPlanName": name,
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "condition-vault"}],
        }
    });
    let (status, _, created) =
        call_reported(router, "PUT", "/backup/plans", Some(&body.to_string())).await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan expected 200, got {status} {created:?}"
    );
    (role, member(&created, "BackupPlanId"))
}

/// `PUT /backup/plans/{BackupPlanId}/selections` — `CreateBackupSelection`'s
/// binding.
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

/// The selection the plan holds under `selection_id`, as `GetBackupSelection`
/// reports it.
async fn read_selection(
    router: &MultiServiceRouter,
    plan_id: &str,
    selection_id: &str,
) -> (u16, Value) {
    let (status, _, body) = call_reported(
        router,
        "GET",
        &format!("/backup/plans/{plan_id}/selections/{selection_id}"),
        None,
    )
    .await;
    (status, body)
}

/// The `SelectionId`s the plan holds, as `ListBackupSelections` reports them.
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

/// One tag condition stating `operator`.
fn condition(operator: &str, key: &str, value: &str) -> Value {
    json!({
        "ConditionType": operator,
        "ConditionKey": key,
        "ConditionValue": value,
    })
}

/// The clause this service states for a condition at `position` (1-based) whose
/// operator is outside the admitted set: the request boundary's own sentence over
/// the path that names the element's `ConditionType`.
fn operator_clause(position: usize, value: &str) -> String {
    format!(
        "Value '{value}' at 'backupSelection.listOfTags.{position}.member.conditionType' failed \
         to satisfy constraint: Member must satisfy enum value set: [{}]",
        ADMITTED_OPERATORS.join(", ")
    )
}

/// The casings of an admitted operator the rule generalises over: the model's own
/// spelling, the spelling the service's documentation uses, all-lower, and a
/// mixed one no document uses at all.
fn admitted_casings() -> Vec<(&'static str, String)> {
    vec![
        ("the model's own spelling", ADMITTED_OPERATORS[0].to_string()),
        (
            "the spelling the documentation uses",
            "StringEquals".to_string(),
        ),
        (
            "every letter lower case",
            ADMITTED_OPERATORS[0].to_lowercase(),
        ),
        (
            "an alternating casing no document uses",
            "sTrInGeQuAlS".to_string(),
        ),
    ]
}

/// A tag condition stating an admitted operator in ANY casing is stored, and the
/// stored selection reads back carrying the condition the create sent.
#[tokio::test]
async fn an_admitted_operator_is_accepted_in_any_letter_case() {
    for (index, (case, operator)) in admitted_casings().into_iter().enumerate() {
        // One emulator per row, so a row sees only what it stored itself.
        let router = router_with_iam();
        let (role, plan_id) =
            plan_with_tagging_role(&router, &format!("operator-role-{index}")).await;

        let sent = json!({
            "SelectionName": format!("by-tag-{index}"),
            "IamRoleArn": role,
            "ListOfTags": [condition(&operator, "backup", "daily")],
        });
        let (status, errortype, answered) = create_selection(&router, &plan_id, &sent).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupSelection 200 for operator {operator:?}, got \
             {status} {errortype} {answered:?}"
        );

        let selection_id = member(&answered, "SelectionId");
        let held = listed_selection_ids(&router, &plan_id).await;
        assert_eq!(
            held,
            vec![selection_id.clone()],
            "case {case:?}: expected the plan to hold exactly the selection it stored, got \
             {held:?}"
        );

        let (status, read) = read_selection(&router, &plan_id, &selection_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBackupSelection 200, got {status} {read:?}"
        );
        // The condition is held under the OPERATOR it names, so a read reports
        // that operator the way the model spells it, whatever casing the create
        // used.
        assert_eq!(
            read["BackupSelection"].get("ListOfTags"),
            Some(&json!([condition(ADMITTED_OPERATORS[0], "backup", "daily")])),
            "case {case:?}: expected the stored condition to read back under the operator it \
             names, got {read:?}"
        );
    }
}

/// One row of the refusal table: the operator a condition states, what makes it
/// no casing of an admitted one, and the value the clause echoes for it — the
/// value itself, except where it is longer than the ceiling an echo is held to.
struct RefusedRow {
    case: &'static str,
    operator: String,
    echoed: String,
}

/// A refused row whose operator is short enough to be echoed whole.
fn refused(case: &'static str, operator: &str) -> RefusedRow {
    RefusedRow {
        case,
        operator: operator.to_string(),
        echoed: operator.to_string(),
    }
}

/// The ways a `ConditionType` falls outside the admitted set: another operator's
/// name (one `Conditions` publishes but `ListOfTags` does not), a respelling with
/// a separator, a prefix of the admitted name, an extension of it, a value that
/// only contains it, the empty value, and one long enough that the clause echoes
/// a prefix of it instead.
fn refused_rows() -> Vec<RefusedRow> {
    // 300 bytes, past the 256-byte ceiling an echoed value is held to, so the
    // clause states the first 256 and the value's own length.
    let overlong = "X".repeat(300);
    vec![
        refused("another operator's name", "StringLike"),
        refused("a negated operator's name", "STRINGNOTEQUALS"),
        refused("the admitted name respelled with a separator", "STRING_EQUALS"),
        refused("a prefix of the admitted name", "STRING"),
        refused("the admitted name extended", "STRINGEQUALSX"),
        refused("a value that merely contains the admitted name", "XSTRINGEQUALS"),
        refused("the empty value", ""),
        RefusedRow {
            case: "a value past the ceiling an echo is held to",
            operator: overlong,
            echoed: format!("{}... [300 bytes]", "X".repeat(256)),
        },
    ]
}

/// A tag condition stating no admitted operator is refused, and nothing is
/// stored for it.
#[tokio::test]
async fn an_operator_outside_the_set_is_refused_and_stores_nothing() {
    for (index, row) in refused_rows().into_iter().enumerate() {
        let router = router_with_iam();
        let (role, plan_id) =
            plan_with_tagging_role(&router, &format!("refused-role-{index}")).await;

        let (status, errortype, refused) = create_selection(
            &router,
            &plan_id,
            &json!({
                "SelectionName": "by-tag",
                "IamRoleArn": role,
                "ListOfTags": [condition(&row.operator, "backup", "daily")],
            }),
        )
        .await;
        assert_eq!(
            (
                status,
                errortype.clone(),
                member(&refused, "Message"),
            ),
            (
                400,
                "InvalidParameterValueException".to_string(),
                operator_clause(1, &row.echoed),
            ),
            "case {:?}: expected the operator refusal for {:?}, got {status} {errortype} \
             {refused:?}",
            row.case,
            row.operator
        );
        let held = listed_selection_ids(&router, &plan_id).await;
        assert_eq!(
            held,
            Vec::<String>::new(),
            "case {:?}: a refused create must store no selection, got {held:?}",
            row.case
        );
    }
}

/// The refusal names the POSITION of the condition at fault among the conditions
/// the selection states, counted from one — so a client is told which condition
/// to correct.
#[tokio::test]
async fn the_refusal_names_the_position_of_the_condition_at_fault() {
    // Each row states three conditions and puts the bad operator at a different
    // position, so what varies is the position the clause has to name.
    for position in 1..=3usize {
        let router = router_with_iam();
        let (role, plan_id) =
            plan_with_tagging_role(&router, &format!("position-role-{position}")).await;

        let bad = "StringNotLike";
        let conditions: Vec<Value> = (1..=3usize)
            .map(|at| {
                let operator = if at == position {
                    bad
                } else {
                    ADMITTED_OPERATORS[0]
                };
                condition(operator, &format!("tier-{at}"), "gold")
            })
            .collect();

        let (status, errortype, refused) = create_selection(
            &router,
            &plan_id,
            &json!({
                "SelectionName": "by-tag",
                "IamRoleArn": role,
                "ListOfTags": conditions,
            }),
        )
        .await;
        assert_eq!(
            (status, member(&refused, "Message")),
            (400, operator_clause(position, bad)),
            "case position {position}: expected the clause to name position {position}, got \
             {status} {errortype} {refused:?}"
        );
    }
}

/// A plan holding a selection stated in any admitted casing refuses its own
/// delete while that selection stands, and serves the SAME delete once the
/// selection is deleted: the selection really is part of the plan's state, and a
/// teardown that removed every selection can always delete the plan.
#[tokio::test]
async fn a_plan_is_deletable_once_a_case_stated_selection_is_deleted() {
    for (index, (case, operator)) in admitted_casings().into_iter().enumerate() {
        let router = router_with_iam();
        let (role, plan_id) =
            plan_with_tagging_role(&router, &format!("teardown-role-{index}")).await;

        let (status, errortype, created) = create_selection(
            &router,
            &plan_id,
            &json!({
                "SelectionName": "by-tag",
                "IamRoleArn": role,
                "ListOfTags": [condition(&operator, "backup", "daily")],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupSelection 200, got {status} {errortype} \
             {created:?}"
        );
        let selection_id = member(&created, "SelectionId");

        let (status, _, barred) =
            call_reported(&router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await;
        assert_eq!(
            (status, member(&barred, "Code"), member(&barred, "Message")),
            (
                400,
                SELECTIONS_EXIST_CODE.to_string(),
                SELECTIONS_EXIST_MESSAGE.to_string(),
            ),
            "case {case:?}: expected the plan delete to be barred by the stored selection, got \
             {status} {barred:?}"
        );

        let (status, _, deleted) = call_reported(
            &router,
            "DELETE",
            &format!("/backup/plans/{plan_id}/selections/{selection_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupSelection 200, got {status} {deleted:?}"
        );
        let held = listed_selection_ids(&router, &plan_id).await;
        assert_eq!(
            held,
            Vec::<String>::new(),
            "case {case:?}: expected the plan to hold no selection after the delete, got {held:?}"
        );

        let (status, _, deleted) =
            call_reported(&router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupPlan 200 once the selection is gone, got \
             {status} {deleted:?}"
        );
        assert_eq!(
            member(&deleted, "BackupPlanId"),
            plan_id,
            "case {case:?}: expected the delete to answer for the plan it deleted, got \
             {deleted:?}"
        );
        assert!(
            deleted.get("DeletionDate").is_some_and(|date| !date.is_null()),
            "case {case:?}: expected the delete to record a DeletionDate, got {deleted:?}"
        );
    }
}

/// A condition that states NO operator at all is still refused: the operator is
/// required, and reading its name in any case does not make it optional.
#[tokio::test]
async fn a_condition_stating_no_operator_is_still_refused() {
    let router = router_with_iam();
    let (role, plan_id) = plan_with_tagging_role(&router, "absent-operator-role").await;

    let (status, errortype, refused) = create_selection(
        &router,
        &plan_id,
        &json!({
            "SelectionName": "by-tag",
            "IamRoleArn": role,
            "ListOfTags": [{"ConditionKey": "backup", "ConditionValue": "daily"}],
        }),
    )
    .await;
    assert_eq!(
        (status, errortype.clone()),
        (400, "MissingParameterValueException".to_string()),
        "case {:?}: expected the absent-member refusal, got {status} {errortype} {refused:?}",
        "no ConditionType at all"
    );
    let held = listed_selection_ids(&router, &plan_id).await;
    assert_eq!(
        held,
        Vec::<String>::new(),
        "case {:?}: a refused create must store no selection, got {held:?}",
        "no ConditionType at all"
    );
}
