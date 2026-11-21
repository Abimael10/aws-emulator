// SPDX-License-Identifier: CC0-1.0

//! The NESTED change sets a change set expands into: the plan of each child stack a change
//! of the planned stack would change, and the id the parent's change reports for it.
//!
//! ## The rule this module implements
//!
//! "If you create a change set for a stack that contains nested stacks, set
//! `IncludeNestedStacks` to `True` to create the change set for the entire stack hierarchy"
//! (`CreateChangeSetInput$IncludeNestedStacks`), and a change on a resource of the
//! nested-stack type then carries `ResourceChange$ChangeSetId` — "the change set ID of the
//! nested change set". So such a change set is not one plan but a HIERARCHY of them: the
//! stack the request named is planned as always, and every change it reports on a child stack
//! that already exists is planned too, as a change set of that child, which the parent's
//! change names by id. That id is how a caller walks into the child's own changes, so it
//! names a change set this store really holds: `DescribeChangeSet` answers for it, reporting
//! the child's plan, the parent change set it belongs to
//! (`DescribeChangeSetOutput$ParentChangeSetId`) and the one at the top of the hierarchy
//! (`$RootChangeSetId`).
//!
//! The expansion follows the hierarchy as far as the templates reach: a nested change set's
//! own changes on a child stack carry their nested id in turn, up to [`MAX_NESTED_DEPTH`]
//! levels and [`MAX_NESTED_CHILDREN`] children in all — the bounds that keep one request's
//! work finite however deeply a caller's templates nest.
//!
//! A child's plan is computed from the SAME two sides the parent's is: the template the
//! parent's declaration addresses (`AWS::CloudFormation::Stack`'s `TemplateURL`, read through
//! [`super::template_url`] like every other template submitted by reference) with the
//! `Parameters` that declaration passes it, against the revision the child stack carries. It
//! is therefore the plan an execution of the hierarchy would carry out on that child.
//!
//! The declarations the hierarchy is read from are the ones of the template the change set
//! would DEPLOY — the submitted body where the request submits one and the stack's own where
//! it reuses it (`UsePreviousTemplate`) — and each of them is EVALUATED under the parameter
//! values that change set would deploy with ([`declared_children`]), since a `TemplateURL` a
//! declaration states as an expression addresses whatever those values make of it. So a
//! hierarchy whose child templates are chosen by parameter is expanded from the values this
//! request puts in force rather than from the template's text.
//!
//! Only a child stack that EXISTS has a revision to plan against, so only a change on one is
//! expanded: a child a change set would create does not exist to hold a change set of its
//! own, and a declaration whose `TemplateURL` is not a readable object has no template to
//! plan from — neither is reported with an id naming a change set nothing holds.
//!
//! ## What a nested change set may be executed as
//!
//! Nothing: "to execute a change set for the entire stack hierarchy, you must execute the
//! root change set" (`ExecuteChangeSet`), so a nested change set is created with its
//! `ExecutionStatus` UNAVAILABLE and the refusal every non-executable change set earns is
//! what `ExecuteChangeSet` answers for it.

use std::collections::BTreeMap;

use ::serde_json::Value;

use super::{
    change_action, change_plan::changes_against, change_set_status, execution_status,
    new_change_set_id, parameters_in_force, template, ComAmazonawsCloudformationCloudformationState,
    DeployedStack, PendingChangeSetStatus, StoredChangeSet, StoredParameter,
    StoredResourceChange, STACK_RESOURCE_TYPE,
};

/// How many levels of nested stacks one call expands.
///
/// Each level costs one read of a child template and one comparison per child, so the depth a
/// caller's templates can ask for is bounded here rather than by the templates themselves.
/// Set to the depth the parent/child relation itself is walked to
/// ([`super::stack_nesting`]'s own bound is the same order), which is past the nesting a
/// hierarchy of templates reaches under the documented stack-name maximum.
pub const MAX_NESTED_DEPTH: usize = 5;

/// How many nested change sets one call creates in all, across every level.
///
/// A template may declare as many nested stacks as it may declare resources, and each of them
/// may declare more, so the product is bounded here: past this many children the hierarchy is
/// not expanded further, and no change reports an id for a change set that was not created.
pub const MAX_NESTED_CHILDREN: usize = 32;

