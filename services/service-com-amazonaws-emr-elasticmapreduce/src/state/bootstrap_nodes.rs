// SPDX-License-Identifier: CC0-1.0

//! What ONE NODE of a cluster is, as a bootstrap action running on it sees it: the node's own
//! metadata service and the file Amazon EMR publishes its role in.
//!
//! # The rule this module implements
//!
//! A bootstrap action runs ON THE CLUSTER'S NODES — "a list of bootstrap actions to run before
//! Hadoop starts on the cluster nodes" (`RunJobFlow$BootstrapActions` doc) — so the script is
//! performed once on every node the launch provisioned, and each run is a run OF THAT NODE: it
//! reads that node's own identity and that node's own role, and the effects it leaves are that
//! node's. A script that writes one object per node therefore leaves as many objects as the
//! cluster has nodes, and one that does something only on the primary does it once.
//!
//! What a node gives such a script is what Amazon EMR documents a node as giving one:
//!
//! * the INSTANCE METADATA SERVICE of the Amazon EC2 instance the node is, at the link-local
//!   address every documented metadata read is made to — the wire itself is this workspace's
//!   one statement of it ([`::service_common::instance_metadata_service`]), and what THIS node
//!   publishes is the stored instance record and nothing else;
//! * `/mnt/var/lib/info/instance.json` ([`INSTANCE_INFO_PATH`]), the file whose `isMaster` an
//!   Amazon EMR bootstrap action reads to tell which node it is running on (Amazon EMR
//!   Management Guide, "Create bootstrap actions to install additional software" — "run if …
//!   `grep isMaster /mnt/var/lib/info/instance.json`").
//!
//! Every value in both is read off the cluster and the instance record Amazon EC2 answered
//! with; nothing is echoed from a request, and a fact the record does not carry is published by
//! neither. No host path is opened: the file is seeded into the session's own tree, where a
//! path is a key ([`::service_common::posix_shell_session`]).

use ::service_common::instance_metadata_service::{
    metadata_client, PublishedInstanceMetadata, METADATA_PREFIX,
};
use ::service_common::posix_shell_session::EnvironmentPrograms;

use super::*;

/// The file Amazon EMR publishes a node's own role in, at the path the Management Guide reads
/// it from.
pub const INSTANCE_INFO_PATH: &str = "/mnt/var/lib/info/instance.json";

/// The metadata categories the listing of the metadata prefix names, in the order a directory
/// listing writes them (alphabetically). A category this node has no value for is left out of
/// the listing by the reader below, exactly as it is left out of the path itself.
const METADATA_CATEGORIES: &[&str] = &[
    "hostname",
    "instance-id",
    "instance-type",
    "local-hostname",
    "local-ipv4",
    "placement/availability-zone",
    "placement/region",
];

/// ONE node of a cluster, as the bootstrap actions running on it see it.
///
/// Every field is read off the cluster and off the instance record Amazon EC2 answered with
/// ([`ClusterRecord::bootstrap_nodes`]); a field the record does not carry is `None`, and
/// nothing is published for it.
#[derive(Debug, Clone)]
pub struct BootstrapNode {
    /// The cluster this node belongs to (`Cluster$Id`), which is the job flow its
    /// `instance.json` names.
    pub cluster_id: String,
    /// `Instance$Ec2InstanceId`: the instance Amazon EC2 minted for this node, which is what
    /// its metadata service publishes as its instance id. `None` for a cluster this emulator
    /// launched no instance for, which is still owed one run of each action — with no node
    /// identity to read.
    pub ec2_instance_id: Option<String>,
    /// `Instance$InstanceType`.
    pub instance_type: Option<String>,
    /// The private address and the private DNS name Amazon EC2 gave it.
    pub private_ip_address: Option<String>,
    pub private_dns_name: Option<String>,
    /// The zone the cluster's instances were placed in
    /// (`Ec2InstanceAttributes$Ec2AvailabilityZone`).
    pub availability_zone: Option<String>,
    /// The Region the request that performs the action was addressed to, which is the Region
    /// this node is in.
    pub region: String,
    /// The collection this node was provisioned by, as one of the two members a cluster states
    /// its capacity in (`Instance$InstanceGroupId`, `Instance$InstanceFleetId`).
    pub instance_group_id: Option<String>,
    pub instance_fleet_id: Option<String>,
    /// Whether this node fills the primary role of the cluster — "valid values are MASTER,
    /// CORE or TASK" (`InstanceGroupConfig$InstanceRole`) — which is what `isMaster` reports.
    pub is_master: bool,
}

