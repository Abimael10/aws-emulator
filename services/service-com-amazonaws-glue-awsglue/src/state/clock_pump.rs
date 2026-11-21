// SPDX-License-Identifier: CC0-1.0

//! The ONE in-process clock pump this store runs: it waits for the next instant the CLOCK owes
//! a transition at, materialises what is then due, and publishes the EventBridge events those
//! transitions produced.
//!
//! # Why a pump exists at all
//!
//! Every owed transition this service holds is materialised by whichever later call reads the
//! record ([`ComAmazonawsGlueAwsglueState::settle_due_job_runs`],
//! [`ComAmazonawsGlueAwsglueState::settle_due_crawls`],
//! [`ComAmazonawsGlueAwsglueState::settle_due_scheduled_trigger_firings`]), which is what
//! makes a read report the
//! state the clock had already reached. That is enough for a client that keeps polling, and
//! not enough for one that does not: Glue is an event SOURCE, and a client that wires a job
//! run or a crawl to downstream work through EventBridge makes no further Glue call at all, so
//! an event published only by the next reader would never be published — and a client that
//! declares a SCHEDULED trigger expects its firings with no call of its own at all. The pump
//! is what lets those events leave, and those firings happen, with no request waiting on any
//! of them.
//!
//! # What it does and does not decide
//!
//! It does NOT decide WHEN anything moves: only the stored due instants do, so a caller
//! reading before one still observes the transient state and two reads at the same instant
//! still answer identically. The pump applies exactly the same settles a reading door applies,
//! and the records those settles produce carry the DUE instant rather than the instant the
//! pump happened to wake up. It performs no work of its own either — no script is run here;
//! the door that performs a script records what it came to.
//!
//! Each wake-up settles every family this store owes transitions to — the firings a schedule
//! owes first, then the runs and crawls — and then drains both
//! event queues ([`publish_pending_job_run_events`], [`publish_pending_crawler_events`]), with
//! NO store guard held across either publication: EventBridge is reached through the
//! framework's in-process callback, and a target the event sets off may call back into this
//! service, so holding the store across it would deadlock the very call the event set off.
//!
//! At most one pump runs per store — the store carries the claim and its generation — it is
//! bounded by [`MAX_CLOCK_WAKE_UPS`] wake-ups, and it stands down as soon as the store owes
//! nothing and has nothing queued. With no async runtime to spawn onto there is no pump, and
//! the transitions are materialised by the next reader instead.
//!
//! # A claim is only as good as the pump holding it
//!
//! The claim is what keeps a second pump from running, so a claim nobody is holding any more
//! would stop EVERY later pump from starting — and the publication is the pump's alone, so
//! from then on nothing this store queues would ever be published, however many runs came to
//! rest. The claim is therefore kept beside a token the pump's own task OWNS
//! ([`ClockPumpLife`]): the token is dropped whenever that task ends, whether it stood down,
//! reached its wake-up cap, panicked or was dropped before it finished, so a claim whose token
//! is gone is not a claim and the next door takes it over. The token is recorded in the SAME
//! critical section the claim is taken in, so the span between taking a claim and spawning the
//! task that holds it is covered too.
//!
//! Why a token and not the task handle itself: a handle exists only once the task has been
//! spawned, which is after the critical section the claim was taken in, so between those two
//! points there is no handle to ask — and a store that read "no handle yet" as "no pump" would
//! start a second pump against the same claim. The token has no such window, because it is
//! created where the claim is.
//!
//! The task's HANDLE is kept beside both ([`ComAmazonawsGlueAwsglueState::clock_pump`]), so a
//! pump this service started stays reachable rather than detached — under the generation of the
//! claim it was spawned for, because a handle cannot be stored in the critical section its
//! claim was taken in and a late-arriving one must not replace, and so abort, the pump that now
//! holds the claim.

use ::std::time::{Duration, SystemTime};

use super::*;

