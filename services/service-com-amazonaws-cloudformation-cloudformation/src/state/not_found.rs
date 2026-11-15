// SPDX-License-Identifier: CC0-1.0

//! The sentences this service refuses a request it cannot RESOLVE with: the "does not exist"
//! message each operation answers for a stack, a change set, a stack set, an extension, an
//! export or a resource it holds none of, beside the few refusals that name a template source
//! or an export in use.
//!
//! Split out of the module root so one file holds that wording and nothing else: each form is
//! an operation's own fixed template with the identifier the request named as its only variable
//! part, and keeping them together is what stops two operations wording the same class
//! differently.

use super::*;

/// The message CloudFormation answers when a request READS a stack that is not addressable:
/// no live stack has that name and no stack has that id.
///
/// ## One refusal per operation, not one sentence per service
///
/// CloudFormation does not word "that stack is not there" once. Each operation answers its
/// OWN fixed template for the class, and the only variable part of any of them is the
/// `StackName` the request named, spelled as the request spelled it — so these forms are
/// kept apart here, one function per template, and an operation is moved between them only
/// on evidence of what it really answers. The forms this service has evidence for:
///
/// * `Stack with id <name> does not exist` — this function, the one the reads of a stack's
///   own record and of its resources answer (`DescribeStacks`, `DescribeStackResources`,
///   `ListStackResources`, `GetTemplate`, `GetTemplateSummary`, `DescribeEvents`,
///   `CreateGeneratedTemplate`).
/// * `Stack [<name>] does not exist` — [`changed_stack_does_not_exist`], which is also
///   `DetectStackResourceDrift`'s, the whole change-set family's and the one the reads of a
///   stack's event and drift LOGS answer.
/// * `Stack '<name>' does not exist` — [`quoted_stack_does_not_exist`].
/// * `Stack <name> does not exist` — [`bare_stack_does_not_exist`].
/// * `Stack: <name> does not exist` — [`labelled_stack_does_not_exist`], which is also
///   `GetStackPolicy`'s and `ContinueUpdateRollback`'s.
pub fn stack_does_not_exist(name_or_id: &str) -> String {
    format!("Stack with id {name_or_id} does not exist")
}

/// The message CloudFormation answers for a stack that is not addressable with the named
/// stack in SQUARE BRACKETS, which is this family's template for `UpdateStack`,
/// `UpdateTerminationProtection`, `RollbackStack`, `DetectStackDrift`,
/// `DetectStackResourceDrift`, the operations that address a stack's CHANGE SETS
/// (`CreateChangeSet`, `ListChangeSets`, `DescribeChangeSet`, `DescribeChangeSetHooks`,
/// `ExecuteChangeSet`) and the reads of a stack's event and drift LOGS
/// (`DescribeStackEvents`, `DescribeStackResourceDrifts`) — see [`stack_does_not_exist`] for
/// the family, of which this is one template among several.
///
/// The bracketed name is the message's only variable part.
pub fn changed_stack_does_not_exist(name_or_id: &str) -> String {
    format!("Stack [{name_or_id}] does not exist")
}

/// The same refusal with the named stack in SINGLE QUOTES, which is this family's template
/// for `DescribeStackResource` ([`stack_does_not_exist`] documents the family).
pub fn quoted_stack_does_not_exist(name_or_id: &str) -> String {
    format!("Stack '{name_or_id}' does not exist")
}

/// The same refusal with the named stack BARE — no quoting, no bracketing — which is this
/// family's template for `SignalResource` ([`stack_does_not_exist`] documents the family).
pub fn bare_stack_does_not_exist(name_or_id: &str) -> String {
    format!("Stack {name_or_id} does not exist")
}

/// The same refusal with the named stack after a COLON, which is this family's template for
/// `SetStackPolicy`, `GetStackPolicy` and `ContinueUpdateRollback`
/// ([`stack_does_not_exist`] documents the family).
pub fn labelled_stack_does_not_exist(name_or_id: &str) -> String {
    format!("Stack: {name_or_id} does not exist")
}

/// The message answered when a stack IS addressable but carries no template: a stack a
/// `CREATE` change set created has none until that change set is executed, and "if the
/// template doesn't exist, a ValidationError is returned".
///
/// Built from what the request named and the status the stack is in, the way
/// [`stack_does_not_exist`] is built from the name — and worded once, so the operations that
/// read a stack's template cannot drift about it.
pub fn template_does_not_exist(name_or_id: &str, status: &str) -> String {
    format!("Template does not exist for stack [{name_or_id}] in {status} state")
}

