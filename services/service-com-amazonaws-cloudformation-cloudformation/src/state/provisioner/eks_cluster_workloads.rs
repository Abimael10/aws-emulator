// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the two Amazon EKS workloads a stack declares ON a
//! cluster: the add-on an `AWS::EKS::Addon` installs and the managed node group an
//! `AWS::EKS::Nodegroup` creates.
//!
//! Both are CHILDREN of a cluster: every operation of that service on one addresses it by
//! the cluster that holds it and the resource's own name, which are the two `@httpLabel`s
//! of its read and its delete (`smithy_query(query="operation",
//! operation="CreateAddon,DescribeAddon,DeleteAddon,CreateNodegroup,DescribeNodegroup,\
//! DeleteNodegroup")` on that service's model). The name is the resource's physical id and
//! the cluster is a property of its declaration, which is where both halves are read from
//! here ([`EksChildIdentity`]).
//!
//! Each create is ASYNCHRONOUS — it answers the resource `CREATING`, in which no add-on
//! serves the cluster and no node has joined it — so the resource's creation is held open
//! until Amazon EKS reports it `ACTIVE`, and a resource that service settles in any other
//! state FAILS rather than being reported as created.

use super::*;

/// The URI prefix a cluster's add-ons hang off: `POST /clusters/{clusterName}/addons`
/// installs one and `GET`/`DELETE /clusters/{clusterName}/addons/{addonName}` reads and
/// removes it.
pub(crate) const EKS_ADDONS_PATH: &str = "addons";

/// The URI prefix a cluster's managed node groups hang off:
/// `POST /clusters/{clusterName}/node-groups` creates one and
/// `GET`/`DELETE /clusters/{clusterName}/node-groups/{nodegroupName}` reads and removes it.
pub(crate) const EKS_NODE_GROUPS_PATH: &str = "node-groups";

/// The `AddonStatus` an installed add-on is usable in: `DescribeAddon` reports an add-on
/// whose Kubernetes resources are running `ACTIVE`.
pub(crate) const EKS_ADDON_ACTIVE: &str = "ACTIVE";

/// The `AddonStatus` a created add-on starts in, which that create answers with: Amazon EKS
/// is still applying the add-on's Kubernetes resources to the cluster.
pub(crate) const EKS_ADDON_CREATING: &str = "CREATING";

/// The `NodegroupStatus` a managed node group is usable in: "the current status of the
/// managed node group" is `ACTIVE` once its nodes have joined the cluster.
pub(crate) const EKS_NODEGROUP_ACTIVE: &str = "ACTIVE";

/// The `NodegroupStatus` a created node group starts in, which that create answers with:
/// Amazon EKS is still bringing the group's Auto Scaling group and its instances up.
pub(crate) const EKS_NODEGROUP_CREATING: &str = "CREATING";

/// The property both types name the cluster they live on in, which is also the
/// `@httpLabel` of every operation of that service on one of these resources.
pub(crate) const EKS_CLUSTER_NAME_PROPERTY: &str = "ClusterName";

/// The `AWS::EKS::Addon` properties `CreateAddon` takes under its own member name, as
/// (property, member) pairs — each a value of the declaration passed through as written.
///
/// Source: AWS's CloudFormation resource reference for the type against the
/// `CreateAddonRequest` shape. The Amazon EKS model spells its members in lower camel case
/// and renames none of these, so the only difference is the first character's case.
/// `AddonName` is not here because the add-on's name is its physical resource id, which the
/// create below sends; `ClusterName` is not because that create binds it to its URI;
/// `Tags` is not because the call takes a tag MAP while the type publishes a tag LIST; and
/// `PodIdentityAssociations`, `NamespaceConfig` and `PreserveOnDelete` are not because each
/// is mapped on its own terms below.
pub(crate) const EKS_ADDON_MEMBERS: &[(&str, &str)] = &[
    ("AddonVersion", "addonVersion"),
    ("ConfigurationValues", "configurationValues"),
    ("ResolveConflicts", "resolveConflicts"),
    ("ServiceAccountRoleArn", "serviceAccountRoleArn"),
];

/// The members of one `PodIdentityAssociations` entry as that create's
/// `podIdentityAssociations` list takes them (`AddonPodIdentityAssociations`'s two required
/// members: the service account and the role it assumes).
pub(crate) const EKS_ADDON_POD_IDENTITY_MEMBERS: &[(&str, &str)] =
    &[("RoleArn", "roleArn"), ("ServiceAccount", "serviceAccount")];

