// SPDX-License-Identifier: CC0-1.0

//! The SECONDARY private IPv4 addresses a network interface holds: who holds an address,
//! the one writer that records one on an interface and the one that takes it off again.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.
//!
//! The addresses an interface holds live on the interface record itself
//! (`NetworkInterface$PrivateIpAddresses`), and the pool
//! [`ComAmazonawsEc2Amazonec2State::reserve_private_ip`] takes from is the same one
//! [`ComAmazonawsEc2Amazonec2State::insert_network_interface`] writes into — so the two
//! writers here keep the record, the pool, the store's byte budget and the subnet's
//! `AvailableIpAddressCount` in step with each other, the way a create and a delete of a
//! whole interface already do.

use super::*;

/// EC2's error code for an assignment past the number of private IPv4 addresses an
/// interface may hold: "PrivateIpAddressLimitExceeded: You've reached the limit on the
/// number of private IP addresses that you can assign to the specified network interface
/// for that type of instance" (EC2 API "Error codes").
pub const PRIVATE_IP_ADDRESS_LIMIT_EXCEEDED: &str = "PrivateIpAddressLimitExceeded";

/// Refuse an assignment that would leave a network interface holding more private IPv4
/// addresses than one may hold ([`MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE`]): HTTP 400
/// [`PRIVATE_IP_ADDRESS_LIMIT_EXCEEDED`], carrying the sentence AWS publishes for that code.
///
/// The ONE wording site for that refusal, so every path that assigns private IPv4 addresses
/// to an existing interface answers it the same way.
pub fn private_ip_address_limit_exceeded<E>() -> Scaffolded<E> {
    Scaffolded::service_error(
        PRIVATE_IP_ADDRESS_LIMIT_EXCEEDED,
        400,
        "You've reached the limit on the number of private IP addresses that you can \
         assign to the specified network interface for that type of instance",
    )
}

/// Refuse an ASSIGNMENT of a private IPv4 address that is not one of the subnet's
/// assignable addresses — outside its CIDR block, or one of the five AWS reserves inside
/// it: HTTP 400 `InvalidParameterValue`.
///
/// AWS words this refusal as FIXED TEXT when the address is being assigned to an interface
/// that already exists: the sentence states that the address is not one of the subnet's and
/// names neither the address the request sent nor the block it missed. A CREATE that names
/// an address outside its subnet's range answers the longer sentence that quotes the
/// parameter, the value and the range (`crate::ops::create_network_interface`), so this is a
/// wording of its own rather than a second caller of that one — the two operations are given
/// different sentences by the service, and each site carries the one its own operation
/// answers.
pub fn address_outside_subnet_range<E>() -> Scaffolded<E> {
    invalid_parameter_value("Address does not fall within the subnet's address range")
}

impl ComAmazonawsEc2Amazonec2State {
    /// Which network interface holds `address` as a private IPv4 address, and whether it
    /// holds it as its PRIMARY one — `None` when no interface of the account does.
    ///
    /// A READ ONLY. The records are the pool for this question, so "already assigned to
    /// another network interface" (`AssignPrivateIpAddressesRequest$AllowReassignment`) is
    /// answered off the same records a describe reports, and the primary address — the one
    /// an interface cannot be left without — is distinguishable from a secondary one. An
    /// entry that carries no `Primary` is read as primary exactly when it leads the list,
    /// which is where [`Self::insert_network_interface`] records the primary address.
    pub fn interface_holding_private_ip(&self, address: &str) -> Option<(String, bool)> {
        self.network_interfaces
            .iter()
            .find_map(|(network_interface_id, interface)| {
                interface
                    .private_ip_addresses
                    .iter()
                    .flatten()
                    .enumerate()
                    .find(|(_, entry)| entry.private_ip_address.as_deref() == Some(address))
                    .map(|(index, entry)| {
                        (
                            network_interface_id.clone(),
                            entry.primary.unwrap_or(index == 0),
                        )
                    })
            })
    }

