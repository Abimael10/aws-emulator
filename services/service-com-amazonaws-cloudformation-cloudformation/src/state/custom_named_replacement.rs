// SPDX-License-Identifier: CC0-1.0

//! The REPLACEMENT an update cannot carry out: one that would have to create the new
//! physical resource under the IDENTITY the resource it replaces still holds.
//!
//! ## The rule this module implements
//!
//! A replacement creates the new physical resource beside the one it supersedes and takes
//! that one down afterwards ([`super::provisioning`]), so for a resource whose physical id IS
//! the name the template states, there has to be a name left for the new one to be created
//! under. CloudFormation generates the name of a resource it may have to replace, and gives
//! the replacement a fresh name of its own ([`super::replacing_update`]); a template that
//! states the name ITSELF leaves nothing to vary, so where the name it states is the name the
//! live resource already holds, the replacement would have to be created as the resource that
//! is already there, which its owning service will not do. WHY the replacement is required
//! does not change that: the name is what the new resource cannot be created under, whether
//! the declaration asks for the replacement by writing that name down for the first time or by
//! restating any other property the resource cannot take a new value for.
//!
//! CloudFormation therefore refuses such an update instead of attempting it: the resource
//! reports `UPDATE_FAILED` carrying the sentences that say what cannot be done and what the
//! caller has to do about it — rename the resource — and the update is rolled back, having
//! asked the owning services for nothing. That is what makes the refusal visible: a client
//! that read `UPDATE_COMPLETE` would have no way to tell whether the resource it depends on
//! was preserved or destroyed and re-created.
//!
//! Two things are therefore NOT refused, each because the replacement has an identity of its
//! own to be created under:
//!
//! * an update that states a DIFFERENT name, which is the rename the refusal asks for — or
//!   that states the same name under a DIFFERENT PARENT, for a type whose name identifies a
//!   resource only within one ([`super::template::name_scope`]): nothing holds that name
//!   there, so the replacement can be created under it;
//! * a resource whose identity its OWNING SERVICE assigns
//!   ([`template::identity_is_service_assigned`]), whose declared name is only what the
//!   resource is called: that service mints the replacement an identifier of its own, and two
//!   of its resources may carry one name.
//!
//! Whether the replacement is required at all is decided nowhere here — it is the plan's own
//! `Replacement`, the one reading of "Update requires: `Replacement`" a change set's preview
//! and a deployment share ([`super::change_plan`]) — so a change this service applies in
//! place is never refused on account of the name.
//!
//! ## Bounds
//!
//! One pass over the plan, which is sized by the template the update submits and so by the
//! quota that document is already held to
//! ([`super::template::MAX_RESOURCES_PER_TEMPLATE`]).

use super::change_plan::{replacement, StoredResourceChange};
use super::provisioning::ResourceTransition;
use super::{change_action, resource_status, template, DeployedResource};

/// A replacement an update plans that CloudFormation cannot perform: which resource, and the
/// name the template states for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedReplacement {
    /// The resource's logical id, which is what the failed resource's own event names.
    pub logical_id: String,
    /// That resource's type.
    pub resource_type: String,
    /// The identifier the resource KEEPS, nothing having been done to it.
    pub physical_resource_id: Option<String>,
    /// The name the resource is declared under and goes on holding — what the refusal asks
    /// the caller to change.
    pub name: String,
}

impl RefusedReplacement {
    /// What the refused resource's own `UPDATE_FAILED` event reports: that a custom-named
    /// resource cannot be replaced by an update, and the name to change.
    ///
    /// Two fixed sentences around the one variable part, which is the resource's name as the
    /// template states it — the value that makes the replacement unperformable, and so the
    /// one thing a caller has to act on.
    pub fn reason(&self) -> String {
        format!(
            "CloudFormation cannot update a stack when a custom-named resource requires \
             replacing. Rename {} and update the stack again.",
            self.name
        )
    }