/// The `AWS::CloudFormation::Stack` property naming the child's template: "the URL of a
/// template that specifies the stack that you want to create as a resource".
const TEMPLATE_URL: &str = "TemplateURL";

/// The property carrying the values the parent passes the child: "the set value pairs that
/// represent the parameters passed to CloudFormation when this nested stack is created".
const PARAMETERS: &str = "Parameters";

/// The separator a nested change set's name and the path of a child are built with: the one
/// character `ChangeSetName`'s own pattern admits beside letters and digits
/// (`^[a-zA-Z][-a-zA-Z0-9]*$`).
const NAME_SEPARATOR: char = '-';

/// The separator between the logical ids of a child's PATH through the hierarchy, which is
/// what keys [`Children`]: a logical id is unique within one template, not across the
/// hierarchy, so a child is addressed by the chain of ids that reaches it.
const PATH_SEPARATOR: char = '/';

/// The most characters a change-set name may carry: `ChangeSetName` is
/// `@length(min: 1, max: 128)`, and a name this service derives is held to the member's own
/// maximum exactly as one a request states is.
const CHANGE_SET_NAME_MAX_LEN: usize = 128;

/// One child stack's template as the parent's declaration addresses it, read so the child's
/// plan is computed from the document an execution would deploy on it.
#[derive(Debug, Clone)]
pub struct PlannedChild {
    /// The body the declaration's `TemplateURL` addressed.
    pub body: String,
    /// The PROCESSED stage of that body — the document an execution would deploy on the child
    /// — computed where the body was read, outside the store lock, exactly as the submitting
    /// operation computes its own.
    pub processed: template::Processed,
    /// That stage as the comparison reads it, read where it was processed for the same
    /// reason: a caller-sized document is parsed outside the lock.
    pub declared: template::Template,
    /// The values the declaration passes the child, as its parameter set.
    pub parameters: Vec<StoredParameter>,
    /// The parameter set in force on the child under that template: the values the
    /// declaration passes, plus the template's own defaults for the rest
    /// ([`parameters_in_force`]).
    pub parameters_in_force: Vec<StoredParameter>,
}

/// Every child stack the hierarchy reaches, keyed by its PATH: the logical id of the
/// declaration in the planned stack's template, then the one in each template below it.
pub type Children = BTreeMap<String, PlannedChild>;

/// One nested-stack declaration, as the walk below carries it: the logical id the template
/// declares it under, the `TemplateURL` it addresses and the values it passes the child.
type Declaration = (String, String, Vec<StoredParameter>);

/// What the declarations of one template are EVALUATED in: the identity of the stack that
/// template is in force on, which is what a pseudo parameter in one of them stands for.
///
/// `stack_name` and `stack_id` are empty where the stack they would name is not known at the
/// point the hierarchy is read — a stack this call would create, and every stack BELOW the one
/// the request named, whose own identity this service only mints when the hierarchy is
/// deployed. An empty one resolves to nothing rather than to a value this service invented, so
/// a declaration built from it addresses no readable object and that child is simply not
/// expanded.
#[derive(Debug, Clone, Copy)]
pub struct PlanningScope<'a> {
    pub region: &'a str,
    pub stack_name: &'a str,
    pub stack_id: &'a str,
    /// The Amazon SNS topics in force on that stack, which is what `AWS::NotificationARNs`
    /// stands for.
    pub notification_arns: &'a [String],
}

