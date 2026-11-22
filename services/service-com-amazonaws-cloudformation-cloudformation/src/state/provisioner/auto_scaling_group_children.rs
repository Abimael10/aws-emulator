// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the three types a stack declares ON an Auto Scaling
//! group of [`super::compute_fleet`]'s: the LIFECYCLE HOOK that holds an instance in a wait
//! state, the SCHEDULED ACTION that changes the group's capacity on a schedule, and the WARM
//! POOL of pre-initialized instances that sits beside it. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! ## The rule this file carries
//!
//! A stack reports a resource created only where the service that OWNS the type really holds
//! it, so a stack that reached `CREATE_COMPLETE` with one of these declared is a stack whose
//! client reads the resource back through Amazon EC2 Auto Scaling's own calls:
//! `DescribeLifecycleHooks` for the hook, `DescribeScheduledActions` for the action and
//! `DescribeWarmPool` for the pool.
//!
//! Each of the three is a CHILD of one group, which is how all three are addressed: the
//! group's name travels on every call beside the child's own name, and the pool — of which a
//! group has at most one — is addressed by the group alone. Each type's properties ARE the
//! members of the `Put…` call that states it ("creates or updates a lifecycle hook for the
//! specified Auto Scaling group", "creates or updates a scheduled scaling action", "creates
//! or updates a warm pool"), under the same names, and those calls are `awsQuery` like the
//! group's own create — so an evaluated declaration is walked by [`query_members`] and the
//! same call applies a CHANGED declaration to the child that exists. The one member the arms
//! carry themselves is the child's NAME, which CloudFormation decides and which is that
//! resource's physical id, so the child is created under that id and no identifier is taken
//! over.
//!
//! Because each `Put…` is an upsert, a changed declaration is applied IN PLACE — except one
//! that moves the child to ANOTHER group, which is a different resource: the group it was on
//! would keep the child this resource put there, so such a change is reported as not applied
//! rather than as a success that left the old group configured.

use super::*;

/// The three template resource types this file provisions.
pub(in crate::state) const AUTOSCALING_LIFECYCLE_HOOK: &str = "AWS::AutoScaling::LifecycleHook";
pub(in crate::state) const AUTOSCALING_SCHEDULED_ACTION: &str =
    "AWS::AutoScaling::ScheduledAction";
pub(in crate::state) const AUTOSCALING_WARM_POOL: &str = "AWS::AutoScaling::WarmPool";

/// The property each of the three declares the GROUP it belongs to in — "the name of the Auto
/// Scaling group", which is the member every call that addresses one of these children takes
/// beside the child's own name.
const GROUP_PROPERTY: &str = "AutoScalingGroupName";

/// The members `PutLifecycleHook` takes, from Amazon EC2 Auto Scaling's own model
/// (`smithy_query(query="operation",operation="PutLifecycleHook",
/// model_path="services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/\
/// smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json")`) — every one of
/// them a scalar, the call carrying no list or structure member at all.
///
/// This is the catalogue the declaration of an `AWS::AutoScaling::LifecycleHook` is walked
/// against: the type's properties ARE these members, so a property whose name is one of them
/// is passed through under it, and one the catalogue does not carry is refused rather than
/// dropped ([`query_members`]).
const PUT_LIFECYCLE_HOOK_MEMBERS: &[(&str, QueryMember)] = &[
    ("AutoScalingGroupName", QueryMember::Scalar),
    ("DefaultResult", QueryMember::Scalar),
    ("HeartbeatTimeout", QueryMember::Scalar),
    ("LifecycleHookName", QueryMember::Scalar),
    ("LifecycleTransition", QueryMember::Scalar),
    ("NotificationMetadata", QueryMember::Scalar),
    ("NotificationTargetARN", QueryMember::Scalar),
    ("RoleARN", QueryMember::Scalar),
];

/// The `AWS::AutoScaling::LifecycleHook` property the walk does NOT turn into a
/// `PutLifecycleHook` member: the hook's own name, which CloudFormation decides and which the
/// arm sets from this resource's physical id.
const LIFECYCLE_HOOK_PROPERTIES_NOT_SENT: &[&str] = &["LifecycleHookName"];

