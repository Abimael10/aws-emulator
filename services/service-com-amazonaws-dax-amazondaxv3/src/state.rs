// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-dax-amazondaxv3 service.
//!
//! This module owns the DAX control plane's persisted records (clusters, their nodes
//! and subnet groups), the event log those records write to, and the projections from
//! a record to the modelled wire shape, so every handler in `src/ops/` renders the SAME
//! view of one stored resource instead of building its own. It also owns the message
//! each fault answers with, so one fault reads the same whatever raises it.
//!
//! One thing a subnet group reports is NOT this service's to hold: the account's
//! network. The subnets a group names belong to Amazon EC2, so they are resolved against
//! the service that owns them ([`ec2_subnets`]) and the VPC and Availability Zone this
//! service reports for them are the ones EC2 answered with — the contract
//! `services/common/src/network.rs` states for every service that places a resource in a
//! subnet.

/// The subnets the ACCOUNT has, as Amazon EC2 reports them.
pub mod ec2_subnets;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ops::types::{
    Cluster, ClusterAlreadyExistsFault, ClusterNotFoundFault, Endpoint, Event, InvalidARNFault,
    InvalidClusterStateFault, InvalidParameterCombinationException, InvalidParameterValueException,
    Node, NodeNotFoundFault, NotificationConfiguration, Parameter, ParameterGroup,
    ParameterGroupAlreadyExistsFault, ParameterGroupNotFoundFault, ParameterGroupStatus,
    SSEDescription, SecurityGroupMembership, Subnet, SubnetGroup, SubnetGroupInUseFault,
    SubnetGroupNotFoundFault, Tag, TagNotFoundFault,
};

/// The region an ARN or an endpoint hostname carries when the request is unsigned and
/// therefore names no SigV4 credential scope.
pub const DEFAULT_REGION: &str = "us-east-1";

/// Statuses a DAX cluster reports. `Cluster$Status` is a free-form string in the model
/// (`smithy_query shape=Cluster`), and DAX words them in lower case.
pub const CLUSTER_CREATING: &str = "creating";
/// The one terminal, serving status: the only one from which a modification is accepted.
pub const CLUSTER_AVAILABLE: &str = "available";
/// Set by `UpdateCluster`/`IncreaseReplicationFactor` until the clock settles the change.
pub const CLUSTER_MODIFYING: &str = "modifying";
/// Set by `DeleteCluster` until the clock removes the cluster.
pub const CLUSTER_DELETING: &str = "deleting";

/// Statuses a node reports (`Node$NodeStatus`, "For example: `available`").
pub const NODE_CREATING: &str = "creating";
/// A node capable of serving requests — what `Cluster$ActiveNodes` counts.
pub const NODE_AVAILABLE: &str = "available";
/// A node of a cluster the clock is about to remove.
pub const NODE_DELETING: &str = "deleting";
/// A node being restarted: "During the reboot, the node status is set to REBOOTING"
/// (`RebootNode`), which DAX words in lower case like every other status.
pub const NODE_REBOOTING: &str = "rebooting";

/// `Node$ParameterGroupStatus` / `ParameterGroupStatus$ParameterApplyStatus`
/// ("For example, `in-sync`").
pub const PARAMETER_GROUP_IN_SYNC: &str = "in-sync";

/// `SecurityGroupMembership$Status` for a security group that is in effect on the nodes.
pub const SECURITY_GROUP_ACTIVE: &str = "active";

/// `NotificationConfiguration$TopicStatus` when a notification topic is attached and
/// the caller has not asked for another status.
pub const NOTIFICATION_ACTIVE: &str = "active";

/// `SSEDescription$Status` values, from the `SSEStatus` enum.
pub const SSE_ENABLED: &str = "ENABLED";
/// `SSEStatus` for a cluster created without `SSESpecification.Enabled`.
pub const SSE_DISABLED: &str = "DISABLED";

/// `ClusterEndpointEncryptionType` for an endpoint that speaks the plain `dax` protocol.
pub const ENCRYPTION_NONE: &str = "NONE";
/// `ClusterEndpointEncryptionType` for an endpoint that speaks `daxs`.
pub const ENCRYPTION_TLS: &str = "TLS";

/// The scheme and port the `Endpoint$URL` doc pins: "The default ports are 8111 for the
/// \"dax\" protocol and 9111 for the \"daxs\" protocol."
pub const DAX_SCHEME: &str = "dax";
/// Port for [`DAX_SCHEME`].
pub const DAX_PORT: i32 = 8111;
/// Scheme used when `ClusterEndpointEncryptionType` is `TLS`.
pub const DAXS_SCHEME: &str = "daxs";
/// Port for [`DAXS_SCHEME`].
pub const DAXS_PORT: i32 = 9111;

/// The network type the emulator's subnets support. Nothing in this emulator models an
/// IPv6 CIDR on a subnet, so a subnet group reports IPv4 only and a cluster created
/// without an explicit `NetworkType` derives `ipv4` from it — which is what the
/// `CreateCluster$NetworkType` doc prescribes ("If no explicit NetworkType is provided,
/// the network type is derived based on the subnet group's configuration").
pub const NETWORK_TYPE_IPV4: &str = "ipv4";

/// The maximum number of nodes a DAX cluster may hold, from the
/// `CreateCluster$ReplicationFactor` doc: "set ReplicationFactor to a number between 3
/// (one primary and two read replicas) and 10 (one primary and nine read replicas)".
pub const MAX_NODES_PER_CLUSTER: i32 = 10;

/// The `MaxResults` window both paginated DAX describes document: "The value for
/// `MaxResults` must be between 20 and 100."
pub const MIN_MAX_RESULTS: i32 = 20;
/// Upper bound of the documented `MaxResults` window, and the page size used when the
/// caller names none.
pub const MAX_MAX_RESULTS: i32 = 100;

/// The number of clusters this service's store may hold, and with it the bound on how
/// far that store grows per request: a create past it is refused with the modelled
/// `ClusterQuotaForCustomerExceededFault` ("You have attempted to exceed the maximum
/// number of DAX clusters for your Amazon Web Services account"). Together with
/// [`MAX_NODES_PER_CLUSTER`] it caps the whole cluster store at a fixed number of
/// records, so no sequence of requests grows it without limit.
pub const MAX_CLUSTERS: usize = 50;

/// The number of subnet groups the store may hold; a create past it is refused with
/// `SubnetGroupQuotaExceededFault`.
pub const MAX_SUBNET_GROUPS: usize = 50;

/// The number of parameter groups the store may hold, counting the default group DAX
/// provides. Past it a create is refused with the modelled
/// `ParameterGroupQuotaExceededFault` ("You have attempted to exceed the maximum number
/// of parameter groups"), so no sequence of requests grows the store without limit. The
/// emulator picks the bound, as it does for [`MAX_CLUSTERS`] and [`MAX_SUBNET_GROUPS`].
pub const MAX_PARAMETER_GROUPS: usize = 50;

/// The number of parameters one `UpdateParameterGroup` request may change: "You can
/// modify up to 20 parameters in a single request by submitting a list parameter name
/// and value pairs" (`UpdateParameterGroup`). A longer list is refused with the modelled
/// `InvalidParameterCombinationException`.
pub const MAX_PARAMETERS_PER_UPDATE: usize = 20;

/// The parameter group family a service-provided parameter group belongs to: the caching
/// software and the version of it whose defaults the group carries. AWS names a group it
/// provides itself after the family rather than after the service alone, so the name a
/// client reads back and passes to `CreateCluster$ParameterGroupName` is version-qualified.
pub const DEFAULT_PARAMETER_GROUP_FAMILY: &str = "dax1.0";

/// The parameter group every DAX account already has: `DescribeParameterGroups` lists it
/// before anything has been created, it is what a cluster created without an explicit
/// `ParameterGroupName` runs with, and `DescribeDefaultParameters` describes its
/// parameters. It is a RESERVED name — a create cannot take it and a delete cannot remove
/// it.
///
/// The name is `default.` followed by [`DEFAULT_PARAMETER_GROUP_FAMILY`] (a `const` cannot
/// be assembled from another `const` string, so the two are kept in step by
/// `tests/all/dax_parameter_groups.rs`).
pub const DEFAULT_PARAMETER_GROUP: &str = "default.dax1.0";

/// The description the default parameter group carries: `Default parameter group for `
/// followed by [`DEFAULT_PARAMETER_GROUP_FAMILY`], the family whose defaults it holds.
pub const DEFAULT_PARAMETER_GROUP_DESCRIPTION: &str = "Default parameter group for dax1.0";

/// The number of subnets one group may name, so one request cannot store an
/// arbitrarily long identifier list that every page then re-renders. Past it the create
/// is refused with `SubnetQuotaExceededFault` ("The request cannot be processed because
/// it would exceed the allowed number of subnets in a subnet group").
pub const MAX_SUBNETS_PER_GROUP: usize = 20;

