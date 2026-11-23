// SPDX-License-Identifier: CC0-1.0

//! The selections a backup plan HOLDS, as `ListBackupSelections` projects them,
//! driven as real HTTP through the router.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over rather than on one recorded request:
//!
//! * every selection `CreateBackupSelection` accepted for a plan is committed
//!   under that plan id and reported as its OWN `BackupSelectionsListMember`,
//!   carrying that selection's `SelectionId`, `SelectionName`, `BackupPlanId`,
//!   `CreationDate` and `IamRoleArn`
//!   (`smithy_query(query="shape",shape="BackupSelectionsListMember")`) —
//!   whatever the number of them, whatever grammar each one selects by, and
//!   however many refused creates came before each one; a refused create is
//!   committed nowhere, so no listed member describes it;
//! * what makes a create the duplicate of a selection the plan already holds is
//!   the selection DOCUMENT it states, not the `SelectionName` it carries: a
//!   create stating a document the plan does not hold is a selection of its own,
//!   with its own `SelectionId`, even where it shares its name with one the plan
//!   holds, while a create restating a document the plan holds is refused
//!   `AlreadyExistsException` (HTTP 400) and leaves the plan's selections as
//!   they were;
//! * `MaxResults` is the only thing that truncates the list: a read that asks
//!   for no page size reports every selection the plan holds and no cursor.
//!
//! The Backup wire is the shared harness in `vault_and_backup_job.rs`; IAM is
//! mounted beside Backup on one engine (`plan_and_selection_refusals.rs`'s
//! `router_with_iam`) because a selection's role is resolved against this
//! account's IAM before anything is stored, and the documents that resolution
//! reads are the ones `CreateRole` and `PutRolePolicy` wrote through IAM's own
//! wire.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::plan_and_selection_refusals::{create_role, put_role_policy, router_with_iam};
use super::vault_and_backup_job::{call_reported, ACCOUNT, REGION};

/// The token and sentence a create naming a role AWS Backup cannot assume is
/// refused with (`crate::state`'s `ERROR_CODE_ROLE_NOT_ASSUMABLE` /
/// `MESSAGE_ROLE_NOT_ASSUMABLE`) — the mistake each row below recovers from.
const ROLE_NOT_ASSUMABLE_CODE: &str = "ERROR_3017";
const ROLE_NOT_ASSUMABLE_MESSAGE: &str = "IAM Role cannot be assumed by AWS Backup";

/// The token and sentence a create restating a selection document the plan
/// already holds is refused with (`crate::state`'s
/// `ERROR_CODE_SELECTION_ALREADY_EXISTS` / `MESSAGE_SELECTION_ALREADY_EXISTS`).
const DOCUMENT_EXISTS_CODE: &str = "ERROR_1301";
const DOCUMENT_EXISTS_MESSAGE: &str =
    "Backup selection with the same selection document already exists";

/// The service principal a role has to admit for AWS Backup to assume it, so
/// every accepted row below gets as far as the rule under test.
const SERVICE_PRINCIPAL: &str = "backup.amazonaws.com";

/// The tagging-API read AWS Backup makes under a selection's role to resolve a
/// tag-based selection, granted to every role here so the tag-based rows are
/// accepted on their document rather than on a permission.
const TAG_READ_ACTION: &str = "tag:GetResources";

/// The one display name the second rule's selections all share.
const SHARED_NAME: &str = "shared-name";

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

/// A role of `name` this account's IAM holds, assumable by AWS Backup and
/// allowed the tagging read, answered as the ARN IAM minted for it.
async fn selection_role(router: &MultiServiceRouter, name: &str) -> String {
    let role = create_role(router, "/", name, &backup_trust()).await;
    put_role_policy(router, name, &tag_read_policy()).await;
    role
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

/// `GET /backup/plans/{BackupPlanId}/selections` — `ListBackupSelections`'s,
/// with the page size when one is asked for.
async fn list_selections(
    router: &MultiServiceRouter,
    plan_id: &str,
    max_results: Option<usize>,
) -> (u16, Value) {
    let path = match max_results {
        None => format!("/backup/plans/{plan_id}/selections"),
        Some(max) => format!("/backup/plans/{plan_id}/selections?maxResults={max}"),
    };
    let (status, _, body) = call_reported(router, "GET", &path, None).await;
    (status, body)
}

/// The members of a listing's `BackupSelectionsList`.
fn listed_members(listed: &Value) -> Vec<Value> {
    listed["BackupSelectionsList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `SelectionId`s a plan's listing reports, in the order it reports them.
async fn listed_ids(router: &MultiServiceRouter, plan_id: &str) -> Vec<String> {
    let (status, listed) = list_selections(router, plan_id, None).await;
    assert_eq!(
        status, 200,
        "expected ListBackupSelections 200, got {status} {listed:?}"
    );
    listed_members(&listed)
        .iter()
        .map(|entry| member(entry, "SelectionId"))
        .collect()
}

/// One legacy `ListOfTags` condition.
fn list_of_tags() -> Value {
    json!([{
        "ConditionType": "STRINGEQUALS",
        "ConditionKey": "backup",
        "ConditionValue": "daily",
    }])
}

/// The ARN of a resource of this account a selection can assign.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}")
}

/// The grammars a selection assigns its resources in, cycled over so the rows
/// of the count table differ in document as well as in number. SOURCE: the
/// grammars the model gives a selection
/// (`smithy_query(query="shape",shape="BackupSelection")` and `Conditions`):
/// `Resources`, `NotResources`, the legacy `ListOfTags` conditions and the
/// `Conditions` operator lists.
fn grammars() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one resource ARN",
            json!({"Resources": [table_arn("orders")]}),
        ),
        (
            "two resource ARNs and an exclusion",
            json!({
                "Resources": [table_arn("invoices"), table_arn("ledger")],
                "NotResources": [table_arn("scratch")],
            }),
        ),
        (
            "one legacy tag condition",
            json!({"ListOfTags": list_of_tags()}),
        ),
        (
            "one Conditions operator",
            json!({
                "Conditions": {
                    "StringLike": [{
                        "ConditionKey": "aws:ResourceTag/app",
                        "ConditionValue": "prod*",
                    }],
                },
            }),
        ),
    ]
}

