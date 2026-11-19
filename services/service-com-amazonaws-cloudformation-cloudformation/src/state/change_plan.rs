// SPDX-License-Identifier: CC0-1.0

//! Computing a change set: the resource-level plan `CreateChangeSet` stores and
//! `DescribeChangeSet` reports.
//!
//! A change set is "a list of changes that will be applied to a stack so that you can
//! review the changes before executing them", and CloudFormation computes it by comparing
//! "the stack's information with the information that you submit in the change set". The
//! stack's information is its TEMPLATE **and** the parameter values in force on it, so the
//! comparison here is between two REVISIONS — the template plus the parameters each side
//! carries — with every intrinsic function evaluated before the two are compared. A
//! property that reads `!Ref Size` is a different value under a different parameter value
//! even though the template text never changed, which is exactly the case
//! `ResourceChangeDetail$ChangeSource` spells `ParameterReference`.
//!
//! What one `Modify` reports:
//!
//! * `Details` — one [`StoredChangeDetail`] per resource ATTRIBUTE the two revisions state
//!   differently and, for `Properties`, one per PROPERTY, carrying the target's
//!   `RequiresRecreation`, the `Evaluation` and the `ChangeSource` with its
//!   `CausingEntity`. A property whose value a parameter drives carries a second detail for
//!   substituting that parameter's new value into the property, which happens when the
//!   change set is executed; and a property reading a resource this plan RECREATES carries
//!   one even though its own text never changed, since "if the resource is recreated, it
//!   will have a new physical ID, so all references to that resource will also be updated".
//! * `Scope` — the attributes those details name, so the scope and the details can never
//!   disagree.
//! * `Replacement` — read off the details exactly as the model documents it: "if the
//!   `RequiresRecreation` field is `Always` and the `Evaluation` field is `Static`,
//!   `Replacement` is `True`. If the `RequiresRecreation` field is `Always` and the
//!   `Evaluation` field is `Dynamic`, `Replacement` is `Conditional`. … If you have
//!   multiple changes with different `RequiresRecreation` values, the `Replacement` value
//!   depends on the change with the most impact."
//! * `PolicyAction` — "the action that will be taken on the physical resource when the
//!   change set is executed", which for a change that recreates the resource is the
//!   `ReplaceAnd…` form of its `UpdateReplacePolicy`.
//!
//! Which property change recreates a resource is published per property in the resource
//! and property types reference, and the three answers it publishes are the three values
//! `RequiresRecreation` takes:
//!
//! * `Always` — the resource cannot take a new value for the property at all: its physical
//!   name, for a type its owning service identifies BY that name
//!   ([`template::replacing_properties`]), and each of the type's other create-only
//!   properties ([`template::replacement_properties`]).
//! * `Conditionally` — "a change to this property causes the resource to be recreated…
//!   [t]o determine the conditions for a `Conditionally` recreation, see the update
//!   behavior for that property": the property carries values of BOTH kinds, so whether
//!   the change recreates the resource depends on which of them the two revisions state
//!   differently ([`template::conditionally_replacing`]).
//! * `Never` — every other property, including the name of a resource whose owning service
//!   RENAMES it in place.
//!
//! Nothing here is computed on a read: `CreateChangeSet` calls [`computed_changes`] once
//! and stores what it answered.

use std::collections::{BTreeMap, BTreeSet};

use ::serde_json::Value;

use super::template;
use super::{change_action, DeployedResource, StoredParameter, StoredResource};
use crate::ops::types::RESOURCE_ATTRIBUTE_VALUES;

/// `Replacement` values (the model's `Replacement` enum).
pub mod replacement {
    pub const TRUE: &str = "True";
    pub const FALSE: &str = "False";
    pub const CONDITIONAL: &str = "Conditional";
}

/// `RequiresRecreation` values (the model's `RequiresRecreation` enum).
pub mod requires_recreation {
    pub const NEVER: &str = "Never";
    pub const ALWAYS: &str = "Always";
    /// "To determine the conditions for a `Conditionally` recreation, see the update
    /// behavior for that property": the change recreates the resource only under the
    /// conditions that property's own update behaviour states.
    pub const CONDITIONALLY: &str = "Conditionally";
}

/// `EvaluationType` values (the model's `EvaluationType` enum).
pub mod evaluation {
    pub const STATIC: &str = "Static";
    pub const DYNAMIC: &str = "Dynamic";
}

/// `ChangeSource` values this service attributes a change to (from the model's
/// `ChangeSource` enum).
pub mod change_source {
    pub const DIRECT_MODIFICATION: &str = "DirectModification";
    pub const PARAMETER_REFERENCE: &str = "ParameterReference";
    pub const RESOURCE_REFERENCE: &str = "ResourceReference";
    pub const RESOURCE_ATTRIBUTE: &str = "ResourceAttribute";
}

/// `PolicyAction` values this service reports for what a change set does to the EXISTING
/// physical resource (from the model's `PolicyAction` enum).
pub mod policy_action {
    pub const DELETE: &str = "Delete";
    pub const RETAIN: &str = "Retain";
    pub const SNAPSHOT: &str = "Snapshot";
    pub const REPLACE_AND_DELETE: &str = "ReplaceAndDelete";
    pub const REPLACE_AND_RETAIN: &str = "ReplaceAndRetain";
    pub const REPLACE_AND_SNAPSHOT: &str = "ReplaceAndSnapshot";
}

// The `DeletionPolicy` values the resource attribute takes: "with the `DeletionPolicy`
// attribute you can preserve, and in some cases, backup a resource when its stack is
// deleted… `Delete`, `Retain`, `RetainExceptOnCreate`, `Snapshot`", `Delete` being the
// default for a resource that declares none. Read from the one list of them, which the
// teardown walks read too.
use super::deletion_policy;

/// The `UpdateReplacePolicy` values the resource attribute takes: "With the
/// `UpdateReplacePolicy` attribute you can specify how CloudFormation handles the OLD
/// resource when the resource is replaced… `Delete`, `Retain`, `Snapshot`", `Delete` being
/// "the default".
///
/// A list of its OWN, and three values rather than the four `DeletionPolicy` documents: the
/// fourth is about a resource whose CREATION failed, which the old resource of a replacement
/// is not, so a declaration stating it here states no documented policy and gets the default.
mod update_replace_policy {
    pub const RETAIN: &str = "Retain";
    pub const SNAPSHOT: &str = "Snapshot";
}

/// The `ResourceAttribute` a property-level change targets.
const PROPERTIES_ATTRIBUTE: &str = "Properties";

/// One property-level change behind a `Modify`, as the modelled `ResourceChangeDetail`
/// carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChangeDetail {
    /// `ResourceTargetDefinition$Attribute`: which resource attribute triggers the update.
    pub attribute: String,
    /// `ResourceTargetDefinition$Name`: "If the `Attribute` value is `Properties`, the
    /// name of the property. For all other attributes, the value is null."
    pub name: Option<String>,
    /// `ResourceTargetDefinition$RequiresRecreation`.
    pub requires_recreation: String,
    /// `ResourceChangeDetail$Evaluation`.
    pub evaluation: String,
    /// `ResourceChangeDetail$ChangeSource`: the group the causing entity belongs to.
    ///
    /// `None` where the change has no such group: a change to the stack's own TAGS is made
    /// by the stack operation itself rather than by anything the template states, so no
    /// source is reported for it.
    pub change_source: Option<String>,
    /// `ResourceChangeDetail$CausingEntity`: "the identity of the entity that triggered
    /// this change… If the `ChangeSource` value is `DirectModification`, no value is given
    /// for `CausingEntity`."
    pub causing_entity: Option<String>,
    /// The two values of a DRIFT-AWARE comparison, for a change computed against the
    /// resource's ACTUAL state ([`DriftedValues`]). `None` for every change computed from
    /// the templates alone, where the members are "only present for drift-aware change
    /// sets".
    pub drifted: Option<DriftedValues>,
    /// What this detail reports about the TARGET's own value: its path, the value each
    /// revision would deploy and what the change does to it ([`TargetValues`]).
    ///
    /// `DescribeChangeSet` reports these only for a request that asked for the property
    /// values, which is what they are. `None` for a change nothing computed them for.
    pub values: Option<TargetValues>,
}

