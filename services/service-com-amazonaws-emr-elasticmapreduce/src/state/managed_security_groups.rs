// SPDX-License-Identifier: CC0-1.0

//! The ingress Amazon EMR AUTHORIZES onto the security groups it manages for a cluster.
//!
//! One concern of this service's state. A launch hands Amazon EMR the groups it is to manage
//! — "the identifier of the Amazon EC2 security group for the master node", "for the core and
//! task nodes" and "for the Amazon EMR service to access clusters in VPC private subnets"
//! (`JobFlowInstancesConfig$EmrManagedMasterSecurityGroup`,
//! `$EmrManagedSlaveSecurityGroup`, `$ServiceAccessSecurityGroup`) — and MANAGING them is
//! writing the rules the cluster needs onto them: the nodes of one cluster talk to each other
//! over every protocol, and the service reaches the primary node of a cluster in a private
//! subnet through the service-access group (Amazon EMR Management Guide, "Control network
//! traffic with security groups", which lists the inbound rules Amazon EMR puts on the groups
//! it manages: all ICMP, all TCP and all UDP from the primary and the core/task group on each
//! of those two groups, the service-access group's port to the nodes, and the nodes' port to
//! the service-access group).
//!
//! Those rules are not this service's resource: a security group rule belongs to Amazon EC2
//! and to the account that owns the group, and it is written THERE, over the in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no
//! other service's crate — the same grounding [`super::studio_network`] and
//! [`super::cluster_instances`] follow. What a caller then reads back with
//! `ec2:DescribeSecurityGroups` are rules Amazon EC2 itself holds, which is also what makes
//! them a live dependency of the groups: a group another group's rule references cannot be
//! deleted until that rule is revoked.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **which groups** — the ones the cluster's own record names
//!    ([`ManagedSecurityGroups::of`]); a cluster whose launch named none has no managed group
//!    for this service to write to, and nothing is written anywhere.
//! 2. **which rules** — one catalogue ([`managed_ingress`]), so what Amazon EMR adds is
//!    stated in exactly one place.
//! 3. **when** — the cluster's own lifecycle
//!    ([`ComAmazonawsEmrElasticmapreduceState::record_cluster_managed_group_changes`]): the
//!    rules are authorized ONCE, as the cluster is provisioned, whoever moved it there.
//!    Nothing takes them off again: the groups belong to the CALLER, so a rule written onto
//!    one of them stays there once the cluster that occasioned it has gone, and removing it
//!    is the caller's own `ec2:RevokeSecurityGroupIngress` — which is why the documented
//!    teardown of a cluster's managed groups is to revoke those rules and only then delete
//!    the groups (Amazon EMR Management Guide, "Control network traffic with security
//!    groups"). A caller that revokes them while the cluster is still up is not overruled
//!    either: the authorization is owed once per cluster and is not written again.

use super::*;

/// The protocol a rule naming every protocol of the ICMP family carries
/// (`IpPermission$IpProtocol`: "the IP protocol name (`tcp`, `udp`, `icmp`, `icmpv6`)").
const PROTOCOL_ICMP: &str = "icmp";

/// The same for TCP.
const PROTOCOL_TCP: &str = "tcp";

/// The same for UDP.
const PROTOCOL_UDP: &str = "udp";

/// The type and code of a rule admitting EVERY ICMP message: "if the start port is -1 (all
/// ICMP types), then the end port must be -1 (all ICMP codes)" (`IpPermission$ToPort`).
const ALL_ICMP: i32 = -1;

/// The ends of a rule admitting every port of a port-carrying protocol.
const LOWEST_PORT: i32 = 0;
const HIGHEST_PORT: i32 = 65_535;

/// The port the Amazon EMR service reaches a cluster's nodes on from the service-access group
/// of a cluster in a private subnet (Amazon EMR Management Guide, "Control network traffic
/// with security groups": the managed node groups admit TCP 8443 from the service-access
/// group).
const SERVICE_ACCESS_TO_NODES_PORT: i32 = 8_443;

/// The port the nodes reach the Amazon EMR service on, which is the one inbound rule the
/// service-access group of such a cluster carries (same reference: TCP 9443 from the primary
/// group).
const NODES_TO_SERVICE_ACCESS_PORT: i32 = 9_443;

