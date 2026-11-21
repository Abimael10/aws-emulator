// SPDX-License-Identifier: CC0-1.0

//! What the LIFECYCLE of the host behind an `instance` registration owes the target group
//! that holds it.
//!
//! An `instance` registration names a host of the account rather than anything this service
//! owns — "if the target type of the target group is `instance`, specify an instance ID"
//! (`TargetDescription$Id`) — and that host is Amazon EC2's record, read here the way every
//! other fact this service does not own is ([`super::network`]). A host whose lifecycle has
//! ENDED can never answer a request or a health check again: `terminated` is the one instance
//! state nothing comes back from, and `shutting-down` is on its way there and nowhere else
//! ([`::service_com_amazonaws_ec2_amazonec2::state::InstanceStateName::is_terminating`]). So a
//! load balancer does not keep a registration of such a host: it DEREGISTERS it, which is the
//! same transition `DeregisterTargets` performs — `draining` first, then out of the target
//! group ([`super::target_health`]) — and the group stops counting it, checking it and
//! forwarding to it for exactly the same reasons it stops for a deregistration a caller asked
//! for.
//!
//! Only a registration naming an INSTANCE is read. Every other target type names something
//! else — an address, a function, a load balancer — whose own existence is no statement about
//! any instance's lifecycle, so an address that happened to be held by an interface of a
//! terminated instance is left registered: what such a registration names is the address, and
//! the checks of it report what holds it now.
//!
//! Nothing is decided from silence: a registration whose instance Amazon EC2 holds NO record
//! for is left exactly as it is, and so is every registration of an engine carrying no EC2
//! store at all. A host EC2 reports `stopped` is not read here either — a stopped instance can
//! be started again, so its registration is still a registration, and what the load balancer
//! reports for it is what its health checks find ([`super::host_counts`]).
//!
//! This is a change the CLOCK owes: the host stopped existing when EC2 recorded it, not when
//! somebody called, so it is materialised by the one pass that drives this service's periodic
//! checking ([`super::host_counts`]) and by nothing on a read path. The due instant of the
//! drain is stored; nothing sleeps for it.
//!
//! The two stores are never held at once: this service's registrations are read under its own
//! guard, EC2 is asked with no guard of this service held, and the deregistration is written
//! under this service's guard again — the lock order every other reader of another service's
//! records in this crate keeps.

use ::std::collections::HashSet;
use ::std::time::{Duration, SystemTime};

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::target_health::due_at;
use super::target_servers::INSTANCE_TARGET_TYPE;
use super::{
    ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State as ElbState, TargetKey,
    SETTLE_AFTER,
};

/// How long the registration of a host that is GONE drains before it leaves the target group:
/// the span this emulator gives a transition the clock owes ([`SETTLE_AFTER`]), and not the
/// group's `deregistration_delay.timeout_seconds`.
///
/// That attribute is "the amount of time, in seconds, for Elastic Load Balancing to wait
/// before changing the state of a deregistering target from `draining` to `unused`" — the time
/// the connections already open to the target are given to complete, which is why a
/// deregistration a caller asks for waits it out ([`super::deregistration_delay`]). A host
/// Amazon EC2 reports terminating holds no such connection: it is gone, so there is nothing
/// left to drain and the only span the removal owes is the one that keeps the `draining` it
/// passes through observable to a caller polling for it.
const DRAIN_OF_A_GONE_HOST: Duration = SETTLE_AFTER;

/// Deregister every registration of this store whose host Amazon EC2 reports terminating, and
/// remove the ones whose drain the clock has already run out.
///
/// Called with NO guard of this store held: it takes the lock itself, three times, so neither
/// store is ever held across the other (see the module docs).
pub fn settle_gone_hosts<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ElbState>,
    now: SystemTime,
) {
    // The guard this statement takes is a TEMPORARY: it is released at the end of the
    // statement, so nothing below holds it.
    let registrations = instance_registrations(&state.lock());
    if registrations.is_empty() {
        return;
    }
    let gone = gone_hosts(engine, &registrations);
    if gone.is_empty() {
        return;
    }
    let mut store = state.lock();
    for (target_group_arn, key) in &registrations {
        if !gone.contains(&key.0) {
            continue;
        }
        let Some(group) = store.target_groups.get_mut(target_group_arn) else {
            // The pass ran with no guard held, so the target group may have been deleted in
            // the meantime — which took its registrations with it.
            continue;
        };
        group.release_gone_host(key, now);
        // And the removal the drain this call started — or an earlier one did — has already
        // made due, which is owed at the same instant whether or not anything called.
        group.settle_draining(now);
    }
}

/// Every registration this store holds that names an INSTANCE, as the target group it belongs
/// to and the key it is stored under.
///
/// Read under this store's guard alone, in the one order the store holds its groups and their
/// registrations in, so two passes read the same registrations in the same order.
fn instance_registrations(store: &ElbState) -> Vec<(String, TargetKey)> {
    store
        .target_groups
        .iter()
        .filter(|(_, group)| group.target_type == INSTANCE_TARGET_TYPE)
        .flat_map(|(target_group_arn, group)| {
            group
                .targets
                .keys()
                .map(move |key| (target_group_arn.clone(), key.clone()))
        })
        .collect()
}

/// The target ids among `registrations` whose instance Amazon EC2 reports TERMINATING — the
/// hosts that are gone.
///
/// Empty for an engine carrying no EC2 store, and an id that store holds no instance for is
/// absent: nothing is decided from silence (see the module docs). One read of that store
/// serves the whole pass, and it costs one lookup per registration rather than a walk of the
/// account's inventory.
fn gone_hosts<Eng: Send + Sync + 'static>(
    engine: &Eng,
    registrations: &[(String, TargetKey)],
) -> HashSet<String> {
    let Some(state) = ec2::service_state(engine) else {
        return HashSet::new();
    };
    let store = state.lock();
    registrations
        .iter()
        .map(|(_, (id, _))| id)
        .filter(|id| {
            store
                .instances
                .get(id.as_str())
                .is_some_and(|instance| instance.state.is_terminating())
        })
        .cloned()
        .collect()
}

impl super::TargetGroupRecord {
    /// Begin the deregistration of the registration `key` names because the host behind it is
    /// gone: it stops receiving new requests at once and leaves the target group once
    /// [`DRAIN_OF_A_GONE_HOST`] has passed.
    ///
    /// A registration that is ALREADY draining keeps the instant it was given: a drain does
    /// not restart, and one a caller's own `DeregisterTargets` asked for is not shortened by
    /// the host going away under it — exactly as a deregistration asked for twice keeps the
    /// first instant ([`super::target_health::TargetGroupRecord::deregister`]).
    fn release_gone_host(&mut self, key: &TargetKey, now: SystemTime) {
        if let Some(target) = self.targets.get_mut(key) {
            if target.draining_until.is_none() {
                target.draining_until = Some(due_at(now, DRAIN_OF_A_GONE_HOST));
            }
        }
    }
}
