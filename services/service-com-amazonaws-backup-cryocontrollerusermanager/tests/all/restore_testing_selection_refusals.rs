// SPDX-License-Identifier: CC0-1.0

//! The REFUSALS of the AWS Backup restore-testing-SELECTION surface, and what a
//! delete of a selection leaves behind.
//!
//! The rules under test:
//!
//! * a refusal names its CONDITION: the modelled exception carries that
//!   condition's token in `Code` and the sentence AWS states for it, so the
//!   token for "no such plan" is not the token for "no such selection of this
//!   plan", and the duplicate-selection token is the selection's own;
//! * the resource a request addresses in its PATH is resolved BEFORE the body it
//!   carries is read, so a call naming a plan the account does not hold is
//!   refused that plan's not-found condition whatever the body says — while the
//!   same body under a plan the account does hold is refused on its own terms;
//! * a `DeleteRestoreTestingSelection` takes the selection out of the store, so
//!   every later READ of the pair takes the selection's not-found path, the name
//!   is free for a create again, and nothing else the plan holds is touched —
//!   while the delete itself asks for a state, so repeating it answers the same
//!   empty success.
//!
//! Every case drives real HTTP through the router, so what is asserted is the
//! body a `backup` SDK client parses; the harness is the shared one in
//! `vault_and_backup_job.rs` and the selection routes are the sibling suite's
//! (`restore_testing_selection.rs`).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::restore_testing_selection::{
    create_body, create_selection, get_selection, minimal_plan, minimal_selection, name_of_length,
    read_selection, role_arn, stored_plan,
};
use crate::vault_and_backup_job::{call_reported, error_code, router, string_at, ACCOUNT};

/// The token and the sentence this service states for "the restore testing plan
/// addressed is not one this account holds" — the condition every call that
/// names a `RestoreTestingPlanName` answers.
const PLAN_NOT_FOUND_CODE: &str = "ERROR_RT_5000";
const PLAN_NOT_FOUND_MESSAGE: &str = "Restore testing plan does not exist.";

/// The token and the sentence for "the restore testing selection addressed is
/// not one the named plan holds" — a DIFFERENT condition from the one above, so
/// a different token.
const SELECTION_NOT_FOUND_CODE: &str = "ERROR_RT_5001";
const SELECTION_NOT_FOUND_MESSAGE: &str = "Restore testing selection does not exist.";

/// The token for "the named plan already holds a selection of this name".
const SELECTION_EXISTS_CODE: &str = "ERROR_RT_3001";

/// The token for "this account already holds a restore testing plan of this
/// name" — the PLAN's create conflict, which shares the modelled class and
/// status with the selection's and so needs its own token.
const PLAN_EXISTS_CODE: &str = "ERROR_RT_3000";

/// The modelled class each condition belongs to.
const NOT_FOUND: &str = "ResourceNotFoundException";
const ALREADY_EXISTS: &str = "AlreadyExistsException";
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
const MISSING_PARAMETER: &str = "MissingParameterValueException";
const LIMIT_EXCEEDED: &str = "LimitExceededException";

/// The most specific protected resource ARNs one selection may name: "you can
/// include up to 30 specific protected resource ARNs in ProtectedResourceArns"
/// (model doc for `CreateRestoreTestingSelection`).
const MAX_PROTECTED_RESOURCE_ARNS: usize = 30;

/// The published ceiling of a validation window: "This is amount of hours (0 to
/// 168) available to run a validation script on the data" (model doc for
/// `RestoreTestingSelectionForCreate$ValidationWindowHours`).
const MAX_VALIDATION_WINDOW_HOURS: i64 = 168;

/// The sentence this service states for a create naming a selection the plan
/// already holds: fixed text around the plan name and the selection name the
/// REQUEST carried and the account the call was made in.
fn selection_exists_message(plan: &str, selection: &str) -> String {
    format!(
        "Restore testing plan [{plan}] in account [{ACCOUNT}] already contains selection \
         [{selection}]."
    )
}

/// The sentence this service states for a create naming a plan the account
/// already holds: fixed text around the name the REQUEST carried and the account
/// the call was made in.
fn plan_exists_message(plan: &str) -> String {
    format!("Restore testing plan [{plan}] already exists in account [{ACCOUNT}].")
}

