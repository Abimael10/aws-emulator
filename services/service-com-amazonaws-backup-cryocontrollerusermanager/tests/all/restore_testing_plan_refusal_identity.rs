// SPDX-License-Identifier: CC0-1.0

//! What a refused call on the restore-testing-PLAN surface tells a client: the
//! token it branches on, and the sentence the condition is worded with.
//!
//! The rule under test: a refusal names its condition TWICE — once as the
//! modelled error shape (the response's `x-amzn-errortype`, and `__type` in its
//! body) and once in the body's `Code` member, which carries that condition's
//! own token — beside the sentence AWS words for the condition, whose fixed text
//! is constant and whose variable parts come from the request and from this
//! emulator's own state. So:
//!
//! * every call that addresses a plan by a name this account holds no plan under
//!   answers ONE condition — the plan's not-found — with its token and its fixed
//!   sentence, whatever the name's spelling and whichever operation asked;
//! * a create naming a plan the account already holds answers the already-exists
//!   condition with ITS token and the sentence built from the name the request
//!   carried and the account the call was made in, so two different names are two
//!   different sentences;
//! * a plan a delete took is held by nothing afterwards, so the read and the
//!   update of its name answer the not-found condition — the same token, sentence
//!   and shape a name nothing ever created answers;
//! * an answer that is SERVED names no error shape and carries no token at all.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, the refusal-body readers are
//! `refusal_error_codes.rs`'s and the plan routes and bodies are
//! `restore_testing_plan.rs`'s.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::refusal_error_codes::{expected, member_or_unset, reported};
use crate::restore_testing_plan::{create_body, create_plan, get_plan, minimal_plan, NOT_FOUND};
use crate::restore_testing_selection::name_of_length;
use crate::vault_and_backup_job::{call_reported, error_code, router, string_at, ACCOUNT};

/// The modelled classes these two conditions are refused as, which is what both
/// the `x-amzn-errortype` header and the body's `__type` name.
const NOT_FOUND_SHAPE: &str = "ResourceNotFoundException";
const ALREADY_EXISTS_SHAPE: &str = "AlreadyExistsException";

/// The token this service states for "the restore testing plan addressed is not
/// one this account holds".
const PLAN_NOT_FOUND_CODE: &str = "ERROR_RT_5000";

/// The token it states for "a restore testing plan of this name already exists",
/// which shares the not-found condition's HTTP status and so cannot share its
/// token.
const PLAN_EXISTS_CODE: &str = "ERROR_RT_3000";

/// The most characters a `RestoreTestingPlanName` may carry: "Maximum length is
/// 50" (model doc for `RestoreTestingPlanForCreate$RestoreTestingPlanName`).
const MAX_NAME_LENGTH: usize = 50;

/// The shape header a SERVED answer carries, which is none.
const NO_SHAPE: &str = "<unset>";

/// The sentence this service states for a create naming a plan the account
/// already holds: fixed text around the name the request carried and the account
/// the call was made in.
fn already_exists(name: &str) -> String {
    format!("Restore testing plan [{name}] already exists in account [{ACCOUNT}].")
}

/// `GET /restore-testing/plans/{RestoreTestingPlanName}` with the shape the
/// response names beside its body.
async fn get_plan_reported(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(
        router,
        "GET",
        &format!("/restore-testing/plans/{name}"),
        None,
    )
    .await
}

/// `PUT /restore-testing/plans/{RestoreTestingPlanName}` — the update, carrying a
/// body the documented rules admit, so what it answers is about the name it
/// addresses rather than about its body.
async fn update_plan_reported(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    let body = json!({ "RestoreTestingPlan": { "StartWindowHours": 12 } });
    call_reported(
        router,
        "PUT",
        &format!("/restore-testing/plans/{name}"),
        Some(&body.to_string()),
    )
    .await
}

/// `PUT /restore-testing/plans`, the create, with the shape the response names.
async fn create_plan_reported(router: &MultiServiceRouter, body: &Value) -> (u16, String, Value) {
    call_reported(
        router,
        "PUT",
        "/restore-testing/plans",
        Some(&body.to_string()),
    )
    .await
}

