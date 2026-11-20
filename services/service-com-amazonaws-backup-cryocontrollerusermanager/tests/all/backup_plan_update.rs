// SPDX-License-Identifier: CC0-1.0

//! `UpdateBackupPlan`: storing another VERSION of a stored backup plan, driven
//! as real HTTP through the router.
//!
//! The rules under test:
//!
//! * an update stores the body it is given as another version of the plan the
//!   request names and answers with the `VersionId` it minted for it ("Updates
//!   the specified backup plan. The new version is uniquely identified by its
//!   ID"), so the plan keeps its id and its ARN while every version id it has
//!   ever answered with is distinct;
//! * a later read that names no version reports the body the latest update
//!   stored, member for member, with a `RuleId` on each rule — and a read that
//!   names a version an update superseded still reports THAT version's body, so
//!   superseding a version does not rewrite it;
//! * a body AWS does not accept is refused with the error this operation models
//!   for it and stores nothing: the plan still reports the version it had;
//! * every rule's `TargetBackupVaultName` is resolved against the account's
//!   vaults in the caller's Region BEFORE anything is stored, so an update
//!   naming a vault the account does not hold is refused in full and the plan
//!   keeps the version it already had;
//! * a `BackupPlanId` naming no live plan — one the account never held, and one
//!   whose deletion is recorded — is 400 `ResourceNotFoundException`;
//! * a mutation of a stored plan that FOLLOWS the call which created it — each
//!   update that stores another version, and the delete that records the
//!   deletion — is published once on the account's default EventBridge bus as
//!   `source` `aws.backup` / `detail-type` `Backup Plan State Change`, whose
//!   `detail` names the plan (`backupPlanId`, `versionId`) and the transition
//!   (`state`: `MODIFIED`, `DELETED`), while the plan a CREATE stored — which is
//!   what that call's own response reports — and a refused update publish
//!   nothing. Source for the envelope: the AWS Backup EventBridge
//!   notifications; the Smithy model describes the control plane only, so every
//!   `detail` member is asserted against what the SAME mutation's response
//!   reported for it.
//!
//! The rows of each table differ in what the update changes about the stored
//! body (a rule's copy actions, how many rules there are, the plan's name, the
//! windows, the advanced and scan settings) and in how many updates a plan is
//! given; the behavioural instance (one update that adds a copy action to a
//! one-rule plan) is one row.
//!
//! The request harness (`router`, `call`, `error_code`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites; the event cases drive the emulator
//! `job_state_change_events.rs` exposes — Backup, EventBridge and Amazon SQS on
//! one engine — because the publication reaches EventBridge through the
//! in-process callback and a rule on that bus delivers to a queue target.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, error_code, router, ACCOUNT, REGION};

// The emulator the event cases drive and the bus readers they share, exposed by
// the suite that pins this service's job state changes: Backup, EventBridge and
// Amazon SQS on ONE engine, with the queue-and-rule setup and the drains written
// once rather than per suite.
use crate::job_state_change_events::{
    delivered, delivered_at_least, emulator, given_queue, given_rule, EVENT_SOURCE,
};

/// The error shape a plan body AWS does not accept names (`smithy_query`
/// operation `UpdateBackupPlan`: `InvalidParameterValueException`, `httpError`
/// 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The error shape a body that leaves a required parameter unsupplied names
/// (`smithy_query` operation `UpdateBackupPlan`:
/// `MissingParameterValueException`, `httpError` 400).
const MISSING_PARAMETER: &str = "MissingParameterValueException";

/// The error shape an id naming no live plan names (`smithy_query` operation
/// `UpdateBackupPlan`: `ResourceNotFoundException`, `httpError` 400).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence AWS states for "the backup plan this update must store the next
/// version of is not one this account holds", with that condition's own `Code`
/// (`crate::state`'s `MESSAGE_BACKUP_PLAN_UPDATE_NOT_FOUND` /
/// `ERROR_CODE_BACKUP_PLAN_UPDATE_NOT_FOUND`).
const PLAN_NOT_FOUND: &str =
    "Cannot update the Backup plan. Backup plan does not exist or has been deleted";
const PLAN_NOT_FOUND_CODE: &str = "ERROR_2204";

