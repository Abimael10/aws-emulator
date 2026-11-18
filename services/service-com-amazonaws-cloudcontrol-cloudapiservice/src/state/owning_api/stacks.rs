// SPDX-License-Identifier: CC0-1.0

//! `AWS::CloudFormation::Stack`, as it lives in the CloudFormation service itself.
//!
//! A stack is not a record of this service: `cloudformation:CreateStack` creates it,
//! `cloudformation:DescribeStacks` reports it, `cloudformation:UpdateStack` changes it and
//! `cloudformation:DeleteStack` removes it — and while it is being created the resources its
//! template declares are created in the services that own THEM, its own reads report it, and
//! a `cloudformation:SignalResource` against a gate its template declares is what lets it
//! finish. None of that is anything this service could do to a record of its own, so every
//! answer here is a call on that service's OWN API, dispatched in process
//! ([`in_process_cloudformation`]): what a `CreateResource` of this type starts is a real
//! stack, and what the request's `OperationStatus` reports is that stack's own lifecycle
//! ([`progress`]).
//!
//! ## The resource model
//!
//! Catalog source: the `AWS::CloudFormation::Stack` resource provider schema's
//! `properties`, whose `primaryIdentifier` is `/properties/StackId` — the stack id
//! CloudFormation assigns, which is also the only reference its own reads resolve a DELETED
//! stack by — whose `createOnlyProperties` is `/properties/StackName`, and whose
//! `readOnlyProperties` are the members CloudFormation computes for a stack
//! ([`READ_ONLY_PROPERTIES`]). A property is carried in the form the schema declares it:
//! the stack's parameters as a MAP of name to value (CloudFormation's own API carries them
//! as a list of `Parameter` structures), its tags as a list of `{ Key, Value }` pairs, a
//! count as a number and a flag as a boolean.
//!
//! The template is a `writeOnlyProperties` member of this type: a stack's own read does not
//! report the document it was deployed from, so a model a client round-trips carries none —
//! which is what makes an update of this type an update that reuses the stack's own template
//! ([`update`]).
//!
//! ## What a refusal carries
//!
//! A call CloudFormation refused is reported as that service's own verdict
//! ([`refusal_of`]): the error code it named decides the `HandlerErrorCode` the resource
//! operation request fails with, and the message it worded is the diagnostic. Nothing here
//! re-words it.

use ::serde_json::{Map, Value};

use super::{
    answered_error, count, flag, tag_entries, text, text_list, Progress, Refusal, MAX_OWNING_PAGES,
    TAGS, TAG_KEY, TAG_VALUE,
};
use crate::state::native::HANDLER_ERROR_INVALID_REQUEST;

/// The `TypeName` this module stands behind.
pub const TYPE_NAME: &str = "AWS::CloudFormation::Stack";

/// The property that carries the type's primary identifier, which its schema's
/// `primaryIdentifier` points at (`/properties/StackId`): the stack id CloudFormation
/// assigns when it starts creating the stack.
pub const IDENTIFIER_PROPERTY: &str = "StackId";

/// The stack's name (`StackName`), which the schema declares create-only: it is what
/// CloudFormation's own calls name the stack by for as long as it is live, and it cannot
/// change while the stack stays the same stack.
const STACK_NAME: &str = "StackName";

/// The template the stack is deployed from, by value and by reference — both
/// `writeOnlyProperties` of this type, because a stack's read reports neither.
const TEMPLATE_BODY: &str = "TemplateBody";
const TEMPLATE_URL: &str = "TemplateURL";

/// The stack policy the stack carries, by value and by reference — write-only for the same
/// reason: a stack's read reports no policy document.
const STACK_POLICY_BODY: &str = "StackPolicyBody";
const STACK_POLICY_URL: &str = "StackPolicyURL";

/// "A list of Parameter structures that specify input parameters for the stack", as this
/// type's schema carries them: a map of parameter name to value.
const PARAMETERS: &str = "Parameters";

/// The capabilities the deployment is made with.
const CAPABILITIES: &str = "Capabilities";

/// "The Amazon SNS topic ARNs to publish stack related events."
const NOTIFICATION_ARNS: &str = "NotificationARNs";

