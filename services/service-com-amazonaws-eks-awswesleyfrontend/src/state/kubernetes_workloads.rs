// SPDX-License-Identifier: CC0-1.0

//! The `apps/v1` WORKLOADS one cluster's API server holds: its Deployments and
//! its DaemonSets.
//!
//! A client that builds a kubeconfig out of `Cluster$endpoint` and
//! `Certificate$data` deploys its work by creating an object in this group — a
//! `Deployment` for a workload that runs a number of replicas anywhere in the
//! cluster, a `DaemonSet` for one that runs a pod on every node — so the group is
//! SERVED, with its discovery documents and its collections, rather than answered
//! as a path the server does not have.
//!
//! A workload here is BACKED by the pods it runs, and they are the pods of the
//! namespace ([`super::kubernetes_objects::NamespaceObject::pods`]): creating a
//! Deployment creates its replicas, a DaemonSet gets a pod on each node the
//! cluster holds as nodes come and go, and what the workload REPORTS in its
//! `status` is counted off those pods rather than echoed back from the request. A
//! client that creates a Deployment and then lists the namespace's pods therefore
//! finds them, and each one is scheduled, pulled and started exactly as a pod a
//! client created itself is ([`super::kubernetes_objects`],
//! [`super::kubernetes_image_pull`]).
//!
//! # The workloads Amazon EKS itself installs
//!
//! An EKS-managed add-on is an in-cluster workload: "Amazon EKS add-ons are
//! curated … Kubernetes operational software", and `DescribeAddon` reporting one
//! `ACTIVE` is the claim that its workload runs in the cluster. So the workloads
//! an installed add-on owns are DERIVED from the add-on records the control plane
//! holds ([`managed_workloads`]) — one source for both answers, so an add-on the
//! control plane reports as installed is an add-on whose Deployment or DaemonSet a
//! client finds in the cluster, at the version the add-on was pinned to, and an
//! add-on that was removed takes its workload with it.

use ::std::collections::{BTreeMap, BTreeSet};
use ::std::time::SystemTime;

use super::kubernetes_lists::{listed, ListRequest};
use super::kubernetes_metadata::ObjectMetadata;
use super::kubernetes_nodes::ClusterNode;
use super::kubernetes_objects::{
    generated_name_suffix, manifest_name, new_object_uid, ApiRefusal, ClusterObjects,
    DEFAULT_SERVICE_ACCOUNT,
};
use super::kubernetes_pod_spec::admitted_pod_spec;
use super::ComAmazonawsEksAwswesleyfrontendState as ServiceStore;

/// The API group the workload objects live in.
pub const APPS_API_GROUP: &str = "apps";

/// The version of that group this API server serves, which is the one every
/// workload object a client sends carries in its `apiVersion`.
pub const APPS_GROUP_VERSION: &str = "v1";

/// `<group>/<version>`, the `apiVersion` every object of the group carries.
pub const APPS_API_VERSION: &str = "apps/v1";

/// The `apiVersion` a discovery document itself carries, which is the core one
/// whatever group it describes.
const DISCOVERY_API_VERSION: &str = "v1";

/// How many workload objects of ONE resource one namespace holds.
///
/// CHOSEN, not published: Kubernetes puts no limit on workloads and a caller can
/// create them without end, so the store needs a bound of its own. It is far above
/// the handful a cluster of this emulator is driven with.
pub const MAX_WORKLOADS_PER_NAMESPACE: usize = 64;

/// How many replicas ONE Deployment of this server runs.
///
/// CHOSEN, not published: Kubernetes bounds `Deployment$spec.replicas` only by the
/// capacity of the cluster, and every replica of a workload here is a POD in this
/// store — so a create asking for a million replicas would otherwise ask this
/// service to hold a million objects. A request past it is refused rather than
/// served with fewer replicas than it asked for, since a client that asked for a
/// count and was given another would read a workload that is not the one it
/// created. It is far above the handful a cluster of this emulator is driven with,
/// and well under [`super::kubernetes_objects::MAX_PODS_PER_NAMESPACE`].
pub const MAX_REPLICAS_PER_WORKLOAD: u64 = 32;

/// The number of replicas a Deployment that names none runs: "Replicas is the
/// number of desired pods … Defaults to 1".
const DEFAULT_REPLICAS: u64 = 1;

/// The label Kubernetes puts on the pods of one revision of a Deployment, which is
/// what tells the pods of one template apart from the pods of the next.
const POD_TEMPLATE_HASH_LABEL: &str = "pod-template-hash";

/// How many characters of the template's derived hash that label carries, which is
/// the length Kubernetes writes it in.
const POD_TEMPLATE_HASH_LEN: usize = 10;

/// The two resources the group serves.
///
/// Both live in a namespace. The discriminant is part of the key the objects are
/// stored under, so one map holds a namespace's workloads of every resource and a
/// listing walks one resource of it in name order — the way the RBAC objects are
/// held ([`super::kubernetes_rbac::RbacScope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkloadResource {
    DaemonSets,
    Deployments,
}

