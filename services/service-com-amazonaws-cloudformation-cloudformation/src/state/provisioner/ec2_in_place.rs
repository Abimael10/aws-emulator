// SPDX-License-Identifier: CC0-1.0

//! Applying a CHANGED declaration to an Amazon EC2 resource that already exists.
//!
//! ## The rule this module carries
//!
//! A property a resource can take a new value for while it goes on existing is applied TO
//! THAT RESOURCE: CloudFormation sends the call the owning service publishes for it, the
//! resource keeps the identifier that service minted, and the logical resource reports
//! `UPDATE_COMPLETE`. A property it cannot is create-only, and a declaration restating one
//! asks for a different resource, which the deployment REPLACES
//! ([`super::template::replacement_properties`] is where each type's create-only list lives).
//! There is no third answer for a property AWS publishes as updatable: a supported property
//! change is never refused.
//!
//! So each arm here is the same shape: the properties this deployment states DIFFERENTLY
//! ([`changed_properties`]) are compared against the ones the type's own update calls carry,
//! and
//!
//! * every change among them is applied, through the call that owns it — a VPC's and a
//!   subnet's switches through the `Modify…Attribute` call that writes one attribute, a
//!   security group's rules through the authorize and revoke calls that add and remove one,
//!   an instance's policies through `ModifyInstanceAttribute`;
//! * a change OUTSIDE them is reported as not applied ([`UNAPPLIED`] answers `None`), which is
//!   what the deployment turns into the resource's failure rather than a success that dropped
//!   the caller's change.
//!
//! `Tags` is in every type's updatable set and is applied by NO arm here: the tag set a
//! resource must end up carrying is reconciled after the configuration change, by the one
//! re-tagging call pair Amazon EC2 publishes ([`retag_ec2_resource`]), because a deployment
//! can state a stack's tags differently with no resource's declaration changing at all.

use super::*;

/// What an arm answers for a change it did not apply: the deployment reports the change as
/// NOT applied rather than as a success that dropped it
/// ([`super::provisioning::reconcile`] words it).
pub(crate) const UNAPPLIED: Option<Created> = None;

/// What an arm answers for a change it applied to the resource that exists: the resource keeps
/// its identity (no physical id is minted) and publishes no new attribute.
pub(crate) fn applied_in_place() -> Option<Created> {
    Some(Created::attributes(BTreeMap::new()))
}

/// The properties this deployment states DIFFERENTLY from the ones in force.
///
/// The SAME reading every other in-place arm of this module makes
/// ([`super::iam_identities::changed_properties`]): a property either document carries and the
/// other does not is a change — one the declaration DROPPED is asking for the resource without
/// it — and so is one both carry with different values. Re-exported here rather than written
/// again, so the arms below and the ones that ask the same question of another service cannot
/// read two revisions of a declaration differently.
pub(in crate::state) use super::iam_identities::changed_properties;

/// Whether every property of `changed` is one the type's update path carries, so the change
/// can be applied in full.
///
/// An arm asks this BEFORE it calls anything: a declaration that changes one property the
/// owning service has no call for and one it does is reported as not applied as a whole,
/// rather than half-applied with the rest dropped.
pub(crate) fn every_change_applied(changed: &[String], applied: &[&str]) -> bool {
    changed
        .iter()
        .all(|property| applied.contains(&property.as_str()))
}

/// The `AWS::EC2::VPC` properties a VPC that already exists can take a new value for: the two
/// DNS switches, which are VPC ATTRIBUTES (`ModifyVpcAttribute`), the instance tenancy, which
/// `ModifyVpcTenancy` writes, and the tag set.
///
/// Source: AWS's CloudFormation resource reference for the type — every other property of it
/// carries "Update requires: `Replacement`"
/// ([`super::template::replacement_properties`] lists them).
pub(crate) const EC2_VPC_UPDATABLE_PROPERTIES: &[&str] = &[
    "EnableDnsHostnames",
    "EnableDnsSupport",
    "InstanceTenancy",
    "Tags",
];