/// The members of the type's `NamespaceConfig` property as that create's `namespaceConfig`
/// (an `AddonNamespaceConfigRequest`, whose one member is the namespace the add-on is
/// installed in) takes them.
pub(crate) const EKS_ADDON_NAMESPACE_CONFIG_MEMBERS: &[(&str, &str)] =
    &[("Namespace", "namespace")];

/// Every `AWS::EKS::Addon` property these arms map, on the terms
/// [`EKS_CLUSTER_MAPPED_PROPERTIES`] records: a declared property no arm maps is refused
/// rather than dropped, because an add-on installed without what its declaration asked for
/// is not the add-on the template declared.
pub(crate) const EKS_ADDON_MAPPED_PROPERTIES: &[&str] = &[
    "AddonName",
    "AddonVersion",
    "ClusterName",
    "ConfigurationValues",
    "NamespaceConfig",
    "PodIdentityAssociations",
    "PreserveOnDelete",
    "ResolveConflicts",
    "ServiceAccountRoleArn",
    "Tags",
];

/// The members `DescribeAddon` reports under this type's own property names, as
/// `(property, member)` pairs, for drift detection — the scalars of the declaration.
/// `Tags` is left out for the same reason as everywhere else here: the tags a resource
/// carries change independently of its declaration.
pub(crate) const EKS_ADDON_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("AddonName", "addonName"),
    ("AddonVersion", "addonVersion"),
    ("ClusterName", "clusterName"),
    ("ConfigurationValues", "configurationValues"),
    ("ServiceAccountRoleArn", "serviceAccountRoleArn"),
];

/// The `AWS::EKS::Nodegroup` properties `CreateNodegroup` takes under its own member name,
/// as (property, member) pairs — read the same way [`EKS_ADDON_MEMBERS`] is.
///
/// `NodegroupName` is not here because the node group's name is its physical resource id,
/// which the create below sends; `ClusterName` is not because that create binds it to its
/// URI; `Tags` is not because the call takes a tag MAP; and the nested configurations are
/// mapped member by member below.
pub(crate) const EKS_NODEGROUP_MEMBERS: &[(&str, &str)] = &[
    ("AmiType", "amiType"),
    ("CapacityType", "capacityType"),
    ("DiskSize", "diskSize"),
    ("InstanceTypes", "instanceTypes"),
    ("Labels", "labels"),
    ("NodeRole", "nodeRole"),
    ("ReleaseVersion", "releaseVersion"),
    ("Subnets", "subnets"),
    ("Version", "version"),
];

/// The members of the type's `ScalingConfig` property as that create's `scalingConfig` (a
/// `NodegroupScalingConfig`) takes them.
pub(crate) const EKS_NODEGROUP_SCALING_MEMBERS: &[(&str, &str)] = &[
    ("DesiredSize", "desiredSize"),
    ("MaxSize", "maxSize"),
    ("MinSize", "minSize"),
];

/// The members of the type's `RemoteAccess` property as that create's `remoteAccess` (a
/// `RemoteAccessConfig`) takes them.
pub(crate) const EKS_NODEGROUP_REMOTE_ACCESS_MEMBERS: &[(&str, &str)] = &[
    ("Ec2SshKey", "ec2SshKey"),
    ("SourceSecurityGroups", "sourceSecurityGroups"),
];

/// The members of the type's `LaunchTemplate` property as that create's `launchTemplate` (a
/// `LaunchTemplateSpecification`) takes them.
pub(crate) const EKS_NODEGROUP_LAUNCH_TEMPLATE_MEMBERS: &[(&str, &str)] =
    &[("Id", "id"), ("Name", "name"), ("Version", "version")];

/// The members of the type's `UpdateConfig` property as that create's `updateConfig` (a
/// `NodegroupUpdateConfig`) takes them.
pub(crate) const EKS_NODEGROUP_UPDATE_CONFIG_MEMBERS: &[(&str, &str)] = &[
    ("MaxUnavailable", "maxUnavailable"),
    ("MaxUnavailablePercentage", "maxUnavailablePercentage"),
    ("UpdateStrategy", "updateStrategy"),
];