impl WorkloadResource {
    /// The resource this path segment addresses, or `None` for a segment the group
    /// does not serve.
    #[must_use]
    pub fn from_segment(segment: &str) -> Option<Self> {
        match segment {
            "daemonsets" => Some(Self::DaemonSets),
            "deployments" => Some(Self::Deployments),
            _ => None,
        }
    }

    /// The plural name the resource is addressed by, which is also the name an RBAC
    /// rule names it with.
    #[must_use]
    pub fn resource(self) -> &'static str {
        match self {
            Self::DaemonSets => "daemonsets",
            Self::Deployments => "deployments",
        }
    }

    /// The `kind` an object of this resource carries.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::DaemonSets => "DaemonSet",
            Self::Deployments => "Deployment",
        }
    }

    /// The `kind` of the LIST a collection read is answered with.
    #[must_use]
    fn list_kind(self) -> &'static str {
        match self {
            Self::DaemonSets => "DaemonSetList",
            Self::Deployments => "DeploymentList",
        }
    }

    /// The singular name discovery reports for the resource.
    #[must_use]
    fn singular(self) -> &'static str {
        match self {
            Self::DaemonSets => "daemonset",
            Self::Deployments => "deployment",
        }
    }

    /// The short name a client may address the resource by.
    #[must_use]
    fn short_name(self) -> &'static str {
        match self {
            Self::DaemonSets => "ds",
            Self::Deployments => "deploy",
        }
    }

    /// Whether a workload of this resource runs ONE pod per node rather than a
    /// number of replicas the client asked for: "a DaemonSet ensures that all (or
    /// some) nodes run a copy of a pod".
    #[must_use]
    fn runs_on_every_node(self) -> bool {
        matches!(self, Self::DaemonSets)
    }

    /// Every resource the group serves, in the order discovery reports them.
    #[must_use]
    fn all() -> [Self; 2] {
        [Self::DaemonSets, Self::Deployments]
    }
}

/// The workloads of ONE namespace, keyed by resource and name.
pub type WorkloadScope = BTreeMap<(WorkloadResource, String), WorkloadObject>;

/// One workload object this cluster holds.
#[derive(Debug)]
pub struct WorkloadObject {
    pub uid: String,
    pub created_at: SystemTime,
    /// The labels and annotations the manifest carried.
    pub metadata: ObjectMetadata,
    /// The workload's `spec` AS ADMITTED: the one the manifest carried, with the
    /// pod template validated the way a pod's own create validates it and the
    /// replica count the server resolved.
    ///
    /// Held whole rather than field by field, because a client reads a workload
    /// back to see what the cluster admitted — its selector, its template and the
    /// containers in it — and the spec a read reports is the spec the create
    /// recorded.
    pub spec: ::serde_json::Value,
    /// The pods this workload runs, by name: the replicas it created, which are
    /// pods of the same namespace. What the workload reports in its `status` is
    /// counted off these.
    pub pods: BTreeSet<String>,
    /// The Amazon EKS add-on that owns this workload, for a workload the control
    /// plane installed ([`managed_workloads`]), and `None` for one a client
    /// created. A managed workload follows its add-on: it is installed with it,
    /// re-rolled when the add-on's version changes and removed with it.
    pub managed_by: Option<String>,
}

impl WorkloadObject {
    /// The pod template this workload runs, which is where its containers live.
    #[must_use]
    fn template(&self) -> &::serde_json::Value {
        static EMPTY: ::std::sync::OnceLock<::serde_json::Value> = ::std::sync::OnceLock::new();
        self.spec
            .get("template")
            .unwrap_or_else(|| EMPTY.get_or_init(|| ::serde_json::json!({})))
    }

    /// How many replicas the workload is to run on a cluster holding `nodes`: the
    /// count the spec asked for, and one per node for a DaemonSet — "the number of
    /// nodes that should be running the daemon pod".
    #[must_use]
    fn desired_replicas(&self, resource: WorkloadResource, nodes: usize) -> usize {
        if resource.runs_on_every_node() {
            return nodes;
        }
        usize::try_from(
            self.spec
                .get("replicas")
                .and_then(::serde_json::Value::as_u64)
                .unwrap_or(DEFAULT_REPLICAS),
        )
        .unwrap_or(usize::MAX)
    }
}

/// One `GroupVersionForDiscovery` of this group.
fn group_version() -> ::serde_json::Value {
    ::serde_json::json!({
        "groupVersion": APPS_API_VERSION,
        "version": APPS_GROUP_VERSION,
    })
}

/// The group's entry in the `APIGroupList` served at `/apis`.
#[must_use]
pub fn api_group_entry() -> ::serde_json::Value {
    ::serde_json::json!({
        "name": APPS_API_GROUP,
        "versions": [group_version()],
        "preferredVersion": group_version(),
    })
}