    /// Record `addresses` on the interface `network_interface_id` as SECONDARY private IPv4
    /// addresses, and answer whether the interface was there to record them on.
    ///
    /// The same bookkeeping [`Self::insert_network_interface`] does for the addresses an
    /// interface is created with, for addresses added to one that already exists: each
    /// address gets the private DNS name the Region builds for it
    /// ([`private_dns_name`]), leaves the one in-use pool
    /// ([`Self::private_ip_in_use`] reads it), is charged against the store's byte budget
    /// the way [`network_interface_payload_bytes`] counts it, and comes off the subnet's
    /// `AvailableIpAddressCount`. [`Self::release_secondary_private_ip`] is the exact
    /// reverse.
    pub fn assign_secondary_private_ips(
        &mut self,
        network_interface_id: &str,
        addresses: &[String],
        region: &str,
    ) -> bool {
        let Some(interface) = self.network_interfaces.get_mut(network_interface_id) else {
            return false;
        };
        let subnet_id = interface.subnet_id.clone();
        let entries = interface.private_ip_addresses.get_or_insert_with(Vec::new);
        for address in addresses {
            entries.push(NetworkInterfacePrivateIpAddress {
                // No Elastic IP address is associated with an address until something
                // associates one.
                association: None,
                // An address an assignment adds is a SECONDARY address: the interface keeps
                // the primary one it already had.
                primary: Some(false),
                private_dns_name: Some(private_dns_name(address, region)),
                private_ip_address: Some(address.clone()),
            });
        }
        let charged: usize = addresses.iter().map(String::len).sum();
        self.add_stored_bytes(charged);
        for address in addresses {
            self.assigned_private_ips.insert(address.clone());
        }
        if let Some(subnet) = subnet_id.and_then(|subnet_id| self.subnets.get_mut(&subnet_id)) {
            let taken = i32::try_from(addresses.len()).unwrap_or(i32::MAX);
            subnet.available_ip_address_count = Some(
                subnet
                    .available_ip_address_count
                    .unwrap_or_default()
                    .saturating_sub(taken)
                    .max(0),
            );
        }
        true
    }

    /// Take the SECONDARY private IPv4 address `address` off the interface
    /// `network_interface_id`, and answer whether it held it as one.
    ///
    /// The exact reverse of [`Self::assign_secondary_private_ips`]: the entry goes, its
    /// bytes go back to the store's budget, the address goes back to the one in-use pool so
    /// something else can be given it, and the subnet gets its
    /// `AvailableIpAddressCount` back.
    ///
    /// `false` — and NOTHING changed — when this store holds no such interface, when the
    /// interface does not hold the address, or when it holds it as its PRIMARY address: an
    /// interface cannot be left without the primary address it was created with, so that
    /// one is not a secondary address to take off.
    pub fn release_secondary_private_ip(
        &mut self,
        network_interface_id: &str,
        address: &str,
    ) -> bool {
        let Some(interface) = self.network_interfaces.get_mut(network_interface_id) else {
            return false;
        };
        let subnet_id = interface.subnet_id.clone();
        let Some(entries) = interface.private_ip_addresses.as_mut() else {
            return false;
        };
        let Some(position) = entries
            .iter()
            .position(|entry| entry.private_ip_address.as_deref() == Some(address))
        else {
            return false;
        };
        if entries[position].primary.unwrap_or(position == 0) {
            return false;
        }
        entries.remove(position);
        self.release_stored_bytes(address.len());
        self.release_private_ip(address);
        if let Some(subnet) = subnet_id.and_then(|subnet_id| self.subnets.get_mut(&subnet_id)) {
            subnet.available_ip_address_count = Some(
                subnet
                    .available_ip_address_count
                    .unwrap_or_default()
                    .saturating_add(1),
            );
        }
        true
    }

    /// Put back every secondary private IPv4 address a half-finished REASSIGNMENT had
    /// already taken off the interface that held it, as `moved` recorded them
    /// (`(network_interface_id, address)`, as [`Self::release_secondary_private_ip`] was
    /// asked for each).
    ///
    /// The counterpart of [`Self::release_private_ips`] for the addresses a move takes from
    /// ANOTHER interface: a reassignment that is abandoned part way has to leave every donor
    /// holding what it held, or an address is held by nothing for the lifetime of the store.
    /// An address only ever moves off an interface that held it as a SECONDARY address, so
    /// putting it back as one restores exactly what was taken.
    pub fn restore_secondary_private_ips(&mut self, moved: &[(String, String)], region: &str) {
        for (network_interface_id, address) in moved {
            let _ = self.assign_secondary_private_ips(
                network_interface_id,
                ::std::slice::from_ref(address),
                region,
            );
        }
    }
}
