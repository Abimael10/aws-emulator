// SPDX-License-Identifier: CC0-1.0

//! The `DeletionDate` a plan read reports, driven as real HTTP through the
//! router.
//!
//! The rule under test: `DeletionDate` is "the date and time a backup plan is
//! deleted" OF THE VERSION the read answers from — the member
//! `GetBackupPlanOutput` reports beside the version it answers from
//! (`smithy_query(query="shape",shape="GetBackupPlanOutput")`), and the one
//! `BackupPlansListMember` carries per version — and a plan stops holding a
//! version in exactly two ways: an update stores another version in its place,
//! or the plan's deletion is recorded, which takes "the current version of a
//! backup plan. Previous versions, if any, will still exist"
//! (`smithy_query(query="doc",shape="DeleteBackupPlan")`). So every version an
//! update superseded reads back with the instant it was superseded, the version
//! in force reads back with no `DeletionDate` at all, and once the plan's
//! deletion is recorded the version it then held is no longer there to read
//! while the ones before it still are.
//!
//! The rows differ in how many versions the plan has (one to five), in what each
//! update changes about the stored body (the windows, the rule set, the plan's
//! name, nothing at all) and in how the read addresses the version (by id, or by
//! naming none) — which is what the rule generalises over; the behavioural
//! instance (a read of the version one update superseded) is one row.
//!
//! The request harness (`router`, `call`, `error_code`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, error_code, router};

/// The vault every row's rules target, so an update resolves it before storing
/// the version it was given.
const VAULT: &str = "version-vault";

/// The error shape, sentence and `Code` AWS states for "the version of the plan
/// this read asks for cannot be read" (`crate::state`'s
/// `MESSAGE_BACKUP_PLAN_VERSION_UNREADABLE` /
/// `ERROR_CODE_BACKUP_PLAN_VERSION_UNREADABLE`; `smithy_query` operation
/// `GetBackupPlan`: `ResourceNotFoundException`, `httpError` 400).
const NOT_FOUND: &str = "ResourceNotFoundException";
const VERSION_UNREADABLE: &str = "Failed reading Backup plan with provided version";
const VERSION_UNREADABLE_CODE: &str = "ERROR_2202";

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

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s HTTP binding, with the
/// optional `versionId` query parameter.
async fn get_plan(
    router: &MultiServiceRouter,
    plan_id: &str,
    version_id: Option<&str>,
) -> (u16, Value) {
    let path = match version_id {
        Some(version) => format!("/backup/plans/{plan_id}?versionId={version}"),
        None => format!("/backup/plans/{plan_id}"),
    };
    call(router, "GET", &path, None).await
}

/// `DELETE /backup/plans/{BackupPlanId}` — `DeleteBackupPlan`'s HTTP binding.
async fn delete_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await
}

/// One plan body whose one rule carries `start_window` minutes, so a version
/// read back is identifiable by a member of its own body.
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

