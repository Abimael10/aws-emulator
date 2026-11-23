// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon ECS CAPACITY PROVIDER an
//! `AWS::ECS::CapacityProvider` declares. One file of [`super`]'s per-type arms, dispatched
//! to by its `Provisioner`, beside the cluster arms the same owning service has in
//! [`super::application_services`] and the service arms in [`super::ecs_services`].
//!
//! ## Why the provider is really created
//!
//! A capacity provider is a NAMED binding onto the capacity that runs a cluster's tasks, and
//! that name is the only handle Amazon ECS takes for one: `DescribeCapacityProviders` reads a
//! provider by it, `PutClusterCapacityProviders` associates one with a cluster by it, and a
//! `capacityProviderStrategy` entry on `RunTask` or `CreateService` names one by nothing
//! else. "Ref returns the capacity provider name", so a stack that reported the resource
//! created while nothing created the provider publishes a name that resolves to nothing: the
//! first read a client makes against the value the stack handed it answers no provider at
//! all, and every placement that names it is refused.
//!
//! ## The properties
//!
//! The type's properties are `CreateCapacityProvider`'s own members in UPPER camel case
//! ([`ECS_CAPACITY_PROVIDER_MEMBERS`] names each pair, and [`lower_camel_members`] re-cases
//! every member name inside a value the same way, so a declared `AutoScalingGroupProvider.\
//! ManagedScaling.TargetCapacity` reaches that call as
//! `autoScalingGroupProvider.managedScaling.targetCapacity`), with two exceptions:
//!
//! * `Name` is the name CloudFormation decided for the resource rather than a member
//!   travelling beside the others — it is this resource's own physical id, which is what
//!   `Ref` returns, so the provider is created under exactly the name the stack reports;
//! * `Tags` is rebuilt rather than carried, so the provider carries the tags CloudFormation
//!   gives the resource beside the declared ones, as the `{key, value}` list Amazon ECS's own
//!   `Tag` shape spells.
//!
//! A declared property none of those members carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a provider reported created with less than its
//! declaration asked for is the shallow success this arm exists to avoid.

use super::*;

/// The resource type these arms provision, which is also the name the rest of this service's
/// state keys its entries for the type by.
pub(crate) const ECS_CAPACITY_PROVIDER: &str = "AWS::ECS::CapacityProvider";

/// The members `CreateCapacityProvider` takes this type's properties as, as
/// (property, member) pairs.
///
/// Source: AWS's CloudFormation resource reference for the type, member by member against the
/// `CreateCapacityProviderRequest` shape
/// (`smithy_query(query="operation",operation="CreateCapacityProvider")`). A property these
/// pairs do not name is one that call has no member for; the provider's `Name` is set from
/// the resource's own identity instead, and its `tags` by [`tags_of`], which carries the
/// stack's propagated tags beside the declared ones.
const ECS_CAPACITY_PROVIDER_MEMBERS: &[(&str, &str)] = &[
    ("AutoScalingGroupProvider", "autoScalingGroupProvider"),
    ("Cluster", "cluster"),
    ("ManagedInstancesProvider", "managedInstancesProvider"),
];

/// The property a template may name the provider itself with, which is the name
/// CloudFormation decided for the resource.
const NAME_PROPERTY: &str = "Name";

/// The property carrying the provider's own tag set, merged with the ones CloudFormation
/// propagates ([`tags_of`]) and rebuilt rather than carried.
const TAGS_PROPERTY: &str = "Tags";

/// The member `CreateCapacityProvider` takes the provider's name as, and the one every other
/// capacity-provider call addresses a provider by.
const NAME_MEMBER: &str = "name";
const CAPACITY_PROVIDER_MEMBER: &str = "capacityProvider";

/// The member a create, a delete or a read answers the provider's own record under, and the
/// members of that record this arm reads.
const CAPACITY_PROVIDERS_MEMBER: &str = "capacityProviders";
const STATUS_MEMBER: &str = "status";
const UPDATE_STATUS_MEMBER: &str = "updateStatus";