/// The `APIGroup` discovery document of the group, which a client reads before it
/// addresses it.
#[must_use]
pub fn api_group_document() -> ::serde_json::Value {
    ::serde_json::json!({
        "kind": "APIGroup",
        "apiVersion": DISCOVERY_API_VERSION,
        "name": APPS_API_GROUP,
        "versions": [group_version()],
        "preferredVersion": group_version(),
    })
}

/// The `APIResourceList` of `apps/v1`: the resources this server serves in the
/// group, the verbs it serves on each and the short names a client may address them
/// by.
///
/// A Kubernetes client resolves the kind it was asked for against THIS document
/// before it addresses anything, so the list names exactly the resources the
/// routing serves and no others.
#[must_use]
pub fn api_resource_list_document() -> ::serde_json::Value {
    let resources: Vec<::serde_json::Value> = WorkloadResource::all()
        .into_iter()
        .map(|resource| {
            ::serde_json::json!({
                "name": resource.resource(),
                "singularName": resource.singular(),
                "namespaced": true,
                "kind": resource.kind(),
                "shortNames": [resource.short_name()],
                "verbs": ["create", "delete", "get", "list", "update"],
            })
        })
        .collect();
    ::serde_json::json!({
        "kind": "APIResourceList",
        "apiVersion": DISCOVERY_API_VERSION,
        "groupVersion": APPS_API_VERSION,
        "resources": resources,
    })
}

/// The `spec` of a workload AS THE API SERVER ADMITS IT: the one the manifest
/// carried, with its pod template validated the way a pod's own create validates
/// one and its replica count resolved and bounded.
///
/// A workload with no pod template is a workload with no work, and a template whose
/// pod spec the API server would refuse is refused here too — the template is
/// validated by the pod admission this server already has
/// ([`admitted_pod_spec`]), so a template that is admitted is one whose pods can be
/// created.
fn admitted_workload_spec(
    resource: WorkloadResource,
    name: &str,
    manifest: &::serde_json::Value,
) -> Result<::serde_json::Value, ApiRefusal> {
    let invalid = |detail: &str| {
        ApiRefusal::invalid(format!(
            "{} \"{name}\" is invalid: spec.{detail}",
            resource.kind()
        ))
    };
    let spec = manifest
        .get("spec")
        .and_then(::serde_json::Value::as_object)
        .ok_or_else(|| invalid("template: Required value"))?;
    let mut admitted = ::serde_json::Map::new();
    let template = spec
        .get("template")
        .filter(|template| template.is_object())
        .ok_or_else(|| invalid("template: Required value"))?;
    // The template's pod spec, validated and defaulted exactly as the create of a
    // pod of its own would be: a template with no container list is refused. Its
    // `metadata` is the map every object's metadata is, so a template carrying
    // anything else there is refused rather than stored and read as empty.
    let mut template = template.clone();
    if template
        .get("metadata")
        .is_some_and(|metadata| !metadata.is_null() && !metadata.is_object())
    {
        return Err(invalid("template.metadata: Invalid value: must be a map"));
    }
    let service_account = template
        .get("spec")
        .and_then(|pod| pod.get("serviceAccountName"))
        .and_then(::serde_json::Value::as_str)
        .filter(|account| !account.is_empty())
        .unwrap_or(DEFAULT_SERVICE_ACCOUNT)
        .to_string();
    template["spec"] = admitted_pod_spec(name, &template, &service_account)?;
    if let Some(selector) = spec.get("selector") {
        admitted.insert("selector".to_string(), selector.clone());
    }
    if !resource.runs_on_every_node() {
        let replicas = match spec.get("replicas") {
            None | Some(::serde_json::Value::Null) => DEFAULT_REPLICAS,
            Some(asked) => asked
                .as_u64()
                .filter(|asked| *asked <= MAX_REPLICAS_PER_WORKLOAD)
                .ok_or_else(|| {
                    invalid(&format!(
                        "replicas: Invalid value: must be a count of at most \
                         {MAX_REPLICAS_PER_WORKLOAD}"
                    ))
                })?,
        };
        admitted.insert("replicas".to_string(), ::serde_json::json!(replicas));
    }
    admitted.insert("template".to_string(), template);
    Ok(::serde_json::Value::Object(admitted))
}

/// The hash of one pod template: the value the `pod-template-hash` label of the
/// pods of that template carries, and the part of their name that tells one
/// revision's pods from the next one's.
///
/// DERIVED from the template itself through the one stable mixer of this workspace
/// (`service_common::stable_hex_id`), so the same template always names the same
/// revision and a changed template names a new one.
fn pod_template_hash(template: &::serde_json::Value) -> String {
    ::service_common::stable_hex_id(&["pod-template", &template.to_string()])
        .chars()
        .take(POD_TEMPLATE_HASH_LEN)
        .collect()
}

