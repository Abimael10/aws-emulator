// SPDX-License-Identifier: CC0-1.0

//! Which NETWORK TYPES a subnet, and a group of subnets, supports — read off the address
//! families the subnets are addressed in.
//!
//! `Subnet$SupportedNetworkTypes` is "the network types supported by this subnet. Returns
//! an array of strings that can include 'ipv4', 'ipv6', or both, indicating whether the
//! subnet supports IPv4 only, IPv6 only, or dual-stack deployments", and
//! `SubnetGroup$SupportedNetworkTypes` is "the network types supported by this subnet
//! group ... indicating the IP address types that can be used for clusters deployed in
//! this subnet group". So which types a subnet offers FOLLOWS from the address families it
//! is addressed in, and the list is those families and nothing else: 'ipv4' for a subnet
//! with an IPv4 block, 'ipv6' for one with an IPv6 block, "or both" for one addressed in
//! both — which is what the member's own documentation calls a dual-stack deployment, rather
//! than a third element. The modelled `NetworkType` enum's `dual_stack` value is what a
//! CLUSTER's own addressing is reported as (`Cluster$NetworkType`: "Returns 'ipv4' for IPv4
//! only, 'ipv6' for IPv6 only, or 'dual-stack' if the cluster supports both"), not an
//! address family a subnet carries.
//!
//! Those blocks belong to Amazon EC2's record of the subnet, so they are read off what that
//! service answered ([`super::subnet_placement`]) and nothing is chosen here — which is why
//! the member is reported at all rather than invented.
//!
//! The same AWS rule is implemented for the sibling services whose own subnet groups publish
//! the member — `service-com-amazonaws-elasticache-amazonelasticachev9`'s
//! `src/state/network_types.rs` and `service-com-amazonaws-docdb-amazonrdsv19`'s
//! `src/state/ec2_subnets.rs`. A service crate does not depend on another service crate, and
//! the shared contract this crate does share with them
//! (`service_common::network`) is documentation only, so each
//! states the derivation for its own model; the note is here so the copies can be kept in
//! agreement, and consolidating them into a shared helper is a refactor of its own.

/// `NetworkType` `ipv4`, the type a subnet addressed in IPv4 offers. One of the modelled
/// enum's values (the generated `crate::ops::types::NETWORK_TYPE_VALUES` catalog:
/// `dual_stack`, `ipv4`, `ipv6`).
pub const NETWORK_TYPE_IPV4: &str = "ipv4";

/// `NetworkType` `ipv6`; see [`NETWORK_TYPE_IPV4`].
pub const NETWORK_TYPE_IPV6: &str = "ipv6";

/// The network types a subnet addressed in these families supports, in the order the
/// modelled `NetworkType` enum declares them: the IPv4 type, the IPv6 type, or both for a
/// subnet addressed in both (see this module's own documentation for why "both" is the whole
/// list for a dual-stack subnet).
///
/// A subnet Amazon EC2 reports no block of either family for supports none: there is no
/// address a node in it could be reached at, and a type is not invented for it.
pub fn subnet_network_types(ipv4: bool, ipv6: bool) -> Vec<String> {
    let mut supported = Vec::with_capacity(2);
    if ipv4 {
        supported.push(NETWORK_TYPE_IPV4.to_string());
    }
    if ipv6 {
        supported.push(NETWORK_TYPE_IPV6.to_string());
    }
    supported
}

/// The network types a subnet group holding `subnets` supports: the ones EVERY one of its
/// subnets supports, in the order the subnets report them.
///
/// The intersection rather than the union, because "MemoryDB uses that subnet group to
/// choose a subnet and IP addresses within that subnet to associate with your nodes"
/// (`CreateSubnetGroup`): a type one subnet of the group cannot carry is not one a cluster
/// deployed in the group can be given. A group holding no subnet supports none.
pub fn group_network_types(subnets: &[super::StoredSubnet]) -> Vec<String> {
    let mut remaining = subnets.iter();
    let Some(first) = remaining.next() else {
        return Vec::new();
    };
    let mut shared = first.supported_network_types.clone();
    for subnet in remaining {
        shared.retain(|supported| subnet.supported_network_types.contains(supported));
    }
    shared
}

/// `supported` as a modelled `NetworkTypeList` member reports it: absent when there is
/// nothing to report, rather than an empty list.
pub fn reported_network_types(supported: Vec<String>) -> Option<Vec<String>> {
    (!supported.is_empty()).then_some(supported)
}