/// Apply a changed `AWS::EC2::VPC` declaration to the VPC the stack created, through the calls
/// that write one attribute of a VPC that exists.
///
/// The VPC keeps the `vpc-…` id Amazon EC2 assigned it, so everything that references it goes
/// on resolving. A switch the declaration DROPPED is written as the value that attribute has
/// when a template states none — `false`, which is what a VPC created without it carries
/// ("by default, DNS hostnames are not enabled") — so the VPC ends up in the state the
/// template describes rather than keeping a value the template no longer asks for.
pub(crate) async fn update_ec2_vpc(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if !every_change_applied(&changed, EC2_VPC_UPDATABLE_PROPERTIES) {
        return Ok(UNAPPLIED);
    }
    let vpc_id = resource.physical_resource_id.clone();
    for attribute in EC2_VPC_ATTRIBUTES {
        if !changed.iter().any(|property| property == attribute) {
            continue;
        }
        let value = declared
            .get(*attribute)
            .cloned()
            .unwrap_or(Value::Bool(false));
        modify_ec2_attribute(
            ctx,
            region,
            MODIFY_VPC_ATTRIBUTE,
            &vpc_id,
            attribute,
            &value,
            true,
        )
        .await?;
    }
    // "Modifies the instance tenancy attribute of the specified VPC" — the one call that
    // changes it on a VPC that exists, taking the tenancy as its own plain member. A
    // declaration that DROPPED the property states no tenancy for the call to carry, and that
    // service documents no default a VPC goes back to, so the VPC keeps the tenancy it has and
    // the change is reported as one this arm did not apply.
    if changed.iter().any(|property| property == "InstanceTenancy") {
        let Some(tenancy) = declared.get("InstanceTenancy") else {
            return Ok(UNAPPLIED);
        };
        call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            "ModifyVpcTenancy",
            &[
                ("VpcId".to_string(), vpc_id.clone()),
                ("InstanceTenancy".to_string(), attribute_text(tenancy)),
            ],
        )
        .await?;
    }
    Ok(applied_in_place())
}

/// The `AWS::EC2::Subnet` properties a subnet that already exists can take a new value for:
/// the switches `ModifySubnetAttribute` writes — the three booleans and the members of
/// `PrivateDnsNameOptionsOnLaunch` — and the tag set.
///
/// Source: the type's own resource reference, read the same way [`EC2_VPC_UPDATABLE_PROPERTIES`]
/// is.
pub(crate) const EC2_SUBNET_UPDATABLE_PROPERTIES: &[&str] = &[
    "AssignIpv6AddressOnCreation",
    "EnableDns64",
    "MapPublicIpOnLaunch",
    "PrivateDnsNameOptionsOnLaunch",
    "Tags",
];

/// Apply a changed `AWS::EC2::Subnet` declaration to the subnet the stack created, through the
/// one call that writes an attribute of a subnet that exists.
///
/// The subnet keeps the `subnet-…` id Amazon EC2 assigned it. A switch the declaration dropped
/// is written `false` for the reason [`update_ec2_vpc`] records, and a member of the private
/// DNS options it dropped is left as it is: that structure's members have no documented value
/// for a subnet created without them, so writing one would be inventing it.
pub(crate) async fn update_ec2_subnet(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if !every_change_applied(&changed, EC2_SUBNET_UPDATABLE_PROPERTIES) {
        return Ok(UNAPPLIED);
    }
    let subnet_id = resource.physical_resource_id.clone();
    for attribute in EC2_SUBNET_BOOLEAN_ATTRIBUTES {
        if !changed.iter().any(|property| property == attribute) {
            continue;
        }
        let value = declared
            .get(*attribute)
            .cloned()
            .unwrap_or(Value::Bool(false));
        modify_ec2_attribute(
            ctx,
            region,
            MODIFY_SUBNET_ATTRIBUTE,
            &subnet_id,
            attribute,
            &value,
            true,
        )
        .await?;
    }
    if changed
        .iter()
        .any(|property| property == "PrivateDnsNameOptionsOnLaunch")
    {
        for (property, member, boolean) in EC2_SUBNET_PRIVATE_DNS_ATTRIBUTES {
            let Some(value) = declared
                .get("PrivateDnsNameOptionsOnLaunch")
                .and_then(|options| options.get(*property))
            else {
                continue;
            };
            modify_ec2_attribute(
                ctx,
                region,
                MODIFY_SUBNET_ATTRIBUTE,
                &subnet_id,
                member,
                value,
                *boolean,
            )
            .await?;
        }
    }
    Ok(applied_in_place())
}

