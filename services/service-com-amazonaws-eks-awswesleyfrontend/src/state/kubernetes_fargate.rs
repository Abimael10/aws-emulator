// SPDX-License-Identifier: CC0-1.0

//! The AWS FARGATE capacity a cluster runs the pods its profiles select on.
//!
//! A Fargate profile says which pods of a cluster run on AWS Fargate rather than
//! on a node group's instances: "the selectors to match for a `Pod` to use this
//! Fargate profile. Each selector must have an associated Kubernetes `namespace`.
//! Optionally, you can also specify `labels` for a `namespace`"
//! (`CreateFargateProfileRequest$selectors`). Fargate is capacity the cluster does
//! not hold until a pod needs it: it provisions one node per selected pod, that
//! node registers with the cluster's API server, and the pod is placed on it —
//! which is what the Pod execution role is for, since it "allows Fargate
//! infrastructure to register with your cluster as a node" and to "make calls to
//! AWS APIs on your behalf, such as pulling container images from Amazon ECR"
//! (`CreateFargateProfileRequest$podExecutionRoleArn`).
//!
//! So the capacity here is the same kind of thing a node group's nodes are
//! ([`super::kubernetes_nodes`]) and the scheduler reads both through one value
//! ([`ClusterCapacity`]): a pod a profile selects is placed on the Fargate node
//! this cluster created FOR it, and the kubelet of that node pulls the pod's
//! images as the profile's Pod execution role
//! ([`super::kubernetes_image_pull`]) — exactly as a node group's node pulls them
//! as its node role.
//!
//! One node runs ONE pod on Fargate, so the node is created with the pod and
//! released again once the cluster holds no pod for it; every instant it reports
//! comes from stored state — the pod's creation and the instant its profile's
//! capacity was there — so each caller reads the same node and the same start.

use ::std::collections::BTreeSet;

use super::kubernetes_nodes::{ClusterNode, NodeTaint, KUBERNETES_TAINT_EFFECT_NO_SCHEDULE};
use super::kubernetes_objects::{ClusterObjects, PodObject};
use super::*;

/// The label every node AWS Fargate registers with a cluster carries, and the
/// value it carries on one — the label an EKS client selects Fargate capacity by
/// ("`eks.amazonaws.com/compute-type: fargate`", Amazon EKS User Guide, *AWS
/// Fargate*).
pub const FARGATE_COMPUTE_TYPE_LABEL: &str = "eks.amazonaws.com/compute-type";
/// See [`FARGATE_COMPUTE_TYPE_LABEL`].
pub const FARGATE_COMPUTE_TYPE: &str = "fargate";

/// The label a Fargate node carries the name of the profile that provisioned it
/// in, beside [`FARGATE_COMPUTE_TYPE_LABEL`].
pub const FARGATE_PROFILE_LABEL: &str = "eks.amazonaws.com/fargate-profile";

/// The scheduler that places a pod on Fargate capacity, which is what
/// `spec.schedulerName` names on such a pod: Amazon EKS runs a scheduler of its
/// own for the pods a Fargate profile selects, and the default scheduler places
/// none of them.
pub const FARGATE_SCHEDULER_NAME: &str = "fargate-scheduler";

/// The processor architecture and operating system AWS Fargate runs a pod on:
/// Fargate on Amazon EKS runs Linux on x86-64 ("Amazon EKS on Fargate... Linux
/// containers on x86-64", Amazon EKS User Guide, *AWS Fargate considerations*),
/// so a node it registers reports that platform rather than one read off a node
/// group's AMI type.
const FARGATE_ARCHITECTURE: &str = "amd64";
/// See [`FARGATE_ARCHITECTURE`].
const FARGATE_OPERATING_SYSTEM: &str = "linux";

/// One selector of a Fargate profile, as the capacity reads it: the namespace it
/// matches and the labels a pod of that namespace has to carry.
#[derive(Debug, Clone)]
pub struct FargateSelector {
    pub namespace: String,
    /// The labels the selector names, all of which a pod has to carry at exactly
    /// these values. Empty for a selector that names none, which selects every
    /// pod of its namespace.
    pub labels: BTreeMap<String, String>,
}

/// The capacity ONE Fargate profile of a cluster provides: the pods it selects,
/// the role its infrastructure acts as, and the instant that capacity was there.
#[derive(Debug, Clone)]
pub struct FargateCapacity {
    pub profile_name: String,
    /// `FargateProfile$podExecutionRoleArn` — the principal the node's kubelet
    /// makes its AWS calls as, the image pull of the pod it runs among them.
    pub pod_execution_role: String,
    pub selectors: Vec<FargateSelector>,
    /// The instant this profile's capacity was available to take a pod: the
    /// instant the profile reached `ACTIVE`, which is a stored instant rather
    /// than the instant of the call that read it.
    pub active_since: SystemTime,
}