/// The status Amazon ECS reports for a capacity provider whose deletion has gone through
/// (`CapacityProvider$status`: "When a capacity provider is successfully deleted, it has an
/// INACTIVE status"), the one it can be used in a cluster in ("Only capacity providers in an
/// ACTIVE state can be used in a cluster"), and the one it is reported in while that service
/// is still standing its capacity up (`CapacityProviderStatus`).
const INACTIVE: &str = "INACTIVE";
const ACTIVE: &str = "ACTIVE";
const PROVISIONING: &str = "PROVISIONING";

/// `CapacityProvider$updateStatus` for a delete Amazon ECS ANSWERED without carrying out:
/// that service reports the hold on the provider rather than refusing the call, so this is
/// what says the provider is still there ([`delete_ecs_capacity_provider`]).
const DELETE_FAILED: &str = "DELETE_FAILED";

/// The SCALAR property a drift read reports back, against the member `CapacityProvider`
/// carries it in.
///
/// The type's other properties are STRUCTURES on the wire (the Auto Scaling group provider
/// and its managed scaling, the managed-instances configuration), which is the position every
/// arm whose property is not a scalar is in and what [`drift`] documents.
const ECS_CAPACITY_PROVIDER_REPORTED_MEMBERS: &[(&str, &str)] = &[(NAME_PROPERTY, NAME_MEMBER)];

/// Create the capacity provider an `AWS::ECS::CapacityProvider` declares, in Amazon ECS
/// itself — so a client that deployed the stack can pass `!Ref <capacity provider>` straight
/// to `DescribeCapacityProviders`, to `PutClusterCapacityProviders`, or to a
/// `capacityProviderStrategy` on `RunTask` or `CreateService`.
///
/// The provider is created under this resource's own physical id, which is the name "Ref
/// returns" for the type, so no identifier is taken over and the name the stack publishes is
/// the one the provider exists under.
///
/// `Err` is Amazon ECS's own refusal of the create, which is what the resource fails with: an
/// Auto Scaling group the account does not hold, a group already backing another provider, a
/// name that account already holds over different capacity, a name outside the character set
/// that service admits.
pub(super) async fn create_ecs_capacity_provider(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type)?;
    let mut request = Map::new();
    if let Value::Object(properties) = &resource.properties {
        for (property, member) in ECS_CAPACITY_PROVIDER_MEMBERS {
            if let Some(value) = properties.get(*property).filter(|value| !value.is_null()) {
                // Read as the TYPES Amazon ECS models `CreateCapacityProvider`'s members as,
                // through the one walk every arm in this position shares: a quoted step size,
                // target capacity or flag — which is every value that reached the property
                // through a template parameter — is the number or the flag that call takes
                // ([`crate::state::provisioning::ecs_scalars::capacity_provider_scalar`]).
                request.insert(
                    (*member).to_string(),
                    modelled_declaration(
                        member,
                        &lower_camel_members(value, 0)?,
                        &crate::state::provisioning::ecs_scalars::capacity_provider_scalar,
                    ),
                );
            }
        }
    }
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("tags".to_string(), tags_as_list(&tags, LOWER_CASED_TAG));
    }
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let reported = call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.CreateCapacityProvider"),
        &Value::Object(request),
    )
    .await?;
    // A create that reported no provider created nothing this stack could publish a handle
    // to, so the resource FAILS rather than the stack reporting a name no call of that
    // service takes.
    if reported
        .get(CAPACITY_PROVIDER_MEMBER)
        .and_then(|provider| reported_text(provider, NAME_MEMBER))
        .is_none()
    {
        return Err(Refusal {
            code: "InvalidRequest".to_string(),
            message: format!(
                "Amazon ECS reported no {NAME_MEMBER} for the {} it created",
                resource.resource_type
            ),
        });
    }
    // The provider was created under the name CloudFormation decided, and the type publishes
    // no attribute of its own: `Ref` returns that name, and there is no `Fn::GetAtt` on it.
    Ok(Created::default())
}