/// The nested-stack declarations one template carries: the logical id of each, the
/// `TemplateURL` it addresses and the parameter values it passes.
///
/// Each declaration is EVALUATED first, against `values` — the parameter values in force on
/// the stack that template is the revision of — because what a property declares and what it
/// DEPLOYS are two different things: a `TemplateURL` stated as `!Ref ChildTemplate` addresses
/// whatever those values make of it, and a hierarchy whose child templates are chosen by
/// parameter is read from the values rather than from the template's text. A declaration this
/// cannot evaluate at all is read as it stands — that text is what is known about it.
///
/// A declaration whose `TemplateURL` does not evaluate to text addresses nothing this can
/// read, and is left out, as is one that states no `TemplateURL` at all: there is no child
/// template to plan from either way.
fn declared_children(
    declared: &template::Template,
    scope: PlanningScope<'_>,
    values: &BTreeMap<String, String>,
) -> Vec<Declaration> {
    let parameter_types = super::ssm_parameters::parameter_types_in_force(declared);
    // A declaration is evaluated for the ONE property that addresses a template, so it is
    // evaluated against the parameters and the stack's own identity alone: the resources of
    // this template are not deployed yet (the hierarchy is a PLAN), and neither an export nor
    // another stack's output is something this reading claims to know. A `TemplateURL` built
    // from one of them therefore resolves to nothing, exactly as one stated as an expression
    // this service cannot evaluate does.
    let no_resources = BTreeMap::new();
    let no_imports = BTreeMap::new();
    let no_stack_outputs = super::stack_outputs::Values::new();
    let evaluated = template::Scope {
        stack: template::StackScope {
            region: scope.region,
            stack_name: scope.stack_name,
            stack_id: scope.stack_id,
            notification_arns: scope.notification_arns,
        },
        parameters: values,
        parameter_types: &parameter_types,
        resources: &no_resources,
        mappings: &declared.mappings,
        conditions: &declared.conditions,
        imports: &no_imports,
        stack_outputs: &no_stack_outputs,
    };
    // ONE budget for the whole template, as a deployment evaluates a whole template out of
    // one: the work one template's declarations can ask for is bounded, not each child's.
    let mut budget = template::Budget::new();
    declared
        .resources
        .iter()
        .filter(|resource| resource.resource_type == STACK_RESOURCE_TYPE)
        .filter_map(|resource| {
            let properties = template::resolve(&resource.properties, &evaluated, &mut budget)
                .unwrap_or_else(|| resource.properties.clone());
            let url = match properties.get(TEMPLATE_URL) {
                Some(Value::String(url)) if !url.is_empty() => url.clone(),
                _ => return None,
            };
            Some((
                resource.logical_id.clone(),
                url,
                declared_parameters(&properties),
            ))
        })
        .collect()
}

/// The parameter set a nested-stack declaration passes its child: one entry per member of its
/// `Parameters` map, with the value as the text a parameter value is — every CloudFormation
/// parameter value is text, which is the same reading the nested-stack provider sends a
/// create with.
///
/// In KEY order, for the reason that provider orders its own request so: `serde_json`'s map
/// order depends on a feature any crate in the build may enable, and what this service stores
/// must not depend on how the workspace was compiled.
fn declared_parameters(properties: &Value) -> Vec<StoredParameter> {
    let Some(declared) = properties.get(PARAMETERS).and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut parameters: Vec<StoredParameter> = declared
        .iter()
        .map(|(key, value)| StoredParameter {
            key: key.clone(),
            value: match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            },
            no_echo: false,
            resolved_value: None,
        })
        .collect();
    parameters.sort_by(|one, other| one.key.cmp(&other.key));
    parameters
}

/// Every child template the hierarchy under `declared` reaches, read through the same
/// reference resolution every template submitted by reference goes through
/// ([`super::template_url::fetch`]).
///
/// `declared` is the template the change set would DEPLOY — the submitted body where the
/// request submits one and the stack's own where it reuses it — and `values` the parameter
/// values it would deploy with, which is what its declarations are evaluated against
/// ([`declared_children`]).
///
/// Read HERE, before the store is locked, for the reason the parent's own template is: a
/// caller-sized object is read outside the lock. A child whose template cannot be read is
/// simply not expanded — nothing of it is planned, so nothing reports a change set for it.
pub async fn fetched_children<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    scope: PlanningScope<'_>,
    declared: Option<&template::Template>,
    values: &BTreeMap<String, String>,
) -> Children {
    let region = scope.region;
    let mut children = Children::new();
    let Some(declared) = declared else {
        return children;
    };
    // The frontier of the walk: the declarations of one template, under the path that reaches
    // it. Breadth-first, so the children of the planned stack are read before any level below
    // them and the count bound is spent on the nearest ones.
    let mut frontier: Vec<(String, Vec<Declaration>)> = vec![(
        String::new(),
        declared_children(declared, scope, values),
    )];
    for _ in 0..MAX_NESTED_DEPTH {
        let mut below = Vec::new();
        for (prefix, declarations) in frontier {
            for (logical_id, url, parameters) in declarations {
                if children.len() >= MAX_NESTED_CHILDREN {
                    return children;
                }
                let Ok(body) = super::template_url::fetch(engine, ctx, region, &url).await else {
                    continue;
                };
                let path = child_path(&prefix, &logical_id);
                // The stage an execution would deploy on the child, and that stage as the
                // comparison reads it: computed HERE, where the body was read, so the
                // expansion under the store lock only compares two already-read templates.
                let processed =
                    template::process_with_parameters(&body, &supplied_values(&parameters));
                let declared = processed
                    .body(&body)
                    .and_then(super::declared_template)
                    .unwrap_or_default();
                let parameters_in_force = parameters_in_force(&body, parameters.clone());
                // The level below this child is read from ITS revision under ITS values: the
                // parameter set the declaration above puts in force on it, with that
                // template's own defaults for the rest. The child stack's own name and id are
                // not known until the hierarchy is deployed, so the scope below carries
                // neither ([`PlanningScope`]).
                let declared_below = declared_children(
                    &declared,
                    PlanningScope {
                        region,
                        stack_name: "",
                        stack_id: "",
                        notification_arns: &[],
                    },
                    &supplied_values(&parameters_in_force),
                );
                children.insert(
                    path.clone(),
                    PlannedChild {
                        body,
                        processed,
                        declared,
                        parameters,
                        parameters_in_force,
                    },
                );
                if !declared_below.is_empty() {
                    below.push((path, declared_below));
                }
            }
        }
        if below.is_empty() {
            break;
        }
        frontier = below;
    }
    children
}

