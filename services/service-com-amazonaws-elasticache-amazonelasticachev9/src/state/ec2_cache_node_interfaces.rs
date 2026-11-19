// SPDX-License-Identifier: CC0-1.0

//! The elastic network interface a cache NODE is reached at, in the inventory Amazon VPC
//! holds.
//!
//! A cache cluster placed in a cache subnet group runs INSIDE the account's VPC: each of
//! its nodes sits in one of the group's subnets, holds a private address of that subnet, and
//! is governed by the cluster's VPC security groups. That presence is an elastic network
//! interface — one per node — which ElastiCache creates and MANAGES on the account's behalf
//! ([`CACHE_INTERFACE_REQUESTER`], which is what `NetworkInterface$RequesterManaged`
//! reports), so a client doing an address-level diagnosis of its VPC finds the cache with
//! `ec2:DescribeNetworkInterfaces` — by the subnet it is in, by the security group that
//! guards it, or by the address it holds.
//!
//! Those interfaces are not this service's records: they live in the account's EC2
//! inventory, the one `DescribeNetworkInterfaces` answers from, so they are written there
//! rather than kept in a copy of this store that no reader of that inventory can see.
//!
//! RECONCILIATION, not bookkeeping. One pass makes that inventory agree with the cache
//! nodes this store holds right now: a node without its interface gets one, an interface
//! whose node is gone (a deleted cluster, a scaled-down node) is given back with the
//! address it held, and an interface whose node has moved subnets or changed security
//! groups is brought into line. So every operation that adds or removes a node — a create,
//! a delete, a node-count or replica-count change — drives the same pass, and none of them
//! has to remember an interface id.
//!
//! LOCK ORDER. The pass takes this store's lock, releases it, and only then takes the EC2
//! store's: no two store locks are ever held at once, and EC2 never reaches this service, so
//! no cycle exists.

use ::std::collections::BTreeSet;

use ::service_com_amazonaws_ec2_amazonec2::ops::types::NetworkInterfaceAttachment;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_com_amazonaws_ec2_amazonec2::ComAmazonawsEc2Amazonec2State;

use super::ComAmazonawsElasticacheAmazonelasticachev9State;

/// `NetworkInterface$RequesterId` of an interface ElastiCache created to carry the traffic
/// of a cache node of the account: the alias the service is published under, which is also
/// what makes the interface one "being managed by Amazon Web Services"
/// (`NetworkInterface$RequesterManaged`) rather than one the account created for itself.
pub const CACHE_INTERFACE_REQUESTER: &str = "amazon-elasticache";

/// The device index a cache node's interface is attached at: the node's ONLY interface, and
/// the account holds no host of its own for it.
const CACHE_INTERFACE_DEVICE_INDEX: i32 = 0;

/// How the account's EC2 inventory describes the interface of one cache node, so a caller
/// reading that inventory can tell which node an interface belongs to: `ElastiCache` and the
/// node's own `<cluster-id>-<node-id>`.
///
/// Also the interface's IDENTITY for the pass below — one node has one interface — so the
/// reconciliation needs no second record of which interface belongs to which node.
#[must_use]
pub fn interface_description(cache_cluster_id: &str, cache_node_id: &str) -> String {
    format!("ElastiCache {cache_cluster_id}-{cache_node_id}")
}

/// What ONE cache node needs in the account's EC2 inventory: where its interface goes and
/// what governs it.
///
/// Collected under THIS store's lock ([`node_interfaces_wanted`]) and answered against the
/// EC2 store under that store's own, so no two locks are held at once.
#[derive(Debug, Clone)]
struct WantedInterface {
    /// The interface's identity and description; see [`interface_description`].
    description: String,
    /// The subnet of the cluster's cache subnet group the node sits in.
    subnet_id: String,
    /// The VPC security groups the cluster carries, which govern its interfaces.
    security_groups: Vec<String>,
    /// The Region the cluster is in, which the interface's private DNS name is built for.
    region: String,
}