/// The number of tags one cluster may carry; past it the create is refused with
/// `TagQuotaPerResourceExceeded` ("You have exceeded the maximum number of tags for
/// this DAX cluster").
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The longest `ClusterName` this emulator STORES. The service states no length rule — the
/// member targets the unconstrained `String` — so this is not a fidelity bound but a
/// resource one, like [`MAX_CLUSTERS`] and [`MAX_EVENTS`]: the name is replicated into the
/// cluster's ARN, its discovery endpoint and, per node, a node id and a node endpoint
/// address, so one request's name is stored many times over. The bound is far above any
/// identifier a client generates, so it refuses nothing the live service accepts.
pub const MAX_STORED_CLUSTER_NAME_LEN: usize = 256;

/// The longest tag KEY this emulator stores, the length AWS allows a tag key across its
/// services. With [`MAX_TAGS_PER_RESOURCE`] it bounds what one cluster's tag set holds.
pub const MAX_TAG_KEY_LEN: usize = 128;

/// The longest tag VALUE this emulator stores, the length AWS allows a tag value across
/// its services.
pub const MAX_TAG_VALUE_LEN: usize = 256;

/// How long after the starting call a clock-owed cluster status move becomes due
/// (`creating`/`modifying` -> `available`, `deleting` -> gone). The emulator picks this
/// span; AWS takes minutes. It is deliberately long enough for a client that POLLS the
/// resource to observe the transient status and the state errors it causes — a caller that
/// reads a moment after the call, and again a moment later, sees `creating` before it sees
/// `available` — and short enough that nothing waits on it for long.
pub const STATUS_SETTLE_DELAY: Duration = Duration::from_secs(3);

/// The Availability Zone suffixes nodes are spread over when neither the request nor the
/// cluster's subnet group names any. DAX "will spread the nodes across Availability
/// Zones for the highest availability" (`CreateCluster$AvailabilityZones`).
const AZ_SUFFIXES: [&str; 3] = ["a", "b", "c"];

/// `Event$SourceType` for an event about a cluster. The `SourceType` enum lists
/// `CLUSTER | PARAMETER_GROUP | SUBNET_GROUP`
/// (`smithy_query(query="shape",shape="SourceType")`).
pub const SOURCE_TYPE_CLUSTER: &str = "CLUSTER";
/// `Event$SourceType` for an event about a parameter group.
pub const SOURCE_TYPE_PARAMETER_GROUP: &str = "PARAMETER_GROUP";
/// `Event$SourceType` for an event about a subnet group.
pub const SOURCE_TYPE_SUBNET_GROUP: &str = "SUBNET_GROUP";

/// Every value the `SourceType` enum lists, so a `DescribeEvents` filter outside the
/// enum is refused rather than quietly selecting nothing.
pub const SOURCE_TYPES: [&str; 3] = [
    SOURCE_TYPE_CLUSTER,
    SOURCE_TYPE_PARAMETER_GROUP,
    SOURCE_TYPE_SUBNET_GROUP,
];

/// The event a `CreateCluster` records against the cluster it minted.
///
/// AWS publishes no message template for a DAX event, so the wording of each of these
/// is the emulator's own; what a client can rely on is the event's source, its type
/// and the instant it was recorded at, which all come from the stored resource.
pub const EVENT_CLUSTER_CREATED: &str = "Cluster created.";
/// The event recorded when the clock settles a cluster into `available`.
pub const EVENT_CLUSTER_AVAILABLE: &str = "Cluster is available.";
/// The event an `UpdateCluster` records.
pub const EVENT_CLUSTER_MODIFIED: &str = "Cluster modified.";
/// The event an `IncreaseReplicationFactor` records.
pub const EVENT_CLUSTER_NODES_ADDED: &str = "Added nodes to the cluster.";
/// The event a `DecreaseReplicationFactor` records.
pub const EVENT_CLUSTER_NODES_REMOVED: &str = "Removed nodes from the cluster.";
/// The event a `RebootNode` records.
pub const EVENT_NODE_REBOOTING: &str = "Rebooting cluster node.";
/// The event a `DeleteCluster` records when the deletion starts.
pub const EVENT_CLUSTER_DELETING: &str = "Deleting cluster.";
/// The event recorded when the clock completes a deletion.
pub const EVENT_CLUSTER_DELETED: &str = "Cluster deleted.";
/// The event a `CreateSubnetGroup` records against the group it minted.
pub const EVENT_SUBNET_GROUP_CREATED: &str = "Subnet group created.";
/// The event an `UpdateSubnetGroup` records against the group it changed.
pub const EVENT_SUBNET_GROUP_MODIFIED: &str = "Subnet group modified.";
/// The event a `DeleteSubnetGroup` records against the group it removed.
pub const EVENT_SUBNET_GROUP_DELETED: &str = "Subnet group deleted.";
/// The event a `CreateParameterGroup` records against the group it minted.
pub const EVENT_PARAMETER_GROUP_CREATED: &str = "Parameter group created.";
/// The event an `UpdateParameterGroup` records against the group whose parameters it
/// changed.
pub const EVENT_PARAMETER_GROUP_MODIFIED: &str = "Parameter group modified.";
/// The event a `DeleteParameterGroup` records.
pub const EVENT_PARAMETER_GROUP_DELETED: &str = "Parameter group deleted.";

/// The furthest back a `DescribeEvents` window may reach, in minutes: "you can
/// retrieve up to 14 days' worth of events if necessary" (`DescribeEvents`).
pub const MAX_EVENT_DURATION: i32 = 14 * 24 * 60;

/// [`MAX_EVENT_DURATION`] as a duration: the age at which a recorded event is no
/// longer available and is dropped from the store.
pub const EVENT_RETENTION: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// The window a `DescribeEvents` that names neither `Duration` nor `StartTime` reads:
/// "By default, only the events occurring within the last 24 hours are returned"
/// (`DescribeEvents`).
pub const DEFAULT_EVENT_DURATION: i32 = 24 * 60;

/// The number of events the store keeps (oldest dropped first), so no sequence of
/// requests grows the event log without limit.
pub const MAX_EVENTS: usize = 500;

/// A subnet of the account's network in one Region, as the service that OWNS subnets
/// reports it ([`ec2_subnets`]): the identifier a request names, the VPC it belongs to
/// and the Availability Zone it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubnetRecord {
    /// The EC2 subnet identifier, as a request names it.
    pub subnet_id: String,
    /// The VPC this subnet belongs to.
    pub vpc_id: String,
    /// The Availability Zone this subnet sits in.
    pub availability_zone: String,
}

/// A stored `Endpoint`: a DNS name, a port and the URL a DAX client dials.
#[derive(Debug, Clone)]
pub struct EndpointRecord {
    pub address: String,
    pub port: i32,
    pub url: String,
}

impl EndpointRecord {
    /// The endpoint for `address` on the protocol `encryption_type` selects.
    pub fn new(address: String, encryption_type: &str) -> Self {
        let (scheme, port) = if encryption_type == ENCRYPTION_TLS {
            (DAXS_SCHEME, DAXS_PORT)
        } else {
            (DAX_SCHEME, DAX_PORT)
        };
        // The URL carries no explicit port: the port is the scheme's documented default,
        // and `Endpoint$Port` reports it on its own.
        let url = format!("{scheme}://{address}");
        Self { address, port, url }
    }

    fn view(&self) -> Endpoint {
        Endpoint {
            address: Some(self.address.clone()),
            port: self.port,
            url: Some(self.url.clone()),
        }
    }
}

/// One node of a stored cluster.
#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub node_id: String,
    pub endpoint: EndpointRecord,
    pub create_time: SystemTime,
    pub availability_zone: String,
    pub node_status: String,
    pub parameter_group_status: Option<String>,
}

impl NodeRecord {
    fn view(&self) -> Node {
        Node {
            node_id: Some(self.node_id.clone()),
            endpoint: Some(self.endpoint.view()),
            node_create_time: Some(to_date_time(self.create_time)),
            availability_zone: Some(self.availability_zone.clone()),
            node_status: Some(self.node_status.clone()),
            parameter_group_status: self.parameter_group_status.clone(),
        }
    }
}

