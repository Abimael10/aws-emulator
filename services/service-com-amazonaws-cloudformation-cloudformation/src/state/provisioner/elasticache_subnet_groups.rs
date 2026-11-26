// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon ElastiCache's CACHE SUBNET GROUP: the group an
//! `AWS::ElastiCache::SubnetGroup` declares. One file of [`super`]'s per-type arms, dispatched
//! to by its `Provisioner`, beside the cluster's ([`super::elasticache_caches`]) and the
//! serverless cache's ([`super::elasticache_serverless_caches`]).
//!
//! The group is what a cache's `CacheSubnetGroupName` names, and Amazon ElastiCache resolves
//! that name against the groups it HOLDS before it will create a cluster or a replication
//! group in one (`CacheSubnetGroupNotFoundFault`) — so a stack that declares a subnet group
//! and a cache placed in it has to create the group in that service for real, or the cache's
//! own create is refused and the stack rolls back. That is the same reason Amazon RDS's
//! `AWS::RDS::DBSubnetGroup` and Amazon Redshift's `AWS::Redshift::ClusterSubnetGroup` arms
//! exist.
//!
//! The type's properties are `CreateCacheSubnetGroup`'s own members but for one spelling: its
//! `Description` is that call's `CacheSubnetGroupDescription`
//! ([`CACHE_SUBNET_GROUP_RENAMED_PROPERTIES`]), so the declaration is walked against that
//! operation's member catalogue ([`CREATE_CACHE_SUBNET_GROUP_MEMBERS`]) the way the cluster's
//! is.
//!
//! "Ref returns the name of the cache subnet group", which is the `CacheSubnetGroupName` the
//! declaration states and the identifier every ElastiCache call on one takes, so the
//! resource's physical id needs no replacing by what the create answered. The create is
//! synchronous — the group exists when it answers — so the type needs neither a readiness nor
//! a removal arm.

use super::*;

/// `AWS::ElastiCache::SubnetGroup`: "Ref returns the name of the cache subnet group", which is
/// the identifier `DescribeCacheSubnetGroups`, `ModifyCacheSubnetGroup` and
/// `DeleteCacheSubnetGroup` each address one by.
pub(crate) const ELASTICACHE_SUBNET_GROUP: &str = "AWS::ElastiCache::SubnetGroup";

/// The error code Amazon ElastiCache answers for a cache subnet group it does not hold:
/// `CacheSubnetGroupNotFoundFault`'s own `awsQueryError` code
/// (`smithy_query(query="operation", operation="DeleteCacheSubnetGroup")`), which is what an
/// awsQuery client branches on.
const CACHE_SUBNET_GROUP_NOT_FOUND: &str = "CacheSubnetGroupNotFoundFault";

/// The members `CreateCacheSubnetGroup` takes, from Amazon ElastiCache's own model
/// (`smithy_query(query="operation", operation="CreateCacheSubnetGroup")`), with each list's
/// element name from its list shape's member `@xmlName` (`smithy_query(query="shape",
/// shape="SubnetIdentifierList,TagList")`).
///
/// This is the catalogue the declaration of an `AWS::ElastiCache::SubnetGroup` is walked
/// against: a property whose name is one of them (or renames to one,
/// [`CACHE_SUBNET_GROUP_RENAMED_PROPERTIES`]) is passed through under it, and one the
/// catalogue does not carry is refused rather than dropped ([`query_members`]).
const CREATE_CACHE_SUBNET_GROUP_MEMBERS: &[(&str, QueryMember)] = &[
    ("CacheSubnetGroupDescription", QueryMember::Scalar),
    ("CacheSubnetGroupName", QueryMember::Scalar),
    ("SubnetIds", QueryMember::List("SubnetIdentifier")),
    ("Tags", QueryMember::List("Tag")),
];

/// The one property of the type whose member is spelled differently: its `Description` is
/// `CreateCacheSubnetGroup$CacheSubnetGroupDescription` ("a description for the cache subnet
/// group"), which `ModifyCacheSubnetGroup` takes under the same name. Every other property is
/// its member's own name.
const CACHE_SUBNET_GROUP_RENAMED_PROPERTIES: &[(&str, &str)] =
    &[("Description", "CacheSubnetGroupDescription")];

/// The properties the walk over the declaration does NOT turn into a create member.
const CACHE_SUBNET_GROUP_PROPERTIES_NOT_SENT: &[&str] = &[
    // The name CloudFormation decides, which is this resource's physical id ("Ref returns the
    // name of the cache subnet group"): the arm sets the member from that id, so the walk must
    // not also push the declared spelling of it.
    "CacheSubnetGroupName",
    // The tag list carries the declared tags AND the ones CloudFormation propagates to the
    // resource, so the arm builds it out of both ([`tags_of`]).
    "Tags",
];

