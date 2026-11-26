// SPDX-License-Identifier: CC0-1.0

//! Where ONE target group's distribution of requests ACROSS ITS OWN TARGETS has got to.
//!
//! A `forward` to a target group does not pick one registration and keep it: the load
//! balancer spreads the requests of that group over the targets it may send them to, the way
//! `load_balancing.algorithm.type` names — "round_robin", "least_outstanding_requests" or
//! "weighted_random". Two of those three are statements about what came BEFORE this request,
//! so the group has to remember two things, and only two:
//!
//! * [`TargetDistribution::rotate`] — how far its ROTATION has got, which is what makes the
//!   next request go to the next target rather than to the one the last request went to;
//! * [`TargetDistribution::in_flight`] — how many requests the load balancer currently has
//!   IN FLIGHT to each registration, which is what "least outstanding requests" is counted
//!   over: a request is counted from the moment this plane chooses its target until the
//!   target's answer is in ([`crate::service::data_plane`] holds one of these for exactly
//!   that span).
//!
//! Both are the LOAD BALANCER's own state about its own forwarding, not a fact about the
//! target: nothing here moves a target's health, consumes a quota or changes what any read of
//! the target group reports. It is held ON the target group's record, so it lives and dies
//! with the group — a deleted group leaves nothing behind, and the in-flight counts are
//! bounded by the registrations the group holds, which its own quota bounds.

use ::std::collections::BTreeMap;

use super::TargetKey;

/// Where one target group's own distribution of requests across its targets has got to.
#[derive(Debug, Clone, Default)]
pub struct TargetDistribution {
    /// How many requests this group has distributed, which is the position its rotation is
    /// at. Wraps, so a long-lived group keeps rotating rather than stopping.
    rotation: u64,
    /// How many requests the load balancer currently has in flight to each registration.
    /// A registration nothing is in flight to holds NO entry, so the map is bounded by the
    /// requests being served rather than by the requests ever served.
    in_flight: BTreeMap<TargetKey, u32>,
}

impl TargetDistribution {
    /// The position in a list of `count` targets this request's share of the rotation lands
    /// on, advancing the rotation by one — so `count` successive requests reach `count`
    /// different positions.
    ///
    /// `None` for a distribution over no target at all, which is no position to land on.
    pub fn rotate(&mut self, count: usize) -> Option<usize> {
        if count == 0 {
            return None;
        }
        let at = usize::try_from(self.rotation % count as u64).ok()?;
        self.rotation = self.rotation.wrapping_add(1);
        Some(at)
    }

    /// How many requests the load balancer currently has in flight to the registration
    /// `key` names.
    #[must_use]
    pub fn in_flight(&self, key: &TargetKey) -> u32 {
        self.in_flight.get(key).copied().unwrap_or_default()
    }

    /// Count one request as in flight to the registration `key` names: the plane has chosen
    /// that target and has not been answered yet.
    pub fn started(&mut self, key: &TargetKey) {
        let counted = self.in_flight.entry(key.clone()).or_default();
        *counted = counted.saturating_add(1);
    }

    /// Count that request as answered. The entry is dropped once nothing is in flight to the
    /// registration, so what this holds is the requests being served.
    pub fn finished(&mut self, key: &TargetKey) {
        let Some(counted) = self.in_flight.get_mut(key) else {
            return;
        };
        *counted = counted.saturating_sub(1);
        if *counted == 0 {
            self.in_flight.remove(key);
        }
    }
}

/// ONE request the load balancer has in flight to one registration, counted for exactly as
/// long as this is held.
///
/// Taken where the plane has chosen the target and dropped where the target's answer is in
/// ([`crate::service::data_plane`]), so "least outstanding requests" is counted over the
/// requests actually being served: a request this plane is waiting on a target for is in
/// flight, and one it has answered is not. Holding it takes no lock — the count is moved
/// under the store's own guard, taken and released by the construction and by the drop.
pub struct RequestInFlight {
    /// The ONE shared store the target groups live in, so the count a later request reads is
    /// the count this request raised.
    state: ::engine::ServiceState<super::ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State>,
    /// The target group the registration belongs to.
    target_group_arn: String,
    /// The registration itself, as the group keys it.
    registration: TargetKey,
}