/// A stored DAX cluster: everything `Cluster` reports, plus the bookkeeping the
/// lifecycle needs (the region its hostnames and ARN were minted in, the AZ pool new
/// nodes land in, and the instant the clock owes it its next status move).
#[derive(Debug, Clone)]
pub struct ClusterRecord {
    pub cluster_name: String,
    pub cluster_arn: String,
    pub description: Option<String>,
    pub node_type: String,
    pub status: String,
    pub nodes: Vec<NodeRecord>,
    /// The ids of the nodes a `DecreaseReplicationFactor` gave up, which the clock owes
    /// this cluster the removal of; empty when no removal is pending.
    pub nodes_to_remove: Vec<String>,
    pub discovery_endpoint: EndpointRecord,
    pub subnet_group: Option<String>,
    pub security_group_ids: Vec<String>,
    pub iam_role_arn: String,
    pub availability_zones: Vec<String>,
    pub preferred_maintenance_window: Option<String>,
    pub notification_topic_arn: Option<String>,
    pub notification_topic_status: Option<String>,
    pub parameter_group_name: Option<String>,
    pub sse_enabled: bool,
    pub cluster_endpoint_encryption_type: String,
    pub network_type: String,
    pub tags: Vec<(String, String)>,
    pub create_time: SystemTime,
    pub region: String,
    /// When set, the instant from which [`ComAmazonawsDaxAmazondaxv3State::settle_due`]
    /// owes this cluster its next status move.
    pub settles_at: Option<SystemTime>,
}

impl ClusterRecord {
    /// The modelled `Cluster` for this record. `TotalNodes` and `ActiveNodes` are
    /// DERIVED from the stored node list — `ActiveNodes` counts the nodes that are
    /// "capable of serving requests" (`Cluster$ActiveNodes`), so a cluster still coming
    /// up reports fewer active than total.
    pub fn view(&self) -> Cluster {
        let total = i32::try_from(self.nodes.len()).unwrap_or(i32::MAX);
        let active = i32::try_from(
            self.nodes
                .iter()
                .filter(|node| node.node_status == NODE_AVAILABLE)
                .count(),
        )
        .unwrap_or(i32::MAX);
        Cluster {
            cluster_name: Some(self.cluster_name.clone()),
            description: self.description.clone(),
            cluster_arn: Some(self.cluster_arn.clone()),
            total_nodes: Some(total),
            active_nodes: Some(active),
            node_type: Some(self.node_type.clone()),
            status: Some(self.status.clone()),
            cluster_discovery_endpoint: Some(self.discovery_endpoint.view()),
            // The nodes a `DecreaseReplicationFactor` gave up, until the clock removes
            // them: "A list of nodes to be removed from the cluster"
            // (`Cluster$NodeIdsToRemove`).
            node_ids_to_remove: (!self.nodes_to_remove.is_empty())
                .then(|| self.nodes_to_remove.clone()),
            nodes: Some(self.nodes.iter().map(NodeRecord::view).collect()),
            preferred_maintenance_window: self.preferred_maintenance_window.clone(),
            notification_configuration: self.notification_topic_arn.as_ref().map(|arn| {
                NotificationConfiguration {
                    topic_arn: Some(arn.clone()),
                    topic_status: Some(
                        self.notification_topic_status
                            .clone()
                            .unwrap_or_else(|| NOTIFICATION_ACTIVE.to_string()),
                    ),
                }
            }),
            subnet_group: self.subnet_group.clone(),
            security_groups: Some(
                self.security_group_ids
                    .iter()
                    .map(|id| SecurityGroupMembership {
                        security_group_identifier: Some(id.clone()),
                        status: Some(SECURITY_GROUP_ACTIVE.to_string()),
                    })
                    .collect(),
            ),
            iam_role_arn: Some(self.iam_role_arn.clone()),
            parameter_group: self
                .parameter_group_name
                .as_ref()
                .map(|name| ParameterGroupStatus {
                    parameter_group_name: Some(name.clone()),
                    parameter_apply_status: Some(PARAMETER_GROUP_IN_SYNC.to_string()),
                    node_ids_to_reboot: Some(Vec::new()),
                }),
            sse_description: Some(SSEDescription {
                status: Some(
                    if self.sse_enabled {
                        SSE_ENABLED
                    } else {
                        SSE_DISABLED
                    }
                    .to_string(),
                ),
            }),
            cluster_endpoint_encryption_type: Some(self.cluster_endpoint_encryption_type.clone()),
            network_type: Some(self.network_type.clone()),
        }
    }

    /// The `Cluster` as `DescribeClusters` reports it. Its doc withholds node-level
    /// detail while the cluster or a node is not yet provisioned: "If the cluster is in
    /// the CREATING state, only cluster level information will be displayed until all of
    /// the nodes are successfully provisioned. If the cluster is in the DELETING state,
    /// only cluster level information will be displayed. If nodes are currently being
    /// added to the DAX cluster, node endpoint information and creation time for the
    /// additional nodes will not be displayed until they are completely provisioned."
    ///
    /// `TotalNodes`/`ActiveNodes` are cluster-level and stay reported throughout.
    pub fn describe_view(&self) -> Cluster {
        let mut view = self.view();
        if self.status == CLUSTER_CREATING || self.status == CLUSTER_DELETING {
            view.nodes = Some(Vec::new());
            return view;
        }
        if let Some(nodes) = view.nodes.as_mut() {
            for node in nodes
                .iter_mut()
                .filter(|node| node.node_status.as_deref() == Some(NODE_CREATING))
            {
                node.endpoint = None;
                node.node_create_time = None;
            }
        }
        view
    }

    /// The node this cluster holds that is NOT in a state a deletion can take it from —
    /// the first one still coming up or already going away — or `None` when every node is
    /// available. A cluster is deleted by deleting its nodes, so this is what a delete of
    /// a cluster whose nodes are not settled is refused for.
    pub fn node_blocking_deletion(&self) -> Option<&NodeRecord> {
        self.nodes
            .iter()
            .find(|node| node.node_status != NODE_AVAILABLE)
    }

    /// The AZ a node at `index` of this cluster lands in: the cluster's AZ pool, cycled.
    pub fn availability_zone_for(&self, index: usize) -> String {
        if self.availability_zones.is_empty() {
            default_availability_zone(&self.region, index)
        } else {
            self.availability_zones[index % self.availability_zones.len()].clone()
        }
    }

    /// The modelled `Tag` list this cluster carries, in the order the tags were stored.
    pub fn tag_views(&self) -> Vec<Tag> {
        self.tags
            .iter()
            .map(|(key, value)| Tag {
                key: Some(key.clone()),
                value: Some(value.clone()),
            })
            .collect()
    }

    /// Store `value` under `key`: "Every DAX cluster can only have one tag with the same
    /// key. If you try to add an existing tag (same key), the existing tag value will be
    /// updated to the new value" (`Tag$Key`).
    pub fn set_tag(&mut self, key: String, value: String) {
        match self.tags.iter_mut().find(|(stored, _)| *stored == key) {
            Some(existing) => existing.1 = value,
            None => self.tags.push((key, value)),
        }
    }

    /// Remove the tag stored under `key`, answering whether this cluster carried one.
    pub fn remove_tag(&mut self, key: &str) -> bool {
        let before = self.tags.len();
        self.tags.retain(|(stored, _)| stored != key);
        self.tags.len() != before
    }

    /// Mint one more node for this cluster and append it.
    ///
    /// A node is identified by the cluster's name and a one-letter suffix — DAX names a
    /// cluster's nodes `<ClusterName>-a`, `<ClusterName>-b`, and so on — so the id is
    /// derived from the cluster, and the lowest suffix no node of this cluster already
    /// carries is the one minted.
    pub fn push_node(&mut self, now: SystemTime) {
        let index = self.nodes.len();
        let mut candidate_index = 0usize;
        let node_id = loop {
            let candidate = format!("{}-{}", self.cluster_name, node_id_suffix(candidate_index));
            if !self.nodes.iter().any(|node| node.node_id == candidate) {
                break candidate;
            }
            candidate_index += 1;
        };
        let address = node_endpoint_address(&node_id, &self.cluster_name, &self.region);
        self.nodes.push(NodeRecord {
            node_id,
            endpoint: EndpointRecord::new(address, &self.cluster_endpoint_encryption_type),
            create_time: now,
            availability_zone: self.availability_zone_for(index),
            node_status: NODE_CREATING.to_string(),
            parameter_group_status: self
                .parameter_group_name
                .as_ref()
                .map(|_| PARAMETER_GROUP_IN_SYNC.to_string()),
        });
    }
}

/// A stored DAX parameter group: everything the modelled `ParameterGroup` reports, plus
/// whether DAX provides it (a default group, which cannot be deleted) and the value this
/// group carries for each parameter of the caching software.
#[derive(Debug, Clone)]
pub struct ParameterGroupRecord {
    pub name: String,
    pub description: Option<String>,
    /// True for a group AWS provides rather than one a caller created.
    pub default_group: bool,
    /// This group's value for every parameter of the caching software, keyed by
    /// `Parameter$ParameterName`. A group is created holding the system-defined value of
    /// each parameter in [`DEFAULT_PARAMETERS`] (see [`default_parameter_values`]), so a
    /// read of a group reports the group's OWN stored values rather than the catalog's;
    /// a `BTreeMap` so those values page in one stable order.
    pub parameter_values: BTreeMap<String, String>,
}

