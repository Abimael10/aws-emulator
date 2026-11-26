// SPDX-License-Identifier: CC0-1.0

//! `StartSchemaMerge` end to end over HTTP: initiating the merge of a source API's
//! schema into the Merged API it is associated with, and the refusal of a pair that
//! names no such association.
//!
//! Three rules are asserted here, each over a TABLE or a class of inputs rather than over
//! the single request that exposed the defect:
//!
//! 1. initiating a merge on an association the service has FINISHED with answers 200 with
//!    the non-terminal `SourceApiAssociationStatus` it is left in (`MERGE_SCHEDULED`),
//!    which the clock then carries to `MERGE_SUCCESS` with no further call — over a table
//!    spanning the `MergeType` the association was configured with, and asserted twice per
//!    row, so the merge date MOVES rather than being reported before the merge ran;
//! 2. a `(mergedApiIdentifier, associationId)` pair that names no association of that
//!    Merged API is refused 404 `NotFoundException`, and an identifier that cannot name
//!    an API at all 400 `BadRequestException` — over a table spanning an unmintable
//!    identifier, an API this account does not hold, an `associationId` never minted,
//!    one in another form entirely, and a real association addressed under the wrong
//!    Merged API. A refused call schedules nothing;
//! 3. an association whose deletion the clock still owes is being destroyed, so a merge
//!    initiated on it is refused 409 `ConcurrentModificationException` and the deletion
//!    the refusal left alone still comes due.
//!
//! The whole non-terminal window that refusal belongs to — every modification in progress,
//! not only a deletion — is the `schema_merge_concurrency_guard` suite's rule.
//!
//! The harness (`router`, `call`, `create_api`, `create_pair`) is the one the
//! association suites already carry, so every assertion goes through the router the
//! production emulator mounts: routing, restJson1 (de)serialization, the handlers and
//! the shared engine together.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::source_api_association::{
    associate_path, await_terminal_merge, call, create_api, create_pair, read_association, router,
    Answer, MERGE_DEADLINE, POLL_SPACING,
};

/// `StartSchemaMerge`'s path.
///
/// The Merged API label takes the API-id form: an ARN carries `/`, which a non-greedy
/// path label does not match, so this operation can only be addressed by the id form.
fn merge_path(merged_api_identifier: &str, association_id: &str) -> String {
    format!("/v1/mergedApis/{merged_api_identifier}/sourceApiAssociations/{association_id}/merge")
}

/// Every value of the `SourceApiAssociationStatus` enum — the whole set the operation's
/// `sourceApiAssociationStatus` output member can take.
///
/// Source: `smithy_query(query="shape", shape="SourceApiAssociationStatus")`.
const ASSOCIATION_STATUSES: &[&str] = &[
    "MERGE_SCHEDULED",
    "MERGE_FAILED",
    "MERGE_SUCCESS",
    "MERGE_IN_PROGRESS",
    "AUTO_MERGE_SCHEDULE_FAILED",
    "DELETION_SCHEDULED",
    "DELETION_IN_PROGRESS",
    "DELETION_FAILED",
];

/// One row of the merge table: the association the call is made against, and the
/// `MergeType` it was configured with.
#[derive(Debug)]
struct Case {
    /// The row's label, used both in the API names and in every assertion message.
    label: &'static str,
    /// The `sourceApiAssociationConfig` the association was created with, if any.
    config: Option<Value>,
}

/// The merge table: an association configured with no `sourceApiAssociationConfig` at
/// all, and one with each `MergeType` value an association can carry — the merge rule
/// holds whichever of them the association merges under.
///
/// Source for the `MergeType` value set: `smithy_query(query="shape",
/// shape="SourceApiAssociationConfig")` — "Valid merge types are `MANUAL_MERGE`
/// (default) and `AUTO_MERGE`".
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "no config",
            config: None,
        },
        Case {
            label: "manual merge",
            config: Some(json!({"mergeType": "MANUAL_MERGE"})),
        },
        Case {
            label: "auto merge",
            config: Some(json!({"mergeType": "AUTO_MERGE"})),
        },
    ]
}

/// Associate the pair `label` names and return the Merged API id and the association id.
async fn associate(router: &MultiServiceRouter, case: &Case) -> (String, String) {
    let (source, merged) = create_pair(router, case.label).await;
    let mut body = json!({"mergedApiIdentifier": merged.id});
    if let Some(config) = case.config.clone() {
        body["sourceApiAssociationConfig"] = config;
    }
    let created = call(router, "POST", &associate_path(&source.id), Some(body)).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected the association fixture to be created, got {} {:?}",
        case.label, created.status, created.body
    );
    let association_id = created.body["sourceApiAssociation"]["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (merged.id, association_id)
}