/// The message answered when a STACK SET is addressable but carries no template.
///
/// The stack wording ([`template_does_not_exist`]) names a stack and interpolates a stack's
/// own status, neither of which holds for a request that named a `StackSetName`: a StackSet
/// carries a `StackSetStatus`, so the refusal names the StackSet the request addressed and
/// nothing else. Worded once, so every operation that reads a StackSet's template cannot
/// drift about it.
pub fn stack_set_template_does_not_exist(name_or_id: &str) -> String {
    format!("Template does not exist for StackSet [{name_or_id}]")
}

/// The `StackSetNotFoundException` message CloudFormation answers when a request names a
/// stack set this administrator account and Region does not hold — an unknown name, an
/// unknown id, or a stack set another Region holds.
///
/// Built from what the request named, the way [`stack_does_not_exist`] is, and within the
/// modelled `ErrorMessage` length (1..255) for any `StackSetName` a caller can send.
pub fn stack_set_does_not_exist(name_or_id: &str) -> String {
    format!("StackSet {name_or_id} not found")
}

/// The `ValidationError` message CloudFormation answers a request for a stack set's
/// AUTOMATIC-DEPLOYMENT targets with when the stack set it resolves is not service-managed.
///
/// Automatic deployment is deployment through Organizations, which is what the
/// service-managed permission model IS ("StackSets automatically creates the IAM roles
/// required to deploy to accounts managed by Organizations"), so a stack set under any other
/// permission model has no such targets to report at all — a different answer from a
/// service-managed stack set that has none YET. Entirely fixed text: no part of it is taken
/// from the request.
pub const AUTO_DEPLOYMENT_IS_SERVICE_MANAGED_ONLY: &str =
    "Auto-deployment targets are only available for service-managed stack sets";

/// The `OperationNotFoundException` message CloudFormation answers when a stack set holds no
/// operation under the `OperationId` a request named.
///
/// The text carries NO variable part, the way [`drift_operation_does_not_exist`] carries
/// none: the refusal names the KIND of record the request did not resolve, and the operation
/// id and the stack set name are the ones the caller already sent. Worded once, so the
/// operation-scoped stack set reads and the stop cannot drift about it.
pub fn stack_set_operation_does_not_exist() -> &'static str {
    "Operation not found"
}

/// The message CloudFormation answers when a request for a stack instance's RESOURCE
/// DRIFTS names an operation that is not a drift detection of that stack set.
///
/// `ListStackInstanceResourceDriftsInput$OperationId` is "the unique ID of the drift
/// operation", so what does not exist is a DRIFT operation under that id — an id the stack
/// set holds no operation for, and an id that names an operation of another action. The
/// text carries no variable part.
pub fn drift_operation_does_not_exist() -> &'static str {
    "Detect Drift Operation not found"
}

/// The `StackInstanceNotFoundException` message CloudFormation answers an operation that
/// READS one stack instance, addressed by the account and Region a request named, and finds
/// the stack set holds none.
///
/// The text carries no variable part, the way [`drift_operation_does_not_exist`] carries
/// none: the error names the KIND of record the request did not resolve, and the account and
/// Region the caller asked for are the ones it already sent. The wording belongs to the
/// reading operations only — an operation that UPDATES the instances a `Regions` list selects
/// words the same exception its own way, see [`no_stack_instance_in_region`].
pub fn stack_instance_does_not_exist() -> &'static str {
    "StackInstance not found"
}

/// The `StackInstanceNotFoundException` message CloudFormation answers an operation that
/// UPDATES the stack instances a request's `Regions` list selects when one of those Regions
/// holds no instance of the stack set.
///
/// An update is addressed at a SELECTION — a `Regions` list crossed with the accounts or
/// deployment targets — rather than at one instance, so the refusal names which Region of
/// that list resolved nothing: fixed text plus ONE variable part, the Region as the request
/// spelled it. A different sentence from [`stack_instance_does_not_exist`] because the
/// message belongs to the operation that refused, not to the exception shape alone.
pub fn no_stack_instance_in_region(region: &str) -> String {
    format!("No Stack Instance found in region {region}")
}

