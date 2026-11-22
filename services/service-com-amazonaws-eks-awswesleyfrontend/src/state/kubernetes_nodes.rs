// SPDX-License-Identifier: CC0-1.0

//! The NODES of a cluster, as its Kubernetes API server reports them.
//!
//! A cluster's nodes are not objects a client creates: they are the Amazon EC2
//! instances the cluster's managed node groups launched, which register
//! themselves with the cluster's API server as they come up. A node group IS "an
//! Amazon EC2 Auto Scaling group" ([`super::nodegroup_capacity`]), so the nodes
//! this module reports are the instances THOSE services hold — read back from
//! them when the node group's capacity is created and recorded on the node group
//! ([`NodegroupCapacity::nodes`]) — and never a count this service invents: a
//! cluster with no node group, or one whose capacity no Amazon EC2 service
//! created, reports no nodes.
//!
//! What each node reports about itself comes from the node group it belongs to:
//! the Kubernetes version its cluster runs, the instance type and Availability
//! Zone the instance really launched with, the capacity type the node group was
//! created with, and the labels and taints the node group applies to its nodes.

use ::std::collections::BTreeSet;

use super::*;

/// The taints the nodes of `nodegroup` carry, as KUBERNETES spells them.
///
/// One projection, read by the `Node` document a client reads and by the
/// scheduler that places a pod, so what a client sees on a node and what the
/// scheduler holds a pod to cannot disagree.
fn node_taints(nodegroup: &NodegroupRecord) -> Vec<NodeTaint> {
    nodegroup
        .taints
        .iter()
        .map(|taint| NodeTaint {
            key: taint.key.clone().unwrap_or_default(),
            value: taint.value.clone().unwrap_or_default(),
            effect: kubernetes_taint_effect(taint.effect.as_deref().unwrap_or_default()),
        })
        .collect()
}

/// One taint a node carries, in the vocabulary Kubernetes carries one in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeTaint {
    pub key: String,
    pub value: String,
    /// The effect, spelled as `Node.spec.taints[].effect` spells one
    /// ([`kubernetes_taint_effect`]).
    pub effect: String,
}

/// The Kubernetes taint effect `NoSchedule` and the two others Kubernetes has, in
/// the only spelling `Node.spec.taints[].effect` takes.
pub const KUBERNETES_TAINT_EFFECT_NO_SCHEDULE: &str = "NoSchedule";
/// See [`KUBERNETES_TAINT_EFFECT_NO_SCHEDULE`].
pub const KUBERNETES_TAINT_EFFECT_NO_EXECUTE: &str = "NoExecute";
/// See [`KUBERNETES_TAINT_EFFECT_NO_SCHEDULE`].
pub const KUBERNETES_TAINT_EFFECT_PREFER_NO_SCHEDULE: &str = "PreferNoSchedule";

/// One `TaintEffect` of the EKS API, spelled as the KUBERNETES taint effect it is.
///
/// The two APIs name the same three effects in their own vocabularies: the EKS API
/// takes `NO_SCHEDULE`, `NO_EXECUTE` and `PREFER_NO_SCHEDULE`
/// (`com.amazonaws.eks#TaintEffect`, the enum `Taint$effect` targets), and
/// Kubernetes takes `NoSchedule`, `NoExecute` and `PreferNoSchedule` and nothing
/// else — so a node group's taint reaches the node it is applied to spelled the
/// way the node's own API spells it. The translation is the one that carries each
/// of those enum values onto its Kubernetes spelling: the words of the enum value,
/// each capitalised.
#[must_use]
pub fn kubernetes_taint_effect(effect: &str) -> String {
    effect
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut letters = word.chars();
            match letters.next() {
                Some(first) => {
                    format!(
                        "{}{}",
                        first.to_ascii_uppercase(),
                        letters.as_str().to_ascii_lowercase()
                    )
                }
                None => String::new(),
            }
        })
        .collect()
}

