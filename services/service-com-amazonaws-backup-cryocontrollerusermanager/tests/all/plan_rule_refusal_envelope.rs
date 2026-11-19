// SPDX-License-Identifier: CC0-1.0

//! WHERE a refused member SAT, as AWS Backup words the refusal, driven as real
//! HTTP through the router.
//!
//! The rule under test: a condition met by a member INSIDE one of a backup plan
//! body's rules fails that rule's validation, so the sentence AWS answers is the
//! condition's own reason after the fixed rule-error envelope
//! (`crate::state`'s `MESSAGE_BACKUP_PLAN_RULES_PREFIX`), which is how a client
//! attributes the refusal to `BackupPlan$Rules` rather than to a top-level plan
//! parameter; the same condition met by a member an operation takes on its own
//! is stated as the reason alone. The envelope is fixed text: nothing from the
//! request enters it, so neither the plan's name nor the rule's appears, and the
//! condition's own token and `Context` are the same on both sides of the
//! distinction.
//!
//! The rows differ along what the rule generalises over: which condition the
//! rule fails (the cold-storage interval of a lifecycle, a schedule expression
//! that is not a cron expression), where inside the rule the member sits (the
//! rule itself, one of its copy actions), which operation stores the body
//! (`CreateBackupPlan`, `UpdateBackupPlan`), and the day pair or expression the
//! member carries. The operation-level side is driven through the two surfaces
//! that take a `Lifecycle` of their own, `StartBackupJob` and `StartCopyJob`.
//!
//! `lifecycle_cold_storage_interval.rs` owns the interval rule itself as a table
//! of day pairs; this suite owns the wording the two positions earn, so neither
//! file states the other's rule.
//!
//! The request harness (`call`, `call_reported`, `router`, `ACCOUNT`, `REGION`)
//! is the one `vault_and_backup_job.rs` exposes to this test binary.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, call_reported, router, ACCOUNT, REGION};

/// The envelope AWS states a per-rule validation failure after
/// (`crate::state`'s `MESSAGE_BACKUP_PLAN_RULES_PREFIX`).
const RULE_ENVELOPE: &str = "Error in some rules due to : ";

/// The reason the cold-storage interval condition states
/// (`MESSAGE_LIFECYCLE_COLD_STORAGE_INTERVAL`), and the token and `Context` it
/// carries wherever it is met (`ERROR_CODE_LIFECYCLE_COLD_STORAGE_INTERVAL`,
/// `MIN_COLD_STORAGE_DAYS`).
const INTERVAL_REASON: &str = "Invalid lifecycle. DeleteAfterDays cannot be less than 90 days \
                               apart from MoveToColdStorageAfterDays";
const INTERVAL_CODE: &str = "ERROR_3011";
const INTERVAL_CONTEXT: &str = "90";

/// The reason an expression that is not a cron expression states
/// (`MESSAGE_CRON_INVALID_PREFIX` + `MESSAGE_CRON_INVALID`).
const CRON_REASON: &str = "Cron expression is not valid: Invalid cron expression";

/// The vault every row's rule, copy destination and job names.
const VAULT: &str = "envelope-vault";

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The ARN of the vault a copy action's destination names.
fn vault_arn() -> String {
    format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:{VAULT}")
}

/// An IAM role ARN a backup job may be started with.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// Create the vault every row names, so each row is refused on real state
/// rather than for a vault that is not there.
async fn vault(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "PUT",
        &format!("/backup-vaults/{VAULT}"),
        Some("{}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault {VAULT} expected 200, got {status} {created:?}"
    );
}

/// Where inside a plan's rule the refused member sits.
#[derive(Clone, Copy, Debug)]
enum Position {
    /// On the rule itself.
    Rule,
    /// On one of the rule's copy actions.
    CopyAction,
}

/// One row: the rule body a plan carries, and the reason its condition states.
struct Row {
    case: &'static str,
    /// The `Lifecycle` the row states, when the condition is the interval one.
    lifecycle: Option<Value>,
    /// The `ScheduleExpression` the row states, when it states one.
    schedule: Option<&'static str>,
    position: Position,
    reason: &'static str,
    code: &'static str,
    context: &'static str,
}

/// The rows the rule generalises over: both conditions, both positions, several
/// day pairs and two expressions nothing resolves to a schedule.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a lifecycle on the rule, one day short of the interval",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 119})),
            schedule: None,
            position: Position::Rule,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
        },
        Row {
            case: "a lifecycle on the rule, a deletion before the transition",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 60, "DeleteAfterDays": 30})),
            schedule: None,
            position: Position::Rule,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
        },
        Row {
            case: "a lifecycle on a copy action, both on the same day",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 45, "DeleteAfterDays": 45})),
            schedule: None,
            position: Position::CopyAction,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
        },
        Row {
            case: "a lifecycle on a copy action, a year-long pair a day short",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 365, "DeleteAfterDays": 454})),
            schedule: None,
            position: Position::CopyAction,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
        },
        Row {
            case: "a schedule expression that is not an expression at all",
            lifecycle: None,
            schedule: Some("every midnight"),
            position: Position::Rule,
            reason: CRON_REASON,
            code: "ERROR_3015",
            context: CRON_REASON,
        },
        Row {
            case: "a cron expression with too few fields",
            lifecycle: None,
            schedule: Some("cron(0 0)"),
            position: Position::Rule,
            reason: CRON_REASON,
            code: "ERROR_3015",
            context: CRON_REASON,
        },
    ]
}