impl ClusterObjects {
    /// `POST /apis/apps/v1/namespaces/{namespace}/{resource}`: create the workload
    /// the manifest names, and the pods it runs.
    ///
    /// The pods are created through the same admission every other pod of this
    /// cluster goes through, so they are scheduled onto the cluster's nodes, pull
    /// their images and reach their phases exactly as a client's own pod does.
    pub fn create_workload(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        manifest: &::serde_json::Value,
        now: SystemTime,
        nodes: &[ClusterNode],
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let name = manifest_name(manifest, resource.kind())?;
        self.namespace(namespace)?;
        let spec = admitted_workload_spec(resource, &name, manifest)?;
        let metadata = ObjectMetadata::from_manifest(resource.kind(), &name, manifest)?;
        let held = self
            .namespaces
            .get_mut(namespace)
            .ok_or_else(|| ApiRefusal::not_found("namespaces", namespace))?;
        if held.workloads.contains_key(&(resource, name.clone())) {
            return Err(ApiRefusal::already_exists(resource.resource(), &name));
        }
        if held
            .workloads
            .keys()
            .filter(|(held_resource, _)| *held_resource == resource)
            .count()
            >= MAX_WORKLOADS_PER_NAMESPACE
        {
            return Err(ApiRefusal::too_many(
                resource.resource(),
                MAX_WORKLOADS_PER_NAMESPACE,
            ));
        }
        held.workloads.insert(
            (resource, name.clone()),
            WorkloadObject {
                uid: new_object_uid(),
                created_at: now,
                metadata,
                spec,
                pods: BTreeSet::new(),
                managed_by: None,
            },
        );
        self.run_workload_replicas(resource, namespace, &name, now, nodes);
        self.workload_json(resource, namespace, &name, nodes)
    }

    /// `GET /apis/apps/v1/namespaces/{namespace}/{resource}/{name}`.
    pub fn workload_json(
        &self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
        nodes: &[ClusterNode],
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let held = self.namespace(namespace)?;
        let workload = held
            .workloads
            .get(&(resource, name.to_string()))
            .ok_or_else(|| ApiRefusal::not_found(resource.resource(), name))?;
        // What the workload RUNS, counted off the pods of this namespace: a pod
        // the store no longer holds is not a replica, and a replica is available
        // only while its containers report ready.
        let running: Vec<&super::kubernetes_objects::PodObject> = workload
            .pods
            .iter()
            .filter_map(|pod| held.pods.get(pod))
            .collect();
        let ready = running.iter().filter(|pod| pod.is_ready()).count();
        let desired = workload.desired_replicas(resource, nodes.len());
        Ok(::serde_json::json!({
            "kind": resource.kind(),
            "apiVersion": APPS_API_VERSION,
            "metadata": workload.metadata.render(
                name,
                Some(namespace),
                &workload.uid,
                workload.created_at,
            ),
            "spec": workload.spec,
            "status": workload_status(resource, desired, running.len(), ready),
        }))
    }

    /// `GET /apis/apps/v1/namespaces/{namespace}/{resource}`: the namespace's
    /// workloads of that resource, as the `<Kind>List` a client reads — one chunk of
    /// it ([`super::kubernetes_lists`]).
    pub fn workload_list_json(
        &self,
        resource: WorkloadResource,
        namespace: &str,
        request: &ListRequest,
        nodes: &[ClusterNode],
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let names: Vec<String> = self
            .namespace(namespace)?
            .workloads
            .keys()
            .filter(|(held, _)| *held == resource)
            .map(|(_, name)| name.clone())
            .collect();
        Ok(listed(resource.list_kind(), &names, request, |name| {
            self.workload_json(resource, namespace, name, nodes).ok()
        }))
    }

    /// `DELETE /apis/apps/v1/namespaces/{namespace}/{resource}/{name}`: remove the
    /// workload and the pods it runs, which is what deleting a workload does to
    /// them.
    pub fn delete_workload(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
        nodes: &[ClusterNode],
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let deleted = self.workload_json(resource, namespace, name, nodes)?;
        self.remove_workload(resource, namespace, name);
        Ok(deleted)
    }

    /// `PUT /apis/apps/v1/namespaces/{namespace}/{resource}/{name}`: replace the
    /// workload's spec, which is how a client scales it or rolls a new template out.
    ///
    /// A replaced template is a new revision, so the pods of the old one are
    /// replaced by pods of the new one; a replaced replica count is converged to by
    /// creating or removing pods of the same revision.
    pub fn replace_workload(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
        manifest: &::serde_json::Value,
        now: SystemTime,
        nodes: &[ClusterNode],
    ) -> Result<::serde_json::Value, ApiRefusal> {
        let spec = admitted_workload_spec(resource, name, manifest)?;
        let metadata = ObjectMetadata::from_manifest(resource.kind(), name, manifest)?;
        let key = (resource, name.to_string());
        let rolled = {
            let held = self
                .namespaces
                .get_mut(namespace)
                .ok_or_else(|| ApiRefusal::not_found("namespaces", namespace))?;
            let workload = held
                .workloads
                .get_mut(&key)
                .ok_or_else(|| ApiRefusal::not_found(resource.resource(), name))?;
            let rolled = workload.template() != spec.get("template").unwrap_or(&spec);
            workload.spec = spec;
            workload.metadata = metadata;
            rolled
        };
        if rolled {
            self.retire_workload_pods(resource, namespace, name);
        }
        self.run_workload_replicas(resource, namespace, name, now, nodes);
        self.workload_json(resource, namespace, name, nodes)
    }

