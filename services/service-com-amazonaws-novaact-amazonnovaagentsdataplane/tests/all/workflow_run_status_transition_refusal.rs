// SPDX-License-Identifier: CC0-1.0

//! `UpdateWorkflowRun`: HOW a status the workflow run's current status does not admit is
//! refused — the classification, the body shape and the sentence.
//!
//! The rule: a status write naming a status the resource's current status does not admit is
//! a bad value for the `status` member the request carries, not a conflict with the
//! resource, so it is refused HTTP 400 `ValidationException` with reason `InvalidStatus`
//! (the value `smithy_query(query="shape",shape="ValidationExceptionReason")` declares for
//! it) and a body carrying only `message` and `reason`
//! (`smithy_query(query="shape",shape="ValidationException")`) — never the 409
//! `ConflictException` whose body reports a `resourceId` and a `resourceType`. For a run the
//! sentence states the KIND of resource found in a state the write did not expect, with
//! nothing of the request or of the run interpolated, because a run that has settled admits
//! no status at all and so there is no admissible set to hand back.
//!
//! Which statuses a run admits is a property of the status it holds: a run still under way
//! takes the status it already holds (which moves nothing, as an `@idempotent` retry must
//! observe) or any one of the statuses that FINISH it, and no run takes `DELETING`, the
//! status the delete of a run reports.
//!
//! So the table below varies what the rule generalises over: the status the run starts in
//! (each of the five `WorkflowRunStatus` declares — `RUNNING` as a new run holds it, each
//! finishing status driven through this very operation, `DELETING` as a removal leaves it)
//! crossed with the status then asked for (each of the five), which is every pair the model
//! admits. The recorded case — a run that SUCCEEDED, asked for FAILED — is one row among
//! them, and so is the `@idempotent` retry that re-sends the status a settled run already
//! holds.

use serde_json::Value;
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::WORKFLOW_RUN_STATUS_VALUES;

use crate::workflow_lifecycle::{text, Emulator};
use crate::workflow_run_status_update::{
    assert_unexpected_state, move_to, put_on_its_way_out, read_run, FINISHING_STATUSES,
    RUN_IN_UNEXPECTED_STATE,
};

/// The definition each case builds its runs under.
const DEFINITION: &str = "run-transition-refusal";

/// Whether a run whose current status is `current` admits a move to `requested`, spelled out
/// here rather than read off the crate so the handler's rule and this expectation cannot be
/// reworded together: only a run still under way admits anything, and `DELETING` is not a
/// status this update sets.
fn admits(current: &str, requested: &str) -> bool {
    current == "RUNNING" && requested != "DELETING"
}

/// One run under [`DEFINITION`] whose current status is `current`.
///
/// `RUNNING` is what `CreateWorkflowRun` stores; a finishing status is reached through this
/// very operation, so the case starts from a run the emulator itself drove there; `DELETING`
/// is reached through the sibling suite's shared helper, which writes it onto the store.
async fn run_in(emulator: &Emulator, case: &str, current: &str) -> String {
    let (status, body) = emulator.create_run(DEFINITION).await;
    assert_eq!(
        status, 201,
        "case {case}: the run must be created, got {status} {body}"
    );
    let run_id = text(&body, "workflowRunId");

    if FINISHING_STATUSES.contains(&current) {
        let (status, answer) = move_to(emulator, DEFINITION, &run_id, current).await;
        assert_eq!(
            status, 200,
            "case {case}: the run must reach {current} first, got {status} {answer}"
        );
    } else if current == "DELETING" {
        put_on_its_way_out(emulator, case, DEFINITION, &run_id);
    }

    let stored = read_run(emulator, DEFINITION, &run_id).await;
    assert_eq!(
        stored["status"], current,
        "case {case}: the run must start in {current}, got {stored}"
    );
    run_id
}