/// What is diagnosable about one of these is which registration it counts a request to, and
/// never the state handle inside it, which carries no `Debug`.
impl ::std::fmt::Debug for RequestInFlight {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("RequestInFlight")
            .field("target_group_arn", &self.target_group_arn)
            .field("registration", &self.registration)
            .finish()
    }
}

impl RequestInFlight {
    /// Count one request as in flight to the registration `registration` names in the target
    /// group `target_group_arn` names, until the answer is in.
    #[must_use]
    pub fn to(
        state: ::engine::ServiceState<
            super::ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State,
        >,
        target_group_arn: String,
        registration: TargetKey,
    ) -> Self {
        if let Some(group) = state.lock().target_groups.get_mut(&target_group_arn) {
            group.distribution.started(&registration);
        }
        Self {
            state,
            target_group_arn,
            registration,
        }
    }
}

impl Drop for RequestInFlight {
    /// The answer is in — or the request was abandoned — so the load balancer no longer has
    /// it in flight. A target group that has gone away in the meantime holds no count to
    /// lower.
    fn drop(&mut self) {
        if let Some(group) = self.state.lock().target_groups.get_mut(&self.target_group_arn) {
            group.distribution.finished(&self.registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A registration key of `id` on `port`.
    fn key(id: &str, port: i32) -> TargetKey {
        (id.to_string(), Some(port))
    }

    /// What a row of the rotation table states: the case, how many registrations the group
    /// holds, and the positions successive requests reach.
    type Rotated<'r> = (&'r str, usize, &'r [Option<usize>]);

    /// RULE: the rotation hands out each position of the list in turn and then begins again,
    /// so successive requests reach successive targets.
    ///
    /// The rows span what the rotation generalises over: the counts a group can hold (one
    /// target, two, three) and the distribution over none.
    #[test]
    fn the_rotation_hands_out_each_position_in_turn() {
        let rows: &[Rotated<'_>] = &[
            ("one target", 1, &[Some(0), Some(0), Some(0)]),
            ("two targets", 2, &[Some(0), Some(1), Some(0), Some(1)]),
            ("three targets", 3, &[Some(0), Some(1), Some(2), Some(0)]),
            ("no target at all", 0, &[None, None]),
        ];
        for (case, count, expected) in rows {
            let mut distribution = TargetDistribution::default();
            let walked: Vec<Option<usize>> = (0..expected.len())
                .map(|_| distribution.rotate(*count))
                .collect();
            assert_eq!(
                walked.as_slice(),
                *expected,
                "case {case:?}: expected the rotation {expected:?}, got {walked:?}"
            );
        }
    }

    /// RULE: a registration's in-flight count is the requests chosen for it and not yet
    /// answered, and a registration nothing is in flight to holds nothing at all.
    #[test]
    fn what_is_in_flight_is_what_was_started_and_not_finished() {
        let mut distribution = TargetDistribution::default();
        let first = key("i-1", 80);
        let second = key("i-2", 80);
        assert_eq!(
            distribution.in_flight(&first),
            0,
            "case \"nothing started\": expected no request in flight"
        );
        distribution.started(&first);
        distribution.started(&first);
        distribution.started(&second);
        assert_eq!(
            (distribution.in_flight(&first), distribution.in_flight(&second)),
            (2, 1),
            "case \"two and one started\": expected the counts 2 and 1"
        );
        distribution.finished(&first);
        assert_eq!(
            distribution.in_flight(&first),
            1,
            "case \"one answered\": expected the remaining request in flight"
        );
        distribution.finished(&first);
        distribution.finished(&second);
        // An answer for a request nothing counted leaves the count where it is rather than
        // taking it below zero.
        distribution.finished(&first);
        assert_eq!(
            (distribution.in_flight(&first), distribution.in_flight(&second)),
            (0, 0),
            "case \"every request answered\": expected nothing in flight"
        );
        assert!(
            distribution.in_flight.is_empty(),
            "case \"every request answered\": expected no entry to be kept, got {:?}",
            distribution.in_flight
        );
    }
}
