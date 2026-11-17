// SPDX-License-Identifier: CC0-1.0

//! The UPDATE POLICY that asks for an Auto Scaling group to be REPLACED rather than changed,
//! and what a deployment does about it.
//!
//! ## The rule this module implements
//!
//! A template states how an update treats an Auto Scaling group with the `UpdatePolicy`
//! resource attribute, and one of its policies —
//! `AutoScalingReplacingUpdate` with `WillReplace: true` — asks for the GROUP ITSELF to be
//! replaced: CloudFormation builds a new group beside the one that exists, waits for the new
//! group's `CreationPolicy` signals, and takes the old group down once the replacement is in
//! force (a failed replacement leaves the original group serving and the update rolled back).
//! That policy governs the updates that change the group's LAUNCH CONFIGURATION — the launch
//! template or launch configuration the group launches from, the mixed-instances policy it
//! launches under, or the subnets it places instances in — which is the set of changes AWS
//! documents the two Auto Scaling update policies as applying to.
//!
//! So the deployment's record of such a group is the record of a NEW physical resource: it
//! carries a name of its own ([`replacement_name`]), which is what makes the walk create the
//! replacement group beside the group the stack has, hold the create at the `CreationPolicy`
//! wait the declaration states, and take the old group down as the old half of a replacement
//! ([`super::provisioning`]). Everything that follows from a replacement — the
//! `UpdateReplacePolicy` the old physical resource is governed by, the rollback that puts the
//! original group back when the replacement fails — is the one replacement path every other
//! replaced resource of a stack goes through, so nothing here decides any of it.
//!
//! A group whose declaration carries no such policy is NOT replaced: its change is applied to
//! the group that exists, through that service's own update call, which is what the other Auto
//! Scaling update policy (`AutoScalingRollingUpdate`) asks for. Neither is a group the
//! declaration NAMES ITSELF: CloudFormation generates the name of a resource it may replace,
//! and a template that states the name has said which group the stack manages — so the change
//! goes to that group, and the stack publishes the name the template asked for, exactly as it
//! does for every other named resource whose declaration asks for a value the resource can
//! take. (On AWS a named group is what makes a replacing update unperformable rather than
//! in-place; refusing such an update is the business of the validation that reads a custom
//! physical name, which this module does not perform.)
//!
//! ## Bounds
//!
//! One pass over the deployment's records per deployment, and one bounded walk
//! ([`MAX_LAUNCH_REFERENCE_NESTING`]) over the launch-configuration properties of each Auto
//! Scaling group among them: both are sized by the template, which is already held to its own
//! documented quotas ([`super::template::MAX_RESOURCES_PER_TEMPLATE`]).

use ::serde_json::Value;

use super::template;
use super::StoredResource;

/// The `UpdatePolicy` whose `WillReplace` asks for the group to be replaced: "to specify how
/// CloudFormation handles replacement updates for an Auto Scaling group, use the
/// `AutoScalingReplacingUpdate` policy", whose `WillReplace` "specifies whether an Auto
/// Scaling group and the instances it contains are replaced during an update".
const AUTOSCALING_REPLACING_UPDATE: &str = "AutoScalingReplacingUpdate";

/// The member of that policy which asks for the replacement.
const WILL_REPLACE: &str = "WillReplace";

/// The resource attribute the policies of an update live under.
const UPDATE_POLICY: &str = "UpdatePolicy";

/// The properties of an Auto Scaling group that state its LAUNCH CONFIGURATION, which are the
/// changes AWS documents `AutoScalingReplacingUpdate` and `AutoScalingRollingUpdate` as
/// applying to: the launch configuration or launch template the group launches from, the
/// mixed-instances policy it launches under, and the subnets it places its instances in.
///
/// Source: the `UpdatePolicy` attribute's own documentation of when those two policies apply,
/// against the members `CreateAutoScalingGroup` publishes for the same values
/// (`LaunchConfigurationName`, `LaunchTemplate`, `MixedInstancesPolicy`,
/// `VPCZoneIdentifier`).
const LAUNCH_CONFIGURATION_PROPERTIES: &[&str] = &[
    "LaunchConfigurationName",
    "LaunchTemplate",
    "MixedInstancesPolicy",
    "VPCZoneIdentifier",
];

/// The types whose resource IS a group's launch configuration: a change to one of them is a
/// change to the launch configuration of every group that launches from it, which is the first
/// case the `UpdatePolicy` attribute documents those policies applying to.
const LAUNCH_CONFIGURATION_TYPES: &[&str] = &[
    template::EC2_LAUNCH_TEMPLATE,
    "AWS::AutoScaling::LaunchConfiguration",
];

/// The members a launch specification names the launch template or launch configuration it
/// launches from by: `CreateAutoScalingGroup`'s `LaunchConfigurationName`, and the
/// `LaunchTemplateSpecification` members `LaunchTemplateId` and `LaunchTemplateName` that both
/// the group's own `LaunchTemplate` and the one inside a `MixedInstancesPolicy` carry.
///
/// A reference to the resource the group launches from is the value of one of THESE members, so
/// a value equal to that identifier anywhere else in the document is not one.
const LAUNCH_REFERENCE_MEMBERS: &[&str] = &[
    "LaunchConfigurationName",
    "LaunchTemplateId",
    "LaunchTemplateName",
];

