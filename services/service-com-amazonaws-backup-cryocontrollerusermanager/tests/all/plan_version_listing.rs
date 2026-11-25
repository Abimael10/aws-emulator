// SPDX-License-Identifier: CC0-1.0

//! `ListBackupPlanVersions`: the version metadata of ONE plan, driven as real
//! HTTP through the router.
//!
//! The rule under test: the listing reports one metadata member per version the
//! named plan still holds — "version metadata of your backup plans, including
//! Amazon Resource Names (ARNs), backup plan IDs, creation and deletion dates,
//! plan names, and version IDs"
//! (`smithy_query(query="doc",shape="ListBackupPlanVersions")`) — in one stable
//! order, paged by `MaxResults` and its cursor, reporting for each version
//! exactly what a read of that version reports. Recording the plan's deletion
//! "deletes the current version of a backup plan. Previous versions, if any,
//! will still exist" (`smithy_query(query="doc",shape="DeleteBackupPlan")`), so
//! the version a deletion took is no longer one the plan holds while every
//! version an update superseded still is; a plan id the account holds no
//! version under has nothing to report, which is an empty page.
//!
//! The rows differ in how many versions the plan holds (one to five), in what
//! each update changes about the stored body (the windows, the rule set, the
//! plan's name, a copy action added), in the page size the walk asks for and in
//! whether the plan's deletion has been recorded — which is what the rule
//! generalises over; the behavioural instance (an update that adds a copy action
//! and supersedes the version in force) is one row.
//!
//! The request harness (`router`, `call`, `create_vault`, `error_code`,
//! `vault_arn`) is the one `vault_and_backup_job.rs` exposes to this test
//! binary, so there is ONE wire path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, error_code, router, vault_arn, REGION};

/// The vault every row's rules target, so a create and an update resolve it
/// before storing the version they were given.
const VAULT: &str = "plan-version-listing-vault";
/// The vault a copy action in the instance-shaped row writes its copies to.
const COPY_VAULT: &str = "plan-version-copy-vault";

/// The published bounds of `MaxResults` on this operation
/// (`smithy_query(query="operation",operation="ListBackupPlanVersions")`:
/// `range {min: 1, max: 1000}`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 1000;

/// The shape, sentence and `Code` AWS states for a page size outside that range
/// (`crate::state`'s `ERROR_CODE_MAX_RESULTS_INVALID` /
/// `MESSAGE_MAX_RESULTS_INVALID`; `smithy_query` operation
/// `ListBackupPlanVersions`: `InvalidParameterValueException`, `httpError` 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
const MAX_RESULTS_CODE: &str = "ERROR_3607";
const MAX_RESULTS_MESSAGE: &str = "Max Results is invalid";

/// A string member of a JSON object, or the empty string.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, plan: &Value) -> (u16, Value) {
    call(
        router,
        "PUT",
        "/backup/plans",
        Some(&json!({"BackupPlan": plan}).to_string()),
    )
    .await
}

/// `POST /backup/plans/{BackupPlanId}` — `UpdateBackupPlan`'s HTTP binding.
async fn update_plan(router: &MultiServiceRouter, plan_id: &str, plan: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&json!({"BackupPlan": plan}).to_string()),
    )
    .await
}

/// `DELETE /backup/plans/{BackupPlanId}` — `DeleteBackupPlan`'s HTTP binding.
async fn delete_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await
}

/// `GET /backup/plans/{BackupPlanId}/versions` — this operation's HTTP binding,
/// with its optional query members.
async fn list_versions(
    router: &MultiServiceRouter,
    plan_id: &str,
    query: &str,
) -> (u16, Value) {
    let path = if query.is_empty() {
        format!("/backup/plans/{plan_id}/versions")
    } else {
        format!("/backup/plans/{plan_id}/versions?{query}")
    };
    call(router, "GET", &path, None).await
}

/// The members of one listing, as reported, in the order they were reported.
fn listed(body: &Value) -> Vec<Value> {
    body["BackupPlanVersionsList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `VersionId`s of one listing, in the order they were reported.
fn listed_version_ids(body: &Value) -> Vec<String> {
    listed(body)
        .iter()
        .map(|version| text(version, "VersionId"))
        .collect()
}

/// One plan body whose one rule carries `start_window` minutes, so a version is
/// identifiable by a member of its own body.
fn plan_body(name: &str, start_window: i64) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{
            "RuleName": "daily",
            "TargetBackupVaultName": VAULT,
            "StartWindowMinutes": start_window,
        }],
    })
}