impl FargateCapacity {
    /// Whether this profile SELECTS a pod of `namespace` carrying `labels`: one of
    /// its selectors has to name that namespace, and every label that selector
    /// names has to be on the pod at the value the selector names. A selector that
    /// names no label selects every pod of its namespace.
    #[must_use]
    pub fn selects(&self, namespace: &str, labels: &BTreeMap<String, String>) -> bool {
        self.selectors.iter().any(|selector| {
            selector.namespace == namespace
                && selector
                    .labels
                    .iter()
                    .all(|(key, value)| labels.get(key) == Some(value))
        })
    }
}

/// One node AWS Fargate provisioned for ONE pod of a cluster and registered with
/// that cluster's API server.
///
/// It is capacity the cluster holds — a client reads it as a `Node` of the cluster
/// and the scheduler places the pod it was made for on it — so it lives with the
/// cluster's other objects ([`ClusterObjects::fargate_nodes`]) and goes when the
/// cluster goes.
#[derive(Debug, Clone)]
pub struct FargateNode {
    /// The name the node registered under ([`fargate_node_name`]).
    pub name: String,
    /// The pod this capacity was provisioned for: its namespace, its name, and the
    /// uid that says it is still the same pod.
    pub namespace: String,
    pub pod_name: String,
    pub pod_uid: String,
    /// The Fargate profile that selected that pod.
    pub profile_name: String,
    /// The Pod execution role of that profile ([`FargateCapacity`]).
    pub pod_execution_role: String,
    /// When the node registered with the cluster: the later of the pod's creation
    /// and the instant its profile's capacity was there, both stored, so the node
    /// and the pod's own start report the same instants to every caller.
    pub joined_at: SystemTime,
}

/// The name the Fargate capacity of one pod registers with the cluster under.
///
/// CHOSEN, not published: AWS names such a node after the private address its
/// pod's network interface was given, and this emulator allocates no address in
/// the customer's VPC — so the node is named after the ONE pod it was provisioned
/// for, which is what identifies it. Derived from the pod's namespace and name, so
/// every read of the cluster reports the same node and two pods never register one
/// node between them.
#[must_use]
pub fn fargate_node_name(namespace: &str, pod_name: &str) -> String {
    format!("{FARGATE_COMPUTE_TYPE}-{namespace}-{pod_name}")
}

impl FargateNode {
    /// This node as the SCHEDULER reads one ([`ClusterNode`]).
    ///
    /// It carries the taint every Fargate node carries, and it takes no pod but
    /// the one it was provisioned for: Fargate runs one pod per node, so the
    /// default scheduler places nothing here.
    #[must_use]
    pub fn as_cluster_node(&self) -> ClusterNode {
        ClusterNode {
            name: self.name.clone(),
            joined_at: self.joined_at,
            node_role: self.pod_execution_role.clone(),
            taints: vec![NodeTaint {
                key: FARGATE_COMPUTE_TYPE_LABEL.to_string(),
                value: FARGATE_COMPUTE_TYPE.to_string(),
                effect: KUBERNETES_TAINT_EFFECT_NO_SCHEDULE.to_string(),
            }],
            dedicated: true,
        }
    }