/// The member `DescribeLifecycleHooks` narrows its answer by, indexed from 1: "the names of
/// one or more lifecycle hooks. If you omit this property, all lifecycle hooks are
/// described", so naming the one hook is what makes the answer that hook's.
const LIFECYCLE_HOOK_NAMES_MEMBER: &str = "LifecycleHookNames.member.1";

/// The members `PutScheduledUpdateGroupAction` takes, from the same model — scalars again,
/// the three instants among them being timestamps the awsQuery wire carries as text.
///
/// The catalogue the declaration of an `AWS::AutoScaling::ScheduledAction` is walked against,
/// on the same terms as the hook's above.
const PUT_SCHEDULED_ACTION_MEMBERS: &[(&str, QueryMember)] = &[
    ("AutoScalingGroupName", QueryMember::Scalar),
    ("DesiredCapacity", QueryMember::Scalar),
    ("EndTime", QueryMember::Scalar),
    ("MaxSize", QueryMember::Scalar),
    ("MinSize", QueryMember::Scalar),
    ("Recurrence", QueryMember::Scalar),
    ("ScheduledActionName", QueryMember::Scalar),
    ("StartTime", QueryMember::Scalar),
    ("Time", QueryMember::Scalar),
    ("TimeZone", QueryMember::Scalar),
];

/// The `AWS::AutoScaling::ScheduledAction` property the walk does NOT turn into a member: the
/// action's own name, which CloudFormation decides and which is this resource's physical id.
const SCHEDULED_ACTION_PROPERTIES_NOT_SENT: &[&str] = &["ScheduledActionName"];

/// The member `DescribeScheduledActions` narrows its answer by, indexed from 1: "the names of
/// one or more scheduled actions… If you specify an unknown scheduled action, it is ignored
/// with no error".
const SCHEDULED_ACTION_NAMES_MEMBER: &str = "ScheduledActionNames.member.1";

/// The members `PutWarmPool` takes, from the same model: the group the pool sits beside, its
/// two sizes, the state its instances are held in, and the reuse policy — which is the one
/// STRUCTURE member of the three calls here.
///
/// The catalogue the declaration of an `AWS::AutoScaling::WarmPool` is walked against. A warm
/// pool has no name of its own — a group holds at most one, and every call that addresses one
/// takes the GROUP alone — so no property is held back from the walk.
const PUT_WARM_POOL_MEMBERS: &[(&str, QueryMember)] = &[
    ("AutoScalingGroupName", QueryMember::Scalar),
    ("InstanceReusePolicy", QueryMember::Structure),
    ("MaxGroupPreparedCapacity", QueryMember::Scalar),
    ("MinSize", QueryMember::Scalar),
    ("PoolState", QueryMember::Scalar),
];

/// The element `DescribeWarmPool` reports the pool's configuration under
/// (`DescribeWarmPoolAnswer$WarmPoolConfiguration`): a group with no warm pool is answered
/// with no such element at all, which is that service reporting it holds no pool.
const WARM_POOL_CONFIGURATION: &str = "WarmPoolConfiguration";

/// Put the lifecycle hook an `AWS::AutoScaling::LifecycleHook` declares, in Amazon EC2 Auto
/// Scaling, under this resource's own physical id as its `LifecycleHookName`.
///
/// Read back through that service's own `DescribeLifecycleHooks`: a create this arm reports as
/// succeeded is a hook a client resolves, so nothing is reported created that the owning
/// service does not hold.
pub(super) async fn put_lifecycle_hook(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let name = declared_name(resource);
    put_hook(ctx, region, &resource.properties, &name).await?;
    if describe_lifecycle_hook(ctx, region, &resource.properties, &name)
        .await?
        .is_none()
    {
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!(
                "PutLifecycleHook was accepted but Amazon EC2 Auto Scaling reports no \
                 lifecycle hook named {name}"
            ),
        });
    }
    Ok(Created::default())
}

