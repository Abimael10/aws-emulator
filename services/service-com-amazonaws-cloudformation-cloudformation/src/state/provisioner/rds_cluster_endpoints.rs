// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of an Amazon RDS CUSTOM CLUSTER ENDPOINT: the endpoint an
//! `AWS::RDS::DBClusterEndpoint` declares on a DB cluster. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! The type's properties are `CreateDBClusterEndpoint`'s own members
//! ([`CREATE_CLUSTER_ENDPOINT_MEMBERS`]) — the cluster the endpoint belongs to, the kind of
//! endpoint it is and the members it routes to or leaves out — under the identifier
//! CloudFormation decides, which is this resource's physical id and what the type's `Ref`
//! returns.
//!
//! The endpoint is created in the service that OWNS it, which is the whole point of
//! provisioning it: the ADDRESS a custom endpoint serves on is Amazon RDS's to assign, so a
//! stack output over this resource's `Endpoint` has to carry the host a client really connects
//! to, and a client's own `DescribeDBClusterEndpoints` has to report the endpoint the template
//! declared.
//!
//! The create answers the endpoint `creating`, in which it resolves to nothing and serves no
//! connection, so the resource's creation is held open until Amazon RDS reports it `available`
//! ([`cluster_endpoint_readiness`]) — and the delete waits for that same status, since "the
//! requested operation can't be performed on the endpoint while the endpoint is in this
//! state".

use super::*;

/// `AWS::RDS::DBClusterEndpoint`: "Ref returns the `DBClusterEndpointIdentifier`", which is
/// the identifier `DescribeDBClusterEndpoints`, `ModifyDBClusterEndpoint` and
/// `DeleteDBClusterEndpoint` each address one by — so this resource's physical id needs no
/// replacing by what the create answered.
pub(crate) const RDS_DB_CLUSTER_ENDPOINT: &str = "AWS::RDS::DBClusterEndpoint";

/// The `awsQueryError` code Amazon RDS answers for a cluster endpoint it does not hold:
/// `DBClusterEndpointNotFoundFault`'s own (`smithy_query(query="operation",
/// operation="DeleteDBClusterEndpoint")`), which is what an awsQuery client branches on.
const CLUSTER_ENDPOINT_NOT_FOUND: &str = "DBClusterEndpointNotFoundFault";

/// The members `CreateDBClusterEndpoint` takes, from Amazon RDS's own model
/// (`smithy_query(query="operation", operation="CreateDBClusterEndpoint")`), with each list's
/// element name from its list shape's member `@xmlName` — `StringList` fixes none, so its
/// entries take the protocol's default `member`, and the tag list's is `Tag`
/// (`smithy_query(query="shape", shape="StringList,TagList")`).
///
/// This is the catalogue the declaration of an `AWS::RDS::DBClusterEndpoint` is walked
/// against: the type's properties ARE these members, so a property whose name is one of them
/// is passed through under it and one the catalogue does not carry is refused rather than
/// dropped ([`query_members`]).
pub(crate) const CREATE_CLUSTER_ENDPOINT_MEMBERS: &[(&str, QueryMember)] = &[
    ("DBClusterEndpointIdentifier", QueryMember::Scalar),
    ("DBClusterIdentifier", QueryMember::Scalar),
    ("EndpointType", QueryMember::Scalar),
    ("ExcludedMembers", QueryMember::List("member")),
    ("StaticMembers", QueryMember::List("member")),
    ("Tags", QueryMember::List("Tag")),
];

/// The properties the walk does NOT turn into a create member: the identifier CloudFormation
/// decides, which is this resource's physical id, and the tag list it merges — the two this
/// arm pushes itself.
const CLUSTER_ENDPOINT_PROPERTIES_NOT_SENT: &[&str] =
    &["DBClusterEndpointIdentifier", "Tags"];

