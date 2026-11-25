// SPDX-License-Identifier: CC0-1.0

//! `UpdateWorkflowRun`: the status of one STORED workflow run, written through and read
//! back through `GetWorkflowRun`.
//!
//! The rule these cases assert is not "this one request answers these bytes": a run's
//! status is writable only while the run is still under way, so a `RUNNING` run takes the
//! status it already holds (changing nothing) or any one of the statuses that FINISH it,
//! the transition is decided and committed under one hold of the store — which is what
//! makes exactly ONE of several callers racing to finish the same run the winner — and
//! every other status write is refused 400 `ValidationException` with reason
//! `InvalidStatus`, because the status it names is not one the run's current status
//! admits: a run that has left `RUNNING` admits none at all, the terminal status it
//! already holds included, and `DELETING` describes a removal this update starts nothing
//! of.
//!
//! So the tables below vary what that rule generalises over: which of the statuses
//! `WorkflowRunStatus` declares is asked for, whether it finishes the run, the status the
//! run starts from, how many callers race for it, the multiset of statuses they ask for
//! (duplicates included), how many updates run in sequence, and which link of the chain is
//! missing. The recorded six-way race is one row among them.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::WORKFLOW_RUN_STATUS_VALUES;
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::state::WorkflowRunStatus;

use crate::workflow_lifecycle::{text, Emulator};

/// The sentence this service states for a run whose current status does not admit the one a
/// request named, spelled out here rather than taken from the crate so a rewording of the
/// handler's constant cannot change both sides of the comparison at once.
pub(crate) const RUN_IN_UNEXPECTED_STATE: &str = "WorkflowRun in unexpected state";

/// The kind name a refusal reports for a workflow run.
const RUN_RESOURCE_TYPE: &str = "workflow-run";

/// The definition each case builds its runs under.
const DEFINITION: &str = "workflow-run-status-update";

/// A well-formed `UuidString`
/// (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) no record was ever
/// minted for.
const ABSENT_UUID: &str = "00000000-0000-4000-8000-000000000000";

/// The values of `WorkflowRunStatus` that FINISH a run: it ran to the end, it failed, or it
/// ran out of time. Only a run in one of them reports an `endedAt`; `RUNNING` describes a
/// run still under way and `DELETING` one on its way out
/// (`smithy_query(query="shape",shape="WorkflowRunStatus")`).
///
/// `pub(crate)` so the sibling suite that tables the refusal reads the finishing set from
/// this one spelling of it.
pub(crate) const FINISHING_STATUSES: [&str; 3] = ["SUCCEEDED", "FAILED", "TIMED_OUT"];

/// One definition to hang every run of a case off.
async fn definition(emulator: &Emulator) {
    let (status, body) = emulator.create_definition(DEFINITION).await;
    assert_eq!(
        status, 201,
        "the definition must be created, got {status} {body}"
    );
}

/// One new run under [`DEFINITION`], created `RUNNING` as `CreateWorkflowRun` stores it.
async fn new_run(emulator: &Emulator) -> String {
    let (status, body) = emulator.create_run(DEFINITION).await;
    assert_eq!(status, 201, "the run must be created, got {status} {body}");
    let run_id = text(&body, "workflowRunId");
    let read = read_run(emulator, DEFINITION, &run_id).await;
    assert_eq!(
        read["status"], "RUNNING",
        "a new run is RUNNING, got {read}"
    );
    assert!(
        read["endedAt"].is_null(),
        "a run still under way reports no endedAt, got {read}"
    );
    run_id
}

/// The `UpdateWorkflowRun` / `GetWorkflowRun` path: both identifiers are `@httpLabel`s
/// (`smithy_query(query="operation",operation="UpdateWorkflowRun")`).
fn run_path(definition_name: &str, run_id: &str) -> String {
    format!("/workflow-definitions/{definition_name}/workflow-runs/{run_id}")
}

/// One `UpdateWorkflowRun` call asking for `status`.
///
/// `pub(crate)` with the reader below so the sibling suite that drives a run through its
/// terminal transition uses this harness rather than a second copy of it.
pub(crate) async fn move_to(
    emulator: &Emulator,
    definition_name: &str,
    run_id: &str,
    status: &str,
) -> (u16, Value) {
    emulator
        .call(
            "PUT",
            &run_path(definition_name, run_id),
            Some(json!({ "status": status })),
        )
        .await
}