/// The shortest one wake-up of the pump waits.
///
/// A due instant already in the past is materialised on the NEXT wake-up, not on this one's
/// instant, so the pump always hands the runtime back and a wake-up that materialises nothing
/// cannot turn the loop into a spin. It is far below the spans a transition is owed after
/// ([`JOB_RUN_SETTLES_AFTER`], [`CRAWL_SETTLES_AFTER`]), so it never delays one noticeably.
const MIN_CLOCK_WAIT: Duration = Duration::from_millis(5);

/// The longest one wake-up of the pump waits before it looks at the store again.
///
/// Every state a record owes falls due within a span or two of the one before it, so a wait is
/// never near this value; it is a backstop, so a due instant far in the future cannot park the
/// pump indefinitely.
const MAX_CLOCK_WAIT: Duration = Duration::from_secs(5);

/// How many wake-ups one pump makes before it stands down.
///
/// A backstop, not a budget: the transitions the store owes are already finite (a run owes a
/// handful and a crawl owes one, and the jobs, the runs one job keeps and the crawlers are all
/// bounded), and every wake-up either materialises a transition or clears the due instant that
/// woke it. The cap guarantees termination even if a later change left a due instant that
/// materialising does not clear — a SCHEDULED trigger is the one record that is owed a FURTHER
/// firing once the one that fell due has been performed, which is what a schedule IS — and the
/// next call starts a fresh pump.
const MAX_CLOCK_WAKE_UPS: usize = 10_000;

impl ComAmazonawsGlueAwsglueState {
    /// What the pump waits for next, or `None` — having RELEASED its claim — when this store
    /// owes nothing more and has nothing queued.
    ///
    /// Observing "nothing is owed" and standing down happen in ONE critical section, which is
    /// what makes the hand-over race-free: a door records its change (and a start stores its
    /// record, and so its due instant) under this same guard BEFORE it tries to claim the
    /// pump, so either this call sees that instant and the running pump keeps it, or the door's
    /// claim finds the flag already cleared and starts a pump of its own. A change recorded but
    /// not yet published keeps the pump awake too, so nothing queued waits for the next call.
    fn next_due_or_release_pump(&mut self, now: SystemTime, generation: u64) -> Option<SystemTime> {
        if self.has_pending_job_run_events() || self.has_pending_crawler_events() {
            return Some(now);
        }
        let next = [
            self.next_job_run_due_at(),
            self.next_crawl_due_at(),
            self.next_scheduled_trigger_firing_due_at(),
        ]
        .into_iter()
        .flatten()
        .min();
        match next {
            Some(due) => Some(due),
            None => {
                self.release_clock_pump(generation);
                None
            }
        }
    }

    /// Claim the single pump for this store: the claim's GENERATION and the token the pump is
    /// to hold, or `None` when a pump really is running.
    ///
    /// The generation is what makes a release safe: a pump releases its own claim and never a
    /// later pump's (see [`Self::release_clock_pump`]). The token is what makes the claim
    /// RECOVERABLE: a claim whose pump has ended without releasing it is taken over here
    /// ([`Self::clock_pump_is_running`]), so the publication this store owes is never parked
    /// behind a pump that no longer exists.
    fn start_clock_pump(&mut self) -> Option<(u64, ::std::sync::Arc<ClockPumpLife>)> {
        if self.clock_pump_is_running() {
            return None;
        }
        let life = ::std::sync::Arc::new(ClockPumpLife);
        // Recorded under the same guard the claim is taken under, and alive from here: the
        // caller holds the token until the task it spawns owns it.
        self.clock_pump_life = Some(::std::sync::Arc::downgrade(&life));
        self.clock_pump_running = true;
        self.clock_pump_generation = self.clock_pump_generation.wrapping_add(1);
        Some((self.clock_pump_generation, life))
    }