/// The property VALUES one detail reports: `ResourceTargetDefinition$Path`,
/// `$BeforeValue`/`$AfterValue` and `$AttributeChangeType`.
///
/// "The property path of the property", "the value of the property before the change is
/// executed", "the value of the property after the change is executed" and "the type of
/// change to be made to the property if the change is executed" — the detailed changes in the
/// property values a `DescribeChangeSet(IncludePropertyValues=true)` asks for, computed from
/// the same two revisions the comparison itself is made over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetValues {
    /// `ResourceTargetDefinition$Path`: where in the resource's declaration the target sits
    /// — `/Properties/<name>` for one of its properties, and `/<Attribute>` for a resource
    /// attribute declared beside them.
    pub path: String,
    /// The value the revision IN FORCE would deploy for it, absent where that revision
    /// states none (the target is being added).
    pub before_value: Option<String>,
    /// The value the SUBMITTED revision would deploy for it, absent where that revision
    /// states none (the target is being removed).
    pub after_value: Option<String>,
    /// `AttributeChangeType`: `Add` for a target only the submitted revision states,
    /// `Remove` for one only the revision in force states, and `Modify` for one both state
    /// differently.
    pub change_type: String,
}

/// `AttributeChangeType` values this comparison reports (from the model's enum).
pub mod attribute_change_type {
    pub const ADD: &str = "Add";
    pub const REMOVE: &str = "Remove";
    pub const MODIFY: &str = "Modify";
}

impl TargetValues {
    /// The values one target of a comparison reports: `path` is where it sits in the
    /// declaration, and `before`/`after` are what each revision would deploy for it — a
    /// value each side states, and `None` where that side states none at all.
    ///
    /// The text of a value is the value itself where it IS text, and its JSON otherwise:
    /// `BeforeValue` and `AfterValue` are strings, and a property whose value is a number, a
    /// list or an object is reported as the document that property carries.
    fn of(path: String, before: Option<&Value>, after: Option<&Value>) -> Self {
        fn text(value: &Value) -> String {
            match value {
                Value::String(text) => text.clone(),
                value => value.to_string(),
            }
        }
        let change_type = match (before, after) {
            (None, _) => attribute_change_type::ADD,
            (_, None) => attribute_change_type::REMOVE,
            _ => attribute_change_type::MODIFY,
        };
        Self {
            path,
            before_value: before.map(text),
            after_value: after.map(text),
            change_type: change_type.to_string(),
        }
    }
}

/// The before and after values a DRIFT-AWARE change reports for one property, and where
/// each of them comes from.
///
/// `ResourceTargetDefinition$BeforeValueFrom` is `ACTUAL_STATE` — "the before value
/// represents current actual state" — because that is what such a change set compares
/// against, and `$AfterValueFrom` is `TEMPLATE`, "the after value comes from the new
/// template": reverting drift puts the template's value back over the live one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftedValues {
    /// `ResourceTargetDefinition$BeforeValue`: the value the resource really carries.
    pub before_value: String,
    /// `ResourceTargetDefinition$AfterValue`: the value the template declares.
    pub after_value: String,
}

/// `ResourceTargetDefinition$BeforeValueFrom` for a drift-aware change: the live resource.
pub const BEFORE_VALUE_FROM_ACTUAL_STATE: &str = "ACTUAL_STATE";

/// `ResourceTargetDefinition$AfterValueFrom` for a drift-aware change: the new template.
pub const AFTER_VALUE_FROM_TEMPLATE: &str = "TEMPLATE";

/// One resource-level change a change set would make, computed once when the change set
/// is created by comparing the submitted revision with the stack's current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResourceChange {
    /// The model's `ChangeAction`: `Add`, `Modify` or `Remove`.
    pub action: String,
    pub logical_resource_id: String,
    pub resource_type: String,
    /// The physical id of the resource the change acts on, for a change to one the stack
    /// has already deployed (a `Modify` or a `Remove`). `None` for an `Add`: the resource
    /// does not exist yet, so it has no physical id to report.
    pub physical_resource_id: Option<String>,
    /// For a `Modify`, the resource attributes that differ, spelled as the model's
    /// `ResourceAttribute` enum spells them and in that enum's order — the attributes
    /// [`Self::details`] names. Empty for an `Add` or a `Remove`, which change the whole
    /// resource rather than an attribute of it.
    pub scope: Vec<String>,
    /// For a `Modify`, "whether CloudFormation will replace the resource by creating a new
    /// one and deleting the old one", read off [`Self::details`]. `None` for an `Add` or a
    /// `Remove`, where the member does not apply.
    pub replacement: Option<String>,
    /// For a `Modify`, the property-level changes behind it. Empty for an `Add` or a
    /// `Remove`.
    pub details: Vec<StoredChangeDetail>,
    /// "The action that will be taken on the physical resource when the change set is
    /// executed": the `ReplaceAnd…` action a change that RECREATES the resource takes on the
    /// one that is already there, which is what its `UpdateReplacePolicy` says. `None` for a
    /// change that leaves the physical resource where it is.
    pub policy_action: Option<String>,
    /// `ResourceChange$ResourceDriftStatus`: the drift status of the resource, "only present
    /// for drift-aware change sets" — so `None` for every change computed from the templates
    /// alone.
    pub resource_drift_status: Option<String>,
    /// `ResourceChange$ChangeSetId`: "the change set ID of the nested change set" — the plan
    /// of the child stack this change would change, created when the change set was created
    /// for the whole stack hierarchy ([`super::nested_change_set`]).
    ///
    /// `None` for every other change: a change on a resource that is not a nested stack has
    /// no hierarchy below it, and one on a child stack that does not exist yet has no stack
    /// to hold a change set of its own.
    pub nested_change_set_id: Option<String>,
    /// `ResourceChange$BeforeContext`: "an encoded JSON string that contains the context of
    /// the resource before the change is executed" — the `Properties` document the revision
    /// IN FORCE would deploy for this resource, evaluated.
    ///
    /// `None` for an `Add`, whose resource is not there to have a context yet, and for a
    /// change computed without a revision to read one from. `DescribeChangeSet` reports it
    /// only for a request that asked for the property values ([`IncludePropertyValues`]).
    ///
    /// [`IncludePropertyValues`]: crate::ops::describe_change_set
    pub before_context: Option<String>,
    /// `ResourceChange$AfterContext`: the same document for the revision the change set
    /// would put in force — "the context of the resource after the change is executed".
    ///
    /// `None` for a `Remove`, whose resource is not there afterwards.
    pub after_context: Option<String>,
}

impl StoredResourceChange {
    /// The evaluated `Properties` document this change would put in force, read back out of
    /// [`Self::after_context`] — the one place that document is held.
    ///
    /// `None` for a change that leaves no resource behind (a `Remove`) and for one computed
    /// without a declaration to read. The context is written here ([`property_context`]) and
    /// read here, so a caller that needs the document the update would deploy — rather than
    /// the text the template states — cannot decode it differently from the way it was
    /// encoded.
    pub fn after_properties(&self) -> Option<Value> {
        let context: Value = ::serde_json::from_str(self.after_context.as_deref()?).ok()?;
        context.get(PROPERTIES_ATTRIBUTE).cloned()
    }
}

/// The revision of a stack a change set is computed AGAINST: the template in force on it,
/// the parameter values in force with it, the resources it has deployed and the stack the
/// pseudo parameters resolve to.
#[derive(Debug, Clone, Copy)]
pub struct StackRevision<'a> {
    pub stack: template::StackScope<'a>,
    /// The template in force on the stack. A stack a `CREATE` change set brought into
    /// existence and nothing has executed yet has none, which is an EMPTY template: every
    /// resource the submitted one declares is an addition.
    pub template: &'a template::Template,
    pub parameters: &'a [StoredParameter],
    pub deployed: &'a [DeployedResource],
    /// The status the stack's last operation left it in: what says whether the stack's
    /// resources carry the configuration its template declares, or whether some of them
    /// carry the one they were PUT BACK to ([`carried_configurations`]).
    pub status: &'a str,
    /// The exported output values the stack's `Fn::ImportValue` references resolved to
    /// when its template was deployed. Both sides of the comparison resolve imports
    /// against the SAME map, for the reason both resolve references against the same
    /// deployed resources: what an import would change is not something this comparison
    /// claims to know.
    pub imports: &'a BTreeMap<String, String>,
}

