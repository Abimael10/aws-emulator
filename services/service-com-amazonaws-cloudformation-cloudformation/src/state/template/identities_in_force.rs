// SPDX-License-Identifier: CC0-1.0

//! The identity a declaration resolves to once the resources it depends on carry the
//! identifiers they really have.
//!
//! [`super::provision`] computes every resource's physical identity before any of them exists,
//! so a declaration whose NAME is composed out of another resource's value
//! (`LogGroupName: !Join ['', ['/aws/<service>/<kind>/', !GetAtt Api.ApiId]]`) is named around
//! the stand-in the naming rules computed for that value. On the deployment that CREATES both,
//! the walk that provisions them brings the name into step with what the owning service minted
//! ([`crate::state::provisioning`]'s re-pointing). A deployment of a stack that ALREADY HAS
//! those resources mints nothing, so there is no pair to re-point with: the identity in force
//! is what the records of the stack carry, and this module is what re-evaluates a declaration
//! against them.
//!
//! Without it, a redeploy of the very same template reads as a RENAME of every resource whose
//! name is composed out of an identifier its stack's own resources already carry: the resource
//! would be created a second time under the stand-in name and the one a reader of that
//! identifier reaches taken away with it.

use super::*;

/// Re-evaluate the identity of each resource whose declaration DEPENDS on another resource of
/// the same template, against the records in force.
///
/// `scope` carries the records as they stand — the identities the stack already has, taken over
/// by [`crate::state::StoredStack::deploy_template`] before this runs — so the name a
/// declaration resolves to here is the name a client composing it from the same identifier
/// builds. A resource whose declaration references nothing cannot be affected and is skipped,
/// which is what keeps this to the few declarations it is about rather than to the template's
/// resource count.
///
/// `keeps_its_identity` names the records whose identifier this deployment cannot recompute —
/// one an owning service, a custom resource's provider or CloudFormation itself assigned, and
/// the one a resource the stack ADOPTED already exists under — for which the identity in force
/// is already the record's and the naming rules compute only a stand-in: re-evaluating one of
/// those would put that stand-in back.
///
/// A resource whose identity is unchanged is left exactly as it is, attributes included, so a
/// record carrying what its owning service reported keeps it.
pub(crate) fn renamed_against_identities_in_force(
    template: &Template,
    resources: &mut [StoredResource],
    dependencies: &BTreeMap<String, BTreeSet<String>>,
    scope: &Scope<'_>,
    keeps_its_identity: &dyn Fn(&StoredResource) -> bool,
    budget: &mut Budget,
) {
    let declared: BTreeMap<&str, &DeclaredResource> = template
        .resources
        .iter()
        .map(|declared| (declared.logical_id.as_str(), declared))
        .collect();
    for resource in resources.iter_mut() {
        if keeps_its_identity(resource) || !dependencies.contains_key(&resource.logical_id) {
            continue;
        }
        let Some(declared) = declared.get(resource.logical_id.as_str()) else {
            continue;
        };
        let name = physical_name(declared, scope, budget);
        let (physical_resource_id, attributes) = return_values(declared, &name, scope, budget);
        if physical_resource_id == resource.physical_resource_id {
            continue;
        }
        // The identity the declaration now resolves to, with the values read off it: the
        // record is the one [`super::provision`] would have produced had the identifiers in
        // force been known when it ran, which is what the two steps below recompute.
        resource.physical_resource_id = physical_resource_id;
        resource.attributes = attributes;
        attach_attributes(resource, declared, &name, scope, budget);
    }
}