/// Apply a CHANGED `AWS::AutoScaling::LifecycleHook` declaration to the hook that exists,
/// through `PutLifecycleHook` again — the one call that states a hook's configuration,
/// documented "creates or updates a lifecycle hook for the specified Auto Scaling group", so
/// the hook keeps the name it was created under.
///
/// `Ok(None)` is a change that call cannot make: a hook declared on a DIFFERENT group is a
/// different hook, and the group it was on would keep the one this resource put there.
pub(super) async fn update_lifecycle_hook(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared_group(declared) != declared_group(&resource.properties) {
        return Ok(None);
    }
    let name = declared_name(resource);
    put_hook(ctx, region, declared, &name).await?;
    Ok(Some(Created::default()))
}

/// The one `PutLifecycleHook` call both the create and the update make, out of the properties
/// in force and the name the hook is written under.
async fn put_hook(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    name: &str,
) -> Result<(), Refusal> {
    let mut parameters = query_members(
        properties,
        PUT_LIFECYCLE_HOOK_MEMBERS,
        &[],
        LIFECYCLE_HOOK_PROPERTIES_NOT_SENT,
        AUTOSCALING_LIFECYCLE_HOOK,
    )?;
    parameters.push(("LifecycleHookName".to_string(), name.to_string()));
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "PutLifecycleHook",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// Delete the lifecycle hook a stack created, addressed by the group it is on and the name it
/// was created under — this resource's physical id.
///
/// A hook Amazon EC2 Auto Scaling no longer holds is already in the state the delete asks
/// for, which is read from that service's own report of the hook rather than from the
/// refusal: every absent-resource refusal of this API is the one `ValidationError` code, so
/// the code alone cannot tell an absent hook from a request refused for another reason.
pub(super) async fn delete_lifecycle_hook(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let name = declared_name(resource);
    let Some(group) = declared_group(&resource.properties) else {
        return Ok(());
    };
    if describe_lifecycle_hook(ctx, region, &resource.properties, &name)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let parameters = vec![
        (GROUP_PROPERTY.to_string(), group),
        ("LifecycleHookName".to_string(), name),
    ];
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DeleteLifecycleHook",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// The lifecycle hook as Amazon EC2 Auto Scaling reports it NOW, or `None` where it reports no
/// such hook: the scalar members of the one entry `DescribeLifecycleHooks` answers for the
/// name, which are named exactly as this type's properties are.
///
/// This is both the read the create confirms itself with and the ACTUAL configuration drift
/// detection compares the declared one against. An awsQuery answer carries each hook as a
/// `<member>` of its `LifecycleHooks` list, and the request names ONE hook of ONE group, so
/// the first such element is that hook. The GROUP is resolved first ([`held_group`]), because
/// a hook only exists against a group that does.
pub(super) async fn describe_lifecycle_hook(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(group) = held_group(ctx, region, properties).await? else {
        return Ok(None);
    };
    let parameters = vec![
        (GROUP_PROPERTY.to_string(), group),
        (LIFECYCLE_HOOK_NAMES_MEMBER.to_string(), name.to_string()),
    ];
    let answered = call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DescribeLifecycleHooks",
        &parameters,
    )
    .await?;
    Ok(reported_child(&answered, "LifecycleHookName", name))
}

/// Put the scheduled action an `AWS::AutoScaling::ScheduledAction` declares, in Amazon EC2
/// Auto Scaling, under this resource's own physical id as its `ScheduledActionName`, and read
/// it back through that service's own `DescribeScheduledActions`.
pub(super) async fn put_scheduled_action(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let name = declared_name(resource);
    put_action(ctx, region, &resource.properties, &name).await?;
    if describe_scheduled_action(ctx, region, &resource.properties, &name)
        .await?
        .is_none()
    {
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!(
                "PutScheduledUpdateGroupAction was accepted but Amazon EC2 Auto Scaling \
                 reports no scheduled action named {name}"
            ),
        });
    }
    Ok(Created::default())
}