/// Every selection a plan accepted is a member of its listing — one member per
/// selection, carrying that selection's own metadata — however many of them the
/// plan holds and however many refused creates preceded each one.
#[tokio::test]
async fn every_accepted_selection_is_a_member_of_the_plans_list() {
    // The rows differ in the NUMBER of selections the plan holds: one, the two
    // the recorded trajectory ends with, and more than two, so the rule is not
    // read as being about a particular count.
    for count in [1_usize, 2, 3, 5] {
        // One emulator per row, so a row sees only what it created.
        let router = router_with_iam();
        let role = selection_role(&router, "held-selections-role").await;
        let plan_id = plan_id_of(&router, "held-selections-plan").await;
        // A role ARN this account's IAM holds nothing under: the mistake each
        // create below is first made with, and the role no listed member may
        // ever carry.
        let absent = format!("arn:aws:iam::{ACCOUNT}:role/absent-selection-role");

        let grammars = grammars();
        let mut accepted: Vec<(String, String)> = Vec::new();
        for index in 0..count {
            let (grammar, document) = &grammars[index % grammars.len()];
            let name = format!("selection-{index}");
            let case = format!("{count} selection(s), {name:?} by {grammar}");

            // The mistake: the document under the role nothing answers to is
            // refused, and nothing of it is committed.
            let mut mistaken = document.clone();
            mistaken["SelectionName"] = json!(name);
            mistaken["IamRoleArn"] = json!(absent);
            let (status, errortype, refused) =
                create_selection(&router, &plan_id, &mistaken).await;
            assert_eq!(
                (
                    status,
                    errortype.clone(),
                    member(&refused, "Code"),
                    member(&refused, "Message"),
                ),
                (
                    400,
                    "InvalidParameterValueException".to_string(),
                    ROLE_NOT_ASSUMABLE_CODE.to_string(),
                    ROLE_NOT_ASSUMABLE_MESSAGE.to_string(),
                ),
                "case {case:?}: expected the unassumable-role refusal, got {status} \
                 {errortype} {refused:?}"
            );

            // The recovery: the SAME document under the corrected role is
            // accepted, so the plan holds one more selection than before.
            let mut corrected = document.clone();
            corrected["SelectionName"] = json!(name);
            corrected["IamRoleArn"] = json!(role);
            let (status, errortype, created) =
                create_selection(&router, &plan_id, &corrected).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateBackupSelection 200, got {status} {errortype} \
                 {created:?}"
            );
            let selection_id = member(&created, "SelectionId");
            assert!(
                !accepted.iter().any(|(held, _)| held == &selection_id),
                "case {case:?}: expected a SelectionId of its own, got {selection_id:?} again"
            );
            accepted.push((selection_id, name));
        }

        // The read that asks for no page reports EVERY accepted selection, one
        // member each, and no cursor.
        let (status, listed) = list_selections(&router, &plan_id, None).await;
        let members = listed_members(&listed);
        assert_eq!(
            (status, members.len()),
            (200, accepted.len()),
            "case {count}: expected one member per accepted selection, got {status} {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_none(),
            "case {count}: expected no cursor where no page size was asked for, got {listed:?}"
        );

        let reported: Vec<(String, String)> = members
            .iter()
            .map(|entry| (member(entry, "SelectionId"), member(entry, "SelectionName")))
            .collect();
        assert!(
            reported.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "case {count}: expected the listing in SelectionId order, got {reported:?}"
        );
        let mut sorted_reported = reported.clone();
        sorted_reported.sort();
        let mut sorted_accepted = accepted.clone();
        sorted_accepted.sort();
        assert_eq!(
            sorted_reported, sorted_accepted,
            "case {count}: expected each accepted selection listed under its own id and name, \
             got {reported:?}"
        );
        for entry in &members {
            assert_eq!(
                (
                    member(entry, "BackupPlanId"),
                    member(entry, "IamRoleArn"),
                    entry["CreationDate"].is_number(),
                ),
                (plan_id.clone(), role.clone(), true),
                "case {count}: expected the member to describe its own selection, got {entry:?}"
            );
            assert!(
                entry.get("CreatorRequestId").is_none_or(Value::is_null),
                "case {count}: a selection created with no token reports none, got {entry:?}"
            );
        }

        // A page size that covers the set truncates nothing, and the smallest
        // one truncates only because it was asked for.
        for max_results in [accepted.len(), 1000] {
            let (status, page) = list_selections(&router, &plan_id, Some(max_results)).await;
            assert_eq!(
                (
                    status,
                    listed_members(&page).len(),
                    page.get("NextToken").is_some(),
                ),
                (200, accepted.len(), false),
                "case {count}/maxResults={max_results}: expected the whole set and no cursor, \
                 got {status} {page:?}"
            );
        }
        let (status, page) = list_selections(&router, &plan_id, Some(1)).await;
        assert_eq!(
            (
                status,
                listed_members(&page).len(),
                page.get("NextToken").is_some(),
            ),
            (200, 1, accepted.len() > 1),
            "case {count}/maxResults=1: expected one member and a cursor for the rest, got \
             {status} {page:?}"
        );
    }
}

