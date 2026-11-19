// SPDX-License-Identifier: CC0-1.0

//! The CLOCK that drives this service's jobs, and the DRAIN every job-plane call makes.
//!
//! One pass, in one place: the transitions the clock has already made due are materialised
//! ([`ComAmazonawsBatchAwsbatchv20160810State::materialize_due`]), and what those transitions
//! left for another service — or for a program this service performs itself — is settled with
//! this store's lock released, beside the job state changes they owe EventBridge
//! ([`job_publication`]). A handler reaches that pass through
//! [`publish_job_state_changes`]; the in-process pump ([`drive_job_clock`]) reaches the same
//! one with no request behind it, so a client that watches a job through EventBridge alone
//! still sees every move.

use super::*;

mod job_publication;
pub use job_publication::*;

/// The shortest one wake-up of the clock pump waits.
///
/// A due instant already in the past is materialised on the NEXT wake-up, not on
/// this one's instant, so the pump always hands the runtime back and a wake-up
/// that materialises nothing cannot turn the loop into a spin. It is far below
/// [`JOB_PHASE_MILLIS`], so it never delays a transition noticeably.
const MIN_CLOCK_WAIT_MILLIS: i64 = 5;

/// The longest one wake-up of the clock pump waits before it looks at the store
/// again.
///
/// Every transition this service owes falls due [`JOB_PHASE_MILLIS`] after the one
/// before it, so a wait is never near this value; it is a backstop, so a due
/// instant far in the future cannot park the pump indefinitely.
const MAX_CLOCK_WAIT_MILLIS: i64 = 5_000;

/// How many wake-ups one pump makes before it stands down.
///
/// A backstop, not a budget: the transitions the store owes are already finite
/// (each job makes at most a handful, and the store is capped by
/// [`limits::MAX_JOBS`]), and every wake-up either materialises a transition or
/// clears the due instant that woke it. The cap guarantees termination even if a
/// later change left a due instant that materialising does not clear, and the next
/// submit starts a fresh pump.
const MAX_CLOCK_WAKE_UPS: usize = 10_000;

/// Publish the job state changes the CLOCK owes, as they fall due, with no further
/// call from anybody.
///
/// AWS Batch notifies EventBridge as a job moves, which is the whole point of the
/// event path: a client that reacts to job completion through EventBridge makes no
/// further Batch call, so a transition published only by the next reader would
/// never be published at all. This starts ONE in-process task that waits for the
/// earliest due instant the store holds, materialises what is then due
/// ([`ComAmazonawsBatchAwsbatchv20160810State::materialize_due`], the same call
/// every operation makes) and publishes what that produced, until nothing is owed.
///
/// It does not change WHEN a transition happens: `materialize_due` applies only
/// what the stored due instant has already made due, so a caller reading before
/// that instant still sees the transient status, and two reads at the same instant
/// still answer identically. The task waits for the clock so that no request ever
/// has to.
///
/// At most one pump runs per store (the store carries the flag), it is bounded by
/// [`MAX_CLOCK_WAKE_UPS`] wake-ups, and it exits as soon as the store owes nothing.
/// With no async runtime to spawn onto there is no pump, and the transitions are
/// materialised and published by the next operation instead.
pub fn drive_job_clock<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &::std::sync::Arc<Eng>,
    region: &str,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = state.lock().start_clock_pump() else {
        return;
    };
    let callback = ctx.callback().clone();
    let engine = ::std::sync::Arc::clone(engine);
    let region = region.to_string();
    let pumped = ::engine::ServiceState::clone(&state);
    let handle = runtime.spawn(async move {
        // `true` only if the wake-up cap below is what ended the loop; a pump that
        // ran out of work has already released its claim inside the one critical
        // section that observed it (`next_job_due_or_release_pump`).
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let Some(due) = state
                .lock()
                .next_job_due_or_release_pump(now_millis(), generation)
            else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what
            // keeps the backstop above from ever costing a CPU-bound spin, and it
            // hands the runtime back between wake-ups.
            let wait = (due - now_millis()).clamp(MIN_CLOCK_WAIT_MILLIS, MAX_CLOCK_WAIT_MILLIS);
            ::tokio::time::sleep(::std::time::Duration::from_millis(
                u64::try_from(wait).unwrap_or(1),
            ))
            .await;
            state.lock().materialize_due(now_millis());
            publish_pending(&callback, &*engine, &region).await;
        }
        if capped {
            // Only THIS pump's claim, so a pump started since is left holding its own.
            state.lock().release_clock_pump(generation);
        }
    });
    // The handle is kept so the pump it replaces is aborted rather than merely
    // forgotten: that is what guarantees no earlier pump task runs beside the one
    // holding the claim.
    pumped.lock().set_clock_pump(handle);
}
