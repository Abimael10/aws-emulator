// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of an Amazon RDS GLOBAL DATABASE: the global cluster an
//! `AWS::RDS::GlobalCluster` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`.
//!
//! The type's properties are `CreateGlobalCluster`'s own members
//! ([`CREATE_GLOBAL_CLUSTER_MEMBERS`]) — the engine the global database runs, the version and
//! lifecycle support it is pinned to, the database it starts with or the source DB cluster it
//! is built around, and whether its storage is encrypted and its deletion protected — under
//! the identifier CloudFormation decides, which is this resource's physical id and what the
//! type's `Ref` returns.
//!
//! The global cluster is created in the service that OWNS it, which is the whole point of
//! provisioning it: a DB cluster joins a global database by naming it, and a client's own
//! `DescribeGlobalClusters` after the stack reports `CREATE_COMPLETE` has to report the global
//! database the template declared.

use super::*;

/// `AWS::RDS::GlobalCluster`: "Ref returns the `GlobalClusterIdentifier`", which is the
/// identifier `DescribeGlobalClusters`, `ModifyGlobalCluster` and `DeleteGlobalCluster` each
/// address one by — so this resource's physical id needs no replacing by what the create
/// answered.
pub(crate) const RDS_GLOBAL_CLUSTER: &str = "AWS::RDS::GlobalCluster";

/// The `awsQueryError` code Amazon RDS answers for a global cluster it does not hold:
/// `GlobalClusterNotFoundFault`'s own (`smithy_query(query="operation",
/// operation="DeleteGlobalCluster")`), which is what an awsQuery client branches on.
const GLOBAL_CLUSTER_NOT_FOUND: &str = "GlobalClusterNotFoundFault";

/// The members `CreateGlobalCluster` takes, from Amazon RDS's own model
/// (`smithy_query(query="operation", operation="CreateGlobalCluster")`), with the tag list's
/// element name from its list shape's member `@xmlName` (`smithy_query(query="shape",
/// shape="TagList")`).
///
/// This is the catalogue the declaration of an `AWS::RDS::GlobalCluster` is walked against:
/// the type's properties ARE these members, so a property whose name is one of them is passed
/// through under it and one the catalogue does not carry is refused rather than dropped
/// ([`query_members`]).
pub(crate) const CREATE_GLOBAL_CLUSTER_MEMBERS: &[(&str, QueryMember)] = &[
    ("DatabaseName", QueryMember::Scalar),
    ("DeletionProtection", QueryMember::Scalar),
    ("Engine", QueryMember::Scalar),
    ("EngineLifecycleSupport", QueryMember::Scalar),
    ("EngineVersion", QueryMember::Scalar),
    ("GlobalClusterIdentifier", QueryMember::Scalar),
    ("SourceDBClusterIdentifier", QueryMember::Scalar),
    ("StorageEncrypted", QueryMember::Scalar),
    ("Tags", QueryMember::List("Tag")),
];

/// The properties the walk does NOT turn into a create member: the identifier CloudFormation
/// decides, which is this resource's physical id, and the tag list it merges — the two this
/// arm pushes itself.
const GLOBAL_CLUSTER_PROPERTIES_NOT_SENT: &[&str] = &["GlobalClusterIdentifier", "Tags"];

/// The element `DescribeGlobalClusters` reports one global cluster as: `GlobalClusterList`'s
/// member `@xmlName` (`smithy_query(query="shape", shape="GlobalClusterList")`), which is the
/// list's own naming rather than the protocol's default `member`.
const GLOBAL_CLUSTER_ENTRY_ELEMENT: &str = "GlobalClusterMember";

/// Create the global database an `AWS::RDS::GlobalCluster` declares.
pub(crate) async fn create_global_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_GLOBAL_CLUSTER_MEMBERS,
        &[],
        GLOBAL_CLUSTER_PROPERTIES_NOT_SENT,
        RDS_GLOBAL_CLUSTER,
    )?;
    parameters.push((
        "GlobalClusterIdentifier".to_string(),
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
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "CreateGlobalCluster",
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "GlobalClusterArn").filter(|arn| !arn.is_empty()) {
        // The global database's own ARN, which is the resource's: every name the type
        // publishes its ARN under is re-pointed at the one Amazon RDS reports
        // ([`super::template::arn_attributes`]), under the conventional spellings and under
        // the member the owning API reports it as.
        attributes.extend(super::template::arn_attributes(RDS_GLOBAL_CLUSTER, &arn));
        attributes.insert("GlobalClusterArn".to_string(), arn);
    }
    if let Some(resource_id) =
        xml_element(&answered, "GlobalClusterResourceId").filter(|id| !id.is_empty())
    {
        attributes.insert("GlobalClusterResourceId".to_string(), resource_id);
    }
    Ok(Created::attributes(attributes))
}

/// The global database's configuration as Amazon RDS reports it, for drift detection. One it
/// no longer holds (`GlobalClusterNotFoundFault`) was deleted.
///
/// A global cluster is reported as a STRUCTURE whose scalar members are named exactly as this
/// type's properties are (`Engine`, `EngineVersion`, `DatabaseName`, `StorageEncrypted`,
/// `DeletionProtection`), so those members are the actual configuration the declared one is
/// compared against. Its composite member is not: `GlobalClusterMembers` reports the DB
/// clusters that joined it, which is no property of this type ([`super::drift`]).
pub(crate) async fn read_global_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "GlobalClusterIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeGlobalClusters",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == GLOBAL_CLUSTER_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(
        xml_entry_structures(&answered, "GlobalClusters", GLOBAL_CLUSTER_ENTRY_ELEMENT)
            .into_iter()
            .next()
            .map(Value::Object),
    )
}

/// Delete the global database a stack created, addressed by its identifier — this resource's
/// physical id. One Amazon RDS no longer holds is already in the state the delete asks for.
pub(crate) async fn delete_global_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let parameters = vec![(
        "GlobalClusterIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DeleteGlobalCluster",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == GLOBAL_CLUSTER_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