/// The stack a submitted revision is compared AGAINST, as the operations that compare
/// against a LIVE stack hold it: the stack's identity, the template body in force with the
/// stage computed for it, the parameter values in force, the resources it has deployed and
/// the exported values its imports resolved to.
///
/// [`StackRevision`] is that same stack once its template has been READ; this is the step
/// before, which is what a handler has — so reading the stack's PROCESSED stage and
/// building the revision from it happens HERE, once, rather than in each operation that
/// compares against a stack ([`changes_against`]).
#[derive(Debug, Clone, Copy)]
pub struct DeployedStack<'a> {
    pub region: &'a str,
    pub stack_name: &'a str,
    pub stack_id: &'a str,
    /// The body the stack's processed stage was computed over — the stored body with any
    /// `AWS::Include` snippet already spliced into it where the operation that holds one
    /// has it. `None` for a stack with no template at all.
    pub body: Option<&'a str>,
    pub processed: &'a template::Processed,
    pub parameters: &'a [StoredParameter],
    pub deployed: &'a [DeployedResource],
    /// The status the stack's last operation left it in ([`StackRevision::status`]).
    pub status: &'a str,
    pub imports: &'a BTreeMap<String, String>,
    /// The Amazon SNS topic ARNs in force on the stack — what `AWS::NotificationARNs`
    /// resolves to on both sides of the comparison, the way the deployed resources are.
    pub notification_arns: &'a [String],
}

/// [`computed_changes`] against a stack whose template has not been read yet: the stack's
/// PROCESSED stage is what the comparison reads, since that is the document it deploys.
///
/// A stack whose processed stage is unavailable, or whose body the reader will not read,
/// declares nothing to compare against — the same answer a stack with no template gives —
/// so every resource the submitted template declares is an addition.
pub fn changes_against(
    current: Option<DeployedStack<'_>>,
    submitted: &template::Template,
    submitted_parameters: &[StoredParameter],
) -> Vec<StoredResourceChange> {
    let Some(current) = current else {
        return computed_changes(None, submitted, submitted_parameters);
    };
    let declared = current
        .body
        .and_then(|body| current.processed.body(body))
        .and_then(super::declared_template)
        .unwrap_or_default();
    computed_changes(
        Some(StackRevision {
            stack: template::StackScope {
                region: current.region,
                stack_name: current.stack_name,
                stack_id: current.stack_id,
                notification_arns: current.notification_arns,
            },
            template: &declared,
            parameters: current.parameters,
            deployed: current.deployed,
            status: current.status,
            imports: current.imports,
        }),
        submitted,
        submitted_parameters,
    )
}

/// The configuration the stack's resources CARRY where it is not the one the template in
/// force declares, keyed by logical id — the live state the comparison's `before` side is
/// taken from for those resources ([`property_details`]).
///
/// A change set is "a list of changes that will be applied to a stack", so what it compares
/// a submitted revision with is the configuration the stack's resources really hold. For
/// almost every stack that IS the template in force: the operation that installed the
/// template carried it through. The exception is a stack whose last operation FAILED and was
/// left standing ([`super::status_preserved_a_failure`]): a resource whose change that
/// operation could not carry through was PUT BACK to the configuration in force before it
/// ([`super::provisioning`]), which the template in force does not declare — so comparing
/// that resource's declarations would report work the stack does not need, and a revision
/// restating what the resource already holds would never read as the no-op it is.
///
/// The resources that were put back are the ones their own rollback COMPLETED for, and the
/// configuration each of them holds is its record's resolved `Properties`. A resource whose
/// revert the owning service refused still holds what the failed operation left there, so
/// the template in force is what declares it, and so this names none of them.
fn carried_configurations(current: &StackRevision<'_>) -> BTreeMap<String, Value> {
    if !super::status_preserved_a_failure(current.status) {
        return BTreeMap::new();
    }
    current
        .deployed
        .iter()
        .filter(|deployed| deployed.status == super::resource_status::UPDATE_ROLLBACK_COMPLETE)
        .map(|deployed| {
            (
                deployed.resource.logical_id.clone(),
                deployed.resource.properties.clone(),
            )
        })
        .collect()
}