/// The histories the table walks: for each row, the body the plan is CREATED
/// with followed by the body of every update it is given, oldest first. The rows
/// differ in how many versions the plan ends up holding and in what each update
/// changes.
fn version_histories() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        (
            "one version, nothing superseded it",
            vec![plan_body("single-version", 60)],
        ),
        (
            "one update, as the behavioural instance has",
            vec![plan_body("one-update", 60), plan_body("one-update", 480)],
        ),
        (
            "two updates, so one version is superseded by another superseded one",
            vec![
                plan_body("two-updates", 60),
                plan_body("two-updates", 120),
                plan_body("two-updates", 180),
            ],
        ),
        (
            "an update that renames the plan and one that adds a rule",
            vec![
                plan_body("renamed-plan", 60),
                plan_body("renamed-plan-again", 60),
                json!({
                    "BackupPlanName": "renamed-plan-again",
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
            "four updates, the last storing the body the plan already had",
            vec![
                plan_body("four-updates", 60),
                plan_body("four-updates", 120),
                plan_body("four-updates", 240),
                plan_body("four-updates", 300),
                plan_body("four-updates", 300),
            ],
        ),
    ]
}

/// One stored version, as the call that stored it reported it.
struct StoredVersion {
    version_id: String,
    creation_date: Value,
    body: Value,
}

/// Create the plan of `history` and give it every update the history names,
/// answering with one entry per stored version, oldest first.
async fn given_versions(
    router: &MultiServiceRouter,
    case: &str,
    history: &[Value],
) -> (String, Vec<StoredVersion>) {
    let (status, vault) = create_vault(router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupVault 200, got {status} {vault:?}"
    );
    let mut bodies = history.iter();
    let created_body = bodies.next().expect("a history creates the plan");
    let (status, created) = create_plan(router, created_body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");
    let mut stored = vec![StoredVersion {
        version_id: text(&created, "VersionId"),
        creation_date: created["CreationDate"].clone(),
        body: created_body.clone(),
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
            body: body.clone(),
        });
    }
    (plan_id, stored)
}

/// The `StartWindowMinutes` of the first rule a plan read reports, so the body a
/// version reads back with is identified rather than assumed.
fn first_start_window(read: &Value) -> Option<&Value> {
    read["BackupPlan"]["Rules"]
        .as_array()
        .and_then(|rules| rules.first())
        .and_then(|rule| rule.get("StartWindowMinutes"))
}

/// A version an update superseded reports the instant it was superseded — the
/// creation instant of the version that replaced it — while the version in force
/// reports no `DeletionDate`, whether the read names it or names none.
#[tokio::test]
async fn a_plan_read_reports_the_deletion_instant_of_the_version_it_answers_from() {
    for (case, history) in version_histories() {
        // One router per row: each row's plan is the only plan the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        let (plan_id, stored) = given_versions(&router, case, &history).await;
        let last = stored.len() - 1;

        for (position, version) in stored.iter().enumerate() {
            // Twice over: a read changes nothing a later reader sees, so the
            // second read answers exactly what the first did.
            for attempt in 0..2 {
                let (status, read) =
                    get_plan(&router, &plan_id, Some(&version.version_id)).await;
                assert_eq!(
                    (status, text(&read, "VersionId").as_str()),
                    (200, version.version_id.as_str()),
                    "case {case:?} / version {position} / read {attempt}: expected the version \
                     asked for, got {status} {read:?}"
                );
                assert_eq!(
                    read.get("CreationDate"),
                    Some(&version.creation_date),
                    "case {case:?} / version {position} / read {attempt}: a version keeps its \
                     own creation instant, got {read:?}"
                );
                assert_eq!(
                    first_start_window(&read),
                    version.body["Rules"][0].get("StartWindowMinutes"),
                    "case {case:?} / version {position} / read {attempt}: a version reads back \
                     with the body stored under it, got {read:?}"
                );
                let expected = if position == last {
                    // The version in force: nothing has stopped the plan from
                    // holding it.
                    None
                } else {
                    // Superseded: deleted the instant its successor was stored.
                    Some(&stored[position + 1].creation_date)
                };
                assert_eq!(
                    read.get("DeletionDate"),
                    expected,
                    "case {case:?} / version {position} / read {attempt}: expected \
                     DeletionDate {expected:?}, got {read:?}"
                );
            }
        }

        // The read that names no version answers from the version in force, so
        // it reports no DeletionDate either.
        let (status, current) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            (
                status,
                text(&current, "VersionId").as_str(),
                current.get("DeletionDate"),
            ),
            (200, stored[last].version_id.as_str(), None),
            "case {case:?}: the unversioned read answers from the version in force with no \
             DeletionDate, got {status} {current:?}"
        );
    }
}

/// Recording a plan's deletion deletes the version it held CURRENT: that version
/// is no longer there to read — exactly as a `VersionId` the plan never had is
/// not — while every version an update superseded still reads back, with the
/// instant it was superseded.
#[tokio::test]
async fn deleting_a_plan_takes_the_version_it_held_current_and_leaves_the_rest_readable() {
    for (case, history) in version_histories() {
        let router = router();
        let (plan_id, stored) = given_versions(&router, case, &history).await;
        let last = stored.len() - 1;

        let (status, deleted) = delete_plan(&router, &plan_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupPlan 200, got {status} {deleted:?}"
        );
        assert_eq!(
            text(&deleted, "VersionId").as_str(),
            stored[last].version_id.as_str(),
            "case {case:?}: the delete reports the version it took, got {deleted:?}"
        );

        // Neither the version the delete took nor a read that names no version
        // answers from the plan any more.
        let unreadable: Vec<(&str, Option<&str>)> = vec![
            ("no version named", None),
            (
                "the version the deletion took",
                Some(stored[last].version_id.as_str()),
            ),
        ];
        for (addressing, version) in unreadable {
            let (status, refused) = get_plan(&router, &plan_id, version).await;
            assert_eq!(
                (
                    status,
                    error_code(&refused).as_str(),
                    text(&refused, "Message").as_str(),
                    text(&refused, "Code").as_str(),
                ),
                (400, NOT_FOUND, VERSION_UNREADABLE, VERSION_UNREADABLE_CODE),
                "case {case:?} / {addressing}: expected the version to be unreadable, got \
                 {status} {refused:?}"
            );
            assert!(
                refused.get("BackupPlan").is_none(),
                "case {case:?} / {addressing}: a refusal reports no plan body, got {refused:?}"
            );
        }

        // ...and every version an update superseded still exists.
        for (position, version) in stored.iter().enumerate().take(last) {
            let (status, read) = get_plan(&router, &plan_id, Some(&version.version_id)).await;
            assert_eq!(
                (
                    status,
                    text(&read, "VersionId").as_str(),
                    read.get("DeletionDate"),
                ),
                (
                    200,
                    version.version_id.as_str(),
                    Some(&stored[position + 1].creation_date),
                ),
                "case {case:?} / version {position}: a superseded version of a deleted plan \
                 still reads back with the instant it was superseded, got {status} {read:?}"
            );
            assert_eq!(
                first_start_window(&read),
                version.body["Rules"][0].get("StartWindowMinutes"),
                "case {case:?} / version {position}: it reads back with the body stored under \
                 it, got {read:?}"
            );
        }
    }
}