/// How deep the walk over a group's launch-configuration properties looks for the resource it
/// launches from. The document is the REQUESTER's, so the walk is bounded for the reason every
/// other walk over one is: a reference to the launch template is a property of the launch
/// specification, never a hundred levels inside one.
const MAX_LAUNCH_REFERENCE_NESTING: usize = 16;

// How many characters the fresh segment of a replacement's name carries, and the longest name
// the owning service takes, both live with the naming rules that compute one
// ([`template::replacement_name`]): that is the one place a generated name is fitted to the
// bound its owning service publishes.

/// Give every record of this deployment that is REPLACED rather than changed a physical name
/// of its OWN, so the deployment creates the replacement beside the resource the stack has.
///
/// Two deployments reach this. One is the `UpdatePolicy` above: an Auto Scaling group the
/// policy asks to be replaced is replaced even though every property of it could be changed in
/// place. The other is every ORDINARY replacement of a resource whose name CloudFormation
/// GENERATED: the record no longer carries the identity in force, because the declaration
/// states a create-only property differently, so the deployment creates a new physical resource
/// for it — and the resource being replaced still holds the name it was created under, which is
/// a name its owning service will not give to a second resource. Both are the same question
/// ([`template::replacement_name`] answers it), which is why they are decided here together.
///
/// `declared` is the template being deployed — where the `UpdatePolicy` resource attribute
/// lives, a stored record carrying the evaluated `Properties` alone — `resources` the records
/// that deployment computed, and `in_force` the records the stack has right now
/// ([`super::StoredStack::current_resources`]).
///
/// Nothing is renamed for a resource the stack does not have yet (there is nothing to replace:
/// that deployment creates it), for a declaration that states the resource's name itself — a
/// name the template asks for is the name the resource has, and CloudFormation replaces such a
/// resource only when the template renames it — or for a type that takes no name at all, whose
/// create carries none.
pub(super) fn rename_replacements(
    declared: &template::Template,
    resources: &mut [StoredResource],
    in_force: &[StoredResource],
) {
    // The resources this deployment CHANGES, by logical id: a record whose evaluated
    // properties differ from the ones in force. Read once, because every group below is
    // judged against the same set.
    let changed: Vec<&StoredResource> = resources
        .iter()
        .filter(|resource| {
            in_force.iter().any(|existing| {
                existing.logical_id == resource.logical_id
                    && existing.resource_type == resource.resource_type
                    && existing.properties != resource.properties
            })
        })
        .collect();
    // Whether each resource is replaced is decided over the records as they stand, before any
    // of them is renamed, so one resource's new name cannot change another's answer.
    let renamed: Vec<(String, String)> = resources
        .iter()
        .filter_map(|resource| {
            let existing = in_force.iter().find(|existing| {
                existing.materialised
                    && existing.logical_id == resource.logical_id
                    && existing.resource_type == resource.resource_type
            })?;
            if super::stack::declared_name(resource).is_some() {
                return None;
            }
            let declaration = declared
                .resources
                .iter()
                .find(|declaration| declaration.logical_id == resource.logical_id);
            let replaced = match declaration.filter(|declaration| will_replace(declaration)) {
                // The `UpdatePolicy` asks for the group to be replaced when the update
                // changes its launch configuration.
                Some(_) => launch_configuration_changed(resource, existing, &changed),
                // ...and every other resource is replaced when the record no longer carries
                // the identity in force: the identities a deployment takes over are already
                // settled by the time this runs ([`super::StoredStack::deploy_template`]), so
                // a record whose identity differs from the one the stack holds is one this
                // deployment creates a new physical resource for.
                //
                // Only where the GENERATED NAME itself did not move: a declaration whose
                // change gives the resource another generated name (a queue whose variant
                // changes the suffix its name carries) already names the replacement something
                // the resource it replaces does not hold, so there is nothing to make unique.
                None => {
                    takes_a_name(&resource.resource_type)
                        && existing.physical_resource_id != resource.physical_resource_id
                        && name_in_force(existing) == name_in_force(resource)
                }
            };
            if !replaced {
                return None;
            }
            Some((
                resource.logical_id.clone(),
                template::replacement_name(
                    &resource.resource_type,
                    &name_in_force(existing),
                    &resource.properties,
                ),
            ))
        })
        .collect();
    for (logical_id, name) in renamed {
        if let Some(resource) = resources
            .iter_mut()
            .find(|resource| resource.logical_id == logical_id)
        {
            // The `Name` attributes the record carries, which is where the create reads the
            // name the resource is made under from
            // (`crate::state::provisioner::declared_name`).
            for (attribute, value) in template::name_attributes(&resource.resource_type, &name) {
                if let Some(held) = resource.attributes.get_mut(&attribute) {
                    *held = value;
                }
            }
            // ...and the physical id too, for a type whose physical id IS that name: through
            // the one step that takes an identifier over, so the `Id` attributes the record
            // reports and the id its `Ref` resolves to cannot drift apart. For a type whose
            // identity its OWNING SERVICE assigns, the id the record carries is the stand-in a
            // reference is resolved against until that service answers the create, and the
            // name is only what the create is made under — so the identity is left alone.
            if !template::identity_is_service_assigned(&resource.resource_type) {
                template::adopt_identity(resource, name);
            }
        }
    }
}

