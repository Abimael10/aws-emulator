// SPDX-License-Identifier: CC0-1.0

//! The ONE in-process clock pump this store runs: it waits for the next instant the clock
//! owes a transition at, materialises what is then due, and publishes the EventBridge
//! events those transitions produced.
//!
//! # Why a pump exists at all
//!
//! Every transition this store owes is also materialised by whichever later call reads the
//! record ([`ComAmazonawsMediaconvertMediaconvertState::settle`]), which is what makes a
//! read report the state the clock had already reached. That is enough for a client that
//! keeps polling, and not enough for one that does not: MediaConvert is an event SOURCE
//! (`super::job_events`), and the documented workflow is to submit a job and then react to
//! its state changes on an EventBridge rule, making no further MediaConvert call at all. A
//! transition materialised only by the next reader would never be materialised for such a
//! client, so its event would never leave. The pump is what lets a job's moves — and the
//! events that report them — happen with no request waiting on any of them.
//!
//! # What it does and does not decide
//!
//! It does NOT decide WHEN anything moves: only the stored due instants do
//! ([`ComAmazonawsMediaconvertMediaconvertState::settle`] applies exactly what was owed at
//! the instant it is handed), so a caller reading before one still observes the transient
//! state and its state errors, two reads at the same instant still answer identically, and
//! a job its queue is holding still does not begin. The records those settles produce carry
//! the DUE instant, not the instant the pump happened to wake up, and the pump performs no
//! work of its own: it applies the very same settle every reporting door applies.
//!
//! Each wake-up settles the store and then drains the queue of recorded changes
//! ([`super::publish_pending`]) with NO store guard held: EventBridge is reached through the
//! framework's in-process callback, and a target the event sets off may call back into this
//! service, so holding the store across the publication would deadlock the call the event
//! set off.
//!
//! At most one pump runs per store — the store carries the claim and its generation — it is
//! bounded by [`MAX_CLOCK_WAKE_UPS`] wake-ups, and it stands down as soon as the store owes
//! nothing and has nothing queued. With no async runtime to spawn onto there is no pump, and
//! the transitions are materialised by the next reader instead.

use ::std::time::{Duration, SystemTime};

use super::ComAmazonawsMediaconvertMediaconvertState;

/// The shortest one wake-up of the pump waits.
///
/// A due instant already in the past is materialised on the NEXT wake-up rather than at
/// this one's instant, so the pump always hands the runtime back and a wake-up that
/// materialises nothing cannot turn the loop into a spin. It is far below the span a job
/// waits for its exit from `SUBMITTED` ([`super::JOB_SETTLES_AFTER`]), so it never delays
/// one noticeably.
const MIN_CLOCK_WAIT: Duration = Duration::from_millis(5);

/// The longest one wake-up of the pump waits before it looks at the store again.
///
/// A backstop, not a span anything is owed after: a due instant far in the future — a hop
/// whose `WaitMinutes` the caller chose, say — must not park the pump on one instant for as
/// long as that, because the store can come to owe an earlier transition while it waits.
const MAX_CLOCK_WAIT: Duration = Duration::from_secs(5);

/// How many wake-ups one pump makes before it stands down.
///
/// A backstop, not a budget: the transitions the store owes are already finite (a job owes
/// its exit from `SUBMITTED` and at most one hop per hop destination it was created with,
/// and the jobs, the jobs queries and the share attempts are each bounded), and every
/// wake-up either materialises what was due or clears the instant that woke it. The cap
/// guarantees termination even if a later change left an instant that materialising does
/// not clear, and the next door that arms the clock starts a fresh pump.
const MAX_CLOCK_WAKE_UPS: usize = 10_000;

impl ComAmazonawsMediaconvertMediaconvertState {
    /// What the pump waits for next, or `None` — having RELEASED its claim — when this
    /// store owes nothing more and has nothing queued for publication.
    ///
    /// Observing "nothing is owed" and standing down happen in ONE critical section, which
    /// is what makes the hand-over race-free: a door stores its record (and so its due
    /// instant) under this same guard BEFORE it tries to claim the pump, so either this
    /// call sees that instant and the running pump keeps it, or the door's claim finds the
    /// flag already cleared and starts a pump of its own. A change recorded but not yet
    /// published is itself work, so it keeps the pump awake and nothing queued waits for
    /// the next call.
    fn next_due_or_release_pump(&mut self, now: SystemTime, generation: u64) -> Option<SystemTime> {
        if self.pending_job_event_count() > 0 {
            return Some(now);
        }
        // The head of each of the four timelines `settle` drains, so the pump waits for the
        // earliest instant any of them is owed at.
        let next = [
            self.pending.first(),
            self.pending_queries.first(),
            self.pending_shares.first(),
            self.pending_hops.first(),
        ]
        .into_iter()
        .flatten()
        .map(|entry| entry.0)
        .min();
        match next {
            Some(due) => Some(due),
            None => {
                self.release_clock_pump(generation);
                None
            }
        }
    }