/// `DELETE /restore-testing/plans/{RestoreTestingPlanName}`.
async fn delete_plan(router: &MultiServiceRouter, name: &str) -> (u16, String, Value) {
    call_reported(
        router,
        "DELETE",
        &format!("/restore-testing/plans/{name}"),
        None,
    )
    .await
}

/// Both calls that address a plan by name and can refuse one, as a client sees
/// them: the operation's name beside its answer.
type Answer = (u16, String, Value);
async fn answers_for(router: &MultiServiceRouter, name: &str) -> Vec<(&'static str, Answer)> {
    vec![
        (
            "GetRestoreTestingPlan",
            get_plan_reported(router, name).await,
        ),
        (
            "UpdateRestoreTestingPlan",
            update_plan_reported(router, name).await,
        ),
    ]
}

/// Assert `answer` is the plan's not-found condition, stated in the body (shape,
/// token and sentence) and in the shape the response NAMES to a client.
fn assert_plan_not_found(case: &str, answer: &Answer) {
    let (status, errortype, body) = answer;
    assert_eq!(
        reported(*status, body),
        expected(400, NOT_FOUND_SHAPE, PLAN_NOT_FOUND_CODE, NOT_FOUND),
        "case {case:?}: expected the plan's not-found refusal with Code={PLAN_NOT_FOUND_CODE}, \
         got {status} {body:?}"
    );
    assert_eq!(
        errortype.as_str(),
        NOT_FOUND_SHAPE,
        "case {case:?}: the response must name {NOT_FOUND_SHAPE} to a client, got {errortype}"
    );
}

/// The forms of an absent name the rows below span: both published ends of the
/// length bound, digits only, underscored words and a case variant of a name the
/// account DOES hold — so the condition follows the name addressed rather than
/// its spelling.
fn absent_plan_names() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(MAX_NAME_LENGTH)),
        ("digits only", "20261121".to_string()),
        ("underscored words", "plan_never_created".to_string()),
        ("a case variant of a held name", "PLAN_HELD".to_string()),
    ]
}

/// Every call that addresses a plan by a name this account holds no plan under
/// states the plan's not-found token and its fixed sentence, beside the modelled
/// shape — and the plan the same emulator does hold is served instead, with no
/// token at all.
#[tokio::test]
async fn a_call_addressing_an_absent_plan_states_the_plan_not_found_token() {
    let router = router();
    let (status, held) = create_plan(&router, &create_body(minimal_plan("plan_held"))).await;
    assert_eq!(
        status, 201,
        "setup: the create should succeed, got {status} {held:?}"
    );

    for (case, name) in absent_plan_names() {
        for (operation, answer) in answers_for(&router, &name).await {
            let case = format!("{case} on {operation} ({name:?})");
            assert_plan_not_found(&case, &answer);
            assert!(
                !member_or_unset(&answer.2, "Message").contains(name.as_str()),
                "case {case:?}: the sentence is fixed text, so it echoes no requested name, got \
                 {:?}",
                answer.2
            );
        }
    }

    // Non-vacuity: the plan this emulator does hold is served, and a served
    // answer names no error shape and carries no token.
    let (status, errortype, read) = get_plan_reported(&router, "plan_held").await;
    assert_eq!(
        (
            status,
            errortype.as_str(),
            error_code(&read).as_str(),
            member_or_unset(&read, "Code").as_str()
        ),
        (200, NO_SHAPE, "<none>", NO_SHAPE),
        "the held plan must be served with no error shape and no token, got {status} \
         {errortype} {read:?}"
    );
}