/// The sentence AWS states for "the plan body sets no rules", with its own
/// `Code` (`crate::state`'s `MESSAGE_BACKUP_PLAN_NO_RULES` /
/// `ERROR_CODE_BACKUP_PLAN_NO_RULES`).
const NO_RULES: &str = "No rules are set";
const NO_RULES_CODE: &str = "ERROR_4202";

/// A plan id of the form this service mints but no plan of a fresh account
/// carries.
const UNHELD_PLAN_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The least `StartWindowMinutes` accepts, as the model documents for
/// `BackupRuleInput`'s member ("If this value is included, it must be at least
/// 60 minutes to avoid errors").
const MIN_START_WINDOW_MINUTES: i64 = 60;

/// The most either backup window accepts, as the model documents for the same
/// member ("This parameter has a maximum value of 100 years (52,560,000
/// minutes)").
const MAX_WINDOW_MINUTES: i64 = 52_560_000;

/// The most rules one plan may hold, read from the service's own cap so the row
/// that probes it cannot drift from what the service enforces.
const MAX_RULES_PER_PLAN: usize =
    service_com_amazonaws_backup_cryocontrollerusermanager::state::MAX_RULES_PER_PLAN;

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "PUT", "/backup/plans", Some(&request.to_string())).await
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

/// The backup vaults this suite's rules target, created in the account before
/// any plan names them: a rule's `TargetBackupVaultName` names a vault of the
/// caller's account and Region, and an update resolves every one of them before
/// it stores the body it was given.
async fn given_target_vaults(router: &MultiServiceRouter) {
    for name in ["plan-vault", "other-vault"] {
        let (status, created) = create_vault(router, name, "{}").await;
        assert_eq!(
            status, 200,
            "setup: expected CreateBackupVault 200 for {name}, got {status} {created:?}"
        );
    }
}

/// A string member of a JSON object, or the empty string.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The body every row's plan is created with, so what an update does to a stored
/// plan is read against ONE known starting point.
fn created_plan(name: &str) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{
            "RuleName": "daily",
            "TargetBackupVaultName": "plan-vault",
            "ScheduleExpression": "cron(0 5 * * ? *)",
        }],
    })
}

/// A destination vault ARN in another region, the form a cross-region copy
/// action names (`CopyAction$DestinationBackupVaultArn`).
fn far_away_vault() -> String {
    format!("arn:aws:backup:us-east-1:{ACCOUNT}:backup-vault:far-away")
}

/// The bodies the supersession table updates a created plan to. The rows differ
/// in what the new version changes about the stored body — a rule's copy
/// actions, the number of rules, the plan's display name, the windows, the
/// advanced and scan settings — which is what "an update stores the body it was
/// given as another version" generalises over.
fn update_cases() -> Vec<(&'static str, Value)> {
    let air_gapped = format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:air-gapped");
    vec![
        (
            "a copy action added to the plan's one rule",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                    "CopyActions": [{
                        "DestinationBackupVaultArn": far_away_vault(),
                        "Lifecycle": {"DeleteAfterDays": 120},
                    }],
                }],
            }),
        ),
        (
            "the same body the plan was created with",
            created_plan("superseded-plan"),
        ),
        (
            "a renamed plan, which the body carries",
            json!({
                "BackupPlanName": "renamed-plan",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
            }),
        ),
        (
            "three rules where the plan had one",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [
                    {
                        "RuleName": "hourly",
                        "TargetBackupVaultName": "plan-vault",
                        "ScheduleExpression": "cron(0 * * * ? *)",
                        "RecoveryPointTags": {"tier": "hot"},
                    },
                    {
                        "RuleName": "daily",
                        "TargetBackupVaultName": "plan-vault",
                        "StartWindowMinutes": MIN_START_WINDOW_MINUTES,
                        "CompletionWindowMinutes": 0,
                    },
                    {
                        "RuleName": "centennial",
                        "TargetBackupVaultName": "other-vault",
                        "StartWindowMinutes": MAX_WINDOW_MINUTES,
                        "CompletionWindowMinutes": MAX_WINDOW_MINUTES,
                        "Lifecycle": {"DeleteAfterDays": 400},
                    },
                ],
            }),
        ),
        (
            "one rule where the plan's rule is replaced by another name",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "weekly",
                    "TargetBackupVaultName": "plan-vault",
                    "ScheduleExpression": "cron(0 5 ? * 1 *)",
                    "ScheduleExpressionTimezone": "Europe/Berlin",
                    "EnableContinuousBackup": true,
                    "TargetLogicallyAirGappedBackupVaultArn": air_gapped,
                }],
            }),
        ),
        (
            "advanced backup settings and scan settings the plan did not have",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
                "AdvancedBackupSettings": [{
                    "ResourceType": "EC2",
                    "BackupOptions": {"WindowsVSS": "enabled"},
                }],
                "ScanSettings": [{
                    "MalwareScanner": "GUARDDUTY",
                    "ResourceTypes": ["EBS"],
                    "ScannerRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/scanner"),
                }],
            }),
        ),
        (
            "the plan's rule carrying two copy actions",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "CopyActions": [
                        {"DestinationBackupVaultArn": far_away_vault()},
                        {
                            "DestinationBackupVaultArn":
                                format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:nearby"),
                            "Lifecycle": {"MoveToColdStorageAfterDays": 30,
                                          "DeleteAfterDays": 120},
                        },
                    ],
                }],
            }),
        ),
    ]
}

