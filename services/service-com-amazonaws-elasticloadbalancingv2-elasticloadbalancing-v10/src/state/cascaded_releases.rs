// SPDX-License-Identifier: CC0-1.0

//! What this store has ALREADY RELEASED, which is what makes a delete of it IDEMPOTENT.
//!
//! A delete asks for a resource to be gone, so a delete naming one this store has already
//! released asks for something that has been performed: the call completes rather than
//! reporting a resource the caller is right to believe is gone. Two releases put an ARN
//! here:
//!
//! * a resource released WITH ITS CONTAINER — deleting a load balancer "also deletes its
//!   listeners" (`DeleteLoadBalancer`), and a listener's rules go with the listener — so a
//!   client tearing a deployment down, or a template delete walking its resources, may name
//!   a rule the cascade already removed;
//! * a LISTENER a caller's own `DeleteListener` removed, so the repeat of that delete a
//!   retry or a second teardown pass sends is answered as the delete it already performed
//!   ([`crate::ops::delete_listener`]).
//!
//! An identifier this store never held is not here, and neither is a resource the CASCADE of
//! its container removed where that container modelled the removal as a refusal: a delete of
//! an ARN nothing ever carried, and a `DeleteListener` naming a listener that went with its
//! load balancer, are the modelled `NotFound` of the operation.
//!
//! The record is a BOUNDED window ([`CascadedReleases::CAPACITY`]), the emulator's own
//! bound as every quota in this module is: it holds far more releases than any workflow
//! performs, and past it the oldest is forgotten rather than the store growing without
//! end — a delete of a release that old is the modelled `NotFound` again.

use std::collections::{HashSet, VecDeque};

/// The ARNs whose release this store has already performed, newest last.
#[derive(Debug, Default)]
pub struct CascadedReleases {
    /// The order the ARNs were released in, so the oldest is what the bound drops.
    order: VecDeque<String>,
    /// The same ARNs as a set, so a delete resolves one in constant time.
    held: HashSet<String>,
}

impl CascadedReleases {
    /// How many cascaded releases this window remembers.
    pub const CAPACITY: usize = 10_000;

    /// Record that the release of `arn` has been performed — with the resource that
    /// contained it, or by the caller's own delete of it.
    pub fn record(&mut self, arn: &str) {
        if !self.held.insert(arn.to_string()) {
            return;
        }
        self.order.push_back(arn.to_string());
        while self.order.len() > Self::CAPACITY {
            if let Some(dropped) = self.order.pop_front() {
                self.held.remove(&dropped);
            }
        }
    }

    /// Whether `arn` names a resource this store has already released, so a delete of it
    /// asks for something already performed.
    pub fn holds(&self, arn: &str) -> bool {
        self.held.contains(arn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_release_is_held_and_an_unrecorded_one_is_not() {
        let mut releases = CascadedReleases::default();
        assert!(
            !releases.holds("arn:aws:elasticloadbalancing:us-east-1:1:listener/app/a/1/2"),
            "case nothing recorded: no ARN is held"
        );
        releases.record("arn:aws:elasticloadbalancing:us-east-1:1:listener/app/a/1/2");
        assert!(
            releases.holds("arn:aws:elasticloadbalancing:us-east-1:1:listener/app/a/1/2"),
            "case one recorded: that ARN is held"
        );
        assert!(
            !releases.holds("arn:aws:elasticloadbalancing:us-east-1:1:listener/app/a/1/3"),
            "case one recorded: another ARN is not"
        );
    }

    #[test]
    fn the_window_forgets_the_oldest_release_past_its_bound() {
        let mut releases = CascadedReleases::default();
        for index in 0..=CascadedReleases::CAPACITY {
            releases.record(&format!("arn-{index}"));
        }
        assert!(
            !releases.holds("arn-0"),
            "case past the bound: the oldest release is forgotten"
        );
        assert!(
            releases.holds(&format!("arn-{}", CascadedReleases::CAPACITY)),
            "case past the bound: the newest release is held"
        );
        // Recording the same ARN twice is one entry, so a repeat cannot push the window.
        let before = releases.order.len();
        releases.record(&format!("arn-{}", CascadedReleases::CAPACITY));
        assert_eq!(
            releases.order.len(),
            before,
            "case a repeated release: it is held once, got {} entries",
            releases.order.len()
        );
    }
}