/// The refusal for a request that names NO source for the template it submits:
/// "Conditional: You must specify only one of the following parameters: TemplateBody,
/// TemplateURL, or set the UsePreviousTemplate to true."
///
/// A request carrying none of them states no template to act on, and the service words that
/// as the two members a template may be SUBMITTED on — the sentence the recorded refusals of
/// this condition carry. The ONE wording of it, shared by every operation whose input carries
/// the three members (`UpdateStack`, `CreateChangeSet`, `UpdateStackSet`), so the sentence
/// cannot drift between them. A request that reuses the stack's template AND submits one of
/// its own is refused for that COMBINATION ([`PREVIOUS_AND_SUBMITTED_TEMPLATE`]), and one
/// that submits two sources for [`BOTH_TEMPLATE_SOURCES`].
pub const ONE_TEMPLATE_SOURCE: &str = "Either Template URL or Template Body must be specified.";

/// The refusal for a request that asks to reuse the stack's existing template AND submits a
/// template of its own.
///
/// `UsePreviousTemplate` ("Reuse the existing template that is associated with the stack")
/// and a submitted `TemplateBody`/`TemplateURL` are two different templates to act on, so the
/// request states no one template — a distinct condition from naming no source at all, and
/// the service words it as its own sentence, naming the member in the lower case the service
/// spells it with there. The ONE wording of it, shared by every operation whose input carries
/// the three members, so neither the condition nor its wording is restated per operation.
pub const PREVIOUS_AND_SUBMITTED_TEMPLATE: &str =
    "You cannot specify both usePreviousTemplate and Template Body/Template URL.";

/// `Err(message)` for a request that does not state exactly ONE source for the template it
/// submits, where the input carries all three ways of stating one — the RULE every operation
/// whose input carries that trio is held to, in one place.
///
/// The three conditions are three distinct refusals, each worded for what the request really
/// states: a template submitted BOTH by value and by reference ([`BOTH_TEMPLATE_SOURCES`],
/// the rule a request carrying only that pair is held to), a request that reuses the stack's
/// template while submitting one of its own ([`PREVIOUS_AND_SUBMITTED_TEMPLATE`]), and one
/// that states no source at all ([`ONE_TEMPLATE_SOURCE`]).
pub fn validate_template_sources(
    template_body: Option<&str>,
    template_url: Option<&str>,
    use_previous_template: bool,
) -> Result<(), String> {
    validate_one_template_source(template_body, template_url)?;
    let submitted = template_body.is_some() || template_url.is_some();
    if use_previous_template && submitted {
        return Err(PREVIOUS_AND_SUBMITTED_TEMPLATE.to_string());
    }
    if !use_previous_template && !submitted {
        return Err(ONE_TEMPLATE_SOURCE.to_string());
    }
    Ok(())
}

/// The `StackStatusReason` of a deletion CloudFormation cancelled because it would have
/// removed an exported output value another stack still imports: the export, and the stack
/// that is still consuming it.
///
/// An export in use cannot be removed — deleting the stack that publishes it would break
/// the reference every importing stack resolved — so CloudFormation cancels the deletion
/// and the stack says so, naming both halves of the reference, taken from the stack being
/// deleted and from this store's index, never from any one recorded run.
pub fn export_in_use_delete_canceled_reason(
    export_name: &str,
    importing_stack_name: &str,
) -> String {
    exports::canceled_reason(
        exports::CanceledOperation::Delete,
        export_name,
        importing_stack_name,
    )
}

/// The message answered when a request names a `LogicalResourceId` the stack it selected
/// has not deployed: the stack is addressable, so what does not exist is the resource
/// under that logical id in that stack. Built from what the request named, the way
/// [`stack_does_not_exist`] is.
///
/// The template of the reads that report one resource of a stack (`DescribeStackResource`);
/// a DRIFT check of one resource words the same class its own way
/// ([`resource_does_not_exist_in_stack`]), the way the stack-not-found templates differ per
/// operation.
pub fn resource_does_not_exist(logical_resource_id: &str, stack_name: &str) -> String {
    format!("Resource {logical_resource_id} does not exist for stack {stack_name}")
}

/// The message `DetectStackResourceDrift` answers when the stack it resolved has not
/// deployed the `LogicalResourceId` the request named.
///
/// The fixed text is "does not exist in stack" and the two variable parts are the logical id
/// the request named and the stack's own ID — the stack as the SERVICE addresses it (its
/// ARN), not as the request spelled it, which is what a drift record itself reports a stack
/// by (`StackResourceDrift$StackId`).
pub fn resource_does_not_exist_in_stack(logical_resource_id: &str, stack_id: &str) -> String {
    format!("Resource {logical_resource_id} does not exist in stack {stack_id}")
}