impl BootstrapNode {
    /// What names this node where one run of an action is claimed and recorded: the instance
    /// Amazon EC2 minted for it, and the primary role for a cluster that holds no instance.
    /// The same naming [`ClusterRecord::bootstrap_node_keys`] answers with.
    #[must_use]
    pub fn key(&self) -> String {
        self.ec2_instance_id
            .clone()
            .unwrap_or_else(|| instance_group_type::MASTER.to_string())
    }

    /// The document `/mnt/var/lib/info/instance.json` holds on this node.
    ///
    /// Written as the file is: one member per line, so the `grep` of one member an Amazon EMR
    /// bootstrap action is documented with selects that member's line. Every member is a fact
    /// of this node, and a member the record carries nothing for is left out rather than
    /// written as a value.
    #[must_use]
    pub fn instance_info(&self) -> String {
        let mut document = ::serde_json::Map::new();
        document.insert(
            "jobFlowId".to_string(),
            ::serde_json::Value::String(self.cluster_id.clone()),
        );
        if let Some(id) = &self.ec2_instance_id {
            document.insert(
                "instanceId".to_string(),
                ::serde_json::Value::String(id.clone()),
            );
        }
        if let Some(id) = &self.instance_group_id {
            document.insert(
                "instanceGroupId".to_string(),
                ::serde_json::Value::String(id.clone()),
            );
        }
        if let Some(id) = &self.instance_fleet_id {
            document.insert(
                "instanceFleetId".to_string(),
                ::serde_json::Value::String(id.clone()),
            );
        }
        document.insert(
            "isMaster".to_string(),
            ::serde_json::Value::Bool(self.is_master),
        );
        format!(
            "{}\n",
            ::serde_json::to_string_pretty(&::serde_json::Value::Object(document))
                .unwrap_or_default()
        )
    }

    /// The programs this node has installed in it, beside the shell's own utilities: the
    /// client that reads its metadata service.
    #[must_use]
    pub fn node_programs(&self) -> Box<dyn EnvironmentPrograms> {
        metadata_client(Box::new(self.clone()))
    }
}

impl PublishedInstanceMetadata for BootstrapNode {
    /// Both forms of the metadata read answer on an Amazon EMR node: the launch of a cluster
    /// asks for no metadata option of its instances, and an instance that requires no session
    /// token is one where "IMDSv2 is optional, which means that you can use either IMDSv2 or
    /// IMDSv1" (`InstanceMetadataOptionsRequest$HttpTokens`) — so a bootstrap action written
    /// either way reads the node it runs on.
    fn token_required(&self) -> bool {
        false
    }

    /// The value this node's metadata service publishes at `path`, as the lines it answers
    /// with, and `None` for a path it publishes nothing at.
    ///
    /// The paths are the instance-metadata categories AWS publishes (Amazon EC2 User Guide,
    /// "Instance metadata categories") for the facts this node holds; a category whose value
    /// the record does not carry is published by neither the listing nor the path itself.
    fn published(&self, path: &str) -> Option<Vec<String>> {
        let one = |value: &Option<String>| value.clone().map(|value| vec![value]);
        match path {
            METADATA_PREFIX => Some(
                METADATA_CATEGORIES
                    .iter()
                    .filter(|category| {
                        self.published(&format!("{METADATA_PREFIX}/{category}"))
                            .is_some()
                    })
                    .map(|category| (*category).to_string())
                    .collect(),
            ),
            "/latest/meta-data/instance-id" => one(&self.ec2_instance_id),
            "/latest/meta-data/instance-type" => one(&self.instance_type),
            "/latest/meta-data/local-ipv4" => one(&self.private_ip_address),
            "/latest/meta-data/local-hostname" | "/latest/meta-data/hostname" => {
                one(&self.private_dns_name)
            }
            "/latest/meta-data/placement/availability-zone" => one(&self.availability_zone),
            "/latest/meta-data/placement/region" => Some(vec![self.region.clone()]),
            _ => None,
        }
    }
}