    /// The transitions the refused resource goes through: the update of it FAILED carrying
    /// the refusal, and — where the failed update is rolled back — the rollback SKIPPING it.
    ///
    /// Nothing was done to the resource: CloudFormation refused the replacement before it
    /// asked the service that owns the resource for anything, so there is no update in
    /// progress to report and the resource keeps the identifier it has. What the ROLLBACK
    /// then does with it is nothing either — there is no change of this update's to put back
    /// — which is the resource reaching `UPDATE_COMPLETE` carrying the sentence that says why
    /// it is complete without having been put back
    /// ([`super::stack::RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK`]), the same event a rollback
    /// records for every resource it skips. `rolled_back` is whether the failed update is
    /// rolled back at all: a caller that asked for the failed state to be kept
    /// (`DisableRollback`) gets no rollback, and so no such event.
    pub fn transitions(&self, rolled_back: bool) -> Vec<ResourceTransition> {
        let failed = ResourceTransition {
            logical_id: self.logical_id.clone(),
            resource_type: self.resource_type.clone(),
            physical_resource_id: self.physical_resource_id.clone(),
            status: resource_status::UPDATE_FAILED.to_string(),
            reason: Some(self.reason()),
            resource_properties: None,
        };
        if !rolled_back {
            return vec![failed];
        }
        let skipped = ResourceTransition {
            status: resource_status::UPDATE_COMPLETE.to_string(),
            reason: Some(super::stack::RESOURCE_SKIPPED_DURING_UPDATE_ROLLBACK.to_string()),
            ..failed.clone()
        };
        vec![failed, skipped]
    }
}

/// The FIRST replacement in `planned` that CloudFormation cannot perform, or `None` when
/// every replacement it plans can be.
///
/// The plan is walked in its own order, so the resource reported is the one the update would
/// have reached first — which is the resource a client's event stream names.
///
/// `deployed` is what the stack has in the services that own its resources: the name a
/// resource really holds is read off its record there, exactly as the create of a replacement
/// reads it ([`super::provisioner::declared_name`]), so this refusal and the name a
/// replacement would be created under cannot disagree.
pub fn refused_replacement(
    planned: &[StoredResourceChange],
    deployed: &[DeployedResource],
) -> Option<RefusedReplacement> {
    planned
        .iter()
        .find_map(|change| refused_change(change, deployed))
}

/// Whether this one planned change is a replacement CloudFormation cannot perform.
fn refused_change(
    change: &StoredResourceChange,
    deployed: &[DeployedResource],
) -> Option<RefusedReplacement> {
    if change.action != change_action::MODIFY {
        return None;
    }
    // "If the `RequiresRecreation` field is `Always` and the `Evaluation` field is `Static`,
    // `Replacement` is `True`": a change the plan reports that way is one the deployment
    // carries out by creating a new physical resource. A `Conditional` is a change that MAY
    // recreate the resource — the value deciding it is one this service could not evaluate —
    // so it is not a replacement this update is known to require.
    if change.replacement.as_deref() != Some(replacement::TRUE) {
        return None;
    }
    // A resource whose identifier its OWNING SERVICE assigns is created under an identity of
    // that service's minting, whatever the template calls it, so a replacement of one always
    // has an identity of its own.
    if template::identity_is_service_assigned(&change.resource_type) {
        return None;
    }
    // The name the SUBMITTED declaration states for the resource. A declaration that states
    // none has left the name to CloudFormation, which gives the replacement one of its own.
    let declared = change.after_properties()?;
    let property = template::name_property(&change.resource_type, &declared)?;
    let declared_name = declared
        .get(&property)
        .and_then(::serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    // ...against the name the resource REALLY HOLDS. A declaration that states another name
    // has named the resource the replacement is to be created under, so that replacement is
    // performable and nothing is refused.
    let in_force = deployed.iter().find(|deployed| {
        deployed.resource.logical_id == change.logical_resource_id
            && deployed.resource.resource_type == change.resource_type
    })?;
    if super::provisioner::declared_name(&in_force.resource) != declared_name {
        return None;
    }
    // ...and under the same PARENT. For a type whose resource lives inside one, the name
    // identifies a resource only together with that parent
    // ([`template::name_scope`] is the scope the owning service addresses one of its
    // resources in), so a declaration that states another parent asks for the replacement
    // under a name nothing holds there: that replacement is performable, and nothing is
    // refused.
    if template::name_scope(&change.resource_type, &declared)
        != super::physical_name::scope_of(&in_force.resource)
    {
        return None;
    }
    Some(RefusedReplacement {
        logical_id: change.logical_resource_id.clone(),
        resource_type: change.resource_type.clone(),
        physical_resource_id: change.physical_resource_id.clone(),
        name: declared_name.to_string(),
    })
}
