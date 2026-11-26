// SPDX-License-Identifier: CC0-1.0

//! WHO reaches a cache at the endpoint the control plane published for it.
//!
//! # The rule this module implements
//!
//! A cache placed in a VPC of the account runs INSIDE that VPC: each of its nodes holds a
//! private address there, behind an elastic network interface
//! ([`crate::state::ec2_cache_node_interfaces`]), and that interface is governed by the VPC
//! SECURITY GROUPS the cache carries ("one or more VPC security groups associated with the
//! cluster", `CreateCacheCluster$SecurityGroupIds`). A security group is a stateful filter on
//! such an interface: an inbound rule names a protocol, a port range and a source, "all rules
//! are permissive" so there is no deny rule, and traffic no rule of any group on the interface
//! names is simply DROPPED rather than refused. So the groups are the gate on WHO may open a
//! connection to a cache's endpoint, and they are read PER CONNECTION: revoking the rule that
//! named one client stops that client's next connection while a client another rule still
//! names keeps getting through, and authorizing it again lets it back in — with nothing of the
//! cache itself changed, and with the sessions already open untouched, because the filter is
//! stateful and judges a connection as it is established.
//!
//! The port the rules are read against is the port the cache ENGINE accepts connections on
//! (`CreateCacheCluster$Port`), which is what a client's rule names — never the port of the
//! socket this emulator serves the cache on.
//!
//! # Where the decision comes from
//!
//! Both halves are Amazon VPC's own records, read in Amazon EC2's store in the shared engine:
//! which groups guard the cache, and what rules those groups hold. The rules themselves are
//! read through the ONE place that crate decides each question
//! ([`ComAmazonawsEc2Amazonec2State::security_groups_admit_ingress`] and
//! [`ComAmazonawsEc2Amazonec2State::security_groups_admit_ingress_from_any_source`]), so this
//! module holds no second reading of what a rule admits — what is its own is only which groups
//! guard a cache and which port its engine answers on.
//!
//! WHERE a connection comes from is what the client states when it opens one: a client of this
//! emulator that reaches the published address in process carries the private address its own
//! traffic leaves from ([`::service_common::dialable_data_planes::connected_from`]), and that
//! address is matched against the rules together with the groups the interface holding it
//! carries — which is what makes a rule whose source is another security group a grant to the
//! members of that group. A connection the CORE accepted on the cache's own socket states
//! none, because the core hands its handlers a byte stream rather than a peer; then the half of
//! the question that does not depend on the sender is still decided — a filter no rule of which
//! names the cache's port over this protocol admits that connection from NOWHERE, so it is
//! dropped, and one that does name it leaves only the sender undecided, which blocks nothing.
//!
//! Nothing is decided from silence: a cache in no VPC of the account is behind no such
//! interface and is guarded by no VPC security group, and a cache whose guarding groups cannot
//! be read at all (an engine holding no EC2 inventory) leaves the question unasked — neither
//! blocks a connection.
//!
//! LOCK ORDER: the Amazon EC2 store's guard is taken and RELEASED inside each call here, and no
//! guard of this service's store is held across one — the same contract every other read of
//! that store from this crate has ([`crate::state::ec2_cache_node_interfaces`]).

use ::std::net::SocketAddr;

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_com_amazonaws_ec2_amazonec2::ComAmazonawsEc2Amazonec2State;

use super::CacheIdentity;

/// The protocol a client's connection to a cache is carried over, as a security group rule
/// states one (`IpPermission$IpProtocol`: "tcp | udp | icmp | icmpv6").
pub const CACHE_PROTOCOL: &str = "tcp";

/// WHERE a cache sits in the account's network and WHAT guards it there: the facts the
/// security groups on its node's interface are read against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodePlacement {
    /// The VPC the cache was placed in, as the service that owns subnets reported it for the
    /// cache subnet group the cache is in.
    pub vpc_id: String,
    /// The subnets the cache's placement names directly, for a cache that is placed in subnets
    /// rather than in a cache subnet group (a serverless cache): the VPC is read off them.
    pub subnet_ids: Vec<String>,
    /// The VPC security groups the cache carries, which are the groups guarding its nodes'
    /// interfaces. Empty for a cache whose request named none, which runs under the VPC's own
    /// default group ([`guarding_groups`]).
    pub group_ids: Vec<String>,
    /// The port the cache's ENGINE accepts connections on, which is the port a rule guarding
    /// this cache names.
    pub port: u16,
}

