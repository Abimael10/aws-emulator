// SPDX-License-Identifier: CC0-1.0

//! How many runs of CALLER-SUPPLIED CODE this process performs at once.
//!
//! Two things this service does are runs of a caller's own program in the contained
//! interpreter this workspace embeds: the inference code a model artifact carries
//! ([`super::inference_script`]) and the program a job's container declares
//! ([`super::container_python`]). Each run stands an interpreter up and holds a thread of this
//! process, so without a count of them the ceilings each run is given — its memory, its steps,
//! its answer — would be multiplied by however many calls arrive at once. The count is stated
//! here ONCE, for both, rather than per caller.
//!
//! It is a counting semaphore a caller can wait on with NO async runtime, because the run it
//! guards is performed synchronously on a thread of its own: the slot is MOVED INTO that
//! thread, so it is given back when the thread really ends — including a run whose caller has
//! already given up waiting for it, which would otherwise take the count past its bound while
//! its thread was still going.

use ::std::sync::{Condvar, Mutex};
use ::std::time::{Duration, Instant};

/// The slots one concurrent-run count hands out.
///
/// The count's BOUND is the caller's, passed to [`RunSlots::take`], because what one kind of
/// run holds while it lasts is that caller's own ceiling; the accounting is this type's.
#[derive(Debug)]
pub struct RunSlots {
    /// How many runs hold a slot.
    taken: Mutex<usize>,
    /// Signalled as each slot is given back.
    freed: Condvar,
}

impl RunSlots {
    /// A count with nothing taken yet, for a `static` of the caller that owns it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            taken: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Take one of `limit` slots, waiting at most `wait` for one — and `None` when none came
    /// free, which is a run that did not happen at all.
    pub fn take(&'static self, limit: usize, wait: Duration) -> Option<RunSlot> {
        let deadline = Instant::now() + wait;
        let mut taken = self.taken.lock().unwrap_or_else(|held| held.into_inner());
        loop {
            if *taken < limit {
                *taken += 1;
                return Some(RunSlot { slots: self });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (held, _) = self
                .freed
                .wait_timeout(taken, left)
                .unwrap_or_else(|held| held.into_inner());
            taken = held;
        }
    }
}

impl Default for RunSlots {
    fn default() -> Self {
        Self::new()
    }
}

/// One taken slot, given back when the run that holds it ends.
#[derive(Debug)]
pub struct RunSlot {
    slots: &'static RunSlots,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        let mut taken = self
            .slots
            .taken
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        *taken = taken.saturating_sub(1);
        drop(taken);
        self.slots.freed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The count two cases below take their slots out of.
    static SLOTS: RunSlots = RunSlots::new();

    /// RULE: a count hands out at most the slots its limit names, and a slot given back is
    /// handed out again — so a run whose thread ended never holds one.
    #[test]
    fn the_count_hands_out_its_limit_and_takes_a_slot_back() {
        let wait = Duration::from_millis(10);
        let first = SLOTS.take(2, wait).expect("the first slot is free");
        let second = SLOTS.take(2, wait).expect("the second slot is free");
        assert!(
            SLOTS.take(2, wait).is_none(),
            "case \"the limit is reached\": expected no slot while both are held"
        );
        drop(second);
        let again = SLOTS.take(2, wait);
        assert!(
            again.is_some(),
            "case \"a slot was given back\": expected the freed slot to be handed out again"
        );
        drop(again);
        drop(first);
        // (case, the limit, how many slots that limit admits at once)
        for (case, limit) in [("one run at a time", 1_usize), ("three at a time", 3)] {
            let held: Vec<RunSlot> = (0..limit)
                .map(|_| {
                    SLOTS
                        .take(limit, wait)
                        .unwrap_or_else(|| panic!("case {case:?}: expected {limit} free slots"))
                })
                .collect();
            assert!(
                SLOTS.take(limit, wait).is_none(),
                "case {case:?}: expected no slot past the limit of {limit}"
            );
            drop(held);
        }
    }
}