/// The resource-level changes that applying `submitted` with `submitted_parameters` to the
/// stack `current` describes would make.
///
/// A resource only the submitted template declares is an `Add`, one only the stack's
/// template declares is a `Remove`, and one both declare is a `Modify` carrying one detail
/// per attribute or property whose DEPLOYED value would differ — except when its `Type`
/// itself changed, which cannot be modified into place and so is reported as the `Remove`
/// of the old resource and the `Add` of the new one. A change to a resource the stack has
/// already deployed carries that resource's physical id. The order is deterministic: the
/// submitted template's resources (in the order its reader yields them, which
/// [`super::declared_template`] sorts by logical id), then the removals in the stack
/// template's.
///
/// `current` is `None` for a stack that does not exist yet, which declares nothing to
/// compare against: every submitted resource is an addition.
pub fn computed_changes(
    current: Option<StackRevision<'_>>,
    submitted: &template::Template,
    submitted_parameters: &[StoredParameter],
) -> Vec<StoredResourceChange> {
    let Some(current) = current else {
        // No stack to compare against and none to resolve against either, so each addition
        // reports the document the submitted template states for it
        // ([`property_context`]).
        let mut budget = template::Budget::new();
        let mut changes: Vec<StoredResourceChange> =
            submitted.resources.iter().map(addition).collect();
        for (change, resource) in changes.iter_mut().zip(&submitted.resources) {
            change.after_context =
                Some(property_context(&resource.properties, None, &mut budget));
        }
        return changes;
    };
    // ONE budget for the whole comparison, as a deployment evaluates a whole template out
    // of one: the total work a caller's two templates can ask for is bounded.
    let mut budget = template::Budget::new();
    let deployed: BTreeMap<String, StoredResource> = current
        .deployed
        .iter()
        .map(|deployed| (deployed.resource.logical_id.clone(), deployed.resource.clone()))
        .collect();
    // What those resources CARRY where the template in force does not declare it: the
    // comparison's `before` side for each of them ([`carried_configurations`]).
    let carried = carried_configurations(&current);
    // The physical resource a change acts on: the identifier of the resource the stack really
    // HOLDS under that logical id ([`super::stack::holds_the_resource`]). A record of a
    // resource whose create the owning service refused, or one an operation took away,
    // describes no physical resource — nothing was ever made, or it is gone — so a change on
    // that logical id reports none, and reports no action on one either
    // ([`modification`], [`removal`]).
    let physical_id = |logical_id: &str| {
        current
            .deployed
            .iter()
            .find(|deployed| deployed.resource.logical_id == logical_id)
            .filter(|deployed| super::stack::holds_the_resource(deployed))
            .map(|deployed| deployed.resource.physical_resource_id.clone())
            .filter(|physical_resource_id| !physical_resource_id.is_empty())
    };
    // Both sides resolve against the resources the stack has DEPLOYED: a reference to
    // another resource therefore resolves to the same value on both sides, so what such a
    // reference would change is reported from the plan itself — the resource it names being
    // recreated (see [`propagate_recreations`]) — rather than from a value this emulator
    // claims to know.
    let before_parameters = parameter_values(current.template, current.parameters);
    let before_types = super::ssm_parameters::parameter_types_in_force(current.template);
    // Neither side reads another stack's output: an `Fn::GetStackOutput` is resolved where a
    // template is DEPLOYED and kept by the stack that read it
    // ([`super::stack_outputs`]), so what one would read next is not something this
    // comparison claims to know — and resolving both sides against the same (empty) table is
    // what keeps a reference that did not change out of the plan, exactly as the shared
    // `imports` map does for an import.
    let no_stack_outputs = super::stack_outputs::Values::new();
    let before = template::Scope {
        stack: current.stack,
        parameters: &before_parameters,
        parameter_types: &before_types,
        resources: &deployed,
        mappings: &current.template.mappings,
        conditions: &current.template.conditions,
        imports: current.imports,
        stack_outputs: &no_stack_outputs,
    };
    let after_parameters = parameter_values(submitted, submitted_parameters);
    let after_types = super::ssm_parameters::parameter_types_in_force(submitted);
    let after = template::Scope {
        stack: current.stack,
        parameters: &after_parameters,
        parameter_types: &after_types,
        resources: &deployed,
        mappings: &submitted.mappings,
        conditions: &submitted.conditions,
        imports: current.imports,
        stack_outputs: &no_stack_outputs,
    };
    let declared_resources: BTreeSet<&str> = submitted
        .resources
        .iter()
        .map(|resource| resource.logical_id.as_str())
        .chain(
            current
                .template
                .resources
                .iter()
                .map(|resource| resource.logical_id.as_str()),
        )
        .collect();
    let changed_parameters: BTreeSet<&str> = before_parameters
        .keys()
        .chain(after_parameters.keys())
        .filter(|key| before_parameters.get(*key) != after_parameters.get(*key))
        .map(String::as_str)
        .collect();
    // Every parameter the submitted template declares, which is what a `Ref` inside it can
    // be a reference TO.
    let declared_parameters: BTreeSet<&str> = submitted
        .parameters
        .iter()
        .map(|parameter| parameter.key.as_str())
        .collect();

    let comparison = Comparison {
        before,
        after,
        changed_parameters: &changed_parameters,
        declared_parameters: &declared_parameters,
        declared_resources: &declared_resources,
    };

    // What the two revisions say about each resource the submitted template declares,
    // computed before anything is reported: whether a resource is RECREATED decides what
    // the resources referring to it report, so the direct comparison has to be complete
    // before the references are followed.
    let mut planned: Vec<Planned<'_>> = Vec::new();
    for resource in &submitted.resources {
        let previous = current
            .template
            .resources
            .iter()
            .find(|candidate| candidate.logical_id == resource.logical_id);
        planned.push(match previous {
            None => Planned::Added { resource },
            // A resource whose TYPE changed is not modified into place: CloudFormation
            // deletes the old resource and creates the new one.
            Some(previous) if previous.resource_type != resource.resource_type => {
                Planned::Retyped { previous, resource }
            }
            Some(previous) => Planned::Modified {
                resource,
                details: change_details(
                    previous,
                    resource,
                    carried.get(&resource.logical_id),
                    comparison,
                    &mut budget,
                ),
            },
        });
    }
    // The resources this change set would give a NEW physical id: the ones it recreates,
    // and the ones it deletes and creates again under a changed type. "If the resource is
    // recreated, it will have a new physical ID, so all references to that resource will
    // also be updated" — which is what [`propagate_recreations`] reports.
    //
    // Following those references can itself recreate a resource (a resource whose NAME is
    // built from a recreated one's), and the resources referring to THAT one are owed the
    // same report, so the two steps run until a round finds nothing new — bounded by the
    // length of the longest chain a template can hold, which is its resource count.
    let mut recreated: BTreeSet<String> = BTreeSet::new();
    for _ in 0..=planned.len() {
        let now_recreated: BTreeSet<String> = planned
            .iter()
            .filter_map(|planned| match planned {
                Planned::Retyped { resource, .. } => Some(resource.logical_id.clone()),
                Planned::Modified { resource, details }
                    if replacement_of(details) != replacement::FALSE =>
                {
                    Some(resource.logical_id.clone())
                }
                _ => None,
            })
            .collect();
        if now_recreated == recreated {
            break;
        }
        recreated = now_recreated;
        for planned in &mut planned {
            if let Planned::Modified { resource, details } = planned {
                propagate_recreations(resource, &recreated, details);
            }
        }
    }
    for planned in &mut planned {
        if let Planned::Modified { details, .. } = planned {
            // One order however the bodies were written, now that both passes have had
            // their say.
            details.sort_by(detail_order);
        }
    }

    let mut changes = Vec::new();
    for planned in planned {
        match planned {
            Planned::Added { resource } => changes.push(addition(resource)),
            Planned::Retyped { previous, resource } => {
                changes.push(removal(previous, physical_id(&previous.logical_id)));
                changes.push(addition(resource));
            }
            // The two revisions would deploy this resource identically, so there is
            // nothing to report about it.
            Planned::Modified { details, .. } if details.is_empty() => {}
            Planned::Modified { resource, details } => changes.push(modification(
                resource,
                physical_id(&resource.logical_id),
                details,
            )),
        }
    }
    for resource in &current.template.resources {
        let still_declared = submitted
            .resources
            .iter()
            .any(|candidate| candidate.logical_id == resource.logical_id);
        if !still_declared {
            changes.push(removal(resource, physical_id(&resource.logical_id)));
        }
    }
    // What each change would deploy, on each side it HAS a side on: the resource's evaluated
    // `Properties` as the revision in force states them and as the submitted revision states
    // them ([`property_context`]). Filled here, from the two declaration sets the comparison
    // above already read, so a change carries the same document the comparison was made over
    // — and a `Remove` reports only the before and an `Add` only the after, since a change
    // that takes a resource away leaves no resource to have an after context and one that
    // brings it into being had none before.
    let declared_before: BTreeMap<&str, &template::DeclaredResource> = current
        .template
        .resources
        .iter()
        .map(|resource| (resource.logical_id.as_str(), resource))
        .collect();
    let declared_after: BTreeMap<&str, &template::DeclaredResource> = submitted
        .resources
        .iter()
        .map(|resource| (resource.logical_id.as_str(), resource))
        .collect();
    for change in &mut changes {
        let logical_id = change.logical_resource_id.as_str();
        if change.action != change_action::ADD {
            change.before_context = declared_before.get(logical_id).map(|resource| {
                // The configuration the resource CARRIES where the template in force no
                // longer declares it is the before side for that resource, exactly as it is
                // for the property comparison above ([`carried_configurations`]).
                match carried.get(logical_id) {
                    Some(carried) => property_context(carried, None, &mut budget),
                    None => property_context(&resource.properties, Some(&before), &mut budget),
                }
            });
        }
        if change.action != change_action::REMOVE {
            change.after_context = declared_after.get(logical_id).map(|resource| {
                property_context(&resource.properties, Some(&after), &mut budget)
            });
        }
    }
    changes
}

/// The `BeforeContext`/`AfterContext` of one resource: "an encoded JSON string that contains
/// the context of the resource" — its `Properties` document, under that one key, with every
/// intrinsic function evaluated in `scope`.
///
/// It is the document the deployment would carry out, so it is EVALUATED: a property reading
/// `!Ref Size` is reported as the value that revision would really deploy, which is the whole
/// point of asking for the property values. A value this reader cannot evaluate at all is
/// reported as the template states it — that declaration is what is known about it — and so
/// is every property of a resource there is no scope to resolve against (a change set over a
/// stack that does not exist yet).
///
/// A declaration stating no properties reports an empty document rather than a null one: the
/// context is the resource's properties, and it has none.
fn property_context(
    properties: &Value,
    scope: Option<&template::Scope<'_>>,
    budget: &mut template::Budget,
) -> String {
    let resolved = scope.and_then(|scope| template::resolve(properties, scope, budget));
    let properties = match resolved.unwrap_or_else(|| properties.clone()) {
        Value::Null => Value::Object(::serde_json::Map::new()),
        resolved => resolved,
    };
    let mut context = ::serde_json::Map::new();
    context.insert(PROPERTIES_ATTRIBUTE.to_string(), properties);
    Value::Object(context).to_string()
}

/// What the comparison says about one resource the submitted template declares, before the
/// references between resources are followed.
#[derive(Debug)]
enum Planned<'a> {
    /// Only the submitted template declares it.
    Added { resource: &'a template::DeclaredResource },
    /// Both declare it, under different types: the old one is removed and the new one
    /// added.
    Retyped {
        previous: &'a template::DeclaredResource,
        resource: &'a template::DeclaredResource,
    },
    /// Both declare it under the same type, with these details differing — none of them
    /// for a resource the two revisions would deploy identically.
    Modified {
        resource: &'a template::DeclaredResource,
        details: Vec<StoredChangeDetail>,
    },
}

