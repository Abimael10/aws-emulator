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
//!   there, so the replacement can be created under it. That covers the name or the parent
//!   a declaration reads off ANOTHER RESOURCE of the stack that this same update RECREATES:
//!   the comparison resolves such a reference against the resource the stack holds right now,
//!   on both of its sides, so the two evaluated declarations read alike while the deployment
//!   will create the replacement under the identifier that resource's own replacement really
//!   gets — and the plan reports that as a change to the very property the identity is read
//!   from ([`identity_moves`]);
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

use super::change_plan::{change_source, replacement, StoredResourceChange, PROPERTIES_ATTRIBUTE};
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
    let recreated = recreated_resources(planned);
    planned
        .iter()
        .find_map(|change| refused_change(change, deployed, &recreated))
}

/// The logical ids this plan gives a NEW physical resource: the resources it RECREATES, and
/// the ones it deletes and creates again under a changed type — which a plan reports as the
/// `Remove` of the old resource beside the `Add` of the new one.
///
/// The same reading the comparison itself makes of its own plan
/// ([`super::change_plan::computed_changes`] collects exactly these before it reports the
/// references to them), because that is what a reference to one of them resolving differently
/// afterwards follows from. A `Conditional` replacement counts: the value deciding it is one
/// this service could not evaluate, so the resource MAY be recreated — and a replacement whose
/// identity might move is not one CloudFormation is known to be unable to perform.
fn recreated_resources(planned: &[StoredResourceChange]) -> ::std::collections::BTreeSet<&str> {
    let mut recreated = ::std::collections::BTreeSet::new();
    for change in planned {
        let recreates = match change.action.as_str() {
            change_action::MODIFY => change
                .replacement
                .as_deref()
                .is_some_and(|planned| planned != replacement::FALSE),
            change_action::ADD => planned.iter().any(|other| {
                other.action == change_action::REMOVE
                    && other.logical_resource_id == change.logical_resource_id
            }),
            _ => false,
        };
        if recreates {
            recreated.insert(change.logical_resource_id.as_str());
        }
    }
    recreated
}

/// Whether this one planned change is a replacement CloudFormation cannot perform.
///
/// `recreated` is the set of resources this plan gives a new physical resource
/// ([`recreated_resources`]), which is what says whether a reference inside this declaration
/// will resolve to something else once the update runs.
fn refused_change(
    change: &StoredResourceChange,
    deployed: &[DeployedResource],
    recreated: &::std::collections::BTreeSet<&str>,
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
    // ...and under an identity no change THIS UPDATE makes moves. A comparison resolves a
    // reference to another resource of the stack against the resource that stack HOLDS, on
    // both of its sides, because the identifier a recreated resource will carry does not exist
    // yet ([`super::change_plan::computed_changes`]) — so a name, or a parent, that a
    // declaration reads off a resource this update RECREATES reads above as the value in
    // force, while the deployment creates the replacement under the identifier that resource's
    // own replacement will really have. The plan reports that as a change to this very
    // property ([`identity_moves`]), and such a replacement has an identity of its own, so
    // nothing is refused.
    if identity_moves(change, &property, recreated) {
        return None;
    }
    Some(RefusedReplacement {
        logical_id: change.logical_resource_id.clone(),
        resource_type: change.resource_type.clone(),
        physical_resource_id: change.physical_resource_id.clone(),
        name: declared_name.to_string(),
    })
}

