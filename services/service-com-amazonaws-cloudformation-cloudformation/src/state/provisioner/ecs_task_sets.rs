// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon ECS TASK SETS a stack declares: the task set an
//! `AWS::ECS::TaskSet` creates in a service whose deployment controller is not the ECS
//! scheduler, and the promotion of one of them an `AWS::ECS::PrimaryTaskSet` declares. One file
//! of [`super`]'s per-type arms, beside the cluster arms the same owning service has in
//! [`super::application_services`], the provider arms in [`super::ecs_capacity_providers`] and
//! the service arms in [`super::ecs_services`].
//!
//! ## Why the task set is really created
//!
//! A task set is the unit an EXTERNAL or `CODE_DEPLOY` deployment shifts traffic between, and
//! the only handle Amazon ECS takes for one is the `id` IT mints (`TaskSet$id`, "the ID of the
//! task set", an `ecs-svc/` identifier): `DescribeTaskSets` reads one by it, `UpdateTaskSet`
//! scales one by it, `DeleteTaskSet` removes one by it and
//! `UpdateServicePrimaryTaskSet$primaryTaskSet` promotes one by nothing else. A stack that
//! reported the resource created while nothing created the task set publishes an identifier that
//! resolves to nothing, so the deploy tool that reads the stack's output and shifts traffic onto
//! it is refused.
//!
//! ## The identities
//!
//! Neither type's identity is a name. A task set's primary identifier is the TRIPLE its own API
//! addresses one by — the cluster, the service and the id Amazon ECS minted
//! ([`ECS_TASK_SET_IDENTITY`]) — so `Ref` returns `<Cluster>|<Service>|<Id>`
//! ([`ecs_task_set_identity`]) and `Fn::GetAtt Id` reads the id itself, which is what a
//! `PrimaryTaskSet`'s `TaskSetId` and every ECS call on the task set take. A primary task set is
//! identified by the PAIR it is the primary task set OF ([`ECS_PRIMARY_TASK_SET_IDENTITY`]):
//! there is one per service, and `UpdateServicePrimaryTaskSet` addresses it by the cluster and
//! the service alone.
//!
//! The last part of a task set's triple is one no naming rule could compute
//! ([`crate::state::template::identity_is_service_assigned`] names the type), so the identifier
//! the naming rules computed is only the stand-in a declaration is resolved against until the
//! create has answered.
//!
//! ## The properties
//!
//! A task set's properties are `CreateTaskSet`'s own members in lower camel case
//! ([`ECS_TASK_SET_MEMBERS`] names each pair, and [`renamed_members`] re-cases every member name
//! inside a value the same way) with one exception: the member a network configuration carries
//! its VPC configuration in is `awsvpcConfiguration`, which no re-casing of the type's
//! `AwsVpcConfiguration` produces ([`ECS_TASK_SET_RENAMED`]). A primary task set's three
//! properties are `UpdateServicePrimaryTaskSet`'s own members the same way, with the task set
//! named by the member that call promotes one under ([`ECS_PRIMARY_TASK_SET_MEMBERS`]).
//!
//! A declared property none of those members carries is REFUSED rather than dropped: a task set
//! reported created with less than its declaration asked for is the shallow success these arms
//! exist to avoid.

use super::*;

/// The resource types these arms provision, which are also the names the rest of this service's
/// state keys its entries for the types by.
pub(in crate::state) const ECS_TASK_SET: &str = "AWS::ECS::TaskSet";
pub(in crate::state) const ECS_PRIMARY_TASK_SET: &str = "AWS::ECS::PrimaryTaskSet";

/// The properties a task set's primary identifier is made of, in the order they are joined in:
/// the cluster and the service it lives in, and the `Id` Amazon ECS minted for it.
///
/// [`crate::state::template::composite_identity`] answers `None` for such a declaration — it
/// carries no `Id` to join — so the identity is built from what the create answered instead
/// ([`ecs_task_set_identity`]).
pub(in crate::state) const ECS_TASK_SET_IDENTITY: &[&str] =
    &[CLUSTER_PROPERTY, SERVICE_PROPERTY, ID_ATTRIBUTE];

