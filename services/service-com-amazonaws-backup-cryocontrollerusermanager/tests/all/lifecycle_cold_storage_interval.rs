// SPDX-License-Identifier: CC0-1.0

//! The interval a `Lifecycle` has to leave between the transition to cold
//! storage and the deletion that follows it, driven as real HTTP through the
//! router.
//!
//! The rule under test: a `Lifecycle` whose `DeleteAfterDays` falls less than 90
//! days after its `MoveToColdStorageAfterDays` is refused with
//! `InvalidParameterValueException` and nothing is stored, because
//! `DeleteAfterDays` "must be at least 90 days after the number of days
//! specified in MoveToColdStorageAfterDays" — "backups transitioned to cold
//! storage must be stored in cold storage for a minimum of 90 days"
//! (`smithy_query(query="shape",shape="Lifecycle")`). A pair that states only one
//! of the two members, or the indefinite `-1` either member carries to "remove
//! the existing lifecycle and retention periods and keep your recovery points
//! indefinitely" (the same doc), measures no such interval and is served.
//!
//! The rule is asserted over a TABLE of day pairs — the ways a pair can and
//! cannot leave that interval, including the published bound itself and a day
//! either side of it — crossed with every surface of this service that takes a
//! `Lifecycle`: a plan rule's own, the lifecycle of one of that rule's copy
//! actions, the same rule stored by `UpdateBackupPlan`, and the lifecycle a
//! `StartBackupJob` keeps its recovery point under. The condition is ONE, so
//! every surface answers it the same way — one token, one `Context` and one
//! sentence, the condition's own — whether the member sat inside one of a plan's
//! rules or was taken by the operation itself.
//!
//! The request harness (`router`, `call`, `call_reported`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, call_reported, router, ACCOUNT, REGION};

/// The token AWS states for a lifecycle that leaves too little time in cold
/// storage (`crate::state`'s `ERROR_CODE_LIFECYCLE_COLD_STORAGE_INTERVAL`).
const INTERVAL_CODE: &str = "ERROR_3011";

/// The interval the service enforces, which the refusal carries in `Context`
/// (`MIN_COLD_STORAGE_DAYS`, the 90 days the model documents).
const INTERVAL_CONTEXT: &str = "90";

/// The sentence AWS states for that condition
/// (`MESSAGE_LIFECYCLE_COLD_STORAGE_INTERVAL`).
const INTERVAL_REASON: &str = "Invalid lifecycle. DeleteAfterDays cannot be less than 90 days \
                               apart from MoveToColdStorageAfterDays";

/// The vault every row's job and copy destination names.
const VAULT: &str = "lifecycle-vault";

/// One row of the table: the day pair a `Lifecycle` states and whether a request
/// carrying it is served.
struct Row {
    case: &'static str,
    cold: Option<i64>,
    delete: Option<i64>,
    served: bool,
}

/// The day pairs the rule generalises over.
///
/// SOURCE of the bound and of the indefinite value: the model's own
/// documentation for the shape being stored —
/// `smithy_query(query="shape",shape="Lifecycle")` — which states the 90-day
/// minimum on `DeleteAfterDays` and `-1` as the value that keeps recovery points
/// indefinitely. The rows walk the bound itself, one day either side of it, a
/// pair far from the recorded one, the orderings a pair can take and each member
/// stated alone.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no lifecycle at all",
            cold: None,
            delete: None,
            served: true,
        },
        Row {
            case: "a deletion with no transition to cold storage",
            cold: None,
            delete: Some(60),
            served: true,
        },
        Row {
            case: "a transition with no deletion",
            cold: Some(30),
            delete: None,
            served: true,
        },
        Row {
            case: "exactly the documented minimum apart",
            cold: Some(30),
            delete: Some(120),
            served: true,
        },
        Row {
            case: "one day short of the documented minimum",
            cold: Some(30),
            delete: Some(119),
            served: false,
        },
        Row {
            case: "one day past the documented minimum",
            cold: Some(30),
            delete: Some(121),
            served: true,
        },
        Row {
            case: "a deletion before the transition",
            cold: Some(60),
            delete: Some(30),
            served: false,
        },
        Row {
            case: "both on the same day",
            cold: Some(45),
            delete: Some(45),
            served: false,
        },
        Row {
            case: "a transition on day zero, the minimum apart",
            cold: Some(0),
            delete: Some(90),
            served: true,
        },
        Row {
            case: "a year-long pair the minimum apart",
            cold: Some(365),
            delete: Some(455),
            served: true,
        },
        Row {
            case: "a year-long pair a day short",
            cold: Some(365),
            delete: Some(454),
            served: false,
        },
        Row {
            case: "the indefinite value for both members",
            cold: Some(-1),
            delete: Some(-1),
            served: true,
        },
        Row {
            case: "an indefinite deletion after a transition",
            cold: Some(30),
            delete: Some(-1),
            served: true,
        },
    ]
}