/// The change set a nested change set belongs to: the one whose change names it.
#[derive(Debug, Clone, Copy)]
pub struct Parent<'a> {
    pub region: &'a str,
    pub change_set_id: &'a str,
    pub change_set_name: &'a str,
    /// The change set at the TOP of the hierarchy, which for the change set the request
    /// created is that change set itself.
    pub root_change_set_id: &'a str,
}

/// Expand `changes` into the hierarchy: give every change on a child stack that exists the id
/// of a change set created on that child, and answer those change sets.
///
/// `changes` is the parent's plan, which this WRITES the nested ids into; the records it
/// answers are the nested change sets the caller stores beside the parent's own. Nothing is
/// written to the store here, so the caller holds one lock for the whole hierarchy.
pub fn expand(
    store: &ComAmazonawsCloudformationCloudformationState,
    parent: Parent<'_>,
    children: &Children,
    changes: &mut [StoredResourceChange],
    now: ::std::time::SystemTime,
) -> Vec<StoredChangeSet> {
    let mut nested = Vec::new();
    expand_at(store, parent, children, "", changes, now, &mut nested, 0);
    nested
}

/// [`expand`] at one level of the hierarchy: `prefix` is the path that reaches the stack whose
/// plan `changes` is, and `depth` how many levels below the planned stack that is.
#[allow(clippy::too_many_arguments)]
fn expand_at(
    store: &ComAmazonawsCloudformationCloudformationState,
    parent: Parent<'_>,
    children: &Children,
    prefix: &str,
    changes: &mut [StoredResourceChange],
    now: ::std::time::SystemTime,
    nested: &mut Vec<StoredChangeSet>,
    depth: usize,
) {
    if depth >= MAX_NESTED_DEPTH {
        return;
    }
    for change in changes {
        if change.resource_type != STACK_RESOURCE_TYPE {
            continue;
        }
        // A child stack a change set would CREATE or REMOVE has no revision of its own to
        // plan a change set against: only the child the plan would change is expanded.
        if change.action != change_action::MODIFY {
            continue;
        }
        let Some(child_stack_id) = change.physical_resource_id.clone() else {
            continue;
        };
        let path = child_path(prefix, &change.logical_resource_id);
        let Some(child) = children.get(&path) else {
            continue;
        };
        // The child stack the resource's physical id names, as this store holds it: a
        // reference to a stack nothing here holds is not a child this can plan for.
        let Some(stack) = store
            .resolve(parent.region, &child_stack_id)
            .filter(|stack| stack.is_live())
        else {
            continue;
        };
        // The store's own bounds on how many change sets it holds, counted with the records
        // this expansion has already minted and the parent's own, which the caller stores
        // after these: the hierarchy is as bounded as every other write to the store.
        let minting = nested.len() + 1;
        if store.held_change_sets() + minting >= super::MAX_CHANGE_SETS
            || store.change_sets_of(&stack.stack_id).count() + minting
                >= super::MAX_CHANGE_SETS_PER_STACK
        {
            continue;
        }
        let mut child_changes = changes_against(
            Some(DeployedStack {
                region: &stack.region,
                stack_name: &stack.stack_name,
                stack_id: &stack.stack_id,
                body: stack.template_body.as_deref(),
                processed: &stack.processed_template,
                parameters: &stack.parameters,
                deployed: &stack.resources,
                status: &stack.status,
                imports: &stack.imports,
                notification_arns: &stack.notification_arns,
            }),
            // Both already read, outside this lock, where the child's template was
            // ([`fetched_children`]): the only caller-driven work left here is the
            // comparison itself, which is what the parent's own plan costs too.
            &child.declared,
            &child.parameters_in_force,
        );
        let change_set_name = nested_change_set_name(parent.change_set_name, &path);
        let change_set_id = new_change_set_id(parent.region, &change_set_name);
        // The level below this child, planned against ITS children the same way: a nested
        // change set's own change on a child stack names a change set too, so a caller walks
        // the whole hierarchy by the same member.
        expand_at(
            store,
            Parent {
                region: parent.region,
                change_set_id: &change_set_id,
                change_set_name: &change_set_name,
                root_change_set_id: parent.root_change_set_id,
            },
            children,
            &path,
            &mut child_changes,
            now,
            nested,
            depth + 1,
        );
        // "The change set ID of the nested change set": written onto the parent's own change,
        // which is what a caller recurses with.
        change.nested_change_set_id = Some(change_set_id.clone());
        nested.push(record(
            parent,
            &stack.stack_id,
            &stack.stack_name,
            &stack.drift_status,
            change_set_id,
            change_set_name,
            child,
            child_changes,
            now,
        ));
    }
}