/// A create naming a plan the account already holds states the already-exists
/// token and the sentence built from the name the REQUEST carried and the account
/// the call was made in — so two different names are two different sentences —
/// while the plan already stored is left exactly as it was.
///
/// The rows differ in the name involved, including both published ends of the
/// length bound, because the sentence is built from the request it answers.
#[tokio::test]
async fn a_duplicate_plan_create_states_the_already_exists_token() {
    let names: Vec<(&str, String)> = vec![
        ("plain name", "plan_duplicated".to_string()),
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(MAX_NAME_LENGTH)),
        ("digits only", "20261121".to_string()),
        ("underscored words", "plan_two_words".to_string()),
    ];

    let mut sentences: Vec<String> = Vec::new();
    for (case, name) in &names {
        let router = router();
        let body = create_body(minimal_plan(name));
        let (status, stored) = create_plan(&router, &body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup: the first create should succeed, got {status} {stored:?}"
        );
        let arn = string_at(&stored, "RestoreTestingPlanArn");

        let (status, errortype, refused) = create_plan_reported(&router, &body).await;
        assert_eq!(
            reported(status, &refused),
            expected(
                400,
                ALREADY_EXISTS_SHAPE,
                PLAN_EXISTS_CODE,
                &already_exists(name)
            ),
            "case {case:?}: expected the already-exists refusal with Code={PLAN_EXISTS_CODE}, got \
             {status} {refused:?}"
        );
        assert_eq!(
            errortype.as_str(),
            ALREADY_EXISTS_SHAPE,
            "case {case:?}: the response must name {ALREADY_EXISTS_SHAPE} to a client, got \
             {errortype}"
        );
        // Two conditions of one status: the duplicate's token is not the token a
        // name holding no plan answers, so a client can tell them apart.
        assert_ne!(
            member_or_unset(&refused, "Code"),
            PLAN_NOT_FOUND_CODE,
            "case {case:?}: the two conditions of this surface must not share one token"
        );
        let sentence = member_or_unset(&refused, "Message");
        assert!(
            sentence.contains(name.as_str()) && sentence.contains(ACCOUNT),
            "case {case:?}: the sentence names the plan asked for and the account the call was \
             made in, got {sentence:?}"
        );
        sentences.push(sentence);

        // The refused create stored nothing and rewrote nothing: the plan the
        // account holds is still the one the first create stored.
        let (status, read) = get_plan(&router, name).await;
        let plan = read
            .get("RestoreTestingPlan")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            (status, string_at(&plan, "RestoreTestingPlanArn")),
            (200, arn),
            "case {case:?}: a refused create must leave the stored plan alone, got {status} \
             {read:?}"
        );
    }

    // One sentence per name: the variable part is the request's, so no two rows
    // can answer the same text.
    let distinct: std::collections::BTreeSet<&String> = sentences.iter().collect();
    assert_eq!(
        distinct.len(),
        sentences.len(),
        "each name must be named in its own refusal, got {sentences:?}"
    );
}

/// A plan a delete took is held by NOTHING afterwards: the read and the update of
/// its name answer the plan's not-found condition — its token and its sentence in
/// the body, and the shape named to the client — where the same read was served
/// before the delete.
///
/// The rows differ in the body the deleted plan was stored from and in its name's
/// form, because the rule is about the record's removal rather than about one
/// plan.
#[tokio::test]
async fn a_deleted_plan_is_refused_by_every_later_call() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "a plan stating no optional member",
            minimal_plan("plan_removed"),
        ),
        (
            "a plan stating both windows and a timezone, at the longest admitted name",
            json!({
                "RestoreTestingPlanName": name_of_length(MAX_NAME_LENGTH),
                "ScheduleExpression": "cron(15 3 ? * MON *)",
                "ScheduleExpressionTimezone": "Europe/Dublin",
                "StartWindowHours": 48,
                "RecoveryPointSelection": {
                    "Algorithm": "RANDOM_WITHIN_WINDOW",
                    "RecoveryPointTypes": ["CONTINUOUS"],
                    "IncludeVaults": ["*"],
                    "SelectionWindowDays": 7,
                },
            }),
        ),
    ];

    for (case, plan) in cases {
        let router = router();
        let name = string_at(&plan, "RestoreTestingPlanName");
        let (status, stored) = create_plan(&router, &create_body(plan)).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup: the create should succeed, got {status} {stored:?}"
        );

        // Before the delete the read is SERVED, which is what makes the refusal
        // below about the delete.
        let (status, errortype, read) = get_plan_reported(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), member_or_unset(&read, "Code")),
            (200, NO_SHAPE, NO_SHAPE.to_string()),
            "case {case:?}: a held plan is served with no token, got {status} {errortype} {read:?}"
        );

        let (status, errortype, deleted) = delete_plan(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), deleted.clone()),
            (204, NO_SHAPE, Value::Null),
            "case {case:?}: expected the modelled empty 204, got {status} {errortype} {deleted:?}"
        );

        for (operation, answer) in answers_for(&router, &name).await {
            assert_plan_not_found(&format!("{case} on {operation} after the delete"), &answer);
        }
    }
}
