// SPDX-License-Identifier: CC0-1.0

//! The in-process CLOCK that carries a started deployment through the stages AppConfig's
//! own schedule owes it, and through the extension action points those stages owe, without
//! a further call from the client.
//!
//! A deployment's progression is AppConfig's work, not the caller's: once `StartDeployment`
//! answers, the growth intervals of the rollout, the final bake and the completion each
//! fall due at an instant of their own, and an extension bound at `AT_DEPLOYMENT_TICK` is
//! invoked at every tick of a deployment that is still in flight — whose answer can roll
//! that deployment back ([`super::extension_actions::DIRECTIVE_ROLL_BACK`]). A client that
//! starts a deployment and then watches what its extension DOES — the queue its handler
//! forwards each tick event to, the configuration its application is served — makes no
//! second AppConfig call, so every one of those instants would be reached by nobody if only
//! the next request could reach it: no tick would ever be invoked, and an extension
//! installed as a kill switch would never be asked. This is the one task that waits for
//! those instants, so no request has to be the one that asks.
//!
//! It does not change WHEN anything happens. Each wake-up performs exactly what a read of
//! the deployment performs, in the same order and through the same functions
//! ([`super::alarm_monitors::settle`], [`super::Environment::materialize`],
//! [`super::extension_actions::drive_deployment`]), each of which applies only what was
//! already due at the instant it is called — so a caller reading a deployment before an
//! instant still sees the state the clock owed it then, and one reading it after sees the
//! same thing whether this task or that read was what materialised it.
//!
//! At most one clock runs per store (the store carries the claim, which a clock whose task
//! has ended without releasing it does not keep — [`claim`]), each clock stands down once
//! the store holds no deployment that is still being worked on, after at most
//! [`MAX_IDLE_WAKE_UPS`] wake-ups that find nothing to do, and carries at most
//! [`MAX_CARRIED`] deployments per wake-up. With no async runtime to spawn onto there is no
//! clock, and whatever a clock has not reached is materialised by the next operation
//! instead, as it always was.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::ComAmazonawsAppconfigAmazonappconfigState as ServiceStore;
use super::Deployment;

/// The shortest a wake-up waits, so an instant already past costs a yield rather than a
/// CPU-bound spin.
const MIN_TICK: Duration = Duration::from_millis(25);

/// The longest a wake-up waits, so the clock re-reads the store — which a request may have
/// changed since, by starting a deployment of its own — at least this often.
const MAX_TICK: Duration = Duration::from_secs(2);

/// How many times the wake-up floor doubles while nothing moves, so a store whose next
/// instant is minutes away is not polled at full rate.
const MAX_IDLE_SHIFT: u32 = 5;

/// How many wake-ups that find NOTHING to do one clock takes before it stands down.
///
/// A backstop, not a schedule: a deployment can be waiting out a final bake time that keeps
/// the minutes the caller declared (`super::bake_duration`), which no action point and no
/// transition falls due inside. A clock that is still performing work is not spent — every
/// wake-up that performs any resets the count — so a deployment whose ticks are being
/// invoked is carried to its terminal state rather than abandoned part way through.
const MAX_IDLE_WAKE_UPS: usize = 1_024;

/// The most deployments one wake-up carries.
///
/// Well above the deployments an account has in flight at once — an environment holds one
/// at a time — so no realistic store is cut short by it; a store holding more contributes
/// the first ones walked, and the rest are carried by the next wake-up or by a read.
const MAX_CARRIED: usize = 512;

/// Where one deployment the clock is carrying lives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Carried {
    application_id: String,
    environment_id: String,
    deployment_number: i32,
}

/// What one wake-up found still being worked on: the deployments to carry, and the earliest
/// instant any of them is owed anything at.
#[derive(Debug)]
struct Pending {
    /// The deployments to carry, each at most once.
    deployments: Vec<Carried>,
    /// The earliest instant among them at which something falls due, if any of them records
    /// one. An instant already past is kept as it is: the wake-up still waits (see
    /// [`Self::wait`]), because the floor is what bounds the rate.
    next_due: Option<SystemTime>,
}

impl Pending {
    /// How long this wake-up waits before it carries them: until the earliest instant
    /// something is due at, never less than the idle floor and never more than [`MAX_TICK`].
    fn wait(&self, idle_shift: u32) -> Duration {
        let floor = (MIN_TICK * (1 << idle_shift)).min(MAX_TICK);
        self.next_due
            .and_then(|due| due.duration_since(SystemTime::now()).ok())
            .unwrap_or_default()
            .clamp(floor, MAX_TICK)
    }
}