/// Whether a resource of this type is created under a NAME at all: its type publishes a
/// property a declaration states one in ([`template::name_property_candidates`]).
///
/// A type that publishes none takes no name on its create — an Amazon EC2 VPC, a route table —
/// so there is no name a replacement could collide under, and nothing to rename.
fn takes_a_name(resource_type: &str) -> bool {
    !template::name_property_candidates(resource_type).is_empty()
}

/// The name the resource a replacement supersedes was created under: the `Name` its record
/// carries, or its physical id where that IS the name.
///
/// The same reading the create makes of the record it is handed
/// (`crate::state::provisioner::declared_name`), so the name a replacement is derived from is
/// the name the resource it replaces really holds.
fn name_in_force(existing: &StoredResource) -> String {
    crate::state::provisioner::declared_name(existing)
}

/// Whether the declaration's `UpdatePolicy` asks for the resource to be REPLACED by an update:
/// its `AutoScalingReplacingUpdate` states `WillReplace`.
///
/// Read off the ATTRIBUTE rather than off the properties, which is where the policies of an
/// update live, and only for the type that attribute's policy governs — an Auto Scaling group.
fn will_replace(declaration: &template::DeclaredResource) -> bool {
    if declaration.resource_type != template::AUTOSCALING_GROUP {
        return false;
    }
    declaration
        .declaration
        .get(UPDATE_POLICY)
        .and_then(|policies| policies.get(AUTOSCALING_REPLACING_UPDATE))
        .and_then(|policy| policy.get(WILL_REPLACE))
        .is_some_and(template::flag)
}

/// Whether this deployment changes the group's LAUNCH CONFIGURATION: one of the group's own
/// launch-configuration properties ([`LAUNCH_CONFIGURATION_PROPERTIES`]) differs from the one
/// in force, or the launch template or launch configuration RESOURCE the group launches from is
/// one this deployment changes.
///
/// The second half is what a template that reads the launch template through a reference asks
/// for: the group's own property then states the identifier of a resource whose own declaration
/// changed, which is the first case the `UpdatePolicy` attribute documents — a change to the
/// launch configuration the group launches from.
fn launch_configuration_changed(
    resource: &StoredResource,
    existing: &StoredResource,
    changed: &[&StoredResource],
) -> bool {
    if LAUNCH_CONFIGURATION_PROPERTIES
        .iter()
        .any(|property| resource.properties.get(*property) != existing.properties.get(*property))
    {
        return true;
    }
    let launches_from = changed
        .iter()
        .filter(|candidate| LAUNCH_CONFIGURATION_TYPES.contains(&candidate.resource_type.as_str()))
        .filter(|candidate| !candidate.physical_resource_id.is_empty());
    for candidate in launches_from {
        for property in LAUNCH_CONFIGURATION_PROPERTIES {
            let Some(declared) = resource.properties.get(*property) else {
                continue;
            };
            if names(declared, &candidate.physical_resource_id, 0) {
                return true;
            }
            if let Some(name) = super::stack::declared_name(candidate) {
                if names(declared, &name, 0) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a property document NAMES the resource `identifier` — the value of a member a
/// launch specification names its template or configuration by
/// ([`LAUNCH_REFERENCE_MEMBERS`]), at the top of the document or inside the structures it
/// carries (a mixed-instances policy nests its specification two levels down).
///
/// Only those members, so a value that merely EQUALS the identifier somewhere else in the
/// document — a subnet id, a tag — is not read as a reference to it. Bounded at
/// [`MAX_LAUNCH_REFERENCE_NESTING`]: the document is the requester's.
fn names(declared: &Value, identifier: &str, depth: usize) -> bool {
    if depth >= MAX_LAUNCH_REFERENCE_NESTING {
        return false;
    }
    match declared {
        Value::Array(items) => items
            .iter()
            .any(|item| names(item, identifier, depth + 1)),
        Value::Object(members) => members.iter().any(|(member, value)| {
            if LAUNCH_REFERENCE_MEMBERS.contains(&member.as_str()) {
                return super::scalar_text(value).is_some_and(|text| text == identifier);
            }
            names(value, identifier, depth + 1)
        }),
        // A property that IS the reference (`LaunchConfigurationName`), whose member name the
        // caller already matched.
        scalar => super::scalar_text(scalar).is_some_and(|text| text == identifier),
    }
}