/// The element one entry of `DBClusterEndpointMessage$DBClusterEndpoints` is reported under:
/// `DBClusterEndpointList`'s member `@xmlName` (`smithy_query(query="shape",
/// shape="DBClusterEndpointList")`), which is the list's own naming rather than the
/// protocol's default `member`.
const CLUSTER_ENDPOINT_ENTRY_ELEMENT: &str = "DBClusterEndpointList";

/// The status an endpoint Amazon RDS has finished creating serves connections in
/// (`DBClusterEndpoint$Status`).
const CLUSTER_ENDPOINT_AVAILABLE: &str = "available";

/// The statuses an endpoint is still SETTLING in, in which it serves no connection: `creating`
/// is what the create answers, and `modifying` is what a change to its membership leaves it in.
const CLUSTER_ENDPOINT_STATUSES_SETTLING: &[&str] = &["creating", "modifying"];

/// The scalar members a created endpoint publishes, each a value Amazon RDS assigns rather
/// than one the declaration could state: the host the endpoint serves on, the resource
/// identifier that service minted for it, the kind of custom endpoint it is and the status it
/// is in. Published under the member's own name, which is the attribute the type's "Return
/// values" section names.
const CLUSTER_ENDPOINT_PUBLISHED_MEMBERS: &[&str] = &[
    "CustomEndpointType",
    "DBClusterEndpointResourceIdentifier",
    "Endpoint",
    "EndpointType",
    "Status",
];

/// Create the custom cluster endpoint an `AWS::RDS::DBClusterEndpoint` declares.
///
/// What the create ANSWERS replaces the values CloudFormation could not have computed: the
/// ARN, the resource identifier Amazon RDS minted and the ADDRESS the endpoint serves on are
/// that service's to assign, and the whole point of provisioning the endpoint is that the
/// handle the stack publishes reaches the one RDS really holds.
pub(crate) async fn create_cluster_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_CLUSTER_ENDPOINT_MEMBERS,
        &[],
        CLUSTER_ENDPOINT_PROPERTIES_NOT_SENT,
        RDS_DB_CLUSTER_ENDPOINT,
    )?;
    parameters.push((
        "DBClusterEndpointIdentifier".to_string(),
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
        "CreateDBClusterEndpoint",
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) =
        xml_element(&answered, "DBClusterEndpointArn").filter(|arn| !arn.is_empty())
    {
        // The endpoint's own ARN, which is the resource's: every name the type publishes its
        // ARN under is re-pointed at the one Amazon RDS reports
        // ([`super::template::arn_attributes`]).
        attributes.extend(super::template::arn_attributes(
            RDS_DB_CLUSTER_ENDPOINT,
            &arn,
        ));
    }
    for member in CLUSTER_ENDPOINT_PUBLISHED_MEMBERS {
        if let Some(value) = xml_element(&answered, member).filter(|value| !value.is_empty()) {
            attributes.insert((*member).to_string(), value);
        }
    }
    Ok(Created::attributes(attributes))
}

/// Whether Amazon RDS reports the endpoint this provisioner created serving yet.
pub(crate) async fn cluster_endpoint_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_cluster_endpoint(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon RDS holds no DB cluster endpoint named {physical_resource_id}"
        )));
    };
    let status = reported
        .get("Status")
        .and_then(scalar_text)
        .unwrap_or_default();
    if status == CLUSTER_ENDPOINT_AVAILABLE {
        return Ok(Readiness::Ready);
    }
    if CLUSTER_ENDPOINT_STATUSES_SETTLING.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon RDS reports the DB cluster endpoint {physical_resource_id} in status {status}"
    )))
}