impl NodeTaint {
    /// Whether this taint KEEPS a pod that does not tolerate it off the node: the
    /// two effects that do ("`NoSchedule`: pods that do not tolerate this taint
    /// are not scheduled on the node", and `NoExecute`, which also evicts one
    /// already there), as against `PreferNoSchedule`, which only makes the node a
    /// worse choice than an untainted one.
    #[must_use]
    pub fn repels_untolerating_pods(&self) -> bool {
        self.effect == KUBERNETES_TAINT_EFFECT_NO_SCHEDULE
            || self.effect == KUBERNETES_TAINT_EFFECT_NO_EXECUTE
    }
}

/// The `Toleration$operator` that tolerates a taint whatever value it carries, as
/// against the default `Equal`, which tolerates it only at one value.
pub const TOLERATION_OPERATOR_EXISTS: &str = "Exists";

/// What a pod TOLERATES, read once out of its `spec.tolerations` and indexed by
/// the key each entry names.
///
/// Read once per pod rather than per node: the scheduler asks the same question of
/// every node it could place the pod on, so the cost of one placement is the
/// pod's toleration list once plus a lookup per taint of the node — never the
/// product of the two lists. The list itself is held to
/// [`super::kubernetes_pod_spec::MAX_TOLERATIONS_PER_POD`] entries when the pod is
/// admitted, before anything is stored.
#[derive(Debug, Clone, Default)]
pub struct PodTolerations {
    /// The effects of the tolerations that name NO key: with operator `Exists`
    /// such a toleration tolerates every taint the node carries, under the effect
    /// it names — the empty string for one that names none, which tolerates a
    /// taint whatever its effect.
    keyless: BTreeSet<String>,
    /// The tolerations that name a key, by that key.
    by_key: BTreeMap<String, KeyTolerations>,
}

/// What a pod tolerates for ONE taint key: the effects it tolerates whatever the
/// taint's value (operator `Exists`), and the effects it tolerates at one value
/// (the default operator `Equal`), by that value.
///
/// Both are keyed so that a taint is answered by lookup: the empty string stands
/// for "whatever the effect", which is what a toleration naming no effect means.
#[derive(Debug, Clone, Default)]
struct KeyTolerations {
    exists: BTreeSet<String>,
    equal: BTreeMap<String, BTreeSet<String>>,
}

impl KeyTolerations {
    /// Whether this key's tolerations tolerate a taint of `effect` carrying
    /// `value`.
    fn tolerates(&self, effect: &str, value: &str) -> bool {
        if self.exists.contains("") || self.exists.contains(effect) {
            return true;
        }
        self.equal
            .get(value)
            .is_some_and(|effects| effects.contains("") || effects.contains(effect))
    }
}

impl PodTolerations {
    /// The tolerations the pod `spec` declares. A spec that declares none
    /// tolerates nothing, which is what a pod without the member does.
    #[must_use]
    pub fn of(spec: &::serde_json::Value) -> Self {
        let mut tolerations = Self::default();
        let declared = spec
            .get("tolerations")
            .and_then(::serde_json::Value::as_array)
            .map(|declared| declared.as_slice())
            .unwrap_or_default();
        for toleration in declared {
            let member = |name: &str| {
                toleration
                    .get(name)
                    .and_then(::serde_json::Value::as_str)
                    .unwrap_or_default()
            };
            // An absent `effect` tolerates the taint whatever its effect, which is
            // what the empty string stands for in both indexes.
            let effect = member("effect").to_string();
            let key = member("key");
            let exists = member("operator") == TOLERATION_OPERATOR_EXISTS;
            if key.is_empty() {
                // A toleration that names no key tolerates every taint — but only
                // with operator `Exists`; naming no key and no operator tolerates
                // nothing, as Kubernetes reads it.
                if exists {
                    tolerations.keyless.insert(effect);
                }
                continue;
            }
            let held = tolerations.by_key.entry(key.to_string()).or_default();
            if exists {
                held.exists.insert(effect);
            } else {
                held.equal
                    .entry(member("value").to_string())
                    .or_default()
                    .insert(effect);
            }
        }
        tolerations
    }

    /// Whether the pod tolerates `taint`.
    #[must_use]
    pub fn tolerate(&self, taint: &NodeTaint) -> bool {
        if self.keyless.contains("") || self.keyless.contains(taint.effect.as_str()) {
            return true;
        }
        self.by_key
            .get(taint.key.as_str())
            .is_some_and(|held| held.tolerates(&taint.effect, &taint.value))
    }
}