/// The stored run read back through `GetWorkflowRun` — the implemented reader of a run — so
/// nothing below can pass on what the update itself answered.
pub(crate) async fn read_run(emulator: &Emulator, definition_name: &str, run_id: &str) -> Value {
    let (status, body) = emulator
        .call("GET", &run_path(definition_name, run_id), None)
        .await;
    assert_eq!(status, 200, "the read must answer, got {status} {body}");
    body
}

/// Put the stored run on its way out, in the store.
///
/// `DeleteWorkflowRun` is not implemented yet, and the delete of the run's definition — which
/// does move the runs under it — makes the whole definition stop resolving after the span the
/// clock owes, which is not what a case about a status write is for. `pub(crate)` so both
/// suites that need a run in `DELETING` reach it the same way.
pub(crate) fn put_on_its_way_out(
    emulator: &Emulator,
    case: &str,
    definition_name: &str,
    run_id: &str,
) {
    let store = emulator.store();
    let mut store = store.lock();
    let run = store
        .workflow_definitions
        .get_mut(definition_name)
        .and_then(|definition| definition.runs.get_mut(run_id))
        .unwrap_or_else(|| panic!("case {case}: the run must be stored"));
    run.move_to(WorkflowRunStatus::Deleting, std::time::SystemTime::now());
}

/// Assert one refusal is the modelled refusal of a status the run's current status does not
/// admit: HTTP 400 `ValidationException` with reason `InvalidStatus`
/// (`smithy_query(query="shape",shape="ValidationException,ValidationExceptionReason")`) and
/// the fixed sentence, echoing nothing of the run or of the request.
///
/// `pub(crate)` so the sibling suite that tables this refusal over every pair of statuses
/// asserts it through this one reading of the rule.
pub(crate) fn assert_unexpected_state(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} {body}"
    );
    assert_eq!(
        body["__type"], "ValidationException",
        "case {case}: expected ValidationException, got {body}"
    );
    assert_eq!(
        body["reason"], "InvalidStatus",
        "case {case}: expected reason \"InvalidStatus\", got {body}"
    );
    let message = text(body, "message");
    assert_eq!(
        message, RUN_IN_UNEXPECTED_STATE,
        "case {case}: expected message {RUN_IN_UNEXPECTED_STATE:?}, got {message:?}"
    );
    // `ValidationException` carries `message`, `reason` and at most a `fieldList`: the
    // members a `ConflictException` body would carry are not part of this refusal, and the
    // value the request named is admissible in itself, so there is no field to point at.
    for absent in ["resourceId", "resourceType", "fieldList"] {
        assert!(
            body[absent].is_null(),
            "case {case}: expected no {absent} member, got {body}"
        );
    }
}

/// Every status that finishes a run is stored, and `endedAt` is reported exactly once the
/// run has finished.
#[tokio::test]
async fn every_finishing_status_is_stored_and_ended_at_follows_it() {
    let emulator = Emulator::new("us-east-1");
    definition(&emulator).await;

    for requested in FINISHING_STATUSES {
        let run_id = new_run(&emulator).await;
        let before = read_run(&emulator, DEFINITION, &run_id).await;

        let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
        assert_eq!(
            status, 200,
            "case {requested:?}: expected HTTP 200, got {status} {answer}"
        );

        let stored = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            stored["status"], requested,
            "case {requested:?}: expected the read to report {requested:?}, got {stored}"
        );
        assert_eq!(
            stored["startedAt"], before["startedAt"],
            "case {requested:?}: the start instant must not move, got {stored}"
        );
        assert_eq!(
            stored["workflowRunArn"], before["workflowRunArn"],
            "case {requested:?}: the ARN must not move, got {stored}"
        );
        assert!(
            !stored["endedAt"].is_null(),
            "case {requested:?}: a finished run reports endedAt, got {stored}"
        );
    }
}

/// The status a run still under way already holds is accepted and changes nothing, which is
/// what the retry of an `@idempotent` update must observe.
#[tokio::test]
async fn the_status_a_running_run_already_holds_changes_nothing() {
    let emulator = Emulator::new("eu-west-2");
    definition(&emulator).await;
    let run_id = new_run(&emulator).await;
    let first = read_run(&emulator, DEFINITION, &run_id).await;

    for attempt in 1..=3 {
        let (status, answer) = move_to(&emulator, DEFINITION, &run_id, "RUNNING").await;
        assert_eq!(
            status, 200,
            "case attempt {attempt}: expected HTTP 200, got {status} {answer}"
        );
        let again = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            again, first,
            "case attempt {attempt}: the run must be unchanged, got {again}"
        );
    }
}