/// Add one detail per property of `resource` that refers to a resource this change set
/// RECREATES, which the resource's own declaration cannot show.
///
/// "If your template includes a reference to a resource that's conditionally recreated, the
/// value of the reference (the physical ID of the resource) might change… If the resource is
/// recreated, it will have a new physical ID, so all references to that resource will also
/// be updated": that this property's value WILL change is therefore known from the plan
/// itself, and so is the value — the recreated resource's — which is what `Static` reports
/// ("for `Static` evaluations, CloudFormation can determine that the target value will
/// change, and its value"), with the reference as the entity that triggered it. It is also
/// what the recorded preview reports for exactly this detail.
///
/// A property the direct comparison already reported keeps that report: the template states
/// the change itself, which is the stronger account of it.
fn propagate_recreations(
    resource: &template::DeclaredResource,
    recreated: &BTreeSet<String>,
    details: &mut Vec<StoredChangeDetail>,
) {
    if recreated.is_empty() {
        return;
    }
    let replacing = template::replacing_properties(&resource.resource_type, &resource.properties);
    let mut propagated = Vec::new();
    for property in property_names(&resource.properties) {
        let already_reported = details.iter().any(|detail| {
            detail.attribute == PROPERTIES_ATTRIBUTE
                && detail.name.as_deref() == Some(property)
        });
        if already_reported {
            continue;
        }
        let referenced = template::references(
            resource
                .properties
                .get(property)
                .unwrap_or(&Value::Null),
        );
        // A resource's own `Ref` is its own physical id, so a self-reference says nothing
        // about this property changing.
        let recreates =
            |name: &str| name != resource.logical_id && recreated.contains(name);
        let causing = referenced
            .attribute_targets()
            .find(|(resource, _)| recreates(resource))
            .map(|(_, path)| (change_source::RESOURCE_ATTRIBUTE, path.to_string()))
            .or_else(|| {
                referenced
                    .by_value
                    .iter()
                    .find(|name| recreates(name))
                    .map(|name| (change_source::RESOURCE_REFERENCE, name.clone()))
            });
        let Some((source, causing_entity)) = causing else {
            continue;
        };
        propagated.push(StoredChangeDetail {
            attribute: PROPERTIES_ATTRIBUTE.to_string(),
            name: Some(property.to_string()),
            requires_recreation: recreation_of(&resource.resource_type, &replacing, property)
                .to_string(),
            // The value this property will carry is the recreated resource's, which the
            // plan already decided: determined, and determined to change, which is what
            // `Static` reports.
            evaluation: evaluation::STATIC.to_string(),
            change_source: Some(source.to_string()),
            causing_entity: Some(causing_entity),
            drifted: None,
            // The value this property will carry is a physical id the recreated resource
            // does not have yet, so neither side of it is a value this comparison read: the
            // target is reported, and what it will become is not something to state.
            values: None,
        });
    }
    details.extend(propagated);
}

/// The `RequiresRecreation` of a change to one property of a declaration of
/// `resource_type`, given the properties of that declaration a change recreates the
/// resource over (`recreating`, from [`template::replacing_properties`]).
///
/// `Always` for a property the resource cannot take a new value for at all, `Conditionally`
/// for one that carries such a value AMONG values it can take anew — "a change to this
/// property causes the resource to be recreated… [t]o determine the conditions for a
/// `Conditionally` recreation, see the update behavior for that property" — and `Never` for
/// every other property.
///
/// The one classification both passes below report, so a property is classified the same
/// way whether the template states its change outright or the plan implies it.
fn recreation_of(resource_type: &str, recreating: &[String], property: &str) -> &'static str {
    if recreating.iter().any(|name| name == property) {
        return requires_recreation::ALWAYS;
    }
    if template::conditionally_replacing(resource_type, property) {
        return requires_recreation::CONDITIONALLY;
    }
    requires_recreation::NEVER
}

/// The order a resource's details are reported in: by the attribute they target, in the
/// order the model's `ResourceAttribute` enum lists them, then by the property name.
fn detail_order(left: &StoredChangeDetail, right: &StoredChangeDetail) -> ::std::cmp::Ordering {
    let position = |detail: &StoredChangeDetail| {
        RESOURCE_ATTRIBUTE_VALUES
            .iter()
            .position(|attribute| *attribute == detail.attribute)
            .unwrap_or(RESOURCE_ATTRIBUTE_VALUES.len())
    };
    position(left)
        .cmp(&position(right))
        .then_with(|| left.name.cmp(&right.name))
}

/// The parameter values in force for one revision: the template's own declared defaults,
/// overridden by the values the revision carries — the same resolution a deployment makes
/// ("`Default`: A value of the appropriate type for the template to use if no value is
/// specified"). An SSM-typed parameter carries the value Parameter Store held when the
/// revision was resolved ([`StoredParameter::value_in_force`]), which is what its
/// expressions resolve to.
fn parameter_values(
    declared: &template::Template,
    carried: &[StoredParameter],
) -> BTreeMap<String, String> {
    let mut values = declared.parameter_defaults();
    for parameter in carried {
        values.insert(
            parameter.key.clone(),
            parameter.value_in_force().to_string(),
        );
    }
    values
}

/// What one resource's two declarations are compared in: the scope each side resolves
/// against, which parameters the two revisions value differently, and which names the
/// templates declare as RESOURCES.
#[derive(Debug, Clone, Copy)]
struct Comparison<'a> {
    before: template::Scope<'a>,
    after: template::Scope<'a>,
    changed_parameters: &'a BTreeSet<&'a str>,
    /// Every parameter the submitted template declares.
    declared_parameters: &'a BTreeSet<&'a str>,
    declared_resources: &'a BTreeSet<&'a str>,
}

/// One detail per resource attribute, and per `Properties` property, the two declarations
/// of one resource would deploy differently.
///
/// `carried` is the configuration the resource HOLDS where that is not what `previous`
/// declares ([`carried_configurations`]), which is then what the properties are compared
/// against.
///
/// The attributes are walked in the order the model's `ResourceAttribute` enum lists them
/// and the properties of each in sorted order, so one change set reports its details — and
/// the scope read off them — in one order however the bodies were written.
fn change_details(
    previous: &template::DeclaredResource,
    submitted: &template::DeclaredResource,
    carried: Option<&Value>,
    comparison: Comparison<'_>,
    budget: &mut template::Budget,
) -> Vec<StoredChangeDetail> {
    let mut details = Vec::new();
    for attribute in RESOURCE_ATTRIBUTE_VALUES {
        if *attribute == PROPERTIES_ATTRIBUTE {
            details.extend(property_details(
                previous, submitted, carried, comparison, budget,
            ));
            continue;
        }
        // Every other `ResourceAttribute` is the resource attribute of that name, declared
        // beside `Properties`: it carries no intrinsic a deployment evaluates, so the two
        // declarations are compared as they are written.
        if previous.declaration.get(attribute) != submitted.declaration.get(attribute) {
            details.push(StoredChangeDetail {
                attribute: (*attribute).to_string(),
                // "For all other attributes, the value is null."
                name: None,
                // Recreation is a property-level judgement: an attribute beside the
                // properties (a `Metadata` block, a `DeletionPolicy`) never recreates the
                // resource.
                requires_recreation: requires_recreation::NEVER.to_string(),
                evaluation: evaluation::STATIC.to_string(),
                change_source: Some(change_source::DIRECT_MODIFICATION.to_string()),
                causing_entity: None,
                drifted: None,
                // The attribute itself is the target, so its path is the attribute and the
                // values are the documents the two declarations state for it.
                values: Some(TargetValues::of(
                    format!("/{attribute}"),
                    previous.declaration.get(attribute),
                    submitted.declaration.get(attribute),
                )),
            });
        }
    }
    details
}

