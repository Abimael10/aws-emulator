// SPDX-License-Identifier: CC0-1.0

//! The replicators this registry HELD and holds no longer.
//!
//! What it is for: the replicator APIs answer a reference to a replicator the
//! service knew and released differently from one it never minted, so the two
//! absences have to be distinguishable after the record itself is gone. This
//! module is the whole memory of that, which is why it is its own file: a store
//! that grows on a caller's request needs its bound where a reader can see it.
//!
//! The bound: ONE queue for the whole registry, holding at most
//! [`MAX_RELEASED_REPLICATORS`] entries and forgetting its oldest first. Nothing
//! here is keyed by a value the request chooses — the Region a release is
//! remembered under is stored IN the entry rather than as a map key — so a
//! caller that creates and deletes replicators, under any Region name and any
//! number of times, leaves this store at the same ceiling it starts at.
//! Forgetting the oldest release is the only thing a bound can do, and a release
//! that has been forgotten reads as an ARN the service never minted.

use std::collections::VecDeque;

use super::MAX_REPLICATORS_IN_REGISTRY;

/// The most releases this registry remembers at once, across every Region.
///
/// The number of replicators it can hold at once
/// ([`MAX_REPLICATORS_IN_REGISTRY`]): remembering more releases than there can
/// be live replicators buys nothing, and this way one constant bounds both the
/// records and the memory of the records.
pub const MAX_RELEASED_REPLICATORS: usize = MAX_REPLICATORS_IN_REGISTRY;

/// The bounded record of released replicators: the (Region, ARN) pairs this
/// registry held and let go, oldest first.
#[derive(Debug, Default)]
pub struct ReleasedReplicators {
    /// Oldest release first, so the entry that leaves when the ceiling is
    /// reached is the one the caller is least likely to ask about.
    entries: VecDeque<(String, String)>,
}

impl ReleasedReplicators {
    /// Record that `region` held the replicator `arn` names and holds it no
    /// longer.
    ///
    /// The ceiling is enforced HERE, on the one insertion path, so no caller of
    /// this module can grow the store past it: a release past
    /// [`MAX_RELEASED_REPLICATORS`] drops the oldest one. A release already
    /// remembered is not recorded twice.
    pub fn remember(&mut self, region: &str, arn: &str) {
        if self.was_held(region, arn) {
            return;
        }
        self.entries
            .push_back((region.to_string(), arn.to_string()));
        while self.entries.len() > MAX_RELEASED_REPLICATORS {
            self.entries.pop_front();
        }
    }

    /// Whether `region` held the replicator `arn` names and holds it no longer.
    ///
    /// Scoped to the Region the request is served for, as every replicator
    /// lookup is, so one Region's releases say nothing about another's. The walk
    /// is over at most [`MAX_RELEASED_REPLICATORS`] entries, whatever a caller
    /// has done.
    pub fn was_held(&self, region: &str, arn: &str) -> bool {
        self.entries
            .iter()
            .any(|(held_region, held_arn)| held_region == region && held_arn == arn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceiling holds whatever a caller does: releases past it forget the
    /// oldest entry, the store never exceeds it, and the Region a release was
    /// remembered under is not a key a caller can multiply.
    #[test]
    fn the_record_of_releases_stays_at_its_ceiling() {
        let mut released = ReleasedReplicators::default();
        // One release per invented Region name, twice the ceiling of them: the
        // store holds the ceiling and nothing more.
        for index in 0..MAX_RELEASED_REPLICATORS * 2 {
            released.remember(&format!("region-{index}"), &format!("arn-{index}"));
        }
        assert_eq!(
            released.entries.len(),
            MAX_RELEASED_REPLICATORS,
            "expected the record to stay at its ceiling, got {}",
            released.entries.len()
        );
        assert!(
            !released.was_held("region-0", "arn-0"),
            "expected the oldest release to be forgotten at the ceiling"
        );
        let newest = MAX_RELEASED_REPLICATORS * 2 - 1;
        assert!(
            released.was_held(&format!("region-{newest}"), &format!("arn-{newest}")),
            "expected the newest release to be remembered"
        );

        // A release is remembered once, and only for the Region it happened in.
        let mut once = ReleasedReplicators::default();
        for _ in 0..3 {
            once.remember("us-west-2", "arn-repeated");
        }
        assert_eq!(
            once.entries.len(),
            1,
            "expected a repeated release to be recorded once, got {}",
            once.entries.len()
        );
        assert!(
            !once.was_held("eu-west-1", "arn-repeated"),
            "expected one Region's releases to say nothing about another's"
        );
    }
}