/// Make the account's EC2 inventory agree with the cache nodes this store holds.
///
/// Called with NO guard of this service's store held, by every operation that adds or
/// removes a cache node. ElastiCache creates a node's interface while the cluster is still
/// `creating` — the node is addressable before it is available — so the create drives this
/// pass as soon as the record is stored.
///
/// Nothing is placed for a cluster outside a cache subnet group (it is in no VPC of the
/// account, so it has no subnet to sit in), for a subnet Amazon VPC holds no record of, for
/// a subnet with no address left, or on an engine that holds no EC2 inventory at all: an
/// interface is that service's record, and this path never makes a second home for one.
pub fn settle_cache_node_interfaces<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let wanted = {
        let store = state.lock();
        node_interfaces_wanted(&store)
    };
    let Some(ec2_state) = ec2::service_state(engine) else {
        return;
    };
    let mut ec2_store = ec2_state.lock();
    // The interfaces of nodes that have gone go FIRST, so the addresses they held are back
    // in their subnets before this pass takes one for a node that is arriving.
    let wanted_descriptions: BTreeSet<&str> = wanted
        .iter()
        .map(|interface| interface.description.as_str())
        .collect();
    let mut held: BTreeSet<String> = BTreeSet::new();
    let managed: Vec<(String, String, Option<String>)> = ec2_store
        .network_interfaces
        .iter()
        .filter(|(_, interface)| {
            interface.requester_managed == Some(true)
                && interface.requester_id.as_deref() == Some(CACHE_INTERFACE_REQUESTER)
        })
        .map(|(network_interface_id, interface)| {
            (
                network_interface_id.clone(),
                interface.description.clone().unwrap_or_default(),
                interface.subnet_id.clone(),
            )
        })
        .collect();
    for (network_interface_id, description, subnet_id) in managed {
        // An interface whose node is gone, and one whose node now sits in another subnet,
        // are both given back: the second is replaced below by one in the subnet the node
        // is placed in now, because an interface cannot move between subnets.
        let node_moved = wanted
            .iter()
            .find(|interface| interface.description == description)
            .is_some_and(|interface| Some(interface.subnet_id.as_str()) != subnet_id.as_deref());
        if !wanted_descriptions.contains(description.as_str()) || node_moved {
            ec2_store.remove_network_interface(&network_interface_id);
            continue;
        }
        held.insert(description.clone());
        // A node that keeps its interface keeps its address and its id; what a change to
        // the cluster's security groups changes about it is which groups govern it.
        if let Some(interface) = wanted
            .iter()
            .find(|interface| interface.description == description)
        {
            let groups = group_identifiers(&ec2_store, interface);
            if let Some(stored) = ec2_store.network_interfaces.get_mut(&network_interface_id) {
                stored.groups = Some(groups);
            }
        }
    }
    for interface in &wanted {
        if !held.contains(&interface.description) {
            place_interface(&mut ec2_store, interface);
        }
    }
}

/// The interface every cache node this store holds is owed, one per node.
///
/// A node's subnet is the one of its cluster's cache subnet group that sits in the node's
/// own Availability Zone — a node is created in a zone and stays there — and, for a node
/// whose zone the group does not hold, the group's subnets taken in turn, so the nodes of a
/// cluster spread across the placement the group describes rather than piling into one
/// subnet.
///
/// A pass over the clusters this store holds, which is bounded by the service's own cluster
/// and node quotas, so what one call costs follows what the store HOLDS rather than
/// anything in a request.
fn node_interfaces_wanted(
    store: &ComAmazonawsElasticacheAmazonelasticachev9State,
) -> Vec<WantedInterface> {
    let mut wanted: Vec<WantedInterface> = Vec::new();
    for cluster in store.cache_clusters.values() {
        // No cache subnet group means no VPC placement: such a cluster has no subnet of the
        // account to sit in, so nothing is placed for it.
        let Some(group) = cluster
            .cache_subnet_group_name
            .as_deref()
            .map(super::stored_subnet_group_name)
            .and_then(|name| store.cache_subnet_groups.get(&name))
        else {
            continue;
        };
        if group.subnets.is_empty() {
            continue;
        }
        for (index, node) in cluster.nodes.iter().enumerate() {
            let subnet = node
                .availability_zone
                .as_deref()
                .and_then(|zone| {
                    group
                        .subnets
                        .iter()
                        .find(|subnet| subnet.availability_zone == zone)
                })
                .unwrap_or_else(|| &group.subnets[index % group.subnets.len()]);
            wanted.push(WantedInterface {
                description: interface_description(&cluster.cache_cluster_id, &node.cache_node_id),
                subnet_id: subnet.subnet_identifier.clone(),
                security_groups: cluster.security_group_ids.clone(),
                region: cluster.region.clone(),
            });
        }
    }
    wanted
}

