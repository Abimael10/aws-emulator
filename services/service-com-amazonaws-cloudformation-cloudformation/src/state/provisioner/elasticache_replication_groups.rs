// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon ElastiCache's REPLICATION GROUP: the group an
//! `AWS::ElastiCache::ReplicationGroup` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`, beside the cluster's ([`super::elasticache_caches`]),
//! the serverless cache's ([`super::elasticache_serverless_caches`]) and the subnet group's
//! ([`super::elasticache_subnet_groups`]).
//!
//! The type's properties ARE `CreateReplicationGroup`'s own members, spelled identically —
//! `ReplicationGroupDescription`, `Engine`, `CacheNodeType`, `NumCacheClusters`,
//! `AutomaticFailoverEnabled`, `CacheSubnetGroupName`, `SecurityGroupIds`, `AuthToken` and the
//! rest — so the declaration is walked against that operation's member catalogue
//! ([`CREATE_REPLICATION_GROUP_MEMBERS`]) exactly as the cache cluster's is. Which is the
//! point of creating the group in the owning service at all: every rule
//! `CreateReplicationGroup` applies to a declared property — the subnet group resolved
//! against the ones that service holds, the security groups against Amazon EC2's, the auth
//! token's own form, the replica counts' bounds — decides the resource's fate, so a template
//! ElastiCache would refuse fails the resource instead of being reported created.
//!
//! "Ref returns the replication group's id", which is the `ReplicationGroupId` the declaration
//! states (`super::super::template::naming_rules`) and the identifier every ElastiCache call
//! on one takes, so the resource's physical id needs no replacing by what the create answered.
//!
//! `CreateReplicationGroup` answers the group `creating`, in which it serves nothing and
//! publishes no endpoint, so the resource's creation is held open until ElastiCache reports it
//! `available` ([`replication_group_readiness`]) and the endpoints the type publishes are read
//! off it then ([`replication_group_ready_attributes`]).

use super::*;

/// `AWS::ElastiCache::ReplicationGroup`: "Ref returns the replication group's id", which is
/// the identifier `DescribeReplicationGroups`, `ModifyReplicationGroup` and
/// `DeleteReplicationGroup` each address one by.
pub(crate) const ELASTICACHE_REPLICATION_GROUP: &str = "AWS::ElastiCache::ReplicationGroup";

/// The property the type states the group's own identifier as, which is this resource's
/// physical id: `CreateReplicationGroup$ReplicationGroupId`. The type's last segment is
/// `ReplicationGroup`, so neither `ReplicationGroupName` nor `Name` finds it, which is why the
/// naming rules carry a row for this type keyed by this name.
pub(crate) const REPLICATION_GROUP_ID_PROPERTY: &str = "ReplicationGroupId";

/// The property the type states the group's daily snapshot source as, which no member of
/// `CreateReplicationGroup` carries: `ModifyReplicationGroup$SnapshottingClusterId` is "the
/// cluster ID that is used as the daily snapshot source for the replication group", so a
/// declaration of it is applied by that call rather than by the create
/// ([`set_declared_snapshot_source`]).
const SNAPSHOTTING_CLUSTER_ID_PROPERTY: &str = "SnapshottingClusterId";

/// The LONGEST `ReplicationGroupId` Amazon ElastiCache accepts: "a name must contain from 1 to
/// 40 alphanumeric characters or hyphens" (`CreateReplicationGroup$ReplicationGroupId`), which
/// the naming rules fit a GENERATED name to — an id past it names no group that service could
/// hold.
pub(crate) const REPLICATION_GROUP_ID_MAX_CHARACTERS: usize = 40;

/// The error code Amazon ElastiCache answers for a replication group it does not hold:
/// `ReplicationGroupNotFoundFault`'s own `awsQueryError` code
/// (`smithy_query(query="operation", operation="DescribeReplicationGroups")`), which is what
/// an awsQuery client branches on.
const REPLICATION_GROUP_NOT_FOUND: &str = "ReplicationGroupNotFoundFault";

/// The `ReplicationGroup$Status` a group is usable in, and the ones a NEW group is still
/// coming up out of. The member documents the lifecycle ("The current state of this
/// replication group - `creating`, `available`, `modifying`, `deleting`, `create-failed`,
/// `snapshotting`"); a group in any other status is in one no wait brings a new group out of,
/// which is a resource that failed to come up rather than one still coming.
const REPLICATION_GROUP_STATUS_AVAILABLE: &str = "available";
const REPLICATION_GROUP_STATUSES_COMING_UP: &[&str] = &["creating", "modifying"];