/// The members of the type's `NodeRepairConfig` property as that create's
/// `nodeRepairConfig` (a `NodeRepairConfig`) takes them.
pub(crate) const EKS_NODEGROUP_REPAIR_CONFIG_MEMBERS: &[(&str, &str)] = &[
    ("Enabled", "enabled"),
    (
        "MaxParallelNodesRepairedCount",
        "maxParallelNodesRepairedCount",
    ),
    (
        "MaxParallelNodesRepairedPercentage",
        "maxParallelNodesRepairedPercentage",
    ),
    (
        "MaxUnhealthyNodeThresholdCount",
        "maxUnhealthyNodeThresholdCount",
    ),
    (
        "MaxUnhealthyNodeThresholdPercentage",
        "maxUnhealthyNodeThresholdPercentage",
    ),
];

/// The members of one `Taints` entry as that create's `taints` list takes them (a `Taint`:
/// the key, the value and the effect it repels pods with).
pub(crate) const EKS_NODEGROUP_TAINT_MEMBERS: &[(&str, &str)] =
    &[("Effect", "effect"), ("Key", "key"), ("Value", "value")];

/// Every `AWS::EKS::Nodegroup` property these arms map, on the terms
/// [`EKS_ADDON_MAPPED_PROPERTIES`] records.
///
/// `ForceUpdateEnabled` is the one property no arm maps: it governs how a LATER version
/// update of the group drains its pods (`UpdateNodegroupVersion$force`), which this
/// emulator applies through no update arm of this type, so a declaration that asks for it
/// is refused rather than created as a node group whose later updates would not honour it.
pub(crate) const EKS_NODEGROUP_MAPPED_PROPERTIES: &[&str] = &[
    "AmiType",
    "CapacityType",
    "ClusterName",
    "DiskSize",
    "InstanceTypes",
    "Labels",
    "LaunchTemplate",
    "NodeRepairConfig",
    "NodeRole",
    "NodegroupName",
    "ReleaseVersion",
    "RemoteAccess",
    "ScalingConfig",
    "Subnets",
    "Tags",
    "Taints",
    "UpdateConfig",
    "Version",
];

/// The members `DescribeNodegroup` reports under this type's own property names, as
/// `(property, member)` pairs, for drift detection — the values of the declaration that
/// service reports back under the same name, `Tags` left out for the reason it is
/// everywhere else here.
pub(crate) const EKS_NODEGROUP_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("AmiType", "amiType"),
    ("CapacityType", "capacityType"),
    ("ClusterName", "clusterName"),
    ("DiskSize", "diskSize"),
    ("InstanceTypes", "instanceTypes"),
    ("Labels", "labels"),
    ("NodeRole", "nodeRole"),
    ("NodegroupName", "nodegroupName"),
    ("ReleaseVersion", "releaseVersion"),
    ("Subnets", "subnets"),
    ("Version", "version"),
];

/// Install the add-on an `AWS::EKS::Addon` declares, in Amazon EKS — so a client that
/// deployed the stack and then calls `eks describe-addon` for the declared add-on reads it
/// back with the status and the resolved version that service assigned, and finds it in
/// `eks list-addons`, rather than a `ResourceNotFoundException` for an add-on nothing
/// installed.
///
/// The add-on's name is the resource's physical id (the `AddonName` the declaration states)
/// and the cluster is the create's URI label; the rest of the declaration is that create's
/// members — the scalars of [`EKS_ADDON_MEMBERS`], the EKS Pod Identity associations the
/// add-on owns and the namespace it is installed into — with the declared tags beside the
/// ones CloudFormation gives the resource as the tag MAP the call takes.
///
/// The ARN Amazon EKS minted is the attribute the type publishes, so it is read off the
/// add-on that service created rather than computed here. The create leaves the add-on
/// `CREATING`, and [`eks_addon_readiness`] is what the resource's creation is held open on.
pub(crate) async fn create_eks_addon(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, EKS_ADDON_MAPPED_PROPERTIES)?;
    let cluster_name = required_property(resource, EKS_CLUSTER_NAME_PROPERTY)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    request.insert(
        "addonName".to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    copy_members(&properties, EKS_ADDON_MEMBERS, &mut request);
    if let Some(declared) = properties.get("NamespaceConfig") {
        request.insert(
            "namespaceConfig".to_string(),
            mapped_members(declared, EKS_ADDON_NAMESPACE_CONFIG_MEMBERS),
        );
    }
    // "An array of EKS Pod Identity associations owned by the add-on. Each association maps
    // a role to a service account in a namespace in the cluster": a LIST, so every entry is
    // mapped the same way its own members are.
    let associations: Vec<Value> = declared_list(&resource.properties, "PodIdentityAssociations")
        .iter()
        .map(|declared| mapped_members(declared, EKS_ADDON_POD_IDENTITY_MEMBERS))
        .collect();
    if !associations.is_empty() {
        request.insert(
            "podIdentityAssociations".to_string(),
            Value::Array(associations),
        );
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_rest_json(
        ctx,
        EKS_SIGNING_NAME,
        region,
        "POST",
        &format!(
            "{EKS_CLUSTERS_URI}/{}/{EKS_ADDONS_PATH}",
            percent_encoded(&cluster_name)
        ),
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = answered
        .get("addon")
        .and_then(|addon| addon.get("addonArn"))
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "Amazon EKS answered CreateAddon without an addonArn".to_string(),
        })?
        .to_string();
    let mut attributes = BTreeMap::new();
    attributes.insert("Arn".to_string(), arn);
    Ok(Created::attributes(attributes))
}