/// "The amount of time that can pass before the stack status becomes CREATE_FAILED."
const TIMEOUT_IN_MINUTES: &str = "TimeoutInMinutes";

/// "Set to true to disable rollback of the stack if stack creation failed."
const DISABLE_ROLLBACK: &str = "DisableRollback";

/// "Whether to enable termination protection on the specified stack."
const ENABLE_TERMINATION_PROTECTION: &str = "EnableTerminationProtection";

/// "The Amazon Resource Name (ARN) of an IAM role that CloudFormation assumes to create the
/// stack."
const ROLE_ARN: &str = "RoleARN";

/// Whether `property` is one of this type's the OWNING service COMPUTES.
///
/// The schema's `readOnlyProperties` are held in this crate's one catalogue of them
/// ([`crate::state::read_only_properties`]), so this module restates none of them; that
/// catalogue leaves the primary identifier out, because a patch that would change it is
/// refused as the create-only property it is, and it is added back here — a desired state
/// may not ask for the identifier either.
///
/// Each is reported by a read and none may be asked for: a create whose desired state names
/// one asks for a value no handler sets, and an update's model carries the value the read
/// reported, which asks for nothing ([`DesiredStack::read`]).
fn read_only_properties() -> Vec<String> {
    let mut computed = crate::state::read_only_properties(TYPE_NAME, IDENTIFIER_PROPERTY);
    computed.push(IDENTIFIER_PROPERTY.to_string());
    computed
}

/// The members of one of the stack's outputs, as the schema's `Outputs` carries them.
const OUTPUT_KEY: &str = "OutputKey";
const OUTPUT_VALUE: &str = "OutputValue";
const OUTPUT_DESCRIPTION: &str = "Description";
const OUTPUT_EXPORT_NAME: &str = "ExportName";

/// The suffix every status CloudFormation reports while a stack operation is still running
/// ends with (`StackStatus`: `CREATE_IN_PROGRESS`, `UPDATE_IN_PROGRESS`,
/// `DELETE_IN_PROGRESS`, `ROLLBACK_IN_PROGRESS`, …).
///
/// What makes a status TERMINAL is that it does not end with it, which is the one rule
/// [`progress`] decides a request's own terminal status by — so a status this emulator's
/// CloudFormation reports and this module never names is still read as the stage it is.
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// The status a stack whose CREATE completed reports (`StackStatus$CREATE_COMPLETE`).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a stack whose UPDATE completed reports.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The status a stack whose DELETE completed reports — which is also what a stack that is
/// gone reports for as long as CloudFormation keeps it addressable by its id.
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The status a stack a change set created but nothing has deployed reports
/// (`REVIEW_IN_PROGRESS`), which is not a stage of this request's own operation.
const REVIEW_IN_PROGRESS: &str = "REVIEW_IN_PROGRESS";

/// The terminal status the stack reaches when `operation` — the `Operation` the resource
/// operation request reports — has been carried out.
fn completed_status(operation: &str) -> &'static str {
    match operation {
        crate::state::OPERATION_DELETE => DELETE_COMPLETE,
        crate::state::OPERATION_UPDATE => UPDATE_COMPLETE,
        _ => CREATE_COMPLETE,
    }
}

