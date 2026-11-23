// SPDX-License-Identifier: CC0-1.0

//! `UpdateRestoreTestingPlan` and what the read over the stored plan
//! (`GetRestoreTestingPlan`) reports afterwards.
//!
//! The rules under test:
//!
//! * an update REWRITES the stored plan: every member its body states is what
//!   every later read of that plan reports, and a member it states nothing for
//!   keeps the value the plan already held — "RestoreTestingPlanName cannot be
//!   updated after it is created" (model doc for `UpdateRestoreTestingPlan`), so
//!   the name and the identifiers minted from it (the ARN and the creation
//!   instant) survive the rewrite;
//! * a submitted `RecoveryPointSelection` replaces the stored one completed with
//!   the documented defaults, exactly as a create completes one
//!   (`SelectionWindowDays` "defaults to 30", `ExcludeVaults` "defaults to empty
//!   list if not listed");
//! * the update's `UpdateTime` is the instant it stored the new body, and it is
//!   what the read then reports as the plan's `LastUpdateTime`;
//! * a body outside what the model documents is refused
//!   `InvalidParameterValueException` naming the member, and a name the account
//!   holds no plan under is the one not-found refusal — a refused update rewrites
//!   nothing.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs` and the bodies are the ones the sibling
//! `restore_testing_plan.rs` suite already states.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_plan::{
    create_body, create_plan, get_plan, int_at, minimal_plan, read_plan,
    DEFAULT_SELECTION_WINDOW_DAYS, DEFAULT_START_WINDOW_HOURS, DEFAULT_TIMEZONE,
    MAX_SELECTION_WINDOW_DAYS, MAX_START_WINDOW_HOURS, MIN_SELECTION_WINDOW_DAYS, NOT_FOUND,
};
use crate::vault_and_backup_job::{call, error_code, router, string_at, ACCOUNT, REGION};

/// `PUT /restore-testing/plans/{RestoreTestingPlanName}`, as the model binds the
/// update.
async fn update_plan(router: &MultiServiceRouter, name: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/restore-testing/plans/{name}"),
        Some(&body.to_string()),
    )
    .await
}

/// An update body carrying `plan` as its `RestoreTestingPlan`.
fn update_body(plan: Value) -> Value {
    json!({ "RestoreTestingPlan": plan })
}

/// A string member of a body, if it carries one.
fn string_of(body: &Value, member: &str) -> Option<String> {
    body.get(member)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// A selection as a STORED plan carries it: the body submitted, with the two
/// members the model gives a default for filled in where it stated neither.
fn completed_selection(selection: &Value) -> Value {
    let mut completed = selection.clone();
    completed["SelectionWindowDays"] = json!(selection
        .get("SelectionWindowDays")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_SELECTION_WINDOW_DAYS));
    completed["ExcludeVaults"] = selection
        .get("ExcludeVaults")
        .cloned()
        .unwrap_or_else(|| json!([]));
    completed
}

/// A plan body stating every member an update may change, so a row that states
/// one member shows the others surviving the rewrite.
fn stated_plan(name: &str) -> Value {
    json!({
        "RestoreTestingPlanName": name,
        "ScheduleExpression": "cron(0 5 ? * * *)",
        "ScheduleExpressionTimezone": "Europe/Dublin",
        "StartWindowHours": 24,
        "RecoveryPointSelection": {
            "Algorithm": "LATEST_WITHIN_WINDOW",
            "RecoveryPointTypes": ["SNAPSHOT"],
            "IncludeVaults": ["*"],
            "SelectionWindowDays": 30,
        },
    })
}

/// Create `plan` and answer the identifiers the create reported.
async fn create_baseline(router: &MultiServiceRouter, case: &str, plan: &Value) -> Value {
    let (status, created) = create_plan(router, &create_body(plan.clone())).await;
    assert_eq!(
        (status, error_code(&created).as_str()),
        (201, "<none>"),
        "case {case:?}: setup create should succeed, got {status} {created:?}"
    );
    created
}

