// SPDX-License-Identifier: CC0-1.0

//! WHICH refusals of a backup plan body carry AWS Backup's rule-error envelope,
//! driven as real HTTP through the router.
//!
//! The rule under test: AWS Backup validates each RULE of a submitted plan body
//! before it stores any of it, and states the reason a rule failed after that
//! pass's fixed envelope (`crate::state`'s `MESSAGE_BACKUP_PLAN_RULES_PREFIX`) —
//! the envelope is how the answer marks the body as refused for one of its RULES
//! rather than for a top-level plan parameter — keeping the condition's own
//! `Code` and `Context` where the condition has them. A refusal the per-rule
//! pass does not produce keeps its sentence alone: what a NESTED copy action is
//! judged by (its own `Lifecycle`, its destination vault ARN) and the
//! `Lifecycle` an operation takes itself. The envelope is fixed text: nothing
//! from the request enters it, so neither the plan's name nor the rule's appears
//! in it.
//!
//! The rows differ along what the rule generalises over: WHICH condition the
//! body fails (a lifecycle that leaves too little time in cold storage, a
//! schedule expression that is not a cron expression, a backup window outside
//! its documented span, two rules sharing one name, a member that names a vault
//! by an ARN that is not one), whether that condition has a token of its own,
//! WHERE inside the body the refused member sits (the rule itself, one of its
//! copy actions), and which operation stores the body (`CreateBackupPlan`,
//! `UpdateBackupPlan`). The operation-level side is driven through the two
//! surfaces that take a `Lifecycle` of their own, `StartBackupJob` and
//! `StartCopyJob`.
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
/// (`MESSAGE_CRON_INVALID_PREFIX` + `MESSAGE_CRON_INVALID`), and the token of
/// the class whose reasons carry no token of their own
/// (`ERROR_CODE_BACKUP_PLAN_RULE_INVALID`), which also repeats the reason in
/// `Context`.
const CRON_REASON: &str = "Cron expression is not valid: Invalid cron expression";
const RULE_INVALID_CODE: &str = "ERROR_3015";

/// The reason a member that names a backup vault by an ARN that is not one
/// states, and that condition's own token (`MESSAGE_BACKUP_VAULT_ARN_INVALID`,
/// `ERROR_CODE_BACKUP_VAULT_ARN_INVALID`); it carries no `Context`.
const VAULT_ARN_REASON: &str = "Invalid backup vault ARN";
const VAULT_ARN_CODE: &str = "ERROR_2307";

/// A value no backup vault is named by: the ARN of a resource of another
/// service.
const NOT_A_VAULT_ARN: &str = "arn:aws:s3:::not-a-backup-vault";

/// The reason a backup window outside the span the model documents for it states
/// (`crate::state`'s `rejected_window_minutes`, over `MIN_START_WINDOW_MINUTES`
/// and `MAX_WINDOW_MINUTES`).
const START_WINDOW_REASON: &str = "StartWindowMinutes must be between 60 and 52560000 minutes";
const COMPLETION_WINDOW_REASON: &str =
    "CompletionWindowMinutes must be between 0 and 52560000 minutes";

/// The reason two rules of one plan sharing one name states; the name is the
/// rule's own, so it is the one this suite sends.
const DUPLICATE_RULE_NAME: &str = "nightly";
const DUPLICATE_REASON: &str = "RuleName nightly is used by more than one rule; rule names must \
                                be unique within a backup plan";

/// What a response reports about itself when a member of the body is absent.
const UNSET: &str = "<unset>";

/// The vault every row's rule, copy destination and job names.
const VAULT: &str = "envelope-vault";

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or(UNSET)
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

/// A rule naming the suite's vault, with `members` merged onto it.
fn rule(members: Value) -> Value {
    let mut rule = json!({"RuleName": "daily", "TargetBackupVaultName": VAULT});
    for (name, value) in members.as_object().cloned().unwrap_or_default() {
        rule[name] = value;
    }
    rule
}

/// One row: the `Rules` list a plan body carries, and what the condition it
/// fails reports.
struct Row {
    case: &'static str,
    /// The `Rules` the row submits.
    rules: fn() -> Value,
    /// The reason the row's condition states.
    reason: &'static str,
    /// The condition's own token, `"<unset>"` where it has none.
    code: &'static str,
    /// The condition's own `Context`, `"<unset>"` where it carries none.
    context: &'static str,
    /// Whether the refusal is one the per-rule validation pass produces, whose
    /// reason the envelope marks as coming from a rule.
    enveloped: bool,
}

