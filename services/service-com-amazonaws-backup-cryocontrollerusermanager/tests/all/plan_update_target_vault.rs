// SPDX-License-Identifier: CC0-1.0

//! What `UpdateBackupPlan` does with a rule whose target backup vault the
//! account does not hold, driven as real HTTP through the router.
//!
//! The rule under test: a rule's `TargetBackupVaultName` names "the name of a
//! logical container where backups are stored"
//! (`smithy_query(query="doc",shape="BackupRule",member="TargetBackupVaultName")`)
//! in the caller's account and Region, and an update resolves every rule's
//! target BEFORE it stores anything: an update one of whose rules names a vault
//! the account does not hold is refused in full with the modelled 400
//! `InvalidParameterValueException`, and the plan is left at the version it
//! already held — same version id, same creation instant, same body, member for
//! member. Once the account holds the vault, the very same update is stored.
//!
//! That is the validate-then-commit half of the rule: a call this service
//! refuses must leave state as it found it, so no later read of the plan reports
//! a configuration the refusal said the account could not have.
//!
//! The rows differ in which of the plan's rules names the missing vault (its
//! only one, the first of several, the last of several), in the name form the
//! rule uses, and in how many versions the plan already holds — which is what
//! the rule generalises over.
//!
//! The request harness (`router`, `call`, `error_code`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, error_code, router};

/// The vault the account DOES hold, which the created plan's rule targets.
const HELD_VAULT: &str = "held-vault";

/// The error shape, sentence and `Code` AWS states for "a backup vault this call
/// names does not exist" (`crate::state`'s
/// `MESSAGE_BACKUP_VAULT_DOES_NOT_EXIST` /
/// `ERROR_CODE_BACKUP_VAULT_DOES_NOT_EXIST`; `smithy_query` operation
/// `UpdateBackupPlan`: `InvalidParameterValueException`, `httpError` 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
const VAULT_DOES_NOT_EXIST: &str = "A Backup vault does not exist";
const VAULT_DOES_NOT_EXIST_CODE: &str = "ERROR_3201";

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

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s HTTP binding.
async fn get_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/backup/plans/{plan_id}"), None).await
}

/// The body every row's plan is created with: one rule targeting the vault the
/// account holds, with a start window a later read is identified by.
fn created_body() -> Value {
    json!({
        "BackupPlanName": "guarded-plan",
        "Rules": [{
            "RuleName": "daily",
            "TargetBackupVaultName": HELD_VAULT,
            "StartWindowMinutes": 60,
        }],
    })
}

/// The update bodies the table sends, each naming a vault the account does not
/// hold in one of its rules. The rows differ in which rule names it and in the
/// name form, and every row also states a `StartWindowMinutes` the stored plan
/// does NOT carry, so a committed refusal would be visible on the next read.
fn missing_vault_updates() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "the plan's only rule targets a vault that was never created",
            "never-created",
            json!({
                "BackupPlanName": "guarded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "never-created",
                    "StartWindowMinutes": 480,
                }],
            }),
        ),
        (
            "the FIRST of two rules targets a missing vault",
            "missing-first",
            json!({
                "BackupPlanName": "guarded-plan",
                "Rules": [
                    {
                        "RuleName": "daily",
                        "TargetBackupVaultName": "missing-first",
                        "StartWindowMinutes": 480,
                    },
                    {"RuleName": "weekly", "TargetBackupVaultName": HELD_VAULT},
                ],
            }),
        ),
        (
            "the LAST of three rules targets a missing vault",
            "missing-last",
            json!({
                "BackupPlanName": "guarded-plan",
                "Rules": [
                    {
                        "RuleName": "daily",
                        "TargetBackupVaultName": HELD_VAULT,
                        "StartWindowMinutes": 480,
                    },
                    {"RuleName": "weekly", "TargetBackupVaultName": HELD_VAULT},
                    {"RuleName": "monthly", "TargetBackupVaultName": "missing-last"},
                ],
            }),
        ),
        (
            // The alphabet `BackupVaultName` admits is `^[a-zA-Z0-9\-\_]{2,50}$`
            // (the model's pattern for the shape), so a name of underscores and
            // mixed case is one the model accepts and the account still does not
            // hold.
            "a missing vault whose name uses the whole alphabet the pattern admits",
            "Held_Vault-2",
            json!({
                "BackupPlanName": "guarded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "Held_Vault-2",
                    "StartWindowMinutes": 480,
                }],
            }),
        ),
        (
            "a two-character name, the shortest the pattern admits",
            "ab",
            json!({
                "BackupPlanName": "guarded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "ab",
                    "StartWindowMinutes": 480,
                }],
            }),
        ),
    ]
}