/// One nested change set's record: the child's plan, the configuration it would deploy with
/// and the hierarchy it belongs to.
///
/// It settles exactly as the change set the request created does — answered while
/// CloudFormation is still computing it, and reaching `CREATE_COMPLETE` (or `FAILED` for a
/// plan that would change nothing) once the clock makes that due — because it is created by
/// the same call. Its `ExecutionStatus` stays `UNAVAILABLE`: the hierarchy is executed
/// through its root.
#[allow(clippy::too_many_arguments)]
fn record(
    parent: Parent<'_>,
    stack_id: &str,
    stack_name: &str,
    stack_drift_status: &str,
    change_set_id: String,
    change_set_name: String,
    child: &PlannedChild,
    changes: Vec<StoredResourceChange>,
    now: ::std::time::SystemTime,
) -> StoredChangeSet {
    let settles_to = match child.processed.unavailable_reason() {
        Some(reason) => (
            change_set_status::FAILED,
            Some(format!(
                "The template's transforms could not be applied: {reason}"
            )),
        ),
        None if changes.is_empty() => (
            change_set_status::FAILED,
            Some(super::NO_CHANGES_REASON.to_string()),
        ),
        None => (change_set_status::CREATE_COMPLETE, None),
    };
    StoredChangeSet {
        change_set_id,
        change_set_name,
        stack_id: stack_id.to_string(),
        stack_name: stack_name.to_string(),
        // The child stack exists, so its nested change set is a plan of an update of it.
        change_set_type: super::change_set_type::UPDATE.to_string(),
        description: None,
        template_body: child.body.clone(),
        processed_template: child.processed.clone(),
        parameters: child.parameters_in_force.clone(),
        // The acknowledgements, the topics, the tags and the roles of a hierarchy are the
        // ROOT request's: a nested change set is created by CloudFormation itself rather than
        // by a request of its own, so it states none of its own.
        capabilities: Vec::new(),
        tags: Vec::new(),
        notification_arns: Vec::new(),
        role_arn: None,
        rollback_configuration: None,
        deployment_config: None,
        deployment_mode: None,
        resource_types: Vec::new(),
        on_stack_failure: None,
        include_nested_stacks: true,
        import_existing_resources: false,
        client_token: None,
        creation_time: now,
        status: change_set_status::CREATE_PENDING.to_string(),
        status_reason: None,
        execution_status: execution_status::UNAVAILABLE.to_string(),
        stack_drift_status: stack_drift_status.to_string(),
        pending: Some(PendingChangeSetStatus {
            status: Some(settles_to.0.to_string()),
            status_reason: settles_to.1,
            // "To execute a change set for the entire stack hierarchy, you must execute the
            // root change set": this one is never offered for execution, so its execution
            // status is not a transition that becomes due.
            execution_status: None,
            due_at: now + super::STATUS_TRANSITION_DELAY,
            awaits_the_stack: false,
        }),
        // A nested change set is planned from the child template this request already read,
        // so there is no macro left for its creation to perform.
        pending_macros: None,
        changes,
        // Nothing here can activate a Hook, for the reason the parent change set plans none
        // ([`super::change_set_hooks`]).
        hooks: Vec::new(),
        parent_change_set_id: Some(parent.change_set_id.to_string()),
        root_change_set_id: Some(parent.root_change_set_id.to_string()),
        // Nothing has discarded this record: a delete is what sets that.
        discarded: false,
    }
}

