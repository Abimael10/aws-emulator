// SPDX-License-Identifier: CC0-1.0

//! The IPv6 ADDRESSES AND PREFIXES one network interface specification asks for.
//!
//! The IPv6 counterpart of [`super::interface_private_ips`], and written for the same reason:
//! the four members that ask for IPv6 space (`Ipv6Addresses`, `Ipv6AddressCount`,
//! `Ipv6Prefixes`, `Ipv6PrefixCount`), plus the `EnablePrimaryIpv6` flag, carry documented
//! combination rules, and those rules belong in ONE place so no two writers hold one caller's
//! request to different terms — nor refuse the same combination with two sentences. The
//! refusal a subnet with no IPv6 range of its own earns is
//! [`super::subnet_has_no_ipv6_range`], which every operation that reads such a range shares.
//!
//! What this answers is only WHICH addresses and prefixes were asked for and HOW MANY the
//! interface holds. Choosing them is the store's own — the subnet's block
//! ([`super::ComAmazonawsEc2Amazonec2State::subnet_ipv6_cidr_block`]) is the only space they
//! come out of, judged against what the account already holds
//! ([`super::ipv6_address_is_taken`] / [`super::ipv6_prefix_is_free`]) — so every IPv6
//! address of a subnet still comes out of the one reading of that space.

use crate::ops::types::{InstanceIpv6Address, Ipv6PrefixSpecificationRequest};
use crate::scaffolded::Scaffolded;

use super::{
    canonical_ipv6_address, canonical_ipv6_cidr, efa_only_interface_has_no_ip_addresses,
    free_subnet_ipv6_addresses, free_subnet_ipv6_prefixes, insufficient_free_ipv6_addresses,
    invalid_parameter_combination, invalid_parameter_value, ipv6_address_is_taken,
    ipv6_cidr_block_contains, ipv6_prefix_is_free, is_assignable_subnet_ipv6_address,
    missing_parameter, ComAmazonawsEc2Amazonec2State, MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE,
};

/// What one interface's IPv6 members resolved to.
#[derive(Debug, Clone, Default)]
pub struct RequestedInterfaceIpv6 {
    /// The IPv6 addresses the request NAMED, in the order it named them.
    pub named_addresses: Vec<String>,
    /// How many IPv6 addresses the interface is to hold in all — the named ones, or the count
    /// the request asked the service to choose. Zero for an interface that asks for none.
    pub wanted_addresses: usize,
    /// The IPv6 prefixes the request NAMED, in the order it named them.
    pub named_prefixes: Vec<String>,
    /// How many IPv6 prefixes the interface is to be delegated in all, on the same terms as
    /// [`Self::wanted_addresses`].
    pub wanted_prefixes: usize,
    /// "Determines if an IPv6 address associated with a network interface is the primary IPv6
    /// address" (`InstanceIpv6Address$IsPrimaryIpv6`): whether the request asked for the
    /// interface's first IPv6 address to be that primary one.
    pub primary: bool,
}

impl RequestedInterfaceIpv6 {
    /// Does the request ask for ANY IPv6 space at all?
    ///
    /// What decides whether the subnet's own IPv6 range has to be read: an interface that asks
    /// for none needs none, so a request that asks for nothing is served in an IPv4-only
    /// subnet exactly as it always was.
    pub fn asks_for_anything(&self) -> bool {
        self.wanted_addresses > 0 || self.wanted_prefixes > 0
    }
}