/// `DELETING` is the status the removal of a run reports, not one this update can set: a run
/// still under way refuses it and stays exactly as it was.
#[tokio::test]
async fn deleting_is_not_a_status_this_update_can_set() {
    let emulator = Emulator::new("us-east-1");
    definition(&emulator).await;
    let run_id = new_run(&emulator).await;
    let before = read_run(&emulator, DEFINITION, &run_id).await;

    let (status, answer) = move_to(&emulator, DEFINITION, &run_id, "DELETING").await;
    assert_unexpected_state("a running run asked for DELETING", status, &answer);

    let after = read_run(&emulator, DEFINITION, &run_id).await;
    assert_eq!(
        after, before,
        "the refused update must change nothing, got {after}"
    );
}

/// A run that has finished keeps the status and the instant it finished with: a request
/// naming ANY status — the one the run holds included — names a status the state the run is
/// in does not admit.
#[tokio::test]
async fn a_finished_run_refuses_every_further_status() {
    let emulator = Emulator::new("us-east-1");
    definition(&emulator).await;

    for finished in FINISHING_STATUSES {
        // Every value the model's enum declares is asked for, so the refusal is stated over
        // the whole set and not only over the other terminal ones.
        for requested in WORKFLOW_RUN_STATUS_VALUES {
            let case = format!("{finished} then {requested}");
            let run_id = new_run(&emulator).await;
            let (status, answer) = move_to(&emulator, DEFINITION, &run_id, finished).await;
            assert_eq!(
                status, 200,
                "case {case}: the run must finish first, got {status} {answer}"
            );
            let finished_read = read_run(&emulator, DEFINITION, &run_id).await;

            let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
            assert_unexpected_state(&case, status, &answer);

            // The refusal left the run exactly as it was, instants included.
            let after = read_run(&emulator, DEFINITION, &run_id).await;
            assert_eq!(
                after, finished_read,
                "case {case}: the refused update must change nothing, got {after}"
            );
        }
    }
}

/// A run on its way out is settled too: the status its deletion put it in is the last one it
/// takes, so an update naming any other is refused as well.
#[tokio::test]
async fn a_run_on_its_way_out_refuses_a_further_status() {
    let emulator = Emulator::new("ap-southeast-1");
    definition(&emulator).await;

    for requested in FINISHING_STATUSES {
        let run_id = new_run(&emulator).await;
        // The run is put on its way out directly, through the shared helper.
        put_on_its_way_out(&emulator, requested, DEFINITION, &run_id);

        let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
        assert_unexpected_state(&format!("DELETING then {requested}"), status, &answer);
        let after = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            after["status"], "DELETING",
            "case {requested:?}: the refused update must leave the run on its way out, \
             got {after}"
        );
    }
}

