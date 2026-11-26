// SPDX-License-Identifier: CC0-1.0

//! The clock that materialises a lifecycle change this service answered BEFORE performing
//! it, and publishes the event that change records.
//!
//! Amazon RDS answers a reboot, a stop and a start while the change is still to come: the
//! instance is reported in its transient status (`rebooting`, `stopping`, `starting`) and the
//! clock owes it the status it is heading for. The event of such a change names the state the
//! instance REACHES, so it is recorded — and published onto the account's default bus — when
//! that transition falls due ([`super::ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`]
//! is what records it), which is AFTER the response that started it was served.
//!
//! Every other settle in this crate is driven by the next call that reads the resource, which
//! is enough for a client polling the control plane. A consumer of the EVENT BUS is not a
//! caller of this service at all: nothing it does would bring that settle about, so without a
//! clock of its own the change it is waiting for would be published only if somebody happened
//! to call Amazon RDS again. This module is that clock — the ONE place a change this service
//! owes is materialised with no request of its own.
//!
//! One clock per REGION, claimed under the store's own lock, so however many calls start a
//! transition there is at most one task per Region and at most [`super::MAX_REGIONS`] in all.
//! It holds the engine WEAKLY and upgrades it per wake-up, so a clock cannot keep an emulator
//! nobody else holds — its listeners, its engines and its stores — alive, and it stands down
//! with it. Its task handle is kept on the store, so a clock a later call replaces is aborted
//! rather than left running, and every wake-up WAITS rather than spinning. With no async
//! runtime to spawn onto there is no clock, and the change is then materialised by the next
//! call that reads the instance, exactly as it was before.
//!
//! Nothing it does changes what the caller of the handler that started it was told.

use ::std::sync::Arc;
use ::std::time::{Duration, Instant};

use super::*;

/// How many times one clock wakes up before it stands down.
///
/// A backstop on the work one request can start, not a schedule: the next call that starts a
/// transition claims a clock again if none is running. Generous next to the number of changes
/// one Region's DB instance quota can have in flight.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// The shortest a wake-up waits, so a change already past its due instant costs a yield
/// rather than a CPU-bound spin, whatever the store holds.
const MIN_WAKE_UP_WAIT: Duration = Duration::from_millis(25);

/// The one clock of a Region: which claim is running, and the task that holds it.
#[derive(Debug, Default)]
pub struct LifecycleEventClock {
    /// Whether a clock is running for this Region.
    running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot release a
    /// claim a later one took.
    generation: u64,
    /// The running clock's task, held so the task a request started stays abortable.
    task: Option<::tokio::task::JoinHandle<()>>,
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The earliest instant at which a settle in `region` will record a lifecycle event, or
    /// `None` when that Region owes no such change.
    ///
    /// Walks that one Region's ordered range — bounded by its DB instance quota — never the
    /// whole store, exactly as [`Self::settle_due_transitions`] does.
    #[must_use]
    pub fn next_lifecycle_event_due_at(&self, region: &str) -> Option<Instant> {
        self.db_instances
            .range((region.to_string(), String::new())..)
            .take_while(|((stored, _), _)| stored == region)
            .filter_map(|(_, instance)| instance.lifecycle_event_due_at())
            .map(|(_, due_at)| due_at)
            .min()
    }
}

