// SPDX-License-Identifier: CC0-1.0

//! The SERIALIZATION of `UpdateRestoreTestingPlan`: what happens when more than
//! one update of one restore testing plan is in flight at the same time.
//!
//! The rule under test: the updates of ONE plan are serialized, so of however
//! many are in flight together at least one is applied and answers 200 and they
//! cannot all be, an update that arrives while another is still being applied is
//! refused 409 `ConflictException` naming the plan it addressed, and what a later
//! read reports is an applied update's body — never a refused racer's. Once the
//! racers have answered the plan is free again, so the next update of it is
//! served rather than refused. Updates of DIFFERENT plans never contend: the
//! serialization is per plan.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs` and the plan bodies and routes are the sibling
//! `restore_testing_plan.rs` suite's.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_plan::{create_body, create_plan, get_plan, minimal_plan, read_plan};
use crate::vault_and_backup_job::{call, error_code, router, string_at};

/// The name the plan every case here contends for is stored under.
const PLAN: &str = "plan_contended_update";

/// The sentence this service states for "another update of this plan is still
/// being applied": fixed text around the name the request addressed the plan by.
fn concurrent_update(name: &str) -> String {
    format!(
        "Failed to update restore testing plan [{name}] due to an attempt to update it \
         concurrently."
    )
}

/// `PUT /restore-testing/plans/{RestoreTestingPlanName}`, as the model binds the
/// update.
async fn update_plan(router: &MultiServiceRouter, name: &str, plan: Value) -> (u16, Value) {
    let body = json!({ "RestoreTestingPlan": plan });
    call(
        router,
        "PUT",
        &format!("/restore-testing/plans/{name}"),
        Some(&body.to_string()),
    )
    .await
}

/// A router holding the plan the racers below update.
async fn with_plan() -> MultiServiceRouter {
    let router = router();
    let (status, created) = create_plan(&router, &create_body(minimal_plan(PLAN))).await;
    assert_eq!(
        status, 201,
        "setup: the contended plan must be created, got {status} {created:?}"
    );
    router
}

/// However many updates of ONE plan are in flight together, at least one is
/// applied with 200; an update that finds the plan held is refused 409
/// `ConflictException` naming the plan, and the plan ends at a WINNER's body.
///
/// The rows differ in how many updates race, so the rule is asserted over the
/// class rather than over one burst size.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_of_one_plan_in_flight_together_are_serialized() {
    for racers in [2_usize, 6, 11] {
        let router = Arc::new(with_plan().await);
        // Each racer submits a DISTINCT schedule, so the value the plan ends at
        // names which call wrote it.
        let schedules: Vec<String> = (0..racers)
            .map(|i| format!("cron(0 {i} ? * * *)"))
            .collect();
        let mut pending = Vec::new();
        for schedule in &schedules {
            let router = Arc::clone(&router);
            let body = json!({ "ScheduleExpression": schedule });
            pending.push(tokio::spawn(async move {
                update_plan(&router, PLAN, body).await
            }));
        }
        let mut answers: Vec<(u16, Value)> = Vec::with_capacity(racers);
        for task in pending {
            answers.push(task.await.expect("a racing update to finish"));
        }

        let case = format!("{racers} updates of one plan, in flight together");
        let winning_schedules: Vec<String> = answers
            .iter()
            .zip(&schedules)
            .filter(|((status, _), _)| *status == 200)
            .map(|(_, schedule)| schedule.clone())
            .collect();
        assert!(
            !winning_schedules.is_empty(),
            "case {case:?}: at least one update must be applied with 200, got {answers:?}"
        );
        // The updates are SERIALIZED, so they cannot all be applied: the ones
        // that found the plan held were refused, and every one of them carries
        // the conflict refusal asserted below. Without that refusal the whole
        // burst would answer 200 and this row would fail here.
        let refused = answers.iter().filter(|(status, _)| *status != 200).count();
        assert!(
            refused >= 1,
            "case {case:?}: an update that arrives while the plan is being written must be \
             refused, so at most {} of the {racers} racers can be applied — got {} applied, \
             {answers:?}",
            racers - 1,
            winning_schedules.len()
        );
        for (status, answer) in answers.iter().filter(|(status, _)| *status != 200) {
            assert_eq!(
                (
                    *status,
                    error_code(answer).as_str(),
                    string_at(answer, "Message").as_str()
                ),
                (409, "ConflictException", concurrent_update(PLAN).as_str()),
                "case {case:?}: an update that lost the plan is refused as a concurrent one, got \
                 {status} {answer:?}"
            );
        }
        // One winner at a time, so the stored body is one call's rather than a
        // blend: the schedule the plan ends at is the one the winner submitted,
        // asserted below.

        // The plan ends at a WINNER's schedule: one of the submitted ones, and
        // one whose own call answered 200.
        let (status, read) = get_plan(&router, PLAN).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the contended plan must be readable, got {status} {read:?}"
        );
        let stored = read_plan(&read);
        let written = string_at(&stored, "ScheduleExpression");
        assert!(
            winning_schedules.contains(&written),
            "case {case:?}: the plan must hold a WINNER's schedule, not a refused racer's — got \
             {written:?} beside winners {winning_schedules:?}"
        );
        assert_eq!(
            string_at(&stored, "RestoreTestingPlanName"),
            PLAN,
            "case {case:?}: the contention stores no second plan, got {stored:?}"
        );

        // The plan is FREE once the racers answered: an update afterwards is
        // served rather than refused as a concurrent one.
        let (status, after) =
            update_plan(&router, PLAN, json!({ "ScheduleExpression": "cron(0 4 ? * * *)" })).await;
        assert_eq!(
            (status, error_code(&after).as_str()),
            (200, "<none>"),
            "case {case:?}: once the racers answered, the next update is served, got {status} \
             {after:?}"
        );
        let (_, read) = get_plan(&router, PLAN).await;
        assert_eq!(
            string_at(&read_plan(&read), "ScheduleExpression"),
            "cron(0 4 ? * * *)",
            "case {case:?}: that update is the one the plan then holds, got {read:?}"
        );
    }
}

/// Updates of DIFFERENT plans do not contend with one another: each plan is
/// taken on its own, so every call is applied.
///
/// The rows differ in how many plans are updated at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_of_different_plans_in_flight_together_are_all_applied() {
    for plans in [2_usize, 7] {
        let router = Arc::new(router());
        let names: Vec<String> = (0..plans).map(|i| format!("plan_apart_{i}")).collect();
        for name in &names {
            let (status, created) = create_plan(&router, &create_body(minimal_plan(name))).await;
            assert_eq!(
                status, 201,
                "setup: {name} must be created, got {status} {created:?}"
            );
        }

        let mut pending = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let router = Arc::clone(&router);
            let name = name.clone();
            let body = json!({ "ScheduleExpression": format!("cron(0 {i} ? * * *)") });
            pending.push(tokio::spawn(async move {
                update_plan(&router, &name, body).await
            }));
        }
        for (i, task) in pending.into_iter().enumerate() {
            let (status, answer) = task.await.expect("an update of its own plan to finish");
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (200, "<none>"),
                "case {plans} plans updated together: the update of plan {i} must be applied, got \
                 {status} {answer:?}"
            );
        }

        // Each plan holds its own update's schedule.
        for (i, name) in names.iter().enumerate() {
            let (status, read) = get_plan(&router, name).await;
            assert_eq!(
                (status, string_at(&read_plan(&read), "ScheduleExpression")),
                (200, format!("cron(0 {i} ? * * *)")),
                "case {plans} plans updated together: {name} must hold its own update, got \
                 {status} {read:?}"
            );
        }
    }
}