/// How many authorizations may wait to be made in Amazon EC2 before the oldest is dropped: a
/// backstop on this process's memory. One cluster is owed one authorization ever, and the
/// cluster map is bounded ([`MAX_CLUSTERS`]), so this is far above what can really queue.
const MAX_PENDING_MANAGED_GROUP_ACTIONS: usize = 2 * MAX_CLUSTERS;

/// The security groups Amazon EMR manages for one cluster, as that cluster's record names
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedSecurityGroups {
    /// `Ec2InstanceAttributes$EmrManagedMasterSecurityGroup`, the group of the primary node.
    pub master: Option<String>,
    /// `$EmrManagedSlaveSecurityGroup`, the group of the core and task nodes.
    pub slave: Option<String>,
    /// `$ServiceAccessSecurityGroup`, the group the service reaches a cluster in a private
    /// subnet through.
    pub service_access: Option<String>,
}

impl ManagedSecurityGroups {
    /// The groups `cluster`'s launch handed Amazon EMR to manage. A member the launch left
    /// out, or left empty, names no group.
    pub fn of(cluster: &ClusterRecord) -> Self {
        let named = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        };
        Self {
            master: named(&cluster.instances.emr_managed_master_security_group),
            slave: named(&cluster.instances.emr_managed_slave_security_group),
            service_access: named(&cluster.instances.service_access_security_group),
        }
    }

    /// The groups the cluster's NODES run behind, deduplicated and in the order the launch
    /// states them: a launch may name one group for both roles, and such a cluster's rules
    /// reference that one group once rather than twice.
    fn node_groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = Vec::new();
        for group in [self.master.as_deref(), self.slave.as_deref()]
            .into_iter()
            .flatten()
        {
            if !groups.iter().any(|held| held == group) {
                groups.push(group.to_string());
            }
        }
        groups
    }
}

/// One grant Amazon EMR authorizes onto a group it manages: the group it is written to, the
/// protocol and port range it admits, and the groups it admits that traffic FROM.
///
/// This is the form both directions of the concern work in — the authorize writes it and the
/// shutdown revokes it — so a rule Amazon EMR added and a rule it removes are the same
/// description of the same rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGroupGrant {
    /// The security group the rule is added to.
    pub group_id: String,
    /// `IpPermission$IpProtocol`.
    pub protocol: String,
    /// `IpPermission$FromPort`, which for ICMP is a message TYPE.
    pub from_port: i32,
    /// `IpPermission$ToPort`, which for ICMP is a message CODE.
    pub to_port: i32,
    /// `IpPermission$UserIdGroupPairs`: the groups this rule admits traffic from.
    pub source_group_ids: Vec<String>,
}

/// The ingress Amazon EMR authorizes onto `groups` for one cluster, in the order it is
/// written.
///
/// The catalogue is the Management Guide's for the groups Amazon EMR manages (this module's
/// docs): each group the cluster's NODES run behind admits every ICMP message, every TCP port
/// and every UDP port from the cluster's node groups, so the primary and the core/task nodes
/// of one cluster reach each other; and where the launch named a service-access group — the
/// group "for the Amazon EMR service to access clusters in VPC private subnets" — the node
/// groups additionally admit the service's port from it and it admits the nodes' port from the
/// primary group.
///
/// A cluster whose launch named no managed node group has nothing for Amazon EMR to manage, so
/// the catalogue is EMPTY for it: nothing is written to a group this cluster was not handed.
pub fn managed_ingress(groups: &ManagedSecurityGroups) -> Vec<ManagedGroupGrant> {
    let nodes = groups.node_groups();
    if nodes.is_empty() {
        return Vec::new();
    }
    let mut grants: Vec<ManagedGroupGrant> = Vec::new();
    for group_id in &nodes {
        for (protocol, from_port, to_port) in [
            (PROTOCOL_ICMP, ALL_ICMP, ALL_ICMP),
            (PROTOCOL_TCP, LOWEST_PORT, HIGHEST_PORT),
            (PROTOCOL_UDP, LOWEST_PORT, HIGHEST_PORT),
        ] {
            grants.push(ManagedGroupGrant {
                group_id: group_id.clone(),
                protocol: protocol.to_string(),
                from_port,
                to_port,
                source_group_ids: nodes.clone(),
            });
        }
    }
    if let Some(service_access) = groups.service_access.as_deref() {
        for group_id in &nodes {
            grants.push(ManagedGroupGrant {
                group_id: group_id.clone(),
                protocol: PROTOCOL_TCP.to_string(),
                from_port: SERVICE_ACCESS_TO_NODES_PORT,
                to_port: SERVICE_ACCESS_TO_NODES_PORT,
                source_group_ids: vec![service_access.to_string()],
            });
        }
        // The one inbound rule of the service-access group itself, from the group the
        // cluster's primary node runs behind — the node the service reaches the cluster
        // through. A launch that named no primary group names no source for it.
        if let Some(master) = groups.master.as_deref() {
            grants.push(ManagedGroupGrant {
                group_id: service_access.to_string(),
                protocol: PROTOCOL_TCP.to_string(),
                from_port: NODES_TO_SERVICE_ACCESS_PORT,
                to_port: NODES_TO_SERVICE_ACCESS_PORT,
                source_group_ids: vec![master.to_string()],
            });
        }
    }
    grants
}

