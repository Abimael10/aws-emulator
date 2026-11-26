// SPDX-License-Identifier: CC0-1.0

//! What `GetCalculationExecutionStatus` answers, driven end to end over the same router the
//! emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`): "gets the status of
//! a current calculation".
//!
//! THE RULES UNDER TEST:
//!
//! 1. A status read is a PROJECTION of the stored calculation: the state it reports is derived
//!    from the instants the record holds and the clock, `CompletionDateTime` is reported
//!    exactly for a calculation that is no longer processing and never before the instant it
//!    was submitted at, `StateChangeReason` is the reason the record holds, and `Statistics`
//!    are reported exactly for a terminal calculation whose run REPORTED — the data processing
//!    unit execution time being the time that run took, in milliseconds. A status read and a
//!    full read of one calculation at the same instant therefore cannot disagree.
//! 2. A status read CHANGES NOTHING a later caller sees: the record it read is left exactly as
//!    it was, and every later read of the same calculation answers the same status — so
//!    polling a calculation neither moves it on nor holds it back.
//! 3. The transition out of "CREATED - The calculation has been created and is ready to run"
//!    is the CLOCK's: a calculation whose run has reported nothing reaches RUNNING through the
//!    status read alone, reports no state besides those two, and never reports itself ready to
//!    run again once it is running.
//!
//! Rules 1 and 2 are asserted over a TABLE rather than one request: the rows differ along the
//! shape of the stored record — whether its run has reported, whether that run failed, how
//! long the host measured it taking (under a millisecond, a few milliseconds, more than a
//! minute), whether its completion has fallen due, whether a cancellation was asked for, and
//! whether the record holds a reason for the change.
//!
//! Nothing here waits on the wall clock: a calculation that must sit at a given point of its
//! own lifecycle is SEEDED with the instants that put it there, and the one transition the
//! clock owes is waited on by POLLING the service for the state itself, under a deadline far
//! above the span it is due after.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, CalculationOutcome, CalculationRecord, CALCULATION_CANCELED_WITH_SESSION,
    CALCULATION_STATE_CANCELED, CALCULATION_STATE_COMPLETED, CALCULATION_STATE_CREATED,
    CALCULATION_STATE_FAILED, CALCULATION_STATE_RUNNING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::call;

/// The workgroup the seeded calculations below belong to. Nothing in a status read is read off
/// the workgroup — a calculation is addressed inside the session it ran in — so it is only
/// what the record carries.
const WORK_GROUP: &str = "spark-status-projection";

/// A span so much longer than any test run that an instant this far back is past every
/// transition this emulator owes, and one this far ahead is past none of them.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// How long a wait for the transition the clock owes may take.
///
/// Generous: what is asserted is the state the transition reaches, never how fast the host
/// reaches it, so the deadline is only there to fail a wait that will never end.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(120);

/// How often such a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One shape of stored calculation, and what a read of it must report.
struct Row {
    /// Names this row in every assertion.
    case: &'static str,
    /// The run's outcome, where the run has reported one: whether it failed and the time the
    /// host measured it taking, in milliseconds.
    run: Option<(bool, u64)>,
    /// Whether the completion the submission owed has already fallen due.
    completed: bool,
    /// Whether a cancellation was asked for, long enough ago for the stop to be done.
    cancelled: bool,
    /// The `StateChangeReason` the record holds.
    reason: Option<&'static str>,
    /// The `CalculationExecutionState` every read must report.
    state: &'static str,
    /// The `DpuExecutionInMillis` every read must report, or `None` where no statistics may be
    /// reported at all.
    dpu_millis: Option<i64>,
}

/// Seed one calculation of the shape `row` names into `engine`'s store, and hand back its
/// `CalculationExecutionId`.
///
/// Seeding is what puts a calculation at an exact point of its own lifecycle without waiting
/// for the spans this emulator owes. Every call under test is made over the wire.
fn given_seeded_calculation(engine: &engine::InMemoryEngine, row: &Row) -> String {
    let now = SystemTime::now();
    let long_ago = now
        .checked_sub(WELL_PAST_ANY_RUN)
        .expect("an instant this far back exists");
    let far_ahead = now
        .checked_add(WELL_PAST_ANY_RUN)
        .expect("an instant this far ahead exists");

    let mut record = CalculationRecord::submitted(
        state::new_calculation_execution_id(),
        state::new_session_id(),
        WORK_GROUP.to_string(),
        "print('seeded')".to_string(),
        long_ago,
    );
    let id = record.id.clone();
    record.completion_due_at = if row.completed { long_ago } else { far_ahead };
    record.outcome = row.run.map(|(failed, millis)| CalculationOutcome {
        failed,
        run_time: Duration::from_millis(millis),
        stdout_uri: None,
        stderr_uri: None,
        result_uri: None,
    });
    record.stop_requested_at = row.cancelled.then_some(long_ago);
    record.state_change_reason = row.reason.map(str::to_string);

    let seeded = record.state_at(SystemTime::now());
    assert_eq!(
        seeded, row.state,
        "case {:?}: the seeded record must read as {:?}, got {seeded:?}",
        row.case, row.state
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().calculations.insert(record, None);
    id
}

/// `GetCalculationExecutionStatus` for `id`, which must be served.
async fn read_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetCalculationExecutionStatus",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCalculationExecutionStatus({id}) must answer 200, got {status} with \
         {body:?}"
    );
    body
}