/// Whether Amazon EKS has finished installing the add-on a stack created.
///
/// An install is asynchronous: the add-on is reported [`EKS_ADDON_CREATING`] while that
/// service applies its Kubernetes resources and [`EKS_ADDON_ACTIVE`] once they are running,
/// which is the state the resource's creation is finished in. Every other status is one no
/// wait will bring the add-on out of — one whose install failed, one running without the
/// replicas it needs, one being updated or deleted out of this stack's reach — so the
/// resource fails with the state that service reports rather than completing as an add-on
/// the cluster cannot use. An add-on that service no longer holds is in the same position,
/// nothing being left to install it.
pub(crate) async fn eks_addon_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identity: &EksChildIdentity,
) -> Result<Readiness, Refusal> {
    let Some(addon) = read_eks_child(ctx, region, identity, EKS_ADDONS_PATH, "addon").await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon EKS holds no add-on {} on cluster {}",
            identity.name, identity.cluster_name
        )));
    };
    match addon
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        EKS_ADDON_ACTIVE => Ok(Readiness::Ready),
        EKS_ADDON_CREATING => Ok(Readiness::Pending(EKS_ADDON_CREATING.to_string())),
        other => Ok(Readiness::Failed(format!(
            "Amazon EKS reports add-on {} on cluster {} as {other}",
            identity.name, identity.cluster_name
        ))),
    }
}

/// The add-on's configuration as Amazon EKS reports it, for drift detection:
/// `DescribeAddon`'s own record, read by the same pair the delete uses. One that service no
/// longer holds was deleted.
pub(crate) async fn describe_eks_addon(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let identity = eks_child_identity_of(resource)?;
    Ok(
        read_eks_child(ctx, region, &identity, EKS_ADDONS_PATH, "addon")
            .await?
            .map(|addon| reported_as_properties(&addon, EKS_ADDON_REPORTED_PROPERTIES)),
    )
}

/// Delete the add-on a stack created, addressed by the cluster that holds it and the
/// add-on's own name — the two `@httpLabel`s of that delete.
///
/// `PreserveOnDelete` is the declaration's own answer to what becomes of the add-on's
/// Kubernetes resources: `DeleteAddon$preserve` is "specifying this option preserves the
/// add-on software on your cluster but Amazon EKS stops managing any settings for the
/// add-on", and that delete takes it as a QUERY parameter, so a declaration that asks for
/// it reaches that service as the delete it asked for. An add-on Amazon EKS no longer
/// holds — and a cluster it no longer holds, which took the add-on with it — is already in
/// the state the delete asks for.
pub(crate) async fn delete_eks_addon(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let mut uri = eks_child_identity_of(resource)?.uri(EKS_ADDONS_PATH);
    if let Some(preserve) = property(resource, "PreserveOnDelete") {
        uri.push_str(&format!("?preserve={}", percent_encoded(&preserve)));
    }
    delete_eks_child(ctx, region, &uri).await
}