/// The `AWS::EC2::SecurityGroup` properties a group that already exists can take a new value
/// for: its two inline rule lists, which are authorize and revoke calls on the group, and the
/// tag set. The group's description, its name and the VPC it is in are create-only — no call of
/// that service changes one — so a declaration restating one replaces the group.
pub(crate) const EC2_SECURITY_GROUP_UPDATABLE_PROPERTIES: &[&str] =
    &["SecurityGroupEgress", "SecurityGroupIngress", "Tags"];

/// Apply a changed `AWS::EC2::SecurityGroup` declaration to the group the stack created, by
/// bringing the rules it carries to the ones the template now declares.
///
/// Each entry of either rule property is ONE permission of the authorize call for its
/// direction, and the revoke for that direction takes the same permission — so a rule the
/// declaration no longer states is revoked and one it has added is authorized, which leaves the
/// group carrying exactly the declared set. A rule the group no longer holds is already in the
/// state the revoke asks for ([`INVALID_PERMISSION_NOT_FOUND`]).
///
/// A rule whose DESCRIPTION alone changed is neither: what makes a rule the rule it is, is its
/// protocol, its port range and the one source it names — "if the security group rule has a
/// description, you do not need to specify the description to revoke the rule"
/// (`RevokeSecurityGroupIngress`) — so that rule still exists and the one call Amazon EC2
/// publishes for it writes the new description onto it
/// ("`UpdateSecurityGroupRuleDescriptions…`: you can replace an existing description, or add a
/// description to a rule that did not have one previously"). The rule therefore keeps the
/// `sgr-…` id EC2 minted for it and its place among the group's permissions, which a revoke
/// and a fresh authorize would both move.
pub(crate) async fn update_ec2_security_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if !every_change_applied(&changed, EC2_SECURITY_GROUP_UPDATABLE_PROPERTIES) {
        return Ok(UNAPPLIED);
    }
    let group_id = resource.physical_resource_id.clone();
    for (property, members, authorize, revoke, describe) in [
        (
            "SecurityGroupIngress",
            EC2_SECURITY_GROUP_INGRESS_MEMBERS,
            "AuthorizeSecurityGroupIngress",
            "RevokeSecurityGroupIngress",
            "UpdateSecurityGroupRuleDescriptionsIngress",
        ),
        (
            "SecurityGroupEgress",
            EC2_SECURITY_GROUP_EGRESS_MEMBERS,
            "AuthorizeSecurityGroupEgress",
            "RevokeSecurityGroupEgress",
            "UpdateSecurityGroupRuleDescriptionsEgress",
        ),
    ] {
        if !changed.iter().any(|name| name == property) {
            continue;
        }
        let held = declared_security_group_rules(&resource.properties, property);
        let wanted = declared_security_group_rules(declared, property);
        // A revision that states OUTBOUND rules where the one in force stated none replaces
        // the rule Amazon EC2 gave the group: a group whose declaration names any outbound
        // rule carries THOSE and no others, which is the same reading the create applies to a
        // declaration that states them from the start ([`create_ec2_security_group`]). The
        // revoke comes first, so a declaration restating the all-traffic rule itself cannot run
        // into that service's refusal of a second identical rule, and a group that no longer
        // holds it is already in the state the revoke asks for.
        if property == "SecurityGroupEgress" && held.is_empty() && !wanted.is_empty() {
            let mut parameters = vec![("GroupId".to_string(), group_id.clone())];
            parameters.extend(allow_all_egress_parameters());
            match call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                revoke,
                &parameters,
            )
            .await
            {
                Ok(_) => {}
                Err(refusal) if refusal.code == INVALID_PERMISSION_NOT_FOUND => {}
                Err(refusal) => return Err(refusal),
            }
        }
        // Matched by what makes a rule the rule it is, so a rule both revisions state is the
        // same rule however its description reads.
        let same_rule = |left: &Value, right: &Value| permission_of(left) == permission_of(right);
        for rule in held
            .iter()
            .filter(|rule| !wanted.iter().any(|wanted| same_rule(wanted, rule)))
        {
            let mut parameters = vec![("GroupId".to_string(), group_id.clone())];
            parameters.extend(security_group_rule_parameters(rule, members)?);
            match call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                revoke,
                &parameters,
            )
            .await
            {
                Ok(_) => {}
                Err(refusal) if refusal.code == INVALID_PERMISSION_NOT_FOUND => {}
                Err(refusal) => return Err(refusal),
            }
        }
        for rule in &wanted {
            let held_rule = held.iter().find(|held| same_rule(held, rule));
            let action = match held_rule {
                // The rule is already on the group: only its description can have changed,
                // and it changed only if the two revisions state it differently.
                Some(held_rule) if *held_rule == *rule => continue,
                Some(_) => describe,
                None => authorize,
            };
            let mut parameters = vec![("GroupId".to_string(), group_id.clone())];
            parameters.extend(security_group_rule_parameters(rule, members)?);
            call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                action,
                &parameters,
            )
            .await?;
        }
    }
    Ok(applied_in_place())
}