/// The documents that differ from each other while sharing ONE display name,
/// each stated as the members a create sends. The rows differ along every
/// member of the document a selection can be told apart by: which resources it
/// assigns, how many, what it excludes, which tag grammar it selects by, and
/// the role it acts under.
fn documents_sharing_one_name(role: &str, other_role: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "one resource ARN",
            json!({"IamRoleArn": role, "Resources": [table_arn("orders")]}),
        ),
        (
            "another resource ARN",
            json!({"IamRoleArn": role, "Resources": [table_arn("invoices")]}),
        ),
        (
            "a second resource beside the first",
            json!({
                "IamRoleArn": role,
                "Resources": [table_arn("orders"), table_arn("invoices")],
            }),
        ),
        (
            "the first resource with an exclusion",
            json!({
                "IamRoleArn": role,
                "Resources": [table_arn("orders")],
                "NotResources": [table_arn("scratch")],
            }),
        ),
        (
            "a legacy tag condition instead of resources",
            json!({"IamRoleArn": role, "ListOfTags": list_of_tags()}),
        ),
        (
            "a Conditions operator instead of resources",
            json!({
                "IamRoleArn": role,
                "Conditions": {
                    "StringEquals": [{
                        "ConditionKey": "aws:ResourceTag/backup",
                        "ConditionValue": "daily",
                    }],
                },
            }),
        ),
        (
            "the first document under another role",
            json!({"IamRoleArn": other_role, "Resources": [table_arn("orders")]}),
        ),
    ]
}

/// A plan holds one selection per DOCUMENT, not per name: selections that share
/// a `SelectionName` and differ in their document are each a selection of the
/// plan, and only a create restating a document the plan holds is the conflict.
#[tokio::test]
async fn a_plan_holds_one_selection_per_document_not_per_name() {
    let router = router_with_iam();
    let role = selection_role(&router, "shared-name-role").await;
    let other_role = selection_role(&router, "shared-name-role-other").await;
    let plan_id = plan_id_of(&router, "shared-name-plan").await;

    let mut held: Vec<String> = Vec::new();
    for (case, document) in documents_sharing_one_name(&role, &other_role) {
        let mut sent = document.clone();
        sent["SelectionName"] = json!(SHARED_NAME);

        let (status, errortype, created) = create_selection(&router, &plan_id, &sent).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a document the plan does not hold to be accepted under \
             the name it shares, got {status} {errortype} {created:?}"
        );
        let selection_id = member(&created, "SelectionId");
        assert!(
            !held.contains(&selection_id),
            "case {case:?}: expected a selection of its own, got {selection_id:?} again"
        );
        held.push(selection_id);

        // Restating that document adds nothing: it is refused with the
        // conflict's own token and sentence, and the plan holds what it held.
        let (status, errortype, refused) = create_selection(&router, &plan_id, &sent).await;
        assert_eq!(
            (
                status,
                errortype.clone(),
                member(&refused, "Code"),
                member(&refused, "Message"),
            ),
            (
                400,
                "AlreadyExistsException".to_string(),
                DOCUMENT_EXISTS_CODE.to_string(),
                DOCUMENT_EXISTS_MESSAGE.to_string(),
            ),
            "case {case:?}: expected the restated-document refusal, got {status} {errortype} \
             {refused:?}"
        );
        let mut expected = held.clone();
        expected.sort();
        let listed = listed_ids(&router, &plan_id).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: a refused create must leave the plan's selections as they were, \
             got {listed:?}"
        );
    }

    // ...and every one of them is listed under the one name they share.
    let (status, listed) = list_selections(&router, &plan_id, None).await;
    let names: Vec<String> = listed_members(&listed)
        .iter()
        .map(|entry| member(entry, "SelectionName"))
        .collect();
    assert_eq!(
        (status, names.len()),
        (200, held.len()),
        "expected one member per stored document, got {status} {listed:?}"
    );
    assert!(
        names.iter().all(|name| name == SHARED_NAME),
        "expected every member to report the name they share, got {names:?}"
    );
}