/// Create the managed node group an `AWS::EKS::Nodegroup` declares, in Amazon EKS — so a
/// client that deployed the stack and then calls `eks describe-nodegroup` reads the group
/// back under its parent cluster, and finds it in `eks list-nodegroups`, rather than a
/// `ResourceNotFoundException` for a node group nothing created.
///
/// The group's name is the resource's physical id (the `NodegroupName` the declaration
/// states, or the name CloudFormation computed for it) and the cluster is the create's URI
/// label; its own properties are that create's members — the scalars of
/// [`EKS_NODEGROUP_MEMBERS`], the nested configurations mapped member by member and the
/// taints its nodes repel pods with — with the declared tags beside the ones CloudFormation
/// gives the resource as the tag MAP the call takes.
///
/// The ARN Amazon EKS minted is the attribute the type publishes here, read off the group
/// that service created. The create leaves the group `CREATING`, and
/// [`eks_nodegroup_readiness`] is what the resource's creation is held open on.
pub(crate) async fn create_eks_nodegroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, EKS_NODEGROUP_MAPPED_PROPERTIES)?;
    let cluster_name = required_property(resource, EKS_CLUSTER_NAME_PROPERTY)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    request.insert(
        "nodegroupName".to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    copy_members(&properties, EKS_NODEGROUP_MEMBERS, &mut request);
    for (property, member, pairs) in [
        (
            "ScalingConfig",
            "scalingConfig",
            EKS_NODEGROUP_SCALING_MEMBERS,
        ),
        (
            "RemoteAccess",
            "remoteAccess",
            EKS_NODEGROUP_REMOTE_ACCESS_MEMBERS,
        ),
        (
            "LaunchTemplate",
            "launchTemplate",
            EKS_NODEGROUP_LAUNCH_TEMPLATE_MEMBERS,
        ),
        (
            "UpdateConfig",
            "updateConfig",
            EKS_NODEGROUP_UPDATE_CONFIG_MEMBERS,
        ),
        (
            "NodeRepairConfig",
            "nodeRepairConfig",
            EKS_NODEGROUP_REPAIR_CONFIG_MEMBERS,
        ),
    ] {
        if let Some(declared) = properties.get(property) {
            request.insert(member.to_string(), mapped_members(declared, pairs));
        }
    }
    // "The Kubernetes taints to be applied to the nodes in the node group when they are
    // created": a LIST, so every entry is mapped the same way its own members are.
    let taints: Vec<Value> = declared_list(&resource.properties, "Taints")
        .iter()
        .map(|declared| mapped_members(declared, EKS_NODEGROUP_TAINT_MEMBERS))
        .collect();
    if !taints.is_empty() {
        request.insert("taints".to_string(), Value::Array(taints));
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_rest_json(
        ctx,
        EKS_SIGNING_NAME,
        region,
        "POST",
        &format!(
            "{EKS_CLUSTERS_URI}/{}/{EKS_NODE_GROUPS_PATH}",
            percent_encoded(&cluster_name)
        ),
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = answered
        .get("nodegroup")
        .and_then(|nodegroup| nodegroup.get("nodegroupArn"))
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "Amazon EKS answered CreateNodegroup without a nodegroupArn".to_string(),
        })?
        .to_string();
    let mut attributes = BTreeMap::new();
    attributes.insert("Arn".to_string(), arn);
    Ok(Created::attributes(attributes))
}

/// Whether Amazon EKS has finished bringing up the node group a stack created.
///
/// A created group is reported [`EKS_NODEGROUP_CREATING`] while that service brings its
/// Auto Scaling group and its instances up, and [`EKS_NODEGROUP_ACTIVE`] once the nodes
/// have joined the cluster, which is the state the resource's creation is finished in.
/// Every other status is one no wait will bring the group out of — one whose create failed,
/// one running without the nodes it needs, one being updated or deleted out of this stack's
/// reach — so the resource fails with the state that service reports. A group that service
/// no longer holds is in the same position.
pub(crate) async fn eks_nodegroup_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identity: &EksChildIdentity,
) -> Result<Readiness, Refusal> {
    let read = read_eks_child(ctx, region, identity, EKS_NODE_GROUPS_PATH, "nodegroup").await?;
    let Some(nodegroup) = read else {
        return Ok(Readiness::Failed(format!(
            "Amazon EKS holds no node group {} on cluster {}",
            identity.name, identity.cluster_name
        )));
    };
    match nodegroup
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        EKS_NODEGROUP_ACTIVE => Ok(Readiness::Ready),
        EKS_NODEGROUP_CREATING => Ok(Readiness::Pending(EKS_NODEGROUP_CREATING.to_string())),
        other => Ok(Readiness::Failed(format!(
            "Amazon EKS reports node group {} on cluster {} as {other}",
            identity.name, identity.cluster_name
        ))),
    }
}