impl ParameterGroupRecord {
    /// A group a caller created: it starts out carrying the system-defined value of
    /// every parameter the caching software defines, which is what a group that has
    /// never been updated reports.
    pub fn new(name: String, description: Option<String>) -> Self {
        Self {
            name,
            description,
            default_group: false,
            parameter_values: default_parameter_values(),
        }
    }

    /// The default group DAX provides in every account.
    pub fn default_group() -> Self {
        Self {
            default_group: true,
            ..Self::new(
                DEFAULT_PARAMETER_GROUP.to_string(),
                Some(DEFAULT_PARAMETER_GROUP_DESCRIPTION.to_string()),
            )
        }
    }

    /// The modelled `ParameterGroup` for this record.
    pub fn view(&self) -> ParameterGroup {
        ParameterGroup {
            parameter_group_name: Some(self.name.clone()),
            description: self.description.clone(),
        }
    }

    /// The names of the parameters this group holds a value for, in the one stable order
    /// they page in (by name).
    pub fn parameter_names(&self) -> Vec<&str> {
        self.parameter_values.keys().map(String::as_str).collect()
    }

    /// The modelled `Parameter` this group reports for `name`: the catalog entry's
    /// definition — its description, `Source`, data type, allowed values, modifiability
    /// and change type — carrying the value THIS group stores for it. `None` when the
    /// group holds no such parameter.
    pub fn parameter_view(&self, name: &str) -> Option<Parameter> {
        let definition = default_parameter(name)?;
        let value = self
            .parameter_values
            .get(name)
            .map(String::as_str)
            .unwrap_or(definition.default_value);
        Some(definition.view(value, definition.source))
    }
}

/// A stored DAX subnet group.
#[derive(Debug, Clone)]
pub struct SubnetGroupRecord {
    pub name: String,
    pub description: Option<String>,
    pub vpc_id: String,
    /// The subnets this group names, as [`ec2_subnets::resolve_requested_subnets`]
    /// resolved them when the group was created, in the order the request named them.
    pub subnets: Vec<SubnetRecord>,
    /// The Region whose network the group's subnets were resolved in.
    pub region: String,
}

impl SubnetGroupRecord {
    /// The modelled `SubnetGroup` for this record. Each `Subnet` reports the zone the
    /// network model holds for it, and its supported network types are IPv4 (see
    /// [`NETWORK_TYPE_IPV4`]).
    pub fn view(&self) -> SubnetGroup {
        SubnetGroup {
            subnet_group_name: Some(self.name.clone()),
            description: self.description.clone(),
            vpc_id: Some(self.vpc_id.clone()),
            subnets: Some(
                self.subnets
                    .iter()
                    .map(|subnet| Subnet {
                        subnet_identifier: Some(subnet.subnet_id.clone()),
                        subnet_availability_zone: Some(subnet.availability_zone.clone()),
                        supported_network_types: Some(vec![NETWORK_TYPE_IPV4.to_string()]),
                    })
                    .collect(),
            ),
            supported_network_types: Some(vec![NETWORK_TYPE_IPV4.to_string()]),
        }
    }

    /// The distinct Availability Zones this group's subnets sit in, sorted — the pool a
    /// cluster placed in this group spreads its nodes over.
    pub fn availability_zones(&self) -> Vec<String> {
        let mut zones: Vec<String> = self
            .subnets
            .iter()
            .map(|subnet| subnet.availability_zone.clone())
            .collect();
        zones.sort_unstable();
        zones.dedup();
        zones
    }
}

// ---------------------------------------------------------------------------
// The system-defined parameter catalog.
// ---------------------------------------------------------------------------

/// `Parameter$Source` for a parameter the caching software's own parameter group defines:
/// "How the parameter is defined. For example, `system` denotes a system-defined
/// parameter." DAX publishes its two cluster parameters as `user`, which is the value each
/// catalog entry below carries and what every read of the catalog reports.
pub const PARAMETER_SOURCE_USER: &str = "user";

/// `Parameter$Source` for a parameter defined by the service itself rather than by a
/// parameter group — the other value the member's documentation names.
pub const PARAMETER_SOURCE_SYSTEM: &str = "system";

/// The `Source` values this service reports, so a reported `Source` can be checked
/// against the vocabulary the member documents.
pub const PARAMETER_SOURCES: [&str; 2] = [PARAMETER_SOURCE_SYSTEM, PARAMETER_SOURCE_USER];

/// `Parameter$ParameterType` (`ParameterType` enum) for a parameter that applies to
/// "any nodes" rather than only nodes of a particular type.
pub const PARAMETER_TYPE_DEFAULT: &str = "DEFAULT";

/// `Parameter$ParameterType` for a parameter that carries per-node-type values.
pub const PARAMETER_TYPE_NODE_TYPE_SPECIFIC: &str = "NODE_TYPE_SPECIFIC";

/// `Parameter$IsModifiable` (`IsModifiable` enum) for a parameter "the customer is
/// allowed to modify".
pub const IS_MODIFIABLE_TRUE: &str = "TRUE";

/// `Parameter$ChangeType` (`ChangeType` enum) for a parameter whose new value takes
/// effect without rebooting a node.
pub const CHANGE_TYPE_IMMEDIATE: &str = "IMMEDIATE";

/// `Parameter$DataType` of a parameter whose value is a whole number ("The data type
/// of the parameter. For example, `integer`").
pub const DATA_TYPE_INTEGER: &str = "integer";

/// One system-defined DAX parameter: the definition behind every `Parameter` this
/// service reports, independent of which group is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterDefinition {
    /// `Parameter$ParameterName`.
    pub name: &'static str,
    /// The value the system defines for it — what a group that has never been updated
    /// reports.
    pub default_value: &'static str,
    /// `Parameter$Description`, as the catalog publishes it.
    pub description: &'static str,
    /// `Parameter$Source`: how this parameter is defined, one of [`PARAMETER_SOURCES`].
    /// It is a property of the catalog entry, so every read of this parameter reports the
    /// same `Source` rather than one chosen where the parameter is projected.
    pub source: &'static str,
    /// `Parameter$DataType`.
    pub data_type: &'static str,
    /// `Parameter$AllowedValues`: "A range of values within which the parameter can be
    /// set", in the AWS range notation (`<min>-` is "`min` and above").
    pub allowed_values: &'static str,
    /// `Parameter$IsModifiable`, one of [`crate::ops::types::IS_MODIFIABLE_VALUES`].
    pub is_modifiable: &'static str,
    /// `Parameter$ChangeType`, one of [`crate::ops::types::CHANGE_TYPE_VALUES`].
    pub change_type: &'static str,
}

impl ParameterDefinition {
    /// The modelled `Parameter` for this definition carrying `value`, reported as
    /// coming from `source`. `ParameterType` is DERIVED: this emulator models no
    /// per-node-type override, so every parameter "can be applied to any nodes" and
    /// reports [`PARAMETER_TYPE_DEFAULT`] with an empty `NodeTypeSpecificValues`.
    pub fn view(&self, value: &str, source: &str) -> Parameter {
        Parameter {
            parameter_name: Some(self.name.to_string()),
            // Nothing in this emulator overrides a parameter for one node type, so
            // every parameter applies to any node: `DEFAULT`, with an empty
            // `NodeTypeSpecificValues`. One that carried such values would report
            // [`PARAMETER_TYPE_NODE_TYPE_SPECIFIC`] instead.
            parameter_type: Some(PARAMETER_TYPE_DEFAULT.to_string()),
            parameter_value: Some(value.to_string()),
            node_type_specific_values: Some(Vec::new()),
            description: Some(self.description.to_string()),
            source: Some(source.to_string()),
            data_type: Some(self.data_type.to_string()),
            allowed_values: Some(self.allowed_values.to_string()),
            is_modifiable: Some(self.is_modifiable.to_string()),
            change_type: Some(self.change_type.to_string()),
        }
    }

    /// This parameter as the catalog defines it: its default value, carrying the entry's
    /// own [`ParameterDefinition::source`]. This is the view `DescribeDefaultParameters`
    /// reports, and the one a group that has never been updated carries.
    pub fn default_view(&self) -> Parameter {
        self.view(self.default_value, self.source)
    }