/// Assert that a read of `plan_id` at `version` reports `expected` as the stored
/// plan body, every rule carrying a `RuleId`.
async fn assert_reads_back(
    router: &MultiServiceRouter,
    case: &str,
    addressing: &str,
    plan_id: &str,
    version: Option<&str>,
    expected: &Value,
) {
    let (status, read) = get_plan(router, plan_id, version).await;
    assert_eq!(
        status, 200,
        "case {case:?} / {addressing}: expected GetBackupPlan 200, got {status} {read:?}"
    );
    assert_eq!(
        read["BackupPlan"].get("BackupPlanName"),
        expected.get("BackupPlanName"),
        "case {case:?} / {addressing}: expected the stored plan name, got {read:?}"
    );
    let sent_rules = expected["Rules"].as_array().cloned().unwrap_or_default();
    let read_rules = read["BackupPlan"]["Rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        read_rules.len(),
        sent_rules.len(),
        "case {case:?} / {addressing}: expected {} rule(s) read back, got {read_rules:?}",
        sent_rules.len()
    );
    for (sent, got) in sent_rules.iter().zip(read_rules.iter()) {
        for (member, value) in sent.as_object().expect("a rule is a JSON object") {
            assert_eq!(
                got.get(member),
                Some(value),
                "case {case:?} / {addressing}: rule member {member} read back differs, \
                 got {got:?}"
            );
        }
        assert!(
            !text(got, "RuleId").is_empty(),
            "case {case:?} / {addressing}: every stored rule carries a RuleId, got {got:?}"
        );
    }
    assert_eq!(
        read["BackupPlan"].get("ScanSettings"),
        expected.get("ScanSettings"),
        "case {case:?} / {addressing}: ScanSettings must report the stored list, got {read:?}"
    );
    assert_eq!(
        read.get("AdvancedBackupSettings"),
        expected.get("AdvancedBackupSettings"),
        "case {case:?} / {addressing}: AdvancedBackupSettings must report the stored list, \
         got {read:?}"
    );
}