/// The stack `identifier` names in `region`, as CloudFormation's own read reports it, or
/// `None` when that service holds no stack under it.
///
/// A stack whose deletion has COMPLETED is addressable by its id — "information for stacks
/// that have been deleted is kept for 90 days" — but it is no longer a resource, so this
/// answers `None` for it: a read of it is a read of a resource that is not there, which is
/// what the not-found verdict is about. [`progress`] asks for the record itself
/// ([`described`]) because the deletion it reports is exactly that status.
pub async fn read(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Option<Value> {
    let stack = held(&in_process_cloudformation(ctx, region), identifier).await?;
    (status_of(&stack) != DELETE_COMPLETE).then(|| model(&stack))
}

/// The stacks CloudFormation holds in `region`, as `(primary identifier, resource model)`
/// in identifier order: at most `limit` of them, and only those whose identifier sorts
/// AFTER `after`.
///
/// "Deleted stacks don't show up in the DescribeStacks operation if the deletion has been
/// completed successfully", so what this enumerates is the stacks that EXIST — the same set
/// a read of one resolves.
///
/// A page CloudFormation did not answer ends the walk, so what a listing reports is the
/// window it could read of that service's own inventory rather than a page assembled from
/// anything else.
pub async fn list(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    after: Option<&str>,
    limit: usize,
) -> Vec<(String, Value)> {
    let mut held: Vec<(String, Value)> = Vec::new();
    let mut next: Option<String> = None;
    let client = in_process_cloudformation(ctx, region);
    // CloudFormation reports its stacks in its own order and one bounded page at a time,
    // while a resource listing of this service pages in IDENTIFIER order, so the pages are
    // walked and the window kept as they go: what is held is never more than the page this
    // listing is about to report, however many stacks the Region holds, and the walk itself
    // stops at [`MAX_OWNING_PAGES`].
    for _ in 0..MAX_OWNING_PAGES {
        let described = client
            .describe_stacks()
            .set_next_token(next.clone())
            .send()
            .await;
        let Ok(described) = described else {
            break;
        };
        for stack in described.stacks.unwrap_or_default() {
            if status_of(&stack) == DELETE_COMPLETE {
                continue;
            }
            let Some(identifier) = stack.stack_id.clone() else {
                continue;
            };
            if after.is_none_or(|after| identifier.as_str() > after) {
                held.push((identifier, model(&stack)));
            }
        }
        held.sort_by(|(left, _), (right, _)| left.cmp(right));
        held.truncate(limit);
        next = described.next_token;
        if next.is_none() {
            break;
        }
    }
    held
}

/// Create the stack the resource model `properties` describes, in `region`, and answer the
/// stack id CloudFormation assigned it — the primary identifier the resource then answers
/// to.
///
/// "After the call completes successfully, the stack creation starts": the call returns
/// while the stack is still `CREATE_IN_PROGRESS`, which is why the request that owes this
/// work goes on to follow that stack's own lifecycle ([`progress`]) rather than reporting a
/// create nothing has finished.
pub async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<String, Refusal> {
    let desired = DesiredStack::read(properties, None)?;
    // The stack's name is its identity for as long as it is live, and CloudFormation's own
    // create takes it as a required member, so a model that names none describes no stack
    // to create.
    let Some(stack_name) = desired.stack_name.clone() else {
        return Err(Refusal::invalid(format!(
            "{STACK_NAME} is required to create a resource of type {TYPE_NAME}"
        )));
    };
    let client = in_process_cloudformation(ctx, region);
    let mut request = client
        .create_stack()
        .stack_name(stack_name)
        .set_template_body(desired.template_body.clone())
        .set_template_url(desired.template_url.clone())
        .set_stack_policy_body(desired.stack_policy_body.clone())
        .set_stack_policy_url(desired.stack_policy_url.clone())
        .set_role_arn(desired.role_arn.clone())
        .set_timeout_in_minutes(desired.timeout_in_minutes)
        .set_disable_rollback(desired.disable_rollback)
        .set_enable_termination_protection(desired.enable_termination_protection);
    request = desired.submitted(request);
    request
        .send()
        .await
        .map_err(refusal_of)?
        .stack_id
        .ok_or_else(|| Refusal::rejected(NO_STACK_ID.to_string()))
}

/// What a create answers with when CloudFormation reported no stack id for the stack it
/// started: nothing names the resource, so the request fails rather than reporting a
/// success with no identifier.
const NO_STACK_ID: &str = "CloudFormation reported no StackId for the stack it created";

/// Apply the resource model `properties` to the stack `identifier` names in `region`.
///
/// The type's template properties are write-only, so the model a patch was applied to
/// carries no template at all: such an update is CloudFormation's own
/// `UsePreviousTemplate` update — the stack's own template with the parameters, tags,
/// capabilities, topics and role the model carries.
pub async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let client = in_process_cloudformation(ctx, region);
    let stack = held(&client, identifier)
        .await
        .filter(|stack| status_of(stack) != DELETE_COMPLETE)
        .ok_or(Refusal::NotFound)?;
    let desired = DesiredStack::read(properties, Some(&model(&stack)))?;
    let mut request = client
        .update_stack()
        .stack_name(identifier)
        .set_template_body(desired.template_body.clone())
        .set_template_url(desired.template_url.clone())
        .set_stack_policy_body(desired.stack_policy_body.clone())
        .set_stack_policy_url(desired.stack_policy_url.clone())
        .set_role_arn(desired.role_arn.clone());
    if desired.template_body.is_none() && desired.template_url.is_none() {
        request = request.use_previous_template(true);
    }
    for (key, value) in &desired.parameters {
        request = request.parameters(
            ::aws_sdk_cloudformation::types::Parameter::builder()
                .parameter_key(key)
                .parameter_value(value)
                .build(),
        );
    }
    for capability in &desired.capabilities {
        request = request.capabilities(::aws_sdk_cloudformation::types::Capability::from(
            capability.as_str(),
        ));
    }
    for arn in &desired.notification_arns {
        request = request.notification_arns(arn);
    }
    for (key, value) in &desired.tags {
        request = request.tags(
            ::aws_sdk_cloudformation::types::Tag::builder()
                .key(key)
                .value(value)
                .build(),
        );
    }
    request.send().await.map_err(refusal_of).map(|_| ())
}

