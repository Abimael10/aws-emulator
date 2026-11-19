// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CancelMLTaskRun`: stopping ONE task run of ONE machine learning
//! transform.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. The transform every run
//! here is started of is registered through the shared create helper
//! ([`crate::ml_transforms::given_transform`]) and the runs through the shared start helper
//! ([`crate::ml_task_runs::start_labeling_run`]), so this suite stages a run exactly as the
//! read and listing suites do.
//!
//! RULE under test: a task run is addressed by its parent `TransformId` and the `TaskRunId`
//! the start minted, so an address this account cannot resolve — either half of it — is the
//! operation's modelled `EntityNotFoundException`; a run the service is still working through
//! is asked to stop, which is observable as the `STOPPING` status the cancel reports and a
//! later read reports with it, and the clock then owes that run `STOPPED`; and a run with no
//! task left to stop is the modelled `InvalidInputException`.
//!
//! The recorded case — one absent transform id and one absent run id — is TWO rows of the
//! miss table below; the rows differ along the form of the id that resolves to nothing, which
//! half of the address it is, and whether it names a run of ANOTHER transform.
//!
//! The due instant a cancelled run's owed status turns on is moved THROUGH THE STORE rather
//! than slept for, and the runs a row observes in flight are HELD in flight the same way, so
//! every window is deterministic and no assertion depends on how fast the host runs.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, ML_TASK_RUN_STATUS_STARTING,
    ML_TASK_RUN_STATUS_STOPPED, ML_TASK_RUN_STATUS_STOPPING, ML_TASK_RUN_STATUS_TIMEOUT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{already_past, emulator_with_engine, far_future};
use crate::ml_task_runs::start_labeling_run;
use crate::ml_transforms::given_transform;
use crate::wire::{call, error_code, error_message, ok};

/// Where a labelling set generation run writes its output: the one `@required` member of the
/// start beside the transform it is a run of.
const OUTPUT_PATH: &str = "s3://ml-labels/cancel/";

/// Cancel `run_id` of `transform_id`, answering the status and body of the call.
async fn cancel(
    router: &MultiServiceRouter,
    transform_id: &str,
    run_id: &str,
) -> (u16, Value) {
    call(
        router,
        "CancelMLTaskRun",
        &json!({ "TransformId": transform_id, "TaskRunId": run_id }),
    )
    .await
}

/// What `GetMLTaskRun` reports for one run.
async fn get_run(router: &MultiServiceRouter, transform_id: &str, run_id: &str) -> Value {
    ok(
        router,
        "GetMLTaskRun",
        &json!({ "TransformId": transform_id, "TaskRunId": run_id }),
    )
    .await
}

/// The `Status` of a reported run.
fn status_of(case: &str, run: &Value) -> String {
    run.get("Status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: expected a Status, got {run}"))
        .to_string()
}

/// Make the status the clock owes every unsettled task run due, so a row reaches it without
/// waiting on the wall clock. The transition is owed by the clock, so moving its due instant
/// is the same thing as the instant arriving.
fn make_owed_status_due(engine: &Arc<engine::InMemoryEngine>) {
    move_owed_status_to(engine, already_past());
}

/// The other direction: hold every unsettled task run in the status it stands in, by pushing
/// what it is owed out of the way — so a row that observes a run IN FLIGHT observes it
/// whatever else the host is doing, instead of racing the span the clock owes it.
fn hold_runs_in_flight(engine: &Arc<engine::InMemoryEngine>) {
    move_owed_status_to(engine, far_future());
}