/// A body member as a string, or `"<unset>"` when the member is absent — the
/// distinction between "states the condition's token" and "states nothing".
fn member_or_unset(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The (status, `__type`, `Code`, `Message`) a refusal reports, as one tuple so
/// an assertion names all four at once.
fn reported(status: u16, body: &Value) -> (u16, String, String, String) {
    (
        status,
        error_code(body),
        member_or_unset(body, "Code"),
        member_or_unset(body, "Message"),
    )
}

/// The expected tuple for a refusal class.
fn expected(status: u16, shape: &str, code: &str, message: &str) -> (u16, String, String, String) {
    (
        status,
        shape.to_string(),
        code.to_string(),
        message.to_string(),
    )
}

/// `DELETE /restore-testing/plans/{RestoreTestingPlanName}/selections/{RestoreTestingSelectionName}`,
/// as the model binds the delete.
async fn delete_selection(
    router: &MultiServiceRouter,
    plan: &str,
    name: &str,
) -> (u16, String, Value) {
    call_reported(
        router,
        "DELETE",
        &format!("/restore-testing/plans/{plan}/selections/{name}"),
        None,
    )
    .await
}

/// Plan names the account holds nothing under, spanning the forms the documented
/// alphabet admits — both published ends of the length bound, digits,
/// underscored words and a case variant of a name that IS held.
fn absent_plan_names() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(50)),
        ("digits only", "20261119".to_string()),
        ("underscored words", "plan_never_created".to_string()),
        ("a case variant of a held name", "PLAN_HELD".to_string()),
    ]
}

/// Every call that addresses a restore testing selection under a plan the
/// account holds nothing under reports the PLAN's not-found token, and the plan
/// the same emulator does hold is served instead.
#[tokio::test]
async fn a_call_under_an_unknown_plan_reports_the_plan_not_found_token() {
    let router = router();
    stored_plan(&router, "plan_held").await;
    let (status, served) =
        create_selection(&router, "plan_held", &create_body(minimal_selection("held"))).await;
    assert_eq!(
        status, 201,
        "setup: the create under the held plan should succeed, got {status} {served:?}"
    );

    for (case, plan) in absent_plan_names() {
        let answers = [
            (
                "CreateRestoreTestingSelection",
                {
                    let (status, body) =
                        create_selection(&router, &plan, &create_body(minimal_selection("held")))
                            .await;
                    (status, String::new(), body)
                },
            ),
            ("GetRestoreTestingSelection", {
                let (status, errortype, body) = call_reported(
                    &router,
                    "GET",
                    &format!("/restore-testing/plans/{plan}/selections/held"),
                    None,
                )
                .await;
                (status, errortype, body)
            }),
            (
                "DeleteRestoreTestingSelection",
                delete_selection(&router, &plan, "held").await,
            ),
        ];
        for (operation, (status, _errortype, refused)) in answers {
            assert_eq!(
                reported(status, &refused),
                expected(400, NOT_FOUND, PLAN_NOT_FOUND_CODE, PLAN_NOT_FOUND_MESSAGE),
                "case {case:?} on {operation} ({plan:?}): expected the plan's not-found refusal \
                 with Code={PLAN_NOT_FOUND_CODE}, got {status} {refused:?}"
            );
            assert!(
                !member_or_unset(&refused, "Message").contains(plan.as_str()),
                "case {case:?} on {operation}: the sentence is fixed text, so it echoes no \
                 requested name, got {refused:?}"
            );
        }

        // The error shape a client resolves from the response headers names the
        // same class as the body.
        let (status, errortype, refused) = call_reported(
            &router,
            "GET",
            &format!("/restore-testing/plans/{plan}/selections/held"),
            None,
        )
        .await;
        assert_eq!(
            (status, errortype.as_str()),
            (400, NOT_FOUND),
            "case {case:?}: expected the response to name {NOT_FOUND}, got {status} {refused:?}"
        );
    }

    // Non-vacuity: the plan this emulator does hold serves all three calls, and
    // a served answer carries no `Code` at all.
    let (status, read) = get_selection(&router, "plan_held", "held").await;
    assert_eq!(
        (status, member_or_unset(&read, "Code")),
        (200, "<unset>".to_string()),
        "case \"the held plan\" on GetRestoreTestingSelection: expected it served with no Code, \
         got {status} {read:?}"
    );
    let (status, _, deleted) = delete_selection(&router, "plan_held", "held").await;
    assert_eq!(
        (status, member_or_unset(&deleted, "Code")),
        (204, "<unset>".to_string()),
        "case \"the held plan\" on DeleteRestoreTestingSelection: expected the modelled 204, got \
         {status} {deleted:?}"
    );
}