/// Remove the stack `identifier` names in `region`.
///
/// CloudFormation answers a delete of a stack it no longer holds as a no-op, so its own
/// read is what decides whether there is a resource to remove: a delete that names none
/// fails with the owning handler's not-found verdict rather than reporting a removal
/// nothing performed.
pub async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<(), Refusal> {
    let client = in_process_cloudformation(ctx, region);
    // CloudFormation answers a delete of a stack it no longer holds as a no-op, so its own
    // read is what decides whether there is a resource to remove — and a read that could not
    // be made is that service's own error rather than a resource that is not there.
    match described(&client, identifier).await {
        Described::Held(stack) if status_of(&stack) != DELETE_COMPLETE => {}
        Described::Held(_) | Described::Absent => return Err(Refusal::NotFound),
        Described::Failed(refusal) => return Err(refusal),
    }
    client
        .delete_stack()
        .stack_name(identifier)
        .send()
        .await
        .map_err(refusal_of)
        .map(|_| ())
}

/// Where CloudFormation's own lifecycle for the stack `identifier` names has got to, for a
/// request whose `Operation` is `operation`.
///
/// The stack's status is what the request reports: while it is one of the `*_IN_PROGRESS`
/// statuses the operation is still running ([`IN_PROGRESS_SUFFIX`]), the terminal status
/// that operation asked for completes the request ([`completed_status`]), and any other
/// terminal status failed it — carrying the stack's own `StackStatusReason`, which is
/// CloudFormation's statement of what happened.
pub async fn progress(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
    operation: &str,
) -> Progress {
    let client = in_process_cloudformation(ctx, region);
    let stack = match described(&client, identifier).await {
        Described::Held(stack) => stack,
        // The stack the request was following is not there at all. For a delete that is the
        // removal itself; for anything else there is nothing left to report a resource model
        // of, which is the owning handler's not-found verdict.
        Described::Absent => {
            return match operation == crate::state::OPERATION_DELETE {
                true => Progress::Settled(None),
                false => Progress::Failed(Refusal::NotFound),
            };
        }
        // The READ did not succeed, which says nothing about the resource: the request goes
        // on running and the next call asks again, rather than reporting an outcome this
        // service never saw.
        Described::Failed(_) => return Progress::Running,
    };
    let status = status_of(&stack);
    if status.ends_with(IN_PROGRESS_SUFFIX) && status != REVIEW_IN_PROGRESS {
        return Progress::Running;
    }
    if status != completed_status(operation) {
        return Progress::Failed(Refusal::rejected(
            stack
                .stack_status_reason
                .clone()
                .unwrap_or_else(|| format!("CloudFormation reports the stack in {status}")),
        ));
    }
    // A delete leaves no resource to report a model of; a create and an update leave the
    // model the owning service now holds.
    match operation == crate::state::OPERATION_DELETE {
        true => Progress::Settled(None),
        false => Progress::Settled(Some(model(&stack))),
    }
}

