// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon ElastiCache's SERVERLESS cache: the cache an
//! `AWS::ElastiCache::ServerlessCache` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`, beside the provisioned cluster's
//! ([`super::elasticache_caches`]).
//!
//! The type's properties ARE `CreateServerlessCache`'s own members, spelled identically —
//! `Engine`, `MajorEngineVersion`, `Description`, `SubnetIds`, `SecurityGroupIds`,
//! `CacheUsageLimits` and the rest — so the declaration is walked against that operation's
//! member catalogue ([`CREATE_SERVERLESS_CACHE_MEMBERS`]) exactly as the cluster's is, and a
//! property the catalogue does not carry is refused rather than dropped. Which is the point
//! of creating the cache in the owning service at all: every rule `CreateServerlessCache`
//! applies to a declared property — the subnets resolved against the ones Amazon EC2 holds,
//! the engine's own major versions, the usage-limit bounds — decides the resource's fate, so
//! a template ElastiCache would refuse fails the resource instead of being reported created.
//!
//! "Ref returns the serverless cache name", which is the `ServerlessCacheName` the
//! declaration states (the convention [`super::super::template::naming_rules`] already finds,
//! the type's last segment being `ServerlessCache`) and the identifier every ElastiCache call
//! on one takes, so the resource's physical id needs no replacing by what the create
//! answered.
//!
//! `CreateServerlessCache` answers the cache `creating`, in which it serves nothing and
//! carries no endpoint, so the resource's creation is held open until ElastiCache reports it
//! `available` ([`serverless_cache_readiness`]) and the endpoints the type publishes are read
//! off it then ([`serverless_cache_ready_attributes`]).

use super::*;

/// `AWS::ElastiCache::ServerlessCache`: "Ref returns the serverless cache name", which is
/// the identifier `DescribeServerlessCaches`, `ModifyServerlessCache` and
/// `DeleteServerlessCache` each address one by.
pub(crate) const ELASTICACHE_SERVERLESS_CACHE: &str = "AWS::ElastiCache::ServerlessCache";

/// The error code Amazon ElastiCache answers for a serverless cache it does not hold:
/// `ServerlessCacheNotFoundFault`'s own `awsQueryError` code
/// (`smithy_query(query="operation", operation="DeleteServerlessCache")`), which is what an
/// awsQuery client branches on.
const SERVERLESS_CACHE_NOT_FOUND: &str = "ServerlessCacheNotFoundFault";

/// The `ServerlessCache$Status` a cache is usable in, and the ones a NEW cache is still
/// coming up out of. The member is a plain `String` whose documentation lists the lifecycle
/// ("The allowed values are CREATING, AVAILABLE, DELETING, CREATE-FAILED and MODIFYING"),
/// and the wire value a client compares against is the lower-case spelling this service
/// reports every resource's lifecycle in. A cache in any other status is in one no wait
/// brings a new cache out of, which is a resource that failed to come up rather than one
/// still coming.
const SERVERLESS_CACHE_STATUS_AVAILABLE: &str = "available";
const SERVERLESS_CACHE_STATUSES_COMING_UP: &[&str] = &["creating", "modifying"];

/// The element one entry of the answer's `ServerlessCaches` list is reported as:
/// `ServerlessCacheList`'s own member fixes no `@xmlName`
/// (`smithy_query(query="shape", shape="ServerlessCacheList")`), so the element is the
/// Smithy default `member` — unlike the cluster list, whose member names itself
/// `CacheCluster`.
const SERVERLESS_CACHE_ENTRY: &str = "member";

/// The members `CreateServerlessCache` takes, from Amazon ElastiCache's own model
/// (`smithy_query(query="operation", operation="CreateServerlessCache")`), with each list's
/// element name from its list shape's member `@xmlName`
/// (`smithy_query(query="shape", shape="SubnetIdsList,SecurityGroupIdsList,SnapshotArnsList,
/// TagList")`) and the usage limits as the STRUCTURE the member's own shape is
/// (`CacheUsageLimits` of `DataStorage` and `ECPUPerSecond`).
///
/// This is the catalogue the declaration of an `AWS::ElastiCache::ServerlessCache` is walked
/// against: the type's properties ARE these members, so a property whose name is one of them
/// is passed through under it.
const CREATE_SERVERLESS_CACHE_MEMBERS: &[(&str, QueryMember)] = &[
    ("CacheUsageLimits", QueryMember::Structure),
    ("ConnectionType", QueryMember::Scalar),
    ("DailySnapshotTime", QueryMember::Scalar),
    ("Description", QueryMember::Scalar),
    ("Engine", QueryMember::Scalar),
    ("KmsKeyId", QueryMember::Scalar),
    ("MajorEngineVersion", QueryMember::Scalar),
    ("NetworkType", QueryMember::Scalar),
    ("SecurityGroupIds", QueryMember::List("SecurityGroupId")),
    ("ServerlessCacheName", QueryMember::Scalar),
    ("SnapshotArnsToRestore", QueryMember::List("SnapshotArn")),
    ("SnapshotRetentionLimit", QueryMember::Scalar),
    ("SubnetIds", QueryMember::List("SubnetId")),
    ("Tags", QueryMember::List("Tag")),
    ("UserGroupId", QueryMember::Scalar),
];