/// A read of a selection name the HELD plan does not hold reports the
/// SELECTION's own token — a different condition from "no such plan", so a
/// different token and a different sentence.
///
/// The rows span the forms the documented alphabet admits, each asked of a plan
/// the account does hold, and the selection the same plan does hold is served,
/// so the token follows the addressed name rather than the route.
#[tokio::test]
async fn an_unknown_selection_of_a_held_plan_reports_the_selection_not_found_token() {
    let router = router();
    stored_plan(&router, "plan_held").await;
    let (status, created) = create_selection(
        &router,
        "plan_held",
        &create_body(minimal_selection("selection_held")),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: the create should succeed, got {status} {created:?}"
    );

    let absent: Vec<(&str, String)> = vec![
        ("the shortest admitted name", name_of_length(1)),
        ("the longest admitted name", name_of_length(50)),
        ("digits only", "20261119".to_string()),
        ("underscored words", "selection_never_created".to_string()),
        (
            "a case variant of a held name",
            "SELECTION_HELD".to_string(),
        ),
    ];
    for (case, name) in absent {
        let (status, errortype, refused) = call_reported(
            &router,
            "GET",
            &format!("/restore-testing/plans/plan_held/selections/{name}"),
            None,
        )
        .await;
        assert_eq!(
            (reported(status, &refused), errortype.as_str()),
            (
                expected(
                    400,
                    NOT_FOUND,
                    SELECTION_NOT_FOUND_CODE,
                    SELECTION_NOT_FOUND_MESSAGE
                ),
                NOT_FOUND
            ),
            "case {case:?} ({name:?}): expected the selection's not-found refusal with \
             Code={SELECTION_NOT_FOUND_CODE}, got {status} {refused:?}"
        );
        assert!(
            !member_or_unset(&refused, "Message").contains(name.as_str()),
            "case {case:?}: the sentence is fixed text, so it echoes no requested name, got \
             {refused:?}"
        );
    }

    // Non-vacuity: the selection the plan does hold is served, with no `Code`.
    let (status, read) = get_selection(&router, "plan_held", "selection_held").await;
    assert_eq!(
        (status, member_or_unset(&read, "Code")),
        (200, "<unset>".to_string()),
        "case \"the held selection\": expected it served with no Code, got {status} {read:?}"
    );
}

/// One row of the ordering table: the case, the selection body, and the class
/// that body is refused with once the plan it names IS resolved.
type BodyRow = (&'static str, Value, &'static str);