/// What CloudFormation's own read answered about one stack.
///
/// "CloudFormation says it holds no such stack" and "the call did not succeed" are different
/// answers and are kept apart here: a read that could not be made reports no resource as
/// absent, and a request following a stack's lifecycle is never terminated on one.
enum Described {
    /// The stack as that read reports it — including one whose deletion has COMPLETED,
    /// which that read resolves by stack id.
    Held(Box<::aws_sdk_cloudformation::types::Stack>),
    /// That service holds no stack under this identifier: "if the stack doesn't exist, a
    /// ValidationError is returned".
    Absent,
    /// The call itself did not succeed, in that service's own terms.
    Failed(Refusal),
}

/// Ask CloudFormation's own read about the stack `identifier` names.
async fn described(client: &::aws_sdk_cloudformation::Client, identifier: &str) -> Described {
    let answered = client
        .describe_stacks()
        .stack_name(identifier)
        .send()
        .await;
    let described = match answered {
        Ok(described) => described,
        // "The name or the unique stack ID that's associated with the stack... If the stack
        // doesn't exist, a ValidationError is returned": that one answer is this service
        // holding no such stack, and every other failure is a call that did not succeed.
        Err(error) => {
            return match refusal_of(error) {
                Refusal::Refused { code, .. } if code == HANDLER_ERROR_INVALID_REQUEST => {
                    Described::Absent
                }
                refusal => Described::Failed(refusal),
            };
        }
    };
    match described
        .stacks
        .unwrap_or_default()
        .into_iter()
        .next()
    {
        Some(stack) => Described::Held(Box::new(stack)),
        None => Described::Absent,
    }
}

/// The stack `identifier` names where that read HOLDS one, and `None` where it holds none or
/// could not be made — for the paths that report a resource rather than settle a request.
async fn held(
    client: &::aws_sdk_cloudformation::Client,
    identifier: &str,
) -> Option<::aws_sdk_cloudformation::types::Stack> {
    match described(client, identifier).await {
        Described::Held(stack) => Some(*stack),
        Described::Absent | Described::Failed(_) => None,
    }
}

/// The `StackStatus` a stack reports, as the text the model's own enum declares it as.
fn status_of(stack: &::aws_sdk_cloudformation::types::Stack) -> &str {
    stack
        .stack_status
        .as_ref()
        .map_or("", ::aws_sdk_cloudformation::types::StackStatus::as_str)
}