/// Read the IPv6 members of ONE interface specification.
///
/// `addresses` is the specification's `Ipv6Addresses`, `address_count` its `Ipv6AddressCount`,
/// `prefixes` its `Ipv6Prefixes`, `prefix_count` its `Ipv6PrefixCount`, `primary_ipv6` its
/// `EnablePrimaryIpv6` and `efa_only` whether its interface type is the one that carries no IP
/// addresses at all.
///
/// Every refusal here is one of the members' own documented rules, asked BEFORE any address is
/// resolved so the answer never depends on which member was looked at first:
///
/// * "You can't specify a count of IPv6 addresses using this parameter if you've specified one
///   of the following: specific IPv6 addresses, specific IPv6 prefixes, or a count of IPv6
///   prefixes" (`Ipv6AddressCount`), which the other three members state from their own side —
///   so at most ONE of the four may ask;
/// * "If you specify efa-only, do not assign any IP addresses to the network interface"
///   (`InterfaceType`), through the one wording site that rule has;
/// * each entry has to carry the address or the prefix it specifies;
/// * the per-interface ceiling [`MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE`] — "you can assign as
///   many IPv6 addresses to a network interface as you can assign private IPv4 addresses"
///   (`AssignIpv6Addresses`) — asked both of the list a caller sent, before any work
///   proportional to its size is done, and of the total the interface would then hold.
pub fn requested_interface_ipv6<E>(
    addresses: &[InstanceIpv6Address],
    address_count: i32,
    prefixes: &[Ipv6PrefixSpecificationRequest],
    prefix_count: i32,
    primary_ipv6: bool,
    efa_only: bool,
) -> Result<RequestedInterfaceIpv6, Scaffolded<E>> {
    // Neither list carries a `@length`, so the list a caller may send is unbounded; the
    // per-interface ceiling is enforced HERE, before any work proportional to its size —
    // parsing an address, probing what the account holds — is done.
    if addresses.len() > MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE {
        return Err(invalid_parameter_value(format!(
            "The request names {} IPv6 addresses, above the maximum of {max} per network \
             interface",
            addresses.len(),
            max = MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE,
        )));
    }
    if prefixes.len() > MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE {
        return Err(invalid_parameter_value(format!(
            "The request names {} IPv6 prefixes, above the maximum of {max} per network \
             interface",
            prefixes.len(),
            max = MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE,
        )));
    }
    // The model declares no `@range` on either count, so a negative one reaches here; a
    // number of addresses or prefixes to assign cannot be below zero.
    if address_count < 0 {
        return Err(invalid_parameter_value(format!(
            "Invalid value '{address_count}' for ipv6AddressCount. It cannot be negative"
        )));
    }
    if prefix_count < 0 {
        return Err(invalid_parameter_value(format!(
            "Invalid value '{prefix_count}' for ipv6PrefixCount. It cannot be negative"
        )));
    }
    // At most one of the four may ask, which is the one rule all four members state.
    let asking = [
        !addresses.is_empty(),
        address_count > 0,
        !prefixes.is_empty(),
        prefix_count > 0,
    ]
    .into_iter()
    .filter(|asks| *asks)
    .count();
    if asking > 1 {
        return Err(invalid_parameter_combination(
            "You may specify only one of ipv6Addresses, ipv6AddressCount, ipv6Prefixes and \
             ipv6PrefixCount",
        ));
    }
    if efa_only && (asking > 0 || primary_ipv6) {
        return Err(efa_only_interface_has_no_ip_addresses());
    }
    let mut named_addresses: Vec<String> = Vec::with_capacity(addresses.len());
    for address in addresses {
        let Some(address) = address.ipv6_address.as_deref().filter(|address| !address.is_empty())
        else {
            return Err(missing_parameter(
                "Each entry of Ipv6Addresses must carry an ipv6Address",
            ));
        };
        named_addresses.push(address.to_string());
    }
    let mut named_prefixes: Vec<String> = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
        let Some(prefix) = prefix.ipv6_prefix.as_deref().filter(|prefix| !prefix.is_empty()) else {
            return Err(missing_parameter(
                "Each entry of Ipv6Prefixes must carry an ipv6Prefix",
            ));
        };
        named_prefixes.push(prefix.to_string());
    }
    // One of each pair is zero by the rule above, so the larger IS what was asked for.
    let mut wanted_addresses = named_addresses
        .len()
        .max(usize::try_from(address_count).unwrap_or_default());
    // "Amazon Web Services will automatically assign an IPv6 address associated with the ENI
    // attached to your instance to be the primary IPv6 address … the first IPv6 GUA will be
    // made the primary IPv6 address" (`EnablePrimaryIpv6`): the flag asks for a primary IPv6
    // address, so an interface that asks for the flag and for no address still gets the one
    // address that primary is.
    if primary_ipv6 && wanted_addresses == 0 {
        wanted_addresses = 1;
    }
    let wanted_prefixes = named_prefixes
        .len()
        .max(usize::try_from(prefix_count).unwrap_or_default());
    if wanted_addresses > MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE {
        return Err(invalid_parameter_value(format!(
            "The request asks for {wanted_addresses} IPv6 addresses, above the maximum of \
             {max} per network interface",
            max = MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE,
        )));
    }
    if wanted_prefixes > MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE {
        return Err(invalid_parameter_value(format!(
            "The request asks for {wanted_prefixes} IPv6 prefixes, above the maximum of \
             {max} per network interface",
            max = MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE,
        )));
    }
    Ok(RequestedInterfaceIpv6 {
        named_addresses,
        wanted_addresses,
        named_prefixes,
        wanted_prefixes,
        primary: primary_ipv6,
    })
}

/// The IPv6 space ONE interface is to be given: the addresses and the prefixes a resolution
/// settled on, in the order the interface holds them.
#[derive(Debug, Clone, Default)]
pub struct AssignedInterfaceIpv6 {
    /// The IPv6 addresses, the ones the request named first and the ones the service chose
    /// after them — "when you enable an IPv6 GUA address to be a primary IPv6, the first IPv6
    /// GUA will be made the primary IPv6 address" (`EnablePrimaryIpv6`), so the order matters.
    pub addresses: Vec<String>,
    /// The IPv6 prefixes, on the same terms.
    pub prefixes: Vec<String>,
}