/// The members `CreateReplicationGroup` takes, from Amazon ElastiCache's own model
/// (`smithy_query(query="operation", operation="CreateReplicationGroup")`), with each list's
/// element name from its list shape's member `@xmlName`
/// (`smithy_query(query="shape", shape="AvailabilityZonesList,NodeGroupConfigurationList,
/// CacheSecurityGroupNameList,SecurityGroupIdsList,SnapshotArnsList,TagList,
/// LogDeliveryConfigurationRequestList,UserGroupIdListInput")` — the user-group list fixes
/// none, so its element is the Smithy default `member`).
///
/// This is the catalogue the declaration of an `AWS::ElastiCache::ReplicationGroup` is walked
/// against: the type's properties ARE these members, so a property whose name is one of them
/// is passed through under it, and one the catalogue does not carry is refused rather than
/// dropped ([`query_members`]).
const CREATE_REPLICATION_GROUP_MEMBERS: &[(&str, QueryMember)] = &[
    ("AtRestEncryptionEnabled", QueryMember::Scalar),
    ("AuthToken", QueryMember::Scalar),
    ("AutoMinorVersionUpgrade", QueryMember::Scalar),
    ("AutomaticFailoverEnabled", QueryMember::Scalar),
    ("CacheNodeType", QueryMember::Scalar),
    ("CacheParameterGroupName", QueryMember::Scalar),
    (
        "CacheSecurityGroupNames",
        QueryMember::List("CacheSecurityGroupName"),
    ),
    ("CacheSubnetGroupName", QueryMember::Scalar),
    ("ClusterMode", QueryMember::Scalar),
    ("DataTieringEnabled", QueryMember::Scalar),
    ("Durability", QueryMember::Scalar),
    ("Engine", QueryMember::Scalar),
    ("EngineVersion", QueryMember::Scalar),
    ("GlobalReplicationGroupId", QueryMember::Scalar),
    ("IpDiscovery", QueryMember::Scalar),
    ("KmsKeyId", QueryMember::Scalar),
    (
        "LogDeliveryConfigurations",
        QueryMember::List("LogDeliveryConfigurationRequest"),
    ),
    ("MultiAZEnabled", QueryMember::Scalar),
    ("NetworkType", QueryMember::Scalar),
    (
        "NodeGroupConfiguration",
        QueryMember::List("NodeGroupConfiguration"),
    ),
    ("NotificationTopicArn", QueryMember::Scalar),
    ("NumCacheClusters", QueryMember::Scalar),
    ("NumNodeGroups", QueryMember::Scalar),
    ("Port", QueryMember::Scalar),
    (
        "PreferredCacheClusterAZs",
        QueryMember::List("AvailabilityZone"),
    ),
    ("PreferredMaintenanceWindow", QueryMember::Scalar),
    ("PrimaryClusterId", QueryMember::Scalar),
    ("ReplicasPerNodeGroup", QueryMember::Scalar),
    ("ReplicationGroupDescription", QueryMember::Scalar),
    ("ReplicationGroupId", QueryMember::Scalar),
    ("SecurityGroupIds", QueryMember::List("SecurityGroupId")),
    ("ServerlessCacheSnapshotName", QueryMember::Scalar),
    ("SnapshotArns", QueryMember::List("SnapshotArn")),
    ("SnapshotName", QueryMember::Scalar),
    ("SnapshotRetentionLimit", QueryMember::Scalar),
    ("SnapshotWindow", QueryMember::Scalar),
    ("Tags", QueryMember::List("Tag")),
    ("TransitEncryptionEnabled", QueryMember::Scalar),
    ("TransitEncryptionMode", QueryMember::Scalar),
    ("UserGroupIds", QueryMember::List("member")),
];

/// Every property of the type is its `CreateReplicationGroup` member's own name, so none is
/// renamed on the way to that call — the empty catalogue [`query_members`] takes beside the
/// member one.
const REPLICATION_GROUP_RENAMED_PROPERTIES: &[(&str, &str)] = &[];