/// Create the cache subnet group an `AWS::ElastiCache::SubnetGroup` declares, under the
/// physical id the naming rules settled — the `CacheSubnetGroupName` the declaration states,
/// or the generated name where it states none.
///
/// The subnets are resolved by the OWNING service against the ones Amazon EC2 holds
/// (`InvalidSubnet`), so a declaration naming a subnet that does not exist fails the resource
/// instead of leaving a group no cache can be placed in.
pub(crate) async fn create_cache_subnet_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_CACHE_SUBNET_GROUP_MEMBERS,
        CACHE_SUBNET_GROUP_RENAMED_PROPERTIES,
        CACHE_SUBNET_GROUP_PROPERTIES_NOT_SENT,
        ELASTICACHE_SUBNET_GROUP,
    )?;
    parameters.push((
        "CacheSubnetGroupName".to_string(),
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
        "CreateCacheSubnetGroup",
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "ARN").filter(|arn| !arn.is_empty()) {
        // The group publishes one ARN, so the conventional `Arn` attribute every type answers
        // is that same value ([`super::template::conventional_attributes`]).
        attributes.insert("Arn".to_string(), arn);
    }
    Ok(Created::attributes(attributes))
}

/// Apply a CHANGED declaration to the cache subnet group a stack already created, through
/// `ModifyCacheSubnetGroup` — the call Amazon ElastiCache changes a group's own configuration
/// with.
///
/// That call takes the group's description and its subnet list, which is everything the type
/// declares beside the name it is addressed by and its tags: a changed declaration therefore
/// reaches the group the stack created rather than being reported as a change nothing applied.
pub(crate) async fn update_cache_subnet_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let mut parameters = query_members(
        declared,
        MODIFY_CACHE_SUBNET_GROUP_MEMBERS,
        CACHE_SUBNET_GROUP_RENAMED_PROPERTIES,
        CACHE_SUBNET_GROUP_PROPERTIES_NOT_SENT,
        ELASTICACHE_SUBNET_GROUP,
    )?;
    parameters.push((
        "CacheSubnetGroupName".to_string(),
        resource.physical_resource_id.clone(),
    ));
    let answered = call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "ModifyCacheSubnetGroup",
        &parameters,
    )
    .await?;
    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "ARN").filter(|arn| !arn.is_empty()) {
        attributes.insert("Arn".to_string(), arn);
    }
    Ok(Some(Created::attributes(attributes)))
}

/// The members `ModifyCacheSubnetGroup` takes, from the same model
/// (`smithy_query(query="operation", operation="ModifyCacheSubnetGroup")`): the create's
/// catalogue less its tag list, which that call carries no member for — the tags
/// CloudFormation holds on a group that exists travel through this service's own tagging
/// calls instead.
const MODIFY_CACHE_SUBNET_GROUP_MEMBERS: &[(&str, QueryMember)] = &[
    ("CacheSubnetGroupDescription", QueryMember::Scalar),
    ("CacheSubnetGroupName", QueryMember::Scalar),
    ("SubnetIds", QueryMember::List("SubnetIdentifier")),
];

/// Delete the cache subnet group a stack created, addressed by its name — this resource's
/// physical id. A group Amazon ElastiCache no longer has
/// (`CacheSubnetGroupNotFoundFault`) is already in the state the delete asks for.
///
/// A group a cache still sits in is refused (`CacheSubnetGroupInUse`), which is the refusal a
/// client would get: the stack's own deletion order is what keeps the cache's removal ahead of
/// the group's.
pub(crate) async fn delete_cache_subnet_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let parameters = vec![(
        "CacheSubnetGroupName".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DeleteCacheSubnetGroup",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == CACHE_SUBNET_GROUP_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The group's configuration as Amazon ElastiCache reports it, for drift detection. A group it
/// no longer holds (`CacheSubnetGroupNotFoundFault`) was deleted.
///
/// A group is reported as one `<CacheSubnetGroup>` of the answer's `CacheSubnetGroups` list
/// (the element name that list's member `@xmlName` fixes — `smithy_query(query="shape",
/// shape="CacheSubnetGroups")`), whose SCALAR members are read out by name; its `Subnets` list
/// is a composite member and is left out, which is the position every arm whose property is
/// not a scalar is in.
pub(crate) async fn read_cache_subnet_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "CacheSubnetGroupName".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DescribeCacheSubnetGroups",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == CACHE_SUBNET_GROUP_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let reported = xml_scalar_children(&answered, "CacheSubnetGroup");
    if reported.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(reported)))
}