    /// Claim the single pump for this store: the claim's GENERATION when the caller may
    /// start one, `None` when one is already running.
    ///
    /// The generation is what makes a release safe: a pump releases its own claim and never
    /// a later pump's (see [`Self::release_clock_pump`]).
    fn start_clock_pump(&mut self) -> Option<u64> {
        if self.clock_pump_running {
            return None;
        }
        self.clock_pump_running = true;
        self.clock_pump_generation = self.clock_pump_generation.wrapping_add(1);
        Some(self.clock_pump_generation)
    }

    /// Release the claim `generation` took — and ONLY that one.
    ///
    /// A pump that ends on the wake-up cap must not clear a claim a pump started since is
    /// holding, or two pumps would run against one store.
    fn release_clock_pump(&mut self, generation: u64) {
        if self.clock_pump_generation == generation {
            self.clock_pump_running = false;
        }
    }

    /// Keep the running pump's task handle, aborting the one it replaces — unless the
    /// claim has moved on since `generation` took it.
    ///
    /// The replaced handle is normally a pump that has already ended, by standing down or
    /// on its wake-up cap, so the abort is a no-op; it is what guarantees the invariant
    /// anyway, that no earlier pump task is still running beside the one that now holds the
    /// claim. The GENERATION is what keeps that guarantee from inverting: a pump that ended
    /// before its handle was stored has released its claim, and the door that claimed it
    /// next is holding its own handle, so storing this one would abort the pump that is
    /// actually running and leave the claim held by nobody.
    fn set_clock_pump(&mut self, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
        if self.clock_pump_generation != generation {
            return;
        }
        if let Some(previous) = self.clock_pump.replace(handle) {
            previous.abort();
        }
    }
}

/// Start this store's clock pump, unless one is already running.
///
/// Called by the doors that leave the store OWING a job transition — the submit that arms a
/// job's exit from `SUBMITTED` and the queue update that arms the jobs a paused queue was
/// holding — with no store guard held. It returns at once: the waiting, the settling and the
/// publication are the spawned task's, so no request waits on the clock.
///
/// `region` is the Region the arming call was served in, which is the Region whose default
/// event bus the events are published onto — the same Region the door's own publication
/// would have used. A pump already running keeps the Region it was started with, exactly as
/// a drain from any door publishes whatever the queue holds.
pub fn drive_job_clock<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &::std::sync::Arc<Eng>,
    region: &str,
) {
    let Some(state) = super::service_state(&**engine) else {
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
        // `true` only if the wake-up cap below is what ended the loop; a pump that ran out
        // of work has already released its claim inside the one critical section that
        // observed it ([`ComAmazonawsMediaconvertMediaconvertState::next_due_or_release_pump`]).
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let now = SystemTime::now();
            let Some(due) = state.lock().next_due_or_release_pump(now, generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps the
            // backstop above from ever costing a CPU-bound spin, and it hands the runtime
            // back between wake-ups.
            let wait = due
                .duration_since(now)
                .unwrap_or_default()
                .clamp(MIN_CLOCK_WAIT, MAX_CLOCK_WAIT);
            ::tokio::time::sleep(wait).await;
            // The same settle every reporting door makes, at the instant the pump woke: it
            // applies what the clock had already made due and nothing else.
            state.lock().settle(SystemTime::now());
            super::publish_pending(&callback, &*engine, &region).await;
            // And the run every job that has BEGUN is owed: a client that reacts to a job
            // on an EventBridge rule makes no further MediaConvert call, so its job's
            // transcode cannot wait for a read that never comes
            // ([`super::run_due_job_runs`]). With no store guard held, as the publication
            // above is.
            super::run_due_job_runs(&callback, &*engine, &region, None).await;
        }
        if capped {
            // Only THIS pump's claim, so a pump started since is left holding its own.
            state.lock().release_clock_pump(generation);
        }
    });
    // The handle is kept so the pump it replaces is aborted rather than merely forgotten:
    // that is what guarantees no earlier pump task runs beside the one holding the claim.
    // A claim that has moved on since keeps the handle it is holding.
    pumped.lock().set_clock_pump(generation, handle);
}