/// One detail per property the two declarations of one resource would deploy differently.
///
/// `carried` is the resolved `Properties` the resource HOLDS, for a resource whose live
/// configuration is not the one `previous` declares ([`carried_configurations`]). It is
/// already evaluated, so each property is compared with the value the submitted declaration
/// RESOLVES to rather than with the text either side is written as: what the deployment
/// would change is the value, and a declaration that spells the value the resource already
/// holds through an intrinsic changes nothing.
fn property_details(
    previous: &template::DeclaredResource,
    submitted: &template::DeclaredResource,
    carried: Option<&Value>,
    comparison: Comparison<'_>,
    budget: &mut template::Budget,
) -> Vec<StoredChangeDetail> {
    let properties: BTreeSet<&str> = property_names(&previous.properties)
        .into_iter()
        .chain(property_names(&submitted.properties))
        .chain(carried.map(property_names).unwrap_or_default())
        .collect();
    // What a change to each property would do to the resource: the properties either
    // revision states that are the type's replacement ones ([`template::replacing_properties`]
    // — the name it takes its identity from, and the type's other create-only properties).
    let recreating: Vec<String> =
        template::replacing_properties(&previous.resource_type, &previous.properties)
            .into_iter()
            .chain(template::replacing_properties(
                &submitted.resource_type,
                &submitted.properties,
            ))
            .collect();
    let mut details = Vec::new();
    for property in properties {
        let declared_before = previous.properties.get(property);
        let declared_after = submitted.properties.get(property);
        // The value the resource CARRIES, for a resource whose live configuration the
        // template in force no longer declares; otherwise what the previous declaration
        // resolves to.
        let resolved_before = match carried {
            Some(carried) => carried.get(property).cloned(),
            None => declared_before
                .and_then(|value| template::resolve(value, &comparison.before, budget)),
        };
        let resolved_after = declared_after
            .and_then(|value| template::resolve(value, &comparison.after, budget));
        // The property changes when the template STATES it differently, and also when the
        // template states it the same way but it would DEPLOY differently — a `!Ref` to a
        // parameter the change set revalues. A value this reader cannot evaluate at all is
        // judged by what it is built from instead: it changes when it reads a parameter the
        // change set revalues, which is the one thing known about it.
        //
        // Against a configuration the resource CARRIES there is no declaration to have been
        // edited: the two sides are a value and a declaration, so only the value each would
        // deploy can be compared.
        let edited = carried.is_none() && declared_before != declared_after;
        let revalued = resolved_before != resolved_after;
        let referenced = template::references(declared_after.unwrap_or(&Value::Null));
        let unevaluated = declared_after.is_some() && resolved_after.is_none()
            || declared_before.is_some() && resolved_before.is_none();
        let revalued_parameter = referenced
            .by_value
            .iter()
            .any(|name| comparison.changed_parameters.contains(name.as_str()));
        let changed = edited || revalued || (unevaluated && revalued_parameter);
        if !changed {
            continue;
        }
        // "For `Static` evaluations, CloudFormation can determine that the target value will
        // change, and its value"; `Dynamic` is for the value it "can't determine… because
        // it depends on the result of an intrinsic function" — which here is a value this
        // template reader could not evaluate.
        let evaluated = if declared_after.is_some() && resolved_after.is_none() {
            evaluation::DYNAMIC
        } else {
            evaluation::STATIC
        };
        let recreates = recreation_of(&submitted.resource_type, &recreating, property);
        let target = |evaluation: &str, source: &str, causing_entity: Option<String>| {
            StoredChangeDetail {
                attribute: PROPERTIES_ATTRIBUTE.to_string(),
                name: Some(property.to_string()),
                // What a change to this property does to the resource: the property the
                // resource takes its physical NAME from, for a type whose owning service
                // cannot rename it, is `Always` — a name is fixed when the resource is
                // created, which is why every such name property's "Update requires" is
                // `Replacement` — and so is each of the type's other create-only
                // properties, while a property that carries a create-only value among
                // updatable ones is `Conditionally` ([`recreation_of`]).
                requires_recreation: recreates.to_string(),
                evaluation: evaluation.to_string(),
                change_source: Some(source.to_string()),
                causing_entity,
                drifted: None,
                // The target is this property, and the values are the ones each revision
                // would really DEPLOY for it — the resolved pair the comparison above was
                // made over, which is what the detailed property values are.
                values: Some(TargetValues::of(
                    format!("/{PROPERTIES_ATTRIBUTE}/{property}"),
                    resolved_before.as_ref().or(declared_before),
                    resolved_after.as_ref().or(declared_after),
                )),
            }
        };
        // What the change is attributed to is the ENTITY the property's new value is built
        // from, and the `ChangeSource` is the group that entity belongs to:
        // "`ParameterReference` entities are `Ref` intrinsic functions that get template
        // parameter values", "`ResourceReference` entities are `Ref` intrinsic functions
        // that refer to resources in the template", "`ResourceAttribute` entities are
        // `Fn::GetAtt` intrinsic functions that get resource attribute values", and
        // "`DirectModification` entities are changes that are made directly to the
        // template" — the last for a value built from nothing but itself, which is the whole
        // of a plain template edit and carries no `CausingEntity` ("if the `ChangeSource`
        // value is `DirectModification`, no value is given for `CausingEntity`").
        let attributed = parameter_entity(&referenced, &comparison)
            .map(|parameter| (change_source::PARAMETER_REFERENCE, parameter))
            .or_else(|| resource_entity(&referenced, &comparison));
        match attributed {
            None => details.push(target(evaluated, change_source::DIRECT_MODIFICATION, None)),
            Some((source, entity)) => {
                details.push(target(evaluated, source, Some(entity)));
                // Substituting a parameter's new value into the property is a further
                // change to the property ITSELF, and it is made when the change set is
                // executed: the `Dynamic` modification of the property, with no entity of
                // its own to name.
                if source == change_source::PARAMETER_REFERENCE {
                    details.push(target(
                        evaluation::DYNAMIC,
                        change_source::DIRECT_MODIFICATION,
                        None,
                    ));
                }
            }
        }
    }
    details
}

/// The resource of this template a property's new value reads, and the `ChangeSource` group
/// that reference belongs to: an `Fn::GetAtt` reads an ATTRIBUTE off a resource, a `Ref`
/// names the resource itself, and each entity is spelled the way the intrinsic spells it
/// (so an attribute reference names the whole `<logical id>.<attribute>` path).
fn resource_entity(
    referenced: &template::References,
    comparison: &Comparison<'_>,
) -> Option<(&'static str, String)> {
    referenced
        .attribute_targets()
        .find(|(resource, _)| comparison.declared_resources.contains(resource))
        .map(|(_, path)| (change_source::RESOURCE_ATTRIBUTE, path.to_string()))
        .or_else(|| {
            referenced
                .by_value
                .iter()
                .find(|name| comparison.declared_resources.contains(name.as_str()))
                .map(|name| (change_source::RESOURCE_REFERENCE, name.clone()))
        })
}

/// The parameter a property's new value reads, the one the request REVALUED first: "if you
/// modified the value of the `KeyPairName` parameter, the `CausingEntity` is the name of the
/// parameter" — the entity is the parameter whose value the change set changes, and only
/// where the value reads none of those is it one the template merely declares.
fn parameter_entity(
    referenced: &template::References,
    comparison: &Comparison<'_>,
) -> Option<String> {
    referenced
        .by_value
        .iter()
        .find(|name| comparison.changed_parameters.contains(name.as_str()))
        .or_else(|| {
            referenced
                .by_value
                .iter()
                .find(|name| comparison.declared_parameters.contains(name.as_str()))
        })
        .cloned()
}