/// Apply a CHANGED `AWS::AutoScaling::ScheduledAction` declaration to the action that exists,
/// through `PutScheduledUpdateGroupAction` again — "creates or updates a scheduled scaling
/// action for an Auto Scaling group" — so the action keeps the name it was created under.
///
/// `Ok(None)` for a declaration moved to another group, for the hook's own reason.
pub(super) async fn update_scheduled_action(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared_group(declared) != declared_group(&resource.properties) {
        return Ok(None);
    }
    let name = declared_name(resource);
    put_action(ctx, region, declared, &name).await?;
    Ok(Some(Created::default()))
}

/// The one `PutScheduledUpdateGroupAction` call both the create and the update make.
async fn put_action(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    name: &str,
) -> Result<(), Refusal> {
    let mut parameters = query_members(
        properties,
        PUT_SCHEDULED_ACTION_MEMBERS,
        &[],
        SCHEDULED_ACTION_PROPERTIES_NOT_SENT,
        AUTOSCALING_SCHEDULED_ACTION,
    )?;
    parameters.push(("ScheduledActionName".to_string(), name.to_string()));
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "PutScheduledUpdateGroupAction",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// Delete the scheduled action a stack created, addressed by the group it is on and the name
/// it was created under, for the reason the hook's delete reads the resource back first.
pub(super) async fn delete_scheduled_action(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let name = declared_name(resource);
    let Some(group) = declared_group(&resource.properties) else {
        return Ok(());
    };
    if describe_scheduled_action(ctx, region, &resource.properties, &name)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let parameters = vec![
        (GROUP_PROPERTY.to_string(), group),
        ("ScheduledActionName".to_string(), name),
    ];
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DeleteScheduledAction",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// The scheduled action as Amazon EC2 Auto Scaling reports it NOW, or `None` where it reports
/// no such action: the scalar members of the one entry `DescribeScheduledActions` answers for
/// the name, out of that answer's `ScheduledUpdateGroupActions` list.
///
/// The group is named beside the action's name, because an action's name is unique within a
/// group rather than within the account — and is resolved first ([`held_group`]), that read
/// being refused for a group this service no longer holds rather than answering that it holds
/// no such action.
pub(super) async fn describe_scheduled_action(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(group) = held_group(ctx, region, properties).await? else {
        return Ok(None);
    };
    let parameters = vec![
        (SCHEDULED_ACTION_NAMES_MEMBER.to_string(), name.to_string()),
        (GROUP_PROPERTY.to_string(), group),
    ];
    let answered = call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DescribeScheduledActions",
        &parameters,
    )
    .await?;
    Ok(reported_child(&answered, "ScheduledActionName", name))
}

/// Put the warm pool an `AWS::AutoScaling::WarmPool` declares, in Amazon EC2 Auto Scaling, and
/// read it back through that service's own `DescribeWarmPool`.
///
/// A warm pool carries no name: it is addressed by the group it sits beside, which the
/// declaration states, so the physical id CloudFormation computed for the resource is not sent
/// anywhere.
pub(super) async fn put_warm_pool(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    write_warm_pool(ctx, region, &resource.properties).await?;
    let group = declared_group(&resource.properties).unwrap_or_default();
    if describe_warm_pool(ctx, region, &resource.properties)
        .await?
        .is_none()
    {
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!(
                "PutWarmPool was accepted but Amazon EC2 Auto Scaling reports no warm pool \
                 for the Auto Scaling group {group}"
            ),
        });
    }
    Ok(Created::default())
}

/// Apply a CHANGED `AWS::AutoScaling::WarmPool` declaration to the pool that exists, through
/// `PutWarmPool` again — "creates or updates a warm pool for the specified Auto Scaling
/// group" — so the group keeps the pool it was given rather than losing it and getting
/// another.
///
/// `Ok(None)` for a declaration moved to another group: a pool beside a DIFFERENT group is a
/// different pool, and the group it was beside would keep the one this resource put there.
pub(super) async fn update_warm_pool(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared_group(declared) != declared_group(&resource.properties) {
        return Ok(None);
    }
    write_warm_pool(ctx, region, declared).await?;
    Ok(Some(Created::default()))
}