/// The node group's configuration as Amazon EKS reports it, for drift detection:
/// `DescribeNodegroup`'s own record, read by the same pair the delete uses.
pub(crate) async fn describe_eks_nodegroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let identity = eks_child_identity_of(resource)?;
    Ok(
        read_eks_child(ctx, region, &identity, EKS_NODE_GROUPS_PATH, "nodegroup")
            .await?
            .map(|nodegroup| {
                reported_as_properties(&nodegroup, EKS_NODEGROUP_REPORTED_PROPERTIES)
            }),
    )
}

/// Delete the node group a stack created, addressed by the cluster that holds it and the
/// group's own name. A group Amazon EKS no longer holds — and a cluster it no longer holds,
/// which took the group with it — is already in the state the delete asks for.
pub(crate) async fn delete_eks_nodegroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = eks_child_identity_of(resource)?.uri(EKS_NODE_GROUPS_PATH);
    delete_eks_child(ctx, region, &uri).await
}

/// What identifies one resource that lives INSIDE an Amazon EKS cluster: the cluster that
/// holds it and the resource's own name, which are the two `@httpLabel`s every operation of
/// that service on one is addressed by.
#[derive(Debug, Clone)]
pub(crate) struct EksChildIdentity {
    /// The cluster the declaration places the resource in.
    pub(crate) cluster_name: String,
    /// The resource's own name, which is its physical resource id.
    pub(crate) name: String,
}

impl EksChildIdentity {
    /// The URI that addresses this ONE resource: the cluster's collection of `path`, then
    /// the resource's own name, each percent-encoded as a non-greedy label is sent.
    pub(crate) fn uri(&self, path: &str) -> String {
        format!(
            "{EKS_CLUSTERS_URI}/{}/{path}/{}",
            percent_encoded(&self.cluster_name),
            percent_encoded(&self.name)
        )
    }
}

/// The pair that identifies one child of a cluster, read off the record the stack holds for
/// the resource.
pub(crate) fn eks_child_identity_of(
    resource: &StoredResource,
) -> Result<EksChildIdentity, Refusal> {
    eks_child_identity(
        &resource.resource_type,
        Some(&resource.properties),
        &resource.physical_resource_id,
    )
}

/// The pair that identifies one child of a cluster: the cluster from the property that names
/// it on the resource's own declaration, and the name from the physical id the resource was
/// created under.
///
/// A declaration that names no cluster describes no resource that could be addressed — the
/// cluster is an `@httpLabel` of every operation on one — so it is refused here rather than
/// with a request Amazon EKS would reject. `properties` is `None` where the declaration is
/// not known to the caller, which is that same refusal.
pub(crate) fn eks_child_identity(
    resource_type: &str,
    properties: Option<&Value>,
    physical_resource_id: &str,
) -> Result<EksChildIdentity, Refusal> {
    let cluster_name = properties
        .and_then(|properties| properties.get(EKS_CLUSTER_NAME_PROPERTY))
        .and_then(scalar_text)
        .filter(|cluster_name| !cluster_name.is_empty())
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY_CODE.to_string(),
            message: format!("{resource_type} declares no {EKS_CLUSTER_NAME_PROPERTY}"),
        })?;
    Ok(EksChildIdentity {
        cluster_name,
        name: physical_resource_id.to_string(),
    })
}

/// The record Amazon EKS holds for one child of a cluster, under `member` of that read's
/// answer, or `None` when it holds none (`ResourceNotFoundException`, judged on the CODE
/// and never on a message — which is also the answer for a cluster that service no longer
/// holds, since a deleted cluster takes its add-ons and node groups with it).
async fn read_eks_child(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identity: &EksChildIdentity,
    path: &str,
    member: &str,
) -> Result<Option<Value>, Refusal> {
    let uri = identity.uri(path);
    match call_rest_json(ctx, EKS_SIGNING_NAME, region, "GET", &uri, None).await {
        Ok(answer) => Ok(answer.get(member).cloned()),
        Err(refusal) if refusal.code == EKS_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Delete one child of a cluster at the URI that addresses it: a resource Amazon EKS no
/// longer holds is already in the state the delete asks for.
async fn delete_eks_child(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    uri: &str,
) -> Result<(), Refusal> {
    match call_rest_json(ctx, EKS_SIGNING_NAME, region, "DELETE", uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == EKS_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