/// Assert that the plan `plan_id` still reads back as the version `version`
/// stored: the refused update left every member of it alone.
async fn assert_unchanged(
    router: &MultiServiceRouter,
    case: &str,
    plan_id: &str,
    stored: &Value,
    extra_versions: usize,
) {
    let (status, read) = get_plan(router, plan_id).await;
    assert_eq!(
        (
            status,
            text(&read, "VersionId").as_str(),
            read.get("CreationDate"),
        ),
        (
            200,
            text(stored, "VersionId").as_str(),
            stored.get("CreationDate"),
        ),
        "case {case:?} / after {extra_versions} accepted update(s): a refused update mints no \
         version, got {status} {read:?}"
    );
    assert_eq!(
        read["BackupPlan"].get("Rules").and_then(Value::as_array).map(Vec::len),
        Some(1),
        "case {case:?}: the stored plan still holds its one rule, got {read:?}"
    );
    assert_eq!(
        read["BackupPlan"]["Rules"][0].get("StartWindowMinutes"),
        Some(&json!(60)),
        "case {case:?}: the stored rule keeps the window it was stored with, got {read:?}"
    );
    assert_eq!(
        read["BackupPlan"]["Rules"][0].get("TargetBackupVaultName"),
        Some(&json!(HELD_VAULT)),
        "case {case:?}: the stored rule keeps the vault it was stored with, got {read:?}"
    );
    assert_eq!(
        read.get("DeletionDate"),
        None,
        "case {case:?}: the version in force reports no DeletionDate, got {read:?}"
    );
}

/// An update naming a vault the account does not hold is refused, and the plan
/// keeps the version it already held; creating that vault makes the same update
/// succeed, so the refusal is the account's vault state rather than the body.
#[tokio::test]
async fn an_update_naming_a_vault_the_account_does_not_hold_is_refused_and_stores_nothing() {
    for (case, missing_vault, update) in missing_vault_updates() {
        // One router per row: each row's plan is the only plan the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        let (status, vault) = create_vault(&router, HELD_VAULT, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupVault 200, got {status} {vault:?}"
        );
        let (status, created) = create_plan(&router, &created_body()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");

        // Twice over: a refused update is refused the same way however often it
        // is sent, and stores nothing either time.
        for attempt in 0..2 {
            let (status, refused) = update_plan(&router, &plan_id, &update).await;
            assert_eq!(
                (
                    status,
                    error_code(&refused).as_str(),
                    text(&refused, "Message").as_str(),
                    text(&refused, "Code").as_str(),
                ),
                (
                    400,
                    INVALID_PARAMETER,
                    VAULT_DOES_NOT_EXIST,
                    VAULT_DOES_NOT_EXIST_CODE,
                ),
                "case {case:?} / attempt {attempt}: expected 400 {INVALID_PARAMETER} \
                 {VAULT_DOES_NOT_EXIST:?}, got {status} {refused:?}"
            );
            assert_unchanged(&router, case, &plan_id, &created, 0).await;
        }

        // The control: once the account holds the vault the body names, the very
        // same update is stored as the plan's next version.
        let (status, vault) = create_vault(&router, missing_vault, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupVault 200 for {missing_vault}, got {status} \
             {vault:?}"
        );
        let (status, updated) = update_plan(&router, &plan_id, &update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: with the vault held, the same update must be stored, got {status} \
             {updated:?}"
        );
        let (status, read) = get_plan(&router, &plan_id).await;
        assert_eq!(
            (
                status,
                text(&read, "VersionId").as_str(),
                read["BackupPlan"]["Rules"][0].get("StartWindowMinutes"),
            ),
            (200, text(&updated, "VersionId").as_str(), Some(&json!(480))),
            "case {case:?}: the accepted update's body is what the plan now reports, got \
             {status} {read:?}"
        );
    }
}

/// The same rule on a plan that already holds SEVERAL versions: the refusal
/// leaves the version in force in force, and the versions before it untouched.
#[tokio::test]
async fn a_refused_update_leaves_a_plans_whole_version_history_alone() {
    let case = "a plan given two accepted updates, then a refused one";
    let router = router();
    let (status, vault) = create_vault(&router, HELD_VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupVault 200, got {status} {vault:?}"
    );
    let (status, created) = create_plan(&router, &created_body()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");

    // Two accepted updates, each storing the created body again, so the plan
    // holds three versions whose bodies are known.
    let mut versions = vec![created.clone()];
    for update in 0..2 {
        let (status, accepted) = update_plan(&router, &plan_id, &created_body()).await;
        assert_eq!(
            status, 200,
            "case {case:?} / update {update}: expected UpdateBackupPlan 200, got {status} \
             {accepted:?}"
        );
        versions.push(accepted);
    }
    let in_force = versions.last().expect("a plan holds a version").clone();

    let (status, refused) = update_plan(
        &router,
        &plan_id,
        &json!({
            "BackupPlanName": "guarded-plan",
            "Rules": [{
                "RuleName": "daily",
                "TargetBackupVaultName": "never-created",
                "StartWindowMinutes": 480,
            }],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, INVALID_PARAMETER),
        "case {case:?}: expected 400 {INVALID_PARAMETER}, got {status} {refused:?}"
    );
    assert_unchanged(&router, case, &plan_id, &in_force, versions.len() - 1).await;

    // Every version the plan held before the refusal still reads back under its
    // own id.
    for (position, version) in versions.iter().enumerate() {
        let version_id = text(version, "VersionId");
        let (status, read) = call(
            &router,
            "GET",
            &format!("/backup/plans/{plan_id}?versionId={version_id}"),
            None,
        )
        .await;
        assert_eq!(
            (status, text(&read, "VersionId").as_str()),
            (200, version_id.as_str()),
            "case {case:?} / version {position}: a refused update leaves every stored version \
             readable, got {status} {read:?}"
        );
    }
}
