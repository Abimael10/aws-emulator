// SPDX-License-Identifier: CC0-1.0

//! `StartSchemaMerge`'s concurrent-modification guard, end to end over HTTP.
//!
//! One rule is asserted here, over a TABLE spanning the non-terminal windows an
//! association can be in rather than the single request that exposed the defect:
//! initiating a merge on a source-API association the service has not finished with —
//! whose `sourceApiAssociationStatus` is not one of the terminal values
//! (`MERGE_SUCCESS`, `MERGE_FAILED`, `AUTO_MERGE_SCHEDULE_FAILED`) — is refused 409
//! `ConcurrentModificationException` with the service's fixed text, for the whole of that
//! window and whichever modification opened it, and only an association in a terminal
//! status admits a new merge.
//!
//! The rows differ along what the rule generalises over:
//!
//! * which modification is in progress — the merge the create scheduled, the merge an
//!   accepted initiation scheduled, and the deletion a disassociate scheduled;
//! * the `MergeType` the association merges under, including the modelled default an
//!   association created with no `sourceApiAssociationConfig` carries.
//!
//! Each row also asserts the guard is not a dead end: the refusal leaves the owed work
//! exactly where it was, so the clock still carries it to a terminal status with no
//! further call, and the merge initiated once it is terminal is accepted.
//!
//! Nothing here waits for a fixed time: every wait is a poll on the CONDITION the
//! association reports, bounded by the suite's generous deadline, so a loaded machine and
//! a fast one reach the same assertions. The harness (`router`, `call`, `create_pair`,
//! `await_terminal_merge`) is the one the association suites already carry, so every
//! assertion goes through the router the production emulator mounts.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::source_api_association::{
    associate_path, await_terminal_merge, call, create_pair, get_path, read_association, router,
    Answer, MERGE_DEADLINE, POLL_SPACING, TERMINAL_MERGE_STATUSES,
};

/// The TEXT the refusal carries, written out here rather than read off the service's own
/// constant, so a change of wording is caught rather than followed.
const IN_PROGRESS_MESSAGE: &str =
    "This association currently has an operation in progress, please wait until that is complete";

/// `StartSchemaMerge`'s path.
///
/// The Merged API label takes the API-id form: an ARN carries `/`, which a non-greedy
/// path label does not match.
fn merge_path(merged_api_identifier: &str, association_id: &str) -> String {
    format!("/v1/mergedApis/{merged_api_identifier}/sourceApiAssociations/{association_id}/merge")
}

/// Which modification a row has in progress when it initiates its merge.
#[derive(Debug, Clone, Copy)]
enum Pending {
    /// The merge creating the association scheduled.
    MergeFromTheCreate,
    /// The merge an accepted `StartSchemaMerge` scheduled, on an association that had
    /// already settled.
    MergeFromAnInitiation,
    /// The deletion a `DisassociateSourceGraphqlApi` scheduled.
    Deletion,
}

/// One row: the modification in progress, and the `MergeType` the association merges
/// under.
#[derive(Debug)]
struct Case {
    /// The row's label, used both in the API names and in every assertion message.
    label: &'static str,
    pending: Pending,
    /// The `mergeType` the create's `sourceApiAssociationConfig` asked for, or `None`
    /// when it asked for no config at all (the modelled default then applies).
    merge_type: Option<&'static str>,
}

/// The table: every modification that can be in progress, across the whole `MergeType`
/// value set and the association created with no config at all.
///
/// Source for the `MergeType` value set: `smithy_query(query="shape",
/// shape="SourceApiAssociationConfig")` — "Valid merge types are `MANUAL_MERGE`
/// (default) and `AUTO_MERGE`".
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the create's merge, no config",
            pending: Pending::MergeFromTheCreate,
            merge_type: None,
        },
        Case {
            label: "the create's merge, manual",
            pending: Pending::MergeFromTheCreate,
            merge_type: Some("MANUAL_MERGE"),
        },
        Case {
            label: "the create's merge, auto",
            pending: Pending::MergeFromTheCreate,
            merge_type: Some("AUTO_MERGE"),
        },
        Case {
            label: "an initiated merge, no config",
            pending: Pending::MergeFromAnInitiation,
            merge_type: None,
        },
        Case {
            label: "an initiated merge, auto",
            pending: Pending::MergeFromAnInitiation,
            merge_type: Some("AUTO_MERGE"),
        },
        Case {
            label: "a scheduled deletion, manual",
            pending: Pending::Deletion,
            merge_type: Some("MANUAL_MERGE"),
        },
        Case {
            label: "a scheduled deletion, auto",
            pending: Pending::Deletion,
            merge_type: Some("AUTO_MERGE"),
        },
    ]
}