/// An update stores the body it was given as another version of the same plan:
/// the plan id and ARN are the created ones, the version id is a new one, a read
/// naming no version reports the new body, and a read naming the superseded
/// version still reports the body that version held.
#[tokio::test]
async fn an_update_stores_another_version_and_supersedes_the_one_before_it() {
    for (case, updated_body) in update_cases() {
        // One router per row: each row's plan is the only plan the account
        // holds, so a row cannot see what another row stored.
        let router = router();
        given_target_vaults(&router).await;
        let created_body = created_plan("superseded-plan");
        let (status, created) = create_plan(&router, &json!({"BackupPlan": created_body})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        let created_version = text(&created, "VersionId");

        let (status, updated) = update_plan(&router, &plan_id, &updated_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {updated:?}"
        );
        for member in ["BackupPlanId", "BackupPlanArn"] {
            assert_eq!(
                updated.get(member),
                created.get(member),
                "case {case:?}: an update must report the created {member}, got {updated:?}"
            );
        }
        let updated_version = text(&updated, "VersionId");
        assert!(
            !updated_version.is_empty() && updated_version != created_version,
            "case {case:?}: expected a new VersionId, got {updated_version:?} against the \
             created {created_version:?}"
        );
        assert!(
            updated["CreationDate"].is_number(),
            "case {case:?}: expected the new version's CreationDate, got {updated:?}"
        );
        // Both list members a response reports only when the stored version
        // carries them.
        assert_eq!(
            updated.get("AdvancedBackupSettings"),
            updated_body.get("AdvancedBackupSettings"),
            "case {case:?}: AdvancedBackupSettings must report the stored list, got {updated:?}"
        );
        assert_eq!(
            updated.get("ScanSettings"),
            updated_body.get("ScanSettings"),
            "case {case:?}: ScanSettings must report the stored list, got {updated:?}"
        );

        // The read that names no version answers from the version the update
        // stored, twice over — a read changes nothing a later reader sees.
        for attempt in 0..2 {
            let addressing = format!("no version named, read {attempt}");
            assert_reads_back(
                &router,
                case,
                &addressing,
                &plan_id,
                None,
                &updated_body,
            )
            .await;
            let (status, read) = get_plan(&router, &plan_id, None).await;
            assert_eq!(
                (status, text(&read, "VersionId").as_str()),
                (200, updated_version.as_str()),
                "case {case:?} / {addressing}: the plan's current version is the update's, \
                 got {status} {read:?}"
            );
            assert_eq!(
                read.get("CreationDate"),
                updated.get("CreationDate"),
                "case {case:?} / {addressing}: the version's creation instant is the one the \
                 update answered, got {read:?}"
            );
        }

        // Naming the superseded version reports the body THAT version held: an
        // update supersedes a version, it does not rewrite it.
        assert_reads_back(
            &router,
            case,
            "the superseded version named",
            &plan_id,
            Some(&created_version),
            &created_plan("superseded-plan"),
        )
        .await;
        let (status, old) = get_plan(&router, &plan_id, Some(&created_version)).await;
        assert_eq!(
            (status, text(&old, "VersionId").as_str()),
            (200, created_version.as_str()),
            "case {case:?}: the superseded version reads back under its own id, got {status} \
             {old:?}"
        );
        assert_eq!(
            old.get("CreationDate"),
            created.get("CreationDate"),
            "case {case:?}: the superseded version keeps its own creation instant, got {old:?}"
        );
    }
}

/// A plan given SEVERAL updates holds a version per update: each answers a
/// distinct `VersionId`, every one of them still reads back under its own id
/// with the body it was given, and a read naming no version reports the latest.
#[tokio::test]
async fn every_update_of_a_plan_adds_a_version_that_stays_readable() {
    for updates in [1_usize, 2, 5] {
        let case = format!("{updates} update(s) of one plan");
        let router = router();
        given_target_vaults(&router).await;
        let (status, created) = create_plan(
            &router,
            &json!({"BackupPlan": created_plan("chained-plan")}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");

        // (the version id, the body stored under it), oldest first.
        let mut stored: Vec<(String, Value)> = vec![(
            text(&created, "VersionId"),
            created_plan("chained-plan"),
        )];
        for update in 0..updates {
            // Each update differs from the one before it in its rule's
            // retention, so a version read back is identifiable.
            let body = json!({
                "BackupPlanName": "chained-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "Lifecycle": {"DeleteAfterDays": 100 + update as i64},
                }],
            });
            let (status, answered) = update_plan(&router, &plan_id, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?} / update {update}: expected UpdateBackupPlan 200, got {status} \
                 {answered:?}"
            );
            stored.push((text(&answered, "VersionId"), body));
        }

        let mut version_ids: Vec<&String> = stored.iter().map(|(id, _)| id).collect();
        let total = version_ids.len();
        version_ids.sort();
        version_ids.dedup();
        assert_eq!(
            version_ids.len(),
            total,
            "case {case:?}: every version of a plan carries its own id, got {stored:?}"
        );

        for (version_id, body) in &stored {
            assert_reads_back(
                &router,
                &case,
                &format!("version {version_id}"),
                &plan_id,
                Some(version_id),
                body,
            )
            .await;
        }
        let (newest_id, newest_body) = stored.last().expect("a plan has at least one version");
        assert_reads_back(&router, &case, "no version named", &plan_id, None, newest_body).await;
        let (status, read) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            (status, text(&read, "VersionId").as_str()),
            (200, newest_id.as_str()),
            "case {case:?}: the latest update's version is the plan's current one, got {status} \
             {read:?}"
        );
    }
}