/// A body this service cannot store, under a plan the account does not hold, is
/// refused the PLAN's condition: the resource the path addresses is resolved
/// before the body is read.
///
/// The rows span the body refusals of this operation — a member outside the
/// documented rules, a body selecting nothing, a body past the documented
/// maximum — because the ordering rule has to hold for every one of them; each
/// row is then sent under a plan that IS held, where it answers on its own
/// terms, so the plan's token is the ORDER's answer and not a blanket refusal.
#[tokio::test]
async fn an_unknown_plan_is_refused_before_the_body_is_read() {
    let rows: Vec<BodyRow> = vec![
        (
            "a body that selects no protected resource",
            json!({
                "RestoreTestingSelectionName": "selection_ordering",
                "IamRoleArn": role_arn("AWSBackupRestoreTesting"),
                "ProtectedResourceType": "EBS",
            }),
            MISSING_PARAMETER,
        ),
        (
            "a name outside the documented alphabet",
            json!({
                "RestoreTestingSelectionName": "selection ordering!",
                "IamRoleArn": role_arn("AWSBackupRestoreTesting"),
                "ProtectedResourceType": "EBS",
                "ProtectedResourceArns": ["*"],
            }),
            INVALID_PARAMETER,
        ),
        (
            "a role that is no ARN",
            json!({
                "RestoreTestingSelectionName": "selection_ordering",
                "IamRoleArn": "AWSBackupRestoreTesting",
                "ProtectedResourceType": "EBS",
                "ProtectedResourceArns": ["*"],
            }),
            INVALID_PARAMETER,
        ),
        (
            "a validation window one hour past its ceiling",
            json!({
                "RestoreTestingSelectionName": "selection_ordering",
                "IamRoleArn": role_arn("AWSBackupRestoreTesting"),
                "ProtectedResourceType": "EBS",
                "ProtectedResourceArns": ["*"],
                "ValidationWindowHours": MAX_VALIDATION_WINDOW_HOURS + 1,
            }),
            INVALID_PARAMETER,
        ),
        (
            "one specific ARN more than the documented maximum",
            json!({
                "RestoreTestingSelectionName": "selection_ordering",
                "IamRoleArn": role_arn("AWSBackupRestoreTesting"),
                "ProtectedResourceType": "EBS",
                "ProtectedResourceArns": (0..=MAX_PROTECTED_RESOURCE_ARNS)
                    .map(|index| format!("arn:aws:ec2:us-west-2:{ACCOUNT}:volume/vol-{index:017x}"))
                    .collect::<Vec<String>>(),
            }),
            LIMIT_EXCEEDED,
        ),
    ];

    for (case, selection, own_class) in rows {
        let router = router();
        stored_plan(&router, "plan_ordering").await;

        let (status, refused) =
            create_selection(&router, "plan_absent", &create_body(selection.clone())).await;
        assert_eq!(
            reported(status, &refused),
            expected(400, NOT_FOUND, PLAN_NOT_FOUND_CODE, PLAN_NOT_FOUND_MESSAGE),
            "case {case:?}: under an unknown plan the plan's absence is the refusal, got {status} \
             {refused:?}"
        );

        let (status, refused) =
            create_selection(&router, "plan_ordering", &create_body(selection)).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, own_class.to_string()),
            "case {case:?}: under a held plan the body answers on its own terms, got {status} \
             {refused:?}"
        );
    }
}