    /// The values this parameter accepts, as `AllowedValues` publishes them in the AWS
    /// range notation: the inclusive minimum, and the inclusive maximum when the entry
    /// names one (`<min>-` is "`min` and above"). `None` for an entry whose
    /// `AllowedValues` is not a range, which bounds nothing.
    pub fn allowed_range(&self) -> Option<(i64, Option<i64>)> {
        // A bound may be negative, so the separator is the one AFTER the minimum's own
        // sign rather than the first hyphen in the notation.
        let (sign, rest) = match self.allowed_values.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", self.allowed_values),
        };
        let (min, max) = rest.split_once('-')?;
        let min = format!("{sign}{min}").parse::<i64>().ok()?;
        let max = if max.is_empty() {
            None
        } else {
            Some(max.parse::<i64>().ok()?)
        };
        Some((min, max))
    }

    /// Why `value` is not one this parameter may be set to, worded for the caller, or
    /// `None` when it is acceptable.
    ///
    /// The check is driven by what the CATALOG publishes for the entry rather than by
    /// its name: a parameter whose `DataType` is a whole number takes one, and
    /// `AllowedValues` ([`allowed_range`](Self::allowed_range)) bounds it. An entry the
    /// customer may not modify accepts no value at all.
    pub fn value_violation(&self, value: &str) -> Option<String> {
        if self.is_modifiable != IS_MODIFIABLE_TRUE {
            return Some(format!("Parameter {} cannot be modified", self.name));
        }
        if self.data_type != DATA_TYPE_INTEGER {
            return None;
        }
        let Ok(parsed) = value.parse::<i64>() else {
            return Some(format!(
                "Invalid value for parameter {}: {value} is not an {}",
                self.name, self.data_type
            ));
        };
        // An entry whose `AllowedValues` is not a range bounds nothing, so the value is
        // checked against its data type alone rather than reported as acceptable
        // untested.
        if let Some((min, max)) = self.allowed_range() {
            if parsed < min || max.is_some_and(|max| parsed > max) {
                return Some(format!(
                    "Invalid value for parameter {}: {value} is outside the allowed values {}",
                    self.name, self.allowed_values
                ));
            }
        }
        None
    }
}

/// The parameters the DAX caching software defines, sorted by name (so [`page`] can
/// page them) and unique — the catalog `DescribeDefaultParameters` reports and the
/// values any parameter group starts from.
///
/// Source: the Amazon DAX Developer Guide's parameter-group / TTL settings, which
/// document exactly two DAX cluster parameters — `query-ttl-millis`, the time to live
/// for the query cache, and `record-ttl-millis`, the time to live for the item cache —
/// each a duration in milliseconds, each defaulting to 300000 (five minutes), each
/// modifiable by the customer and applied to the cluster without a node reboot; the
/// description and `Source` of each are the ones DAX publishes for that entry.
pub const DEFAULT_PARAMETERS: &[ParameterDefinition] = &[
    ParameterDefinition {
        name: "query-ttl-millis",
        default_value: "300000",
        description: "Duration in milliseconds for queries to remain cached",
        source: PARAMETER_SOURCE_USER,
        data_type: DATA_TYPE_INTEGER,
        allowed_values: "0-",
        is_modifiable: IS_MODIFIABLE_TRUE,
        change_type: CHANGE_TYPE_IMMEDIATE,
    },
    ParameterDefinition {
        name: "record-ttl-millis",
        default_value: "300000",
        description: "Duration in milliseconds for records to remain valid in cache \
                      (Default: 0 = infinite)",
        source: PARAMETER_SOURCE_USER,
        data_type: DATA_TYPE_INTEGER,
        allowed_values: "0-",
        is_modifiable: IS_MODIFIABLE_TRUE,
        change_type: CHANGE_TYPE_IMMEDIATE,
    },
];

/// The names of [`DEFAULT_PARAMETERS`], in the order they page in.
pub fn default_parameter_names() -> Vec<&'static str> {
    DEFAULT_PARAMETERS
        .iter()
        .map(|parameter| parameter.name)
        .collect()
}

/// The system definition of the parameter called `name`, or `None` when DAX defines no
/// such parameter.
pub fn default_parameter(name: &str) -> Option<&'static ParameterDefinition> {
    DEFAULT_PARAMETERS
        .iter()
        .find(|parameter| parameter.name == name)
}

/// The values a newly created parameter group carries: the system-defined value of every
/// parameter in [`DEFAULT_PARAMETERS`], keyed by name.
pub fn default_parameter_values() -> BTreeMap<String, String> {
    DEFAULT_PARAMETERS
        .iter()
        .map(|parameter| {
            (
                parameter.name.to_string(),
                parameter.default_value.to_string(),
            )
        })
        .collect()
}

/// One recorded DAX event: what happened to which resource, and when.
#[derive(Debug, Clone)]
pub struct EventRecord {
    /// Monotonic sequence number, which a `DescribeEvents` page token names.
    pub seq: u64,
    /// The resource the event is about (`Event$SourceName`).
    pub source_name: String,
    /// One of [`SOURCE_TYPES`] (`Event$SourceType`).
    pub source_type: String,
    /// What happened (`Event$Message`).
    pub message: String,
    /// When it happened (`Event$Date`).
    pub date: SystemTime,
}

impl EventRecord {
    /// The modelled `Event` for this record.
    pub fn view(&self) -> Event {
        Event {
            source_name: Some(self.source_name.clone()),
            source_type: Some(self.source_type.clone()),
            message: Some(self.message.clone()),
            date: Some(to_date_time(self.date)),
        }
    }
}

/// Per-service state for com-amazonaws-dax-amazondaxv3. A handler reaches it through [`service_state`].
#[derive(Debug)]
pub struct ComAmazonawsDaxAmazondaxv3State {
    /// Clusters by their (lower-cased) `ClusterName`. A `BTreeMap` so every describe
    /// pages in one stable order.
    pub clusters: BTreeMap<String, ClusterRecord>,
    /// Subnet groups by `SubnetGroupName`.
    pub subnet_groups: BTreeMap<String, SubnetGroupRecord>,
    /// Parameter groups by `ParameterGroupName`, seeded with the default group DAX
    /// provides (see [`DEFAULT_PARAMETER_GROUP`]). A `BTreeMap` so every describe pages
    /// in one stable order.
    pub parameter_groups: BTreeMap<String, ParameterGroupRecord>,
    /// Events recorded for this service's resources, oldest first.
    pub events: Vec<EventRecord>,
    /// Monotonic event counter, so one page token names exactly one event.
    pub next_event_seq: u64,
}

/// A fresh store is an account DAX has just become available in: no clusters and no
/// subnet groups, and the one parameter group AWS provides itself. Seeding it HERE rather
/// than from a handler is what keeps `DescribeParameterGroups` a pure read.
impl Default for ComAmazonawsDaxAmazondaxv3State {
    fn default() -> Self {
        let default_group = ParameterGroupRecord::default_group();
        Self {
            clusters: BTreeMap::new(),
            subnet_groups: BTreeMap::new(),
            parameter_groups: BTreeMap::from([(default_group.name.clone(), default_group)]),
            events: Vec::new(),
            next_event_seq: 0,
        }
    }
}

impl ComAmazonawsDaxAmazondaxv3State {
    /// The (lower-cased) name of the cluster a tagging operation's `ResourceName`
    /// addresses, or why it addresses none.
    ///
    /// A `ResourceName` is the ARN of a DAX resource, so the value is READ as one, in two
    /// steps: a value that does not carry [`ARN_COMPONENTS`] components is not an ARN at
    /// all, and one that does but does not name a cluster of THIS service is not one of
    /// this account's, both refused with `InvalidARNFault`. An ARN that names a cluster
    /// this account does not hold is refused with `ClusterNotFoundFault`.
    /// `CreateCluster$ClusterName` "is stored as a lowercase string", so the ARN's id is
    /// resolved lower-cased.
    pub fn tagged_cluster_name(&self, resource_name: &str) -> Result<String, ResourceNameRefusal> {
        if resource_name.split([':', '/']).count() != ARN_COMPONENTS {
            return Err(ResourceNameRefusal::InvalidArn(arn_component_count()));
        }
        let addressed = ::service_common::arn::parse(resource_name)
            .filter(|arn| arn.vendor == DAX_VENDOR)
            .and_then(|_| {
                ::service_common::arn::resource_id_of_type(resource_name, CLUSTER_RESOURCE_TYPE)
            })
            .filter(|id| !id.is_empty());
        let Some(id) = addressed else {
            return Err(ResourceNameRefusal::InvalidArn(invalid_resource_arn()));
        };
        let name = id.to_lowercase();
        if !self.clusters.contains_key(&name) {
            return Err(ResourceNameRefusal::ClusterNotFound(cluster_not_found()));
        }
        Ok(name)
    }