/// The rows the rule generalises over: five conditions, with and without a
/// token of their own, in a rule's own members and in a nested copy action's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a lifecycle on the rule, one day short of the interval",
            rules: || {
                json!([rule(
                    json!({"Lifecycle": {"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 119}})
                )])
            },
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: true,
        },
        Row {
            case: "a lifecycle on the rule, a deletion before the transition",
            rules: || {
                json!([rule(
                    json!({"Lifecycle": {"MoveToColdStorageAfterDays": 60, "DeleteAfterDays": 30}})
                )])
            },
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: true,
        },
        Row {
            case: "a lifecycle on a copy action, both on the same day",
            rules: || {
                json!([rule(json!({"CopyActions": [{
                    "DestinationBackupVaultArn": vault_arn(),
                    "Lifecycle": {"MoveToColdStorageAfterDays": 45, "DeleteAfterDays": 45},
                }]}))])
            },
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: false,
        },
        Row {
            case: "a lifecycle on a copy action, a year-long pair a day short",
            rules: || {
                json!([rule(json!({"CopyActions": [{
                    "DestinationBackupVaultArn": vault_arn(),
                    "Lifecycle": {"MoveToColdStorageAfterDays": 365, "DeleteAfterDays": 454},
                }]}))])
            },
            reason: INTERVAL_REASON,
            code: INTERVAL_CODE,
            context: INTERVAL_CONTEXT,
            enveloped: false,
        },
        Row {
            case: "a schedule expression that is not an expression at all",
            rules: || json!([rule(json!({"ScheduleExpression": "every midnight"}))]),
            reason: CRON_REASON,
            code: RULE_INVALID_CODE,
            context: CRON_REASON,
            enveloped: true,
        },
        Row {
            case: "a cron expression with too few fields",
            rules: || json!([rule(json!({"ScheduleExpression": "cron(0 0)"}))]),
            reason: CRON_REASON,
            code: RULE_INVALID_CODE,
            context: CRON_REASON,
            enveloped: true,
        },
        Row {
            case: "a start window below the documented floor",
            rules: || json!([rule(json!({"StartWindowMinutes": 59}))]),
            reason: START_WINDOW_REASON,
            code: UNSET,
            context: UNSET,
            enveloped: true,
        },
        Row {
            case: "a completion window past the documented ceiling",
            rules: || json!([rule(json!({"CompletionWindowMinutes": 52_560_001i64}))]),
            reason: COMPLETION_WINDOW_REASON,
            code: UNSET,
            context: UNSET,
            enveloped: true,
        },
        Row {
            case: "two rules sharing one name",
            rules: || {
                json!([
                    rule(json!({"RuleName": DUPLICATE_RULE_NAME})),
                    rule(json!({"RuleName": DUPLICATE_RULE_NAME, "ScheduleExpression": "cron(0 5 ? * * *)"})),
                ])
            },
            reason: DUPLICATE_REASON,
            code: UNSET,
            context: UNSET,
            enveloped: true,
        },
        Row {
            case: "a logically air-gapped vault named by an ARN that is not a vault's",
            rules: || {
                json!([rule(
                    json!({"TargetLogicallyAirGappedBackupVaultArn": NOT_A_VAULT_ARN})
                )])
            },
            reason: VAULT_ARN_REASON,
            code: VAULT_ARN_CODE,
            context: UNSET,
            enveloped: true,
        },
        Row {
            case: "a copy action whose destination is an ARN that is not a vault's",
            rules: || {
                json!([rule(
                    json!({"CopyActions": [{"DestinationBackupVaultArn": NOT_A_VAULT_ARN}]})
                )])
            },
            reason: VAULT_ARN_REASON,
            code: VAULT_ARN_CODE,
            context: UNSET,
            enveloped: false,
        },
    ]
}

impl Row {
    /// The plan body this row submits under `name`.
    fn plan_body(&self, name: &str) -> Value {
        json!({"BackupPlan": {"BackupPlanName": name, "Rules": (self.rules)()}})
    }

    /// The sentence the row's condition earns from a plan body: a reason the
    /// per-rule pass produces after the envelope, every other reason on its own.
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

/// RULE: every operation that stores a plan body states a reason the per-rule
/// validation pass produces after the rule-error envelope and every other
/// condition's reason on its own, and stores nothing either way.
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
        assert_worded(
            &format!("CreateBackupPlan/{}", row.case),
            status,
            &answered,
            &row,
        );

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
        assert_worded(
            &format!("UpdateBackupPlan/{}", row.case),
            status,
            &answered,
            &row,
        );
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
/// without the envelope, because the refusal does not come from the per-rule
/// validation pass over a plan body's rules.
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