/// The same plan body with a copy action added to its rule — the change the
/// behavioural instance's update makes.
fn plan_body_with_copy_action(name: &str, start_window: i64) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{
            "RuleName": "daily",
            "TargetBackupVaultName": VAULT,
            "StartWindowMinutes": start_window,
            "CopyActions": [{
                "DestinationBackupVaultArn": vault_arn(REGION, COPY_VAULT),
            }],
        }],
    })
}

/// The histories the table walks: for each row, the body the plan is CREATED
/// with followed by the body of every update it is given, oldest first.
fn version_histories() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        (
            "one version, nothing superseded it",
            vec![plan_body("listed-single-version", 60)],
        ),
        (
            // The behavioural instance: one update adds a copy action to the
            // rule, superseding the version in force.
            "an update that adds a copy action",
            vec![
                plan_body("listed-copy-action", 60),
                plan_body_with_copy_action("listed-copy-action", 60),
            ],
        ),
        (
            "two updates, so one superseded version is followed by another",
            vec![
                plan_body("listed-two-updates", 60),
                plan_body("listed-two-updates", 120),
                plan_body("listed-two-updates", 180),
            ],
        ),
        (
            "an update that renames the plan and one that adds a rule",
            vec![
                plan_body("listed-renamed", 60),
                plan_body("listed-renamed-again", 60),
                json!({
                    "BackupPlanName": "listed-renamed-again",
                    "Rules": [
                        {
                            "RuleName": "daily",
                            "TargetBackupVaultName": VAULT,
                            "StartWindowMinutes": 60,
                        },
                        {"RuleName": "weekly", "TargetBackupVaultName": VAULT},
                    ],
                }),
            ],
        ),
        (
            "four updates, each storing another body",
            vec![
                plan_body("listed-four-updates", 60),
                plan_body("listed-four-updates", 120),
                plan_body("listed-four-updates", 240),
                plan_body("listed-four-updates", 300),
                plan_body("listed-four-updates", 360),
            ],
        ),
    ]
}

/// One stored version, as the call that stored it reported it.
struct StoredVersion {
    version_id: String,
    creation_date: Value,
    name: String,
}