/// One node of a cluster: the Amazon EC2 instance a managed node group launched,
/// as the services that own it reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeInstance {
    /// The instance's id in Amazon EC2 (`i-…`), which is what the node's
    /// `spec.providerID` names and what makes the node resolvable in the service
    /// that owns it.
    pub instance_id: String,
    /// The instance's private DNS name, which is the name a node registers with
    /// its cluster under. Empty when EC2 reported none, in which case the node is
    /// named by its instance id — the other naming an Amazon EKS node registers
    /// under.
    pub private_dns_name: String,
    /// The Availability Zone the instance launched in, as the Auto Scaling group
    /// reported it.
    pub availability_zone: String,
    /// The instance type it launched as, as that group reported it.
    pub instance_type: String,
    /// Its private IPv4 address, as EC2 reported it, which is the node's
    /// `InternalIP` address.
    pub private_ip_address: String,
}

impl NodeInstance {
    /// The name this node registers with the cluster under: its private DNS name,
    /// or its instance id when EC2 published none.
    #[must_use]
    pub fn node_name(&self) -> &str {
        if self.private_dns_name.is_empty() {
            &self.instance_id
        } else {
            &self.private_dns_name
        }
    }
}

/// The node's `metadata.uid`.
///
/// A node is DERIVED from the instance behind it rather than stored, so the uid
/// is derived too — from that instance's id, through the one derivation every
/// UUID-shaped identifier of this service goes through — and every read of the
/// node reports the same value.
fn node_uid(instance_id: &str) -> String {
    derive_uuid_from_seed(&format!("node/{instance_id}"))
}

/// The `kubelet` version a node reports: the cluster's own Kubernetes version in
/// the form an Amazon EKS node publishes it, built from the version the cluster
/// record carries so a node and `DescribeCluster` cannot disagree about which
/// Kubernetes the cluster runs.
fn kubelet_version(cluster: &ClusterRecord) -> String {
    let version = cluster
        .version
        .clone()
        .unwrap_or_else(|| DEFAULT_KUBERNETES_VERSION.to_string());
    format!("v{version}.0-eks")
}

/// The processor architecture a node group's nodes run, read from the AMI type
/// the node group was created with: an `ARM` AMI runs `arm64` nodes, every other
/// published Amazon EKS AMI type runs `amd64` ones.
fn architecture(ami_type: &str) -> &'static str {
    if ami_type.contains("ARM") {
        "arm64"
    } else {
        "amd64"
    }
}

/// The operating system a node group's nodes run: every published Amazon EKS AMI
/// type whose name does not say Windows is a Linux one.
fn operating_system(ami_type: &str) -> &'static str {
    if ami_type.to_ascii_uppercase().contains("WINDOWS") {
        "windows"
    } else {
        "linux"
    }
}