/// The path that addresses one child: the chain of logical ids that reaches it from the stack
/// the request named.
fn child_path(prefix: &str, logical_id: &str) -> String {
    if prefix.is_empty() {
        return logical_id.to_string();
    }
    format!("{prefix}{PATH_SEPARATOR}{logical_id}")
}

/// The name a nested change set carries: the parent change set's name and the path of the
/// child it plans for.
///
/// CloudFormation names a nested change set itself — no request states one — so the name is
/// derived from what the hierarchy already states, which makes it unique among the change
/// sets of that child stack (one path reaches one child once per parent change set) and keeps
/// it within `ChangeSetName`'s own pattern and maximum: the path's separators become the one
/// punctuation that pattern admits, and the name is cut to that maximum.
fn nested_change_set_name(parent_change_set_name: &str, path: &str) -> String {
    let separated: String = path
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                NAME_SEPARATOR
            }
        })
        .collect();
    format!("{parent_change_set_name}{NAME_SEPARATOR}{separated}")
        .chars()
        .take(CHANGE_SET_NAME_MAX_LEN)
        .collect()
}

/// The values a child's template is processed with: its parameter set as the expression scope
/// reads it.
fn supplied_values(parameters: &[StoredParameter]) -> BTreeMap<String, String> {
    parameters
        .iter()
        .map(|parameter| (parameter.key.clone(), parameter.value.clone()))
        .collect()
}

