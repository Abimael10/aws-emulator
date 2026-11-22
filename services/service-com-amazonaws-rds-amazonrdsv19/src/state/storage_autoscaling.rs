// SPDX-License-Identifier: CC0-1.0

//! STORAGE AUTOSCALING of a DB instance: raising `AllocatedStorage` toward
//! `MaxAllocatedStorage` from the storage the instance's engine is actually using.
//!
//! `MaxAllocatedStorage` is "the upper limit in gibibytes (GiB) to which Amazon RDS can
//! automatically scale the storage of the DB instance"
//! (`CreateDBInstanceMessage$MaxAllocatedStorage`), and setting it is the whole of what a client
//! does to ask for autoscaling: RDS then raises the storage ITSELF, with no
//! `ModifyDBInstance(AllocatedStorage=…)` call, once the instance is running out of room. A
//! client that sets the member instead of pre-provisioning has no other way to be given more
//! storage, so an instance whose `AllocatedStorage` never moves runs its database out of disk.
//!
//! WHAT MAKES IT DUE is the storage the instance is USING, which the engine behind its endpoint
//! accounts for ([`data_plane::sql_engine::InstanceDatabase::stored_bytes`]) — never the number
//! of calls, and never who called. Amazon RDS starts a storage modification when all of the
//! following hold, which is what this module materialises:
//!
//! * free available space is less than [`LOW_STORAGE_FREE_SHARE`] of the allocated storage;
//! * that low-storage condition has lasted at least [`LOW_STORAGE_WINDOW`]; and
//! * at least [`AUTOSCALING_COOLDOWN`] has passed since the last storage modification.
//!
//! The increment is the larger of [`MIN_STORAGE_INCREMENT_GIB`] and
//! [`STORAGE_INCREMENT_SHARE`] of the storage currently allocated, and it never passes the
//! `MaxAllocatedStorage` the instance carries: "the maximum amount of storage to which the DB
//! instance can grow".
//!
//! Where that leaves an instance this emulator serves NO engine wire for (an Oracle or SQL
//! Server instance, whose data this process does not hold): nothing is using its storage here,
//! so there is no low-storage condition to observe and its storage is never raised — the
//! measurement is the engine's, and no number is invented in its place.

use std::time::{Duration, Instant};

use super::{ComAmazonawsRdsAmazonrdsv19State, RegionalKey};

/// How much of its allocated storage an instance must have FREE to be clear of the
/// low-storage condition: Amazon RDS starts a storage modification when free available space
/// is less than 10 percent of the allocated storage (Amazon RDS User Guide, "Managing capacity
/// automatically with Amazon RDS storage autoscaling").
pub const LOW_STORAGE_FREE_SHARE: f64 = 0.10;

/// How long the low-storage condition must LAST before the storage is raised: the same
/// published rule's five minutes, which is what keeps a burst that frees its space again from
/// growing the instance.
pub const LOW_STORAGE_WINDOW: Duration = Duration::from_secs(300);

/// How long after one storage modification the next may happen: the same published rule's six
/// hours.
pub const AUTOSCALING_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);

/// The smallest raise one storage modification makes, in GiB: the published rule's 10 GiB.
pub const MIN_STORAGE_INCREMENT_GIB: i32 = 10;

/// The share of the currently allocated storage one raise adds where that is more than
/// [`MIN_STORAGE_INCREMENT_GIB`]: the published rule's 10 percent.
pub const STORAGE_INCREMENT_SHARE: i32 = 10;

/// One gibibyte in bytes, the unit `AllocatedStorage` counts in and the engine's measurement is
/// converted from.
const BYTES_PER_GIB: i64 = 1024 * 1024 * 1024;

/// How much of an instance's allocated storage is still FREE, as a share of it, given the
/// `used_bytes` its engine has allocated.
///
/// Zero for an instance using at least everything it has, and one for an empty one. An
/// instance with no allocated storage at all has nothing free.
#[must_use]
pub fn free_storage_share(allocated_gib: i32, used_bytes: i64) -> f64 {
    let allocated = i64::from(allocated_gib.max(0)).saturating_mul(BYTES_PER_GIB);
    if allocated <= 0 {
        return 0.0;
    }
    let free = allocated.saturating_sub(used_bytes.max(0));
    (free as f64 / allocated as f64).clamp(0.0, 1.0)
}

/// Whether an instance with `allocated_gib` of storage using `used_bytes` of it is IN the
/// low-storage condition: less than [`LOW_STORAGE_FREE_SHARE`] of its storage is free.
#[must_use]
pub fn is_low_on_storage(allocated_gib: i32, used_bytes: i64) -> bool {
    free_storage_share(allocated_gib, used_bytes) < LOW_STORAGE_FREE_SHARE
}