impl Row {
    /// The `Lifecycle` member this row sends, or `None` for the row that sends
    /// no lifecycle at all.
    fn lifecycle(&self) -> Option<Value> {
        if self.cold.is_none() && self.delete.is_none() {
            return None;
        }
        let mut lifecycle = json!({});
        if let Some(cold) = self.cold {
            lifecycle["MoveToColdStorageAfterDays"] = json!(cold);
        }
        if let Some(delete) = self.delete {
            lifecycle["DeleteAfterDays"] = json!(delete);
        }
        Some(lifecycle)
    }
}

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

/// A plan body of one rule: `lifecycle` on the rule itself when `on_rule`, and
/// on the rule's single copy action when not.
fn plan_body(name: &str, lifecycle: Option<Value>, on_rule: bool) -> Value {
    let mut rule = json!({"RuleName": "daily", "TargetBackupVaultName": VAULT});
    match (lifecycle, on_rule) {
        (None, _) => {}
        (Some(lifecycle), true) => rule["Lifecycle"] = lifecycle,
        (Some(lifecycle), false) => {
            rule["CopyActions"] =
                json!([{"DestinationBackupVaultArn": vault_arn(), "Lifecycle": lifecycle}]);
        }
    }
    json!({"BackupPlan": {"BackupPlanName": name, "Rules": [rule]}})
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "PUT", "/backup/plans", Some(&body.to_string())).await
}

/// `POST /backup/plans/{BackupPlanId}` — `UpdateBackupPlan`'s.
async fn update_plan(router: &MultiServiceRouter, plan_id: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&body.to_string()),
    )
    .await
}

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s.
async fn get_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/backup/plans/{plan_id}"), None).await
}

/// Create the vault every row names, so a served row is served on real state.
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

/// Assert a plan-body row: a pair outside the interval earns the condition's
/// own refusal and stores nothing, and one inside it is stored with the
/// lifecycle it named.
async fn assert_plan_surface(surface: &str, row: &Row, on_rule: bool) {
    let router = router();
    vault(&router).await;
    let name = "lifecycle-plan";
    let body = plan_body(name, row.lifecycle(), on_rule);
    let (status, answered) = create_plan(&router, &body).await;
    if !row.served {
        assert_eq!(
            (
                status,
                member(&answered, "Code"),
                member(&answered, "Message"),
                member(&answered, "Context"),
            ),
            (
                400,
                INTERVAL_CODE.to_string(),
                INTERVAL_REASON.to_string(),
                INTERVAL_CONTEXT.to_string(),
            ),
            "case {surface}/{:?}: expected the interval refusal, got {status} \
             {answered:?}",
            row.case
        );
        // Nothing was stored: the plan the refused create named is listed
        // nowhere.
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
            "case {surface}/{:?}: a refused create must store no plan, got {status} {listed:?}",
            row.case
        );
        return;
    }
    assert_eq!(
        status, 200,
        "case {surface}/{:?}: expected CreateBackupPlan 200, got {status} {answered:?}",
        row.case
    );
    // The stored rule reports the lifecycle the create named, where it named
    // one at all.
    let plan_id = member(&answered, "BackupPlanId");
    let (status, read) = get_plan(&router, &plan_id).await;
    let stored_rule = &read["BackupPlan"]["Rules"][0];
    let stored = if on_rule {
        stored_rule.get("Lifecycle").cloned()
    } else {
        stored_rule["CopyActions"][0].get("Lifecycle").cloned()
    };
    assert_eq!(
        (status, stored),
        (200, row.lifecycle()),
        "case {surface}/{:?}: expected the stored rule to report the lifecycle it was given, \
         got {status} {read:?}",
        row.case
    );
}

