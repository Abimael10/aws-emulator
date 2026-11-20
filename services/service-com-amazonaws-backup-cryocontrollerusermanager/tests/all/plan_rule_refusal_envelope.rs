// SPDX-License-Identifier: CC0-1.0

//! WHICH refusals of a backup plan body carry AWS Backup's rule-error envelope,
//! driven as real HTTP through the router.
//!
//! The rule under test: the envelope (`crate::state`'s
//! `MESSAGE_BACKUP_PLAN_RULES_PREFIX`) belongs to AWS Backup's PER-RULE
//! VALIDATION class — the checks it runs over a rule it is about to schedule
//! jobs from, whose reasons it reports together under that class's one token — so
//! a reason of that class is stated after the envelope wherever a plan body is
//! stored, while a condition with a TOKEN OF ITS OWN is stated by its own
//! sentence alone, whichever member of the body carried it and whichever
//! operation stored it. The envelope is fixed text: nothing from the request
//! enters it, so neither the plan's name nor the rule's appears.
//!
//! The rows differ along what the rule generalises over: which condition the
//! body fails (the cold-storage interval of a lifecycle, which carries its own
//! token, and a schedule expression that is not a cron expression, which is one
//! of the per-rule validations), where inside the rule the member sits (the rule
//! itself, one of its copy actions), which operation stores the body
//! (`CreateBackupPlan`, `UpdateBackupPlan`), and the day pair or expression the
//! member carries. The operation-level side is driven through the two surfaces
//! that take a `Lifecycle` of their own, `StartBackupJob` and `StartCopyJob`,
//! which answer the same sentence as the plan body's lifecycle.
//!
//! `lifecycle_cold_storage_interval.rs` owns the interval rule itself as a table
//! of day pairs; this suite owns which refusals the envelope wraps, so neither
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
    /// Whether the row's condition is one of the per-rule validations, whose
    /// reason the envelope wraps.
    enveloped: bool,
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
            enveloped: false,
        },
        Row {
            case: "a lifecycle on the rule, a deletion before the transition",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 60, "DeleteAfterDays": 30})),
            schedule: None,
            position: Position::Rule,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: false,
        },
        Row {
            case: "a lifecycle on a copy action, both on the same day",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 45, "DeleteAfterDays": 45})),
            schedule: None,
            position: Position::CopyAction,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: false,
        },
        Row {
            case: "a lifecycle on a copy action, a year-long pair a day short",
            lifecycle: Some(json!({"MoveToColdStorageAfterDays": 365, "DeleteAfterDays": 454})),
            schedule: None,
            position: Position::CopyAction,
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: false,
        },
        Row {
            case: "a schedule expression that is not an expression at all",
            lifecycle: None,
            schedule: Some("every midnight"),
            position: Position::Rule,
            reason: CRON_REASON,
            code: "ERROR_3015",
            context: CRON_REASON,
            enveloped: true,
        },
        Row {
            case: "a cron expression with too few fields",
            lifecycle: None,
            schedule: Some("cron(0 0)"),
            position: Position::Rule,
            reason: CRON_REASON,
            code: "ERROR_3015",
            context: CRON_REASON,
            enveloped: true,
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

    /// The sentence the row's condition earns from a plan body: the per-rule
    /// validations after the envelope, every other condition on its own.
    fn sentence(&self) -> String {
        if self.enveloped {
            format!("{RULE_ENVELOPE}{}", self.reason)
        } else {
            self.reason.to_string()
        }
    }
}

/// Assert `answered` is the row's refusal, worded as its class is worded.
fn assert_worded(case: &str, status: u16, answered: &Value, row: &Row) {
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
            row.sentence(),
            row.context.to_string(),
        ),
        "case {case}: expected the refusal {:?}, got {status} {answered:?}",
        row.sentence()
    );
}

/// RULE: every operation that stores a plan body states a per-rule validation
/// reason after the rule-error envelope and every other condition's reason on
/// its own, and stores nothing either way.
#[tokio::test]
async fn a_refused_plan_body_is_worded_by_the_class_of_the_condition_it_failed() {
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
        assert_worded(&format!("CreateBackupPlan/{}", row.case), status, &answered, &row);

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
        assert_worded(&format!("UpdateBackupPlan/{}", row.case), status, &answered, &row);
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
/// for the same condition — one token, one `Context`, the same reason — which is
/// the sentence a plan body's lifecycle earns too, because the condition carries
/// a token of its own rather than being one of the per-rule validations.
#[tokio::test]
async fn a_lifecycle_an_operation_takes_itself_states_the_same_reason() {
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
