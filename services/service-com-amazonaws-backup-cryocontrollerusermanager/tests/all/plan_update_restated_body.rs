// SPDX-License-Identifier: CC0-1.0

//! What `UpdateBackupPlan` does with a body that states the plan document the
//! account ALREADY holds, driven as real HTTP through the router.
//!
//! The rule under test: an update stores another version of a plan only when the
//! plan document it states DIFFERS from the one in force. An update that
//! restates that document stores nothing — the plan keeps the `VersionId` it
//! holds, that version keeps its `RuleId`s and its `CreationDate`, the version
//! in force is not superseded, and the call answers with the version in force.
//! What is compared is the DOCUMENT the service holds, so a body that spells out
//! a member the service would have filled in itself (`StartWindowMinutes`,
//! `CompletionWindowMinutes`, `ScheduleExpressionTimezone`, a settings list left
//! empty) restates it, while a body that changes one member, reorders the rules
//! or renames the plan states another document and is stored as the next
//! version, with a token of its own.
//!
//! RECORDED behaviour (corpus `simparity-wire-aws-v1`, fixture
//! `backup-rbx-traj-update-plan-refusals`): of two successful updates of one
//! plan, the one that restated the stored document answered with the `VersionId`
//! the plan already held, and the one that changed the document answered with a
//! new one. The model says only that "the new version is uniquely identified by
//! its ID" (`smithy_query(query="operation",operation="UpdateBackupPlan")`),
//! which is about the version an update ADDS.
//!
//! The rows span the ways a body can restate one document without being the same
//! request text, crossed against the smallest changes that do state another
//! document — which is what the rule generalises over; the behavioural instance
//! (a one-rule plan restated verbatim) is one row.
//!
//! The request harness (`router`, `call`, `create_vault`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, ACCOUNT, REGION};

/// The vaults every row's rules target: a rule's `TargetBackupVaultName` names a
/// vault of the caller's account and Region, and an update resolves every one of
/// them before it stores anything.
const VAULTS: [&str; 2] = ["plan-vault", "other-vault"];

/// A router with the account's vaults already created.
async fn given_vaults() -> MultiServiceRouter {
    let router = super::vault_and_backup_job::router();
    for name in VAULTS {
        let (status, created) = create_vault(&router, name, "{}").await;
        assert_eq!(
            status, 200,
            "setup: expected CreateBackupVault 200 for {name}, got {status} {created:?}"
        );
    }
    router
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

/// A string member of a JSON object, or the empty string.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ARN a copy action's destination names, in another Region, as a
/// cross-region copy action carries it.
fn far_away_vault() -> String {
    format!("arn:aws:backup:us-east-1:{ACCOUNT}:backup-vault:far-away")
}

/// The ARN of the air-gapped vault one row's rule names.
fn air_gapped_vault() -> String {
    format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:air-gapped")
}

/// A plan of one rule, with the members a caller usually states.
fn one_rule_plan(name: &str) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{
            "RuleName": "daily",
            "TargetBackupVaultName": VAULTS[0],
            "ScheduleExpression": "cron(0 5 * * ? *)",
        }],
    })
}

/// A plan of two rules, each carrying a different shape of rule.
fn two_rule_plan(name: &str) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [
            {
                "RuleName": "daily",
                "TargetBackupVaultName": VAULTS[0],
                "ScheduleExpression": "cron(0 5 * * ? *)",
                "Lifecycle": {"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 120},
                "RecoveryPointTags": {"tier": "hot"},
                "CopyActions": [{
                    "DestinationBackupVaultArn": far_away_vault(),
                    "Lifecycle": {"DeleteAfterDays": 400},
                }],
            },
            {
                "RuleName": "weekly",
                "TargetBackupVaultName": VAULTS[1],
                "ScheduleExpression": "cron(0 5 ? * 1 *)",
                "ScheduleExpressionTimezone": "Europe/Berlin",
                "EnableContinuousBackup": false,
                "TargetLogicallyAirGappedBackupVaultArn": air_gapped_vault(),
            },
        ],
    })
}