/// Rule-name rows for the body that carries one more rule than a plan may hold.
fn rules_past_the_cap() -> Value {
    let rules: Vec<Value> = (0..=MAX_RULES_PER_PLAN)
        .map(|index| {
            json!({
                "RuleName": format!("rule-{index}"),
                "TargetBackupVaultName": "plan-vault",
            })
        })
        .collect();
    json!({"BackupPlanName": "superseded-plan", "Rules": rules})
}

/// An update carrying a body AWS does not accept is refused with the error this
/// operation models for that condition, and the plan still reports the version
/// it had: a refused update stores nothing.
#[tokio::test]
async fn an_update_outside_what_aws_accepts_is_refused_and_stores_nothing() {
    // (case, the body the update sends, the error shape AWS names for it)
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "no rules at all",
            json!({"BackupPlanName": "superseded-plan", "Rules": []}),
            MISSING_PARAMETER,
        ),
        (
            "one rule name used twice",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [
                    {"RuleName": "daily", "TargetBackupVaultName": "plan-vault"},
                    {"RuleName": "daily", "TargetBackupVaultName": "other-vault"},
                ],
            }),
            INVALID_PARAMETER,
        ),
        (
            "a start window one minute under its documented floor",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "StartWindowMinutes": MIN_START_WINDOW_MINUTES - 1,
                }],
            }),
            INVALID_PARAMETER,
        ),
        (
            "a completion window one minute past its documented ceiling",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "CompletionWindowMinutes": MAX_WINDOW_MINUTES + 1,
                }],
            }),
            INVALID_PARAMETER,
        ),
        (
            "a copy action whose destination is not an ARN",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "CopyActions": [{"DestinationBackupVaultArn": "far-away"}],
                }],
            }),
            INVALID_PARAMETER,
        ),
        (
            "an air-gapped target that is not an ARN",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "TargetLogicallyAirGappedBackupVaultArn": "air-gapped",
                }],
            }),
            INVALID_PARAMETER,
        ),
        (
            "one rule more than a plan may hold",
            rules_past_the_cap(),
            INVALID_PARAMETER,
        ),
        (
            "a rule targeting a vault the account does not hold",
            json!({
                "BackupPlanName": "superseded-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "never-created-vault",
                }],
            }),
            INVALID_PARAMETER,
        ),
    ];

    for (case, body, expected_shape) in cases {
        let router = router();
        given_target_vaults(&router).await;
        let (status, created) = create_plan(
            &router,
            &json!({"BackupPlan": created_plan("superseded-plan")}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");

        let (status, refused) = update_plan(&router, &plan_id, &body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, expected_shape),
            "case {case:?}: expected 400 {expected_shape}, got {status} {refused:?}"
        );
        assert!(
            !text(&refused, "Message").is_empty(),
            "case {case:?}: the refusal states the condition, got {refused:?}"
        );

        // Nothing was stored: the plan's current version is still the create's,
        // body and id.
        assert_reads_back(
            &router,
            case,
            "after the refused update",
            &plan_id,
            None,
            &created_plan("superseded-plan"),
        )
        .await;
        let (status, read) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            (status, read.get("VersionId")),
            (200, created.get("VersionId")),
            "case {case:?}: a refused update must mint no version, got {status} {read:?}"
        );
    }
}