/// The PERMISSION one inline security group rule is, without the description it carries: what
/// makes the rule the rule it is, which is the comparison a revoke matches a rule by
/// ("if the security group rule has a description, you do not need to specify the description
/// to revoke the rule", `RevokeSecurityGroupIngress`).
fn permission_of(rule: &Value) -> Value {
    let Value::Object(members) = rule else {
        return rule.clone();
    };
    Value::Object(
        members
            .iter()
            .filter(|(member, _)| *member != "Description")
            .map(|(member, value)| (member.clone(), value.clone()))
            .collect(),
    )
}

/// The `AWS::EC2::Instance` properties an instance that already exists can take a new value
/// for THROUGH A CALL THIS EMULATOR SERVES: the policies `ModifyInstanceAttribute` writes
/// while the instance runs — whether its API termination is disabled, what a shutdown from
/// inside does, and the security groups it is in — beside the tag set.
///
/// The type publishes more updatable properties than these, and each of the rest is one whose
/// value is part of the machine the instance RUNS AS (its type, its kernel and RAM disk, the
/// data its first boot reads, its EBS optimization). AWS applies those with "some
/// interruptions" — it STOPS the instance, modifies it and starts it again, which is what
/// `ModifyInstanceAttribute` requires of them ("to modify some attributes, the instance must be
/// stopped") — and this deployment does not stop a running instance, so such a change is
/// reported as not applied rather than sent to a call that would refuse it.
pub(crate) const EC2_INSTANCE_UPDATABLE_PROPERTIES: &[&str] = &[
    "DisableApiTermination",
    "InstanceInitiatedShutdownBehavior",
    "SecurityGroupIds",
    "Tags",
];

/// The `AWS::EC2::Instance` properties that are attributes `ModifyInstanceAttribute` writes
/// while the instance runs, each against the member of that call which carries it.
///
/// Source: the type's resource reference against
/// `smithy_query(query="operation",operation="ModifyInstanceAttribute")` — a boolean attribute
/// travels as that member's `Value` ([`modify_ec2_instance_attribute`]), and the security
/// groups as the call's own flattened `GroupId.N` list.
const EC2_INSTANCE_ATTRIBUTE_MEMBERS: &[(&str, &str)] = &[
    ("DisableApiTermination", "DisableApiTermination"),
    (
        "InstanceInitiatedShutdownBehavior",
        "InstanceInitiatedShutdownBehavior",
    ),
];

