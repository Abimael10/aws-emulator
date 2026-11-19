// SPDX-License-Identifier: CC0-1.0

//! The CALIBRATION of the bounds one submitted Spark application run is given: that they are
//! coherent with each other.
//!
//! THE RULE: the bounds a run of caller-supplied code is held to are this emulator's, and they
//! only mean what they say while they agree with one another — the span a run may last is read
//! against a clock the run itself drives, one tick per traced event it delivers, so the whole
//! step budget has to spend LESS than that span for the budget to be what answers an
//! application that computes without end; and the host time one run may take has to exceed the
//! engine's own spans, so an application that FINISHED is reported on what it came to rather
//! than on how fast this host read it.
//!
//! The constants are the EMR crate's own
//! (`service_com_amazonaws_emr_elasticmapreduce::state`), so this suite reads the bounds the
//! service really runs an application under rather than a copy of them.

use std::time::Duration;

use service_com_amazonaws_emr_elasticmapreduce::state::{
    APPLICATION_ABANDON_GRACE, APPLICATION_CALL_CEILING, APPLICATION_CLOCK_TICK,
    APPLICATION_FIRST_WAIT, APPLICATION_HOST_CEILING, APPLICATION_RUN_CEILING,
    MAX_APPLICATION_RUNS_PER_CALL,
};

/// The span the init phase of the engine takes in the run's own time, which the host ceiling
/// has to allow for beside the application's own span.
const INIT_PHASE_LIMIT: Duration =
    ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT;

/// The whole step budget spends less than the span a run may last, so an application that
/// computes without end is answered by the budget — a count of its OWN work — and not by a
/// span of time.
#[test]
fn the_step_budget_spends_less_than_the_run_may_last() {
    let budget = service_com_amazonaws_emr_elasticmapreduce::state::application_budget();
    let spent = APPLICATION_CLOCK_TICK.saturating_mul(
        u32::try_from(budget.steps).unwrap_or(u32::MAX),
    );
    assert!(
        spent < APPLICATION_RUN_CEILING,
        "case {:?}: the step budget spends {spent:?} of the run's own span, which must stay \
         under {APPLICATION_RUN_CEILING:?}",
        "the whole step budget"
    );
}

/// The host time one run may take exceeds the engine's own spans, and the wait for its thread
/// exceeds that ceiling: a run this emulator stops is one it gave up on, never one whose own
/// span merely ran out on a loaded host.
#[test]
fn the_host_ceiling_exceeds_the_engines_own_spans() {
    let engine_spans = INIT_PHASE_LIMIT.saturating_add(APPLICATION_RUN_CEILING);
    assert!(
        APPLICATION_HOST_CEILING > engine_spans,
        "case {:?}: the host ceiling {APPLICATION_HOST_CEILING:?} must exceed the engine's \
         own spans {engine_spans:?}",
        "the host ceiling"
    );
    let waited = APPLICATION_HOST_CEILING.saturating_add(APPLICATION_ABANDON_GRACE);
    assert!(
        waited > APPLICATION_HOST_CEILING,
        "case {:?}: the wait for the run's thread {waited:?} must outlast the ceiling the \
         engine stops it on",
        "the abandon grace"
    );
}

/// What ONE call may spend starting owed applications holds every run it starts: the call
/// waits a moment for each ([`APPLICATION_FIRST_WAIT`]) and leaves the rest to the next call,
/// so no client's call is ever held for an application's own work.
#[test]
fn one_call_spends_no_more_than_its_own_ceiling_starting_runs() {
    let spent = APPLICATION_FIRST_WAIT.saturating_mul(
        u32::try_from(MAX_APPLICATION_RUNS_PER_CALL).unwrap_or(u32::MAX),
    );
    assert!(
        spent <= APPLICATION_CALL_CEILING,
        "case {:?}: starting {MAX_APPLICATION_RUNS_PER_CALL} runs spends {spent:?}, which \
         must stay inside {APPLICATION_CALL_CEILING:?}",
        "every run one call starts"
    );
    assert!(
        APPLICATION_FIRST_WAIT < APPLICATION_RUN_CEILING,
        "case {:?}: the wait for a run that has just started must be shorter than the run's \
         own span, got {APPLICATION_FIRST_WAIT:?}",
        "the first wait"
    );
}