    /// This node as the `Node` object a Kubernetes client reads.
    ///
    /// It reports no `providerID`: there is no Amazon EC2 instance behind Fargate
    /// capacity, which is also why it carries no Availability Zone or instance-type
    /// label — a node group's node reports those because EC2 reported them for its
    /// instance ([`super::kubernetes_nodes`]).
    #[must_use]
    pub fn to_json(&self, cluster: &ClusterRecord, region: &str) -> ::serde_json::Value {
        let joined_at = super::kubernetes_objects::rfc3339_seconds(self.joined_at);
        let labels: BTreeMap<&str, &str> = [
            ("kubernetes.io/hostname", self.name.as_str()),
            ("kubernetes.io/os", FARGATE_OPERATING_SYSTEM),
            ("kubernetes.io/arch", FARGATE_ARCHITECTURE),
            ("beta.kubernetes.io/os", FARGATE_OPERATING_SYSTEM),
            ("beta.kubernetes.io/arch", FARGATE_ARCHITECTURE),
            ("topology.kubernetes.io/region", region),
            (FARGATE_COMPUTE_TYPE_LABEL, FARGATE_COMPUTE_TYPE),
            (FARGATE_PROFILE_LABEL, self.profile_name.as_str()),
        ]
        .into_iter()
        .collect();
        let kubelet = super::kubernetes_nodes::kubelet_version(cluster);
        ::serde_json::json!({
            "kind": "Node",
            "apiVersion": super::kubernetes_objects::CORE_API_VERSION,
            "metadata": {
                "name": self.name,
                "uid": derive_uuid_from_seed(&format!("fargate-node/{}", self.name)),
                "creationTimestamp": joined_at,
                "labels": labels,
            },
            "spec": {
                "taints": [{
                    "key": FARGATE_COMPUTE_TYPE_LABEL,
                    "value": FARGATE_COMPUTE_TYPE,
                    "effect": KUBERNETES_TAINT_EFFECT_NO_SCHEDULE,
                }],
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
                "addresses": [{
                    "type": "Hostname",
                    "address": self.name,
                }],
                "nodeInfo": {
                    "architecture": FARGATE_ARCHITECTURE,
                    "operatingSystem": FARGATE_OPERATING_SYSTEM,
                    "kubeletVersion": kubelet,
                    "kubeProxyVersion": kubelet,
                },
            },
        })
    }
}

/// Everywhere one cluster can run a pod: the nodes registered with it, and the
/// Fargate capacity its ACTIVE profiles provide.
///
/// Read once per request from the control plane's own records and handed to the
/// scheduling that follows, so the capacity a create places a pod on, the capacity
/// a clock-owed move places one on, and the capacity a workload counts its
/// replicas against are one reading.
#[derive(Debug, Clone, Default)]
pub struct ClusterCapacity {
    /// Every node registered with the cluster: its node groups' instances, and the
    /// Fargate nodes it has already provisioned for pods.
    pub nodes: Vec<ClusterNode>,
    /// The Fargate profiles that can take a pod they select, in profile-name
    /// order.
    pub fargate: Vec<FargateCapacity>,
}

impl ClusterCapacity {
    /// The nodes that take ANY pod the scheduler places on them: the shared ones,
    /// which is every node but the Fargate capacity provisioned for one pod.
    pub fn shared_nodes(&self) -> impl Iterator<Item = &ClusterNode> {
        self.nodes.iter().filter(|node| !node.dedicated)
    }

    /// How many shared nodes the cluster holds — what a workload's replica count
    /// is read against.
    #[must_use]
    pub fn shared_node_count(&self) -> usize {
        self.shared_nodes().count()
    }

    /// Whether `name` is a node this cluster still holds.
    #[must_use]
    pub fn holds_node(&self, name: &str) -> bool {
        self.nodes.iter().any(|node| node.name == name)
    }

    /// The shared node the scheduler places a pod with these tolerations on: the
    /// node it NAMED when it named one the cluster holds, else the first node that
    /// admits the pod.
    #[must_use]
    pub fn node_for(
        &self,
        requested_node: Option<&str>,
        tolerations: &super::kubernetes_nodes::PodTolerations,
    ) -> Option<&ClusterNode> {
        match requested_node {
            Some(named) => self.shared_nodes().find(|node| node.name == named),
            None => self.shared_nodes().find(|node| node.admits(tolerations)),
        }
    }

    /// The profile whose capacity takes a pod of `namespace` carrying `labels`, and
    /// `None` when none of them selects it.
    ///
    /// The first in profile-name order, so a pod more than one profile selects is
    /// placed by the same one on every call.
    #[must_use]
    pub fn fargate_for(
        &self,
        namespace: &str,
        labels: &BTreeMap<String, String>,
    ) -> Option<&FargateCapacity> {
        self.fargate
            .iter()
            .find(|capacity| capacity.selects(namespace, labels))
    }
}

impl ComAmazonawsEksAwswesleyfrontendState {
    /// The Fargate capacity of the cluster `cluster_name` in `region`, in
    /// profile-name order: one entry per profile that is ACTIVE and is not on its
    /// way out, since a profile the cluster is still creating or is taking off it
    /// carries no pod.
    pub fn fargate_capacity(&self, region: &str, cluster_name: &str) -> Vec<FargateCapacity> {
        self.fargate_profiles_of(region, cluster_name)
            .filter(|profile| {
                profile.removed_at.is_none() && profile.status == FARGATE_PROFILE_STATUS_ACTIVE
            })
            .map(|profile| FargateCapacity {
                profile_name: profile.fargate_profile_name.clone(),
                pod_execution_role: profile.pod_execution_role_arn.clone(),
                selectors: profile
                    .selectors
                    .iter()
                    .filter_map(|selector| {
                        // "Each selector must have an associated Kubernetes
                        // `namespace`": one that names none selects no pod.
                        let namespace = selector
                            .namespace
                            .clone()
                            .filter(|namespace| !namespace.is_empty())?;
                        Some(FargateSelector {
                            namespace,
                            labels: selector
                                .labels
                                .iter()
                                .flatten()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect(),
                        })
                    })
                    .collect(),
                // The instant the profile reached `ACTIVE`, computed from the
                // stored creation instant and the span the activation is owed
                // ([`FARGATE_PROFILE_ACTIVATION_DELAY`]).
                active_since: profile.created_at + FARGATE_PROFILE_ACTIVATION_DELAY,
            })
            .collect()
    }