impl ComAmazonawsEc2Amazonec2State {
    /// The IPv6 addresses and prefixes `requested` asks for, chosen out of the subnet range
    /// `block` — or the refusal the request earns, with nothing taken.
    ///
    /// The ONE resolution of those members in this crate, so an interface a create stores and
    /// one an assignment adds space to are given it on the same terms: an address the request
    /// NAMED is judged by the same window a service-chosen one is drawn from
    /// ([`is_assignable_subnet_ipv6_address`]) and against the same reading of what the
    /// account already holds ([`ipv6_address_is_taken`], which sees every address and prefix
    /// the account's interfaces carry), so the two cannot disagree; an address or prefix named
    /// twice in one request is in use the second time, exactly like one another interface
    /// holds.
    ///
    /// Nothing is written: an IPv6 address is held by the interface record that carries it, so
    /// the taking IS the write the caller makes afterwards — which is why a refusal here
    /// leaves the store exactly as it found it.
    pub fn resolve_requested_ipv6<E>(
        &self,
        block: &str,
        requested: &RequestedInterfaceIpv6,
    ) -> Result<AssignedInterfaceIpv6, Scaffolded<E>> {
        // What the account already holds, read ONCE off the interface records that ARE the
        // pool and grown with what this resolution hands out — so no refusal and no choice
        // below re-reads the store, and the work one request does is bounded by what it asks
        // for plus what the account holds.
        let mut taken_addresses = self.assigned_ipv6_addresses();
        let mut taken_prefixes = self.assigned_ipv6_prefixes();
        let mut addresses: Vec<String> = Vec::new();
        for address in &requested.named_addresses {
            let Some(canonical) = canonical_ipv6_address(address) else {
                return Err(invalid_parameter_value(format!(
                    "Value ({address}) for parameter ipv6Addresses is invalid. \
                     This is not a valid IPv6 address."
                )));
            };
            if !is_assignable_subnet_ipv6_address(block, &canonical) {
                return Err(invalid_parameter_value(format!(
                    "Value ({address}) for parameter ipv6Addresses is invalid. \
                     Address does not fall within the subnet's assignable address range \
                     {block}"
                )));
            }
            if ipv6_address_is_taken(&canonical, &taken_addresses, &taken_prefixes) {
                return Err(Scaffolded::invalid_ip_address_in_use(&canonical));
            }
            taken_addresses.push(canonical.clone());
            addresses.push(canonical);
        }
        // "Amazon EC2 automatically selects the IPv6 addresses from the subnet range"
        // (`Ipv6AddressCount`): the addresses the request did not name, the service chooses.
        let to_choose = requested.wanted_addresses.saturating_sub(addresses.len());
        if to_choose > 0 {
            let Some(chosen) =
                free_subnet_ipv6_addresses(block, &taken_addresses, &taken_prefixes, to_choose)
            else {
                return Err(insufficient_free_ipv6_addresses());
            };
            taken_addresses.extend(chosen.iter().cloned());
            addresses.extend(chosen);
        }

        // The prefixes, on the same terms: an IPv6 prefix is a range OF the subnet's block, so
        // it has to sit inside the assignable part of it and overlap nothing the account
        // already holds.
        let mut prefixes: Vec<String> = Vec::new();
        for prefix in &requested.named_prefixes {
            let Ok(canonical) = canonical_ipv6_cidr(prefix) else {
                return Err(invalid_parameter_value(format!(
                    "Value ({prefix}) for parameter ipv6Prefixes is invalid. \
                     This is not a valid IPv6 CIDR block."
                )));
            };
            // The block is canonical, so the text before the `/` is its LOWEST address: a
            // prefix inside the subnet's block whose lowest address is assignable holds no
            // reserved address at all.
            let lowest = canonical.split('/').next().unwrap_or_default().to_string();
            if !ipv6_cidr_block_contains(block, &canonical)
                || !is_assignable_subnet_ipv6_address(block, &lowest)
            {
                return Err(invalid_parameter_value(format!(
                    "Value ({prefix}) for parameter ipv6Prefixes is invalid. \
                     Prefix does not fall within the subnet's assignable address range \
                     {block}"
                )));
            }
            if !ipv6_prefix_is_free(&canonical, &taken_addresses, &taken_prefixes) {
                return Err(Scaffolded::invalid_ip_address_in_use(&canonical));
            }
            taken_prefixes.push(canonical.clone());
            prefixes.push(canonical);
        }
        // "The number of IPv6 prefixes that Amazon Web Services automatically assigns to the
        // network interface" (`Ipv6PrefixCount`): the service picks them out of the same
        // block, at the one size Prefix Delegation hands out.
        let to_delegate = requested.wanted_prefixes.saturating_sub(prefixes.len());
        if to_delegate > 0 {
            let Some(chosen) =
                free_subnet_ipv6_prefixes(block, &taken_addresses, &taken_prefixes, to_delegate)
            else {
                return Err(insufficient_free_ipv6_addresses());
            };
            prefixes.extend(chosen);
        }
        Ok(AssignedInterfaceIpv6 {
            addresses,
            prefixes,
        })
    }
}