/// The properties of the type the walk over the declaration does NOT turn into a
/// `CreateReplicationGroup` member.
const REPLICATION_GROUP_PROPERTIES_NOT_SENT: &[&str] = &[
    // The identifier CloudFormation decides, which is this resource's physical id ("Ref
    // returns the replication group's id"): the arm sets the member from that id, so the walk
    // must not also push the declared spelling of it.
    REPLICATION_GROUP_ID_PROPERTY,
    // The tag list carries the declared tags AND the ones CloudFormation propagates to the
    // resource, so the arm builds it out of both ([`tags_of`]).
    "Tags",
    // A member of a DIFFERENT call: the group's daily snapshot source is one of its own member
    // clusters, which do not exist until the group does, so `CreateReplicationGroup` takes no
    // member for it at all. It is applied by `ModifyReplicationGroup` once that service
    // reports the group up ([`set_declared_snapshot_source`]), which is the position an
    // `AWS::RDS::DBInstance`'s role associations are in
    // ([`super::rds_role_associations`]).
    SNAPSHOTTING_CLUSTER_ID_PROPERTY,
];

/// Create the replication group an `AWS::ElastiCache::ReplicationGroup` declares, under the
/// physical id the naming rules settled — the `ReplicationGroupId` the declaration states, or
/// the generated name where it states none.
///
/// What the create ANSWERS replaces nothing of the identity: the group is created under the
/// identifier this resource already carries, which is what the type's `Ref` returns. Its ARN
/// is read off that answer, because it is ElastiCache's to mint; the endpoints are not —
/// `CreateReplicationGroup` answers a group that is still `creating`, which has none yet, so
/// the endpoint attributes are read once that service reports the group up
/// ([`replication_group_ready_attributes`]).
pub(crate) async fn create_replication_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_REPLICATION_GROUP_MEMBERS,
        REPLICATION_GROUP_RENAMED_PROPERTIES,
        REPLICATION_GROUP_PROPERTIES_NOT_SENT,
        ELASTICACHE_REPLICATION_GROUP,
    )?;
    parameters.push((
        REPLICATION_GROUP_ID_PROPERTY.to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        "Tags",
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    let answered = call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "CreateReplicationGroup",
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "ARN").filter(|arn| !arn.is_empty()) {
        // The type publishes one ARN, so the conventional `Arn` attribute every type
        // answers is that same value ([`super::template::conventional_attributes`]).
        attributes.insert("Arn".to_string(), arn);
    }
    Ok(Created::attributes(attributes))
}

/// Whether Amazon ElastiCache reports the replication group a stack created UP.
///
/// A group the create has just answered for is `creating`: it publishes no endpoint and serves
/// no client, so the resource's creation is held open until that service reports it
/// `available`. A status no new group comes out of — `create-failed`, and a group ElastiCache
/// does not hold at all — is a failure, because nothing is going to bring it up.
///
/// The one declared property the create takes no member for is applied here, where that service
/// has just reported the group up: its daily snapshot source is one of the group's own member
/// clusters, and `ModifyReplicationGroup` is the call that names it
/// ([`set_declared_snapshot_source`]).
pub(crate) async fn replication_group_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_replication_group(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon ElastiCache holds no replication group named {physical_resource_id}"
        )));
    };
    let status = reported
        .get("Status")
        .and_then(scalar_text)
        .unwrap_or_default();
    if status == REPLICATION_GROUP_STATUS_AVAILABLE {
        if let Some(refusal) = set_declared_snapshot_source(
            ctx,
            region,
            physical_resource_id,
            declared,
            &reported,
        )
        .await?
        {
            return Ok(Readiness::Refused(refusal));
        }
        return Ok(Readiness::Ready);
    }
    if REPLICATION_GROUP_STATUSES_COMING_UP.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon ElastiCache reports the replication group {physical_resource_id} in status \
         {status}"
    )))
}