    /// Everywhere that cluster can run a pod ([`ClusterCapacity`]).
    pub fn cluster_capacity(&self, region: &str, cluster_name: &str) -> ClusterCapacity {
        ClusterCapacity {
            nodes: self.scheduler_nodes(region, cluster_name),
            fargate: self.fargate_capacity(region, cluster_name),
        }
    }
}

/// Provision the Fargate capacity a pod the cluster's profiles select needs, and
/// hand back the node it registered — `None` for a pod no profile selects.
///
/// Takes the pod ITSELF rather than looking it up, because this runs inside the
/// walk that places the cluster's pods; the node is handed back so the caller
/// records it on the cluster once that walk has let the namespaces go.
pub(super) fn fargate_node_for(
    namespace: &str,
    pod_name: &str,
    pod: &PodObject,
    capacity: &ClusterCapacity,
) -> Option<FargateNode> {
    let selected = capacity.fargate_for(namespace, &pod.metadata.labels)?;
    Some(FargateNode {
        name: fargate_node_name(namespace, pod_name),
        namespace: namespace.to_string(),
        pod_name: pod_name.to_string(),
        pod_uid: pod.uid.clone(),
        profile_name: selected.profile_name.clone(),
        pod_execution_role: selected.pod_execution_role.clone(),
        // The capacity was there once BOTH the pod and the profile were, which is
        // the later of the two stored instants.
        joined_at: pod.created_at.max(selected.active_since),
    })
}

impl ClusterObjects {
    /// Release the Fargate capacity this cluster no longer has a pod for: the node
    /// of a pod that is gone (or of a pod created again under another uid), and the
    /// node of a pod no ACTIVE profile selects any more — a profile that was
    /// deleted among them.
    ///
    /// Hands back the names of the nodes it released, so the walk that follows
    /// treats a pod on one of them as a pod waiting for capacity again, exactly as
    /// it treats a pod whose node group's node is gone.
    pub(super) fn release_spent_fargate_capacity(
        &mut self,
        capacity: &ClusterCapacity,
    ) -> BTreeSet<String> {
        let spent: Vec<String> = self
            .fargate_nodes
            .values()
            .filter(|node| {
                let held = self
                    .namespaces
                    .get(&node.namespace)
                    .and_then(|held| held.pods.get(&node.pod_name))
                    .filter(|pod| pod.uid == node.pod_uid);
                match held {
                    None => true,
                    Some(pod) => capacity
                        .fargate_for(&node.namespace, &pod.metadata.labels)
                        .is_none_or(|selected| selected.profile_name != node.profile_name),
                }
            })
            .map(|node| node.name.clone())
            .collect();
        for name in &spent {
            self.fargate_nodes.remove(name);
        }
        spent.into_iter().collect()
    }

    /// The Fargate nodes this cluster holds, as the SCHEDULER reads them.
    pub fn fargate_cluster_nodes(&self) -> Vec<ClusterNode> {
        self.fargate_nodes
            .values()
            .map(FargateNode::as_cluster_node)
            .collect()
    }
}

impl PodObject {
    /// Place this pod on the Fargate capacity `node` was provisioned for it: the
    /// pod is bound to that node, and its `spec.schedulerName` names the scheduler
    /// that placed it, which is the Fargate one rather than the cluster's default
    /// ([`FARGATE_SCHEDULER_NAME`]).
    ///
    /// The member is written through the spec's own map rather than by indexing the
    /// stored document: that document came from a client's manifest, and a member
    /// is added to it only when it is the object a spec is.
    pub fn place_on_fargate(&mut self, node: &FargateNode) {
        self.bind_to(&node.as_cluster_node());
        if let Some(spec) = self.spec.as_object_mut() {
            spec.insert(
                "schedulerName".to_string(),
                ::serde_json::json!(FARGATE_SCHEDULER_NAME),
            );
        }
    }
}