/// The one `PutWarmPool` call both the create and the update make.
async fn write_warm_pool(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let parameters = query_members(
        properties,
        PUT_WARM_POOL_MEMBERS,
        &[],
        &[],
        AUTOSCALING_WARM_POOL,
    )?;
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "PutWarmPool",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// Delete the warm pool a stack created, addressed by the group it sits beside.
///
/// `ForceDelete` is set for the reason the Auto Scaling group's own delete sets it: the pool
/// the stack created is to be removed, and without it that service refuses the delete of a
/// pool that still holds instances (`ResourceInUse`, "specifies that the warm pool is to be
/// deleted along with all of its associated instances, without waiting for all instances to
/// be terminated") — which would leave the stack reporting a resource it could not remove.
pub(super) async fn delete_warm_pool(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let Some(group) = declared_group(&resource.properties) else {
        return Ok(());
    };
    if describe_warm_pool(ctx, region, &resource.properties)
        .await?
        .is_none()
    {
        return Ok(());
    }
    let parameters = vec![
        (GROUP_PROPERTY.to_string(), group),
        ("ForceDelete".to_string(), "true".to_string()),
    ];
    call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DeleteWarmPool",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// The warm pool as Amazon EC2 Auto Scaling reports it NOW, or `None` where it reports none:
/// the scalar members of the `WarmPoolConfiguration` `DescribeWarmPool` answers with, which
/// are named exactly as this type's properties are.
///
/// The GROUP is resolved first, through the read the group's own arms take
/// ([`held_group`]): a warm pool lives on a group, so a group that service no longer holds
/// holds no pool — and that read of the pool itself is refused for the absent group rather
/// than answering that there is none.
pub(super) async fn describe_warm_pool(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<Option<Value>, Refusal> {
    let Some(group) = held_group(ctx, region, properties).await? else {
        return Ok(None);
    };
    let parameters = vec![(GROUP_PROPERTY.to_string(), group.clone())];
    let answered = call_query(
        ctx,
        AUTOSCALING_SIGNING_NAME,
        region,
        AUTOSCALING_API_VERSION,
        "DescribeWarmPool",
        &parameters,
    )
    .await?;
    let mut reported = xml_scalar_children(&answered, WARM_POOL_CONFIGURATION);
    if reported.is_empty() {
        return Ok(None);
    }
    // The pool is reported WITHOUT the group it sits beside — that answer is the group's
    // already — so the property the declaration addresses it by is carried back on it, which
    // is what drift detection compares the declaration against.
    reported.insert(GROUP_PROPERTY.to_string(), Value::String(group));
    Ok(Some(Value::Object(reported)))
}

/// The one `<member>` of a child-listing answer whose `name_member` is `name`, as the object
/// of its scalar members — or `None` where the answer carries no such child.
///
/// The request names ONE child, so the first such element is that child; comparing the name
/// back is what keeps an answer about something else from being read as this resource. Shared
/// with the scaling policy's own read ([`super::auto_scaling_policies`]), which is the same
/// rule over the same shape of answer.
pub(super) fn reported_child(answered: &str, name_member: &str, name: &str) -> Option<Value> {
    let reported = xml_scalar_children(answered, "member");
    reported
        .get(name_member)
        .and_then(Value::as_str)
        .is_some_and(|reported_name| reported_name == name)
        .then_some(Value::Object(reported))
}

/// The group a declaration states the child on, where Amazon EC2 Auto Scaling still HOLDS it.
///
/// Every one of these children is addressed by its group, and each of that service's reads of
/// one is group-scoped: a group it no longer holds is refused rather than answered empty, and a
/// declaration that states no group names no child at all. So the group is resolved through the
/// read the group's own arms take ([`describe_auto_scaling_group`]) and `None` — no child — is
/// what either case reports, which is what lets a teardown that has already taken the group
/// read its children as gone instead of as a refused read.
async fn held_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<Option<String>, Refusal> {
    let Some(group) = declared_group(properties) else {
        return Ok(None);
    };
    Ok(describe_auto_scaling_group(ctx, region, &group)
        .await?
        .is_some()
        .then_some(group))
}