/// Name the group's daily snapshot source, where the declaration states one and the group does
/// not already carry it.
///
/// `Ok(None)` is nothing left to ask for — including a declaration that states no source, which
/// makes no call at all. `Ok(Some(refusal))` is Amazon ElastiCache REFUSING the change (a member
/// cluster it does not hold, a cluster that is not a member of this group, a group whose
/// keyspace is partitioned), answered AS that refusal so the reporting site words it the way it
/// words any other refused provider call.
///
/// The source the group already carries is read back first — off the same answer the readiness
/// check was made from — so the arm converges: the readiness answer is asked again on every pass
/// of the stabilisation walk, and a group that already names the declared cluster is left alone.
/// The comparison ignores case because the identifier is "stored as a lowercase string", so a
/// declaration spelling a member cluster's id in mixed case names the cluster the group already
/// snapshots from.
async fn set_declared_snapshot_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
    reported: &Value,
) -> Result<Option<Refusal>, Refusal> {
    let Some(asked) = declared
        .and_then(|declared| declared.get(SNAPSHOTTING_CLUSTER_ID_PROPERTY))
        .and_then(scalar_text)
        .filter(|asked| !asked.is_empty())
    else {
        return Ok(None);
    };
    let held = reported
        .get(SNAPSHOTTING_CLUSTER_ID_PROPERTY)
        .and_then(scalar_text)
        .unwrap_or_default();
    if held.eq_ignore_ascii_case(&asked) {
        return Ok(None);
    }
    let parameters = vec![
        (
            REPLICATION_GROUP_ID_PROPERTY.to_string(),
            physical_resource_id.to_string(),
        ),
        (SNAPSHOTTING_CLUSTER_ID_PROPERTY.to_string(), asked),
        ("ApplyImmediately".to_string(), "true".to_string()),
    ];
    match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "ModifyReplicationGroup",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(None),
        Err(refusal) => Ok(Some(refusal)),
    }
}

/// The endpoint attributes the type publishes, read off the group once ElastiCache reports it
/// up: a group still `creating` publishes none of them, so the create's answer carries none.
///
/// The type publishes the addresses a client connects to, each read off the member of
/// ElastiCache's own record of the group that holds it:
///
/// * `ConfigurationEndPoint.Address` / `.Port` — the group's `ConfigurationEndpoint`, "the
///   configuration endpoint for this replication group", which a cluster mode ENABLED group is
///   reached through;
/// * `PrimaryEndPoint.Address` / `.Port` — the first node group's `PrimaryEndpoint`, "the
///   endpoint of the primary node in this node group (shard)", which is where a cluster mode
///   disabled group takes its writes;
/// * `ReaderEndPoint.Address` / `.Port` — that same shard's `ReaderEndpoint`, "the endpoint of
///   the replica nodes in this node group (shard)";
/// * `ReadEndPoint.Addresses` / `.Ports` — the `ReadEndpoint` of every node group member, "the
///   information required for client programs to connect to a node for read operations", as
///   the comma-delimited strings those two return values are, in the one document order that
///   makes the ports line up with the addresses.
///
/// The `.List` form of either read-endpoint return value is a LIST, which the attribute map
/// cannot carry ([`super::super::template::return_values`] records why), so it is left to the
/// delimited form above.
pub(crate) async fn replication_group_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut attributes = BTreeMap::new();
    let Some(answered) = describe_replication_group(ctx, region, physical_resource_id).await?
    else {
        return Ok(attributes);
    };
    // Each is a STRUCTURE of the group's answer, so it is read out of the same document on its
    // own ([`xml_scalar_children`] leaves a composite member out). The reading is scoped to the
    // one group because the READ is: `DescribeReplicationGroups` is addressed by
    // `ReplicationGroupId`, so the answer carries that group or nothing — and a group reports
    // the primary and the reader endpoint of a shard at most once, the first `<NodeGroup>`
    // being the only one a cluster mode disabled group has.
    for (attribute, member) in [
        ("ConfigurationEndPoint", "ConfigurationEndpoint"),
        ("PrimaryEndPoint", "PrimaryEndpoint"),
        ("ReaderEndPoint", "ReaderEndpoint"),
    ] {
        let endpoint = xml_scalar_children(&answered, member);
        for value in ["Address", "Port"] {
            let Some(published) = endpoint
                .get(value)
                .and_then(scalar_text)
                .filter(|published| !published.is_empty())
            else {
                continue;
            };
            attributes.insert(format!("{attribute}.{value}"), published);
        }
    }
    let read_endpoints = xml_scalar_children_of_each(&answered, "ReadEndpoint");
    for (attribute, value) in [("Addresses", "Address"), ("Ports", "Port")] {
        let published: Vec<String> = read_endpoints
            .iter()
            .filter_map(|endpoint| endpoint.get(value).and_then(scalar_text))
            .filter(|published| !published.is_empty())
            .collect();
        if published.is_empty() {
            continue;
        }
        attributes.insert(format!("ReadEndPoint.{attribute}"), published.join(","));
    }
    Ok(attributes)
}