/// The windows and the time zone this service fills in where a rule states none
/// (`state::DEFAULT_START_WINDOW_MINUTES`,
/// `state::DEFAULT_COMPLETION_WINDOW_MINUTES` and the model's documented default
/// UTC zone), so a row can spell out exactly what was stored for it.
const DEFAULT_START_WINDOW_MINUTES: i64 = 480;
const DEFAULT_COMPLETION_WINDOW_MINUTES: i64 = 10080;
const DEFAULT_TIMEZONE: &str = "Etc/UTC";

/// The rows: a plan to create, and a body that states the SAME document a
/// different way.
fn restatements() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "the created body, restated verbatim",
            one_rule_plan("restated-plan"),
            one_rule_plan("restated-plan"),
        ),
        (
            "the created body with the windows the service filled in spelled out",
            one_rule_plan("restated-plan"),
            json!({
                "BackupPlanName": "restated-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                    "StartWindowMinutes": DEFAULT_START_WINDOW_MINUTES,
                    "CompletionWindowMinutes": DEFAULT_COMPLETION_WINDOW_MINUTES,
                }],
            }),
        ),
        (
            "the created body with the default time zone spelled out",
            one_rule_plan("restated-plan"),
            json!({
                "BackupPlanName": "restated-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                    "ScheduleExpressionTimezone": DEFAULT_TIMEZONE,
                }],
            }),
        ),
        (
            "the created body with the settings lists stated empty",
            one_rule_plan("restated-plan"),
            json!({
                "BackupPlanName": "restated-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                }],
                "AdvancedBackupSettings": [],
                "ScanSettings": [],
            }),
        ),
        (
            "a plan created with the windows spelled out, restated without them",
            json!({
                "BackupPlanName": "window-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                    "StartWindowMinutes": DEFAULT_START_WINDOW_MINUTES,
                    "CompletionWindowMinutes": DEFAULT_COMPLETION_WINDOW_MINUTES,
                }],
            }),
            json!({
                "BackupPlanName": "window-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                }],
            }),
        ),
        (
            "a two-rule plan with lifecycles, tags and a copy action, restated verbatim",
            two_rule_plan("two-rule-plan"),
            two_rule_plan("two-rule-plan"),
        ),
        (
            "a plan carrying advanced and scan settings, restated verbatim",
            settings_plan("settings-plan"),
            settings_plan("settings-plan"),
        ),
    ]
}

/// A plan whose body carries both settings lists, so a restatement of one is
/// exercised too.
fn settings_plan(name: &str) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{"RuleName": "daily", "TargetBackupVaultName": VAULTS[0]}],
        "AdvancedBackupSettings": [{
            "ResourceType": "EC2",
            "BackupOptions": {"WindowsVSS": "enabled"},
        }],
        "ScanSettings": [{
            "MalwareScanner": "GUARDDUTY",
            "ResourceTypes": ["EBS"],
            "ScannerRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/scanner"),
        }],
    })
}

/// The contrast rows: the smallest ways a body can state ANOTHER document than
/// the one in force, each of which stores a version.
fn changes() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "one window moved by a single minute",
            one_rule_plan("changed-plan"),
            json!({
                "BackupPlanName": "changed-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": VAULTS[0],
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                    "StartWindowMinutes": DEFAULT_START_WINDOW_MINUTES + 1,
                }],
            }),
        ),
        (
            "the plan renamed, the rules untouched",
            one_rule_plan("changed-plan"),
            one_rule_plan("changed-plan-renamed"),
        ),
        (
            "a second rule added",
            one_rule_plan("changed-plan"),
            json!({
                "BackupPlanName": "changed-plan",
                "Rules": [
                    {
                        "RuleName": "daily",
                        "TargetBackupVaultName": VAULTS[0],
                        "ScheduleExpression": "cron(0 5 * * ? *)",
                    },
                    {"RuleName": "weekly", "TargetBackupVaultName": VAULTS[1]},
                ],
            }),
        ),
        (
            "the same two rules in the other order",
            two_rule_plan("ordered-plan"),
            reversed_rules(two_rule_plan("ordered-plan")),
        ),
        (
            "one recovery-point tag given another value",
            two_rule_plan("tagged-plan"),
            retagged(two_rule_plan("tagged-plan")),
        ),
    ]
}

/// The same plan body with its rules in the opposite order.
fn reversed_rules(mut plan: Value) -> Value {
    let rules = plan["Rules"].as_array_mut().expect("a plan lists rules");
    rules.reverse();
    plan
}