/// The property names a declaration's `Properties` object carries; none for a declaration
/// that states no properties at all.
fn property_names(properties: &Value) -> Vec<&str> {
    properties
        .as_object()
        .map(|members| members.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// `ResourceChange$Replacement` read off a `Modify`'s details, as the model documents it:
/// `Always` + `Static` is `True`, `Always` + `Dynamic` is `Conditional`, a `Conditionally`
/// recreation is `Conditional` whatever its evaluation, and with several details "the
/// `Replacement` value depends on the change with the most impact. A `RequiresRecreation`
/// value of `Always` has the most impact, followed by `Conditional`, and then `Never`".
fn replacement_of(details: &[StoredChangeDetail]) -> &'static str {
    let mut answer = replacement::FALSE;
    for detail in details {
        let implied = match (
            detail.requires_recreation.as_str(),
            detail.evaluation.as_str(),
        ) {
            (requires_recreation::ALWAYS, evaluation::STATIC) => replacement::TRUE,
            (requires_recreation::ALWAYS, _) => replacement::CONDITIONAL,
            // `Conditionally`: recreation depends on the value, which is the same impact a
            // recreation this emulator cannot evaluate has.
            (requires_recreation::CONDITIONALLY, _) => replacement::CONDITIONAL,
            (requires_recreation::NEVER, _) => replacement::FALSE,
            _ => replacement::CONDITIONAL,
        };
        answer = match (answer, implied) {
            (replacement::TRUE, _) | (_, replacement::TRUE) => replacement::TRUE,
            (replacement::CONDITIONAL, _) | (_, replacement::CONDITIONAL) => {
                replacement::CONDITIONAL
            }
            _ => replacement::FALSE,
        };
    }
    answer
}

/// The attributes a `Modify`'s details name, in the order the model's `ResourceAttribute`
/// enum lists them and without repetition: `ResourceChange$Scope`, read off the details so
/// the two can never disagree.
fn scope_of(details: &[StoredChangeDetail]) -> Vec<String> {
    RESOURCE_ATTRIBUTE_VALUES
        .iter()
        .filter(|attribute| {
            details
                .iter()
                .any(|detail| detail.attribute == **attribute)
        })
        .map(|attribute| (*attribute).to_string())
        .collect()
}

/// The `Add` of a resource only the submitted template declares.
fn addition(resource: &template::DeclaredResource) -> StoredResourceChange {
    StoredResourceChange {
        action: change_action::ADD.to_string(),
        logical_resource_id: resource.logical_id.clone(),
        resource_type: resource.resource_type.clone(),
        // "Resources that you are adding don't have physical IDs because they haven't been
        // created."
        physical_resource_id: None,
        scope: Vec::new(),
        replacement: None,
        details: Vec::new(),
        policy_action: None,
        resource_drift_status: None,
        nested_change_set_id: None,
        before_context: None,
        after_context: None,
    }
}

/// The `Remove` of a resource the submitted template no longer declares.
///
/// "The action that will be taken on the physical resource when the change set is executed"
/// is what the resource's own `DeletionPolicy` says ([`delete_action`]): the resource the
/// change takes out of the stack is deleted, retained or snapshotted, and the change reports
/// which — unless the stack holds no physical resource under that logical id at all
/// ([`acts_on_a_physical_resource`]), where the change set's execution does nothing to a
/// physical resource and so reports no action on one.
fn removal(
    resource: &template::DeclaredResource,
    physical_resource_id: Option<String>,
) -> StoredResourceChange {
    let acts = acts_on_a_physical_resource(&physical_resource_id);
    StoredResourceChange {
        action: change_action::REMOVE.to_string(),
        logical_resource_id: resource.logical_id.clone(),
        resource_type: resource.resource_type.clone(),
        physical_resource_id,
        scope: Vec::new(),
        replacement: None,
        details: Vec::new(),
        policy_action: acts.then(|| delete_action(resource).to_string()),
        resource_drift_status: None,
        nested_change_set_id: None,
        before_context: None,
        after_context: None,
    }
}

/// Whether a change ACTS on a physical resource: whether the stack holds one under the
/// change's logical id ([`computed_changes`]).
///
/// `ResourceChange$PolicyAction` is "the action that will be taken on the physical resource
/// when the change set is executed", so it is reported only where there IS one to act on. A
/// logical id the stack holds no physical resource for — the create of that resource was
/// refused, so nothing was ever made — has nothing for the execution to replace, delete,
/// retain or snapshot: the resource is CREATED instead
/// ([`super::provisioning::already_deployed`]), and a plan that announced an action on a
/// physical resource would be announcing one on a resource that does not exist.
fn acts_on_a_physical_resource(physical_resource_id: &Option<String>) -> bool {
    physical_resource_id
        .as_deref()
        .is_some_and(|physical_resource_id| !physical_resource_id.is_empty())
}

/// The `Modify` of a resource both templates declare, with its scope, its replacement and
/// the action it takes on the existing physical resource read off the details.
fn modification(
    resource: &template::DeclaredResource,
    physical_resource_id: Option<String>,
    details: Vec<StoredChangeDetail>,
) -> StoredResourceChange {
    let replaces = replacement_of(&details);
    let acts = acts_on_a_physical_resource(&physical_resource_id);
    StoredResourceChange {
        action: change_action::MODIFY.to_string(),
        logical_resource_id: resource.logical_id.clone(),
        resource_type: resource.resource_type.clone(),
        physical_resource_id,
        scope: scope_of(&details),
        replacement: Some(replaces.to_string()),
        details,
        // Only a change that recreates the resource does anything to the physical resource
        // that is already there, and what it does is that resource's
        // `UpdateReplacePolicy` — where there is one there at all
        // ([`acts_on_a_physical_resource`]).
        policy_action: (replaces != replacement::FALSE && acts)
            .then(|| replace_action(resource).to_string()),
        resource_drift_status: None,
        nested_change_set_id: None,
        before_context: None,
        after_context: None,
    }
}

/// The `Modify` of one resource the stack's template declares and the service that owns it
/// reports DIFFERENTLY — a drift-aware change, as a change set whose `DeploymentMode`
/// compares the template against the resource's actual state reports it.
///
/// Such a change puts the TEMPLATE's value back over the live one, so its before value is
/// the live one and its after value the template's
/// ([`DriftedValues`]) — one detail per property the two disagree about, in the order the
/// comparison found them. It changes no property the template states differently from the
/// previous template, so it never recreates the resource: `Replacement` is `False`.
pub fn drift_change(drifted: &super::drift::DriftedResource) -> StoredResourceChange {
    let details: Vec<StoredChangeDetail> = drifted
        .differences
        .iter()
        .map(|difference| StoredChangeDetail {
            attribute: PROPERTIES_ATTRIBUTE.to_string(),
            // "If the `Attribute` value is `Properties`, the name of the property": the
            // DECLARED property the difference is inside, which is the first segment of the
            // fully-qualified path the comparison reports it under.
            name: difference
                .property_path
                .trim_start_matches('/')
                .split('/')
                .next()
                .map(ToString::to_string),
            requires_recreation: requires_recreation::NEVER.to_string(),
            // Both values are READ: the live one from the owning service, the other from the
            // template — so the change and its value are determined, which is what `Static`
            // reports.
            evaluation: evaluation::STATIC.to_string(),
            change_source: Some(change_source::DIRECT_MODIFICATION.to_string()),
            causing_entity: None,
            drifted: Some(DriftedValues {
                before_value: difference.actual_value.clone(),
                after_value: difference.expected_value.clone(),
            }),
            // The target's path and its two values are the drift comparison's own, reported
            // beside where each of them came from: the property path it found the difference
            // under, the value the resource really carries and the one the template declares.
            values: Some(TargetValues {
                path: format!(
                    "/{PROPERTIES_ATTRIBUTE}/{}",
                    difference.property_path.trim_start_matches('/')
                ),
                before_value: Some(difference.actual_value.clone()),
                after_value: Some(difference.expected_value.clone()),
                change_type: attribute_change_type::MODIFY.to_string(),
            }),
        })
        .collect();
    StoredResourceChange {
        action: change_action::MODIFY.to_string(),
        logical_resource_id: drifted.logical_id.clone(),
        resource_type: drifted.resource_type.clone(),
        physical_resource_id: Some(drifted.physical_resource_id.clone()),
        scope: scope_of(&details),
        replacement: Some(replacement::FALSE.to_string()),
        details,
        policy_action: None,
        // The resource's properties were modified outside CloudFormation, which is what
        // `MODIFIED` states.
        resource_drift_status: Some(super::resource_drift_status::MODIFIED.to_string()),
        nested_change_set_id: None,
        // The two values this change carries are the DETAIL's own ([`DriftedValues`]): what
        // a drift-aware comparison read off the live resource and off the template, not the
        // whole document either side would deploy.
        before_context: None,
        after_context: None,
    }
}

/// What a replacement does to the OLD physical resource: its `UpdateReplacePolicy`, which
/// "specif[ies] how CloudFormation handles the [old] resource when the resource is
/// replaced" and defaults to deleting it.
fn replace_action(resource: &template::DeclaredResource) -> &'static str {
    match resource
        .declaration
        .get("UpdateReplacePolicy")
        .and_then(Value::as_str)
    {
        Some(update_replace_policy::RETAIN) => policy_action::REPLACE_AND_RETAIN,
        Some(update_replace_policy::SNAPSHOT) => policy_action::REPLACE_AND_SNAPSHOT,
        _ => policy_action::REPLACE_AND_DELETE,
    }
}