/// The properties a primary task set's primary identifier is made of: the cluster and the
/// service whose primary task set it is, both of which the declaration states — so
/// [`crate::state::template::composite_identity`] builds that identity itself.
pub(in crate::state) const ECS_PRIMARY_TASK_SET_IDENTITY: &[&str] =
    &[CLUSTER_PROPERTY, SERVICE_PROPERTY];

/// The ONE attribute `AWS::ECS::TaskSet` publishes: "Id — the ID of the task set", which is the
/// handle every Amazon ECS call on a task set takes.
pub(in crate::state) const ID_ATTRIBUTE: &str = "Id";

/// The members `CreateTaskSet` takes a task set's properties as, as (property, member) pairs.
///
/// Source: AWS's CloudFormation resource reference for the type, member by member against the
/// `CreateTaskSetRequest` shape (`smithy_query(query="operation",operation="CreateTaskSet")`). A
/// property these pairs do not name is one that call has no member for; the task set's `tags`
/// are rebuilt by [`tags_of`], which carries the stack's propagated tags beside the declared
/// ones.
const ECS_TASK_SET_MEMBERS: &[(&str, &str)] = &[
    ("CapacityProviderStrategy", "capacityProviderStrategy"),
    (CLUSTER_PROPERTY, CLUSTER_MEMBER),
    ("ExternalId", "externalId"),
    ("LaunchType", "launchType"),
    ("LoadBalancers", "loadBalancers"),
    ("NetworkConfiguration", "networkConfiguration"),
    ("PlatformVersion", "platformVersion"),
    (SCALE_PROPERTY, SCALE_MEMBER),
    (SERVICE_PROPERTY, SERVICE_MEMBER),
    ("ServiceRegistries", "serviceRegistries"),
    ("TaskDefinition", "taskDefinition"),
];

/// The member inside a declared property whose API name is NOT the property with its initial
/// letter lower-cased: `NetworkConfiguration$AwsVpcConfiguration` is Amazon ECS's
/// `NetworkConfiguration$awsvpcConfiguration` — that model's own member name, which carries no
/// capital in `vpc`.
const ECS_TASK_SET_RENAMED: &[(&str, &str)] = &[("AwsVpcConfiguration", "awsvpcConfiguration")];

/// The members `UpdateServicePrimaryTaskSet` takes an `AWS::ECS::PrimaryTaskSet`'s properties
/// as, as (property, member) pairs.
///
/// Source: AWS's CloudFormation resource reference for the type against that call's own members
/// (`smithy_query(query="operation",operation="UpdateServicePrimaryTaskSet")`): the cluster and
/// the service address the call, and the task set to promote is its `primaryTaskSet`.
const ECS_PRIMARY_TASK_SET_MEMBERS: &[(&str, &str)] = &[
    (CLUSTER_PROPERTY, CLUSTER_MEMBER),
    (SERVICE_PROPERTY, SERVICE_MEMBER),
    (TASK_SET_ID_PROPERTY, PRIMARY_TASK_SET_MEMBER),
];

/// The properties naming the cluster and the service a task set lives in, which ADDRESS every
/// call about it, and the members those calls take them as.
const CLUSTER_PROPERTY: &str = "Cluster";
const CLUSTER_MEMBER: &str = "cluster";
const SERVICE_PROPERTY: &str = "Service";
const SERVICE_MEMBER: &str = "service";

/// The property an `AWS::ECS::PrimaryTaskSet` names the task set to promote in, and the member
/// `UpdateServicePrimaryTaskSet` promotes one under.
const TASK_SET_ID_PROPERTY: &str = "TaskSetId";
const PRIMARY_TASK_SET_MEMBER: &str = "primaryTaskSet";

/// The one property of a task set that `UpdateTaskSet` can change — "a floating-point
/// percentage of the desired number of tasks to place and keep running in the task set" — and
/// the member that call takes it as.
const SCALE_PROPERTY: &str = "Scale";
const SCALE_MEMBER: &str = "scale";

/// The property carrying a task set's own tag set, merged with the ones CloudFormation
/// propagates ([`tags_of`]) and rebuilt rather than carried.
const TAGS_PROPERTY: &str = "Tags";