    /// Bring the pods one workload runs up to the number it is to run: create the
    /// replicas it is short of, and remove the ones it no longer runs.
    ///
    /// A DaemonSet is converged against the NODES — one pod per node the cluster
    /// holds, so a pod on a node the cluster no longer holds is removed and a node
    /// with no pod of it gets one — and a Deployment against its replica count.
    fn run_workload_replicas(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
        now: SystemTime,
        nodes: &[ClusterNode],
    ) {
        let key = (resource, name.to_string());
        let Some(held) = self.namespaces.get(namespace) else {
            return;
        };
        let Some(workload) = held.workloads.get(&key) else {
            return;
        };
        let template = workload.template().clone();
        let hash = pod_template_hash(&template);
        // The pods it already runs, and which node each of them is on.
        let running: Vec<(String, Option<String>)> = workload
            .pods
            .iter()
            .filter_map(|pod| {
                held.pods
                    .get(pod)
                    .map(|held| (pod.clone(), held.node_name.clone()))
            })
            .collect();
        let mut create: Vec<(String, Option<String>)> = Vec::new();
        let mut remove: Vec<String> = Vec::new();
        if resource.runs_on_every_node() {
            for node in nodes {
                if !running
                    .iter()
                    .any(|(_, on)| on.as_deref() == Some(node.name.as_str()))
                {
                    create.push((
                        format!("{name}-{}", generated_name_suffix()),
                        Some(node.name.clone()),
                    ));
                }
            }
            // A pod of a node the cluster no longer holds is one the daemon set no
            // longer runs: there is no node for it to be a copy on.
            let gone: Vec<String> = running
                .iter()
                .filter(|(_, on)| {
                    on.as_deref()
                        .is_none_or(|on| !nodes.iter().any(|node| node.name == on))
                })
                .map(|(pod, _)| pod.clone())
                .collect();
            remove.extend(gone);
        } else {
            let desired = workload.desired_replicas(resource, nodes.len());
            for _ in running.len()..desired {
                create.push((format!("{name}-{hash}-{}", generated_name_suffix()), None));
            }
            for (pod, _) in running.iter().skip(desired) {
                remove.push(pod.clone());
            }
        }
        for pod in remove {
            self.remove_workload_pod(resource, namespace, name, &pod);
        }
        for (pod, node) in create {
            let manifest = replica_manifest(resource, &pod, &hash, node.as_deref(), &template);
            // A replica the namespace cannot take — its service account is gone,
            // the namespace is full — is not created, and the workload reports the
            // replicas it does run rather than a count nothing backs.
            if self.create_pod(namespace, &manifest, now, nodes).is_err() {
                continue;
            }
            if let Some(workload) = self
                .namespaces
                .get_mut(namespace)
                .and_then(|held| held.workloads.get_mut(&(resource, name.to_string())))
            {
                workload.pods.insert(pod);
            }
        }
    }

    /// Remove every pod of one workload, which is what rolling a new revision out
    /// does to the pods of the old one.
    fn retire_workload_pods(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
    ) {
        let pods: Vec<String> = self
            .namespaces
            .get(namespace)
            .and_then(|held| held.workloads.get(&(resource, name.to_string())))
            .map(|workload| workload.pods.iter().cloned().collect())
            .unwrap_or_default();
        for pod in pods {
            self.remove_workload_pod(resource, namespace, name, &pod);
        }
    }

    /// Remove one pod of one workload: the pod leaves the namespace and the
    /// workload no longer runs it.
    fn remove_workload_pod(
        &mut self,
        resource: WorkloadResource,
        namespace: &str,
        name: &str,
        pod: &str,
    ) {
        if let Some(held) = self.namespaces.get_mut(namespace) {
            held.pods.remove(pod);
            if let Some(workload) = held.workloads.get_mut(&(resource, name.to_string())) {
                workload.pods.remove(pod);
            }
        }
    }

    /// Remove one workload and the pods it runs.
    fn remove_workload(&mut self, resource: WorkloadResource, namespace: &str, name: &str) {
        self.retire_workload_pods(resource, namespace, name);
        if let Some(held) = self.namespaces.get_mut(namespace) {
            held.workloads.remove(&(resource, name.to_string()));
        }
    }