/// Apply a CHANGED declaration to the replication group a stack already created, through
/// `ModifyReplicationGroup` — the call Amazon ElastiCache changes a group's own configuration
/// with.
///
/// Only the members the modification TAKES are sent: a create-only member is not something a
/// modification can change, so a declaration that changed one is applied as far as the
/// modification reaches and the rest is left to the replacement CloudFormation decides
/// elsewhere. `ApplyImmediately` is sent because a stack reports a resource updated when the
/// update is done rather than when the group's next maintenance window comes round.
pub(crate) async fn update_replication_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let mut parameters = query_members(
        declared,
        MODIFY_REPLICATION_GROUP_MEMBERS,
        REPLICATION_GROUP_RENAMED_PROPERTIES,
        REPLICATION_GROUP_UPDATE_PROPERTIES_NOT_SENT,
        ELASTICACHE_REPLICATION_GROUP,
    )?;
    parameters.push((
        REPLICATION_GROUP_ID_PROPERTY.to_string(),
        resource.physical_resource_id.clone(),
    ));
    parameters.push(("ApplyImmediately".to_string(), "true".to_string()));
    let answered = call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "ModifyReplicationGroup",
        &parameters,
    )
    .await?;
    // The group keeps the identity it was created with — the ARN and the endpoints all follow
    // the identifier — so the modification re-points nothing of it; the ARN is answered again
    // so an attribute read off the create is not dropped by the update.
    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "ARN").filter(|arn| !arn.is_empty()) {
        attributes.insert("Arn".to_string(), arn);
    }
    Ok(Some(Created::attributes(attributes)))
}

/// The members `ModifyReplicationGroup` takes that an `AWS::ElastiCache::ReplicationGroup`
/// declaration can carry, from Amazon ElastiCache's own model
/// (`smithy_query(query="operation", operation="ModifyReplicationGroup")`): the create's
/// catalogue without the members that can only be set when the group is created.
const MODIFY_REPLICATION_GROUP_MEMBERS: &[(&str, QueryMember)] = &[
    ("AutoMinorVersionUpgrade", QueryMember::Scalar),
    ("AutomaticFailoverEnabled", QueryMember::Scalar),
    ("CacheNodeType", QueryMember::Scalar),
    ("CacheParameterGroupName", QueryMember::Scalar),
    (
        "CacheSecurityGroupNames",
        QueryMember::List("CacheSecurityGroupName"),
    ),
    ("ClusterMode", QueryMember::Scalar),
    ("Durability", QueryMember::Scalar),
    ("Engine", QueryMember::Scalar),
    ("EngineVersion", QueryMember::Scalar),
    ("IpDiscovery", QueryMember::Scalar),
    (
        "LogDeliveryConfigurations",
        QueryMember::List("LogDeliveryConfigurationRequest"),
    ),
    ("MultiAZEnabled", QueryMember::Scalar),
    ("NotificationTopicArn", QueryMember::Scalar),
    ("PreferredMaintenanceWindow", QueryMember::Scalar),
    ("PrimaryClusterId", QueryMember::Scalar),
    ("ReplicationGroupDescription", QueryMember::Scalar),
    ("SecurityGroupIds", QueryMember::List("SecurityGroupId")),
    ("SnapshotRetentionLimit", QueryMember::Scalar),
    ("SnapshotWindow", QueryMember::Scalar),
    (SNAPSHOTTING_CLUSTER_ID_PROPERTY, QueryMember::Scalar),
    ("TransitEncryptionEnabled", QueryMember::Scalar),
];