/// The member a create, an update or a delete answers the task set's own record under, the
/// member that record carries the id Amazon ECS minted in, and the member the reads take the
/// task sets to report in.
const TASK_SET_MEMBER: &str = "taskSet";
const TASK_SETS_MEMBER: &str = "taskSets";
const ID_MEMBER: &str = "id";
const STATUS_MEMBER: &str = "status";

/// The SCALAR members of a task set a drift read reports back, each against the property the
/// type declares it under.
///
/// A property that is a LIST or a structure on the wire (the load balancers, the network
/// configuration, the scale) is left out, which is the position every arm whose property is not
/// a scalar is in and what [`drift`] documents.
const ECS_TASK_SET_REPORTED_MEMBERS: &[(&str, &str)] = &[
    ("ExternalId", "externalId"),
    ("LaunchType", "launchType"),
    ("PlatformVersion", "platformVersion"),
    ("TaskDefinition", "taskDefinition"),
];

/// `TaskSet$status` for the task set that "is serving production traffic", which is what an
/// `AWS::ECS::PrimaryTaskSet` promotes one to.
const TASK_SET_PRIMARY: &str = "PRIMARY";

/// The error codes Amazon ECS answers about a task set, or the service that would hold one, that
/// it does not hold ("The specified task set wasn't found", "The specified service wasn't
/// found") — which is what says a delete has nothing left to remove.
const TASK_SET_NOT_FOUND: &str = "TaskSetNotFoundException";
const SERVICE_NOT_FOUND: &str = "ServiceNotFoundException";

/// Create the task set an `AWS::ECS::TaskSet` declares, in Amazon ECS itself — so a client that
/// read the stack's output finds the task set in `DescribeTaskSets`, and the deploy tool that
/// shifts traffic onto it reaches the one the template described.
///
/// The identity reported back is the TRIPLE Amazon ECS addresses a task set by, built from the
/// cluster and the service the declaration states and the id that service minted, which is what
/// this type's `Ref` returns; the id itself is the type's one published attribute.
///
/// `Err` is Amazon ECS's own refusal of the create, which is what the resource fails with: a
/// cluster or a service it does not hold, a service whose deployment controller takes no task
/// sets, a task definition it cannot resolve, a scale outside the percentage that member admits.
pub(super) async fn create_ecs_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type, task_set_applied())?;
    let cluster = required_property(resource, CLUSTER_PROPERTY)?;
    let service = required_property(resource, SERVICE_PROPERTY)?;
    let mut request = Map::new();
    if let Value::Object(properties) = &resource.properties {
        for (property, member) in ECS_TASK_SET_MEMBERS {
            if let Some(value) = properties.get(*property).filter(|value| !value.is_null()) {
                request.insert(
                    (*member).to_string(),
                    renamed_members(value, 0, ECS_TASK_SET_RENAMED)?,
                );
            }
        }
    }
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("tags".to_string(), tags_as_list(&tags, LOWER_CASED_TAG));
    }
    let reported = call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.CreateTaskSet"),
        &Value::Object(request),
    )
    .await?;
    // A create that reported no id created nothing this stack could publish a handle to, so the
    // resource FAILS rather than the stack reporting an identifier no call of that service
    // takes.
    let Some(id) = reported
        .get(TASK_SET_MEMBER)
        .and_then(|task_set| reported_text(task_set, ID_MEMBER))
    else {
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("CreateTaskSet answered no {ID_MEMBER}"),
        });
    };
    let mut attributes = BTreeMap::new();
    attributes.insert(ID_ATTRIBUTE.to_string(), id.clone());
    Ok(Created {
        physical_resource_id: Some(ecs_task_set_identity(&cluster, &service, &id)),
        attributes,
    })
}