/// The plan of `history`, created and then given every update the history
/// names: its id, its ARN and one entry per stored version, oldest first.
async fn given_versions(
    router: &MultiServiceRouter,
    case: &str,
    history: &[Value],
) -> (String, String, Vec<StoredVersion>) {
    for vault in [VAULT, COPY_VAULT] {
        let (status, created) = create_vault(router, vault, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupVault {vault:?} 200, got {status} {created:?}"
        );
    }
    let mut bodies = history.iter();
    let created_body = bodies.next().expect("a history creates the plan");
    let (status, created) = create_plan(router, created_body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");
    let plan_arn = text(&created, "BackupPlanArn");
    let mut stored = vec![StoredVersion {
        version_id: text(&created, "VersionId"),
        creation_date: created["CreationDate"].clone(),
        name: text(created_body, "BackupPlanName"),
    }];
    for body in bodies {
        let (status, updated) = update_plan(router, &plan_id, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {updated:?}"
        );
        stored.push(StoredVersion {
            version_id: text(&updated, "VersionId"),
            creation_date: updated["CreationDate"].clone(),
            name: text(body, "BackupPlanName"),
        });
    }
    (plan_id, plan_arn, stored)
}

/// The listing reports every version the plan holds, each with the identifiers
/// and instants the call that stored it reported, and the version in force
/// reports no `DeletionDate` while every superseded one reports the instant it
/// was superseded.
#[tokio::test]
async fn the_listing_reports_every_version_the_plan_holds() {
    for (case, history) in version_histories() {
        // One router per row: each row's plan is the only plan the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        let (plan_id, plan_arn, stored) = given_versions(&router, case, &history).await;
        let last = stored.len() - 1;

        // Twice over: a read changes nothing a later caller sees, so the second
        // listing answers exactly what the first did.
        for attempt in 0..2 {
            let (status, page) = list_versions(&router, &plan_id, "").await;
            assert_eq!(
                status, 200,
                "case {case:?} / read {attempt}: expected ListBackupPlanVersions 200, got \
                 {status} {page:?}"
            );
            assert_eq!(
                listed_version_ids(&page),
                stored
                    .iter()
                    .map(|version| version.version_id.clone())
                    .collect::<Vec<String>>(),
                "case {case:?} / read {attempt}: expected one member per stored version, got \
                 {page:?}"
            );
            assert!(
                page.get("NextToken").is_none(),
                "case {case:?} / read {attempt}: a listing that reported every version carries \
                 no cursor, got {page:?}"
            );
            for (position, reported) in listed(&page).iter().enumerate() {
                let version = &stored[position];
                assert_eq!(
                    (
                        text(reported, "BackupPlanId").as_str(),
                        text(reported, "BackupPlanArn").as_str(),
                        text(reported, "BackupPlanName").as_str(),
                    ),
                    (plan_id.as_str(), plan_arn.as_str(), version.name.as_str()),
                    "case {case:?} / version {position} / read {attempt}: expected the plan's \
                     own identifiers and the name this version carries, got {reported:?}"
                );
                assert_eq!(
                    reported.get("CreationDate"),
                    Some(&version.creation_date),
                    "case {case:?} / version {position} / read {attempt}: a version keeps its \
                     own creation instant, got {reported:?}"
                );
                let expected_deletion = if position == last {
                    // The version in force: nothing has stopped the plan from
                    // holding it.
                    None
                } else {
                    // Superseded: deleted the instant its successor was stored.
                    Some(&stored[position + 1].creation_date)
                };
                assert_eq!(
                    reported.get("DeletionDate"),
                    expected_deletion,
                    "case {case:?} / version {position} / read {attempt}: expected \
                     DeletionDate {expected_deletion:?}, got {reported:?}"
                );
            }
        }

        // The listing agrees with a read of each version it reported: same
        // version id, same creation instant, same deletion instant.
        for (position, version) in stored.iter().enumerate() {
            let (status, read) = call(
                &router,
                "GET",
                &format!(
                    "/backup/plans/{plan_id}?versionId={}",
                    version.version_id
                ),
                None,
            )
            .await;
            let (_, page) = list_versions(&router, &plan_id, "").await;
            let reported = listed(&page)[position].clone();
            assert_eq!(
                (
                    status,
                    text(&read, "VersionId"),
                    read.get("CreationDate").cloned(),
                    read.get("DeletionDate").cloned(),
                ),
                (
                    200,
                    text(&reported, "VersionId"),
                    reported.get("CreationDate").cloned(),
                    reported.get("DeletionDate").cloned(),
                ),
                "case {case:?} / version {position}: the listing and the read of that version \
                 must agree, got {read:?} against {reported:?}"
            );
        }
    }
}

/// A page is `MaxResults` members plus a cursor, and walking the cursors visits
/// every version the plan holds exactly once, in the listing's order.
#[tokio::test]
async fn the_listing_is_paginated_by_max_results_and_its_cursor() {
    let (case, history) = version_histories()
        .into_iter()
        .next_back()
        .expect("the table's longest history");
    let router = router();
    let (plan_id, _, stored) = given_versions(&router, case, &history).await;
    let all: Vec<String> = stored
        .iter()
        .map(|version| version.version_id.clone())
        .collect();

    for page_size in [MAX_RESULTS_MIN, 2, 4, 5, 10, MAX_RESULTS_MAX] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let query = match token.as_deref() {
                Some(token) => format!("maxResults={page_size}&nextToken={token}"),
                None => format!("maxResults={page_size}"),
            };
            let (status, page) = list_versions(&router, &plan_id, &query).await;
            assert_eq!(
                status, 200,
                "case {page_size}: expected ListBackupPlanVersions 200, got {status} {page:?}"
            );
            let members = listed_version_ids(&page);
            assert!(
                i64::try_from(members.len()).is_ok_and(|held| held <= page_size),
                "case {page_size}: a page holds at most {page_size} members, got {} in {page:?}",
                members.len()
            );
            seen.extend(members);
            token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all,
            "case {page_size}: walking the cursors must visit every version exactly once, in \
             the listing's order"
        );
    }

    // A cursor that names no position in this result set is refused rather than
    // answered with an arbitrary page.
    let (status, refused) = list_versions(&router, &plan_id, "nextToken=not-a-cursor").await;
    assert_eq!(
        (status, error_code(&refused)),
        (400, INVALID_PARAMETER.to_string()),
        "case \"a cursor of this result set\": expected the unknown cursor to be refused, got \
         {status} {refused:?}"
    );
}