/// The `AllocatedStorage` a storage modification of an instance at `allocated_gib` raises it
/// to, bounded by the `max_gib` it may grow to — or `None` when there is no room to grow.
///
/// The larger of [`MIN_STORAGE_INCREMENT_GIB`] and [`STORAGE_INCREMENT_SHARE`] percent of what
/// is allocated now, never past `MaxAllocatedStorage`: an instance already at its maximum is
/// raised no further, which is what makes the member a bound rather than a target.
#[must_use]
pub fn autoscaled_allocated_storage(allocated_gib: i32, max_gib: i32) -> Option<i32> {
    if max_gib <= allocated_gib {
        return None;
    }
    let increment = MIN_STORAGE_INCREMENT_GIB.max(allocated_gib / STORAGE_INCREMENT_SHARE);
    let raised = allocated_gib.saturating_add(increment).min(max_gib);
    (raised > allocated_gib).then_some(raised)
}

/// How long one DB instance has been in the low-storage condition, and when its storage was
/// last raised.
///
/// Kept per instance under its IMMUTABLE resource id, so a rename cannot orphan one and cannot
/// give an instance a second clock.
#[derive(Debug, Default, Clone)]
pub struct StorageAutoscalingClock {
    /// When the instance was first observed low on storage, or `None` while it has room.
    low_since: Option<Instant>,
    /// When its storage was last raised by this service, or `None` while it never was.
    raised_at: Option<Instant>,
}

impl StorageAutoscalingClock {
    /// Whether a storage modification is DUE at `now` for an instance that has been low on
    /// storage since [`Self::low_since`]: the condition has lasted its window, and the
    /// cooldown since the last raise has passed.
    fn raise_is_due(&self, now: Instant) -> bool {
        let lasted = self
            .low_since
            .is_some_and(|since| now.saturating_duration_since(since) >= LOW_STORAGE_WINDOW);
        let cooled = self
            .raised_at
            .is_none_or(|raised| now.saturating_duration_since(raised) >= AUTOSCALING_COOLDOWN);
        lasted && cooled
    }

    /// What one pass owes an instance at `allocated_gib` of storage that may grow to `max_gib`
    /// and is using `used_bytes` of it, at `now`: the storage it is RAISED to, or `None` when
    /// nothing is owed yet.
    ///
    /// The whole of the decision, and the only place this clock moves: an instance with room
    /// again clears the condition it was building up, one that has just become low starts the
    /// window, and one that has been low for the window — and is past its cooldown — is raised
    /// and starts its window again from the storage it has NOW.
    fn decide(
        &mut self,
        allocated_gib: i32,
        max_gib: i32,
        used_bytes: i64,
        now: Instant,
    ) -> Option<i32> {
        if !is_low_on_storage(allocated_gib, used_bytes) {
            self.low_since = None;
            return None;
        }
        let _ = self.low_since.get_or_insert(now);
        if !self.raise_is_due(now) {
            return None;
        }
        let target = autoscaled_allocated_storage(allocated_gib, max_gib)?;
        self.raised_at = Some(now);
        self.low_since = None;
        Some(target)
    }
}

/// What one pass observed about ONE DB instance that keeps storage autoscaling on: the record it
/// belongs to, and the storage it has and is using.
///
/// `pub` so the crate's own suites can drive [`ComAmazonawsRdsAmazonrdsv19State::apply_storage_autoscaling`]
/// at an instant of their own: the low-storage condition the raise answers takes the published
/// five-minute window to build up, which no test can wait out.
#[derive(Debug, Clone)]
pub struct ObservedStorage {
    /// The instance's key in the store, which is what a raise is written through.
    pub key: RegionalKey,
    /// Its immutable `DbiResourceId`, which its clock is kept under.
    pub resource_id: String,
    /// `AllocatedStorage`, in GiB.
    pub allocated: i32,
    /// `MaxAllocatedStorage`, in GiB.
    pub max: i32,
    /// How many bytes of that storage the engine has allocated.
    pub used_bytes: i64,
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// Materialise the storage autoscaling the clock owes the DB instances of `region`.
    ///
    /// Called from [`Self::settle_due_transitions`] with the instance statuses already settled,
    /// so what this reads is the state that was already due. Nothing here depends on who
    /// called: the condition is the storage the engine is using and the spans the stored
    /// instants have already crossed, so an instance that ran out of room while no request was
    /// in flight is raised the moment the next one arrives — exactly as it would have been had
    /// a request been in flight all along.
    pub(super) fn settle_storage_autoscaling(&mut self, region: &str) {
        let observed = self.observed_storage(region);
        self.apply_storage_autoscaling(region, &observed, Instant::now());
    }

