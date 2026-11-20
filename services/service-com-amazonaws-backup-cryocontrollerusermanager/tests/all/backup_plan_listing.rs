// SPDX-License-Identifier: CC0-1.0

//! `ListBackupPlans`: what the account's plans look like in a listing, driven as
//! real HTTP through the router.
//!
//! The rules under test:
//!
//! * the listing reports one member per plan the account holds, each at the
//!   plan's CURRENT version — its id, ARN, name, version id and creation instant
//!   — so a plan created or updated is reported exactly as the create, the update
//!   and `GetBackupPlan` report it;
//! * "lists the ACTIVE backup plans for the account" (this operation's own
//!   documentation): a plan whose deletion was recorded is left out, and reported
//!   with its `DeletionDate` only for a request that asks for the deleted ones
//!   ("a Boolean value with a default value of FALSE that returns deleted backup
//!   plans when set to TRUE",
//!   `smithy_query(query="doc",shape="ListBackupPlansInput",member="IncludeDeleted")`);
//! * a page is `MaxResults` members plus a cursor to the next one, and walking
//!   the cursors visits every plan exactly once.
//!
//! The request harness (`router`, `call`) is the one `vault_and_backup_job.rs`
//! exposes to this test binary.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, router};

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, name: &str) -> Value {
    let body = json!({
        "BackupPlan": {
            "BackupPlanName": name,
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "listing-vault"}],
        }
    });
    let (status, created) = call(router, "PUT", "/backup/plans", Some(&body.to_string())).await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan {name:?} expected 200, got {status} {created:?}"
    );
    created
}

/// The backup vault this suite's rules target, created before any plan names
/// it: a rule's `TargetBackupVaultName` names a vault of the caller's account
/// and Region, which an update resolves before it stores a new version.
async fn given_target_vault(router: &MultiServiceRouter) {
    let (status, created) = create_vault(router, "listing-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault expected 200, got {status} {created:?}"
    );
}

/// `GET /backup/plans` — `ListBackupPlans`'s binding, with its query members.
async fn list_plans(
    router: &MultiServiceRouter,
    include_deleted: bool,
    page: Option<(usize, Option<&str>)>,
) -> (u16, Value) {
    let mut path = format!("/backup/plans?includeDeleted={include_deleted}");
    if let Some((max_results, token)) = page {
        path.push_str(&format!("&maxResults={max_results}"));
        if let Some(token) = token {
            path.push_str(&format!("&nextToken={token}"));
        }
    }
    call(router, "GET", &path, None).await
}

/// The members of one listing, as (`BackupPlanId`, `BackupPlanName`,
/// `VersionId`) triples in the order they were reported.
fn listed(body: &Value) -> Vec<(String, String, String)> {
    body["BackupPlansList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|plan| {
            (
                member(plan, "BackupPlanId"),
                member(plan, "BackupPlanName"),
                member(plan, "VersionId"),
            )
        })
        .collect()
}