/// Apply a changed `AWS::EC2::Instance` declaration to the instance the stack launched,
/// through the one call that writes an attribute of an instance that exists.
///
/// The instance keeps the `i-…` id Amazon EC2 assigned it, so everything that references it —
/// an Elastic IP's association, a route pointing at it — goes on resolving.
pub(crate) async fn update_ec2_instance(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if !every_change_applied(&changed, EC2_INSTANCE_UPDATABLE_PROPERTIES) {
        return Ok(UNAPPLIED);
    }
    let instance_id = resource.physical_resource_id.clone();
    for (property, member) in EC2_INSTANCE_ATTRIBUTE_MEMBERS {
        if !changed.iter().any(|name| name == property) {
            continue;
        }
        // A declaration that DROPPED the property states no value for the attribute, and the
        // call takes no empty one — so the instance keeps the policy it has, which is a change
        // this arm did not apply rather than one it is reporting as applied.
        let Some(value) = declared.get(*property) else {
            return Ok(UNAPPLIED);
        };
        modify_ec2_instance_attribute(
            ctx,
            region,
            &instance_id,
            &format!("{member}.Value"),
            &[attribute_text(value)],
        )
        .await?;
    }
    // "The IDs of the security groups. You must specify the ID of at least one security group"
    // (`ModifyInstanceAttributeRequest$Groups`), which that call takes as its own list — so a
    // declaration that drops every group states a set the call cannot carry: the instance keeps
    // the groups it is in and the change is reported as one this arm did not apply.
    if changed.iter().any(|name| name == "SecurityGroupIds") {
        let groups: Vec<String> = declared
            .get("SecurityGroupIds")
            .and_then(Value::as_array)
            .map(|groups| groups.iter().map(attribute_text).collect())
            .unwrap_or_default();
        if groups.is_empty() {
            return Ok(UNAPPLIED);
        }
        modify_ec2_instance_attribute(ctx, region, &instance_id, "GroupId", &groups).await?;
    }
    Ok(applied_in_place())
}

/// Write ONE attribute of an instance that exists: `ModifyInstanceAttribute` carrying the
/// instance and that attribute's own member, which is a single parameter for a scalar and the
/// flattened `<Member>.N` form for a list ("you can specify only one attribute at a time").
async fn modify_ec2_instance_attribute(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_id: &str,
    member: &str,
    values: &[String],
) -> Result<(), Refusal> {
    let mut parameters = vec![("InstanceId".to_string(), instance_id.to_string())];
    if let [only] = values {
        parameters.push((member.to_string(), only.clone()));
    } else {
        for (index, value) in values.iter().enumerate() {
            parameters.push((format!("{member}.{}", index + 1), value.clone()));
        }
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "ModifyInstanceAttribute",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// Bring the tags of one Amazon EC2 resource to the set this deployment gives it, through that
/// service's own `CreateTags` and `DeleteTags`.
///
/// `desired` is the set the CREATE of that type applies — the declaration's own tags beside
/// the ones CloudFormation propagates, read by whichever of this module's callers knows where
/// the type states them — so a key outside it is one this deployment no longer gives the
/// resource and is removed, and one in it with another value is re-valued ("if you specify a
/// tag key that is already associated with the resource, the new tag value that you specify
/// replaces the previous value", `CreateTags`). What the resource HOLDS is read back from
/// Amazon EC2 rather than guessed from the declaration it was created with, so the two halves
/// are computed against the tags really in force; where they already agree nothing is called at
/// all, which is what makes this safe to ask on every deployment.
pub(crate) async fn retag_ec2_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_id: &str,
    desired: &[(String, String)],
) -> Result<(), Refusal> {
    let held = described_ec2_tags(ctx, region, resource_id).await?;
    let (added, removed) = tag_delta(&held, desired);
    if !removed.is_empty() {
        let mut parameters = vec![("ResourceId.1".to_string(), resource_id.to_string())];
        for (index, key) in removed.iter().enumerate() {
            parameters.push((format!("Tag.{}.Key", index + 1), key.clone()));
        }
        call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            "DeleteTags",
            &parameters,
        )
        .await?;
    }
    tag_ec2_resource(ctx, region, resource_id, &added).await
}

/// The tags Amazon EC2 holds for one resource, as the pairs this module reconciles.
///
/// "Describes the specified tags for your EC2 resources" (`DescribeTags`), narrowed to the one
/// resource by that operation's own `resource-id` filter — so the reconciliation reads the tags
/// really in force rather than the ones a previous declaration asked for. The answer reports a
/// key and a value on every entry, so the two lists are read in parallel and zipped.
async fn described_ec2_tags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_id: &str,
) -> Result<Vec<(String, String)>, Refusal> {
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeTags",
        &[
            ("Filter.1.Name".to_string(), "resource-id".to_string()),
            ("Filter.1.Value.1".to_string(), resource_id.to_string()),
        ],
    )
    .await?;
    Ok(xml_elements(&answered, "key")
        .into_iter()
        .zip(xml_elements(&answered, "value"))
        .collect())
}