/// One authorization waiting to be made in Amazon EC2.
#[derive(Debug, Clone)]
pub struct ManagedGroupAction {
    /// The cluster whose groups it is about, which is what a refused write is reported
    /// against.
    pub cluster_id: String,
    /// The grants to write.
    pub grants: Vec<ManagedGroupGrant>,
}

/// Whether a cluster in `cluster_state` is one whose managed groups Amazon EMR writes its
/// rules onto.
///
/// A cluster is being PROVISIONED from the moment it is launched — its nodes come up behind
/// these groups — so the rules are owed from its first state on. A cluster that has begun
/// shutting down owes nothing: the rules it was launched with are already there, and they are
/// not taken off again (this module's docs).
fn owes_authorization(cluster_state: &str) -> bool {
    matches!(
        cluster_state,
        cluster_state::STARTING
            | cluster_state::BOOTSTRAPPING
            | cluster_state::RUNNING
            | cluster_state::WAITING
    )
}

impl ComAmazonawsEmrElasticmapreduceState {
    /// Queue the authorization every cluster whose managed groups have not had Amazon EMR's
    /// rules written onto them yet is owed, as of the states the store now holds.
    ///
    /// The comparison is against what was really DONE rather than against who moved the
    /// cluster, so a cluster the clock moved into its first state is provisioned exactly like
    /// one a `RunJobFlow` did, and nothing is authorized twice — not when the cluster moves
    /// on, and not when the caller revokes a rule of its own accord. A cluster this store no
    /// longer holds cannot move again, so its entry goes with it.
    ///
    /// Nothing is ever queued the other way: the rules sit on the CALLER's groups, so they
    /// stay there when the cluster shuts down and revoking them is the caller's own call
    /// (this module's docs).
    pub fn record_cluster_managed_group_changes(&mut self) {
        let clusters = &self.clusters;
        self.managed_groups_authorized
            .retain(|cluster_id| clusters.contains_key(cluster_id));
        let mut observed: Vec<(String, Vec<ManagedGroupGrant>)> = Vec::new();
        for cluster in self.clusters.values() {
            if !owes_authorization(&cluster.status.state)
                || self.managed_groups_authorized.contains(&cluster.id)
            {
                continue;
            }
            observed.push((
                cluster.id.clone(),
                managed_ingress(&ManagedSecurityGroups::of(cluster)),
            ));
        }
        for (cluster_id, grants) in observed {
            self.managed_groups_authorized.insert(cluster_id.clone());
            if grants.is_empty() {
                // A cluster whose launch handed Amazon EMR no group to manage: there is
                // nothing to write, so no call is queued for it.
                continue;
            }
            if self.pending_managed_group_actions.len() >= MAX_PENDING_MANAGED_GROUP_ACTIONS {
                let dropped = self.pending_managed_group_actions.len() + 1
                    - MAX_PENDING_MANAGED_GROUP_ACTIONS;
                self.pending_managed_group_actions.drain(..dropped);
                ::tracing::warn!(
                    "the queue of Amazon EMR managed security-group changes waiting to be made \
                     in Amazon EC2 is full at {MAX_PENDING_MANAGED_GROUP_ACTIONS}; the \
                     {dropped} oldest change(s) were dropped"
                );
            }
            self.pending_managed_group_actions.push(ManagedGroupAction {
                cluster_id,
                grants,
            });
        }
    }