/// Delete the capacity provider a stack created, addressed by the name it was created under.
///
/// A provider Amazon ECS no longer holds is already in the state the delete asks for. One a
/// CLUSTER still lists is NOT deleted — "Only capacity providers that aren't associated with
/// a cluster can be deleted" — and Amazon ECS reports that on the PROVIDER rather than
/// refusing the call: the answer carries the provider under the status it already had with
/// its `updateStatus` [`DELETE_FAILED`]. So the ANSWER is what decides the outcome here, not
/// the HTTP status: a provider that service did not delete fails the resource's deletion
/// instead of the stack reporting gone a resource that is still there.
pub(super) async fn delete_ecs_capacity_provider(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        CAPACITY_PROVIDER_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let reported = call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.DeleteCapacityProvider"),
        &Value::Object(request),
    )
    .await?;
    let reported = reported.get(CAPACITY_PROVIDER_MEMBER);
    let not_deleted = reported
        .and_then(|provider| reported_text(provider, UPDATE_STATUS_MEMBER))
        .as_deref()
        == Some(DELETE_FAILED);
    if not_deleted {
        // The code carries `InUse`, so the failure is reported in the class a resource
        // another resource still depends on falls in rather than as a bad request.
        return Err(Refusal {
            code: "ResourceInUse".to_string(),
            message: format!(
                "Amazon ECS did not delete the capacity provider {}: a cluster it is \
                 associated with still holds it",
                resource.physical_resource_id
            ),
        });
    }
    Ok(())
}

/// The provider's configuration as Amazon ECS reports it, for drift detection: its own
/// `DescribeCapacityProviders` entry, projected onto the property names the type declares
/// ([`ECS_CAPACITY_PROVIDER_REPORTED_MEMBERS`]).
///
/// A provider that read reports none of, and one it reports [`INACTIVE`] — the status a
/// deleted provider carries — each say the resource was deleted, which is what `None`
/// reports.
pub(super) async fn describe_ecs_capacity_provider(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(reported) = described(ctx, region, physical_resource_id).await? else {
        return Ok(None);
    };
    if reported_text(&reported, STATUS_MEMBER).as_deref() == Some(INACTIVE) {
        return Ok(None);
    }
    Ok(Some(reported_as_properties(
        &reported,
        ECS_CAPACITY_PROVIDER_REPORTED_MEMBERS,
    )))
}

/// Whether Amazon ECS has finished bringing up the capacity provider a stack created.
///
/// [`ACTIVE`] is the status a provider can be used in a cluster in — `CapacityProvider$status`:
/// "Only capacity providers in an ACTIVE state can be used in a cluster" — and
/// [`PROVISIONING`] the one a provider whose capacity that service is still standing up is
/// answered under, which it brings the provider out of by itself. Every other status is one no
/// wait will bring a provider out of: one already deleted, and one that service no longer
/// holds at all, since nothing is going to create it.
pub(super) async fn ecs_capacity_provider_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(status) = reported_status(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon ECS holds no capacity provider {physical_resource_id}"
        )));
    };
    match status.as_str() {
        ACTIVE => Ok(Readiness::Ready),
        PROVISIONING => Ok(Readiness::Pending(status)),
        other => Ok(Readiness::Failed(format!(
            "Amazon ECS reports capacity provider {physical_resource_id} as {other}"
        ))),
    }
}

/// Whether Amazon ECS has finished removing the capacity provider a stack deleted.
///
/// [`INACTIVE`] is the status "a capacity provider … successfully deleted" has, so the
/// deletion is finished there — whatever the release of the capacity it bound onto is still
/// doing, which that service reports on the provider's own update status. A provider that
/// service no longer reports at all is gone too; one it still reports LIVE is one this
/// stack's delete has not taken away, so the deletion is not finished and the wait says so.
pub(super) async fn ecs_capacity_provider_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let Some(status) = reported_status(ctx, region, physical_resource_id).await? else {
        return Ok(Removal::Gone);
    };
    if status == INACTIVE {
        return Ok(Removal::Gone);
    }
    Ok(Removal::Pending(status))
}