    /// Record one event against `source_name` (a [`SOURCE_TYPES`] resource) at `at`.
    ///
    /// The log is BOUNDED: past [`MAX_EVENTS`] the oldest events are dropped, so no
    /// sequence of requests grows it without limit.
    pub fn record_event(
        &mut self,
        source_name: &str,
        source_type: &str,
        message: &str,
        at: SystemTime,
    ) {
        self.next_event_seq += 1;
        self.events.push(EventRecord {
            seq: self.next_event_seq,
            source_name: source_name.to_string(),
            source_type: source_type.to_string(),
            message: message.to_string(),
            date: at,
        });
        if self.events.len() > MAX_EVENTS {
            let overflow = self.events.len() - MAX_EVENTS;
            self.events.drain(..overflow);
        }
    }

    /// Drop every event older than [`EVENT_RETENTION`] as of `now`.
    ///
    /// This is a move the CLOCK already made due — an event 14 days old is no longer
    /// available whether or not anybody called — so a reporting operation may run it
    /// before it reads.
    pub fn expire_events(&mut self, now: SystemTime) {
        let Some(cutoff) = now.checked_sub(EVENT_RETENTION) else {
            return;
        };
        self.events.retain(|event| event.date >= cutoff);
    }

    /// Apply every cluster status move the clock has already made due as of `now`:
    /// `creating`/`modifying` becomes `available` (with its nodes), and a `deleting`
    /// cluster is removed. Nothing here depends on a call having been made — each move
    /// was owed at the same instant even if nobody had asked — so a reporting operation
    /// may run it before it reads.
    pub fn settle_due(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .clusters
            .iter()
            .filter(|(_, cluster)| cluster.settles_at.is_some_and(|at| at <= now))
            .map(|(name, _)| name.clone())
            .collect();
        for name in due {
            let deleting = self
                .clusters
                .get(&name)
                .is_some_and(|cluster| cluster.status == CLUSTER_DELETING);
            if deleting {
                self.clusters.remove(&name);
                self.record_event(&name, SOURCE_TYPE_CLUSTER, EVENT_CLUSTER_DELETED, now);
                continue;
            }
            let mut settled = false;
            if let Some(cluster) = self.clusters.get_mut(&name) {
                // A removal the clock owed is materialised first: the nodes a decrease
                // gave up are gone, and what stays is available.
                if !cluster.nodes_to_remove.is_empty() {
                    let removing = std::mem::take(&mut cluster.nodes_to_remove);
                    cluster
                        .nodes
                        .retain(|node| !removing.contains(&node.node_id));
                }
                cluster.status = CLUSTER_AVAILABLE.to_string();
                for node in &mut cluster.nodes {
                    node.node_status = NODE_AVAILABLE.to_string();
                }
                cluster.settles_at = None;
                settled = true;
            }
            if settled {
                self.record_event(&name, SOURCE_TYPE_CLUSTER, EVENT_CLUSTER_AVAILABLE, now);
            }
        }
    }
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsDaxAmazondaxv3State>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsDaxAmazondaxv3State>)
}

/// [`service_state`] or the 501 this service answers when it is mounted on an engine
/// that holds no store at all.
///
/// Unreachable in this emulator — the router builds one `engine::InMemoryEngine` and
/// hands it to every operation — but a DAX model declares no server-side fault, so a
/// deployment with no store has genuinely implemented nothing to answer with.
pub fn store<Eng: Send + Sync + 'static, E>(
    engine: &Eng,
    operation: &str,
) -> Result<::engine::ServiceState<ComAmazonawsDaxAmazondaxv3State>, crate::scaffolded::Scaffolded<E>>
{
    service_state(engine).ok_or_else(|| crate::scaffolded::Scaffolded::not_implemented(operation))
}