/// Start the one in-process task that waits for this store's earliest owed instant,
/// performs what is then due, and stands down once no deployment is still being worked on.
///
/// Called by the operations that start a deployment and that report one, so a store with a
/// deployment in flight keeps a clock running; a second call while one runs starts nothing.
pub fn drive<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Some(generation) = claim(&mut state.lock()) else {
        return;
    };
    let driven = ::engine::ServiceState::clone(&state);
    let engine = Arc::clone(engine);
    // The same in-process handle into this emulator the request itself would have invoked
    // an action's target through, owned by the task rather than borrowed from the request
    // that started it, and the Region that request was signed for: which endpoint of this
    // same emulator an action's Lambda, Amazon SQS, Amazon SNS or Amazon EventBridge call
    // goes to. WHICH target each action names is on the extension association itself.
    let callback = callback.clone();
    let region = region.to_string();
    let handle = runtime.spawn(async move {
        // `true` only if the idle cap is what ended the loop; a clock that ran out of work
        // has already released its claim inside the one critical section that observed it.
        let mut capped = true;
        let mut idle_shift = 0;
        let mut idle_wake_ups = 0;
        while idle_wake_ups < MAX_IDLE_WAKE_UPS {
            let pending = pending_or_release(&mut driven.lock(), generation, SystemTime::now());
            let Some(pending) = pending else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps a
            // due instant from costing a CPU-bound spin, and it hands the runtime back
            // between wake-ups.
            ::tokio::time::sleep(pending.wait(idle_shift)).await;
            let performed = carry(&engine, &callback, &region, &pending.deployments).await;
            idle_shift = match performed {
                0 => (idle_shift + 1).min(MAX_IDLE_SHIFT),
                _ => 0,
            };
            // A wake-up that performed work leaves the clock as fresh as a new one, so only
            // the ones that found nothing count against the cap.
            idle_wake_ups = match performed {
                0 => idle_wake_ups + 1,
                _ => 0,
            };
        }
        if capped {
            // A deployment is still being worked on and this clock is spent, so say so:
            // from here what it owes waits for the next call to this service, which is the
            // one thing a client watching only its extension's target never makes.
            ::tracing::warn!(
                "the deployment clock stood down after {MAX_IDLE_WAKE_UPS} wake-ups that \
                 found nothing due while a deployment was still being worked on; the next \
                 call to this service performs what it owes"
            );
            // Only THIS clock's claim, so a clock started since is left holding its own.
            release(&mut driven.lock(), generation);
        }
    });
    set_clock(&mut state.lock(), generation, handle);
}

/// Perform everything the clock owes `deployments` at this instant, and answer how much
/// work that was: the transitions it materialised plus the action-point invocations it
/// made.
///
/// Exactly what a read of a deployment performs, in the same order: the rollback a
/// monitored alarm has already made due, the stages the strategy's own durations have
/// already made due, and then the action points those stages and the tick interval owe.
async fn carry<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    deployments: &[Carried],
) -> usize {
    let now = SystemTime::now();
    super::alarm_monitors::settle(&**engine, now);
    let mut performed = materialize_due(&**engine, now, deployments);
    for carried in deployments {
        performed += super::extension_actions::drive_deployment(
            &**engine,
            callback,
            region,
            &carried.application_id,
            &carried.environment_id,
            carried.deployment_number,
        )
        .await;
    }
    performed
}

/// What one deployment reports of its own progress: the state it is in, the share of
/// targets it is available to, and how many events it has logged.
type Progress = (String, f32, usize);

/// Materialise the stages the clock has already made due for the environments holding
/// `deployments`, and answer how many of those deployments it moved.
///
/// The materialisation itself is the environment's own ([`super::Environment::materialize`],
/// the same call every read of a deployment makes); this only reports whether it moved
/// anything, which is what tells the clock it is still doing work.
fn materialize_due<Eng: Send + Sync + 'static>(
    engine: &Eng,
    now: SystemTime,
    deployments: &[Carried],
) -> usize {
    let Some(state) = super::service_state(engine) else {
        return 0;
    };
    let mut store = state.lock();
    let before = progress_of(&store, deployments);
    for carried in deployments {
        if let Some(environment) = store
            .application_mut(&carried.application_id)
            .and_then(|application| application.environments.get_mut(&carried.environment_id))
        {
            environment.materialize(now);
        }
    }
    let after = progress_of(&store, deployments);
    before
        .iter()
        .filter(|(carried, progress)| after.get(*carried) != Some(progress))
        .count()
}