/// An update rewrites exactly the members its body states, leaves every other
/// member of the stored plan alone, and every later read reports the result.
///
/// The rows differ along what the rule generalises over: which members the body
/// states (one at a time, and all four at once), the spelling of the schedule
/// (wrapped in `cron(…)` and bare), the selection's algorithm, recovery-point
/// types and vaults, whether the baseline plan stated a timezone at all, and
/// both windows — at the floor of their range, at their published ceiling and
/// absent.
#[tokio::test]
async fn an_update_rewrites_the_members_its_body_states() {
    let vault = format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:tested");
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "the schedule alone",
            stated_plan("plan_update_schedule"),
            json!({ "ScheduleExpression": "cron(0 6 ? * * *)" }),
        ),
        (
            "the selection alone, its window at the floor",
            stated_plan("plan_update_selection"),
            json!({
                "RecoveryPointSelection": {
                    "Algorithm": "RANDOM_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["CONTINUOUS"],
                    "IncludeVaults": [vault],
                    "SelectionWindowDays": MIN_SELECTION_WINDOW_DAYS,
                },
            }),
        ),
        (
            "the timezone alone",
            stated_plan("plan_update_timezone"),
            json!({ "ScheduleExpressionTimezone": "America/New_York" }),
        ),
        (
            "the start window alone, at its published ceiling",
            stated_plan("plan_update_window"),
            json!({ "StartWindowHours": MAX_START_WINDOW_HOURS }),
        ),
        (
            "every member at once, a bare cron and the window ceiling",
            stated_plan("plan_update_everything"),
            json!({
                "ScheduleExpression": "15 * ? * * *",
                "ScheduleExpressionTimezone": "Asia/Tokyo",
                "StartWindowHours": 1,
                "RecoveryPointSelection": {
                    "Algorithm": "LATEST_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["SNAPSHOT", "CONTINUOUS"],
                    "IncludeVaults": ["*"],
                    "ExcludeVaults": [vault],
                    "SelectionWindowDays": MAX_SELECTION_WINDOW_DAYS,
                },
            }),
        ),
        (
            "a selection stating neither of its defaulted members",
            stated_plan("plan_update_defaults"),
            json!({
                "RecoveryPointSelection": {
                    "Algorithm": "RANDOM_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["SNAPSHOT"],
                    "IncludeVaults": ["*"],
                },
            }),
        ),
        (
            "an empty body over a plan created with no timezone and no windows",
            minimal_plan("plan_update_nothing"),
            json!({}),
        ),
        (
            "the schedule over a plan created with no timezone and no windows",
            minimal_plan("plan_update_minimal"),
            json!({ "ScheduleExpression": "cron(30 7 ? * MON *)" }),
        ),
    ];

    for (case, baseline, body) in cases {
        let router = router();
        let name = string_at(&baseline, "RestoreTestingPlanName");
        let create_answer = create_baseline(&router, case, &baseline).await;

        let (status, updated) = update_plan(&router, &name, &update_body(body.clone())).await;
        assert_eq!(
            (status, error_code(&updated).as_str()),
            (200, "<none>"),
            "case {case:?}: expected UpdateRestoreTestingPlan 200, got {status} {updated:?}"
        );
        // The name addresses the plan rather than being rewritten, so the
        // identifiers minted from it survive.
        assert_eq!(
            (
                string_at(&updated, "RestoreTestingPlanName"),
                string_at(&updated, "RestoreTestingPlanArn"),
                updated.get("CreationTime").cloned()
            ),
            (
                name.clone(),
                string_at(&create_answer, "RestoreTestingPlanArn"),
                create_answer.get("CreationTime").cloned()
            ),
            "case {case:?}: the update must report the plan's own identifiers, got {updated:?}"
        );
        let update_time = updated.get("UpdateTime").cloned();
        assert!(
            update_time.as_ref().is_some_and(Value::is_number),
            "case {case:?}: the update reports when it stored the body, got {updated:?}"
        );
        let (created_at, updated_at) = (
            create_answer
                .get("CreationTime")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            update_time.as_ref().and_then(Value::as_f64).unwrap_or(-1.0),
        );
        assert!(
            updated_at >= created_at,
            "case {case:?}: the update's instant must not precede the create's, got {updated_at} \
             after {created_at}"
        );

        // What the read must now report: a member the update stated, else the
        // one the create stored, else the documented default.
        let expected_schedule = string_of(&body, "ScheduleExpression")
            .or_else(|| string_of(&baseline, "ScheduleExpression"))
            .unwrap_or_default();
        let expected_timezone = string_of(&body, "ScheduleExpressionTimezone")
            .or_else(|| string_of(&baseline, "ScheduleExpressionTimezone"))
            .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
        let expected_start_window = body
            .get("StartWindowHours")
            .and_then(Value::as_i64)
            .or_else(|| baseline.get("StartWindowHours").and_then(Value::as_i64))
            .unwrap_or(DEFAULT_START_WINDOW_HOURS);
        let expected_selection = completed_selection(
            &body
                .get("RecoveryPointSelection")
                .or_else(|| baseline.get("RecoveryPointSelection"))
                .cloned()
                .unwrap_or(Value::Null),
        );

        let (status, read) = get_plan(&router, &name).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: expected GetRestoreTestingPlan 200, got {status} {read:?}"
        );
        let stored = read_plan(&read);
        assert_eq!(
            (
                string_at(&stored, "RestoreTestingPlanName"),
                string_at(&stored, "RestoreTestingPlanArn"),
                string_at(&stored, "ScheduleExpression"),
                string_at(&stored, "ScheduleExpressionTimezone"),
                int_at(&stored, "StartWindowHours"),
            ),
            (
                name.clone(),
                string_at(&create_answer, "RestoreTestingPlanArn"),
                expected_schedule,
                expected_timezone,
                expected_start_window,
            ),
            "case {case:?}: the read must report the updated plan, got {read:?}"
        );
        assert_eq!(
            stored.get("RecoveryPointSelection").cloned(),
            Some(expected_selection),
            "case {case:?}: a submitted selection replaces the stored one, completed with its \
             defaults, got {stored:?}"
        );
        assert_eq!(
            (
                stored.get("CreationTime").cloned(),
                stored.get("LastUpdateTime").cloned()
            ),
            (create_answer.get("CreationTime").cloned(), update_time),
            "case {case:?}: the read must report the update's instant beside the create's, got \
             {read:?}"
        );
    }
}