/// Apply a CHANGED declaration to the task set the stack already has, through Amazon ECS's own
/// `UpdateTaskSet` — the one call that changes one, and the one property of the type it takes
/// ([`SCALE_PROPERTY`]).
///
/// Every other property of a task set is fixed when it is made — the cluster and the service it
/// lives in, its launch type, its task definition, the load balancers and registries it
/// registers with — and no member of that call carries one, which is why a declaration changing
/// one replaces the resource instead.
///
/// `Ok(None)` is a deployment that changed no property this call takes: nothing is asked of that
/// service for it.
pub(super) async fn update_ecs_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type, task_set_applied())?;
    let Some(scale) = declared.get(SCALE_PROPERTY).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let mut request = addressed_task_set(resource, declared);
    request.insert(
        SCALE_MEMBER.to_string(),
        renamed_members(scale, 0, ECS_TASK_SET_RENAMED)?,
    );
    let reported = call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.UpdateTaskSet"),
        &Value::Object(request),
    )
    .await?;
    let mut attributes = BTreeMap::new();
    if let Some(id) = reported
        .get(TASK_SET_MEMBER)
        .and_then(|task_set| reported_text(task_set, ID_MEMBER))
    {
        attributes.insert(ID_ATTRIBUTE.to_string(), id);
    }
    Ok(Some(Created::attributes(attributes)))
}

/// Delete the task set a stack created, addressed by the triple Amazon ECS addresses one by.
///
/// `force` is sent because the task set a stack deletes is the one it deployed, traffic and all:
/// "you must specify the `force` flag when deleting a task set that is serving production
/// traffic" (`DeleteTaskSet$force`) — the alternative being to shift the service's traffic
/// elsewhere first, which is not something a deletion of this resource can decide. A task set
/// Amazon ECS no longer holds, and a service or cluster it no longer holds, are each already in
/// the state the delete asks for — judged on that service's own CODE, never on a message that
/// carries the request's own values.
pub(super) async fn delete_ecs_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let mut request = addressed_task_set(resource, &resource.properties);
    request.insert("force".to_string(), Value::Bool(true));
    match call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.DeleteTaskSet"),
        &Value::Object(request),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal)
            if [TASK_SET_NOT_FOUND, SERVICE_NOT_FOUND, CLUSTER_NOT_FOUND]
                .contains(&refusal.code.as_str()) =>
        {
            Ok(())
        }
        Err(refusal) => Err(refusal),
    }
}

/// The task set's configuration as Amazon ECS reports it, for drift detection: its own
/// `DescribeTaskSets` entry, projected onto the property names the type declares
/// ([`ECS_TASK_SET_REPORTED_MEMBERS`]).
///
/// A task set that read reports none of, and a service or cluster Amazon ECS no longer holds,
/// each say the resource was deleted, which is what `None` reports.
pub(super) async fn describe_ecs_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(reported) = described_task_set(ctx, region, resource).await? else {
        return Ok(None);
    };
    Ok(Some(reported_as_properties(
        &reported,
        ECS_TASK_SET_REPORTED_MEMBERS,
    )))
}

/// Promote the task set an `AWS::ECS::PrimaryTaskSet` names, through Amazon ECS's own
/// `UpdateServicePrimaryTaskSet` — "modifies which task set in a service is the primary task
/// set", the one serving production traffic.
///
/// This is the type's CREATE and its UPDATE alike: there is one primary task set per service, so
/// a declaration naming a different task set is the same call with the task set the template now
/// names.
///
/// `Err` is Amazon ECS's own refusal: a cluster or service it does not hold, a task set the
/// service does not have, a service whose deployment controller has no task sets to promote
/// between.
pub(super) async fn update_service_primary_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type, primary_applied())?;
    let mut request = Map::new();
    for (property, member) in ECS_PRIMARY_TASK_SET_MEMBERS {
        if let Some(value) = declared.get(*property).and_then(scalar_text) {
            request.insert((*member).to_string(), Value::String(value));
        }
    }
    call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.UpdateServicePrimaryTaskSet"),
        &Value::Object(request),
    )
    .await?;
    // The resource's identity is the pair it is the primary task set OF, which the declaration
    // states, and the type publishes no attribute of its own.
    Ok(Created::default())
}