/// What each of `deployments` reports of its own progress right now, for the ones the store
/// still holds.
fn progress_of(store: &ServiceStore, deployments: &[Carried]) -> BTreeMap<Carried, Progress> {
    deployments
        .iter()
        .filter_map(|carried| {
            let deployment = store
                .application(&carried.application_id)?
                .environments
                .get(&carried.environment_id)?
                .deployments
                .get(&carried.deployment_number)?;
            Some((
                carried.clone(),
                (
                    deployment.state.clone(),
                    deployment.percentage_complete,
                    deployment.event_log.len(),
                ),
            ))
        })
        .collect()
}

/// What is still being worked on — or `None`, having RELEASED the claim, when nothing is.
///
/// Observing "nothing is being worked on" and standing down happen in ONE critical section,
/// which is what makes the hand-over race-free: a call that starts a deployment holds this
/// same lock to store it BEFORE it tries to claim the clock, so either this call sees that
/// deployment and the running clock keeps it, or the starter's own claim finds the flag
/// cleared and starts a clock of its own.
fn pending_or_release(
    store: &mut ServiceStore,
    generation: u64,
    now: SystemTime,
) -> Option<Pending> {
    let mut pending = Pending {
        deployments: Vec::new(),
        next_due: None,
    };
    for application in store.all_applications() {
        for environment in application.environments.values() {
            for deployment in environment.deployments.values() {
                if pending.deployments.len() >= MAX_CARRIED {
                    break;
                }
                // A deployment the service has finished with is still being worked on while
                // it owes the action point of the stage that finished it.
                let owes_a_point =
                    super::extension_actions::owes_an_action_point(deployment, now);
                if !deployment.is_active() && !owes_a_point {
                    continue;
                }
                pending.deployments.push(Carried {
                    application_id: environment.application_id.clone(),
                    environment_id: environment.id.clone(),
                    deployment_number: deployment.deployment_number,
                });
                let due = if owes_a_point {
                    Some(now)
                } else {
                    next_due_of(deployment)
                };
                pending.next_due = pending.next_due.into_iter().chain(due).min();
            }
        }
    }
    if pending.deployments.is_empty() {
        release(store, generation);
        return None;
    }
    Some(pending)
}

/// The earliest instant `deployment` is owed anything at, or `None` when it is owed
/// nothing.
///
/// Read off the deployment's own schedule rather than from a clock of this module's: the
/// rollback it is working through falls due when the record says it does, and a deployment
/// in flight is owed its first unreached growth interval, the end of its rollout, its
/// completion and its next tick — the same instants a read of it materialises against.
fn next_due_of(deployment: &Deployment) -> Option<SystemTime> {
    if deployment.is_rolling_back() {
        return deployment.rollback_completion_due;
    }
    if !deployment.is_in_flight() {
        return None;
    }
    // The instant the rollout ends, which is also where the walk below stops. Added with
    // the checked arithmetic every instant in this module uses, so a stored span nothing
    // can represent the end of reads as "already there" rather than panicking.
    let rollout_end = deployment
        .started_at
        .checked_add(deployment.rollout_duration())
        .unwrap_or(deployment.started_at);
    let mut owed = vec![rollout_end, deployment.monitored_until()];
    owed.extend(deployment.next_tick_due);
    let interval = deployment.growth_interval();
    // The growth intervals of the rollout fall due at multiples of the interval, so the
    // EARLIEST of them is the first one — the only one a minimum can come from. Nothing
    // enumerates them: how many intervals a rollout holds follows from the strategy a
    // caller created, so walking the grid would be work a request chose the size of.
    if let Some(first_interval) = (!interval.is_zero())
        .then(|| deployment.started_at.checked_add(interval))
        .flatten()
        .filter(|step| *step < rollout_end)
    {
        owed.push(first_interval);
    }
    owed.into_iter().min()
}