    /// Bring the pods of EVERY workload this cluster holds in line with what that
    /// workload is to run: the replicas a Deployment asked for, and one copy per
    /// node for a DaemonSet.
    ///
    /// This is controller work the cluster owes continuously rather than a write a
    /// request makes — a DaemonSet gets a copy on a node that has just joined, and
    /// loses the copy of a node that is gone, whether or not anybody called — so it
    /// runs before any workload is reported and leaves every caller reading the
    /// same cluster.
    pub fn reconcile_workloads(&mut self, now: SystemTime, nodes: &[ClusterNode]) {
        let held: Vec<(String, WorkloadResource, String)> = self
            .namespaces
            .iter()
            .flat_map(|(namespace, held)| {
                held.workloads
                    .keys()
                    .map(move |(resource, name)| (namespace.clone(), *resource, name.clone()))
            })
            .collect();
        for (namespace, resource, name) in held {
            self.run_workload_replicas(resource, &namespace, &name, now, nodes);
        }
    }

    /// Materialise the workloads the cluster's installed add-ons own, as
    /// `managed` names them: install the ones that are missing, re-roll one whose
    /// add-on moved to another version, and remove the workload of an add-on the
    /// cluster no longer holds. The pods each of them runs are converged by
    /// [`Self::reconcile_workloads`], like every other workload's.
    ///
    /// This is controller work the cluster owes continuously rather than a write a
    /// request makes: what it converges to is decided by the add-on records the
    /// control plane already holds, so it was owed at this instant whether or not
    /// anybody called, and two callers read the same cluster.
    pub fn materialize_managed_workloads(&mut self, managed: &[ManagedWorkload], now: SystemTime) {
        // The managed workloads this cluster holds that `managed` no longer names:
        // their add-on is gone, so the workload goes with it.
        let stale: Vec<(String, WorkloadResource, String)> = self
            .namespaces
            .iter()
            .flat_map(|(namespace, held)| {
                held.workloads
                    .iter()
                    .filter_map(move |((resource, name), workload)| {
                        let owner = workload.managed_by.as_deref()?;
                        let still_owned = managed.iter().any(|workload| {
                            workload.addon == owner
                                && workload.resource == *resource
                                && workload.name == *name
                                && workload.namespace == *namespace
                        });
                        (!still_owned).then(|| (namespace.clone(), *resource, name.clone()))
                    })
            })
            .collect();
        for (namespace, resource, name) in stale {
            self.remove_workload(resource, &namespace, &name);
        }
        for workload in managed {
            // An add-on is installed into a namespace of the cluster; one whose
            // namespace the cluster does not hold has nowhere to run.
            if !self.namespaces.contains_key(&workload.namespace) {
                continue;
            }
            let key = (workload.resource, workload.name.clone());
            let spec = workload.spec();
            let rolled = match self
                .namespaces
                .get_mut(&workload.namespace)
                .and_then(|held| held.workloads.get_mut(&key))
            {
                Some(held) => {
                    let rolled = held.spec != spec;
                    held.spec = spec;
                    rolled
                }
                None => {
                    let Some(held) = self.namespaces.get_mut(&workload.namespace) else {
                        continue;
                    };
                    held.workloads.insert(
                        key,
                        WorkloadObject {
                            uid: new_object_uid(),
                            created_at: now,
                            metadata: workload.object_metadata(),
                            spec,
                            pods: BTreeSet::new(),
                            managed_by: Some(workload.addon.clone()),
                        },
                    );
                    false
                }
            };
            if rolled {
                self.retire_workload_pods(workload.resource, &workload.namespace, &workload.name);
            }
        }
    }
}

