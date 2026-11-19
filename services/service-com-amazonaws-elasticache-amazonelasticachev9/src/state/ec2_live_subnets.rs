// SPDX-License-Identifier: CC0-1.0

//! The subnets a cache subnet group holds, as the service that OWNS subnets holds them NOW.
//!
//! A cache subnet group's `Subnets` and `SupportedNetworkTypes` are DERIVED from the subnets
//! it was built over: the group stores which subnets it names, and every other fact about
//! one — the Availability Zone it sits in, the address families it is addressed in — belongs
//! to Amazon EC2's record of that subnet. A subnet EC2 no longer holds is therefore no
//! longer a member of the group: the group itself survives the subnets it was built over
//! (nothing of a cache subnet group is deleted when a subnet is), keeps its name and the
//! `VpcId` it was created with, and its derived membership shrinks to the subnets that are
//! still there — down to none at all.
//!
//! So a reader resolves the stored identifiers against the owner's inventory rather than
//! reporting the request's list back: without that, a group would go on reporting
//! membership of subnets `ec2:DescribeSubnets` answers nothing for, and a client
//! reconciling cache placement against EC2 could not tell an orphaned group from a live
//! one.
//!
//! This is a READ of the owner's records: nothing here writes to either store, so reading a
//! group changes nothing a later caller sees.
//!
//! LOCKING. The EC2 store lock is taken for one resolution and dropped before returning,
//! and the caller holds no guard of this service's own store while it asks — the same order
//! [`super::ec2_cache_node_interfaces`] takes the two in, so no two store locks are ever
//! held at once.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::{AccountSubnet, SubnetRecord};

/// The subnets `subnet_ids` names that Amazon EC2 holds for the account right now, in the
/// order they were named.
///
/// `None` means the account's EC2 inventory could not be consulted at all — this emulator
/// was stood up without the service that owns subnets — in which case a caller reports what
/// it stored rather than claiming every subnet has gone away.
///
/// An identifier EC2 holds no subnet for is simply absent from the answer, which is what
/// makes a deleted subnet drop out of every group that named it.
pub fn held_subnets<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    subnet_ids: &[String],
) -> Option<Vec<AccountSubnet>> {
    let state = ec2::service_state(engine)?;
    let store = state.lock();
    Some(
        subnet_ids
            .iter()
            .filter_map(|subnet_id| store.subnets.get(subnet_id))
            .map(|subnet| AccountSubnet {
                subnet_id: subnet.subnet_id.clone().unwrap_or_default(),
                // The VPC and the zone are read off the owner's record, exactly as a create
                // reads them off the answer EC2 gave it.
                vpc_id: subnet.vpc_id.clone().unwrap_or_default(),
                availability_zone: subnet.availability_zone.clone().unwrap_or_default(),
                has_ipv4_cidr: subnet
                    .cidr_block
                    .as_deref()
                    .is_some_and(|cidr| !cidr.is_empty()),
                has_ipv6_cidr: subnet
                    .ipv6_cidr_block_association_set
                    .iter()
                    .flatten()
                    .any(|association| {
                        association
                            .ipv6_cidr_block
                            .as_deref()
                            .is_some_and(|cidr| !cidr.is_empty())
                    }),
            })
            .collect(),
    )
}

/// `held` as the subnets of a cache subnet group, in the order they were named.
///
/// The ONE reading of an owner's subnet record as a group's member subnet, so the subnets a
/// create stores and the subnets a read derives carry the same zone and the same network
/// types.
pub fn group_subnets(held: Vec<AccountSubnet>) -> Vec<SubnetRecord> {
    held.into_iter()
        .map(|subnet| SubnetRecord {
            // The network types the subnet supports follow from the blocks EC2 reported for
            // it; see [`super::network_types`].
            supported_network_types: subnet.supported_network_types(),
            subnet_identifier: subnet.subnet_id,
            availability_zone: subnet.availability_zone,
        })
        .collect()
}

/// `group` as the modelled `CacheSubnetGroup`, over the subnets the OWNER of subnets holds
/// for it NOW.
///
/// The one place a reader of a SINGLE group performs the resolution ([`held_subnets`]) and
/// the derivation ([`group_subnets_of`]) together, so every answer that carries one group
/// reports the membership and the network types a describe of the same group reports. A
/// deployment stood up without the owner of subnets has no inventory to resolve against, and
/// the group then reports what it stored.
///
/// LOCKING: as [`held_subnets`] — the caller must hold no guard of this service's own store.
pub fn reported_live<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    group: &super::CacheSubnetGroupRecord,
) -> crate::ops::types::CacheSubnetGroup {
    let named: Vec<String> = group
        .subnets
        .iter()
        .map(|subnet| subnet.subnet_identifier.clone())
        .collect();
    match held_subnets(engine, &named) {
        Some(held) => group.reported_over(&group_subnets_of(group, &held)),
        None => group.to_modeled(),
    }
}

/// The membership of ONE group out of the subnets `held` reports, in the order the group
/// names them.
///
/// For a reader that resolved the identifiers of several groups in one hop
/// ([`held_subnets`]): each group keeps its own order, and an identifier absent from `held`
/// is absent from its membership — the same derivation [`group_subnets`] performs for a
/// single group's own answer.
pub fn group_subnets_of(
    group: &super::CacheSubnetGroupRecord,
    held: &[AccountSubnet],
) -> Vec<SubnetRecord> {
    let named: Vec<AccountSubnet> = group
        .subnets
        .iter()
        .filter_map(|stored| {
            held.iter()
                .find(|subnet| subnet.subnet_id == stored.subnet_identifier)
                .cloned()
        })
        .collect();
    group_subnets(named)
}