/// A plan the account holds is listed at its current version; a deleted one is
/// listed only when the request asks for the deleted plans.
#[tokio::test]
async fn the_listing_reports_the_accounts_plans_at_their_current_version() {
    let router = router();
    given_target_vault(&router).await;

    // Nothing created, nothing listed — the empty account is a page, not a
    // refusal.
    let (status, empty) = list_plans(&router, false, None).await;
    assert_eq!(
        (status, listed(&empty)),
        (200, Vec::new()),
        "case \"an account holding no plan\": expected an empty list, got {status} {empty:?}"
    );

    let first = create_plan(&router, "listed-first").await;
    let second = create_plan(&router, "listed-second").await;
    let (status, both) = list_plans(&router, false, None).await;
    let mut reported = listed(&both);
    reported.sort();
    let mut expected = vec![
        (
            member(&first, "BackupPlanId"),
            "listed-first".to_string(),
            member(&first, "VersionId"),
        ),
        (
            member(&second, "BackupPlanId"),
            "listed-second".to_string(),
            member(&second, "VersionId"),
        ),
    ];
    expected.sort();
    assert_eq!(
        (status, reported),
        (200, expected),
        "case \"two plans created\": expected both at the version the create minted, got \
         {status} {both:?}"
    );
    // The ARN a member reports is the one the create answered with.
    let arns: Vec<String> = both["BackupPlansList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|plan| member(plan, "BackupPlanArn"))
        .collect();
    assert!(
        arns.contains(&member(&first, "BackupPlanArn")),
        "case \"two plans created\": expected the listing to report the ARN the create \
         answered, got {arns:?}"
    );

    // An update moves the plan's current version, which is the version the
    // listing reports.
    let plan_id = member(&first, "BackupPlanId");
    let (status, updated) = call(
        &router,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(
            &json!({
                "BackupPlan": {
                    "BackupPlanName": "listed-renamed",
                    "Rules": [{"RuleName": "weekly", "TargetBackupVaultName": "listing-vault"}],
                }
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: UpdateBackupPlan expected 200, got {status} {updated:?}"
    );
    let (status, after) = list_plans(&router, false, None).await;
    assert!(
        status == 200
            && listed(&after).contains(&(
                plan_id.clone(),
                "listed-renamed".to_string(),
                member(&updated, "VersionId"),
            )),
        "case \"a plan updated\": expected the listing to report the current version, got \
         {status} {after:?}"
    );

    // A deleted plan is not an active plan.
    let (status, deleted) =
        call(&router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await;
    assert_eq!(
        status, 200,
        "setup: DeleteBackupPlan expected 200, got {status} {deleted:?}"
    );
    let (status, active) = list_plans(&router, false, None).await;
    let ids: Vec<String> = listed(&active).into_iter().map(|plan| plan.0).collect();
    assert_eq!(
        (status, ids),
        (200, vec![member(&second, "BackupPlanId")]),
        "case \"a plan deleted\": expected only the plan still held, got {status} {active:?}"
    );

    // ...and it is reported, with the instant of its deletion, exactly for the
    // request that asks for the deleted ones.
    let (status, with_deleted) = list_plans(&router, true, None).await;
    let deletion_dates: Vec<Option<Value>> = with_deleted["BackupPlansList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|plan| member(plan, "BackupPlanId") == plan_id)
        .map(|plan| plan.get("DeletionDate").cloned())
        .collect();
    assert_eq!(
        (status, deletion_dates.len()),
        (200, 1),
        "case \"includeDeleted\": expected the deleted plan to be reported once, got {status} \
         {with_deleted:?}"
    );
    assert!(
        deletion_dates[0]
            .as_ref()
            .is_some_and(Value::is_number),
        "case \"includeDeleted\": expected the deleted plan to report its DeletionDate, got \
         {with_deleted:?}"
    );
}

/// A page is `MaxResults` members plus a cursor, and walking the cursors visits
/// every plan the account holds exactly once.
#[tokio::test]
async fn the_listing_is_paginated_by_max_results_and_its_cursor() {
    let router = router();
    let mut created = Vec::new();
    for index in 0..5 {
        let plan = create_plan(&router, &format!("paged-{index}")).await;
        created.push(member(&plan, "BackupPlanId"));
    }
    created.sort();

    for page_size in [1_usize, 2, 5, 10] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (status, page) =
                list_plans(&router, false, Some((page_size, token.as_deref()))).await;
            assert_eq!(
                status, 200,
                "case {page_size}: expected ListBackupPlans 200, got {status} {page:?}"
            );
            let members = listed(&page);
            assert!(
                members.len() <= page_size,
                "case {page_size}: a page must hold at most {page_size} members, got \
                 {} in {page:?}",
                members.len()
            );
            seen.extend(members.into_iter().map(|plan| plan.0));
            token = page.get("NextToken").and_then(Value::as_str).map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        seen.sort();
        assert_eq!(
            seen, created,
            "case {page_size}: walking the cursors must visit every plan exactly once"
        );
    }
}