/// Initiate the merge of `association_id` under `merged_api_id`.
async fn start_merge(
    router: &MultiServiceRouter,
    merged_api_id: &str,
    association_id: &str,
) -> Answer {
    call(
        router,
        "POST",
        &merge_path(merged_api_id, association_id),
        None,
    )
    .await
}

/// Assert one accepted initiation: HTTP 200 and a `sourceApiAssociationStatus` that is a
/// modelled enum value, and the non-terminal status an initiated merge leaves the
/// association in.
fn assert_scheduled(answer: &Answer, case: &str, when: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case:?} ({when}): expected StartSchemaMerge 200, got {} {:?}",
        answer.status, answer.body
    );
    let status = answer.body["sourceApiAssociationStatus"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ASSOCIATION_STATUSES.contains(&status),
        "case {case:?} ({when}): expected a modelled SourceApiAssociationStatus out of \
         {ASSOCIATION_STATUSES:?}, got {status:?}"
    );
    assert_eq!(
        status,
        state::MERGE_SCHEDULED,
        "case {case:?} ({when}): expected the initiated merge to be scheduled ({:?}), got \
         {status:?}",
        state::MERGE_SCHEDULED
    );
}

#[tokio::test]
async fn a_merge_initiated_on_a_settled_association_is_scheduled_and_performed() {
    let router = router();

    for case in cases() {
        let (merged_api_id, association_id) = associate(&router, &case).await;

        // Creating the association SCHEDULED a merge, which the clock performs with no
        // further call: that is the association a merge can be initiated on.
        let settled =
            await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;
        let date_before = settled["lastSuccessfulMergeDate"].clone();
        assert!(
            date_before.as_f64().is_some_and(|epoch| epoch > 0.0),
            "case {:?}: expected a merge date once the create's merge ran, got {date_before:?}",
            case.label
        );

        // On that association the call schedules a fresh merge and answers the
        // non-terminal status it leaves behind.
        let answer = start_merge(&router, &merged_api_id, &association_id).await;
        assert_scheduled(&answer, case.label, "on the association that merged once");

        // The merge that call left owed is a NEW merge: the clock performs it with no
        // further call, and the date moves past the one the first merge left.
        let after =
            await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;
        let date_after = after["lastSuccessfulMergeDate"].clone();
        assert!(
            date_after.as_f64().is_some_and(|moved| date_before
                .as_f64()
                .is_some_and(|before| moved > before)),
            "case {:?}: expected the merge date to move past {date_before:?}, got {date_after:?}",
            case.label
        );
    }
}

/// `DisassociateSourceGraphqlApi`'s path — the call that schedules the association's
/// deletion, which a merge initiated afterwards has to reckon with.
fn disassociate_path(merged_api_identifier: &str, association_id: &str) -> String {
    format!("/v1/mergedApis/{merged_api_identifier}/sourceApiAssociations/{association_id}")
}

#[tokio::test]
async fn a_merge_on_an_association_being_deleted_is_refused() {
    let router = router();
    let case = Case {
        label: "deleting",
        config: None,
    };
    let (merged_api_id, association_id) = associate(&router, &case).await;

    let deleted = call(
        &router,
        "DELETE",
        &disassociate_path(&merged_api_id, &association_id),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case deleting: expected DisassociateSourceGraphqlApi 200, got {} {:?}",
        deleted.status, deleted.body
    );

    // While the deletion the call scheduled is still owed, the association is being
    // destroyed: that is a modification in progress, so a merge initiated on it is
    // refused with the status the model gives that error and the service's own text for
    // an association that already has an operation in progress.
    let refused = call(
        &router,
        "POST",
        &merge_path(&merged_api_id, &association_id),
        None,
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (409, "ConcurrentModificationException"),
        "case deleting: expected a 409 ConcurrentModificationException while the deletion is \
         owed, got {} {} {:?}",
        refused.status,
        refused.error_type,
        refused.body
    );
    assert_eq!(
        refused.body["message"].as_str(),
        Some(state::ASSOCIATION_OPERATION_IN_PROGRESS_MESSAGE),
        "case deleting: expected the association-scoped concurrent-modification text, got {:?}",
        refused.body["message"]
    );

    // The refusal left the deletion where it was, so the clock still carries it out —
    // and the association the merge could not touch stops existing. A wait on the
    // CONDITION: the merge is initiated again until the association is gone (404) or a
    // generous deadline passes, and while it is still being destroyed the same conflict
    // is the answer.
    let deadline = tokio::time::Instant::now() + MERGE_DEADLINE;
    let gone = loop {
        let answer = start_merge(&router, &merged_api_id, &association_id).await;
        if answer.status != 409 {
            break answer;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case deleted: expected the deletion the clock owes to have been carried out, got \
             {} {} {:?}",
            answer.status,
            answer.error_type,
            answer.body
        );
        tokio::time::sleep(POLL_SPACING).await;
    };
    assert_eq!(
        (gone.status, gone.error_type.as_str()),
        (404, "NotFoundException"),
        "case deleted: expected a 404 NotFoundException once the deletion came due, got {} {} \
         {:?}",
        gone.status,
        gone.error_type,
        gone.body
    );
}