/// The manifest one replica of a workload is created from: the workload's own pod
/// template, named and labelled as the pod of that revision.
///
/// `node` is the node a DaemonSet's copy runs on, which the pod names so the
/// cluster binds it there rather than letting the scheduler choose.
///
/// Every member is built out of the MAP the template holds there, so a template
/// whose `metadata` or `spec` is not an object — which no stored template is, since
/// the create admitted both — yields a manifest with that member empty rather than
/// an index into a value that is not a map.
fn replica_manifest(
    resource: WorkloadResource,
    pod: &str,
    template_hash: &str,
    node: Option<&str>,
    template: &::serde_json::Value,
) -> ::serde_json::Value {
    let member = |name: &str| -> ::serde_json::Map<String, ::serde_json::Value> {
        template
            .get(name)
            .and_then(::serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let mut metadata = member("metadata");
    metadata.insert("name".to_string(), ::serde_json::json!(pod));
    // The revision the pod is of, which is what tells one Deployment revision's
    // pods from the next one's. A DaemonSet's copies are told apart by their node
    // rather than by a revision label.
    if !resource.runs_on_every_node() {
        let mut labels = metadata
            .get("labels")
            .and_then(::serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        labels.insert(
            POD_TEMPLATE_HASH_LABEL.to_string(),
            ::serde_json::json!(template_hash),
        );
        metadata.insert("labels".to_string(), ::serde_json::Value::Object(labels));
    }
    let mut spec = member("spec");
    if let Some(node) = node {
        spec.insert("nodeName".to_string(), ::serde_json::json!(node));
    }
    ::serde_json::json!({ "metadata": metadata, "spec": spec })
}

/// The `status` a workload reports: how many replicas it is to run, how many it
/// does run, and how many of those are available.
///
/// Every number is COUNTED off the pods the workload runs, so a workload reports
/// what the cluster holds for it rather than what its create asked for.
fn workload_status(
    resource: WorkloadResource,
    desired: usize,
    running: usize,
    ready: usize,
) -> ::serde_json::Value {
    let unavailable = desired.saturating_sub(ready);
    // "ObservedGeneration is the most recent generation observed": a workload of
    // this server is converged by the call that changed it, so what the controller
    // has observed is always the spec the store holds.
    let observed = ::serde_json::json!(1);
    match resource {
        WorkloadResource::DaemonSets => ::serde_json::json!({
            "observedGeneration": observed,
            "desiredNumberScheduled": desired,
            "currentNumberScheduled": running,
            "updatedNumberScheduled": running,
            "numberReady": ready,
            "numberAvailable": ready,
            "numberUnavailable": unavailable,
            "numberMisscheduled": 0,
        }),
        WorkloadResource::Deployments => ::serde_json::json!({
            "observedGeneration": observed,
            "replicas": running,
            "updatedReplicas": running,
            "readyReplicas": ready,
            "availableReplicas": ready,
            "unavailableReplicas": unavailable,
            "conditions": [{
                "type": "Available",
                "status": if ready >= desired && desired > 0 { "True" } else { "False" },
                "reason": if ready >= desired && desired > 0 {
                    "MinimumReplicasAvailable"
                } else {
                    "MinimumReplicasUnavailable"
                },
            }],
        }),
    }
}

/// One workload an installed Amazon EKS add-on owns.
#[derive(Debug, Clone)]
pub struct ManagedWorkload {
    pub resource: WorkloadResource,
    /// The namespace the add-on was installed into.
    pub namespace: String,
    /// The name of the workload the add-on installs.
    pub name: String,
    /// The container image that workload runs: the add-on's image at the version
    /// the add-on is pinned to.
    pub image: String,
    /// How many replicas it runs, for a workload that is not one per node.
    pub replicas: u64,
    /// The add-on that owns it.
    pub addon: String,
}

impl ManagedWorkload {
    /// The spec this workload runs: one container, running the add-on's image,
    /// selected and labelled by the add-on's own name — which is how the add-on's
    /// workload is labelled in a real cluster.
    #[must_use]
    fn spec(&self) -> ::serde_json::Value {
        let labels = ::serde_json::json!({ "k8s-app": self.name });
        let mut spec = ::serde_json::json!({
            "selector": { "matchLabels": labels },
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "containers": [{ "name": self.name, "image": self.image }],
                    "serviceAccountName": DEFAULT_SERVICE_ACCOUNT,
                },
            },
        });
        if !self.resource.runs_on_every_node() {
            spec["replicas"] = ::serde_json::json!(self.replicas);
        }
        spec
    }

    /// The `metadata` the managed workload carries: the labels a real cluster's
    /// add-on workload carries, naming the add-on that manages it.
    #[must_use]
    fn object_metadata(&self) -> ObjectMetadata {
        ObjectMetadata {
            labels: [
                ("k8s-app".to_string(), self.name.clone()),
                (
                    "app.kubernetes.io/managed-by".to_string(),
                    "eks".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            annotations: BTreeMap::new(),
        }
    }
}

/// One workload an EKS-managed add-on installs, as the table below states it.
#[derive(Debug, Clone, Copy)]
struct AddonWorkloadRow {
    resource: WorkloadResource,
    /// The name the workload is installed under, which is the name a client reads
    /// it back by.
    name: &'static str,
    /// The image repository it runs out of the Amazon EKS add-on registry.
    repository: &'static str,
    /// How many replicas it runs, for a workload that is not one per node.
    replicas: u64,
}

/// One row of [`MANAGED_ADDON_WORKLOADS`], written as the table reads.
const fn row(
    resource: WorkloadResource,
    name: &'static str,
    repository: &'static str,
    replicas: u64,
) -> AddonWorkloadRow {
    AddonWorkloadRow {
        resource,
        name,
        repository,
        replicas,
    }
}

/// The in-cluster workloads each EKS-managed add-on installs: the add-on's name,
/// and one row per workload.
///
/// Taken from what Amazon EKS documents each add-on as installing (the Deployment
/// or DaemonSet a cluster runs the add-on as, under the name a client reads it back
/// by), which is also what `kubectl -n kube-system get deployments,daemonsets`
/// reports on a real cluster. An add-on this table does not carry installs no
/// workload here: a workload named for it would be a claim about objects nothing
/// states.
const MANAGED_ADDON_WORKLOADS: &[(&str, &[AddonWorkloadRow])] = &[
    (
        "vpc-cni",
        &[row(
            WorkloadResource::DaemonSets,
            "aws-node",
            "amazon-k8s-cni",
            0,
        )],
    ),
    (
        "kube-proxy",
        &[row(
            WorkloadResource::DaemonSets,
            "kube-proxy",
            "kube-proxy",
            0,
        )],
    ),
    (
        "coredns",
        &[row(WorkloadResource::Deployments, "coredns", "coredns", 2)],
    ),
    (
        "eks-pod-identity-agent",
        &[row(
            WorkloadResource::DaemonSets,
            "eks-pod-identity-agent",
            "eks-pod-identity-agent",
            0,
        )],
    ),
    (
        "aws-ebs-csi-driver",
        &[
            row(
                WorkloadResource::Deployments,
                "ebs-csi-controller",
                "aws-ebs-csi-driver",
                2,
            ),
            row(
                WorkloadResource::DaemonSets,
                "ebs-csi-node",
                "aws-ebs-csi-driver",
                0,
            ),
        ],
    ),
    (
        "aws-efs-csi-driver",
        &[
            row(
                WorkloadResource::Deployments,
                "efs-csi-controller",
                "aws-efs-csi-driver",
                2,
            ),
            row(
                WorkloadResource::DaemonSets,
                "efs-csi-node",
                "aws-efs-csi-driver",
                0,
            ),
        ],
    ),
    (
        "aws-mountpoint-s3-csi-driver",
        &[row(
            WorkloadResource::DaemonSets,
            "s3-csi-node",
            "aws-mountpoint-s3-csi-driver",
            0,
        )],
    ),
    (
        "snapshot-controller",
        &[row(
            WorkloadResource::Deployments,
            "snapshot-controller",
            "csi-snapshotter",
            2,
        )],
    ),
];

/// The account Amazon EKS publishes its add-on images under, which is the registry
/// a cluster's nodes pull an EKS-managed add-on's image from.
///
/// It is AWS's own registry account rather than this account's, which is why an
/// add-on's image reference names it: a reference naming this account's registry
/// would claim the account holds a repository it does not.
const ADDON_REGISTRY_ACCOUNT: &str = "602401143452";

/// The workloads the add-ons the cluster `cluster_name` holds own at `now`.
///
/// An add-on owns its workloads from the instant its install is applied in the
/// cluster — the instant the clock owes it its terminal status
/// ([`super::AddonRecord::active_at`]) — while that status is one in which its
/// resources ARE applied: `ACTIVE`, and `DEGRADED`, which is the add-on whose
/// resources were applied but which is not healthy. An install that never came up
/// applied nothing, and an add-on whose removal is under way no longer owns them.
#[must_use]
pub fn managed_workloads(
    store: &ServiceStore,
    region: &str,
    cluster_name: &str,
    now: SystemTime,
) -> Vec<ManagedWorkload> {
    let mut workloads = Vec::new();
    for ((addon_region, addon_cluster, addon_name), addon) in &store.addons {
        if addon_region != region || addon_cluster != cluster_name {
            continue;
        }
        if addon.removed_at.is_some() {
            continue;
        }
        // The status the add-on has reached at `now`: the one it carries, or the
        // one its install settles in once the clock has reached the instant that
        // install was owed.
        let status = match addon.active_at {
            Some(due) if now < due => continue,
            Some(_) => addon.install_outcome.status.as_str(),
            None => addon.status.as_str(),
        };
        if !matches!(
            status,
            super::ADDON_STATUS_ACTIVE | super::ADDON_STATUS_DEGRADED
        ) {
            continue;
        }
        let Some((_, rows)) = MANAGED_ADDON_WORKLOADS
            .iter()
            .find(|(name, _)| *name == addon_name)
        else {
            continue;
        };
        for installed in *rows {
            workloads.push(ManagedWorkload {
                resource: installed.resource,
                namespace: addon.namespace.clone(),
                name: installed.name.to_string(),
                image: addon_image(installed.repository, &addon.addon_version, region),
                replicas: installed.replicas,
                addon: addon_name.clone(),
            });
        }
    }
    workloads
}

/// The image an add-on's workload runs: the add-on's repository in the Amazon EKS
/// add-on registry, at the version the add-on is pinned to.
///
/// The registry host is composed by Amazon ECR's own composer of its published
/// spelling, so the reference a pod carries is one that registry reads
/// ([`super::kubernetes_image_pull`]) rather than a host written here. The TAG is
/// the add-on's own version, which is what makes the image a client reads off the
/// workload the version the add-on was pinned to.
#[must_use]
pub fn addon_image(repository: &str, addon_version: &str, region: &str) -> String {
    use ::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state as ecr_store;

    format!(
        "{}/eks/{repository}:{addon_version}",
        ecr_store::registry_host(ADDON_REGISTRY_ACCOUNT, region)
    )
}