/// The resource model one stack renders as: the schema's properties the stack carries, and
/// only those it HAS.
fn model(stack: &::aws_sdk_cloudformation::types::Stack) -> Value {
    let mut model = Map::new();
    for (property, carried) in [
        (IDENTIFIER_PROPERTY, &stack.stack_id),
        (STACK_NAME, &stack.stack_name),
        ("Description", &stack.description),
        ("ChangeSetId", &stack.change_set_id),
        ("ParentId", &stack.parent_id),
        ("RootId", &stack.root_id),
        (ROLE_ARN, &stack.role_arn),
        ("StackStatusReason", &stack.stack_status_reason),
    ] {
        if let Some(value) = carried {
            model.insert(property.to_string(), Value::from(value.clone()));
        }
    }
    let status = status_of(stack);
    if !status.is_empty() {
        model.insert("StackStatus".to_string(), Value::from(status.to_string()));
    }
    if let Some(created) = stack.creation_time.as_ref().and_then(timestamp) {
        model.insert("CreationTime".to_string(), Value::from(created));
    }
    if let Some(updated) = stack.last_updated_time.as_ref().and_then(timestamp) {
        model.insert("LastUpdateTime".to_string(), Value::from(updated));
    }
    if let Some(timeout) = stack.timeout_in_minutes {
        model.insert(TIMEOUT_IN_MINUTES.to_string(), Value::from(timeout));
    }
    if let Some(flag) = stack.disable_rollback {
        model.insert(DISABLE_ROLLBACK.to_string(), Value::from(flag));
    }
    if let Some(flag) = stack.enable_termination_protection {
        model.insert(
            ENABLE_TERMINATION_PROTECTION.to_string(),
            Value::from(flag),
        );
    }
    // "A list of Parameter structures", as this type's schema carries them: a map of name
    // to value, so a client reads one parameter by name rather than by position.
    let parameters: Map<String, Value> = stack
        .parameters
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|parameter| {
            let key = parameter.parameter_key?;
            Some((key, Value::from(parameter.parameter_value?)))
        })
        .collect();
    if !parameters.is_empty() {
        model.insert(PARAMETERS.to_string(), Value::Object(parameters));
    }
    let capabilities: Vec<Value> = stack
        .capabilities
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|capability| Value::from(capability.as_str().to_string()))
        .collect();
    if !capabilities.is_empty() {
        model.insert(CAPABILITIES.to_string(), Value::Array(capabilities));
    }
    let topics: Vec<Value> = stack
        .notification_arns
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(Value::from)
        .collect();
    if !topics.is_empty() {
        model.insert(NOTIFICATION_ARNS.to_string(), Value::Array(topics));
    }
    let tags: Vec<Value> = stack
        .tags
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tag| {
            let (key, value) = (tag.key?, tag.value?);
            Some(Value::Object(Map::from_iter([
                (TAG_KEY.to_string(), Value::from(key)),
                (TAG_VALUE.to_string(), Value::from(value)),
            ])))
        })
        .collect();
    if !tags.is_empty() {
        model.insert(TAGS.to_string(), Value::Array(tags));
    }
    let outputs: Vec<Value> = stack
        .outputs
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|output| {
            let mut rendered = Map::new();
            for (member, value) in [
                (OUTPUT_KEY, output.output_key),
                (OUTPUT_VALUE, output.output_value),
                (OUTPUT_DESCRIPTION, output.description),
                (OUTPUT_EXPORT_NAME, output.export_name),
            ] {
                if let Some(value) = value {
                    rendered.insert(member.to_string(), Value::from(value));
                }
            }
            Value::Object(rendered)
        })
        .collect();
    if !outputs.is_empty() {
        model.insert("Outputs".to_string(), Value::Array(outputs));
    }
    Value::Object(model)
}

/// One of the stack's instants as the resource model carries it: the ISO 8601 form every
/// timestamp of this emulator is written in.
fn timestamp(at: &::aws_smithy_types::DateTime) -> Option<String> {
    at.fmt(::aws_smithy_types::date_time::Format::DateTime).ok()
}

/// What a resource model asks a stack to be: the members CloudFormation's own create and
/// update take, each read off the model's properties.
#[derive(Debug, Default)]
struct DesiredStack {
    stack_name: Option<String>,
    template_body: Option<String>,
    template_url: Option<String>,
    stack_policy_body: Option<String>,
    stack_policy_url: Option<String>,
    role_arn: Option<String>,
    timeout_in_minutes: Option<i32>,
    disable_rollback: Option<bool>,
    enable_termination_protection: Option<bool>,
    parameters: Vec<(String, String)>,
    capabilities: Vec<String>,
    notification_arns: Vec<String>,
    tags: Vec<(String, String)>,
}