/// A page size outside the published range is refused with the sentence AWS
/// states for it; each published bound is admitted.
#[tokio::test]
async fn a_page_size_outside_the_published_range_is_refused() {
    let (case, history) = version_histories()
        .into_iter()
        .next()
        .expect("the table's shortest history");
    let router = router();
    let (plan_id, _, stored) = given_versions(&router, case, &history).await;

    for refused_size in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1] {
        let (status, refused) =
            list_versions(&router, &plan_id, &format!("maxResults={refused_size}")).await;
        assert_eq!(
            (
                status,
                error_code(&refused),
                text(&refused, "Code"),
                text(&refused, "Message"),
            ),
            (
                400,
                INVALID_PARAMETER.to_string(),
                MAX_RESULTS_CODE.to_string(),
                MAX_RESULTS_MESSAGE.to_string(),
            ),
            "case {refused_size}: expected the page size to be refused, got {status} {refused:?}"
        );
        assert!(
            refused.get("BackupPlanVersionsList").is_none(),
            "case {refused_size}: a refusal reports no versions, got {refused:?}"
        );
    }

    for admitted_size in [MAX_RESULTS_MIN, MAX_RESULTS_MAX] {
        let (status, page) =
            list_versions(&router, &plan_id, &format!("maxResults={admitted_size}")).await;
        assert_eq!(
            (status, listed_version_ids(&page).len()),
            (200, stored.len()),
            "case {admitted_size}: expected the plan's one version at a published bound, got \
             {status} {page:?}"
        );
    }
}

/// Recording the plan's deletion takes the version it held current: the listing
/// stops reporting it and keeps reporting every version an update superseded.
#[tokio::test]
async fn deleting_the_plan_takes_the_version_it_held_current_from_the_listing() {
    for (case, history) in version_histories() {
        let router = router();
        let (plan_id, _, stored) = given_versions(&router, case, &history).await;
        let last = stored.len() - 1;

        let (status, deleted) = delete_plan(&router, &plan_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupPlan 200, got {status} {deleted:?}"
        );

        let (status, page) = list_versions(&router, &plan_id, "").await;
        let expected: Vec<String> = stored
            .iter()
            .take(last)
            .map(|version| version.version_id.clone())
            .collect();
        assert_eq!(
            (status, listed_version_ids(&page)),
            (200, expected),
            "case {case:?}: expected the versions the updates superseded and not the one the \
             deletion took, got {status} {page:?}"
        );
        for (position, reported) in listed(&page).iter().enumerate() {
            assert_eq!(
                reported.get("DeletionDate"),
                Some(&stored[position + 1].creation_date),
                "case {case:?} / version {position}: a superseded version of a deleted plan \
                 still reports the instant it was superseded, got {reported:?}"
            );
        }
    }
}

/// A plan id the account holds no version under has nothing to report: an empty
/// page with no cursor, whatever form the id takes.
#[tokio::test]
async fn a_plan_id_the_account_holds_no_version_under_lists_nothing() {
    let (case, history) = version_histories()
        .into_iter()
        .next()
        .expect("the table's shortest history");
    let router = router();
    let (plan_id, _, stored) = given_versions(&router, case, &history).await;

    // The account holds exactly one plan, so every id below names no version of
    // one: an id of the service's own form, an id differing from the held one in
    // its last character, a name rather than an id, and the plan's own VersionId.
    let mut neighbour = plan_id.clone();
    neighbour.pop();
    neighbour.push('0');
    let unknown: Vec<(&str, String)> = vec![
        ("an id of the service's own form", "2a1b3c4d5e6f708192a3b4c5d6e7f809".to_string()),
        ("an id one character from the held one", neighbour),
        ("a plan name rather than an id", "listed-single-version".to_string()),
        ("the plan's version id rather than its id", stored[0].version_id.clone()),
    ];
    for (addressing, id) in unknown {
        for query in ["", "maxResults=1"] {
            let (status, page) = list_versions(&router, &id, query).await;
            assert_eq!(
                (status, listed_version_ids(&page), page.get("NextToken")),
                (200, Vec::new(), None),
                "case {addressing:?} / query {query:?}: expected an empty page, got {status} \
                 {page:?}"
            );
        }
    }

    // ...and the plan the account does hold is still listed, so the empty pages
    // changed nothing.
    let (status, page) = list_versions(&router, &plan_id, "").await;
    assert_eq!(
        (status, listed_version_ids(&page).len()),
        (200, stored.len()),
        "case \"the plan the account holds\": expected its versions to still be listed, got \
         {status} {page:?}"
    );
}