/// Associate the pair this row needs and return the Merged API id and the association id.
async fn associate(router: &MultiServiceRouter, case: &Case) -> (String, String) {
    let (source, merged) = create_pair(router, case.label).await;
    let mut body = json!({"mergedApiIdentifier": merged.id});
    if let Some(merge_type) = case.merge_type {
        body["sourceApiAssociationConfig"] = json!({"mergeType": merge_type});
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

/// Assert one refused initiation: the status, the error shape and the fixed text the
/// model and the service give a modification already in progress.
fn assert_refused(answer: &Answer, case: &str, when: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (409, "ConcurrentModificationException"),
        "case {case:?} ({when}): expected a 409 ConcurrentModificationException, got {} {} {:?}",
        answer.status,
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(IN_PROGRESS_MESSAGE),
        "case {case:?} ({when}): expected the message {IN_PROGRESS_MESSAGE:?}, got {:?}",
        answer.body["message"]
    );
    assert!(
        answer.body["sourceApiAssociationStatus"].is_null(),
        "case {case:?} ({when}): expected a refusal to report no status, got {:?}",
        answer.body
    );
}

/// Assert one accepted initiation: 200 and the non-terminal status an initiated merge
/// leaves the association in.
fn assert_accepted(answer: &Answer, case: &str, when: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case:?} ({when}): expected StartSchemaMerge 200, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["sourceApiAssociationStatus"].as_str(),
        Some(state::MERGE_SCHEDULED),
        "case {case:?} ({when}): expected the initiated merge to be scheduled ({:?}), got {:?}",
        state::MERGE_SCHEDULED,
        answer.body["sourceApiAssociationStatus"]
    );
}

/// Open this row's non-terminal window and answer the status the association reports
/// inside it.
async fn open_the_window(
    router: &MultiServiceRouter,
    case: &Case,
    merged_api_id: &str,
    association_id: &str,
) {
    match case.pending {
        // Creating the association already scheduled a merge, so the window is open.
        Pending::MergeFromTheCreate => {}
        Pending::MergeFromAnInitiation => {
            await_terminal_merge(router, case.label, merged_api_id, association_id).await;
            let accepted = start_merge(router, merged_api_id, association_id).await;
            assert_accepted(&accepted, case.label, "on the association that had settled");
        }
        Pending::Deletion => {
            await_terminal_merge(router, case.label, merged_api_id, association_id).await;
            let deleted = call(
                router,
                "DELETE",
                &get_path(merged_api_id, association_id),
                None,
            )
            .await;
            assert_eq!(
                deleted.status, 200,
                "case {:?}: expected DisassociateSourceGraphqlApi 200, got {} {:?}",
                case.label, deleted.status, deleted.body
            );
        }
    }
}