/// A second update over an already updated plan rewrites it again: the last
/// body submitted is what the read reports, and the plan's last-update instant
/// moves on while its creation instant and ARN stay put.
#[tokio::test]
async fn successive_updates_each_rewrite_the_plan() {
    let router = router();
    let name = "plan_update_twice";
    let create_answer = create_baseline(&router, "successive updates", &stated_plan(name)).await;

    let schedules = ["cron(0 6 ? * * *)", "cron(0 7 ? * * *)", "0 8 ? * * *"];
    let mut previous = create_answer
        .get("CreationTime")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    for (round, schedule) in schedules.iter().enumerate() {
        let window = 45 + i64::try_from(round).unwrap_or_default();
        let body = update_body(json!({
            "ScheduleExpression": schedule,
            "RecoveryPointSelection": {
                "Algorithm": "LATEST_WITHIN_WINDOW",
                "RecoveryPointTypes": ["SNAPSHOT"],
                "IncludeVaults": ["*"],
                "SelectionWindowDays": window,
            },
        }));
        let (status, updated) = update_plan(&router, name, &body).await;
        assert_eq!(
            (status, error_code(&updated).as_str()),
            (200, "<none>"),
            "round {round}: expected UpdateRestoreTestingPlan 200, got {status} {updated:?}"
        );
        let update_time = updated
            .get("UpdateTime")
            .and_then(Value::as_f64)
            .unwrap_or(-1.0);
        assert!(
            update_time >= previous,
            "round {round}: the update's instant must not precede the previous write's, got \
             {update_time} after {previous}"
        );
        previous = update_time;

        let (status, read) = get_plan(&router, name).await;
        let stored = read_plan(&read);
        assert_eq!(
            (
                status,
                string_at(&stored, "ScheduleExpression"),
                int_at(
                    &stored
                        .get("RecoveryPointSelection")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "SelectionWindowDays"
                ),
                stored.get("LastUpdateTime").and_then(Value::as_f64),
                stored.get("CreationTime").cloned(),
            ),
            (
                200,
                (*schedule).to_string(),
                window,
                Some(update_time),
                create_answer.get("CreationTime").cloned(),
            ),
            "round {round}: the read must report the body the last update submitted, got {read:?}"
        );
        assert_eq!(
            string_at(&stored, "RestoreTestingPlanArn"),
            string_at(&create_answer, "RestoreTestingPlanArn"),
            "round {round}: an update must not re-mint the plan's ARN, got {stored:?}"
        );
    }
}

