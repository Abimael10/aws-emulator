// SPDX-License-Identifier: CC0-1.0

//! `DeleteRestoreTestingPlan`: what a delete of a restore testing plan takes out
//! of the store, and what bars it.
//!
//! The rules under test:
//!
//! * a delete takes the named plan OUT of the store, so every later call that
//!   resolves the name takes the plan's not-found path and the name is free for a
//!   create again — which mints another plan, with its own ARN — while every
//!   other plan the account holds is untouched;
//! * the model marks the operation `@idempotent` and gives it no not-found error,
//!   so a name this account holds no plan under — one nothing created, or one a
//!   previous delete already took — answers the same empty 204;
//! * "Deletion can only successfully occur if all associated restore testing
//!   selections are deleted first" (model doc for the operation), so a plan that
//!   still holds one is left exactly as it was and the call is refused
//!   `InvalidRequestException`, carrying that condition's own token and a
//!   sentence that names neither the plan nor how many selections bar it; once
//!   the last selection is deleted the plan's delete succeeds.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, the plan routes are `restore_testing_plan.rs`'s
//! and the selection routes `restore_testing_selection.rs`'s.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_plan::{
    assert_plan_arn_shape, create_body, create_plan, get_plan, minimal_plan, read_plan, NOT_FOUND,
};
use crate::restore_testing_selection::{
    create_body as selection_create_body, create_selection, get_selection, minimal_selection,
    name_of_length,
};
use crate::vault_and_backup_job::{call, call_reported, error_code, router, string_at};

/// The token this service states for "the restore testing plan this delete
/// addresses still holds restore testing selections".
const SELECTIONS_EXIST_CODE: &str = "ERROR_RT_9201";

/// The sentence it states for that condition — FIXED text: "Deletion can only
/// successfully occur if all associated restore testing selections are deleted
/// first" (model doc for `DeleteRestoreTestingPlan`), so it names what has to be
/// deleted first and nothing of the request.
const SELECTIONS_EXIST_MESSAGE: &str =
    "Related restore testing selections must be deleted prior to restore testing plan deletion";

/// The modelled class of that refusal, and of a name holding no plan.
const INVALID_REQUEST: &str = "InvalidRequestException";
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The most characters a `RestoreTestingPlanName` may carry: "Maximum length is
/// 50" (model doc for the member).
const MAX_NAME_LENGTH: usize = 50;

/// `DELETE /restore-testing/plans/{RestoreTestingPlanName}`, as the model binds
/// the delete.
async fn delete_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(
        router,
        "DELETE",
        &format!("/restore-testing/plans/{name}"),
        None,
    )
    .await
}

/// `PUT /restore-testing/plans/{RestoreTestingPlanName}`, the update, used to
/// show a deleted plan is gone for a MUTATION as well as for a read.
async fn update_plan(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let body = json!({ "RestoreTestingPlan": { "StartWindowHours": 12 } });
    call(
        router,
        "PUT",
        &format!("/restore-testing/plans/{name}"),
        Some(&body.to_string()),
    )
    .await
}

/// Assert `answer` is the empty success the model binds this delete to: 204, no
/// body and no error shape named.
fn assert_empty_success(case: &str, answer: &(u16, String, Value)) {
    let (status, errortype, body) = answer;
    assert_eq!(
        (*status, errortype.as_str(), body.clone()),
        (204, "<unset>", Value::Null),
        "case {case:?}: expected DeleteRestoreTestingPlan 204 with an empty body, got \
         {status} {errortype} {body:?}"
    );
}