    /// Whether a pump that EXISTS is holding the claim: it was taken, and the token the pump
    /// holding it owns is still alive.
    fn clock_pump_is_running(&self) -> bool {
        self.clock_pump_running
            && self
                .clock_pump_life
                .as_ref()
                .is_some_and(|life| life.strong_count() > 0)
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

    /// Keep the handle of the pump that took the claim `generation`, so a pump this service
    /// started stays abortable, and abort the EARLIER pump's handle it replaces.
    ///
    /// The handle of one pump cannot be stored in the same critical section its claim was taken
    /// in — it exists only once the task has been spawned — so a handle may arrive after a
    /// later pump has taken the claim. Such a handle belongs to a pump that has already stood
    /// down, so it is DROPPED rather than kept, and the handle the store keeps stays the one of
    /// the pump holding the claim: replacing unconditionally would abort that live pump, which
    /// would leave the claim held by a task that no longer runs.
    fn set_clock_pump(&mut self, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
        if self.clock_pump_generation != generation {
            return;
        }
        // The handle this replaces is an EARLIER pump's — one generation is minted per spawn, and
        // a handle lands here only while its own claim is still the live one — and that pump has
        // already stood down, so aborting it is what guarantees no earlier pump task runs beside
        // the one holding the claim.
        if let Some((_, earlier)) = self.clock_pump.replace((generation, handle)) {
            earlier.abort();
        }
    }
}

/// What a running pump OWNS for as long as its task is alive, so the claim it took can be told
/// from one no pump is holding any more. See this module's docs; the store keeps a weak
/// reference to it ([`ComAmazonawsGlueAwsglueState::clock_pump_life`]).
#[derive(Debug)]
pub struct ClockPumpLife;

/// Start this store's clock pump, unless one is already running.
///
/// Called by the doors that leave the store owing a transition — a job run's start or stop, a
/// crawl's start — with no store guard held. It returns at once: the waiting, the settling and
/// the publication are the spawned task's, so no request waits on the clock.
pub fn drive_clock(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsGlueAwsglueState>,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some((generation, life)) = state.lock().start_clock_pump() else {
        return;
    };
    let callback = callback.clone();
    let pumped = ::engine::ServiceState::clone(state);
    let state = ::engine::ServiceState::clone(state);
    let handle = runtime.spawn(async move {
        // Held for as long as this task runs and dropped whatever ends it, which is what lets
        // the next door tell this claim from one nobody is holding.
        let _life = life;
        // `true` only if the wake-up cap below is what ended the loop; a pump that ran out of
        // work has already released its claim inside the one critical section that observed it
        // ([`ComAmazonawsGlueAwsglueState::next_due_or_release_pump`]).
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let now = SystemTime::now();
            let Some(due) = state.lock().next_due_or_release_pump(now, generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps the
            // backstop above from ever costing a CPU-bound spin, and it hands the runtime back
            // between wake-ups.
            let wait = due
                .duration_since(now)
                .unwrap_or_default()
                .clamp(MIN_CLOCK_WAIT, MAX_CLOCK_WAIT);
            ::tokio::time::sleep(wait).await;
            {
                let settled_at = SystemTime::now();
                let mut store = state.lock();
                // The firings a schedule owed come FIRST: a trigger's firing begins runs, and
                // those runs are then given whatever the clock owes them in the same wake-up.
                store.settle_every_due_scheduled_trigger_firing(settled_at);
                store.settle_due_job_runs(settled_at);
                store.settle_due_crawls(settled_at);
            }
            publish_pending_job_run_events(&callback, &state).await;
            publish_pending_crawler_events(&callback, &state).await;
        }
        if capped {
            // Only THIS pump's claim, so a pump started since is left holding its own.
            state.lock().release_clock_pump(generation);
        }
    });
    // The handle is kept so a pump this service started stays abortable rather than detached,
    // under the claim it was spawned for ([`ComAmazonawsGlueAwsglueState::set_clock_pump`]).
    pumped.lock().set_clock_pump(generation, handle);
}