/// One row of the duplicate table: the case, the plan the selection belongs to,
/// its name and the body the FIRST create carried.
type DuplicateRow = (&'static str, String, String, Value);

/// A create naming a selection the plan already holds reports the
/// already-exists token and AWS's sentence for it — the plan and the selection
/// the request named, and the account the call was made in.
///
/// The rows differ in the names involved and in what the first create carried,
/// because the sentence is built from the REQUEST and must follow it rather than
/// be one fixed string; another name, and the same name under another plan, are
/// still stored, so what is refused is the collision of the pair.
#[tokio::test]
async fn a_duplicate_selection_create_reports_the_already_exists_token() {
    let rows: Vec<DuplicateRow> = vec![
        (
            "a bare create",
            "plan_duplicate".to_string(),
            "selection_duplicate".to_string(),
            minimal_selection("selection_duplicate"),
        ),
        (
            "the shortest admitted names",
            name_of_length(1),
            name_of_length(1),
            minimal_selection(&name_of_length(1)),
        ),
        (
            "the longest admitted names",
            name_of_length(50),
            name_of_length(50),
            minimal_selection(&name_of_length(50)),
        ),
        (
            "a first create carrying a creator token and conditions",
            "plan_duplicate_conditions".to_string(),
            "selection_by_tag".to_string(),
            json!({
                "RestoreTestingSelectionName": "selection_by_tag",
                "IamRoleArn": role_arn("RestoreTestingByTag"),
                "ProtectedResourceType": "EFS",
                "ProtectedResourceConditions": {
                    "StringEquals": [{ "Key": "team", "Value": "backup" }],
                },
            }),
        ),
    ];

    for (case, plan, selection_name, first) in rows {
        let router = router();
        stored_plan(&router, &plan).await;
        stored_plan(&router, "plan_other").await;

        let mut body = create_body(first);
        body["CreatorRequestId"] = json!("the-first-call");
        let (status, created) = create_selection(&router, &plan, &body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the first create should succeed, got {status} {created:?}"
        );

        // A second create of the same pair carrying ANOTHER token is no retry of
        // the first, so it collides.
        let mut second = create_body(minimal_selection(&selection_name));
        second["CreatorRequestId"] = json!("the-second-call");
        let (status, refused) = create_selection(&router, &plan, &second).await;
        assert_eq!(
            reported(status, &refused),
            expected(
                400,
                ALREADY_EXISTS,
                SELECTION_EXISTS_CODE,
                &selection_exists_message(&plan, &selection_name)
            ),
            "case {case:?}: expected the already-exists refusal with Code={SELECTION_EXISTS_CODE} \
             naming the plan, the account and the selection, got {status} {refused:?}"
        );
        assert_eq!(
            (
                member_or_unset(&refused, "Arn"),
                member_or_unset(&refused, "CreatorRequestId")
            ),
            ("<unset>".to_string(), "<unset>".to_string()),
            "case {case:?}: the refusal discloses neither Arn nor CreatorRequestId of the stored \
             selection, got {refused:?}"
        );

        // Non-vacuity: another name under the same plan, and the same name under
        // another plan, are both stored.
        let (status, accepted) = create_selection(
            &router,
            &plan,
            &create_body(minimal_selection("selection_other_name")),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: another name under the same plan is another selection, got {status} \
             {accepted:?}"
        );
        let (status, accepted) = create_selection(
            &router,
            "plan_other",
            &create_body(minimal_selection(&selection_name)),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: the same name under another plan is another selection, got {status} \
             {accepted:?}"
        );
    }
}

/// The duplicate-PLAN condition and the duplicate-SELECTION condition are TWO
/// conditions: they share the modelled class and status, so each states its own
/// token and its own sentence and a client can tell them apart.
///
/// The rows differ in the names involved, because each sentence is built from
/// the request it answers.
#[tokio::test]
async fn a_duplicate_plan_and_a_duplicate_selection_state_different_tokens() {
    for (case, plan, selection) in [
        ("plain names", "plan_tokens".to_string(), "sel".to_string()),
        (
            "the longest admitted names",
            name_of_length(50),
            name_of_length(50),
        ),
    ] {
        let router = router();
        stored_plan(&router, &plan).await;
        let (status, created) =
            create_selection(&router, &plan, &create_body(minimal_selection(&selection))).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup create should succeed, got {status} {created:?}"
        );

        let (status, _, plan_refused) = call_reported(
            &router,
            "PUT",
            "/restore-testing/plans",
            Some(&minimal_plan(&plan).to_string()),
        )
        .await;
        assert_eq!(
            reported(status, &plan_refused),
            expected(
                400,
                ALREADY_EXISTS,
                PLAN_EXISTS_CODE,
                &plan_exists_message(&plan)
            ),
            "case {case:?}: a duplicate plan create states the PLAN's token, got {status} \
             {plan_refused:?}"
        );

        let (status, selection_refused) =
            create_selection(&router, &plan, &create_body(minimal_selection(&selection))).await;
        assert_eq!(
            reported(status, &selection_refused),
            expected(
                400,
                ALREADY_EXISTS,
                SELECTION_EXISTS_CODE,
                &selection_exists_message(&plan, &selection)
            ),
            "case {case:?}: a duplicate selection create states the SELECTION's token, got \
             {status} {selection_refused:?}"
        );
        assert_ne!(
            member_or_unset(&plan_refused, "Code"),
            member_or_unset(&selection_refused, "Code"),
            "case {case:?}: two conditions of one class must not share one token"
        );
    }
}