/// `GetCalculationExecution` for `id`, which must be served.
async fn read(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetCalculationExecution",
        &json!({ "CalculationExecutionId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCalculationExecution({id}) must answer 200, got {status} with {body:?}"
    );
    body
}

/// The rows every assertion below is made over: each one a shape the stored record can be in,
/// and the answer that shape projects to. Every state here is one the record cannot leave
/// while the test runs, so a read of it at any instant reports the same thing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a run that has reported nothing",
            run: None,
            completed: false,
            cancelled: false,
            reason: None,
            state: CALCULATION_STATE_RUNNING,
            dpu_millis: None,
        },
        Row {
            case: "a run that reported before its completion fell due",
            run: Some((false, 7)),
            completed: false,
            cancelled: false,
            reason: None,
            state: CALCULATION_STATE_RUNNING,
            dpu_millis: None,
        },
        Row {
            case: "a run the host measured as under a millisecond",
            run: Some((false, 0)),
            completed: true,
            cancelled: false,
            reason: None,
            state: CALCULATION_STATE_COMPLETED,
            dpu_millis: Some(0),
        },
        Row {
            case: "a run of a few seconds that completed",
            run: Some((false, 4_321)),
            completed: true,
            cancelled: false,
            reason: None,
            state: CALCULATION_STATE_COMPLETED,
            dpu_millis: Some(4_321),
        },
        Row {
            case: "a run of more than a minute that the code failed in",
            run: Some((true, 90_061)),
            completed: true,
            cancelled: false,
            reason: None,
            state: CALCULATION_STATE_FAILED,
            dpu_millis: Some(90_061),
        },
        Row {
            case: "a calculation cancelled before its run reported",
            run: None,
            completed: false,
            cancelled: true,
            reason: Some(CALCULATION_CANCELED_WITH_SESSION),
            state: CALCULATION_STATE_CANCELED,
            dpu_millis: None,
        },
        Row {
            case: "a calculation cancelled after its run reported",
            run: Some((false, 11)),
            completed: false,
            cancelled: true,
            reason: None,
            state: CALCULATION_STATE_CANCELED,
            dpu_millis: Some(11),
        },
    ]
}

/// Whether a state is one the calculation can no longer leave, which is when it has a
/// completion instant to report.
fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        CALCULATION_STATE_COMPLETED | CALCULATION_STATE_FAILED | CALCULATION_STATE_CANCELED
    )
}

/// RULE 1: a status read projects the stored record — the state its instants read as, a
/// completion instant exactly once it is no longer processing, the reason it holds, and the
/// statistics of a run that reported — and the full read of the same calculation at the same
/// instant reports the very same status and statistics.
#[tokio::test]
async fn a_status_read_projects_the_stored_calculation() {
    let (engine, router) = crate::harness::engine_and_router();

    for row in rows() {
        let case = row.case;
        let id = given_seeded_calculation(&engine, &row);
        let answered = read_status(&router, case, &id).await;
        let status = &answered["Status"];

        assert_eq!(
            status["State"].as_str(),
            Some(row.state),
            "case {case:?}: expected the state {:?}, got {answered:?}",
            row.state
        );
        assert_eq!(
            status["StateChangeReason"].as_str(),
            row.reason,
            "case {case:?}: expected the reason {:?}, got {answered:?}",
            row.reason
        );
        assert_eq!(
            answered["Statistics"]["DpuExecutionInMillis"].as_i64(),
            row.dpu_millis,
            "case {case:?}: expected the DPU execution time {:?}, got {answered:?}",
            row.dpu_millis
        );
        assert_eq!(
            answered["Statistics"].is_null(),
            row.dpu_millis.is_none(),
            "case {case:?}: statistics are reported exactly for a terminal calculation whose \
             run reported, got {answered:?}"
        );

        let submitted = status["SubmissionDateTime"].as_f64();
        assert!(
            submitted.is_some(),
            "case {case:?}: the instant the calculation was submitted at must be reported, got \
             {answered:?}"
        );
        let completed = status["CompletionDateTime"].as_f64();
        assert_eq!(
            completed.is_some(),
            is_terminal(row.state),
            "case {case:?}: a completion instant is reported exactly for a calculation that is \
             no longer processing, got {answered:?}"
        );
        if let (Some(completed), Some(submitted)) = (completed, submitted) {
            assert!(
                completed >= submitted,
                "case {case:?}: the completion instant {completed} must not be before the \
                 submission instant {submitted}"
            );
        }

        // The same record, read through the operation that reports all of it: one projection,
        // so a status read and a full read of one calculation cannot disagree.
        let whole = read(&router, case, &id).await;
        assert_eq!(
            whole["Status"], answered["Status"],
            "case {case:?}: the full read must report the status the status read reports, got \
             {whole:?}"
        );
        assert_eq!(
            whole["Statistics"], answered["Statistics"],
            "case {case:?}: the full read must report the statistics the status read reports, \
             got {whole:?}"
        );
    }
}