/// The primary task set as Amazon ECS reports it, for drift detection: the cluster and the
/// service whose primary task set this is, and the task set that service reports
/// [`TASK_SET_PRIMARY`] — which is the one the declaration asked it to promote.
///
/// A service that reports no primary task set at all — one whose task sets are all `ACTIVE`, and
/// a service or cluster Amazon ECS no longer holds — says the resource's effect is gone, which
/// is what `None` reports.
pub(super) async fn describe_service_primary_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let (Some(cluster), Some(service)) = (
        property(resource, CLUSTER_PROPERTY),
        property(resource, SERVICE_PROPERTY),
    ) else {
        return Ok(None);
    };
    let Some(reported) = described_task_sets(ctx, region, &cluster, &service, &[]).await? else {
        return Ok(None);
    };
    let Some(primary) = reported.iter().find(|task_set| {
        reported_text(task_set, STATUS_MEMBER).as_deref() == Some(TASK_SET_PRIMARY)
    }) else {
        return Ok(None);
    };
    let Some(id) = reported_text(primary, ID_MEMBER) else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    configuration.insert(CLUSTER_PROPERTY.to_string(), Value::String(cluster));
    configuration.insert(SERVICE_PROPERTY.to_string(), Value::String(service));
    configuration.insert(TASK_SET_ID_PROPERTY.to_string(), Value::String(id));
    Ok(Some(Value::Object(configuration)))
}

/// The physical resource id of an `AWS::ECS::TaskSet`: the triple Amazon ECS addresses one by —
/// the cluster, the service and the id it minted — joined the way CloudFormation joins the parts
/// of a composite identifier, which is what this type's `Ref` returns.
pub(in crate::state) fn ecs_task_set_identity(cluster: &str, service: &str, id: &str) -> String {
    crate::state::template::composite_identity_triple(cluster, service, id)
}

/// The request a call about this resource's task set is addressed by: the cluster, the service
/// and the task set's own id.
///
/// The id is the last part of this resource's physical id, which IS that triple
/// ([`ecs_task_set_identity`]), and otherwise the `Id` attribute the create reported — so a call
/// is addressed by the identifier Amazon ECS minted and never by the stand-in the naming rules
/// computed. The cluster and the service come from the declaration in force, both of which that
/// API admits as a short name or a full ARN.
fn addressed_task_set(resource: &StoredResource, declared: &Value) -> Map<String, Value> {
    let identity = resource.physical_resource_id.as_str();
    let parts = crate::state::template::composite_identity_triple_parts(identity);
    let cluster = declared
        .get(CLUSTER_PROPERTY)
        .and_then(scalar_text)
        .filter(|cluster| !cluster.is_empty())
        .or_else(|| parts.map(|(cluster, _, _)| cluster.to_string()))
        .unwrap_or_default();
    let service = declared
        .get(SERVICE_PROPERTY)
        .and_then(scalar_text)
        .filter(|service| !service.is_empty())
        .or_else(|| parts.map(|(_, service, _)| service.to_string()))
        .unwrap_or_default();
    let id = parts
        .map(|(_, _, id)| id.to_string())
        .or_else(|| resource.attributes.get(ID_ATTRIBUTE).cloned())
        .unwrap_or_else(|| identity.to_string());
    let mut request = Map::new();
    request.insert(CLUSTER_MEMBER.to_string(), Value::String(cluster));
    request.insert(SERVICE_MEMBER.to_string(), Value::String(service));
    request.insert(TASK_SET_MEMBER.to_string(), Value::String(id));
    request
}