impl DesiredStack {
    /// Read `properties` as the model of a stack — or state why CloudFormation would not
    /// take it.
    ///
    /// `held` is the model the stack ALREADY reports, for a model applied to a stack that
    /// exists, and `None` for one that describes a stack to create. The type's read-only
    /// properties are the owning service's to compute: a model may carry one — a read
    /// reports it, so a model a client round-trips carries it — but only with the value the
    /// stack already holds, and a create, where there is no stack to hold one, may carry
    /// none at all.
    fn read(properties: &Value, held: Option<&Value>) -> Result<Self, Refusal> {
        let Some(model) = properties.as_object() else {
            return Err(Refusal::invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self::default();
        let computed = read_only_properties();
        for (property, value) in model {
            if computed.contains(property) {
                if held.and_then(|held| held.get(property)) != Some(value) {
                    return Err(Refusal::invalid(format!(
                        "{property} is a read-only property of {TYPE_NAME}"
                    )));
                }
                continue;
            }
            match property.as_str() {
                STACK_NAME => desired.stack_name = Some(text(property, value)?),
                TEMPLATE_BODY => desired.template_body = Some(text(property, value)?),
                TEMPLATE_URL => desired.template_url = Some(text(property, value)?),
                STACK_POLICY_BODY => desired.stack_policy_body = Some(text(property, value)?),
                STACK_POLICY_URL => desired.stack_policy_url = Some(text(property, value)?),
                ROLE_ARN => desired.role_arn = Some(text(property, value)?),
                TIMEOUT_IN_MINUTES => desired.timeout_in_minutes = Some(count(property, value)?),
                DISABLE_ROLLBACK => desired.disable_rollback = Some(flag(property, value)?),
                ENABLE_TERMINATION_PROTECTION => {
                    desired.enable_termination_protection = Some(flag(property, value)?);
                }
                PARAMETERS => desired.parameters = parameter_map(property, value)?,
                CAPABILITIES => desired.capabilities = text_list(property, value)?,
                NOTIFICATION_ARNS => desired.notification_arns = text_list(property, value)?,
                TAGS => desired.tags = tag_entries(value)?,
                other => {
                    return Err(Refusal::invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        Ok(desired)
    }

    /// Put the list-valued members of this model on a create: the parameters, the
    /// capabilities, the topics and the tags, each as CloudFormation's own create takes
    /// them.
    fn submitted(
        &self,
        request: ::aws_sdk_cloudformation::operation::create_stack::builders::CreateStackFluentBuilder,
    ) -> ::aws_sdk_cloudformation::operation::create_stack::builders::CreateStackFluentBuilder {
        let mut request = request;
        for (key, value) in &self.parameters {
            request = request.parameters(
                ::aws_sdk_cloudformation::types::Parameter::builder()
                    .parameter_key(key)
                    .parameter_value(value)
                    .build(),
            );
        }
        for capability in &self.capabilities {
            request = request.capabilities(::aws_sdk_cloudformation::types::Capability::from(
                capability.as_str(),
            ));
        }
        for arn in &self.notification_arns {
            request = request.notification_arns(arn);
        }
        for (key, value) in &self.tags {
            request = request.tags(
                ::aws_sdk_cloudformation::types::Tag::builder()
                    .key(key)
                    .value(value)
                    .build(),
            );
        }
        request
    }
}

/// The parameters `value` carries, or why it is not the map `property` takes.
///
/// This type's schema carries the stack's parameters as a map of name to value, so that is
/// the form a model states them in; CloudFormation's own calls take the same pairs as a
/// list of `Parameter` structures.
fn parameter_map(property: &str, value: &Value) -> Result<Vec<(String, String)>, Refusal> {
    let Some(entries) = value.as_object() else {
        return Err(Refusal::invalid(format!(
            "{property} must be a map of parameter names to values"
        )));
    };
    entries
        .iter()
        .map(|(name, value)| Ok((name.clone(), text(name, value)?)))
        .collect()
}

/// A call CloudFormation refused, as the refusal this service reports: the error code that
/// service NAMED decides which handler verdict the resource operation request fails with,
/// and the message it worded is the diagnostic, passed on rather than re-worded.
fn refusal_of<E>(
    error: ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> Refusal
where
    ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::fmt::Display,
{
    let (code, detail) = answered_error(error);
    match code.as_str() {
        // "The specified stack already exists" / "a stack with the same name already
        // exists": the resource is already there, which is the create handler's own
        // already-exists verdict.
        "AlreadyExistsException" => Refusal::already_exists(detail),
        // "The quota for the resource has already been reached."
        "LimitExceededException" => Refusal::at_capacity(detail),
        // CloudFormation's own answer to a request it will not take at all, which for this
        // type is a fault in the resource model the request carried.
        "ValidationError" => Refusal::invalid(detail),
        _ => Refusal::rejected(detail),
    }
}

/// A CloudFormation client whose transport is the framework's IN-PROCESS callback into this
/// same emulator — the one way this crate reaches the service that owns this type.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches to the service mounted beside this one INSIDE this process. No socket is
/// opened, no host name is resolved, nothing outside this process is reachable through it,
/// and nothing a request carries can steer it elsewhere — the endpoint URL exists because
/// the SDK requires one, and the only part of it a caller contributes is the Region token
/// its own credential scope named, which `crate::state::request_region` has already
/// narrowed to the characters a Region name is made of. The credentials are the emulator's
/// own; the caller's `Authorization` header is never forwarded.
fn in_process_cloudformation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudformation::Client {
    let config = ::aws_sdk_cloudformation::Config::builder()
        .behavior_version(::aws_sdk_cloudformation::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudformation::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://cloudformation.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        // The transport is in-process: nothing to retry, time out or watch for a stalled
        // stream, and each would need a sleep implementation this client is not given.
        .retry_config(::aws_sdk_cloudformation::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudformation::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudformation::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudformation::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudformation::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type's read-only properties are the ones no desired state may ask for, and the
    /// primary identifier is one of them: a create that names any of them describes a stack
    /// CloudFormation would not create.
    #[test]
    fn a_create_may_not_name_a_read_only_property() {
        let computed = read_only_properties();
        assert!(
            computed.contains(&IDENTIFIER_PROPERTY.to_string()),
            "expected the type's primary identifier to be one the owning service computes, \
             got {computed:?}"
        );
        for property in &computed {
            let model = ::serde_json::json!({ "StackName": "a-stack", property: "anything" });
            let refusal = DesiredStack::read(&model, None).err();
            assert_eq!(
                refusal,
                Some(Refusal::invalid(format!(
                    "{property} is a read-only property of {TYPE_NAME}"
                ))),
                "property {property:?}: expected a create naming it to be refused, got \
                 {refusal:?}"
            );
        }
    }

    /// ...and a model a client round-trips carries them with the values the stack reports,
    /// which asks for nothing: such a model is taken, and what it asks of CloudFormation is
    /// the writable members it carries.
    #[test]
    fn an_update_takes_the_read_only_values_the_read_reported() {
        let held = ::serde_json::json!({
            "StackId": "arn:aws:cloudformation:us-east-1:000000000000:stack/a-stack/0001",
            "StackName": "a-stack",
            "StackStatus": "CREATE_COMPLETE",
            "Parameters": { "Size": "small" },
        });
        let mut desired = held.clone();
        desired["Parameters"]["Size"] = ::serde_json::json!("large");
        let read = DesiredStack::read(&desired, Some(&held))
            .expect("a model carrying the values the read reported");
        assert_eq!(
            read.parameters,
            vec![("Size".to_string(), "large".to_string())],
            "expected the patched parameter to be what the update asks for, got {read:?}"
        );
        assert_eq!(
            read.stack_name.as_deref(),
            Some("a-stack"),
            "expected the stack's own name to travel with the update, got {read:?}"
        );
    }

    /// A property the type's schema does not declare asks for something no handler applies,
    /// which is refused rather than accepted and dropped.
    #[test]
    fn a_property_the_type_does_not_declare_is_refused() {
        let model = ::serde_json::json!({ "StackName": "a-stack", "Rubbish": true });
        let refusal = DesiredStack::read(&model, None).err();
        assert_eq!(
            refusal,
            Some(Refusal::invalid(format!(
                "Rubbish is not a property of {TYPE_NAME}"
            ))),
            "expected an undeclared property to be refused, got {refusal:?}"
        );
    }

    /// The terminal status a request's own operation asks for is the one that completes it,
    /// and every `*_IN_PROGRESS` status is a stage of it.
    #[test]
    fn each_operation_asks_for_its_own_terminal_status() {
        for (operation, expected) in [
            (crate::state::OPERATION_CREATE, CREATE_COMPLETE),
            (crate::state::OPERATION_UPDATE, UPDATE_COMPLETE),
            (crate::state::OPERATION_DELETE, DELETE_COMPLETE),
        ] {
            let reached = completed_status(operation);
            assert_eq!(
                reached, expected,
                "operation {operation:?}: expected it to complete at {expected:?}, got \
                 {reached:?}"
            );
            assert!(
                !expected.ends_with(IN_PROGRESS_SUFFIX),
                "operation {operation:?}: {expected:?} must be a terminal status"
            );
        }
    }
}