#[tokio::test]
async fn a_merge_initiated_on_a_non_terminal_association_is_refused() {
    let router = router();

    for case in cases() {
        let (merged_api_id, association_id) = associate(&router, &case).await;
        open_the_window(&router, &case, &merged_api_id, &association_id).await;

        // The modification the row opened is in progress, so this initiation is refused:
        // the call is made straight after the one that opened the window, while the
        // service still owes the work that window is made of.
        let refused = start_merge(&router, &merged_api_id, &association_id).await;
        assert_refused(&refused, case.label, "while the modification is in progress");

        match case.pending {
            Pending::Deletion => {
                // The refusal left the deletion where it was, so the clock still carries
                // it out and the association stops existing — reported as the absent
                // association it then is. A wait on the CONDITION: look again until the
                // merge path answers something other than the conflict.
                let deadline = tokio::time::Instant::now() + MERGE_DEADLINE;
                let gone = loop {
                    let answer = start_merge(&router, &merged_api_id, &association_id).await;
                    if answer.status != 409 {
                        break answer;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "case {:?}: expected the deletion the clock owes to have been carried \
                         out, got {} {} {:?}",
                        case.label,
                        answer.status,
                        answer.error_type,
                        answer.body
                    );
                    tokio::time::sleep(POLL_SPACING).await;
                };
                assert_eq!(
                    (gone.status, gone.error_type.as_str()),
                    (404, "NotFoundException"),
                    "case {:?}: expected a 404 NotFoundException once the deletion came due, got \
                     {} {} {:?}",
                    case.label,
                    gone.status,
                    gone.error_type,
                    gone.body
                );
            }
            _ => {
                // The refusal left the merge where it was, so the clock still performs it
                // with no further call, and the guard it cleared admits the next
                // initiation.
                let settled =
                    await_terminal_merge(&router, case.label, &merged_api_id, &association_id)
                        .await;
                assert_eq!(
                    settled["sourceApiAssociationStatus"].as_str(),
                    Some(state::MERGE_SUCCESS),
                    "case {:?}: expected the owed merge to have been performed, got {settled:?}",
                    case.label
                );
                let accepted = start_merge(&router, &merged_api_id, &association_id).await;
                assert_accepted(&accepted, case.label, "once the association is terminal");
                // And that acceptance opened the window again: the very next initiation
                // meets the merge it scheduled.
                let again = start_merge(&router, &merged_api_id, &association_id).await;
                assert_refused(&again, case.label, "while the accepted merge is owed");
            }
        }
    }
}

/// The guard is keyed on the STATUS the association reports, not on which call asked:
/// every initiation made while `GetSourceApiAssociation` reports a non-terminal status is
/// refused, and the first one made after it reports a terminal one is accepted.
///
/// A poll on the CONDITION, bounded by the suite's deadline: the loop reads the
/// association and initiates a merge until the read reports a terminal status.
#[tokio::test]
async fn every_initiation_inside_the_non_terminal_window_is_refused() {
    let router = router();
    let case = Case {
        label: "the whole window",
        pending: Pending::MergeFromTheCreate,
        merge_type: None,
    };
    let (merged_api_id, association_id) = associate(&router, &case).await;

    let deadline = tokio::time::Instant::now() + MERGE_DEADLINE;
    let mut refusals = 0_usize;
    loop {
        let association =
            read_association(&router, case.label, &merged_api_id, &association_id).await;
        let status = association["sourceApiAssociationStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_MERGE_STATUSES.contains(&status.as_str()) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {:?}: expected the merge the clock owes to have been performed, got \
             {association:?}",
            case.label
        );
        let answer = start_merge(&router, &merged_api_id, &association_id).await;
        // The window may have closed between the read and this call, which is the clock's
        // own doing: an accepted initiation is only correct when the association had
        // reached a terminal status, and it opens a new window the next round refuses.
        if answer.status == 200 {
            assert_accepted(&answer, case.label, "the window closed before the initiation");
            continue;
        }
        assert_refused(&answer, case.label, "inside the non-terminal window");
        refusals += 1;
        tokio::time::sleep(POLL_SPACING).await;
    }
    assert!(
        refusals > 0,
        "case {:?}: expected at least one initiation to be refused inside the window the create \
         opened, got {refusals}",
        case.label
    );

    // Past the window the guard is clear, and the merge that admits is a REAL one: the
    // clock performs it and the association reports a merge date for it.
    let accepted = start_merge(&router, &merged_api_id, &association_id).await;
    assert_accepted(&accepted, case.label, "past the window");
    let settled = await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;
    assert_eq!(
        settled["sourceApiAssociationStatus"].as_str(),
        Some(state::MERGE_SUCCESS),
        "case {:?}: expected the accepted merge to have been performed, got {settled:?}",
        case.label
    );
    let date: Value = settled["lastSuccessfulMergeDate"].clone();
    assert!(
        date.as_f64().is_some_and(|epoch| epoch > 0.0),
        "case {:?}: expected a merge date once the accepted merge ran, got {date:?}",
        case.label
    );
}