    /// What each instance of `region` that keeps autoscaling on HAS and is USING, read before
    /// anything is written so the measurement and the record cannot disagree.
    ///
    /// Reads one region's ordered range, bounded by that region's DB instance quota, and asks
    /// the engine for its page count only for the instances that have autoscaling turned on at
    /// all: an instance with no `MaxAllocatedStorage`, or one already at it, is not measured —
    /// and neither is one this emulator serves no engine for, whose storage nothing here holds.
    fn observed_storage(&self, region: &str) -> Vec<ObservedStorage> {
        self.instances_in(region)
            .into_iter()
            .filter_map(|instance| {
                let allocated = instance.allocated_storage?;
                let max = instance.max_allocated_storage?;
                if max <= allocated {
                    return None;
                }
                let key: RegionalKey = (region.to_string(), instance.identifier.clone());
                // The engine behind the instance's own endpoint is what accounts for the
                // storage its data takes.
                let used_bytes = self
                    .engine_endpoint(&key)
                    .map(|endpoint| endpoint.database().stored_bytes())?;
                Some(ObservedStorage {
                    key,
                    resource_id: instance.dbi_resource_id.clone(),
                    allocated,
                    max,
                    used_bytes,
                })
            })
            .collect()
    }

    /// Move each observed instance's clock on at `now` and write the raises that are due.
    ///
    /// The whole decision, over the observations alone, so it is the one place the condition,
    /// the window, the cooldown and the write meet — and so a test can drive it at an instant of
    /// its own rather than waiting out a five-minute window.
    pub fn apply_storage_autoscaling(
        &mut self,
        region: &str,
        observed: &[ObservedStorage],
        now: Instant,
    ) {
        let mut raised: Vec<(RegionalKey, i32)> = Vec::new();
        for instance in observed {
            let clock_key: RegionalKey = (region.to_string(), instance.resource_id.clone());
            let clock = self
                .storage_autoscaling
                .entry(clock_key)
                .or_default();
            if let Some(target) = clock.decide(instance.allocated, instance.max, instance.used_bytes, now) {
                raised.push((instance.key.clone(), target));
            }
        }
        for (key, target) in raised {
            if let Some(instance) = self.db_instances.get_mut(&key) {
                instance.allocated_storage = Some(target);
            }
        }
        // The clocks of the instances this store no longer holds go with them.
        let held: ::std::collections::BTreeSet<String> = self
            .instances_in(region)
            .into_iter()
            .map(|instance| instance.dbi_resource_id.clone())
            .collect();
        self.storage_autoscaling
            .retain(|(stored, resource_id), _| stored != region || held.contains(resource_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the condition table: an instance's storage, what it is using, whether that is
    /// the low-storage condition, and the storage a raise would take it to.
    struct Row {
        case: &'static str,
        allocated: i32,
        max: i32,
        used_bytes: i64,
        low: bool,
        raised_to: Option<i32>,
    }

    /// RULE: an instance is in the low-storage condition exactly while less than a tenth of its
    /// allocated storage is free, and the raise it is then owed is the larger of 10 GiB and a
    /// tenth of what it has, bounded by its `MaxAllocatedStorage`.
    #[test]
    fn the_condition_and_the_raise_follow_the_published_rule() {
        // The rows span what the rule generalises over: how full the storage is (empty, half,
        // exactly at the threshold, past it, and completely full), the size of the instance
        // (one where the 10 GiB floor is the larger increment and one where the tenth is), and
        // how much room `MaxAllocatedStorage` leaves (plenty, less than one increment, and
        // none at all).
        let rows: &[Row] = &[
            Row {
                case: "an empty instance",
                allocated: 20,
                max: 100,
                used_bytes: 0,
                low: false,
                raised_to: Some(30),
            },
            Row {
                case: "half full",
                allocated: 20,
                max: 100,
                used_bytes: 10 * BYTES_PER_GIB,
                low: false,
                raised_to: Some(30),
            },
            Row {
                case: "exactly a tenth free, which is not yet below it",
                allocated: 20,
                max: 100,
                used_bytes: 18 * BYTES_PER_GIB,
                low: false,
                raised_to: Some(30),
            },
            Row {
                case: "just past the threshold",
                allocated: 20,
                max: 100,
                used_bytes: 19 * BYTES_PER_GIB,
                low: true,
                raised_to: Some(30),
            },
            Row {
                case: "completely full",
                allocated: 20,
                max: 100,
                used_bytes: 20 * BYTES_PER_GIB,
                low: true,
                raised_to: Some(30),
            },
            Row {
                case: "a large instance, where a tenth is the larger increment",
                allocated: 500,
                max: 1_000,
                used_bytes: 499 * BYTES_PER_GIB,
                low: true,
                raised_to: Some(550),
            },
            Row {
                case: "less room than one increment leaves",
                allocated: 20,
                max: 25,
                used_bytes: 19 * BYTES_PER_GIB,
                low: true,
                raised_to: Some(25),
            },
            Row {
                case: "already at its maximum",
                allocated: 100,
                max: 100,
                used_bytes: 99 * BYTES_PER_GIB,
                low: true,
                raised_to: None,
            },
            Row {
                case: "no storage allocated at all",
                allocated: 0,
                max: 100,
                used_bytes: 0,
                low: true,
                raised_to: Some(10),
            },
        ];
        for row in rows {
            let case = row.case;
            let observed = is_low_on_storage(row.allocated, row.used_bytes);
            assert_eq!(
                observed, row.low,
                "case {case:?}: expected low_on_storage {}, got {observed} (free share {})",
                row.low,
                free_storage_share(row.allocated, row.used_bytes)
            );
            let target = autoscaled_allocated_storage(row.allocated, row.max);
            assert_eq!(
                target, row.raised_to,
                "case {case:?}: expected a raise to {:?}, got {target:?}",
                row.raised_to
            );
            if let Some(target) = target {
                assert!(
                    target <= row.max && target > row.allocated,
                    "case {case:?}: a raise stays inside MaxAllocatedStorage and moves the \
                     storage on, got {target}"
                );
            }
        }
    }

    /// RULE: the raise is owed only once the low-storage condition has lasted its window, it
    /// takes the instance to the storage the published increment names, and the next one waits
    /// out the cooldown; an instance that gets room back before the window is up is not raised
    /// at all.
    #[test]
    fn one_clock_raises_after_the_window_and_then_waits_out_the_cooldown() {
        let start = Instant::now();
        let mut clock = StorageAutoscalingClock::default();
        // Room to spare: nothing is owed, and the condition has not begun.
        assert_eq!(
            clock.decide(20, 100, 0, start),
            None,
            "case an instance with room: no raise is owed"
        );
        assert_eq!(
            clock.low_since, None,
            "case an instance with room: the condition has not begun"
        );
        // Low on storage, but only just: the window has to pass first.
        assert_eq!(
            clock.decide(20, 100, 19 * BYTES_PER_GIB, start),
            None,
            "case the condition has just begun: no raise is owed yet"
        );
        assert!(
            clock.low_since.is_some(),
            "case the condition has just begun: the window is running"
        );
        // It frees space again before the window is up, so the condition is over and the
        // instance is not raised when the window would have passed.
        let freed = start + LOW_STORAGE_WINDOW / 2;
        assert_eq!(
            clock.decide(20, 100, 10 * BYTES_PER_GIB, freed),
            None,
            "case it has room again: no raise is owed"
        );
        assert_eq!(
            clock.low_since, None,
            "case it has room again: the condition is over"
        );
        // Low again, and this time it stays low for the whole window: the raise is owed, and it
        // is the published increment.
        let low_again = freed + LOW_STORAGE_WINDOW;
        assert_eq!(
            clock.decide(20, 100, 19 * BYTES_PER_GIB, low_again),
            None,
            "case low again: the window starts over"
        );
        let due = low_again + LOW_STORAGE_WINDOW;
        assert_eq!(
            clock.decide(20, 100, 19 * BYTES_PER_GIB, due),
            Some(30),
            "case the condition has lasted its window: the instance is raised by the \
             published increment"
        );
        assert_eq!(
            clock.low_since, None,
            "case just raised: the window starts again from the storage it has now"
        );
        // Still low on the NEW storage: the window starts over from the raise, and inside the
        // cooldown the raise is refused even once that window HAS elapsed — which is the one
        // leg where the cooldown, and not the window, is what withholds it.
        let low_after_the_raise = due + Duration::from_secs(1);
        assert_eq!(
            clock.decide(30, 100, 29 * BYTES_PER_GIB, low_after_the_raise),
            None,
            "case low again right after a raise: the window starts over"
        );
        let window_elapsed_inside_the_cooldown = low_after_the_raise + LOW_STORAGE_WINDOW;
        assert!(
            window_elapsed_inside_the_cooldown < due + AUTOSCALING_COOLDOWN,
            "the case this row tests needs the window to elapse INSIDE the cooldown"
        );
        assert_eq!(
            clock.decide(
                30,
                100,
                29 * BYTES_PER_GIB,
                window_elapsed_inside_the_cooldown
            ),
            None,
            "case the window has elapsed but the cooldown has not: no second raise is owed"
        );
        // And past the cooldown, with the condition still standing, the next raise is owed.
        let past = due + AUTOSCALING_COOLDOWN;
        assert_eq!(
            clock.decide(30, 100, 29 * BYTES_PER_GIB, past),
            Some(40),
            "case past the cooldown: the next raise is owed"
        );
    }
}