/// Claim the single clock for this store: the claim's GENERATION when the caller may start
/// one, `None` when one is really running.
///
/// The claim is taken over from a clock whose TASK HAS ALREADY ENDED while still holding
/// it. A clock releases its claim itself on the two ends it can see coming — nothing left
/// to carry, and the idle cap — and [`release`] drops the handle with the claim, so a claim
/// still held while the task is over belongs to a clock that ended on neither: one wake-up's
/// work panicked, or the task was cancelled. Without the take-over that claim is held for as
/// long as this store lives, and because the claim is what stops a second clock from
/// starting, no later call could ever start one again.
///
/// A granted claim takes the OLD handle out with it, so the window between this call and the
/// new handle being installed ([`set_clock`]) holds no handle at all: a call that reaches
/// this one inside that window sees a held claim and no ended task, and starts nothing. One
/// clock runs at a time.
fn claim(store: &mut ServiceStore) -> Option<u64> {
    let ended = store
        .deployment_clock
        .as_ref()
        .is_some_and(::tokio::task::JoinHandle::is_finished);
    if store.deployment_clock_running && !ended {
        return None;
    }
    if ended && store.deployment_clock_running {
        ::tracing::warn!(
            "the deployment clock this store holds the claim for has ended without releasing \
             it; the claim is taken over so what it owes is still performed"
        );
    }
    // The task this handle names has ended, or has released its claim and is ending, so
    // nothing is lost by dropping it; the abort is what makes that true in the second case
    // as well.
    if let Some(previous) = store.deployment_clock.take() {
        previous.abort();
    }
    store.deployment_clock_running = true;
    store.deployment_clock_generation = store.deployment_clock_generation.wrapping_add(1);
    Some(store.deployment_clock_generation)
}

/// Release the claim `generation` took, and the handle with it — and ONLY that one, so a
/// clock that ended on the idle cap cannot clear a claim a clock started since is holding.
fn release(store: &mut ServiceStore, generation: u64) {
    if store.deployment_clock_generation == generation {
        store.deployment_clock_running = false;
        store.deployment_clock = None;
    }
}

/// Keep the running clock's task handle, aborting the one it replaces — and only while
/// `generation` is still the CURRENT claim, so a starter whose own task has already stood
/// down installs nothing but its own abort.
///
/// An abort leaves exactly what a request dropped mid-flight leaves: an action point marked
/// as reached whose invocation did not finish, which is what the service records for an
/// invocation that did not run either way. Nothing is reported as done for work that did not
/// happen, because every outcome is stored in one critical section after the work returns.
fn set_clock(
    store: &mut ServiceStore,
    generation: u64,
    handle: ::tokio::task::JoinHandle<()>,
) {
    // A claim that is no longer the current one, or no longer held at all (the task has
    // already run out of work and released it), installs nothing: the handle and the claim
    // are written together, so the store never holds one without the other.
    if store.deployment_clock_generation != generation || !store.deployment_clock_running {
        handle.abort();
        return;
    }
    if let Some(previous) = store.deployment_clock.replace(handle) {
        previous.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wait is bounded at both ends: an instant already past costs the floor, a distant
    /// one costs no more than the ceiling, and the floor doubles as the clock idles.
    #[test]
    fn a_wake_up_waits_between_the_floor_and_the_ceiling() {
        let rows: [(&str, Option<SystemTime>, u32, Duration); 4] = [
            ("nothing due at all", None, 0, MIN_TICK),
            (
                "an instant already past",
                Some(SystemTime::UNIX_EPOCH),
                0,
                MIN_TICK,
            ),
            (
                "an instant an hour out",
                Some(SystemTime::now() + Duration::from_secs(3_600)),
                0,
                MAX_TICK,
            ),
            ("the floor doubled while idle", None, 3, MIN_TICK * 8),
        ];
        for (case, next_due, idle_shift, expected) in rows {
            let pending = Pending {
                deployments: Vec::new(),
                next_due,
            };
            let actual = pending.wait(idle_shift);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected a wait of {expected:?}, got {actual:?}"
            );
        }
    }

    /// One claim at a time, and a released claim is a claim the next caller may take.
    #[test]
    fn the_store_grants_one_claim_at_a_time() {
        let mut store = ServiceStore::default();
        let first = claim(&mut store);
        assert_eq!(
            first,
            Some(1),
            "case \"the first claim\": expected the first generation, got {first:?}"
        );
        let second = claim(&mut store);
        assert_eq!(
            second, None,
            "case \"a second claim while one is held\": expected no claim, got {second:?}"
        );
        // A clock that has stood down releases only its OWN claim.
        release(&mut store, 0);
        assert!(
            store.deployment_clock_running,
            "case \"a stale release\": a claim another generation holds must stand"
        );
        release(&mut store, 1);
        let third = claim(&mut store);
        assert_eq!(
            third,
            Some(2),
            "case \"after the claim was released\": expected the next generation, got {third:?}"
        );
    }
}