/// One racer's answer: which racer it was, the status it asked for, the HTTP status it got
/// and the body.
type Answer = (usize, &'static str, u16, Value);

/// Send one `UpdateWorkflowRun` per entry of `wanted` AT ONCE, and answer one [`Answer`]
/// per racer, ordered by racer index.
///
/// Each call runs in its own spawned task holding a handle on the ONE shared emulator, so on
/// the multi-thread runtime this suite's race declares the calls really do overlap rather
/// than interleaving at a single task's await points.
async fn race(emulator: &Arc<Emulator>, run_id: &str, wanted: &[&'static str]) -> Vec<Answer> {
    let mut racers = tokio::task::JoinSet::new();
    for (index, requested) in wanted.iter().enumerate() {
        let emulator = Arc::clone(emulator);
        let run_id = run_id.to_string();
        let requested = *requested;
        racers.spawn(async move {
            let (status, body) = move_to(&emulator, DEFINITION, &run_id, requested).await;
            (index, requested, status, body)
        });
    }
    let mut answers: Vec<Answer> = Vec::new();
    while let Some(joined) = racers.join_next().await {
        answers.push(joined.expect("a racer task must not panic"));
    }
    answers.sort_by_key(|(index, _, _, _)| *index);
    answers
}

/// Of several callers racing to finish one run, exactly one commits its status and the rest
/// are refused the modelled refusal — and the run reports the WINNER's status afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exactly_one_racer_wins_the_terminal_transition() {
    let emulator = Arc::new(Emulator::new("us-west-2"));
    definition(&emulator).await;

    // Rows differ in how many callers race and in the multiset of statuses they ask for:
    // two distinct, two the SAME (so a repeat is not a second winner), the recorded six
    // (SUCCEEDED/FAILED/TIMED_OUT twice over), three identical, and one caller on its own.
    let rows: [(&str, &[&'static str]); 5] = [
        ("one caller", &["SUCCEEDED"]),
        ("two callers, distinct statuses", &["SUCCEEDED", "FAILED"]),
        ("two callers, the same status", &["FAILED", "FAILED"]),
        (
            "three callers, one status",
            &["TIMED_OUT", "TIMED_OUT", "TIMED_OUT"],
        ),
        (
            "six callers over every finishing status twice",
            &[
                "SUCCEEDED",
                "FAILED",
                "TIMED_OUT",
                "SUCCEEDED",
                "FAILED",
                "TIMED_OUT",
            ],
        ),
    ];

    for (case, wanted) in rows {
        let run_id = new_run(&emulator).await;
        let answers = race(&emulator, &run_id, wanted).await;

        let winners: Vec<&Answer> = answers
            .iter()
            .filter(|(_, _, status, _)| *status == 200)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: exactly one racer wins the terminal transition, got {:?}",
            answers
                .iter()
                .map(|(index, requested, status, _)| (index, requested, status))
                .collect::<Vec<_>>()
        );
        let (_, won_with, _, _) = winners[0];

        // Every loser is refused the one modelled refusal: once the winner has committed,
        // the run admits no further status.
        for (index, requested, status, body) in answers
            .iter()
            .filter(|(_, _, status, _)| *status != 200)
        {
            assert_unexpected_state(
                &format!("{case:?} racer {index} asking for {requested}"),
                *status,
                body,
            );
        }

        // The run reports the winner's status, with the instant it finished.
        let stored = read_run(&emulator, DEFINITION, &run_id).await;
        assert_eq!(
            stored["status"], *won_with,
            "case {case:?}: the run must report the winner's status {won_with:?}, got {stored}"
        );
        assert!(
            !stored["endedAt"].is_null(),
            "case {case:?}: the finished run reports endedAt, got {stored}"
        );
    }
}

/// Each link of the path is resolved before the run is written: the first one the store does
/// not hold is reported as the missing resource, by its ARN.
#[tokio::test]
async fn an_update_naming_a_resource_the_store_does_not_hold_is_a_404() {
    let region = "us-west-2";
    let emulator = Emulator::new(region);
    definition(&emulator).await;
    let run_id = new_run(&emulator).await;

    let rows: [(&str, String, &str, String); 2] = [
        (
            "an absent definition",
            run_path("absent", &run_id),
            "workflow-definition",
            "workflow-definition/absent".to_string(),
        ),
        (
            "an absent run under a present definition",
            run_path(DEFINITION, ABSENT_UUID),
            RUN_RESOURCE_TYPE,
            format!("workflow-definition/{DEFINITION}/workflow-run/{ABSENT_UUID}"),
        ),
    ];

    for (case, path, resource_type, resource_path) in rows {
        let (status, answer) = emulator
            .call("PUT", &path, Some(json!({ "status": "SUCCEEDED" })))
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {answer}"
        );
        assert_eq!(
            answer["__type"], "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {answer}"
        );
        assert_eq!(
            answer["resourceType"], resource_type,
            "case {case:?}: expected resourceType {resource_type:?}, got {answer}"
        );
        let expected = format!(
            "arn:aws:nova-act:{region}:{}:{resource_path}",
            engine::EMULATOR_ACCOUNT_ID
        );
        assert_eq!(
            answer["resourceId"], expected,
            "case {case:?}: expected resourceId {expected:?}, got {answer}"
        );
    }

    // The run the rows above named is still RUNNING: nothing was written down a path that
    // did not resolve.
    let stored = read_run(&emulator, DEFINITION, &run_id).await;
    assert_eq!(
        stored["status"], "RUNNING",
        "the present run must be untouched, got {stored}"
    );
    assert!(
        stored["endedAt"].is_null(),
        "the present run must report no endedAt, got {stored}"
    );
}