/// Move the due instant of every unsettled task run to `due`.
fn move_owed_status_to(engine: &Arc<engine::InMemoryEngine>, due: std::time::SystemTime) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    for transform in store.ml_transforms.values_mut() {
        for run in &mut transform.task_runs {
            if run.settles_at.is_some() {
                run.settles_at = Some(due);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The cancel
// ---------------------------------------------------------------------------

/// A cancel of a run still in flight reports `STOPPING`, a read of the same run reports the
/// same status, and the clock then owes the run `STOPPED` — with no error string, because a
/// cancelled run failed on nothing.
///
/// The rows differ along how many runs the transform holds and which of them is cancelled, so
/// the cancel is shown to reach the ONE run its address names and leave the others alone.
#[tokio::test]
async fn a_cancel_stops_the_run_it_addresses_and_no_other() {
    let (router, engine) = emulator_with_engine();

    for (case, runs, cancelled) in [
        ("the only run of the transform", 1usize, 0usize),
        ("the first of three runs", 3, 0),
        ("the last of three runs", 3, 2),
    ] {
        let transform = given_transform(&router, &format!("cancel-{cancelled}-{runs}"), json!({}))
            .await;
        let mut started = Vec::new();
        for index in 0..runs {
            started.push(
                start_labeling_run(&router, &transform, &format!("{OUTPUT_PATH}{index}/")).await,
            );
        }
        // Every run is held in the status its start put it in, so what this row observes is
        // the cancel's own work and not the span the clock owes each run.
        hold_runs_in_flight(&engine);

        let (status, body) = cancel(&router, &transform, &started[cancelled]).await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body,
            json!({
                "TransformId": transform,
                "TaskRunId": started[cancelled],
                "Status": ML_TASK_RUN_STATUS_STOPPING,
            }),
            "case {case}: the cancel reports the run it stopped, got {body}"
        );

        // The status the cancel reported is the store's: a read of the run agrees with it,
        // and every other run of the transform is untouched. The cancel owes the run its way
        // to stopped, so that span is held open too.
        hold_runs_in_flight(&engine);
        let read = get_run(&router, &transform, &started[cancelled]).await;
        assert_eq!(
            status_of(case, &read),
            ML_TASK_RUN_STATUS_STOPPING,
            "case {case}: a read of the cancelled run must report it stopping, got {read}"
        );
        assert_eq!(
            read.get("ErrorString"),
            None,
            "case {case}: a cancelled run failed on nothing, got {read}"
        );
        for (index, other) in started.iter().enumerate() {
            if index == cancelled {
                continue;
            }
            let read = get_run(&router, &transform, other).await;
            assert_eq!(
                status_of(case, &read),
                ML_TASK_RUN_STATUS_STARTING,
                "case {case}: run {index} was not cancelled, got {read}"
            );
        }

        // The clock owes a stopping run its way to stopped, and the run that was left alone
        // its own end — the one its budget running out puts it in.
        make_owed_status_due(&engine);
        let read = get_run(&router, &transform, &started[cancelled]).await;
        assert_eq!(
            status_of(case, &read),
            ML_TASK_RUN_STATUS_STOPPED,
            "case {case}: the clock owes a cancelled run STOPPED, got {read}"
        );
        assert_eq!(
            read.get("ErrorString"),
            None,
            "case {case}: a stopped run carries no error string, got {read}"
        );
        assert!(
            read.get("CompletedOn").is_some(),
            "case {case}: a stopped run has completed, got {read}"
        );
        for (index, other) in started.iter().enumerate() {
            if index == cancelled {
                continue;
            }
            let read = get_run(&router, &transform, other).await;
            assert_eq!(
                status_of(case, &read),
                ML_TASK_RUN_STATUS_TIMEOUT,
                "case {case}: run {index} ends the way an uncancelled run ends, got {read}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// An address this account cannot resolve is the modelled miss, worded for the half of the
/// address that missed, and it cancels nothing.
///
/// The rows span the forms an id that resolves to nothing takes: a transform id of another
/// shape entirely, one that is a well-formed hex id nothing minted, the id of a run offered
/// as a transform id, and — against a REAL transform — a run id nothing minted and the id of
/// a run of another transform.
#[tokio::test]
async fn a_cancel_of_an_address_that_resolves_to_nothing_is_the_modelled_miss() {
    let (router, engine) = emulator_with_engine();
    let transform = given_transform(&router, "cancel-miss-host", json!({})).await;
    let run = start_labeling_run(&router, &transform, OUTPUT_PATH).await;
    let other_transform = given_transform(&router, "cancel-miss-other", json!({})).await;
    let other_run = start_labeling_run(&router, &other_transform, OUTPUT_PATH).await;
    // Both staged runs stay in flight for the whole row set, so "the refusal cancelled
    // nothing" is about the refusals and not about the span the clock owes each run.
    hold_runs_in_flight(&engine);

    // The transform half of the address: whatever run id goes with it, no transform means no
    // run at all.
    for (case, transform_id) in [
        ("a name rather than an id", "cancel-miss-host".to_string()),
        (
            "a well-formed id nothing minted",
            "0123456789abcdef0123456789abcdef".to_string(),
        ),
        ("a run id offered as a transform id", run.clone()),
        ("an empty-looking id", "-".to_string()),
    ] {
        let (status, body) = cancel(&router, &transform_id, &run).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::ml_transform_not_found(&transform_id),
            "case {case}: expected the transform miss for {transform_id}, got {body}"
        );
    }

    // The run half: the transform resolves, so what missed is the run.
    for (case, run_id) in [
        (
            "a well-formed run id nothing minted",
            "ffffffffffffffffffffffffffffffff".to_string(),
        ),
        ("a run of another transform", other_run.clone()),
        ("the transform's own id as a run id", transform.clone()),
    ] {
        let (status, body) = cancel(&router, &transform, &run_id).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::ml_task_run_not_found(&run_id, &transform),
            "case {case}: expected the task run miss for {run_id}, got {body}"
        );
    }

    // Nothing was cancelled by any of the refusals: both staged runs are as they were.
    for (case, transform_id, run_id) in [
        ("the host transform's run", &transform, &run),
        ("the other transform's run", &other_transform, &other_run),
    ] {
        let read = get_run(&router, transform_id, run_id).await;
        assert_eq!(
            status_of(case, &read),
            ML_TASK_RUN_STATUS_STARTING,
            "case {case}: a refused cancel must stop nothing, got {read}"
        );
    }
}

/// A run with no task left to stop is the modelled refusal, and it is still the run it was:
/// the rows span a run this operation already cancelled, one the clock has already stopped,
/// and one the clock ended on its own.
#[tokio::test]
async fn a_cancel_of_a_run_with_no_task_left_is_the_modelled_refusal() {
    let (router, engine) = emulator_with_engine();
    let transform = given_transform(&router, "cancel-finished", json!({})).await;

    // A run that has been ASKED to stop and has not stopped yet: its task is already being
    // torn down, so a second cancel has nothing to ask for. Checked before the clock is
    // moved, so the run really is still `STOPPING`.
    let stopping = start_labeling_run(&router, &transform, OUTPUT_PATH).await;
    hold_runs_in_flight(&engine);
    let (status, body) = cancel(&router, &transform, &stopping).await;
    assert_eq!(status, 200, "the first cancel is served, got {status} {body}");
    // The cancel owes the run its way to stopped; that span is held open, so the run really
    // is still `STOPPING` for the rest of this row whatever the host is doing.
    hold_runs_in_flight(&engine);
    let before = get_run(&router, &transform, &stopping).await;
    assert_eq!(
        status_of("already stopping", &before),
        ML_TASK_RUN_STATUS_STOPPING,
        "the run is still stopping before the clock moves, got {before}"
    );
    let (status, body) = cancel(&router, &transform, &stopping).await;
    assert_eq!(
        status, 400,
        "case already stopping: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        messages::ML_TASK_RUN_CANNOT_BE_CANCELLED,
        "case already stopping: expected the not-cancellable refusal, got {body}"
    );

    // A run that has STOPPED, and a run the clock terminated without any cancel.
    let stopped = start_labeling_run(&router, &transform, OUTPUT_PATH).await;
    let terminated = start_labeling_run(&router, &transform, OUTPUT_PATH).await;
    hold_runs_in_flight(&engine);
    let (status, body) = cancel(&router, &transform, &stopped).await;
    assert_eq!(status, 200, "the first cancel is served, got {status} {body}");
    make_owed_status_due(&engine);
    let _ = get_run(&router, &transform, &stopped).await;

    for (case, run_id, expected_status) in [
        ("already stopped", &stopped, ML_TASK_RUN_STATUS_STOPPED),
        (
            "ended without a cancel",
            &terminated,
            ML_TASK_RUN_STATUS_TIMEOUT,
        ),
    ] {
        let before = get_run(&router, &transform, run_id).await;
        let (status, body) = cancel(&router, &transform, run_id).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            messages::ML_TASK_RUN_CANNOT_BE_CANCELLED,
            "case {case}: expected the not-cancellable refusal, got {body}"
        );

        let after = get_run(&router, &transform, run_id).await;
        assert_eq!(
            after, before,
            "case {case}: a refused cancel must change nothing, expected {before}, got {after}"
        );
        assert_eq!(
            status_of(case, &after),
            expected_status,
            "case {case}: the run stands where it stood, got {after}"
        );
    }
}