/// The security groups of one wanted interface, as the EC2 inventory identifies them.
///
/// The groups the CLUSTER carries govern network access to its nodes
/// (`CreateCacheCluster$SecurityGroupIds`: "one or more VPC security groups associated with
/// the cluster"), so they are the groups of its interfaces. A cluster that names none runs
/// under the VPC's own default group, which is the group such a placement gets.
fn group_identifiers(
    ec2_store: &ComAmazonawsEc2Amazonec2State,
    wanted: &WantedInterface,
) -> Vec<::service_com_amazonaws_ec2_amazonec2::ops::types::GroupIdentifier> {
    let vpc_id = ec2_store
        .subnets
        .get(&wanted.subnet_id)
        .and_then(|subnet| subnet.vpc_id.clone())
        .unwrap_or_default();
    if wanted.security_groups.is_empty() {
        return ec2_store
            .default_security_group_of_vpc(&vpc_id)
            .into_iter()
            .collect();
    }
    wanted
        .security_groups
        .iter()
        .map(|group_id| {
            ec2_store.group_identifier_of(Some(vpc_id.as_str()), Some(group_id.as_str()), None)
        })
        .collect()
}

/// Place ONE cache node's interface in the account's EC2 inventory, inside a guard the
/// caller already holds.
///
/// Everything the interface carries comes off the subnet it goes in and the cluster it
/// serves: the subnet's VPC, zone and CIDR block, and the cluster's security groups. The id,
/// the MAC address, the private DNS name and the status are Amazon EC2's own to derive,
/// which is why the record is written through that store's one writer seam
/// ([`ComAmazonawsEc2Amazonec2State::insert_network_interface`]) rather than assembled here.
///
/// Nothing is placed for a subnet that store holds no record of or no CIDR block for, when
/// the subnet's range has no free address left, or when the inventory is full — a cache node
/// without an interface is reported exactly as it was before, rather than through an
/// interface carrying an invented address.
fn place_interface(ec2_store: &mut ComAmazonawsEc2Amazonec2State, wanted: &WantedInterface) {
    let Some(subnet) = ec2_store.subnets.get(&wanted.subnet_id) else {
        return;
    };
    let vpc_id = subnet.vpc_id.clone().unwrap_or_default();
    let availability_zone = subnet.availability_zone.clone().unwrap_or_default();
    let availability_zone_id = subnet
        .availability_zone_id
        .clone()
        .or_else(|| ec2::zone_id_for_zone_name(&availability_zone))
        .unwrap_or_default();
    let Some(cidr_block) = subnet.cidr_block.clone() else {
        return;
    };
    if !ec2_store.has_network_interface_capacity() {
        return;
    }
    // The address the interface holds comes out of its SUBNET, taken from the one pool every
    // address of that store is drawn from, so no other resource of the account can be handed
    // the address this cache node is reached at.
    let Some(address) = ec2_store.mint_private_ip_in_cidr(&cidr_block) else {
        return;
    };
    let groups = group_identifiers(ec2_store, wanted);
    let attachment = NetworkInterfaceAttachment {
        attach_time: Some(ec2::system_time_to_date_time(::std::time::SystemTime::now())),
        // The attachment id is of the SERVICE-MANAGED kind: no instance of the account holds
        // this interface, the cache node does, so the id's own form tells a caller reading the
        // inventory that the attachment is not one it may take down.
        attachment_id: Some(ec2_store.next_id(ec2::SERVICE_NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX)),
        // The interface ElastiCache creates for a node is torn down with that node.
        delete_on_termination: Some(true),
        device_index: Some(CACHE_INTERFACE_DEVICE_INDEX),
        network_card_index: Some(0),
        // No instance of the account holds it: the cache node it serves does, and ElastiCache
        // is the owner of that attachment.
        instance_id: None,
        instance_owner_id: Some(CACHE_INTERFACE_REQUESTER.to_string()),
        status: Some(ec2::ATTACHMENT_STATUS_ATTACHED.to_string()),
        // ENA Express is a setting of an attachment something asked for; this asks for none.
        ena_srd_specification: None,
        ena_queue_count: None,
    };
    let spec = ec2::NetworkInterfaceSpec {
        subnet_id: wanted.subnet_id.clone(),
        vpc_id,
        availability_zone,
        availability_zone_id,
        region: wanted.region.clone(),
        description: wanted.description.clone(),
        interface_type: ec2::NETWORK_INTERFACE_TYPE_INTERFACE.to_string(),
        private_ip_addresses: vec![address.clone()],
        groups,
        attachment: Some(attachment),
        // ElastiCache created it to carry the traffic of a cache of the account, so
        // ElastiCache manages it.
        requester: Some(CACHE_INTERFACE_REQUESTER.to_string()),
        // The cluster carries the create's tags; its interfaces are not the resource the
        // request tagged, and nothing here asks for IPv6 space.
        ..Default::default()
    };
    if ec2_store.insert_network_interface(spec).is_none() {
        // The inventory holds no more interfaces, so the address this one would have held
        // goes back to the subnet it came out of.
        ec2_store.release_private_ip(&address);
    }
}