/// Every pair of (status the run holds, status asked for): the pairs the run's current
/// status admits are taken, and every other one is refused the modelled 400.
#[tokio::test]
async fn every_status_a_run_does_not_admit_is_refused_as_a_bad_status_value() {
    let emulator = Emulator::new("us-east-1");
    let (status, body) = emulator.create_definition(DEFINITION).await;
    assert_eq!(
        status, 201,
        "the definition must be created, got {status} {body}"
    );

    for current in WORKFLOW_RUN_STATUS_VALUES {
        for requested in WORKFLOW_RUN_STATUS_VALUES {
            let case = format!("{current} then {requested}");
            let run_id = run_in(&emulator, &case, current).await;
            let before = read_run(&emulator, DEFINITION, &run_id).await;

            let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
            if admits(current, requested) {
                assert_eq!(
                    status, 200,
                    "case {case}: an admitted status must be taken, got {status} {answer}"
                );
                let after = read_run(&emulator, DEFINITION, &run_id).await;
                assert_eq!(
                    after["status"], *requested,
                    "case {case}: the run must report {requested}, got {after}"
                );
                continue;
            }

            assert_unexpected_state(&case, status, &answer);
            let after = read_run(&emulator, DEFINITION, &run_id).await;
            assert_eq!(
                after, before,
                "case {case}: the refused update must change nothing, got {after}"
            );
        }
    }
}

/// The refusal echoes NOTHING a caller sent or the store holds: not the status the run is
/// in, not the status the request named, not the run's id, not the definition's name — so
/// two callers that find a run settled read the very same sentence.
#[tokio::test]
async fn the_refusal_echoes_nothing_the_caller_sent() {
    let emulator = Emulator::new("eu-west-1");
    let (status, body) = emulator.create_definition(DEFINITION).await;
    assert_eq!(
        status, 201,
        "the definition must be created, got {status} {body}"
    );

    // Rows whose run id, current status and requested status all differ, including the
    // `@idempotent` retry that re-sends the status the run already holds.
    let rows: [(&str, &str); 4] = [
        ("SUCCEEDED", "FAILED"),
        ("SUCCEEDED", "SUCCEEDED"),
        ("TIMED_OUT", "RUNNING"),
        ("DELETING", "TIMED_OUT"),
    ];
    let mut sentences: Vec<String> = Vec::new();
    for (current, requested) in rows {
        let case = format!("{current} then {requested}");
        let run_id = run_in(&emulator, &case, current).await;

        let (status, answer) = move_to(&emulator, DEFINITION, &run_id, requested).await;
        assert_unexpected_state(&case, status, &answer);

        let message = text(&answer, "message");
        for named in [requested, current, run_id.as_str(), DEFINITION] {
            assert!(
                !message.contains(named),
                "case {case}: {named:?} must not appear in the refusal, got {message:?}"
            );
        }
        sentences.push(message);
    }

    for (index, sentence) in sentences.iter().enumerate() {
        assert_eq!(
            sentence, RUN_IN_UNEXPECTED_STATE,
            "case row {index}: every refusal reads alike, got {sentence:?}"
        );
    }
}

/// The refusal is answered on exactly the runs the rule says, and nothing about the request
/// body's other members changes it: a settled run refuses a status whichever well-formed
/// `status` value is named, and a run still under way keeps taking one it admits afterwards.
#[tokio::test]
async fn a_refused_write_leaves_the_rest_of_the_store_taking_writes() {
    let emulator = Emulator::new("ap-southeast-2");
    let (status, body) = emulator.create_definition(DEFINITION).await;
    assert_eq!(
        status, 201,
        "the definition must be created, got {status} {body}"
    );

    // One settled run and one still under way, under the same definition.
    let settled = run_in(&emulator, "a settled run", "FAILED").await;
    let under_way = run_in(&emulator, "a run still under way", "RUNNING").await;

    for requested in WORKFLOW_RUN_STATUS_VALUES {
        let case = format!("the settled run asked for {requested}");
        let (status, answer) = move_to(&emulator, DEFINITION, &settled, requested).await;
        assert_unexpected_state(&case, status, &answer);
    }

    // The sibling the refusals did not name is untouched and still takes a status it
    // admits.
    let before: Value = read_run(&emulator, DEFINITION, &under_way).await;
    assert_eq!(
        before["status"], "RUNNING",
        "the sibling must still be RUNNING, got {before}"
    );
    let (status, answer) = move_to(&emulator, DEFINITION, &under_way, "SUCCEEDED").await;
    assert_eq!(
        status, 200,
        "the sibling must still take a status it admits, got {status} {answer}"
    );
    let after = read_run(&emulator, DEFINITION, &under_way).await;
    assert_eq!(
        after["status"], "SUCCEEDED",
        "the sibling must report the status it took, got {after}"
    );
}