impl ClusterRecord {
    /// What NAMES each node a bootstrap action of this cluster runs on
    /// ([`BootstrapNode::key`]), which is what a recorded run is kept under: the cheap reading
    /// of the node list, for the questions that only count the runs owed and answered.
    pub(crate) fn bootstrap_node_keys(&self) -> Vec<String> {
        let keys: Vec<String> = self
            .ec2_instances
            .iter()
            .filter(|instance| instance.state != instance_state::TERMINATED)
            .map(|instance| instance.ec2_instance_id.clone())
            .collect();
        if keys.is_empty() {
            return vec![instance_group_type::MASTER.to_string()];
        }
        keys
    }

    /// The nodes a bootstrap action of this cluster runs on: every Amazon EC2 instance the
    /// cluster's collections were provisioned from that has not been terminated, in launch
    /// order.
    ///
    /// A cluster this emulator launched no instance for — nothing that owns instances is
    /// mounted beside Amazon EMR — is still owed one run of each action, so ONE node stands for
    /// it: the phase a launch carrying actions is owed cannot be skipped for want of an
    /// instance record, and that node publishes no identity, because none is known.
    pub(crate) fn bootstrap_nodes(&self, region: &str) -> Vec<BootstrapNode> {
        let nodes: Vec<BootstrapNode> = self
            .ec2_instances
            .iter()
            .filter(|instance| instance.state != instance_state::TERMINATED)
            .map(|instance| BootstrapNode {
                cluster_id: self.id.clone(),
                ec2_instance_id: Some(instance.ec2_instance_id.clone()),
                instance_type: Some(instance.instance_type.clone()),
                private_ip_address: instance.private_ip_address.clone(),
                private_dns_name: instance.private_dns_name.clone(),
                availability_zone: self.instances.ec2_availability_zone.clone(),
                region: region.to_string(),
                instance_group_id: instance.instance_group_id.clone(),
                instance_fleet_id: instance.instance_fleet_id.clone(),
                is_master: self.fills_master_role(instance),
            })
            .collect();
        if !nodes.is_empty() {
            return nodes;
        }
        vec![BootstrapNode {
            cluster_id: self.id.clone(),
            ec2_instance_id: None,
            instance_type: None,
            private_ip_address: None,
            private_dns_name: None,
            availability_zone: self.instances.ec2_availability_zone.clone(),
            region: region.to_string(),
            instance_group_id: None,
            instance_fleet_id: None,
            is_master: true,
        }]
    }

    /// Whether `instance` fills the PRIMARY role of this cluster: its collection is the one
    /// whose role is `MASTER`, in whichever of the two vocabularies the cluster states its
    /// capacity in (`InstanceGroupConfig$InstanceRole`, `InstanceFleetType`).
    fn fills_master_role(&self, instance: &ClusterInstanceRecord) -> bool {
        let group = instance.instance_group_id.as_deref().is_some_and(|id| {
            self.groups.iter().any(|group| {
                group.id == id && group.config.instance_role == instance_group_type::MASTER
            })
        });
        let fleet = instance.instance_fleet_id.as_deref().is_some_and(|id| {
            self.fleets.iter().any(|fleet| {
                fleet.id == id && fleet.config.instance_fleet_type == instance_group_type::MASTER
            })
        });
        group || fleet
    }
}