/// Every property of the type is its `CreateServerlessCache` member's own name, so none is
/// renamed on the way to that call — the empty catalogue [`query_members`] takes beside the
/// member one.
const SERVERLESS_CACHE_RENAMED_PROPERTIES: &[(&str, &str)] = &[];

/// The properties of the type the walk over the declaration does NOT turn into a
/// `CreateServerlessCache` member. Every other property the catalogue does not carry is
/// refused rather than dropped ([`query_members`]).
const SERVERLESS_CACHE_PROPERTIES_NOT_SENT: &[&str] = &[
    // The name CloudFormation decides, which is this resource's physical id ("Ref returns
    // the serverless cache name"): the arm sets the member from that id, so the walk must
    // not also push the declared spelling of it.
    "ServerlessCacheName",
    // The tag list carries the declared tags AND the ones CloudFormation propagates to the
    // resource, so the arm builds it out of both ([`tags_of`]).
    "Tags",
    // A member of a DIFFERENT call: `DeleteServerlessCache$FinalSnapshotName` is "the name
    // of the final snapshot taken of a cache before it's deleted", which says nothing about
    // the cache a create makes — which is why [`delete_serverless_cache`] reads it off the
    // resource's own record when it deletes the cache.
    "FinalSnapshotName",
    // Read-only members CloudFormation publishes as this type's RETURN values: a create
    // neither takes them nor could be told them.
    "Endpoint",
    "ReaderEndpoint",
];

/// Create the serverless cache an `AWS::ElastiCache::ServerlessCache` declares, under the
/// physical id the naming rules settled — the `ServerlessCacheName` the declaration states,
/// or the generated name where it states none.
///
/// What the create ANSWERS replaces nothing of the identity: the cache is created under the
/// identifier this resource already carries, which is what the type's `Ref` returns. Its ARN
/// is read off that answer, because it is ElastiCache's to mint; the endpoints are not —
/// `CreateServerlessCache` answers a cache that is still `creating`, which has none yet, so
/// the endpoint attributes are read once that service reports the cache up
/// ([`serverless_cache_ready_attributes`]).
pub(crate) async fn create_serverless_cache(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_SERVERLESS_CACHE_MEMBERS,
        SERVERLESS_CACHE_RENAMED_PROPERTIES,
        SERVERLESS_CACHE_PROPERTIES_NOT_SENT,
        ELASTICACHE_SERVERLESS_CACHE,
    )?;
    parameters.push((
        "ServerlessCacheName".to_string(),
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
        "CreateServerlessCache",
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

/// Whether Amazon ElastiCache reports the serverless cache a stack created UP.
///
/// A cache the create has just answered for is `creating`: it has no endpoint and serves no
/// client, so the resource's creation is held open until that service reports it
/// `available`. A status no new cache comes out of — `create-failed`, and a cache
/// ElastiCache does not hold at all — is a failure, because nothing is going to bring it up.
pub(crate) async fn serverless_cache_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_serverless_cache(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon ElastiCache holds no serverless cache named {physical_resource_id}"
        )));
    };
    let status = reported
        .get("Status")
        .and_then(scalar_text)
        .unwrap_or_default();
    if status == SERVERLESS_CACHE_STATUS_AVAILABLE {
        return Ok(Readiness::Ready);
    }
    if SERVERLESS_CACHE_STATUSES_COMING_UP.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon ElastiCache reports the serverless cache {physical_resource_id} in status \
         {status}"
    )))
}