/// A delete takes the plan out of the store: the read and the update of its name
/// both take the plan's not-found path, the name is free for a create again —
/// which mints a DIFFERENT ARN — and the other plan the account holds is
/// untouched.
///
/// The rows differ along what the rule generalises over: the name's length (1 and
/// the published maximum of 50) and spelling, whether the plan states the
/// optional members of its body, and whether it carries tags.
#[tokio::test]
async fn a_deleted_restore_testing_plan_is_no_longer_held() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "a plan stating no optional member",
            create_body(minimal_plan("plan_deleted")),
        ),
        (
            "the shortest admitted name",
            create_body(minimal_plan(&name_of_length(1))),
        ),
        (
            "the longest admitted name",
            create_body(minimal_plan(&name_of_length(MAX_NAME_LENGTH))),
        ),
        ("digits only", create_body(minimal_plan("20261121"))),
        (
            "a plan stating both windows and a timezone",
            create_body(json!({
                "RestoreTestingPlanName": "plan_full_body",
                "ScheduleExpression": "cron(15 3 ? * MON *)",
                "ScheduleExpressionTimezone": "Europe/Dublin",
                "StartWindowHours": 48,
                "RecoveryPointSelection": {
                    "Algorithm": "RANDOM_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["CONTINUOUS"],
                    "IncludeVaults": ["*"],
                    "SelectionWindowDays": 7,
                },
            })),
        ),
        ("a tagged plan", {
            let mut body = create_body(minimal_plan("plan_tagged"));
            body["Tags"] = json!({ "team": "restore-testing" });
            body
        }),
    ];

    for (case, body) in cases {
        let router = router();
        // A second plan, created first, so the delete below can be shown to take
        // only the plan it names.
        let (status, bystander) = create_plan(&router, &create_body(minimal_plan("plan_kept"))).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup: the bystander create should succeed, got {bystander:?}"
        );

        let plan = body
            .get("RestoreTestingPlan")
            .cloned()
            .unwrap_or(Value::Null);
        let name = string_at(&plan, "RestoreTestingPlanName");
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup: the create should succeed, got {status} {created:?}"
        );
        let first_arn = string_at(&created, "RestoreTestingPlanArn");

        let answer = delete_plan(&router, &name).await;
        assert_empty_success(case, &answer);

        let (status, read) = get_plan(&router, &name).await;
        assert_eq!(
            (
                status,
                error_code(&read).as_str(),
                string_at(&read, "Message").as_str()
            ),
            (400, RESOURCE_NOT_FOUND, NOT_FOUND),
            "case {case:?}: a deleted plan is no longer held, got {status} {read:?}"
        );
        let (status, updated) = update_plan(&router, &name).await;
        assert_eq!(
            (status, error_code(&updated).as_str()),
            (400, RESOURCE_NOT_FOUND),
            "case {case:?}: a deleted plan cannot be updated either, got {status} {updated:?}"
        );

        // The name is free again, and what it addresses afterwards is ANOTHER
        // plan: its ARN "uniquely identifies a restore testing plan" (model doc
        // for the member), so it is not the deleted plan's.
        let (status, recreated) = create_plan(&router, &create_body(minimal_plan(&name))).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the name must be free for a create again, got {status} {recreated:?}"
        );
        let second_arn = string_at(&recreated, "RestoreTestingPlanArn");
        assert_plan_arn_shape(case, &name, &second_arn);
        assert_ne!(
            second_arn, first_arn,
            "case {case:?}: a plan created under a deleted plan's name is another resource, got \
             {second_arn:?}"
        );

        let (status, kept) = get_plan(&router, "plan_kept").await;
        assert_eq!(
            (
                status,
                string_at(&read_plan(&kept), "RestoreTestingPlanArn")
            ),
            (200, string_at(&bystander, "RestoreTestingPlanArn")),
            "case {case:?}: the delete must touch no other plan, got {status} {kept:?}"
        );
    }
}

/// The delete asks for a STATE — that the named plan is not held — so a name the
/// account holds no plan under answers the same empty success, and repeating the
/// delete of a plan answers as the first one did.
///
/// The rows differ in the form of the absent name, spanning what the documented
/// alphabet admits: both published ends of the length bound, digits, underscored
/// words and a case variant of a name that IS held.
#[tokio::test]
async fn deleting_a_plan_the_account_does_not_hold_is_the_same_empty_success() {
    let router = router();
    let (status, held) = create_plan(&router, &create_body(minimal_plan("plan_held"))).await;
    assert_eq!(
        status, 201,
        "setup: the create should succeed, got {status} {held:?}"
    );

    let absent: Vec<(&str, String)> = vec![
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(MAX_NAME_LENGTH)),
        ("digits only", "20261121".to_string()),
        ("underscored words", "plan_never_created".to_string()),
        ("a case variant of a held name", "PLAN_HELD".to_string()),
    ];
    for (case, name) in &absent {
        let answer = delete_plan(&router, name).await;
        assert_empty_success(case, &answer);
    }

    // Non-vacuity: the plan this emulator does hold is still held after all
    // those deletes, so the empty successes above removed nothing else.
    let (status, read) = get_plan(&router, "plan_held").await;
    assert_eq!(
        (status, string_at(&read_plan(&read), "RestoreTestingPlanArn")),
        (200, string_at(&held, "RestoreTestingPlanArn")),
        "a delete of an absent name must leave the held plan alone, got {status} {read:?}"
    );

    // And the second, third … delete of a plan the account DID hold is the same
    // empty success as the first.
    for attempt in 1..=3 {
        let case = format!("delete attempt {attempt}");
        let answer = delete_plan(&router, "plan_held").await;
        assert_empty_success(&case, &answer);
    }
    let (status, gone) = get_plan(&router, "plan_held").await;
    assert_eq!(
        (status, error_code(&gone).as_str()),
        (400, RESOURCE_NOT_FOUND),
        "the repeated deletes must leave the name holding no plan, got {status} {gone:?}"
    );
}