/// Amazon ECS's own `DescribeTaskSets` entry for the task set this resource names, or `None`
/// where that read reports no such task set.
async fn described_task_set(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let addressed = addressed_task_set(resource, &resource.properties);
    let read = |member: &str| {
        addressed
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (cluster, service, id) = (
        read(CLUSTER_MEMBER),
        read(SERVICE_MEMBER),
        read(TASK_SET_MEMBER),
    );
    let reported = described_task_sets(ctx, region, &cluster, &service, &[id]).await?;
    Ok(reported.and_then(|task_sets| task_sets.into_iter().next()))
}

/// Amazon ECS's own `DescribeTaskSets` answer for a service's task sets, narrowed to `task_sets`
/// where that list names any, and `None` where it no longer holds the cluster or the service —
/// each of which it answers as a refusal of the call rather than as an empty list.
async fn described_task_sets(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster: &str,
    service: &str,
    task_sets: &[String],
) -> Result<Option<Vec<Value>>, Refusal> {
    let mut request = Map::new();
    request.insert(CLUSTER_MEMBER.to_string(), Value::String(cluster.to_string()));
    request.insert(SERVICE_MEMBER.to_string(), Value::String(service.to_string()));
    if !task_sets.is_empty() {
        request.insert(
            TASK_SETS_MEMBER.to_string(),
            Value::Array(
                task_sets
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            ),
        );
    }
    let answered = match call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.DescribeTaskSets"),
        &Value::Object(request),
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal)
            if [SERVICE_NOT_FOUND, CLUSTER_NOT_FOUND].contains(&refusal.code.as_str()) =>
        {
            return Ok(None)
        }
        Err(refusal) => return Err(refusal),
    };
    let reported = answered
        .get(TASK_SETS_MEMBER)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if reported.is_empty() {
        return Ok(None);
    }
    Ok(Some(reported))
}

/// One property the type REQUIRES, as the text the API member takes it as.
///
/// A declaration carrying none describes no task set at all — `CreateTaskSet` requires the
/// cluster and the service, and this resource's own identity is built from both — so it is
/// refused here rather than with a request that service would have to reject.
fn required_property(resource: &StoredResource, name: &str) -> Result<String, Refusal> {
    property(resource, name).ok_or_else(|| Refusal {
        code: MISSING_PROPERTY_CODE.to_string(),
        message: format!("{} declares no {name}", resource.resource_type),
    })
}

/// The properties an `AWS::ECS::TaskSet` arm applies: the pair table plus the `Tags` it
/// rebuilds.
fn task_set_applied() -> Vec<&'static str> {
    ECS_TASK_SET_MEMBERS
        .iter()
        .map(|(property, _)| *property)
        .chain([TAGS_PROPERTY])
        .collect()
}