/// The status Amazon ECS reports the provider in, and `None` where it reports no such
/// provider at all.
async fn reported_status(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<String>, Refusal> {
    Ok(described(ctx, region, physical_resource_id)
        .await?
        .and_then(|reported| reported_text(&reported, STATUS_MEMBER)))
}

/// Amazon ECS's own `DescribeCapacityProviders` entry for the provider, by the name it was
/// created under, and `None` where that read reports no such provider — which it answers as a
/// `failures` entry beside an empty list rather than as a refusal of the call.
async fn described(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert(
        CAPACITY_PROVIDERS_MEMBER.to_string(),
        Value::Array(vec![Value::String(physical_resource_id.to_string())]),
    );
    let answered = call_json_rpc(
        ctx,
        ECS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECS_TARGET_PREFIX}.DescribeCapacityProviders"),
        &Value::Object(request),
    )
    .await?;
    Ok(first_listed(&answered, CAPACITY_PROVIDERS_MEMBER))
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a provider from being reported created with less than its
/// declaration asked for.
///
/// What the arm applies is the pair table ([`ECS_CAPACITY_PROVIDER_MEMBERS`]) plus the two
/// properties it handles itself — the `Name` the provider is created under and the `Tags` it
/// rebuilds — so a property outside that set is one no call of this arm carries.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    for property in members.keys() {
        let applied = ECS_CAPACITY_PROVIDER_MEMBERS
            .iter()
            .any(|(declared, _)| declared == property)
            || [NAME_PROPERTY, TAGS_PROPERTY].contains(&property.as_str());
        if applied {
            continue;
        }
        return Err(Refusal {
            code: "NotImplemented".to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every property of the type the arm carries reaches `CreateCapacityProvider`
    /// under that call's own member name, all the way down — and a property no member of the
    /// call takes is refused rather than dropped.
    #[test]
    fn a_declared_property_the_arm_does_not_apply_is_refused() {
        let carried = serde_json::json!({
            "Name": "providers-own-name",
            "Tags": [{"Key": "Owner", "Value": "reports"}],
            "AutoScalingGroupProvider": {
                "AutoScalingGroupArn": "fleet",
                "ManagedScaling": {"Status": "ENABLED", "TargetCapacity": 75},
            },
        });
        assert!(
            refuse_unapplied_properties(&carried, ECS_CAPACITY_PROVIDER).is_ok(),
            "case {carried:?}: every declared property is one the arm applies"
        );
        let refused = serde_json::json!({"NoSuchProperty": "anything"});
        let answer = refuse_unapplied_properties(&refused, ECS_CAPACITY_PROVIDER);
        let message = answer
            .expect_err("a property no member of the create takes is refused")
            .message;
        assert!(
            message.contains("NoSuchProperty"),
            "case {refused:?}: the refusal names the property, got {message:?}"
        );
    }

    /// RULE: the member names inside a declared property are the API's own, which for this
    /// type is each property with its initial letter lower-cased, all the way down.
    #[test]
    fn the_declared_members_reach_the_api_under_its_own_names() {
        let declared = serde_json::json!({
            "AutoScalingGroupArn": "fleet",
            "ManagedScaling": {
                "Status": "ENABLED",
                "TargetCapacity": 75,
                "MinimumScalingStepSize": 1,
                "MaximumScalingStepSize": 10,
                "InstanceWarmupPeriod": 300,
            },
            "ManagedTerminationProtection": "ENABLED",
            "ManagedDraining": "ENABLED",
        });
        let converted = lower_camel_members(&declared, 0).expect("the document is readable");
        let expected = serde_json::json!({
            "autoScalingGroupArn": "fleet",
            "managedScaling": {
                "status": "ENABLED",
                "targetCapacity": 75,
                "minimumScalingStepSize": 1,
                "maximumScalingStepSize": 10,
                "instanceWarmupPeriod": 300,
            },
            "managedTerminationProtection": "ENABLED",
            "managedDraining": "ENABLED",
        });
        assert_eq!(
            converted, expected,
            "case {declared:?}: expected {expected:?}, got {converted:?}"
        );
    }
}