/// One refusal row: the pair to address and the answer it must take.
#[derive(Debug)]
struct Refusal {
    label: &'static str,
    /// `{merged}`, `{other}`, `{graphql}`, `{association}` are substituted with the
    /// fixture's ids before the call.
    merged_api_identifier: &'static str,
    association_id: &'static str,
    status: u16,
    error_type: &'static str,
    /// The text the refusal must carry, written out here rather than read off the
    /// service's own constant, so a change of wording is caught rather than followed.
    /// `None` where the row only pins the status and the error shape.
    message: Option<&'static str>,
}

/// Every way a merge request can name no association: an identifier nothing can name
/// (400), a Merged API this account does not hold (404), an `associationId` that was
/// never minted — in this service's own id form and in another form entirely (404), and
/// a real association addressed under the wrong Merged API, whether that one is another
/// Merged API or a standard one (404).
///
/// The over-long identifier is one character past the 26 an AppSync API id has
/// (`state::API_ID_LEN`), the bound `state::is_api_id` publishes.
fn refusals() -> Vec<Refusal> {
    vec![
        Refusal {
            label: "merged identifier too long to be one",
            merged_api_identifier: "aaaaaaaaaaaaaaaaaaaaaaaaaaa",
            association_id: "{association}",
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            label: "merged api this account does not hold",
            merged_api_identifier: "abcdefghijklmnopqrstuvwxyz",
            association_id: "{association}",
            status: 404,
            error_type: "NotFoundException",
            message: Some("API not found."),
        },
        Refusal {
            label: "association id never minted",
            merged_api_identifier: "{merged}",
            association_id: "abcdefghijklmnopqrstuvwxyz",
            status: 404,
            error_type: "NotFoundException",
            message: Some("No source API association was found"),
        },
        Refusal {
            label: "association id in another form entirely",
            merged_api_identifier: "{merged}",
            association_id: "3f1b9c7e-5a24-4d8f-9b61-0c2e7d84af53",
            status: 404,
            error_type: "NotFoundException",
            message: Some("No source API association was found"),
        },
        Refusal {
            label: "a real association under another merged api",
            merged_api_identifier: "{other}",
            association_id: "{association}",
            status: 404,
            error_type: "NotFoundException",
            message: Some("No source API association was found"),
        },
        Refusal {
            label: "a real association under a standard api",
            merged_api_identifier: "{graphql}",
            association_id: "{association}",
            status: 404,
            error_type: "NotFoundException",
            message: Some("No source API association was found"),
        },
    ]
}

#[tokio::test]
async fn a_pair_naming_no_association_is_refused_and_schedules_nothing() {
    let router = router();
    let case = Case {
        label: "refuse",
        config: None,
    };
    let (merged_api_id, association_id) = associate(&router, &case).await;
    // The fixtures the rows are written against: a second Merged API that holds no
    // association, and a standard API that can hold none at all.
    let other = create_api(&router, "refuse-other-merged", state::MERGED_API_TYPE).await;
    let graphql = create_api(&router, "refuse-standard", state::GRAPHQL_API_TYPE).await;

    // Let the merge the create scheduled come due first, so the association sits in a
    // state a refused call could only change by scheduling something. A wait on the
    // CONDITION, not on a fixed time.
    let before = await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;

    let fill = |template: &str| {
        template
            .replace("{merged}", &merged_api_id)
            .replace("{other}", &other.id)
            .replace("{graphql}", &graphql.id)
            .replace("{association}", &association_id)
    };

    for refusal in refusals() {
        let answer: Answer = call(
            &router,
            "POST",
            &merge_path(
                &fill(refusal.merged_api_identifier),
                &fill(refusal.association_id),
            ),
            None,
        )
        .await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (refusal.status, refusal.error_type),
            "case {:?}: expected {} {}, got {} {} {:?}",
            refusal.label,
            refusal.status,
            refusal.error_type,
            answer.status,
            answer.error_type,
            answer.body
        );
        match refusal.message {
            Some(message) => assert_eq!(
                answer.body["message"].as_str(),
                Some(message),
                "case {:?}: expected the message {message:?}, got {:?}",
                refusal.label,
                answer.body["message"]
            ),
            None => assert!(
                answer.body["message"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "case {:?}: expected a message stating why, got {:?}",
                refusal.label,
                answer.body["message"]
            ),
        }
    }

    // A refused call scheduled nothing: the association is where the clock left it,
    // merge date and all.
    let after = read_association(&router, case.label, &merged_api_id, &association_id).await;
    assert_eq!(
        after, before,
        "case refused writes nothing: expected the association to be untouched ({before:?}), got \
         {after:?}"
    );
}