/// An update whose body is outside what the model documents is refused
/// `InvalidParameterValueException` naming the member it rejected, and the plan
/// it addressed is left exactly as it was.
///
/// The rows span the documented rules the update's body carries, each one step
/// past its bound.
#[tokio::test]
async fn an_update_body_outside_the_documented_rules_is_refused() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a schedule that is not a cron expression",
            json!({ "ScheduleExpression": "nightly" }),
            "ScheduleExpression",
        ),
        (
            "a cron expression short of its six fields",
            json!({ "ScheduleExpression": "cron(0 5 ? *)" }),
            "ScheduleExpression",
        ),
        (
            "a start window one hour past its ceiling",
            json!({ "StartWindowHours": MAX_START_WINDOW_HOURS + 1 }),
            "StartWindowHours",
        ),
        (
            "a negative start window",
            json!({ "StartWindowHours": -1 }),
            "StartWindowHours",
        ),
        (
            "a selection window one day past its ceiling",
            json!({
                "RecoveryPointSelection": {
                    "Algorithm": "LATEST_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["SNAPSHOT"],
                    "IncludeVaults": ["*"],
                    "SelectionWindowDays": MAX_SELECTION_WINDOW_DAYS + 1,
                },
            }),
            "SelectionWindowDays",
        ),
        (
            "a negative selection window",
            json!({
                "RecoveryPointSelection": {
                    "Algorithm": "LATEST_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["SNAPSHOT"],
                    "IncludeVaults": ["*"],
                    "SelectionWindowDays": -1,
                },
            }),
            "SelectionWindowDays",
        ),
    ];

    for (case, body, member) in cases {
        let router = router();
        let name = "plan_update_refused";
        create_baseline(&router, case, &stated_plan(name)).await;
        let (_, before) = get_plan(&router, name).await;

        let (status, refused) = update_plan(&router, name, &update_body(body)).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str(),
                string_at(&refused, "Context").as_str()
            ),
            (
                400,
                "InvalidParameterValueException",
                format!("Parameter {member} is invalid.").as_str(),
                member
            ),
            "case {case:?}: expected the refusal to name {member}, got {status} {refused:?}"
        );
        let (_, after) = get_plan(&router, name).await;
        assert_eq!(
            read_plan(&after),
            read_plan(&before),
            "case {case:?}: a refused update must rewrite nothing"
        );
    }
}

/// An update naming a plan the account does not hold is the one not-found
/// refusal, and it stores nothing: the name still holds no plan afterwards.
#[tokio::test]
async fn an_update_of_an_unknown_name_is_refused() {
    let router = router();
    create_baseline(&router, "setup", &stated_plan("plan_update_present")).await;

    for name in ["plan_update_absent", "plan_Update_Present", "PLAN_PRESENT"] {
        let (status, refused) = update_plan(
            &router,
            name,
            &update_body(json!({ "ScheduleExpression": "cron(0 6 ? * * *)" })),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {name:?}: a name holding no plan is not found, got {status} {refused:?}"
        );
        let (status, read) = get_plan(&router, name).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ResourceNotFoundException"),
            "case {name:?}: a refused update must store no plan, got {status} {read:?}"
        );
    }
}