/// Claim the one clock of `region`: the task that waits out each change this service has
/// answered but not performed yet, materialises it when it falls due, and publishes the
/// events it recorded.
///
/// Called by the handler that accepted such a change, with this service's store already
/// released — the publication reaches EventBridge's own handler, and a target of a matching
/// rule may call back into this service, so nothing of this store may be held across it (see
/// [`super::bus_events`]) — and the claim itself takes that same lock. A change already due
/// when this is called is materialised by the clock's FIRST wake-up, so a handler never
/// settles or publishes anything itself.
pub fn drive_due_lifecycle_events<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = claim(&mut state.lock(), region) else {
        return;
    };
    let waiting = Arc::downgrade(engine);
    let callback = callback.clone();
    let claimed = ::engine::ServiceState::clone(&state);
    let standing_down = ::engine::ServiceState::clone(&state);
    let own_region = region.to_string();
    let region = own_region.clone();
    let task = runtime.spawn(async move {
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            // The emulator this clock serves, for as long as it is still there.
            let Some(alive) = waiting.upgrade() else {
                break;
            };
            // Observing "nothing is owed" and standing down happen in ONE critical section,
            // which is what makes the hand-over race-free: a handler holds this same lock to
            // store its transient status BEFORE it tries to claim the clock, so either this
            // read sees that change and the running clock keeps it, or the handler's claim
            // finds the flag cleared and starts a clock of its own.
            let Some(due_at) = next_wait_or_release(&mut state.lock(), generation, &region) else {
                return;
            };
            drop(alive);
            // Every wake-up WAITS, even for an instant already past: that is what keeps a due
            // change from costing a CPU-bound spin, and it hands the runtime back in between.
            let wait = due_at
                .saturating_duration_since(Instant::now())
                .max(MIN_WAKE_UP_WAIT);
            ::tokio::time::sleep(wait).await;
            let Some(alive) = waiting.upgrade() else {
                break;
            };
            settle_and_publish(&*alive, &callback, &region).await;
        }
        // Only THIS clock's claim, so a clock started since keeps its own.
        release(&mut standing_down.lock(), generation, &region);
    });
    install(&mut claimed.lock(), generation, &own_region, task);
}

/// Materialise the transitions `region` already owes and publish every event they recorded.
///
/// The guard is bound to the statement that settles, so it is released before the first
/// publication is awaited.
async fn settle_and_publish<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) {
    if let Some(state) = service_state(engine) {
        state.lock().settle_due_transitions(region);
    }
    bus_events::publish_recorded_events(engine, callback).await;
}

/// Claim the single clock of `region`: the claim's GENERATION when the caller may start one,
/// `None` when one already runs.
fn claim(store: &mut ComAmazonawsRdsAmazonrdsv19State, region: &str) -> Option<u64> {
    // A Region this store holds no record for owes no change either, so no clock is claimed
    // for it and the map cannot grow past the regions the store already counts.
    if !store.lifecycle_event_clocks.contains_key(region) && !store.has_room_for_region(region) {
        return None;
    }
    let clock = store
        .lifecycle_event_clocks
        .entry(region.to_string())
        .or_default();
    if clock.running {
        return None;
    }
    clock.running = true;
    clock.generation = clock.generation.wrapping_add(1);
    Some(clock.generation)
}

/// Keep the running clock's task handle, aborting the one it replaces — and only while
/// `generation` is still the CURRENT claim, so a starter whose own task has already stood down
/// installs nothing and aborts nothing.
fn install(
    store: &mut ComAmazonawsRdsAmazonrdsv19State,
    generation: u64,
    region: &str,
    task: ::tokio::task::JoinHandle<()>,
) {
    let Some(clock) = store.lifecycle_event_clocks.get_mut(region) else {
        task.abort();
        return;
    };
    if clock.generation != generation {
        task.abort();
        return;
    }
    if let Some(previous) = clock.task.replace(task) {
        previous.abort();
    }
}

/// When this clock's next wake-up is due, or `None` — having RELEASED its claim — when
/// `region` owes no change that records an event.
fn next_wait_or_release(
    store: &mut ComAmazonawsRdsAmazonrdsv19State,
    generation: u64,
    region: &str,
) -> Option<Instant> {
    match store.next_lifecycle_event_due_at(region) {
        Some(due_at) => Some(due_at),
        None => {
            release(store, generation, region);
            None
        }
    }
}

/// Release the claim `generation` took on `region`'s clock — and ONLY that one, so a clock
/// that ended on the wake-up cap cannot clear a claim a clock started since is holding.
fn release(store: &mut ComAmazonawsRdsAmazonrdsv19State, generation: u64, region: &str) {
    let Some(clock) = store.lifecycle_event_clocks.get_mut(region) else {
        return;
    };
    if clock.generation == generation {
        clock.running = false;
        // The task standing down is this one, so the handle it left is nothing to abort.
        clock.task = None;
    }
}
