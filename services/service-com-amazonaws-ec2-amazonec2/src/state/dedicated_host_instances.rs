// SPDX-License-Identifier: CC0-1.0

//! Which instances are ON a Dedicated Host.
//!
//! A host's `Instances` is "the IDs and instance type that are currently running on the
//! Dedicated Host" (`Host$Instances`), so it is not a fact a caller states about the host: it
//! FOLLOWS from where the instances of the account were launched. The launch that places an
//! instance on a host records that host on the instance
//! ([`super::InstanceDetail::host_id`]), and the host's own member is RECOMPUTED from the
//! instance store here — the same arrangement a security group's `IpPermissions` has
//! ([`ComAmazonawsEc2Amazonec2State::resync_security_group_permissions`]), so a host and the
//! instances on it cannot disagree about which they are.
//!
//! That is what makes the dependency a release has to honour real: "you must stop or terminate
//! all instances on a host before it can be released" (`ReleaseHosts`) reads this one list, so
//! a host carrying a running instance is reported in `Unsuccessful` instead of released under
//! it.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::{ComAmazonawsEc2Amazonec2State, InstanceRecord, InstanceStateName};
use crate::ops::types::HostInstance;

/// Does `instance` occupy the Dedicated Host it was launched on?
///
/// A host reports what is "currently running" on it, and a release asks the caller to "stop or
/// terminate" what is — so an instance that has stopped or terminated has left the host, while
/// one still coming up, running, or on its way down is still on it. The one definition of that
/// question, read by the recompute below and therefore by every operation that reports a host.
pub fn instance_occupies_its_host(instance: &InstanceRecord) -> bool {
    !matches!(
        instance.state,
        InstanceStateName::Stopped | InstanceStateName::Terminated
    )
}

impl ComAmazonawsEc2Amazonec2State {
    /// Does the Dedicated Host `host_id` still have an instance on it?
    ///
    /// Asked of the INSTANCE store rather than of the host's reported `Instances`, so the
    /// dependency a release has to honour — "you must stop or terminate all instances on a host
    /// before it can be released" (`ReleaseHosts`) — is read from where the fact lives rather
    /// than from a derived member a writer might not have recomputed yet.
    pub fn dedicated_host_is_occupied(&self, host_id: &str) -> bool {
        self.instances.values().any(|instance| {
            instance.detail.host_id.as_deref() == Some(host_id)
                && instance_occupies_its_host(instance)
        })
    }

    /// Recompute every stored Dedicated Host's `Instances` from the instance store.
    ///
    /// Called where the instances on a host can have changed — by the launch that places one
    /// and by [`Self::materialize_due_transitions`], which is where an instance reaches the
    /// state that takes it off its host — so the member a host REPORTS is the one its
    /// instances make true at that instant rather than one a writer left behind. A pure
    /// recompute of a derived member: it moves no state of its own and consumes nothing.
    ///
    /// The entries are in instance-id order, the order the instance store is kept in, so two
    /// reads of one host report them the same way.
    pub fn resync_dedicated_host_instances(&mut self) {
        let mut on_host: ::std::collections::BTreeMap<String, Vec<HostInstance>> =
            ::std::collections::BTreeMap::new();
        let owner_id = ::engine::emulator_account_id().to_string();
        for instance in self.instances.values() {
            let Some(host_id) = instance.detail.host_id.as_deref() else {
                continue;
            };
            if !instance_occupies_its_host(instance) {
                continue;
            }
            on_host
                .entry(host_id.to_string())
                .or_default()
                .push(HostInstance {
                    // "The ID of instance that is running on the Dedicated Host"
                    // (`HostInstance$InstanceId`) and "the instance type (for example,
                    // `m3.medium`) of the running instance" (`$InstanceType`), both read off
                    // the instance itself.
                    instance_id: Some(instance.instance_id.clone()),
                    instance_type: Some(instance.instance_type.clone()),
                    // "The ID of the Amazon Web Services account that owns the instance"
                    // (`HostInstance$OwnerId`): this emulator serves one account.
                    owner_id: Some(owner_id.clone()),
                });
        }
        for (host_id, host) in &mut self.dedicated_hosts {
            host.instances = Some(on_host.remove(host_id).unwrap_or_default());
        }
    }
}