/// RULE 2: reading a calculation's status changes nothing — the record is left as it was and
/// every later read answers the same status, so a client that polls sees what a client that
/// reads once does.
#[tokio::test]
async fn a_status_read_leaves_the_calculation_as_it_was() {
    let (engine, router) = crate::harness::engine_and_router();

    for row in rows() {
        let case = row.case;
        let id = given_seeded_calculation(&engine, &row);

        let store = state::service_state(&*engine).expect("this service's state");
        let before = store
            .lock()
            .calculations
            .calculation(&id)
            .expect("the seeded calculation")
            .clone();

        let first = read_status(&router, case, &id).await;
        for look in 1..=3 {
            let again = read_status(&router, case, &id).await;
            assert_eq!(
                again, first,
                "case {case:?}: read {look} must answer what the first read answered, got \
                 {again:?}"
            );
        }

        let after = store
            .lock()
            .calculations
            .calculation(&id)
            .expect("the calculation a read may not remove")
            .clone();
        let now = SystemTime::now();
        assert_eq!(
            after.state_at(now),
            before.state_at(now),
            "case {case:?}: the reads may not move the calculation on, got {after:?}"
        );
        assert_eq!(
            after.submitted_at, before.submitted_at,
            "case {case:?}: the reads may not restate when the calculation was submitted, got \
             {after:?}"
        );
        assert_eq!(
            after.completion_due_at, before.completion_due_at,
            "case {case:?}: the reads may not move when the completion is due, got {after:?}"
        );
        assert_eq!(
            after.stop_requested_at, before.stop_requested_at,
            "case {case:?}: the reads may not ask for a cancellation, got {after:?}"
        );
        assert_eq!(
            after.outcome.as_ref().map(|outcome| outcome.run_time),
            before.outcome.as_ref().map(|outcome| outcome.run_time),
            "case {case:?}: the reads may not restate what the run produced, got {after:?}"
        );
        assert_eq!(
            after.state_change_reason, before.state_change_reason,
            "case {case:?}: the reads may not restate the reason for the state change, got \
             {after:?}"
        );
    }
}

/// RULE 3: the step out of CREATED is the clock's, and the status read alone observes it: a
/// calculation whose run reports nothing is CREATED and then RUNNING, and never CREATED again.
///
/// The CONDITION is what is waited on — the state the service reports — rather than a span of
/// the host's own speed, so the row holds on a loaded host as well as an idle one.
#[tokio::test]
async fn the_status_read_observes_the_step_the_clock_owes() {
    let (engine, router) = crate::harness::engine_and_router();
    let case = "a calculation submitted just now";

    // Submitted at this instant, with no outcome ever reported: so it is CREATED until that
    // step falls due and RUNNING from then on, and reaches no terminal state at all.
    let mut record = CalculationRecord::submitted(
        state::new_calculation_execution_id(),
        state::new_session_id(),
        WORK_GROUP.to_string(),
        "print('owed')".to_string(),
        SystemTime::now(),
    );
    let id = record.id.clone();
    record.completion_due_at = SystemTime::now()
        .checked_add(WELL_PAST_ANY_RUN)
        .expect("an instant this far ahead exists");
    state::service_state(&*engine)
        .expect("this service's state")
        .lock()
        .calculations
        .insert(record, None);

    let deadline = tokio::time::Instant::now() + TRANSITION_DEADLINE;
    loop {
        let answered = read_status(&router, case, &id).await;
        let state = answered["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match state.as_str() {
            CALCULATION_STATE_CREATED => assert!(
                answered["Status"]["CompletionDateTime"].is_null(),
                "case {case:?}: a calculation that is ready to run has not completed, got \
                 {answered:?}"
            ),
            CALCULATION_STATE_RUNNING => {
                assert!(
                    answered["Statistics"].is_null(),
                    "case {case:?}: no statistics may be reported while the calculation runs, \
                     got {answered:?}"
                );
                break;
            }
            other => panic!(
                "case {case:?}: a calculation whose run reported nothing may only be CREATED or \
                 RUNNING, got {other:?} in {answered:?}"
            ),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the calculation must be observed running, last read {answered:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And it stays running: the step the clock owed is taken once, so no later read reports
    // the calculation ready to run again, and none reports a terminal state for a run that has
    // reported nothing.
    for look in 1..=3 {
        let answered = read_status(&router, case, &id).await;
        assert_eq!(
            answered["Status"]["State"].as_str(),
            Some(CALCULATION_STATE_RUNNING),
            "case {case:?}: read {look} after the step must still report the calculation \
             running, got {answered:?}"
        );
    }
}