/// One node of `nodegroup` as the `Node` object a Kubernetes client reads.
fn node_json(
    cluster: &ClusterRecord,
    region: &str,
    nodegroup: &NodegroupRecord,
    node: &NodeInstance,
) -> ::serde_json::Value {
    let name = node.node_name();
    let architecture = architecture(&nodegroup.ami_type);
    let operating_system = operating_system(&nodegroup.ami_type);
    // The labels every Amazon EKS node carries: the well-known Kubernetes ones
    // that name the node, its platform and its placement, and the Amazon EKS ones
    // that name the node group it belongs to. The node group's OWN labels are
    // applied over them, which is what "the Kubernetes labels applied to the
    // nodes in the node group" (`Nodegroup$labels`) means.
    let mut labels: BTreeMap<String, String> = [
        ("kubernetes.io/hostname", name.to_string()),
        ("kubernetes.io/os", operating_system.to_string()),
        ("kubernetes.io/arch", architecture.to_string()),
        ("beta.kubernetes.io/os", operating_system.to_string()),
        ("beta.kubernetes.io/arch", architecture.to_string()),
        (
            "node.kubernetes.io/instance-type",
            node.instance_type.clone(),
        ),
        (
            "beta.kubernetes.io/instance-type",
            node.instance_type.clone(),
        ),
        ("topology.kubernetes.io/region", region.to_string()),
        (
            "topology.kubernetes.io/zone",
            node.availability_zone.clone(),
        ),
        (
            "failure-domain.beta.kubernetes.io/region",
            region.to_string(),
        ),
        (
            "failure-domain.beta.kubernetes.io/zone",
            node.availability_zone.clone(),
        ),
        (
            "eks.amazonaws.com/nodegroup",
            nodegroup.nodegroup_name.clone(),
        ),
        (
            "eks.amazonaws.com/capacityType",
            nodegroup.capacity_type.clone(),
        ),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    labels.extend(
        nodegroup
            .labels
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let taints: Vec<::serde_json::Value> = node_taints(nodegroup)
        .iter()
        .map(|taint| {
            ::serde_json::json!({
                "key": taint.key,
                "value": taint.value,
                "effect": taint.effect,
            })
        })
        .collect();
    let mut addresses = vec![::serde_json::json!({
        "type": "Hostname",
        "address": name,
    })];
    if !node.private_ip_address.is_empty() {
        addresses.push(::serde_json::json!({
            "type": "InternalIP",
            "address": node.private_ip_address,
        }));
    }
    if !node.private_dns_name.is_empty() {
        addresses.push(::serde_json::json!({
            "type": "InternalDNS",
            "address": node.private_dns_name,
        }));
    }
    // A node of a node group that has come up is a node that joined: the node
    // group reports `ACTIVE` only once its nodes are registered with the cluster,
    // which is the condition `kubelet` posts.
    let joined_at = super::kubernetes_objects::rfc3339_seconds(nodegroup.created_at);
    ::serde_json::json!({
        "kind": "Node",
        "apiVersion": super::kubernetes_objects::CORE_API_VERSION,
        "metadata": {
            "name": name,
            "uid": node_uid(&node.instance_id),
            "creationTimestamp": joined_at,
            "labels": labels,
        },
        "spec": {
            // "aws:///<availability zone>/<instance id>": how the in-cluster AWS
            // cloud provider names the instance behind a node.
            "providerID": format!("aws:///{}/{}", node.availability_zone, node.instance_id),
            "taints": taints,
        },
        "status": {
            "conditions": [{
                "type": "Ready",
                "status": "True",
                "reason": "KubeletReady",
                "message": "kubelet is posting ready status",
                "lastHeartbeatTime": joined_at,
                "lastTransitionTime": joined_at,
            }],
            "addresses": addresses,
            "nodeInfo": {
                "architecture": architecture,
                "operatingSystem": operating_system,
                "kubeletVersion": kubelet_version(cluster),
                "kubeProxyVersion": kubelet_version(cluster),
                "systemUUID": node.instance_id,
            },
        },
    })
}

/// The fields a NODE may be SELECTED by, as `(the field label a client writes, the
/// member of the node document it names)`: its own name — a node belongs to no
/// namespace — and whether it was marked unschedulable, which are the fields the
/// Kubernetes API server's own node registry indexes. A `fieldSelector` naming
/// anything else is refused the way the API server refuses a field label it does not
/// index.
pub const NODE_FIELD_SELECTORS: &[(&str, &str)] = &[
    ("metadata.name", "metadata.name"),
    ("spec.unschedulable", "spec.unschedulable"),
];

/// One node of a cluster, as the SCHEDULER needs it: the name it registered
/// under and the instant it joined the cluster.
/// Reported without rendering the node's whole document, because binding a pod to
/// a node needs neither the node's labels nor its conditions — and the instant is
/// what makes the pod's own start instant a value read from stored state rather
/// than from whichever call noticed it.
#[derive(Debug, Clone)]
pub struct ClusterNode {
    pub name: String,
    pub joined_at: SystemTime,
    /// The IAM role the node runs under: the `nodeRole` of the node group that
    /// launched it ("the IAM role associated with your node group"), which is the
    /// principal the node's kubelet makes its AWS calls as — the image pull of a
    /// pod scheduled onto this node among them
    /// ([`super::kubernetes_image_pull`]). Empty for a node group that states
    /// none.
    pub node_role: String,
    /// The taints the node group applied to this node, as Kubernetes spells them
    /// ([`node_taints`]) — what the scheduler holds a pod's tolerations against
    /// before it places the pod here.
    pub taints: Vec<NodeTaint>,
}

impl ClusterNode {
    /// Whether this node TAKES a pod with these tolerations: a node repels a pod
    /// that does not tolerate each of its taints that repels untolerating pods
    /// ([`NodeTaint::repels_untolerating_pods`]), which is what a taint on a node
    /// does to the scheduler's choice.
    ///
    /// `tolerations` is read off the pod ONCE ([`PodTolerations::of`]) and asked of
    /// every node, so placing one pod costs its toleration list once plus a lookup
    /// per taint of each node.
    #[must_use]
    pub fn admits(&self, tolerations: &PodTolerations) -> bool {
        self.taints
            .iter()
            .filter(|taint| taint.repels_untolerating_pods())
            .all(|taint| tolerations.tolerate(taint))
    }
}

impl ComAmazonawsEksAwswesleyfrontendState {
    /// Every node of the cluster `cluster_name` in `region`, in node-name order:
    /// the instances of each of its node groups that has come up.
    ///
    /// A node group that is still being created has not registered its nodes with
    /// the cluster yet, and one that is being deleted is taking them away, so
    /// neither contributes a node — the nodes a client sees are the nodes of the
    /// node groups the cluster reports as `ACTIVE`.
    pub fn cluster_nodes(
        &self,
        region: &str,
        cluster_name: &str,
    ) -> Vec<::serde_json::Value> {
        let Some(cluster) = self.clusters.get(&(region.to_string(), cluster_name.to_string()))
        else {
            return Vec::new();
        };
        let mut nodes: Vec<(String, ::serde_json::Value)> = Vec::new();
        for ((key_region, key_cluster, _), nodegroup) in &self.nodegroups {
            if key_region != region || key_cluster != cluster_name {
                continue;
            }
            if nodegroup.removed_at.is_some() || nodegroup.status != STATUS_ACTIVE {
                continue;
            }
            for node in &nodegroup.capacity.nodes {
                nodes.push((
                    node.node_name().to_string(),
                    node_json(cluster, region, nodegroup, node),
                ));
            }
        }
        nodes.sort_by(|left, right| left.0.cmp(&right.0));
        nodes.into_iter().map(|(_, document)| document).collect()
    }

    /// The nodes of that cluster a pod can be scheduled onto, in the order
    /// [`Self::cluster_nodes`] reports them: empty for a cluster whose node groups
    /// have registered none.
    ///
    /// Reads the node group records directly rather than the rendered documents:
    /// this runs on every request to the cluster's API server, and rendering every
    /// node's document to take a name and an instant off it is work no request
    /// needs.
    pub fn scheduler_nodes(&self, region: &str, cluster_name: &str) -> Vec<ClusterNode> {
        let mut nodes: Vec<ClusterNode> = self
            .nodegroups
            .iter()
            .filter(|((key_region, key_cluster, _), nodegroup)| {
                key_region == region
                    && key_cluster == cluster_name
                    && nodegroup.removed_at.is_none()
                    && nodegroup.status == STATUS_ACTIVE
            })
            .flat_map(|(_, nodegroup)| {
                nodegroup.capacity.nodes.iter().map(|node| ClusterNode {
                    name: node.node_name().to_string(),
                    // The instant the node joined the cluster, which is what the
                    // node itself reports as its `creationTimestamp`.
                    joined_at: nodegroup.created_at,
                    // The role the node group launched its instances under, which
                    // is what the node's kubelet acts as.
                    node_role: nodegroup.node_role.clone(),
                    // The taints the node group applies to its nodes, which is
                    // what the scheduler holds a pod's tolerations against.
                    taints: node_taints(nodegroup),
                })
            })
            .collect();
        nodes.sort_by(|left, right| left.name.cmp(&right.name));
        nodes
    }

    /// The node `name` of that cluster, for a read of one node.
    pub fn cluster_node(
        &self,
        region: &str,
        cluster_name: &str,
        name: &str,
    ) -> Option<::serde_json::Value> {
        self.cluster_nodes(region, cluster_name)
            .into_iter()
            .find(|node| {
                node.get("metadata")
                    .and_then(|metadata| metadata.get("name"))
                    .and_then(::serde_json::Value::as_str)
                    == Some(name)
            })
    }
}