/// Every change set of the hierarchy BELOW `change_set_id`, and that change set itself.
///
/// "If `IncludeNestedStacks` specifies `True` during the creation of the nested change set,
/// then `DeleteChangeSet` will delete all change sets that belong to the stacks hierarchy"
/// (`DeleteChangeSet`): a nested change set exists only as part of the one that created it,
/// so what a caller discards is the whole hierarchy rather than the root alone. Walked
/// through [`StoredChangeSet::parent_change_set_id`], which is where the relation is
/// recorded, and bounded by the depth one call expands ([`MAX_NESTED_DEPTH`]).
pub fn hierarchy_of(
    store: &ComAmazonawsCloudformationCloudformationState,
    change_set_id: &str,
) -> ::std::collections::BTreeSet<String> {
    let mut hierarchy = ::std::collections::BTreeSet::new();
    hierarchy.insert(change_set_id.to_string());
    for _ in 0..MAX_NESTED_DEPTH {
        let below: Vec<String> = store
            .change_sets
            .iter()
            .filter(|change_set| {
                change_set
                    .parent_change_set_id
                    .as_deref()
                    .is_some_and(|parent| hierarchy.contains(parent))
                    && !hierarchy.contains(&change_set.change_set_id)
            })
            .map(|change_set| change_set.change_set_id.clone())
            .collect();
        if below.is_empty() {
            break;
        }
        hierarchy.extend(below);
    }
    hierarchy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the children a template declares are the resources of the nested-stack type
    /// whose declaration addresses a template, each with the values it passes — and a
    /// declaration is read as the parameter values in force make it, not as its text stands.
    #[test]
    fn the_declared_children_are_the_nested_stacks_the_template_addresses() {
        let body = r#"{"Parameters":{"ChildTemplate":{"Type":"String"},
                "Assets":{"Type":"String","Default":"assets"}},
            "Resources":{
            "Child":{"Type":"AWS::CloudFormation::Stack","Properties":{
                "TemplateURL":"https://assets.s3.us-east-1.amazonaws.com/child.json",
                "Parameters":{"Size":"small","Count":3}}},
            "Chosen":{"Type":"AWS::CloudFormation::Stack","Properties":{
                "TemplateURL":{"Ref":"ChildTemplate"},
                "Parameters":{"Size":{"Ref":"Assets"}}}},
            "Built":{"Type":"AWS::CloudFormation::Stack","Properties":{
                "TemplateURL":{"Fn::Sub":
                    "https://${Assets}.s3.${AWS::Region}.amazonaws.com/built.json"}}},
            "Unknowable":{"Type":"AWS::CloudFormation::Stack","Properties":{
                "TemplateURL":{"Fn::GetAtt":["Bucket","WebsiteURL"]}}},
            "NoTemplate":{"Type":"AWS::CloudFormation::Stack","Properties":{}},
            "Bucket":{"Type":"AWS::S3::Bucket","Properties":{}}}}"#;
        let declared = super::super::declared_template(body).expect("a readable template");
        let scope = PlanningScope {
            region: "us-east-1",
            stack_name: "parent",
            stack_id: "",
            notification_arns: &[],
        };
        let values: BTreeMap<String, String> = [
            ("ChildTemplate", "https://assets.s3.us-east-1.amazonaws.com/v2.json"),
            ("Assets", "assets"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let children = declared_children(&declared, scope, &values);
        let addressed: Vec<(&str, &str)> = children
            .iter()
            .map(|(logical_id, url, _)| (logical_id.as_str(), url.as_str()))
            .collect();
        assert_eq!(
            addressed,
            vec![
                (
                    "Built",
                    "https://assets.s3.us-east-1.amazonaws.com/built.json"
                ),
                ("Child", "https://assets.s3.us-east-1.amazonaws.com/child.json"),
                ("Chosen", "https://assets.s3.us-east-1.amazonaws.com/v2.json"),
            ],
            "case \"a stated, a parameterised and a built URL among six resources\": \
             expected each addressed as the values in force make it, got {addressed:?}"
        );
        let passed: Vec<(&str, &str)> = children
            .iter()
            .find(|(logical_id, _, _)| logical_id == "Child")
            .expect("the stated child")
            .2
            .iter()
            .map(|parameter| (parameter.key.as_str(), parameter.value.as_str()))
            .collect();
        assert_eq!(
            passed,
            vec![("Count", "3"), ("Size", "small")],
            "case \"the values the declaration passes\": expected every member as text, got \
             {passed:?}"
        );
        let resolved: Vec<(&str, &str)> = children
            .iter()
            .find(|(logical_id, _, _)| logical_id == "Chosen")
            .expect("the parameterised child")
            .2
            .iter()
            .map(|parameter| (parameter.key.as_str(), parameter.value.as_str()))
            .collect();
        assert_eq!(
            resolved,
            vec![("Size", "assets")],
            "case \"a value the declaration passes as an expression\": expected the value in \
             force, got {resolved:?}"
        );
    }

    /// RULE: a nested change set's name is derived from the hierarchy — the parent's name and
    /// the path of the child — and stays within what `ChangeSetName` admits.
    #[test]
    fn a_nested_change_set_is_named_for_the_child_it_plans_for() {
        let rows: &[(&str, &str, &str, &str)] = &[
            ("a first-level child", "review", "Child", "review-Child"),
            (
                "a child below one",
                "review",
                "Child/Grandchild",
                "review-Child-Grandchild",
            ),
            (
                "a path cut to the documented maximum",
                "r",
                &"L".repeat(CHANGE_SET_NAME_MAX_LEN * 2),
                &format!("r-{}", "L".repeat(CHANGE_SET_NAME_MAX_LEN - 2)),
            ),
        ];
        for (case, parent, path, expected) in rows {
            let name = nested_change_set_name(parent, path);
            assert_eq!(
                name, *expected,
                "case {case:?}: expected {expected:?}, got {name:?}"
            );
            assert!(
                name.len() <= CHANGE_SET_NAME_MAX_LEN,
                "case {case:?}: expected a name within {CHANGE_SET_NAME_MAX_LEN} \
                 characters, got {name:?}"
            );
        }
    }
}