/// Where the cache `identity` names sits and what guards it, or `None` for a cache in no VPC
/// of the account — which no VPC security group guards — and for a cache this store no longer
/// holds.
///
/// Read under this service's own guard and answered with owned values, so the caller holds no
/// guard of it while the account's network is asked ([`CacheNetwork::admits`]).
#[must_use]
pub fn node_placement(
    store: &crate::state::ComAmazonawsElasticacheAmazonelasticachev9State,
    identity: &CacheIdentity,
) -> Option<NodePlacement> {
    match identity {
        CacheIdentity::CacheCluster { cache_cluster_id }
        | CacheIdentity::ReplicationGroupMember {
            cache_cluster_id, ..
        } => {
            // A member of a replication group is a cache CLUSTER of its own: the groups and
            // the placement guarding its node are that cluster's own record's, which the
            // create copied down from the group.
            let cluster = store.cache_clusters.get(cache_cluster_id)?;
            let placement = cluster
                .cache_subnet_group_name
                .as_deref()
                .map(crate::state::stored_subnet_group_name)
                .and_then(|name| store.cache_subnet_groups.get(&name))?;
            Some(NodePlacement {
                vpc_id: placement.vpc_id.clone(),
                subnet_ids: Vec::new(),
                group_ids: cluster.security_group_ids.clone(),
                port: u16::try_from(cluster.engine_port).unwrap_or_default(),
            })
        }
        CacheIdentity::ServerlessCache { name } | CacheIdentity::ServerlessCacheReader { name } => {
            let cache = store.serverless_caches.get(name)?;
            Some(NodePlacement {
                // A serverless cache is placed in SUBNETS rather than in a cache subnet group,
                // so the VPC is the one those subnets belong to, read off the service that
                // owns them.
                vpc_id: String::new(),
                subnet_ids: cache.subnet_ids.clone(),
                group_ids: cache.security_group_ids.clone(),
                // A serverless cache carries no `Port` member: it answers on the engine's own
                // client port, which is the port a rule guarding it names.
                port: u16::try_from(crate::state::default_port(&cache.engine)).unwrap_or_default(),
            })
        }
    }
}

/// The account's own network, as the question "is this connection admitted to the cache?" is
/// asked of it.
///
/// A handle on the Amazon EC2 store in the SAME shared engine this service's own store lives in
/// — clone-cheap and `'static`, so the RESP server of a cache can keep one beside the socket it
/// serves, which outlives the request that created the cache.
#[derive(Clone)]
pub struct CacheNetwork(::engine::ServiceState<ComAmazonawsEc2Amazonec2State>);

/// Written by hand rather than derived: the store behind the handle carries no `Debug`, and
/// what is diagnosable about this handle is that it is one — never an account's groups or the
/// addresses its clients hold in a log line.
impl ::std::fmt::Debug for CacheNetwork {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter.debug_struct("CacheNetwork").finish()
    }
}

impl CacheNetwork {
    /// The network of the SAME shared engine this service's store lives in, or `None` for an
    /// engine that vends none — on which no security group of the account can be read at all.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        ec2::service_state(engine).map(Self)
    }

    /// Whether the security groups guarding the cache `placement` describes admit a connection
    /// arriving from `from` at the cache's own port — `None` for a connection whose source is
    /// not stated (see this module's own documentation).
    ///
    /// A READ: deciding whether a connection is admitted moves no record of the cache and
    /// nothing of the network. The guard is released before the answer is returned, so no
    /// caller holds it while the cache serves the session.
    #[must_use]
    pub fn admits(&self, placement: &NodePlacement, from: Option<SocketAddr>) -> bool {
        let store = self.0.lock();
        let guarding = guarding_groups(&store, placement);
        if guarding.is_empty() {
            // Nothing of this emulator says which group guards this cache, so no rule can be
            // read either way: an unasked question blocks no connection.
            return true;
        }
        match from {
            Some(from) => {
                let source_ip = from.ip().to_string();
                // The groups the SENDER is a member of, which is what a rule whose source is
                // "the security group itself" names: the groups on the interface of this
                // account that holds the address the connection comes from. An address no
                // interface of the account holds is a member of no group of it.
                let source_groups = source_groups(&store, &source_ip);
                store.security_groups_admit_ingress(
                    &guarding,
                    CACHE_PROTOCOL,
                    placement.port,
                    &source_ip,
                    &source_groups,
                )
            }
            None => store.security_groups_admit_ingress_from_any_source(
                &guarding,
                CACHE_PROTOCOL,
                placement.port,
            ),
        }
    }
}

/// The `GroupId` of each security group guarding the nodes of the cache `placement` describes:
/// the groups the cache carries, and — for a cache whose request named none — the default
/// security group of the VPC it is placed in, which is the group such a placement gets
/// ([`ComAmazonawsEc2Amazonec2State::default_security_group_of_vpc`], the same reading the
/// cache's own interfaces are given).
fn guarding_groups(
    store: &ComAmazonawsEc2Amazonec2State,
    placement: &NodePlacement,
) -> Vec<String> {
    if !placement.group_ids.is_empty() {
        return placement.group_ids.clone();
    }
    let vpc_id = match placement.vpc_id.is_empty() {
        false => placement.vpc_id.clone(),
        true => placement
            .subnet_ids
            .iter()
            .filter_map(|subnet_id| store.subnets.get(subnet_id))
            .find_map(|subnet| subnet.vpc_id.clone())
            .unwrap_or_default(),
    };
    if vpc_id.is_empty() {
        return Vec::new();
    }
    store
        .default_security_group_of_vpc(&vpc_id)
        .and_then(|group| group.group_id)
        .into_iter()
        .collect()
}

/// The `GroupId` of each security group on the interface of this account that holds `address`
/// as a private address — empty where no interface of it holds that address.
///
/// The interface is found through that store's own reader of which interface holds an address
/// ([`ComAmazonawsEc2Amazonec2State::interface_holding_private_ip`]), so an address a client
/// states and an address Amazon EC2 reports for its interface resolve to the one record.
fn source_groups(store: &ComAmazonawsEc2Amazonec2State, address: &str) -> Vec<String> {
    let Some((network_interface_id, _)) = store.interface_holding_private_ip(address) else {
        return Vec::new();
    };
    store
        .network_interfaces
        .get(&network_interface_id)
        .map(|interface| {
            interface
                .groups
                .iter()
                .flatten()
                .filter_map(|group| group.group_id.clone())
                .collect()
        })
        .unwrap_or_default()
}