/// Whether the plan reports that this change gives the resource a DIFFERENT identity: one of
/// the property-level details behind it targets the property the resource is named by, or one
/// of the properties that SCOPE that name ([`template::name_scope_properties`]), and reports
/// the value as one a reference to ANOTHER RESOURCE of the stack drives.
///
/// Those are the details a comparison records for a property whose value will change because
/// the resource it reads is itself RECREATED — "if the resource is recreated, it will have a
/// new physical ID, so all references to that resource will also be updated" — which is the
/// one change to an identity that comparing the two evaluated declarations cannot show: both
/// sides are resolved against the resource the stack holds right now, so the property reads
/// the same on both.
///
/// So the resource the detail names as the entity that caused it has to be one this plan
/// recreates (`recreated`, from [`recreated_resources`]; the detail's causing entity is a
/// logical id, or an attribute path whose first segment is one). A detail naming any other
/// resource is a property whose declaration merely READS differently while resolving to the
/// value the comparison above already compared — the identity stands still, and the
/// replacement is refused.
///
/// `name_property` is the property this declaration is named by.
fn identity_moves(
    change: &StoredResourceChange,
    name_property: &str,
    recreated: &::std::collections::BTreeSet<&str>,
) -> bool {
    let scoping = template::name_scope_properties(&change.resource_type);
    change.details.iter().any(|detail| {
        let names_the_identity = detail
            .name
            .as_deref()
            .is_some_and(|name| name == name_property || scoping.contains(&name));
        let reads_a_recreated_resource = matches!(
            detail.change_source.as_deref(),
            Some(change_source::RESOURCE_REFERENCE) | Some(change_source::RESOURCE_ATTRIBUTE)
        ) && detail.causing_entity.as_deref().is_some_and(|entity| {
            recreated.contains(template::resource_of(entity))
        });
        detail.attribute == PROPERTIES_ATTRIBUTE && names_the_identity && reads_a_recreated_resource
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::change_plan::{evaluation, requires_recreation, StoredChangeDetail};

    /// One property-level detail of a plan, as a comparison records it: `causing_entity` is the
    /// entity the comparison attributes the change to — a logical id for a `Ref`, an
    /// `<id>.<attribute>` path for an `Fn::GetAtt`.
    fn detail(property: &str, change_source: Option<&str>, causing_entity: &str) -> StoredChangeDetail {
        StoredChangeDetail {
            attribute: PROPERTIES_ATTRIBUTE.to_string(),
            name: Some(property.to_string()),
            requires_recreation: requires_recreation::ALWAYS.to_string(),
            evaluation: evaluation::STATIC.to_string(),
            change_source: change_source.map(ToString::to_string),
            causing_entity: Some(causing_entity.to_string()),
            drifted: None,
            values: None,
        }
    }

    /// One planned `Modify` carrying those details.
    fn modification(resource_type: &str, details: Vec<StoredChangeDetail>) -> StoredResourceChange {
        StoredResourceChange {
            action: change_action::MODIFY.to_string(),
            logical_resource_id: "Child".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: Some("a-child".to_string()),
            scope: vec![PROPERTIES_ATTRIBUTE.to_string()],
            replacement: Some(replacement::TRUE.to_string()),
            details,
            policy_action: None,
            resource_drift_status: None,
            nested_change_set_id: None,
            before_context: None,
            after_context: None,
        }
    }

    /// Which planned changes MOVE the identity a replacement would be created under, and which
    /// say nothing about it.
    ///
    /// The rows span what the rule generalises over: which half of the identity the detail
    /// targets (the resource's own name property, the property that SCOPES that name), which
    /// form of reference drives it (a `Ref`, an `Fn::GetAtt` path), a detail on a property that
    /// is no part of the identity, a detail the template's own text caused rather than a
    /// reference, a detail naming a resource this plan does NOT recreate, and a change with no
    /// property-level detail at all.
    #[test]
    fn only_a_reference_to_a_recreated_resource_moves_the_identity() {
        struct Row {
            case: &'static str,
            resource_type: &'static str,
            name_property: &'static str,
            details: Vec<StoredChangeDetail>,
            expected: bool,
        }
        let stream = crate::state::provisioning::logs_resources::LOGS_LOG_STREAM;
        // The one resource the plan of these rows recreates.
        let recreated: ::std::collections::BTreeSet<&str> = ["Group"].into_iter().collect();
        let rows = vec![
            Row {
                case: "the parent the name is scoped by, read through a Ref",
                resource_type: stream,
                name_property: "LogStreamName",
                details: vec![detail(
                    "LogGroupName",
                    Some(change_source::RESOURCE_REFERENCE),
                    "Group",
                )],
                expected: true,
            },
            Row {
                case: "that same parent read through an Fn::GetAtt",
                resource_type: stream,
                name_property: "LogStreamName",
                details: vec![detail(
                    "LogGroupName",
                    Some(change_source::RESOURCE_ATTRIBUTE),
                    "Group.Arn",
                )],
                expected: true,
            },
            Row {
                case: "the resource's own name, composed out of a reference",
                resource_type: stream,
                name_property: "LogStreamName",
                details: vec![detail(
                    "LogStreamName",
                    Some(change_source::RESOURCE_REFERENCE),
                    "Group",
                )],
                expected: true,
            },
            Row {
                case: "a property that is no part of the identity",
                resource_type: crate::state::provisioning::logs_resources::LOGS_METRIC_FILTER,
                name_property: "FilterName",
                details: vec![detail(
                    "FilterPattern",
                    Some(change_source::RESOURCE_REFERENCE),
                    "Group",
                )],
                expected: false,
            },
            Row {
                case: "the identity restated by the template itself",
                resource_type: stream,
                name_property: "LogStreamName",
                details: vec![detail(
                    "LogStreamName",
                    Some(change_source::DIRECT_MODIFICATION),
                    "Group",
                )],
                expected: false,
            },
            Row {
                case: "the identity reading a resource this plan does not recreate",
                resource_type: stream,
                name_property: "LogStreamName",
                details: vec![detail(
                    "LogGroupName",
                    Some(change_source::RESOURCE_REFERENCE),
                    "AnotherGroup",
                )],
                expected: false,
            },
            Row {
                case: "a change with no property-level detail",
                resource_type: stream,
                name_property: "LogStreamName",
                details: Vec::new(),
                expected: false,
            },
        ];
        for row in rows {
            let case = row.case;
            let change = modification(row.resource_type, row.details);
            let moved = identity_moves(&change, row.name_property, &recreated);
            assert_eq!(
                moved, row.expected,
                "case {case:?}: expected the identity to move: {}, got {moved}",
                row.expected
            );
        }
    }

    /// Which logical ids a plan gives a NEW physical resource ([`recreated_resources`]): a
    /// change that recreates the resource, a `Conditional` one that may, and a resource
    /// re-typed into a `Remove` beside an `Add` — against a change that leaves the physical
    /// resource where it is, and an `Add` of a resource nothing removes.
    #[test]
    fn the_recreated_resources_are_the_ones_given_a_new_physical_resource() {
        let planned = vec![
            StoredResourceChange {
                logical_resource_id: "Replaced".to_string(),
                ..modification("AWS::Logs::LogGroup", Vec::new())
            },
            StoredResourceChange {
                logical_resource_id: "MayBeReplaced".to_string(),
                replacement: Some(replacement::CONDITIONAL.to_string()),
                ..modification("AWS::Logs::LogGroup", Vec::new())
            },
            StoredResourceChange {
                logical_resource_id: "Changed".to_string(),
                replacement: Some(replacement::FALSE.to_string()),
                ..modification("AWS::Logs::LogGroup", Vec::new())
            },
            StoredResourceChange {
                logical_resource_id: "Retyped".to_string(),
                action: change_action::REMOVE.to_string(),
                replacement: None,
                ..modification("AWS::Logs::LogGroup", Vec::new())
            },
            StoredResourceChange {
                logical_resource_id: "Retyped".to_string(),
                action: change_action::ADD.to_string(),
                replacement: None,
                ..modification("AWS::Logs::LogStream", Vec::new())
            },
            StoredResourceChange {
                logical_resource_id: "Added".to_string(),
                action: change_action::ADD.to_string(),
                replacement: None,
                ..modification("AWS::Logs::LogStream", Vec::new())
            },
        ];
        let recreated = recreated_resources(&planned);
        let expected: ::std::collections::BTreeSet<&str> =
            ["Replaced", "MayBeReplaced", "Retyped"].into_iter().collect();
        assert_eq!(
            recreated, expected,
            "expected the recreated resources {expected:?}, got {recreated:?}"
        );
    }
}