/// What taking a resource OUT of the stack does to the physical resource: its
/// `DeletionPolicy`, which "specif[ies] how CloudFormation handles the resource" when the
/// stack stops managing it and defaults to deleting it.
///
/// `RetainExceptOnCreate` keeps the resource exactly as `Retain` does for everything but a
/// create that fails — "retain the resource only if the resource creation fails" — and a
/// change that removes a resource the stack HAS is not that case, so it is reported as the
/// retention it is.
fn delete_action(resource: &template::DeclaredResource) -> &'static str {
    match resource
        .declaration
        .get("DeletionPolicy")
        .and_then(Value::as_str)
    {
        Some(deletion_policy::RETAIN | deletion_policy::RETAIN_EXCEPT_ON_CREATE) => {
            policy_action::RETAIN
        }
        Some(deletion_policy::SNAPSHOT) => policy_action::SNAPSHOT,
        _ => policy_action::DELETE,
    }
}

/// The `ResourceAttribute` a change to the STACK's own tags triggers on each of its
/// resources: `Scope` "indicates which resource attribute is triggering this update, such as
/// a change in the resource attribute's `Metadata`, `Properties`, or `Tags`".
const TAGS_ATTRIBUTE: &str = "Tags";

/// Add what the stack's own TAG SET does to a plan: the stack's tags are the tags of the
/// resources it manages — "key-value pairs to associate with this stack. CloudFormation also
/// propagates these tags to resources in the stack" — so applying a tag set to the stack is
/// applying it to every resource the stack holds, under the `Tags` attribute rather than
/// under `Properties`.
///
/// There is work to plan where the submitted revision states the tags DIFFERENTLY from the
/// ones in force — the resources carry the set in force, so a revision restating it plans
/// nothing — and where the APPLICATION of the set in force is still outstanding
/// ([`super::stack::StoredStack::tag_application_outstanding`]), which is `outstanding` here:
/// a stack parked by an operation that failed and was left standing never finished applying
/// the set that operation put in force, so even a revision restating it asks for that
/// application. A revision with no tags at all has nothing to apply, so even there it plans
/// nothing.
///
/// A stack whose last operation COMPLETED carries the set in force on every resource it holds,
/// and so does a stack parked by a failed update that left the tags alone — the operation
/// before it applied them — so a revision restating the set plans nothing on either. (A
/// resource whose PROPERTIES a parked operation could not carry through is the other half of
/// the comparison, and that one is read off the live resource: [`carried_configurations`].)
///
/// One `Modify` per resource the stack HOLDS whose tags CloudFormation really keeps and the
/// plan does not already take away or bring into being: a resource the plan removes is gone
/// (nothing re-tags it) and one it adds is created with the new set, so neither carries a tag
/// change, while a resource the plan already reports a change for gains the `Tags` attribute
/// beside the ones it carries — its `Scope` is read off its details, so the two can never
/// disagree ([`scope_of`]).
///
/// Which resources those are is which of them the stack holds a physical resource for
/// ([`super::stack::holds_the_resource`]) and which of them a service TAGS: CloudFormation
/// propagates a stack's tags to the resources in it through each type's own tagging, so a
/// resource an operation took away or whose create the owning service refused is not there to
/// be tagged, and a resource of a type no service here holds — one this emulator leaves
/// declared-only ([`super::provisioning::owner_for`]) — holds no tags for a tag change to
/// change, so the execution of the change set would ask nobody anything about either.
///
/// Re-tagging never recreates a resource (`RequiresRecreation` `Never`), the two tag sets are
/// both read so the change and its value are determined (`Static`), and it is the revision
/// itself that states them, which is what `DirectModification` is.
///
/// `in_force` is the stack's tag set, `submitted` the one the revision would put in force and
/// `outstanding` whether the stack has still to apply the set in force to its resources.
pub fn tag_changes(
    changes: &mut Vec<StoredResourceChange>,
    in_force: &[super::StoredTag],
    submitted: &[super::StoredTag],
    deployed: &[DeployedResource],
    outstanding: bool,
) {
    let asks_for_an_application = outstanding && !submitted.is_empty();
    if super::same_tags(in_force, submitted) && !asks_for_an_application {
        return;
    }
    let detail = StoredChangeDetail {
        attribute: TAGS_ATTRIBUTE.to_string(),
        // "For all other attributes, the value is null": `Tags` is not `Properties`, so the
        // detail names no property.
        name: None,
        requires_recreation: requires_recreation::NEVER.to_string(),
        evaluation: evaluation::STATIC.to_string(),
        // "If the `ChangeSource` value is `DirectModification`, no value is given for
        // `CausingEntity`" — and a tag change names no source at all: it is not something
        // the template states, so there is no entity and no group one could belong to.
        change_source: None,
        causing_entity: None,
        drifted: None,
        // The target is the resource's tag set, and the two values are the sets the stack
        // carries and the revision would put in force ([`tag_set_text`]).
        values: Some(TargetValues::of(
            format!("/{TAGS_ATTRIBUTE}"),
            Some(&tag_set_text(in_force)),
            Some(&tag_set_text(submitted)),
        )),
    };
    for held in deployed {
        // A resource the stack does not really hold is not a resource to re-tag: a record it
        // kept at `DELETE_COMPLETE` is one the operation that disposed of it took away, and
        // one whose create the owning service refused describes a resource that was never
        // made ([`super::stack::holds_the_resource`]).
        if !super::stack::holds_the_resource(held) {
            continue;
        }
        // ...and neither is a resource no service of this emulator holds: nothing tags it.
        if super::provisioning::owner_for(&held.resource.resource_type).is_none() {
            continue;
        }
        let logical_id = held.resource.logical_id.as_str();
        match changes
            .iter_mut()
            .find(|change| change.logical_resource_id == logical_id)
        {
            Some(change) if change.action == change_action::MODIFY => {
                if change
                    .details
                    .iter()
                    .any(|detail| detail.attribute == TAGS_ATTRIBUTE)
                {
                    continue;
                }
                change.details.push(detail.clone());
                change.details.sort_by(detail_order);
                change.scope = scope_of(&change.details);
            }
            // The resource is being created or taken away by this plan: the tags it ends up
            // with are the ones that create carries, and a resource that is gone has none.
            Some(_) => {}
            None => {
                let mut tagged = StoredResourceChange {
                    action: change_action::MODIFY.to_string(),
                    logical_resource_id: held.resource.logical_id.clone(),
                    resource_type: held.resource.resource_type.clone(),
                    physical_resource_id: Some(held.resource.physical_resource_id.clone()),
                    scope: Vec::new(),
                    replacement: Some(replacement::FALSE.to_string()),
                    details: vec![detail.clone()],
                    policy_action: None,
                    resource_drift_status: None,
                    nested_change_set_id: None,
                    before_context: None,
                    after_context: None,
                };
                tagged.scope = scope_of(&tagged.details);
                changes.push(tagged);
            }
        }
    }
}

/// The text one tag set is reported as on the target of a tag change: its pairs as JSON,
/// keyed and ordered by tag key, which is how two sets are compared ([`super::same_tags`]).
fn tag_set_text(tags: &[super::StoredTag]) -> Value {
    let set: BTreeMap<&str, &str> = tags
        .iter()
        .map(|tag| (tag.key.as_str(), tag.value.as_str()))
        .collect();
    Value::String(
        ::serde_json::to_string(&set).unwrap_or_else(|_| String::from("{}")),
    )
}