/// A deleted selection is GONE: the read and a second delete of that pair take
/// the selection's not-found path, the name is free for a create again, and the
/// sibling selection the plan also holds is untouched.
///
/// The rows differ along the form of the selection deleted — how it selects its
/// resources, its protected resource type, its name's length — because the rule
/// is about the record's removal and not about one body.
#[tokio::test]
async fn a_deleted_selection_is_gone_from_every_later_call() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "a wildcard selection",
            minimal_selection("selection_deleted"),
        ),
        (
            "a selection of specific ARNs, at the longest admitted name",
            json!({
                "RestoreTestingSelectionName": name_of_length(50),
                "IamRoleArn": role_arn("AWSBackupDefaultServiceRole"),
                "ProtectedResourceType": "DynamoDB",
                "ProtectedResourceArns": [
                    format!("arn:aws:dynamodb:us-west-2:{ACCOUNT}:table/demo"),
                ],
            }),
        ),
        (
            "a selection by conditions, with metadata overrides",
            json!({
                "RestoreTestingSelectionName": name_of_length(1),
                "IamRoleArn": role_arn("RestoreTesting"),
                "ProtectedResourceType": "EC2",
                "ProtectedResourceArns": ["*"],
                "ProtectedResourceConditions": {
                    "StringEquals": [{ "Key": "stage", "Value": "test" }],
                },
                "RestoreMetadataOverrides": { "instanceType": "t3.micro" },
                "ValidationWindowHours": MAX_VALIDATION_WINDOW_HOURS,
            }),
        ),
    ];

    for (case, selection) in cases {
        let router = router();
        stored_plan(&router, "plan_teardown").await;
        let name = string_at(&selection, "RestoreTestingSelectionName");

        let (status, created) =
            create_selection(&router, "plan_teardown", &create_body(selection)).await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup create should succeed, got {status} {created:?}"
        );
        let (status, sibling) = create_selection(
            &router,
            "plan_teardown",
            &create_body(minimal_selection("selection_kept")),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: setup of the sibling selection should succeed, got {status} \
             {sibling:?}"
        );

        let (status, errortype, deleted) = delete_selection(&router, "plan_teardown", &name).await;
        assert_eq!(
            (status, errortype.as_str()),
            (204, "<unset>"),
            "case {case:?}: the delete answers the modelled 204, got {status} {deleted:?}"
        );

        let (status, errortype, read) = call_reported(
            &router,
            "GET",
            &format!("/restore-testing/plans/plan_teardown/selections/{name}"),
            None,
        )
        .await;
        assert_eq!(
            (reported(status, &read), errortype.as_str()),
            (
                expected(
                    400,
                    NOT_FOUND,
                    SELECTION_NOT_FOUND_CODE,
                    SELECTION_NOT_FOUND_MESSAGE
                ),
                NOT_FOUND
            ),
            "case {case:?}: a deleted selection reads back as not found with \
             Code={SELECTION_NOT_FOUND_CODE}, got {status} {read:?}"
        );

        let (status, _, again) = delete_selection(&router, "plan_teardown", &name).await;
        assert_eq!(
            (status, member_or_unset(&again, "Code")),
            (204, "<unset>".to_string()),
            "case {case:?}: a second delete of the pair asks for the state that already holds, so \
             it answers the same empty success, got {status} {again:?}"
        );

        // The sibling the plan also holds is untouched by a delete addressed to
        // another name.
        let (status, kept) = get_selection(&router, "plan_teardown", "selection_kept").await;
        assert_eq!(
            (status, member_or_unset(&kept, "Code")),
            (200, "<unset>".to_string()),
            "case {case:?}: the delete removes only the pair it addressed, got {status} {kept:?}"
        );

        // The name is free again, and what the new create stored is what the
        // read reports.
        let mut recreated = minimal_selection(&name);
        recreated["ProtectedResourceType"] = json!("S3");
        let (status, stored) =
            create_selection(&router, "plan_teardown", &create_body(recreated)).await;
        assert_eq!(
            status, 201,
            "case {case:?}: the deleted name is free for a create again, got {status} {stored:?}"
        );
        let (status, read) = get_selection(&router, "plan_teardown", &name).await;
        assert_eq!(
            (
                status,
                string_at(&read_selection(&read), "ProtectedResourceType")
            ),
            (200, "S3".to_string()),
            "case {case:?}: the re-created selection is the one the read reports, got {status} \
             {read:?}"
        );
    }
}