/// The attributes the type publishes off the cache itself, read once ElastiCache reports it
/// up: a cache still `creating` has no endpoint, so the create's answer carries none.
///
/// The type publishes `Endpoint.Address` / `Endpoint.Port` (the address a client writes to),
/// `ReaderEndpoint.Address` / `ReaderEndpoint.Port` (the one it reads from), and the three
/// values that service decides as it provisions the cache — its `Status`, its `CreateTime`
/// and the `FullEngineVersion` it settled on for the declared major version. Every one is
/// read off ElastiCache's own record of the cache, so nothing published here is a value
/// CloudFormation invented.
pub(crate) async fn serverless_cache_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let mut attributes = BTreeMap::new();
    let Some(reported) = read_serverless_cache(ctx, region, physical_resource_id).await? else {
        return Ok(attributes);
    };
    for endpoint in ["Endpoint", "ReaderEndpoint"] {
        for member in ["Address", "Port"] {
            let Some(value) = reported
                .get(endpoint)
                .and_then(|published| published.get(member))
                .and_then(scalar_text)
            else {
                continue;
            };
            attributes.insert(format!("{endpoint}.{member}"), value);
        }
    }
    for member in ["Status", "CreateTime", "FullEngineVersion"] {
        if let Some(value) = reported.get(member).and_then(scalar_text) {
            attributes.insert(member.to_string(), value);
        }
    }
    Ok(attributes)
}

/// Delete the serverless cache a stack created, addressed by its identifier — this
/// resource's physical id. A cache Amazon ElastiCache no longer has
/// (`ServerlessCacheNotFoundFault`) is already in the state the delete asks for.
///
/// `DeleteServerlessCache$FinalSnapshotName` is "the name of the final snapshot taken of a
/// cache before it's deleted", which the type publishes as its own `FinalSnapshotName`
/// property and which a `Snapshot` deletion policy asks for as well
/// ([`super::provisioning::Policies`]); the declared name is the more specific statement
/// about this cache, so it is what the call carries where the declaration states one.
/// Without either the call carries no snapshot member at all, which is the form
/// CloudFormation makes for the `Delete` policy.
pub(crate) async fn delete_serverless_cache(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    final_snapshot: Option<&str>,
) -> Result<(), Refusal> {
    let mut parameters = vec![(
        "ServerlessCacheName".to_string(),
        resource.physical_resource_id.clone(),
    )];
    let declared = resource
        .properties
        .get("FinalSnapshotName")
        .and_then(scalar_text)
        .filter(|name| !name.is_empty());
    if let Some(snapshot_name) = declared.or_else(|| final_snapshot.map(str::to_string)) {
        parameters.push(("FinalSnapshotName".to_string(), snapshot_name));
    }
    match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DeleteServerlessCache",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == SERVERLESS_CACHE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon ElastiCache has finished removing the serverless cache a stack deleted.
///
/// `DeleteServerlessCache` leaves the cache `deleting` while that service takes it away, so
/// the cache is still there when the delete answers: the deletion is finished once that
/// service no longer holds it, which is what keeps a stack from deleting the subnets and the
/// security groups the cache still sits in.
pub(crate) async fn serverless_cache_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_serverless_cache(ctx, region, physical_resource_id).await? {
        Some(reported) => Ok(Removal::Pending(
            reported
                .get("Status")
                .and_then(scalar_text)
                .unwrap_or_default(),
        )),
        None => Ok(Removal::Gone),
    }
}

/// The cache's configuration as Amazon ElastiCache reports it, for the readiness and removal
/// reads above and for drift detection. A cache it no longer holds
/// (`ServerlessCacheNotFoundFault`) was deleted.
///
/// A cache is reported as one entry of the answer's `ServerlessCaches` list
/// ([`SERVERLESS_CACHE_ENTRY`]), whose SCALAR members are read out by name; `Endpoint` and
/// `ReaderEndpoint` are structures, so each is read out of the same document on its own
/// ([`xml_scalar_children`] leaves a composite member out).
///
/// Both readings are scoped to the one cache because the READ is: `DescribeServerlessCaches` is
/// addressed by `ServerlessCacheName`, so the answer carries that cache or nothing. Which is
/// also what makes the generic entry element safe to read by name — the entry's own lists name
/// their elements (`SubnetId`, `SecurityGroupId`), so it holds no nested entry element, and
/// [`xml_scalar_children`] stops at the first one's end tag.
pub(crate) async fn read_serverless_cache(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "ServerlessCacheName".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        ELASTICACHE_SIGNING_NAME,
        region,
        ELASTICACHE_API_VERSION,
        "DescribeServerlessCaches",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == SERVERLESS_CACHE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let mut reported = xml_scalar_children(&answered, SERVERLESS_CACHE_ENTRY);
    if reported.is_empty() {
        return Ok(None);
    }
    for endpoint in ["Endpoint", "ReaderEndpoint"] {
        let published = xml_scalar_children(&answered, endpoint);
        if !published.is_empty() {
            reported.insert(endpoint.to_string(), Value::Object(published));
        }
    }
    Ok(Some(Value::Object(reported)))
}