/// "Deletion can only successfully occur if all associated restore testing
/// selections are deleted first" (model doc for the operation): while the plan
/// holds one, the delete is refused `InvalidRequestException` with that
/// condition's token, the plan and its selections are left exactly as they were,
/// and the delete succeeds only once the LAST selection is gone.
///
/// The rows differ in how many selections the plan holds, so the rule is
/// asserted over the class rather than over one plan with one selection.
#[tokio::test]
async fn a_plan_still_holding_selections_cannot_be_deleted() {
    for held in [1_usize, 2, 4] {
        let case = format!("{held} selection(s) held");
        let router = router();
        let (status, created) = create_plan(&router, &create_body(minimal_plan("plan_parent"))).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup: the plan create should succeed, got {status} {created:?}"
        );
        let plan_arn = string_at(&created, "RestoreTestingPlanArn");

        let names: Vec<String> = (0..held).map(|index| format!("selection_{index}")).collect();
        for name in &names {
            let (status, stored) = create_selection(
                &router,
                "plan_parent",
                &selection_create_body(minimal_selection(name)),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: setup: the selection create should succeed, got {status} \
                 {stored:?}"
            );
        }

        // Every selection but the last one is deleted one at a time: while ANY
        // remains, the plan's delete is refused, and the plan is left as it was.
        for (removed, name) in names.iter().enumerate() {
            let (status, errortype, refused) = delete_plan(&router, "plan_parent").await;
            assert_eq!(
                (
                    status,
                    errortype.as_str(),
                    error_code(&refused).as_str(),
                    string_at(&refused, "Code").as_str(),
                    string_at(&refused, "Message").as_str()
                ),
                (
                    400,
                    INVALID_REQUEST,
                    INVALID_REQUEST,
                    SELECTIONS_EXIST_CODE,
                    SELECTIONS_EXIST_MESSAGE
                ),
                "case {case:?} with {} selection(s) left: expected the selections-exist refusal, \
                 got {status} {refused:?}",
                held - removed
            );
            // The refused delete removed nothing: the plan still reads back as
            // the create stored it, and so does every selection it holds.
            let (status, read) = get_plan(&router, "plan_parent").await;
            assert_eq!(
                (status, string_at(&read_plan(&read), "RestoreTestingPlanArn")),
                (200, plan_arn.clone()),
                "case {case:?}: a refused delete must leave the plan held, got {status} {read:?}"
            );
            for still_held in &names[removed..] {
                let (status, selection) =
                    get_selection(&router, "plan_parent", still_held).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a refused delete must leave {still_held:?} held, got \
                     {status} {selection:?}"
                );
            }

            let (status, deleted, _) = call_reported(
                &router,
                "DELETE",
                &format!("/restore-testing/plans/plan_parent/selections/{name}"),
                None,
            )
            .await;
            assert_eq!(
                (status, deleted.as_str()),
                (204, "<unset>"),
                "case {case:?}: setup: the selection delete should succeed, got {status}"
            );
        }

        // With the last selection gone, the plan's delete is the empty success.
        let answer = delete_plan(&router, "plan_parent").await;
        assert_empty_success(&case, &answer);
        let (status, gone) = get_plan(&router, "plan_parent").await;
        assert_eq!(
            (
                status,
                error_code(&gone).as_str(),
                string_at(&gone, "Message").as_str()
            ),
            (400, RESOURCE_NOT_FOUND, NOT_FOUND),
            "case {case:?}: the plan must be gone once its selections are, got {status} {gone:?}"
        );
    }
}