impl Row {
    /// The plan body this row submits under `name`.
    fn plan_body(&self, name: &str) -> Value {
        let mut rule = json!({"RuleName": "daily", "TargetBackupVaultName": VAULT});
        if let Some(schedule) = self.schedule {
            rule["ScheduleExpression"] = json!(schedule);
        }
        if let Some(lifecycle) = self.lifecycle.clone() {
            match self.position {
                Position::Rule => rule["Lifecycle"] = lifecycle,
                Position::CopyAction => {
                    rule["CopyActions"] = json!([
                        {"DestinationBackupVaultArn": vault_arn(), "Lifecycle": lifecycle}
                    ]);
                }
            }
        }
        json!({"BackupPlan": {"BackupPlanName": name, "Rules": [rule]}})
    }

    /// The sentence the row's condition earns inside a plan rule.
    fn enveloped(&self) -> String {
        format!("{RULE_ENVELOPE}{}", self.reason)
    }
}

/// Assert `answered` is the row's refusal, worded with the envelope.
fn assert_enveloped(case: &str, status: u16, answered: &Value, row: &Row) {
    assert_eq!(
        (
            status,
            member(answered, "Code"),
            member(answered, "Message"),
            member(answered, "Context"),
        ),
        (
            400,
            row.code.to_string(),
            row.enveloped(),
            row.context.to_string(),
        ),
        "case {case}: expected the per-rule refusal {:?}, got {status} {answered:?}",
        row.enveloped()
    );
}

/// RULE: every operation that stores a plan body states a condition one of the
/// body's rules fails after the rule-error envelope, and stores nothing.
#[tokio::test]
async fn a_condition_a_plan_rule_fails_is_stated_after_the_rule_error_envelope() {
    for row in rows() {
        // CreateBackupPlan — the body is the one being stored.
        let router = router();
        vault(&router).await;
        let (status, answered) = call(
            &router,
            "PUT",
            "/backup/plans",
            Some(&row.plan_body("envelope-plan").to_string()),
        )
        .await;
        assert_enveloped(&format!("CreateBackupPlan/{}", row.case), status, &answered, &row);

        // Nothing was stored: the refused plan name is listed nowhere.
        let (status, listed) = call(&router, "GET", "/backup/plans", None).await;
        let names: Vec<String> = listed["BackupPlansList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|plan| member(plan, "BackupPlanName"))
            .collect();
        assert_eq!(
            (status, names),
            (200, Vec::new()),
            "case CreateBackupPlan/{}: a refused create must store no plan, got {status} \
             {listed:?}",
            row.case
        );

        // UpdateBackupPlan — the SAME body, stored as the next version of a
        // plan whose first version carries nothing the rule refuses.
        let accepted = json!({"BackupPlan": {
            "BackupPlanName": "envelope-plan",
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": VAULT}],
        }});
        let (status, created) = call(
            &router,
            "PUT",
            "/backup/plans",
            Some(&accepted.to_string()),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: the first version must be created, got {status} {created:?}"
        );
        let plan_id = member(&created, "BackupPlanId");
        let first_version = member(&created, "VersionId");
        let (status, answered) = call(
            &router,
            "POST",
            &format!("/backup/plans/{plan_id}"),
            Some(&row.plan_body("envelope-plan").to_string()),
        )
        .await;
        assert_enveloped(&format!("UpdateBackupPlan/{}", row.case), status, &answered, &row);
        let (status, read) = call(&router, "GET", &format!("/backup/plans/{plan_id}"), None).await;
        assert_eq!(
            (status, member(&read, "VersionId")),
            (200, first_version),
            "case UpdateBackupPlan/{}: a refused update must store no version, got {status} \
             {read:?}",
            row.case
        );
    }
}

/// RULE, the other side: a `Lifecycle` an operation takes on its OWN is refused
/// for the same condition — one token, one `Context`, the same reason — stated
/// without the envelope, because no plan rule carried it.
#[tokio::test]
async fn a_lifecycle_an_operation_takes_itself_is_stated_without_the_envelope() {
    // (the surface, its method and path, the body the row sends)
    let lifecycles = [
        json!({"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 119}),
        json!({"MoveToColdStorageAfterDays": 60, "DeleteAfterDays": 30}),
        json!({"MoveToColdStorageAfterDays": 365, "DeleteAfterDays": 454}),
    ];
    for lifecycle in lifecycles {
        let router = router();
        vault(&router).await;
        let surfaces: Vec<(&str, &str, Value)> = vec![
            (
                "StartBackupJob",
                "/backup-jobs",
                json!({
                    "BackupVaultName": VAULT,
                    "ResourceArn": format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo"),
                    "IamRoleArn": role_arn(),
                    "Lifecycle": lifecycle.clone(),
                }),
            ),
            (
                "StartCopyJob",
                "/copy-jobs",
                json!({
                    "SourceBackupVaultName": VAULT,
                    "DestinationBackupVaultArn": vault_arn(),
                    "RecoveryPointArn": format!(
                        "arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo"
                    ),
                    "IamRoleArn": role_arn(),
                    "Lifecycle": lifecycle.clone(),
                }),
            ),
        ];
        for (surface, path, body) in surfaces {
            let (status, errortype, answered) =
                call_reported(&router, "PUT", path, Some(&body.to_string())).await;
            assert_eq!(
                (
                    status,
                    errortype,
                    member(&answered, "Code"),
                    member(&answered, "Message"),
                    member(&answered, "Context"),
                ),
                (
                    400,
                    "InvalidParameterValueException".to_string(),
                    INTERVAL_CODE.to_string(),
                    INTERVAL_REASON.to_string(),
                    INTERVAL_CONTEXT.to_string(),
                ),
                "case {surface}/{lifecycle}: expected the bare interval refusal, got {status} \
                 {answered:?}"
            );
        }
    }
}