/// An update of a `BackupPlanId` the account holds no live plan under is refused
/// with the not-found refusal every plan-scoped call states, and the id it does
/// hold is updated.
#[tokio::test]
async fn an_update_of_a_plan_the_account_does_not_hold_is_resource_not_found() {
    let router = router();
    given_target_vaults(&router).await;
    let (status, created) = create_plan(
        &router,
        &json!({"BackupPlan": created_plan("held-for-update")}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBackupPlan 200, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");

    // A second plan, deleted, so one row addresses a plan whose deletion is
    // recorded rather than one that never existed.
    let (status, deletable) = create_plan(
        &router,
        &json!({"BackupPlan": created_plan("deleted-before-update")}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the second CreateBackupPlan 200, got {status} {deletable:?}"
    );
    let deleted_id = text(&deletable, "BackupPlanId");
    let (status, deleted) = delete_plan(&router, &deleted_id).await;
    assert_eq!(
        status, 200,
        "setup: expected DeleteBackupPlan 200, got {status} {deleted:?}"
    );

    let body = created_plan("updated-name");
    for (case, id) in [
        ("an id the account never held", UNHELD_PLAN_ID),
        ("an id one character short of a held one", &plan_id[1..]),
        ("a plan whose deletion is recorded", deleted_id.as_str()),
    ] {
        let (status, refused) = update_plan(&router, id, &body).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                text(&refused, "Message").as_str(),
                text(&refused, "Code").as_str(),
            ),
            (400, NOT_FOUND, PLAN_NOT_FOUND, PLAN_NOT_FOUND_CODE),
            "case {case:?}: expected 400 {NOT_FOUND} {PLAN_NOT_FOUND:?}, got {status} {refused:?}"
        );
    }

    // The control: the id the account does hold a live plan under is updated,
    // and the refusals above left that plan's version alone until then.
    let (status, updated) = update_plan(&router, &plan_id, &body).await;
    assert_eq!(
        (status, updated.get("BackupPlanArn")),
        (200, created.get("BackupPlanArn")),
        "the plan the account holds must be updated, got {status} {updated:?}"
    );
    assert_ne!(
        updated.get("VersionId"),
        created.get("VersionId"),
        "the accepted update mints a version of its own, got {updated:?}"
    );
}

/// The `detail-type` AWS Backup publishes a plan's mutations under, and the two
/// transitions `detail.state` reports, spelled out here rather than imported from
/// the service crate so a wrong constant in the implementation cannot make these
/// cases pass.
const BACKUP_PLAN_STATE_CHANGE: &str = "Backup Plan State Change";
const MODIFIED: &str = "MODIFIED";
const DELETED: &str = "DELETED";

/// The rule the event cases install, with the pattern a client writes for these
/// envelopes.
const PLAN_EVENT_RULE: &str = "plan-state-changes";

/// Assert that `envelope` is the plan state change `state` of the plan `answered`
/// identifies: the identifiers and the version the MUTATION's own response
/// reported, so the event is the stored plan rather than a second, invented copy
/// of it.
fn assert_plan_envelope(case: &str, envelope: &Value, answered: &Value, name: &str, state: &str) {
    assert_eq!(
        (
            envelope["source"].clone(),
            envelope["detail-type"].clone(),
            envelope["resources"].clone(),
        ),
        (
            json!(EVENT_SOURCE),
            json!(BACKUP_PLAN_STATE_CHANGE),
            json!([text(answered, "BackupPlanArn")]),
        ),
        "case {case:?}: expected the {state} envelope of this plan, got {envelope}"
    );
    assert_eq!(
        (
            envelope["detail"]["backupPlanId"].clone(),
            envelope["detail"]["versionId"].clone(),
            envelope["detail"]["backupPlanArn"].clone(),
            envelope["detail"]["backupPlanName"].clone(),
            envelope["detail"]["state"].clone(),
        ),
        (
            answered["BackupPlanId"].clone(),
            answered["VersionId"].clone(),
            answered["BackupPlanArn"].clone(),
            json!(name),
            json!(state),
        ),
        "case {case:?}: expected the plan, the version and the state the mutation \
         reported, got {envelope}"
    );
}

/// Every mutation a stored plan is given after its create is published on the
/// account's default bus, and the create itself publishes nothing.
///
/// The rows are the supersession table's: they differ in what the new version
/// changes about the stored body, which is what "a mutation of the plan is
/// announced" generalises over, and each row then deletes the plan, so the
/// deletion's own envelope is read for every shape of plan too.
#[tokio::test]
async fn a_plan_mutation_is_published_as_a_backup_plan_state_change() {
    for (case, body) in update_cases() {
        // Backup, EventBridge and Amazon SQS on one engine: the publication
        // reaches EventBridge in process, and the rule delivers to this queue.
        let router = emulator();
        given_target_vaults(&router).await;
        let (queue, queue_arn) = given_queue(&router, case, "plan-state-change-events").await;
        given_rule(
            &router,
            case,
            PLAN_EVENT_RULE,
            &json!({
                "source": [EVENT_SOURCE],
                "detail-type": [BACKUP_PLAN_STATE_CHANGE],
            }),
            &[queue_arn],
        )
        .await;

        let (status, created) =
            create_plan(&router, &json!({"BackupPlan": created_plan("event-plan")})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        // The plan a create stored is what the create's own response reports, so
        // the bus carries nothing for it. The publication a mutation owes is made
        // by the mutating call itself, so a drain taken once that call has
        // answered is the whole of what it published.
        let after_create = delivered(&router, case, &queue).await;
        assert!(
            after_create.is_empty(),
            "case {case:?}: a create publishes no plan state change, got {after_create:?}"
        );

        // The update: ONE envelope, naming the plan and the version this call
        // answered with.
        let (status, updated) = update_plan(&router, &plan_id, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {updated:?}"
        );
        let updated_name = text(&body, "BackupPlanName");
        let modified = delivered_at_least(&router, case, &queue, 1).await;
        assert_eq!(
            modified.len(),
            1,
            "case {case:?}: the update owes exactly one envelope, got {modified:?}"
        );
        assert_plan_envelope(case, &modified[0], &updated, &updated_name, MODIFIED);

        // And the delete: one more, carrying the plan's recorded deletion.
        let (status, deleted) = delete_plan(&router, &plan_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteBackupPlan 200, got {status} {deleted:?}"
        );
        let removed = delivered_at_least(&router, case, &queue, 1).await;
        assert_eq!(
            removed.len(),
            1,
            "case {case:?}: the delete owes exactly one envelope, got {removed:?}"
        );
        assert_plan_envelope(case, &removed[0], &deleted, &updated_name, DELETED);
        assert!(
            removed[0]["detail"]["deletionDate"].is_string(),
            "case {case:?}: the deletion envelope reports the stored deletion \
             instant, got {}",
            removed[0]
        );
    }
}

/// An update this service REFUSES publishes nothing: there is no stored
/// transition to announce, so the bus carries only the envelope of the update it
/// ACCEPTED — read on a path the accepted update proves live.
#[tokio::test]
async fn a_refused_update_publishes_no_plan_state_change() {
    let case = "a refused update beside an accepted one";
    let router = emulator();
    given_target_vaults(&router).await;
    let (queue, queue_arn) = given_queue(&router, case, "refused-plan-events").await;
    // Owed EVERY envelope of this source: a rule filtering any of them out could
    // not tell "withheld" from "filtered".
    given_rule(
        &router,
        case,
        PLAN_EVENT_RULE,
        &json!({"source": [EVENT_SOURCE]}),
        &[queue_arn],
    )
    .await;

    let (status, created) =
        create_plan(&router, &json!({"BackupPlan": created_plan("refused-plan")})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");

    // An id the account holds no plan under, and a body AWS does not accept on
    // the plan it DOES hold: each is refused, and neither stores a version.
    let (status, refused) =
        update_plan(&router, UNHELD_PLAN_ID, &created_plan("never-stored")).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, NOT_FOUND),
        "case {case:?}: expected 400 {NOT_FOUND}, got {status} {refused:?}"
    );
    let (status, refused) = update_plan(
        &router,
        &plan_id,
        &json!({"BackupPlanName": "no-rules-at-all", "Rules": []}),
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&refused).as_str(),
            text(&refused, "Message").as_str(),
            text(&refused, "Code").as_str(),
        ),
        (400, MISSING_PARAMETER, NO_RULES, NO_RULES_CODE),
        "case {case:?}: expected 400 {MISSING_PARAMETER} {NO_RULES:?}, got {status} {refused:?}"
    );

    // The accepted update that follows is the first thing the queue is handed,
    // which is only true if the refusals published nothing.
    let accepted = created_plan("accepted-after-refusals");
    let (status, updated) = update_plan(&router, &plan_id, &accepted).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the accepted UpdateBackupPlan 200, got {status} {updated:?}"
    );
    let seen = delivered_at_least(&router, case, &queue, 1).await;
    assert_eq!(
        seen.len(),
        1,
        "case {case:?}: the accepted update's envelope is the ONE the bus carries, \
         got {seen:?}"
    );
    assert_plan_envelope(
        case,
        &seen[0],
        &updated,
        &text(&accepted, "BackupPlanName"),
        MODIFIED,
    );
}