/// The region this request was signed for, or [`DEFAULT_REGION`] when it carries no
/// SigV4 credential scope. Every ARN and endpoint hostname a cluster stores is minted
/// in it.
pub fn request_region(ctx: &smithy_server_runtime::OperationContext<'_>) -> String {
    ::emulator_server::sigv4_credential_scope(&ctx.parts().headers)
        .map(|scope| scope.region)
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// The ARN of a DAX cluster: `arn:aws:dax:<region>:<account>:cache/<ClusterName>`.
pub fn cluster_arn(region: &str, cluster_name: &str) -> String {
    ::engine::emulator_arn(DAX_VENDOR, region, &format!("{CLUSTER_RESOURCE_TYPE}/{cluster_name}"))
}

/// The service namespace a DAX resource's ARN names, and the vendor a `ResourceName` a
/// tagging operation carries has to be one of.
pub const DAX_VENDOR: &str = "dax";

/// The resource type a DAX cluster's ARN addresses it by, so one `ResourceName` resolves
/// to the cluster this store holds under that id.
pub const CLUSTER_RESOURCE_TYPE: &str = "cache";

/// Why a `ResourceName` a tagging operation carries names no cluster of this account.
///
/// The tagging operations (`TagResource`, `UntagResource`, `ListTags`) address a resource
/// by ARN rather than by name, and each models both refusals, so the reading is shared and
/// each handler lifts the refusal into its own error channel.
#[derive(Debug)]
pub enum ResourceNameRefusal {
    /// The value is not a well-formed ARN of a DAX cluster.
    InvalidArn(InvalidARNFault),
    /// It is one, but this account holds no cluster it addresses.
    ClusterNotFound(ClusterNotFoundFault),
}

/// How many components DAX reads a resource ARN as carrying: the literal `arn`, the
/// partition, the service namespace, the Region, the account, the resource type and the
/// resource id — the six colon-separated ARN fields with the resource field split into its
/// type and its id, so `arn:aws:dax:<region>:<account>:cache/<ClusterName>` has seven.
pub const ARN_COMPONENTS: usize = 7;

/// `InvalidARNFault` for a `ResourceName` whose component count is not
/// [`ARN_COMPONENTS`]: the value cannot be read as a resource ARN at all, which is the
/// refusal DAX words from this fixed template.
pub fn arn_component_count() -> InvalidARNFault {
    InvalidARNFault {
        message: Some(format!(
            "Input ARN string does not have {ARN_COMPONENTS} components."
        )),
    }
}

/// `InvalidARNFault` for a value that carries [`ARN_COMPONENTS`] components but is not an
/// ARN of THIS service naming a cluster — another partition's syntax, another vendor's
/// resource, or another resource type of this one. The modelled sentence for the fault
/// ("The Amazon Resource Name (ARN) supplied in the request is not valid"), as a fixed
/// template naming no identifier: the request already carries the only value it could mean.
pub fn invalid_resource_arn() -> InvalidARNFault {
    InvalidARNFault {
        message: Some(
            "The Amazon Resource Name (ARN) supplied in the request is not valid.".to_string(),
        ),
    }
}

/// `TagNotFoundFault` for an untag naming keys the resource does not carry ("The tag does
/// not exist"), listing every key that is not present — the one part of the template taken
/// from the request.
pub fn tag_not_found(missing: &[String]) -> TagNotFoundFault {
    TagNotFoundFault {
        message: Some(format!(
            "Failing request. These tags are not present : {}",
            missing.join(", ")
        )),
    }
}

/// `NodeNotFoundFault` for a node id no node of the addressed cluster carries ("None of
/// the nodes in the cluster have the given node ID"). DAX words this from a FIXED template
/// that names no identifier: the request already carries the id it could mean.
pub fn node_not_found() -> NodeNotFoundFault {
    NodeNotFoundFault {
        message: Some("Node not found.".to_string()),
    }
}

/// `SubnetGroupInUseFault` for a subnet group a stored cluster is placed in ("The
/// specified subnet group is currently in use"), naming the group the request addressed.
pub fn subnet_group_in_use(name: &str) -> SubnetGroupInUseFault {
    SubnetGroupInUseFault {
        message: Some(format!(
            "Subnet group {name} is currently in use by a cluster."
        )),
    }
}

/// The DNS name of a cluster's discovery endpoint.
pub fn discovery_endpoint_address(cluster_name: &str, region: &str) -> String {
    format!("{cluster_name}.dax-clusters.{region}.amazonaws.com")
}

/// The DNS name of one node's endpoint.
pub fn node_endpoint_address(node_id: &str, cluster_name: &str, region: &str) -> String {
    format!("{node_id}.{cluster_name}.dax-clusters.{region}.amazonaws.com")
}

/// The suffix the node at `index` of a cluster is identified by: a single letter, since
/// DAX names a cluster's nodes `<ClusterName>-a`, `<ClusterName>-b`, and so on. Past the
/// letters the index itself continues the sequence, so an id stays unique whatever the
/// node count — a cluster holds at most [`MAX_NODES_PER_CLUSTER`] nodes, so that is
/// unreachable today.
fn node_id_suffix(index: usize) -> String {
    match u8::try_from(index) {
        Ok(offset) if index < 26 => char::from(b'a' + offset).to_string(),
        _ => index.to_string(),
    }
}

/// The AZ a node at `index` lands in when nothing named a pool: DAX spreads nodes over
/// the region's zones.
pub fn default_availability_zone(region: &str, index: usize) -> String {
    format!("{region}{}", AZ_SUFFIXES[index % AZ_SUFFIXES.len()])
}

/// A `ClusterName` the `CreateCluster$ClusterName` doc accepts: it carries only
/// alphanumeric characters or hyphens, "The first character must be a letter. A name
/// cannot end with a hyphen or contain two consecutive hyphens."
///
/// The member's SHAPE is the rule the service states: `ClusterName` targets the
/// unconstrained `String`, so the model states no `@length`
/// (`smithy_query(query="operation",operation="CreateCluster")` and
/// `smithy_query(query="shape",shape="String")`), and the service accepts names far longer
/// than the member's documentation prose ("A name must contain from 1 to 20 alphanumeric
/// characters or hyphens") claims. A name is therefore read for its shape, and its length
/// only against [`MAX_STORED_CLUSTER_NAME_LEN`], the bound this emulator picks on what one
/// request may make it store. The empty name the model's `@required` admits begins with no
/// letter, so the first-character clause is what refuses it.
///
/// Returns the reason it is refused, worded for the caller, or `None` when it is valid.
pub fn cluster_name_violation(name: &str) -> Option<String> {
    // The bound on STORED bytes first, so no longer value is scanned or copied.
    if name.len() > MAX_STORED_CLUSTER_NAME_LEN {
        return Some(format!(
            "ClusterName must be at most {MAX_STORED_CLUSTER_NAME_LEN} characters"
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(
            "ClusterName must contain only alphanumeric characters or hyphens".to_string(),
        );
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Some("ClusterName must begin with a letter".to_string());
    }
    if name.ends_with('-') {
        return Some("ClusterName cannot end with a hyphen".to_string());
    }
    if name.contains("--") {
        return Some("ClusterName cannot contain two consecutive hyphens".to_string());
    }
    None
}

/// Why the tag `(key, value)` cannot be stored, or `None` when it can.
///
/// A tag is stored UNDER its key, so a tag without one addresses nothing. Both halves are
/// bounded by what this emulator stores per tag ([`MAX_TAG_KEY_LEN`],
/// [`MAX_TAG_VALUE_LEN`]) which, with [`MAX_TAGS_PER_RESOURCE`], bounds one cluster's
/// whole tag set. Every path that STORES tags — `CreateCluster$Tags` and `TagResource` —
/// reads them this way, so one request cannot leave a tag the other path would refuse.
pub fn tag_violation(key: &str, value: &str) -> Option<String> {
    if key.is_empty() {
        return Some("Tag key must not be empty.".to_string());
    }
    if key.len() > MAX_TAG_KEY_LEN {
        return Some(format!(
            "Tag key must be at most {MAX_TAG_KEY_LEN} characters"
        ));
    }
    if value.len() > MAX_TAG_VALUE_LEN {
        return Some(format!(
            "Tag value must be at most {MAX_TAG_VALUE_LEN} characters"
        ));
    }
    None
}

/// The message DAX answers when `MaxResults` falls outside the documented window. DAX
/// words this rejection from a FIXED template that echoes nothing of the request, so it
/// is built from the documented bounds alone rather than from the value received.
pub fn max_results_out_of_range_message() -> String {
    format!(
        "Invalid value for max records. Must be between {MIN_MAX_RESULTS} and {MAX_MAX_RESULTS}"
    )
}

/// The message `ParameterGroupNotFoundFault` carries on the paths that name parameter
/// groups as a FILTER over the groups they report (`DescribeParameterGroups`'s
/// `ParameterGroupNames`) and that REMOVE one (`DeleteParameterGroup`): one fixed
/// template, naming no identifier. DAX words this fault per PATH, and each wording in
/// this module is the one that path's own recorded answer carries.
pub fn parameter_group_not_found_message() -> String {
    "Parameter Group not found.".to_string()
}

/// The message `ParameterGroupNotFoundFault` carries when the parameter group whose
/// PARAMETERS a read was asked for (`DescribeParameters`) is not one this store holds:
/// the fixed text `CacheParameterGroupnot found: ` (DAX's own wording, with no space
/// between `CacheParameterGroup` and `not`, naming the parameter-group member of the
/// parameter-group API this service's control plane shares) followed by the
/// `ParameterGroupName` the request asked for, verbatim, so the caller is told WHICH
/// group could not be resolved.
pub fn parameter_group_not_found_for_parameters_message(name: &str) -> String {
    format!("CacheParameterGroupnot found: {name}")
}

/// The message a request that CREATES or CHANGES a parameter group is refused with when
/// the name it addresses the group by is not a valid parameter-group identifier.
///
/// The name is read as an IDENTIFIER: it "must begin with a letter; must contain only ASCII
/// letters, digits, and hyphens; and must not end with a hyphen or contain two consecutive
/// hyphens" ([`is_parameter_group_identifier`]), and the member the validation reports is
/// the parameter-group name member of the parameter-group API this service's control plane
/// shares (`CacheParameterGroupName`). The text is ENTIRELY fixed: it states the rule that
/// was broken and interpolates nothing from the request, so a caller is told WHY the name
/// was rejected rather than being handed its own input back.
pub const PARAMETER_GROUP_NAME_NOT_AN_IDENTIFIER_MESSAGE: &str =
    "The parameter CacheParameterGroupName is not a valid identifier. Identifiers must \
     begin with a letter; must contain only ASCII letters, digits, and hyphens; and must \
     not end with a hyphen or contain two consecutive hyphens.";

/// Whether `name` is a valid parameter-group identifier, which is the rule
/// [`PARAMETER_GROUP_NAME_NOT_AN_IDENTIFIER_MESSAGE`] states: it must begin with a letter,
/// carry only ASCII letters, digits and hyphens, not end with a hyphen and not contain two
/// consecutive hyphens. The empty name the model's `@required` admits begins with no
/// letter, so it is not one either.
///
/// A request that CREATES a parameter group or CHANGES one is read this way BEFORE the
/// store is consulted, so such a name is refused on the rule it breaks rather than on what
/// the store happens to hold under it. A request that READS a group is not: the group this
/// service provides is named `default.` followed by its family
/// ([`DEFAULT_PARAMETER_GROUP`]), so it is described and its parameters listed under a name
/// that is NOT an identifier — and that same dot is why nothing about it can be changed.
pub fn is_parameter_group_identifier(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_alphabetic())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// The message `ParameterGroupNotFoundFault` carries when a request names a parameter
/// group it MODIFIES THROUGH — a cluster's `ParameterGroupName`, or the group an
/// `UpdateParameterGroup` changes the parameters of: the fixed text
/// `ParameterGroup not found: ` followed by that name, so the caller is told which group
/// could not be resolved. DAX words the two paths differently, and each is taken from
/// its own recorded answer.
pub fn parameter_group_not_found_named_message(name: &str) -> String {
    format!("ParameterGroup not found: {name}")
}

/// The page of `names` to answer, and the token that continues it.
///
/// `start_from` is the name a continuation token resolved to (see [`decode_token`]); the
/// page begins at the first name at or after it. `max_results` is the caller's page
/// size, clamped into `1..=`[`MAX_MAX_RESULTS`] here as well as validated by the
/// handler, so this helper can never answer an unbounded page nor an empty one that
/// hands back the same token forever.
pub fn page<'a>(
    names: &'a [&'a str],
    max_results: Option<i32>,
    start_from: Option<&str>,
) -> (&'a [&'a str], Option<String>) {
    let start = match start_from {
        Some(from) => names.partition_point(|name| *name < from),
        None => 0,
    };
    let size = usize::try_from(max_results.unwrap_or(MAX_MAX_RESULTS).clamp(1, MAX_MAX_RESULTS))
        .unwrap_or(1);
    let rest = &names[start.min(names.len())..];
    let end = size.min(rest.len());
    let token = rest.get(end).map(::aws_smithy_types::base64::encode);
    (&rest[..end], token)
}

/// The name a continuation token produced by [`page`] resolves to; `None` when the
/// token is not one this service issued, which the handler refuses rather than
/// answering some arbitrary page with a 200.
pub fn decode_token(token: &str) -> Option<String> {
    ::aws_smithy_types::base64::decode(token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|name| !name.is_empty())
}

/// A stored instant as the modelled `TStamp`.
pub fn to_date_time(at: SystemTime) -> ::aws_smithy_types::DateTime {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or(0.0);
    ::aws_smithy_types::DateTime::from_secs_f64(secs)
}