/// The endpoint's configuration as Amazon RDS reports it, for drift detection and for the
/// readiness arm above. An endpoint it no longer holds is `None`.
///
/// `DescribeDBClusterEndpoints` answers a LIST — it reports every endpoint of a cluster when
/// asked about a cluster — so an identifier it holds nothing under comes back as an empty list
/// rather than as a refusal, which is the `None` here. Only the scalar members take part in
/// the comparison: the membership lists are reported as lists where the properties are lists
/// too, but a list is not something `Fn::GetAtt` reports and comparing them is
/// [`super::drift`]'s own concern.
pub(crate) async fn read_cluster_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "DBClusterEndpointIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeDBClusterEndpoints",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == CLUSTER_ENDPOINT_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(xml_entry_structures(
        &answered,
        "DBClusterEndpoints",
        CLUSTER_ENDPOINT_ENTRY_ELEMENT,
    )
    .into_iter()
    .next()
    .map(Value::Object))
}

/// How often this module checks an endpoint's status, and how many times — the same bounded
/// wait every other Amazon RDS arm here keeps, for the reason stated on
/// [`super::rds_snapshots`]'s own: an upper bound on the wait, never a duration anything is
/// owed.
const CLUSTER_ENDPOINT_CHECK_INTERVAL: ::std::time::Duration =
    ::std::time::Duration::from_millis(25);
/// See [`CLUSTER_ENDPOINT_CHECK_INTERVAL`].
const CLUSTER_ENDPOINT_CHECKS: usize = 24;

/// Delete the custom cluster endpoint a stack created, addressed by its identifier — this
/// resource's physical id. An endpoint Amazon RDS no longer holds is already in the state the
/// delete asks for.
///
/// An endpoint that has not finished settling is one that service refuses the delete of, so
/// the delete waits for it first. Amazon RDS answers the delete by putting the endpoint in
/// `deleting` and removing it a moment later, so this arm then WAITS for it to be gone:
/// CloudFormation deletes a stack's resources in dependency order and waits for each deletion
/// to complete before it deletes what that resource sat on, and a cluster whose endpoint is
/// still there is still in use.
pub(crate) async fn delete_cluster_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    await_cluster_endpoint_settled(ctx, region, physical_resource_id).await;
    let parameters = vec![(
        "DBClusterEndpointIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DeleteDBClusterEndpoint",
        &parameters,
    )
    .await
    {
        Ok(_) => {
            confirm_cluster_endpoint_deleted(ctx, region, physical_resource_id).await;
            Ok(())
        }
        Err(refusal) if refusal.code == CLUSTER_ENDPOINT_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Wait until Amazon RDS reports the endpoint out of the statuses it settles in.
///
/// Returns as soon as it is no longer settling, and gives up after
/// [`CLUSTER_ENDPOINT_CHECKS`] checks — a wait that runs out is not a failure of its own; what
/// fails then is the call that needed the endpoint settled, reported as that service's own
/// refusal of it (`InvalidDBClusterEndpointStateFault`). Any refusal of the READ ends the wait
/// the same way: it says nothing about whether the endpoint is settled.
async fn await_cluster_endpoint_settled(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) {
    for _ in 0..CLUSTER_ENDPOINT_CHECKS {
        match cluster_endpoint_readiness(ctx, region, physical_resource_id).await {
            Ok(Readiness::Pending(_)) => {}
            Ok(_) | Err(_) => return,
        }
        ::tokio::time::sleep(CLUSTER_ENDPOINT_CHECK_INTERVAL).await;
    }
}

/// Wait until Amazon RDS no longer holds the endpoint whose deletion it has ACCEPTED.
///
/// Returns as soon as it is gone, and gives up after [`CLUSTER_ENDPOINT_CHECKS`] checks — the
/// delete itself succeeded, so a wait that runs out is not a failure of it; what fails then is
/// whatever the next delete of the same stack needs the endpoint gone for, reported as that
/// service's own refusal.
async fn confirm_cluster_endpoint_deleted(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) {
    for _ in 0..CLUSTER_ENDPOINT_CHECKS {
        match read_cluster_endpoint(ctx, region, physical_resource_id).await {
            Ok(None) | Err(_) => return,
            Ok(Some(_)) => {}
        }
        ::tokio::time::sleep(CLUSTER_ENDPOINT_CHECK_INTERVAL).await;
    }
}