/// The same plan body with the first rule's one recovery-point tag re-valued.
fn retagged(mut plan: Value) -> Value {
    plan["Rules"][0]["RecoveryPointTags"] = json!({"tier": "cold"});
    plan
}

/// The rules a plan reads back with, `RuleId`s included: the service's own name
/// for each rule, which an update that stores no version leaves alone.
fn read_rules(read: &Value) -> Vec<Value> {
    read["BackupPlan"]["Rules"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// An update that restates the document in force stores nothing: it answers with
/// the version the plan already holds, that version keeps its rule ids and its
/// creation instant, and it is not superseded — so the plan still holds exactly
/// the version the create stored.
#[tokio::test]
async fn an_update_that_restates_the_document_in_force_stores_no_version() {
    for (case, created_body, restating_body) in restatements() {
        let router = given_vaults().await;
        let (status, created) = create_plan(&router, &created_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        let created_version = text(&created, "VersionId");
        let (status, before) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBackupPlan 200 before the update, got {status} {before:?}"
        );

        // Twice over: restating the document in force is idempotent, so the
        // second restatement answers exactly what the first did.
        for attempt in 0..2 {
            let (status, answered) = update_plan(&router, &plan_id, &restating_body).await;
            assert_eq!(
                (
                    status,
                    text(&answered, "VersionId"),
                    answered.get("BackupPlanId").cloned(),
                    answered.get("BackupPlanArn").cloned(),
                    answered.get("CreationDate").cloned(),
                ),
                (
                    200,
                    created_version.clone(),
                    created.get("BackupPlanId").cloned(),
                    created.get("BackupPlanArn").cloned(),
                    created.get("CreationDate").cloned(),
                ),
                "case {case:?} / attempt {attempt}: expected the version in force, \
                 got {status} {answered:?}"
            );

            // The stored plan is untouched: the same version, the same rules
            // under the same rule ids, the same instant.
            let (status, read) = get_plan(&router, &plan_id, None).await;
            assert_eq!(
                (
                    status,
                    text(&read, "VersionId"),
                    read_rules(&read),
                    read.get("CreationDate").cloned(),
                ),
                (
                    200,
                    created_version.clone(),
                    read_rules(&before),
                    before.get("CreationDate").cloned(),
                ),
                "case {case:?} / attempt {attempt}: the plan must read back unchanged, \
                 got {status} {read:?}"
            );

            // Nothing superseded the version the create stored, so it reports no
            // deletion instant: an update that stored a version would have given
            // it one.
            let (status, pinned) = get_plan(&router, &plan_id, Some(&created_version)).await;
            assert_eq!(
                (status, pinned.get("DeletionDate").cloned()),
                (200, None),
                "case {case:?} / attempt {attempt}: the version in force must not be \
                 superseded, got {status} {pinned:?}"
            );
        }
    }
}

/// The contrast: a body that states another document IS stored as the next
/// version — a token of its own, a creation instant of its own, and the version
/// it replaces superseded the instant it was stored.
#[tokio::test]
async fn an_update_that_states_another_document_stores_the_next_version() {
    for (case, created_body, changed_body) in changes() {
        let router = given_vaults().await;
        let (status, created) = create_plan(&router, &created_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        let created_version = text(&created, "VersionId");

        let (status, answered) = update_plan(&router, &plan_id, &changed_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {answered:?}"
        );
        let stored_version = text(&answered, "VersionId");
        assert!(
            !stored_version.is_empty() && stored_version != created_version,
            "case {case:?}: expected a version of its own, got {stored_version:?} against the \
             created {created_version:?}"
        );

        // The plan now answers from the version this update stored.
        let (status, read) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            (status, text(&read, "VersionId"), read.get("CreationDate")),
            (200, stored_version.clone(), answered.get("CreationDate")),
            "case {case:?}: the plan's current version is the update's, got {status} {read:?}"
        );
        // The version it replaced was superseded the instant this one was
        // stored, and still reads back under its own id.
        let (status, pinned) = get_plan(&router, &plan_id, Some(&created_version)).await;
        assert_eq!(
            (
                status,
                text(&pinned, "VersionId"),
                pinned.get("DeletionDate"),
            ),
            (200, created_version.clone(), answered.get("CreationDate")),
            "case {case:?}: the superseded version reports the instant it was superseded, \
             got {status} {pinned:?}"
        );
    }
}