/// Every surface that stores a plan body applies the interval rule: the rule's
/// own lifecycle, a copy action's lifecycle, and the body an update stores. Each
/// states the condition's own sentence, with nothing wrapped around it.
#[tokio::test]
async fn a_plan_body_is_stored_only_with_a_lifecycle_that_leaves_the_cold_storage_interval() {
    for row in rows() {
        assert_plan_surface("CreateBackupPlan rule", &row, true).await;
        assert_plan_surface("CreateBackupPlan copy action", &row, false).await;

        // The update stores the NEXT version of a plan whose first version
        // carried no lifecycle at all, so what is judged is the body being
        // stored rather than the one already there.
        let router = router();
        vault(&router).await;
        let (status, created) = create_plan(&router, &plan_body("update-plan", None, true)).await;
        assert_eq!(
            status, 200,
            "setup: the first version should be created, got {status} {created:?}"
        );
        let plan_id = member(&created, "BackupPlanId");
        let first_version = member(&created, "VersionId");
        let (status, answered) = update_plan(
            &router,
            &plan_id,
            &plan_body("update-plan", row.lifecycle(), true),
        )
        .await;
        if row.served {
            assert_eq!(
                status, 200,
                "case UpdateBackupPlan/{:?}: expected 200, got {status} {answered:?}",
                row.case
            );
            assert_ne!(
                member(&answered, "VersionId"),
                first_version,
                "case UpdateBackupPlan/{:?}: a stored update must mint a new version, got \
                 {answered:?}",
                row.case
            );
        } else {
            assert_eq!(
                (
                    status,
                    member(&answered, "Code"),
                    member(&answered, "Message"),
                    member(&answered, "Context"),
                ),
                (
                    400,
                    INTERVAL_CODE.to_string(),
                    INTERVAL_REASON.to_string(),
                    INTERVAL_CONTEXT.to_string(),
                ),
                "case UpdateBackupPlan/{:?}: expected the interval refusal, got \
                 {status} {answered:?}",
                row.case
            );
            // A refused update stored no version: the plan still reads back at
            // the version the create minted.
            let (status, read) = get_plan(&router, &plan_id).await;
            assert_eq!(
                (status, member(&read, "VersionId")),
                (200, first_version.clone()),
                "case UpdateBackupPlan/{:?}: a refused update must store no version, got \
                 {status} {read:?}",
                row.case
            );
        }
    }
}

/// The same interval rule decides whether a backup JOB is started: the
/// recovery point it would create is kept under the lifecycle the request
/// names, and the refusal reports the SAME condition a plan body's lifecycle
/// does — one token, one `Context`, one reason — because the condition is one
/// wherever the member that carries it sits.
#[tokio::test]
async fn a_backup_job_is_started_only_with_a_lifecycle_that_leaves_the_cold_storage_interval() {
    for row in rows() {
        let router = router();
        vault(&router).await;
        let mut body = json!({
            "BackupVaultName": VAULT,
            "ResourceArn": format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo"),
            "IamRoleArn": role_arn(),
        });
        if let Some(lifecycle) = row.lifecycle() {
            body["Lifecycle"] = lifecycle;
        }
        let (status, errortype, answered) =
            call_reported(&router, "PUT", "/backup-jobs", Some(&body.to_string())).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case StartBackupJob/{:?}: expected 200, got {status} {answered:?}",
                row.case
            );
            continue;
        }
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
            "case StartBackupJob/{:?}: expected the interval refusal, got {status} {answered:?}",
            row.case
        );
        // A refused start created no job.
        let (status, listed) = call(&router, "GET", "/backup-jobs", None).await;
        let jobs = listed["BackupJobs"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            (status, jobs.len()),
            (200, 0),
            "case StartBackupJob/{:?}: a refused start must store no job, got {status} \
             {listed:?}",
            row.case
        );
    }
}