/// A modelled `TStamp` as an instant, or `None` for a timestamp this service cannot
/// place: a value that is not finite, is before the epoch, or is further from it than
/// an instant reaches. A request carries this value, so every one of those answers
/// `None` for the caller to refuse rather than panicking in the conversion.
pub fn from_date_time(at: ::aws_smithy_types::DateTime) -> Option<SystemTime> {
    let secs = at.as_secs_f64();
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(secs).ok()?)
}

// ---------------------------------------------------------------------------
// The message each fault answers with
//
// A fault's wording is built here and nowhere else, so every site raising it answers
// the same text. A fault raised on more than one KIND of path has one template per kind,
// each taken from that path's own recorded answer and each named for it: a request that
// ADDRESSES one resource, one that names it as one element of a requested LIST, and one
// that REFERENCES it from another resource word their not-found differently. A template's
// variable parts come from the request and from this store, never from where the fault
// happened to be raised.
// ---------------------------------------------------------------------------

/// `ClusterNotFoundFault` for a request addressed to ONE cluster: the template names no
/// identifier, because the request already carries the only name it could mean.
pub fn cluster_not_found() -> ClusterNotFoundFault {
    ClusterNotFoundFault {
        message: Some("Cluster not found.".to_string()),
    }
}

/// `ClusterNotFoundFault` for one element of a requested LIST of cluster names: the
/// template NAMES that element, so a caller that asked for several clusters can tell
/// which of them is missing. `name` is the identifier as this service holds it —
/// `CreateCluster$ClusterName` documents that it "is stored as a lowercase string", so
/// the lower-cased form is the cluster's identity rather than a rewrite of the request.
pub fn cluster_not_found_named(name: &str) -> ClusterNotFoundFault {
    ClusterNotFoundFault {
        message: Some(format!("Cluster {name} not found.")),
    }
}

/// `SubnetGroupNotFoundFault` for a request that REFERENCES one subnet group — a cluster
/// placed in it: the template names no identifier, because the request already carries
/// the only group it could mean.
pub fn subnet_group_not_found() -> SubnetGroupNotFoundFault {
    SubnetGroupNotFoundFault {
        message: Some("Subnet Group not found.".to_string()),
    }
}

/// `SubnetGroupNotFoundFault` for one element of a requested LIST of subnet group names:
/// the template NAMES that group, so a caller that asked for several groups can tell
/// which of them is missing. `name` is the identifier as this service holds it —
/// `CreateSubnetGroup$SubnetGroupName` documents that it "is stored as a lowercase
/// string", so the lower-cased form is the group's identity rather than a rewrite of the
/// request.
pub fn subnet_group_not_found_named(name: &str) -> SubnetGroupNotFoundFault {
    SubnetGroupNotFoundFault {
        message: Some(format!("Subnet group {name} not found.")),
    }
}

/// `SubnetGroupNotFoundFault` for a request ADDRESSED to the one subnet group it names —
/// a modification of that group: the template names the group and words the failure as
/// the group not existing, which is how the addressed-group paths answer a name that
/// resolves to nothing. `name` is the identifier as this service holds it, for the reason
/// [`subnet_group_not_found_named`] gives.
pub fn subnet_group_does_not_exist(name: &str) -> SubnetGroupNotFoundFault {
    SubnetGroupNotFoundFault {
        message: Some(format!("Subnet Group {name} does not exist.")),
    }
}

/// `InvalidClusterStateFault` for a cluster that is not [`CLUSTER_AVAILABLE`], which
/// is the only status a modification is accepted from.
pub fn invalid_cluster_state() -> InvalidClusterStateFault {
    InvalidClusterStateFault {
        message: Some(format!("Cluster is not in the {CLUSTER_AVAILABLE} state.")),
    }
}

/// `ClusterAlreadyExistsFault` for a create of a name the account already holds: DAX
/// words this refusal from a FIXED template that names no identifier, because the request
/// already carries the only name it could mean.
pub fn cluster_already_exists() -> ClusterAlreadyExistsFault {
    ClusterAlreadyExistsFault {
        message: Some("Cluster with specified name already exists.".to_string()),
    }
}

/// `InvalidClusterStateFault` for a DELETE of a cluster whose nodes are not all in a
/// state a deletion can take them from. A cluster is deleted by deleting its NODES, so
/// the refusal is worded about the node that blocks it and names that node's id — the one
/// part taken from stored state — rather than about the cluster.
pub fn node_not_in_deletable_state(node_id: &str) -> InvalidClusterStateFault {
    InvalidClusterStateFault {
        message: Some(format!(
            "Node {node_id} is not in a valid state to be deleted."
        )),
    }
}

/// `InvalidParameterValueException` for a `MaxResults` outside the window both
/// paginated describes document ("The value for `MaxResults` must be between 20 and
/// 100"), carrying [`max_results_out_of_range_message`].
pub fn invalid_max_results() -> InvalidParameterValueException {
    InvalidParameterValueException {
        message: Some(max_results_out_of_range_message()),
    }
}

/// `InvalidParameterValueException` for a `NextToken` this service did not issue,
/// naming the operation whose page it would have continued.
pub fn invalid_next_token(operation: &str) -> InvalidParameterValueException {
    InvalidParameterValueException {
        message: Some(format!(
            "NextToken is not a continuation token from a prior {operation} response"
        )),
    }
}

/// `InvalidParameterValueException` for a request naming subnets that do not exist in
/// the Region's network: the template lists the identifiers that could not be
/// resolved, which is the one part of it built from the request.
pub fn invalid_subnets(subnet_ids: &[String]) -> InvalidParameterValueException {
    InvalidParameterValueException {
        message: Some(format!(
            "Some input subnets in :[{}] are invalid.",
            subnet_ids.join(", ")
        )),
    }
}

/// `InvalidParameterCombinationException` for a `DescribeEvents` window reaching
/// further back than the events DAX retains ([`MAX_EVENT_DURATION`]).
pub fn events_beyond_retention() -> InvalidParameterCombinationException {
    InvalidParameterCombinationException {
        message: Some(format!(
            "Events occurring more than {} days in the past are not available.",
            MAX_EVENT_DURATION / (24 * 60)
        )),
    }
}

/// `ParameterGroupNotFoundFault` for a parameter group this store does not hold, on the
/// paths that name groups as a FILTER over what they report and that REMOVE one, carrying
/// [`parameter_group_not_found_message`].
pub fn parameter_group_not_found() -> ParameterGroupNotFoundFault {
    ParameterGroupNotFoundFault {
        message: Some(parameter_group_not_found_message()),
    }
}

/// `ParameterGroupNotFoundFault` for a read of the PARAMETERS of a parameter group this
/// store does not hold, carrying [`parameter_group_not_found_for_parameters_message`] for
/// the name the request asked for. The single site that path raises this fault from, so
/// it reads the same whether the group was never created or has been removed.
pub fn parameter_group_not_found_for_parameters(name: &str) -> ParameterGroupNotFoundFault {
    ParameterGroupNotFoundFault {
        message: Some(parameter_group_not_found_for_parameters_message(name)),
    }
}

/// `ParameterGroupNotFoundFault` for a parameter group a cluster REFERENCES and this
/// store does not hold, carrying [`parameter_group_not_found_named_message`] for the name
/// the request carried.
pub fn parameter_group_not_found_named(name: &str) -> ParameterGroupNotFoundFault {
    ParameterGroupNotFoundFault {
        message: Some(parameter_group_not_found_named_message(name)),
    }
}

/// `InvalidParameterValueException` for a request that asks to CREATE a parameter group,
/// or to CHANGE one, under a name which is not a valid parameter-group identifier
/// ([`is_parameter_group_identifier`]), carrying
/// [`PARAMETER_GROUP_NAME_NOT_AN_IDENTIFIER_MESSAGE`].
pub fn parameter_group_name_not_an_identifier() -> InvalidParameterValueException {
    InvalidParameterValueException {
        message: Some(PARAMETER_GROUP_NAME_NOT_AN_IDENTIFIER_MESSAGE.to_string()),
    }
}

/// `ParameterGroupAlreadyExistsFault` for a name this store already holds. Like this
/// service's other parameter-group fault the template is generic: the name the request
/// carried is NOT echoed back.
pub fn parameter_group_already_exists() -> ParameterGroupAlreadyExistsFault {
    ParameterGroupAlreadyExistsFault {
        message: Some("Parameter Group already exists.".to_string()),
    }
}

/// `InvalidParameterValueException` for a request naming a parameter the caching
/// software does not define. The names it MAY name are exactly the ones
/// [`DEFAULT_PARAMETERS`] publishes, so the refusal is driven by the catalog rather than
/// by a list written out beside it.
pub fn unknown_parameter_name(name: &str) -> InvalidParameterValueException {
    InvalidParameterValueException {
        message: Some(format!("Unknown parameter name: {name}")),
    }
}