/// The properties the UPDATE does not send: the create's own list, plus every property the
/// modification takes no member for.
///
/// A property outside the modification's catalogue is not something that call can change, and
/// it is not an unknown property either — the create accepted it — so it is left out of the
/// modification rather than refused: what a modification cannot change is what the replacement
/// CloudFormation decides elsewhere is for. The AUTH TOKEN is in that position too: changing
/// one takes `ModifyReplicationGroup$AuthTokenUpdateStrategy` ("rotate" or "set"), which this
/// type publishes no property for at all, so a declaration that changes the token is not
/// rotated behind the caller's back. The transit-encryption MODE is there as well: "setting
/// `TransitEncryptionMode` to `required` is a two-step process that requires you to first set
/// the `TransitEncryptionMode` to `preferred`", which one modification cannot make.
const REPLICATION_GROUP_UPDATE_PROPERTIES_NOT_SENT: &[&str] = &[
    REPLICATION_GROUP_ID_PROPERTY,
    "Tags",
    "AtRestEncryptionEnabled",
    "AuthToken",
    "CacheSubnetGroupName",
    "DataTieringEnabled",
    "GlobalReplicationGroupId",
    "KmsKeyId",
    "NetworkType",
    "NodeGroupConfiguration",
    "NumCacheClusters",
    "NumNodeGroups",
    "Port",
    "PreferredCacheClusterAZs",
    "ReplicasPerNodeGroup",
    "ServerlessCacheSnapshotName",
    "SnapshotArns",
    "SnapshotName",
    "TransitEncryptionMode",
    "UserGroupIds",
];

/// Delete the replication group a stack created, addressed by its identifier — this resource's
/// physical id. A group Amazon ElastiCache no longer has (`ReplicationGroupNotFoundFault`) is
/// already in the state the delete asks for.
///
/// `DeleteReplicationGroup$FinalSnapshotIdentifier` is "the name of a final node group
/// (shard) snapshot", which is what a `Snapshot` deletion policy asks for
/// ([`super::super::provisioning::Policies`]); without one the call carries no snapshot member
/// at all, which is the form CloudFormation makes for the `Delete` policy.
pub(crate) async fn delete_replication_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    final_snapshot: Option<&str>,
) -> Result<(), Refusal> {
    let mut parameters = vec![(
        REPLICATION_GROUP_ID_PROPERTY.to_string(),
        physical_resource_id.to_string(),
    )];
    if let Some(snapshot_identifier) = final_snapshot {
        parameters.push((
            "FinalSnapshotIdentifier".to_string(),
            snapshot_identifier.to_string(),
        ));
    }
    match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DeleteReplicationGroup",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REPLICATION_GROUP_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon ElastiCache has finished removing the replication group a stack deleted.
///
/// "The `DeleteReplicationGroup` operation deletes an existing replication group" and leaves
/// it `deleting` until the deletion completes, so the group is still there when the delete
/// answers: the deletion is finished once that service no longer holds it, which is what keeps
/// a stack from deleting the subnet group the group's member clusters still sit in.
pub(crate) async fn replication_group_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_replication_group(ctx, region, physical_resource_id).await? {
        Some(reported) => Ok(Removal::Pending(
            reported
                .get("Status")
                .and_then(scalar_text)
                .unwrap_or_default(),
        )),
        None => Ok(Removal::Gone),
    }
}

/// The group's configuration as Amazon ElastiCache reports it, for the readiness and removal
/// reads above and for drift detection. A group it no longer holds
/// (`ReplicationGroupNotFoundFault`) was deleted.
///
/// A group is reported as one `<ReplicationGroup>` of the answer's `ReplicationGroups` list
/// (the element name that list's member `@xmlName` fixes — `smithy_query(query="shape",
/// shape="ReplicationGroupList")`), whose SCALAR members are read out by name; a member that
/// is a LIST (the node groups, the member clusters) or a STRUCTURE (either endpoint) is left
/// out, which is the position every arm whose property is not a scalar is in and what
/// [`super::super::drift`] documents — the endpoints the TYPE publishes are read off the same
/// answer on their own ([`replication_group_ready_attributes`]).
pub(crate) async fn read_replication_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(answered) = describe_replication_group(ctx, region, physical_resource_id).await?
    else {
        return Ok(None);
    };
    let reported = xml_scalar_children(&answered, "ReplicationGroup");
    if reported.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(reported)))
}

/// Amazon ElastiCache's own answer for the one replication group `physical_resource_id` names,
/// or `None` where that service holds no group under it.
///
/// The one read both the configuration above and the endpoint attributes are taken from, so
/// the two cannot ask for the group in two different ways.
async fn describe_replication_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<String>, Refusal> {
    let parameters = vec![(
        REPLICATION_GROUP_ID_PROPERTY.to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DescribeReplicationGroups",
        &parameters,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == REPLICATION_GROUP_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}
