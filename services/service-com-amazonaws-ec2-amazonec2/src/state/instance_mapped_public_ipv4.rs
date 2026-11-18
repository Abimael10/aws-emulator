// SPDX-License-Identifier: CC0-1.0

//! The public IPv4 address AMAZON maps to a running instance, and what the stop/start of
//! that instance does to it.
//!
//! "A public IPv4 address is assigned to your instance from Amazon's pool of public IPv4
//! addresses… The public IPv4 address is released when you stop or terminate your instance"
//! and "a new public IPv4 address is assigned when you start it again" (the EC2 User Guide's
//! "Public IPv4 addresses", which is what `AssociatePublicIpAddress` /
//! `Subnet$MapPublicIpOnLaunch` hand an instance). So a mapped address belongs to the
//! RUNNING instance and not to the instance record: an instance that has reached `stopped`
//! reports none at all, and the one it reports after a later start is a different address.
//!
//! An ELASTIC IP the account allocated is the other kind, and it is untouched here: "an
//! Elastic IP address […] remains associated with your instance when you stop it" — an
//! allocation outlives the running instance, which is the whole point of allocating one.
//! The two are told apart by the owner of the interface's association
//! ([`super::AMAZON_ASSIGNED_IP_OWNER_ID`]), the same way every other reader of this crate
//! tells them apart.
//!
//! Both seams here are the CLOCK's: the release is owed at the instant the stop completes
//! and the assignment at the instant the start does, so
//! [`ComAmazonawsEc2Amazonec2State::materialize_due_transitions`] — the one place a move the
//! clock owes an instance is applied — is what calls them, and a caller that never calls
//! again still sees the address released and re-assigned.

use super::{
    ComAmazonawsEc2Amazonec2State, PRIMARY_NETWORK_INTERFACE_DEVICE_INDEX, region_of_zone_name,
};

impl ComAmazonawsEc2Amazonec2State {
    /// The network interfaces the instance `instance_id` holds, in device-index order.
    ///
    /// The account's interface store is the one place "which interfaces does this instance
    /// have" is answered, so this asks the attachments rather than keeping a second list.
    fn instance_interface_ids(&self, instance_id: &str) -> Vec<(i32, String)> {
        let mut held: Vec<(i32, String)> = self
            .network_interfaces
            .iter()
            .filter_map(|(network_interface_id, interface)| {
                let attachment = interface.attachment.as_ref()?;
                if attachment.instance_id.as_deref() != Some(instance_id) {
                    return None;
                }
                Some((
                    attachment.device_index.unwrap_or_default(),
                    network_interface_id.clone(),
                ))
            })
            .collect();
        held.sort();
        held
    }

    /// Release the public IPv4 address Amazon mapped to the instance `instance_id`, on the
    /// instance itself and on the interface that held it.
    ///
    /// Called at the instant a stop COMPLETES (see the module docs): the address is Amazon's
    /// rather than the account's, so nothing of the account holds it afterwards and the
    /// instance reports no public address at all. An Elastic IP association is left exactly
    /// where it is — and an instance reporting one goes on reporting it — and so is an
    /// instance that holds no mapped address.
    ///
    /// The instance keeps the record that it IS mapped
    /// ([`super::InstanceRecord::mapped_public_ipv4`]), which is what makes the start below
    /// assign it a fresh one.
    pub(super) fn release_mapped_public_ipv4(&mut self, instance_id: &str) {
        for (_, network_interface_id) in self.instance_interface_ids(instance_id) {
            // Through the ONE seam that takes an address AWS assigned off an interface
            // ([`ComAmazonawsEc2Amazonec2State::release_amazon_public_ipv4`]), so the
            // interface's own association, the association on the private address the public
            // one was bound to, the interface's public hostname and the address the instance
            // reports all go together — the exact reverse of the assignment. An interface
            // holding an Elastic IP the ACCOUNT allocated holds nothing of AWS's to take, so
            // the seam leaves it exactly where it is, and an instance reporting one goes on
            // reporting it.
            self.release_amazon_public_ipv4(&network_interface_id);
        }
    }

    /// Give the instance `instance_id` the public IPv4 address Amazon maps to it when it
    /// starts, if it is an instance that gets one and holds none right now.
    ///
    /// Called at the instant a start COMPLETES (see the module docs). The address goes to the
    /// instance's PRIMARY interface — "the public IP address can only be assigned to a
    /// network interface for eth0" (`AssociatePublicIpAddress`) — through the one place a
    /// public IPv4 address of this store is handed out
    /// ([`ComAmazonawsEc2Amazonec2State::assign_amazon_public_ipv4`]), so it is a fresh
    /// address of Amazon's pool rather than the one the stop released.
    pub(super) fn assign_mapped_public_ipv4(&mut self, instance_id: &str) {
        let Some(instance) = self.instances.get(instance_id) else {
            return;
        };
        if !instance.mapped_public_ipv4 || instance.public_ip.is_some() {
            return;
        }
        // The Region the address's public DNS name is built for is the one the instance's
        // zone belongs to, read through the ONE reading of that ([`region_of_zone_name`]).
        let region = region_of_zone_name(&instance.placement_az).to_string();
        let Some((_, primary)) = self
            .instance_interface_ids(instance_id)
            .into_iter()
            .find(|(device_index, _)| *device_index == PRIMARY_NETWORK_INTERFACE_DEVICE_INDEX)
        else {
            return;
        };
        // An interface that already holds an Elastic IP the account associated while the
        // instance was stopped keeps it: that association is the account's, so no mapped
        // address displaces it.
        if self
            .network_interfaces
            .get(&primary)
            .is_some_and(|interface| interface.association.is_some())
        {
            return;
        }
        let assigned = self.assign_amazon_public_ipv4(&primary, &region);
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.public_ip = assigned;
        }
    }
}