/// The message answered when a request asks for the drift of a resource whose TYPE drift
/// detection is not supported for: "resources that don't currently support drift detection
/// can't be checked", so there is no comparison to answer with and the request is refused.
/// CloudFormation's own sentence for it, naming the `ResourceType` the named resource
/// carries.
pub fn drift_detection_unsupported(resource_type: &str) -> String {
    format!("Drift detection is not supported for ResourceType {resource_type}")
}

/// The message answered when a request names an extension the registry of this account and
/// Region does not hold — an unknown name or ARN, a name registered under another kind or
/// in another Region, a publisher that published no such extension, or a version the
/// record does not carry.
///
/// The sentence CloudFormation answers for `TypeNotFoundException`, with the EXTENSION the
/// request identified quoted in it: its type name, which is what an account-scoped registry
/// ARN carries ([`registry::arn_extension_name`]) as much as the name form does, so the two
/// ways of identifying one extension are refused identically. Anything else the request
/// named is quoted as the request spelled it; nothing else of the request appears in it.
pub fn extension_does_not_exist(identifier: &str) -> String {
    let named = registry::arn_extension_name(identifier)
        .unwrap_or_else(|| identifier.to_string());
    format!("The type '{named}' cannot be found.")
}

/// The message answered when a request names a `StackDriftDetectionId` this account has
/// not run a drift detection under — an id nothing issued, or one whose result this store
/// no longer retains ("the number of drift results CloudFormation retains for any given
/// stack, and for how long, may vary").
///
/// What the request did not resolve is the RESULT of a drift detection, which is what
/// `DescribeStackDriftDetectionStatus` reads, and the detection id the request named is the
/// message's only variable part — bracketed, as the stack-not-found template of the
/// operations that bracket theirs is ([`changed_stack_does_not_exist`]).
pub fn drift_detection_does_not_exist(detection_id: &str) -> String {
    format!("Stack drift detection result [{detection_id}] does not exist")
}

/// The `HookResultNotFoundException` message CloudFormation answers when a request names a
/// Hook invocation result this account and Region does not hold — an id no invocation was
/// recorded under, one another Region holds, or one this store no longer retains.
///
/// A FIXED sentence, unlike the messages above: this refusal states that the result was not
/// found and names nothing of the request, so there is no member to interpolate.
pub const HOOK_RESULT_NOT_FOUND: &str = "The hook result was not found.";

/// The message CloudFormation answers when a request names a `PhysicalResourceId` no
/// stack has deployed: such a request names its stack only through that resource, so what
/// is not addressable is the stack the resource would belong to.
pub fn stack_for_physical_resource_does_not_exist(physical_resource_id: &str) -> String {
    format!("Stack for {physical_resource_id} does not exist")
}

/// The message CloudFormation answers a read of the importers of an export name this account
/// and Region publishes NO export under — a name nothing ever exported, and one whose
/// exporting stack is gone.
///
/// The quoted name is the `ExportName` the request named and the rest is fixed text, the way
/// [`stack_does_not_exist`] is built from what the request named.
pub fn export_does_not_exist(export_name: &str) -> String {
    format!("Export '{export_name}' does not exist.")
}

/// The message CloudFormation answers a read of the importers of an export that EXISTS and
/// no stack imports.
///
/// A different refusal from [`export_does_not_exist`], because the two are what the
/// documented workflow asks apart: "to modify or remove an exported output value, first use
/// this action to see which stacks are using it" is answerable for an export that is there
/// and unused, and unanswerable for a name that is not there at all. The quoted name is the
/// `ExportName` the request named and the rest is fixed text.
pub fn export_is_not_imported(export_name: &str) -> String {
    format!("Export '{export_name}' is not imported by any stack.")
}

/// The reason CloudFormation reports a DEPLOYMENT it refused because the template imports
/// an export name nothing publishes in the account and Region it runs in.
///
/// A different sentence from the two reads above, because it answers a different question:
/// those refuse a request that NAMED the export, while this says why a stack operation
/// could not be carried out — the export the template's `Fn::ImportValue` names was not
/// found, so the declaration carrying it has no value. The name is the one the template
/// imported and the rest is fixed text; a create that is then rolled back reports it with
/// the rollback clause the service appends ([`rollback_requested`]).
pub fn no_export_named(export_name: &str) -> String {
    format!("No export named {export_name} found.")
}
