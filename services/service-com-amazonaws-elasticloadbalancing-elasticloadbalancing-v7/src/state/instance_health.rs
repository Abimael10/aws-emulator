// SPDX-License-Identifier: CC0-1.0

//! The REGISTERED-INSTANCE SET of one Classic load balancer: which instances are
//! registered with it, in which order, and the health checking each registration started.
//!
//! A registration is not only a membership — it is what the load balancer CHECKS — so the
//! set and the per-registration check state travel together here, beside the
//! [`super::instance_checks`] machinery that says what one check observes and which
//! verdict a run of checks reaches. `DescribeLoadBalancers` reports the set, and
//! `DescribeInstanceHealth` reports the verdict each registration's checks have reached.

use super::*;

/// One instance's registration with one load balancer.
///
/// A registration is not only a membership: it is what the load balancer CHECKS, so it
/// carries the instant it was made — the instant the checks it owes are counted from — and
/// where those checks have got to ([`HealthProbe`]).
#[derive(Debug, Clone)]
pub struct Registration {
    /// The instant this instance was registered.
    pub registered_at: SystemTime,
    /// What this registration's health checks have observed and decided.
    pub probe: HealthProbe,
    /// The WORKLOAD another emulated service's scheduler made this registration on behalf
    /// of, and `None` for one a caller of this service made itself
    /// (`RegisterInstancesWithLoadBalancer`).
    ///
    /// A scheduler that registers an instance it runs owns that registration: it is the one
    /// that takes it out again when the workload stops needing it
    /// ([`InstanceSet::scheduler_registrations`]), and a caller's own registration is never
    /// claimed by one — which is what keeps the two kinds apart in one set.
    pub registered_for: Option<String>,
}

impl Registration {
    /// The registration an instance registered at `at` starts with: no check has run yet.
    pub fn starting_at(at: SystemTime) -> Self {
        Self {
            registered_at: at,
            probe: HealthProbe::starting_at(at),
            registered_for: None,
        }
    }

    /// The registration a SCHEDULER made at `at` for the workload `workload`: the same
    /// registration, carrying what made it.
    pub fn starting_at_for(at: SystemTime, workload: &str) -> Self {
        Self {
            registered_for: Some(workload.to_string()),
            ..Self::starting_at(at)
        }
    }
}

/// The registered-instance set of one load balancer: deduplicated, in registration
/// order, with membership answered from an index rather than by scanning.
///
/// The ordered `Vec` is what `DescribeLoadBalancers` and `DescribeInstanceHealth`
/// report; the `BTreeMap` makes each membership test O(log n), so a batched request
/// costs O(batch x log n) instead of O(batch x n), and it carries each instance's
/// [`Registration`] — when it was made and the checks it owes.
#[derive(Debug, Clone, Default)]
pub struct InstanceSet {
    order: Vec<String>,
    /// Instance id -> its registration with this load balancer.
    registered: BTreeMap<String, Registration>,
}

impl InstanceSet {
    /// Add `instance_id`, registered at `at`, unless it is already registered. Answers
    /// whether it was added.
    ///
    /// An instance the set already holds keeps the registration it already has: the
    /// call restates a registration that already happened, so the health checks it
    /// started do not restart.
    pub fn insert(&mut self, instance_id: &str, at: SystemTime) -> bool {
        self.insert_registration(instance_id, Registration::starting_at(at))
    }

    /// Add `instance_id` as a registration another emulated service's scheduler made at `at`
    /// for the workload `workload`, unless it is already registered. Answers whether it was
    /// added.
    ///
    /// An instance the set already holds keeps the registration it already has, exactly as
    /// [`InstanceSet::insert`] leaves it: a scheduler restating a registration that already
    /// happened does not restart the health checks it started.
    pub fn insert_for(&mut self, instance_id: &str, at: SystemTime, workload: &str) -> bool {
        self.insert_registration(instance_id, Registration::starting_at_for(at, workload))
    }

    /// The one insertion both forms above go through, so a registration is held in the set
    /// and in its order exactly once however it was made.
    fn insert_registration(&mut self, instance_id: &str, registration: Registration) -> bool {
        if self.registered.contains_key(instance_id) {
            return false;
        }
        self.registered.insert(instance_id.to_string(), registration);
        self.order.push(instance_id.to_string());
        true
    }

    /// Every registration of this set another service's scheduler made, as
    /// `(instance id, workload)` pairs in registration order: what a scheduler reads to find
    /// the registrations it owns, including the ones it no longer owes.
    ///
    /// A registration a CALLER of this service made carries no workload and is not reported
    /// here, so nothing a scheduler does can take it out.
    pub fn scheduler_registrations(&self) -> Vec<(String, String)> {
        self.order
            .iter()
            .filter_map(|instance_id| {
                let registration = self.registered.get(instance_id)?;
                let workload = registration.registered_for.clone()?;
                Some((instance_id.clone(), workload))
            })
            .collect()
    }

    /// Remove every id in `ids` in ONE pass over the ordered set, so a batch costs
    /// O(n log |ids|) rather than O(|ids| x n). Answers how many were registered.
    pub fn remove_all(&mut self, ids: &BTreeSet<String>) -> usize {
        let before = self.order.len();
        self.order.retain(|held| !ids.contains(held));
        for id in ids {
            self.registered.remove(id);
        }
        before - self.order.len()
    }

    pub fn contains(&self, instance_id: &str) -> bool {
        self.registered.contains_key(instance_id)
    }

    /// The instant `instance_id` was registered, or `None` when it is not registered.
    pub fn registered_at(&self, instance_id: &str) -> Option<SystemTime> {
        self.registered
            .get(instance_id)
            .map(|registration| registration.registered_at)
    }

    /// Move an existing registration to `at`, leaving the set, its order and the checks
    /// that have already run as they are. Answers whether the instance was registered.
    ///
    /// The checks a registered instance owes are counted from its registration instant, so
    /// this moves them with it: the next check falls due the same distance from the
    /// registration as before. That is how a registration — and with it the checks the
    /// clock owes it — is placed at a chosen point in time.
    pub fn restamp(&mut self, instance_id: &str, at: SystemTime) -> bool {
        match self.registered.get_mut(instance_id) {
            Some(registration) => {
                registration.probe.reschedule(registration.registered_at, at);
                registration.registered_at = at;
                true
            }
            None => false,
        }
    }

    /// The health checks `instance_id`'s registration owes, for a settle to run the ones
    /// the clock has made due — `None` when it is not registered.
    pub fn probe_mut(&mut self, instance_id: &str) -> Option<&mut HealthProbe> {
        self.registered
            .get_mut(instance_id)
            .map(|registration| &mut registration.probe)
    }

    /// What the checks of `instance_id`'s registration have decided, or `None` when it is
    /// not registered.
    pub fn probe(&self, instance_id: &str) -> Option<&HealthProbe> {
        self.registered
            .get(instance_id)
            .map(|registration| &registration.probe)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The registered ids in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }
}