    /// Take the changes not yet made, oldest first, leaving the queue empty.
    ///
    /// Taking them out under the lock is what stops two concurrent callers from making one
    /// change twice.
    fn take_managed_group_actions(&mut self) -> Vec<ManagedGroupAction> {
        ::std::mem::take(&mut self.pending_managed_group_actions)
    }
}

/// One grant in the shape Amazon EC2 takes it in.
fn ip_permission(grant: &ManagedGroupGrant) -> ::aws_sdk_ec2::types::IpPermission {
    let pairs: Vec<::aws_sdk_ec2::types::UserIdGroupPair> = grant
        .source_group_ids
        .iter()
        .map(|group_id| {
            ::aws_sdk_ec2::types::UserIdGroupPair::builder()
                .group_id(group_id)
                .build()
        })
        .collect();
    ::aws_sdk_ec2::types::IpPermission::builder()
        .ip_protocol(&grant.protocol)
        .from_port(grant.from_port)
        .to_port(grant.to_port)
        .set_user_id_group_pairs(Some(pairs))
        .build()
}

/// Write one grant to Amazon EC2.
///
/// A rule the group ALREADY holds is refused by Amazon EC2 for the whole call
/// (`InvalidPermission.Duplicate` names the request, not one of its sources), so a grant that
/// was refused as a whole is retried one source at a time: a group that already admits one of
/// the cluster's node groups still gains the others.
async fn authorize_grant(
    client: &::aws_sdk_ec2::Client,
    cluster_id: &str,
    grant: &ManagedGroupGrant,
) {
    let whole = client
        .authorize_security_group_ingress()
        .group_id(&grant.group_id)
        .ip_permissions(ip_permission(grant))
        .send()
        .await;
    match whole {
        Ok(_) => return,
        Err(error) => ::tracing::debug!(
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            cluster_id = %cluster_id,
            group_id = %grant.group_id,
            "Amazon EC2 did not authorize this cluster's managed ingress as one rule"
        ),
    }
    if grant.source_group_ids.len() < 2 {
        return;
    }
    for source_group_id in &grant.source_group_ids {
        let single = ManagedGroupGrant {
            source_group_ids: vec![source_group_id.clone()],
            ..grant.clone()
        };
        let answer = client
            .authorize_security_group_ingress()
            .group_id(&grant.group_id)
            .ip_permissions(ip_permission(&single))
            .send()
            .await;
        if let Err(error) = answer {
            ::tracing::debug!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                cluster_id = %cluster_id,
                group_id = %grant.group_id,
                "Amazon EC2 did not authorize this one source of a managed ingress rule"
            );
        }
    }
}

/// Make every authorization this store's clusters have made due onto the groups Amazon EMR
/// manages for them, and leave the queue empty.
///
/// Called with NO guard held: the queue is recorded and taken out in one critical section, so
/// nothing is written twice, and the Amazon EC2 calls run with the guard released — a call
/// into another service is never made while this service's store is locked.
///
/// A call Amazon EC2 refused, and one that reached no Amazon EC2 at all, is logged and
/// otherwise left alone: the cluster's own lifecycle is this service's to move and does not
/// depend on the answer.
///
/// What one drain can cost is bounded by two constants and by nothing a caller chooses: at
/// most [`MAX_PENDING_MANAGED_GROUP_ACTIONS`] actions, each carrying the fixed catalogue
/// [`managed_ingress`] builds for one cluster.
pub async fn settle_managed_security_groups<Eng: Send + Sync + 'static>(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let actions = {
        let mut store = state.lock();
        store.record_cluster_managed_group_changes();
        store.take_managed_group_actions()
    };
    if actions.is_empty() {
        return;
    }
    let client = studio_network::ec2_client_over(callback, region);
    for action in actions {
        for grant in &action.grants {
            authorize_grant(&client, &action.cluster_id, grant).await;
        }
    }
}