/// The properties an `AWS::ECS::PrimaryTaskSet` arm applies, which are the three that call
/// takes.
fn primary_applied() -> Vec<&'static str> {
    ECS_PRIMARY_TASK_SET_MEMBERS
        .iter()
        .map(|(property, _)| *property)
        .collect()
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a resource from being reported created with less than its
/// declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    applied: Vec<&'static str>,
) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    match unmapped_property(members, &applied) {
        Some(property) => Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record one row of the tables below provisions.
    fn stored(resource_type: &str, identity: &str, properties: &Value) -> StoredResource {
        StoredResource {
            logical_id: "TaskSet".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: identity.to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties: properties.clone(),
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The three values a call about a task set is addressed by, as the table below states
    /// them.
    type Triple = (&'static str, &'static str, &'static str);

    /// One row of that table: the case, the identity the resource carries, the properties it
    /// declares and the triple the call must be addressed by.
    type AddressedRow = (&'static str, &'static str, Value, Triple);

    /// RULE: a call about a task set is addressed by the triple Amazon ECS addresses one by —
    /// the cluster, the service and the id THAT service minted, which is the last part of this
    /// resource's own identity rather than the stand-in the naming rules computed.
    #[test]
    fn a_call_is_addressed_by_the_minted_identifier() {
        let rows: &[AddressedRow] = &[
            (
                "the identity the create reported",
                "orders|checkout|ecs-svc/0000000000000000001",
                serde_json::json!({ "Cluster": "orders", "Service": "checkout" }),
                ("orders", "checkout", "ecs-svc/0000000000000000001"),
            ),
            (
                "a cluster and a service named by their ARNs",
                "arn:aws:ecs:us-east-1:000000000000:cluster/orders|checkout|ecs-svc/0000000000000000002",
                serde_json::json!({
                    "Cluster": "arn:aws:ecs:us-east-1:000000000000:cluster/orders",
                    "Service": "checkout",
                }),
                (
                    "arn:aws:ecs:us-east-1:000000000000:cluster/orders",
                    "checkout",
                    "ecs-svc/0000000000000000002",
                ),
            ),
            (
                "an identity that is no triple at all: the id is the identity itself",
                "a-stand-in-name",
                serde_json::json!({ "Cluster": "orders", "Service": "checkout" }),
                ("orders", "checkout", "a-stand-in-name"),
            ),
        ];
        for (case, identity, properties, expected) in rows {
            let resource = stored(ECS_TASK_SET, identity, properties);
            let request = addressed_task_set(&resource, properties);
            let addressed = (
                request[CLUSTER_MEMBER].as_str().unwrap_or_default(),
                request[SERVICE_MEMBER].as_str().unwrap_or_default(),
                request[TASK_SET_MEMBER].as_str().unwrap_or_default(),
            );
            assert_eq!(
                addressed, *expected,
                "case {case:?}: expected the call addressed to {expected:?}, got {addressed:?}"
            );
        }
    }

    /// RULE: the task set's identity is the triple its own API addresses one by, joined the way
    /// CloudFormation joins the parts of a composite identifier — and it reads back as those
    /// three parts.
    #[test]
    fn the_identity_is_the_cluster_the_service_and_the_minted_id() {
        let identity = ecs_task_set_identity("orders", "checkout", "ecs-svc/0000000000000000003");
        assert_eq!(
            identity, "orders|checkout|ecs-svc/0000000000000000003",
            "case \"a task set's identity\": expected the triple, got {identity:?}"
        );
        let parts = crate::state::template::composite_identity_triple_parts(&identity);
        assert_eq!(
            parts,
            Some(("orders", "checkout", "ecs-svc/0000000000000000003")),
            "case \"a task set's identity\": expected it to read back as its parts, got {parts:?}"
        );
    }

    /// RULE: every property of each type reaches its call under that call's own member name, all
    /// the way down — and a property no member of the call takes is refused rather than dropped.
    #[test]
    fn a_property_the_calls_do_not_carry_is_refused() {
        // (case, the type, the properties it applies, a property it does not)
        let rows: &[(&str, &str, Value, Value)] = &[
            (
                "a task set",
                ECS_TASK_SET,
                serde_json::json!({
                    "Cluster": "orders",
                    "Service": "checkout",
                    "TaskDefinition": "a-family:1",
                    "LaunchType": "EC2",
                    "Scale": { "Unit": "PERCENT", "Value": 50 },
                    "Tags": [{ "Key": "Tier", "Value": "gold" }],
                }),
                serde_json::json!({ "Cluster": "orders", "NotAProperty": true }),
            ),
            (
                "a primary task set",
                ECS_PRIMARY_TASK_SET,
                serde_json::json!({
                    "Cluster": "orders",
                    "Service": "checkout",
                    "TaskSetId": "ecs-svc/0000000000000000001",
                }),
                serde_json::json!({ "Cluster": "orders", "Tags": [] }),
            ),
        ];
        for (case, resource_type, accepted, refused) in rows {
            let applied = if *resource_type == ECS_TASK_SET {
                task_set_applied()
            } else {
                primary_applied()
            };
            assert!(
                refuse_unapplied_properties(accepted, resource_type, applied.clone()).is_ok(),
                "case {case:?}: expected the properties this arm applies to be admitted, got a \
                 refusal"
            );
            let answer = refuse_unapplied_properties(refused, resource_type, applied);
            assert!(
                answer.is_err(),
                "case {case:?}: expected a property the arm does not apply to be refused, got \
                 {answer:?}"
            );
        }
    }

    /// RULE: the member names inside a declared property are the API's own — each property with
    /// its initial letter lower-cased, except the one member Amazon ECS spells differently.
    #[test]
    fn the_declared_members_reach_the_api_under_its_own_names() {
        let declared = serde_json::json!({
            "AwsVpcConfiguration": {
                "Subnets": ["subnet-0123456789abcdef0"],
                "SecurityGroups": ["sg-0123456789abcdef0"],
                "AssignPublicIp": "DISABLED",
            },
        });
        let converted = renamed_members(&declared, 0, ECS_TASK_SET_RENAMED)
            .expect("the document is readable");
        let expected = serde_json::json!({
            "awsvpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"],
                "assignPublicIp": "DISABLED",
            },
        });
        assert_eq!(
            converted, expected,
            "case {declared:?}: expected {expected:?}, got {converted:?}"
        );
    }
}
