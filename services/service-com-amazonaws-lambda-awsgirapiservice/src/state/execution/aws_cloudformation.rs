// SPDX-License-Identifier: CC0-1.0

//! The AWS CloudFormation half of the SDK seam a RUNNING handler calls another service
//! through: the STACK a function reads — its template, its resources, its events and the
//! stacks the account holds — and the stack operations a function drives.
//!
//! READING THE STACK IT WAS DEPLOYED BY is the shape most functions that reach this service
//! take: a handler declared in a template is handed its own stack's name in an environment
//! variable and then asks CloudFormation what that stack holds — the template it was
//! deployed from (`GetTemplate`), the resources it deployed and the physical ids they were
//! given (`ListStackResources`, `DescribeStackResources`, `DescribeStackResource`), the
//! stack's own status, parameters and outputs (`DescribeStacks`, `ListStacks`), the events it
//! progressed through (`DescribeStackEvents`) and the values other stacks export
//! (`ListExports`). DRIVING A STACK is the other shape, and the one a deployment handler or a
//! custom resource's provider takes: creating, updating and deleting a stack, signalling a
//! resource that waits on work the handler did, and validating a template before it is used.
//! A handler that cannot reach this service can do none of it.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation of
//! every one of them ([`super::aws_api`]). This service does not: its model declares
//! `awsQuery` (`smithy_query(query="service_info")`), so a request to it is a form-encoded
//! `Action` with the input flattened into the parameters beside it, and the answer is an XML
//! document. That binding is not written out here: the request is built, signed, dispatched
//! and parsed by the CLIENT this crate holds (`aws_sdk_cloudformation`), over the same
//! in-process callback transport every other call of this seam uses, so the wire details are
//! the client's and the answer is the emulated CloudFormation service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own transport
//!   and the service's own regional endpoint, so no host name is resolved and no socket is
//!   opened.
//! * The call is signed as the identity the client was built with — the session of the
//!   function's execution role, or the credentials the handler passed that client
//!   ([`super::aws_api::AwsAccess::signing_as`]) — so CloudFormation judges it against the
//!   policies in force on that identity exactly as it judges any other signed request.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what CloudFormation answered: a success carries
//!   the output members this module reads off it, and a refusal is that service's own error
//!   code, message and status, which the SDK the guest calls raises as `ClientError`
//!   ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_cloudformation::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_flag, insert_number, insert_structures, insert_text, insert_timestamp, some_list, Input,
    Observed, PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "AWS CloudFormation";

/// The operations of AWS CloudFormation this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the surface a handler that reads
/// the stack it belongs to uses — the stack's own record, its resources one summary or one
/// detail at a time, the events it progressed through, the template it was deployed from, the
/// stacks the account holds and the values they export — and the surface a handler that
/// DRIVES a stack uses: creating, updating and deleting one, signalling a resource that waits
/// on the handler's own work, and validating a template. A handler naming any other operation
/// of the service is told the call was not made and why, because a request this module cannot
/// bind is one it would otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "DescribeStacks",
    "DescribeStackEvents",
    "DescribeStackResource",
    "DescribeStackResources",
    "ListStackResources",
    "ListStacks",
    "ListExports",
    "GetTemplate",
    "ValidateTemplate",
    "CreateStack",
    "UpdateStack",
    "DeleteStack",
    "SignalResource",
];

/// Make ONE CloudFormation call: `operation`, with `body` as the operation's input document
/// (the keyword arguments the handler passed its client method), in `region`.
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, region, observed.clone(), chain);
    match operation {
        "DescribeStacks" => describe_stacks(&client, &input, &observed).await,
        "DescribeStackEvents" => describe_stack_events(&client, &input, &observed).await,
        "DescribeStackResource" => describe_stack_resource(&client, &input, &observed).await,
        "DescribeStackResources" => describe_stack_resources(&client, &input, &observed).await,
        "ListStackResources" => list_stack_resources(&client, &input, &observed).await,
        "ListStacks" => list_stacks(&client, &input, &observed).await,
        "ListExports" => list_exports(&client, &input, &observed).await,
        "GetTemplate" => get_template(&client, &input, &observed).await,
        "ValidateTemplate" => validate_template(&client, &input, &observed).await,
        "CreateStack" => create_stack(&client, &input, &observed).await,
        "UpdateStack" => update_stack(&client, &input, &observed).await,
        "DeleteStack" => delete_stack(&client, &input, &observed).await,
        "SignalResource" => signal_resource(&client, &input, &observed).await,
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// A CloudFormation client over the in-process callback into this same emulator, signing as
/// the identity the SDK client the guest built was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot have —
/// retries, timeouts, identity caching and stalled-stream protection, each of which needs a
/// sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_cloudformation::Config::builder()
        .behavior_version(::aws_sdk_cloudformation::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudformation::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://cloudformation.{region}.amazonaws.com"))
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_cloudformation::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudformation::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudformation::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudformation::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => {
            config.credentials_provider(::aws_sdk_cloudformation::config::Credentials::new(
                identity.access_key_id.clone(),
                identity.secret_access_key.clone(),
                identity.token().map(str::to_string),
                None,
                PROVIDER_NAME,
            ))
        }
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `DescribeStacks`: the stack the call names, or every stack the account holds in this Region.
async fn describe_stacks(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "NextToken"])?;
    let described = client
        .describe_stacks()
        .set_stack_name(input.text("StackName")?)
        .set_next_token(input.text("NextToken")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(&mut document, "Stacks", described.stacks(), stack_document);
    insert_text(&mut document, "NextToken", described.next_token());
    observed.answer(document)
}

/// `DescribeStackEvents`: the events one stack's operations progressed through, newest first.
async fn describe_stack_events(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "NextToken"])?;
    let described = client
        .describe_stack_events()
        .set_stack_name(input.text("StackName")?)
        .set_next_token(input.text("NextToken")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "StackEvents",
        described.stack_events(),
        stack_event_document,
    );
    insert_text(&mut document, "NextToken", described.next_token());
    observed.answer(document)
}

/// `DescribeStackResource`: ONE resource of one stack, by the logical id its template gave it.
async fn describe_stack_resource(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "LogicalResourceId"])?;
    let described = client
        .describe_stack_resource()
        .stack_name(input.required_text("StackName")?)
        .logical_resource_id(input.required_text("LogicalResourceId")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(detail) = described.stack_resource_detail() {
        document.insert(
            "StackResourceDetail".to_string(),
            stack_resource_detail_document(detail),
        );
    }
    observed.answer(document)
}

/// `DescribeStackResources`: the resources of the stack the call names, or the one stack a
/// physical id belongs to.
async fn describe_stack_resources(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "LogicalResourceId", "PhysicalResourceId"])?;
    let described = client
        .describe_stack_resources()
        .set_stack_name(input.text("StackName")?)
        .set_logical_resource_id(input.text("LogicalResourceId")?)
        .set_physical_resource_id(input.text("PhysicalResourceId")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "StackResources",
        described.stack_resources(),
        stack_resource_document,
    );
    observed.answer(document)
}

/// `ListStackResources`: one stack's resources, one bounded page at a time.
async fn list_stack_resources(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "NextToken"])?;
    let listed = client
        .list_stack_resources()
        .stack_name(input.required_text("StackName")?)
        .set_next_token(input.text("NextToken")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "StackResourceSummaries",
        listed.stack_resource_summaries(),
        stack_resource_summary_document,
    );
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `ListStacks`: the account's stacks in this Region, one summary each, narrowed to the
/// statuses the call names.
async fn list_stacks(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["NextToken", "StackStatusFilter"])?;
    let listed = client
        .list_stacks()
        .set_next_token(input.text("NextToken")?)
        .set_stack_status_filter(some_list(
            input
                .text_list("StackStatusFilter")?
                .into_iter()
                .map(|status| {
                    ::aws_sdk_cloudformation::types::StackStatus::from(status.as_str())
                })
                .collect(),
        ))
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "StackSummaries",
        listed.stack_summaries(),
        stack_summary_document,
    );
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `ListExports`: the values the account's stacks export, which another stack imports by name.
async fn list_exports(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["NextToken"])?;
    let listed = client
        .list_exports()
        .set_next_token(input.text("NextToken")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(&mut document, "Exports", listed.exports(), export_document);
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `GetTemplate`: the template body a stack was deployed from, at the stage the call asks for.
async fn get_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "ChangeSetName", "TemplateStage"])?;
    let read = client
        .get_template()
        .set_stack_name(input.text("StackName")?)
        .set_change_set_name(input.text("ChangeSetName")?)
        .set_template_stage(
            input
                .text("TemplateStage")?
                .map(|stage| {
                    ::aws_sdk_cloudformation::types::TemplateStage::from(stage.as_str())
                }),
        )
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "TemplateBody", read.template_body());
    insert_list(
        &mut document,
        "StagesAvailable",
        read.stages_available()
            .iter()
            .map(|stage| stage.as_str().to_string())
            .collect(),
    );
    observed.answer(document)
}

/// `ValidateTemplate`: what a template declares, as the service reads it.
async fn validate_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["TemplateBody", "TemplateURL"])?;
    let validated = client
        .validate_template()
        .set_template_body(input.text("TemplateBody")?)
        .set_template_url(input.text("TemplateURL")?)
        .send()
        .await;
    let validated = match validated {
        Ok(validated) => validated,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "Parameters",
        validated.parameters(),
        template_parameter_document,
    );
    insert_text(&mut document, "Description", validated.description());
    insert_list(
        &mut document,
        "Capabilities",
        validated
            .capabilities()
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect(),
    );
    insert_text(
        &mut document,
        "CapabilitiesReason",
        validated.capabilities_reason(),
    );
    insert_list(
        &mut document,
        "DeclaredTransforms",
        validated.declared_transforms().to_vec(),
    );
    observed.answer(document)
}

/// `CreateStack`: a stack of the handler's own, created from the template it passed.
async fn create_stack(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "StackName",
        "TemplateBody",
        "TemplateURL",
        "Parameters",
        "DisableRollback",
        "RollbackConfiguration",
        "TimeoutInMinutes",
        "NotificationARNs",
        "Capabilities",
        "ResourceTypes",
        "RoleARN",
        "OnFailure",
        "StackPolicyBody",
        "StackPolicyURL",
        "Tags",
        "ClientRequestToken",
        "EnableTerminationProtection",
        "RetainExceptOnCreate",
    ])?;
    let created = client
        .create_stack()
        .stack_name(input.required_text("StackName")?)
        .set_template_body(input.text("TemplateBody")?)
        .set_template_url(input.text("TemplateURL")?)
        .set_parameters(some_list(parameters(input)?))
        .set_disable_rollback(input.boolean("DisableRollback")?)
        .set_rollback_configuration(rollback_configuration(input)?)
        .set_timeout_in_minutes(input.integer("TimeoutInMinutes")?)
        .set_notification_arns(some_list(input.text_list("NotificationARNs")?))
        .set_capabilities(some_list(capabilities(input)?))
        .set_resource_types(some_list(input.text_list("ResourceTypes")?))
        .set_role_arn(input.text("RoleARN")?)
        .set_on_failure(input.text("OnFailure")?.map(|chosen| {
            ::aws_sdk_cloudformation::types::OnFailure::from(chosen.as_str())
        }))
        .set_stack_policy_body(input.text("StackPolicyBody")?)
        .set_stack_policy_url(input.text("StackPolicyURL")?)
        .set_tags(some_list(tags(input)?))
        .set_client_request_token(input.text("ClientRequestToken")?)
        .set_enable_termination_protection(input.boolean("EnableTerminationProtection")?)
        .set_retain_except_on_create(input.boolean("RetainExceptOnCreate")?)
        .send()
        .await;
    let created = match created {
        Ok(created) => created,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "StackId", created.stack_id());
    observed.answer(document)
}

/// `UpdateStack`: the same stack, deployed again from the template or the parameters the
/// handler passed.
async fn update_stack(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "StackName",
        "TemplateBody",
        "TemplateURL",
        "UsePreviousTemplate",
        "StackPolicyDuringUpdateBody",
        "StackPolicyDuringUpdateURL",
        "Parameters",
        "Capabilities",
        "ResourceTypes",
        "RoleARN",
        "RollbackConfiguration",
        "StackPolicyBody",
        "StackPolicyURL",
        "NotificationARNs",
        "Tags",
        "DisableRollback",
        "ClientRequestToken",
        "RetainExceptOnCreate",
    ])?;
    let updated = client
        .update_stack()
        .stack_name(input.required_text("StackName")?)
        .set_template_body(input.text("TemplateBody")?)
        .set_template_url(input.text("TemplateURL")?)
        .set_use_previous_template(input.boolean("UsePreviousTemplate")?)
        .set_stack_policy_during_update_body(input.text("StackPolicyDuringUpdateBody")?)
        .set_stack_policy_during_update_url(input.text("StackPolicyDuringUpdateURL")?)
        .set_parameters(some_list(parameters(input)?))
        .set_capabilities(some_list(capabilities(input)?))
        .set_resource_types(some_list(input.text_list("ResourceTypes")?))
        .set_role_arn(input.text("RoleARN")?)
        .set_rollback_configuration(rollback_configuration(input)?)
        .set_stack_policy_body(input.text("StackPolicyBody")?)
        .set_stack_policy_url(input.text("StackPolicyURL")?)
        .set_notification_arns(some_list(input.text_list("NotificationARNs")?))
        .set_tags(some_list(tags(input)?))
        .set_disable_rollback(input.boolean("DisableRollback")?)
        .set_client_request_token(input.text("ClientRequestToken")?)
        .set_retain_except_on_create(input.boolean("RetainExceptOnCreate")?)
        .send()
        .await;
    let updated = match updated {
        Ok(updated) => updated,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "StackId", updated.stack_id());
    observed.answer(document)
}

/// `DeleteStack`: the stack stood down, with the resources the call asks to retain left in
/// place.
///
/// The operation answers no output members at all, so a served call reports the empty document
/// the SDK gives a handler for one.
async fn delete_stack(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "StackName",
        "RetainResources",
        "RoleARN",
        "ClientRequestToken",
        "DeletionMode",
    ])?;
    let deleted = client
        .delete_stack()
        .stack_name(input.required_text("StackName")?)
        .set_retain_resources(some_list(input.text_list("RetainResources")?))
        .set_role_arn(input.text("RoleARN")?)
        .set_client_request_token(input.text("ClientRequestToken")?)
        .set_deletion_mode(input.text("DeletionMode")?.map(|mode| {
            ::aws_sdk_cloudformation::types::DeletionMode::from(mode.as_str())
        }))
        .send()
        .await;
    match deleted {
        Ok(_) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `SignalResource`: the signal a resource waiting on the handler's own work is given, which
/// is how a function reports that work to the stack that is waiting for it.
///
/// Answers no output members either, for the same reason as the delete above.
async fn signal_resource(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["StackName", "LogicalResourceId", "UniqueId", "Status"])?;
    let signalled = client
        .signal_resource()
        .stack_name(input.required_text("StackName")?)
        .logical_resource_id(input.required_text("LogicalResourceId")?)
        .unique_id(input.required_text("UniqueId")?)
        .status(::aws_sdk_cloudformation::types::ResourceSignalStatus::from(
            input.required_text("Status")?.as_str(),
        ))
        .send()
        .await;
    match signalled {
        Ok(_) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// The `Parameters` of a call, read as the parameter values the service takes.
fn parameters(
    input: &Input,
) -> Result<Vec<::aws_sdk_cloudformation::types::Parameter>, Refusal> {
    let mut held = Vec::new();
    for parameter in input.structures("Parameters")? {
        parameter.only(&[
            "ParameterKey",
            "ParameterValue",
            "UsePreviousValue",
            "ResolvedValue",
        ])?;
        held.push(
            ::aws_sdk_cloudformation::types::Parameter::builder()
                .set_parameter_key(parameter.text("ParameterKey")?)
                .set_parameter_value(parameter.text("ParameterValue")?)
                .set_use_previous_value(parameter.boolean("UsePreviousValue")?)
                .set_resolved_value(parameter.text("ResolvedValue")?)
                .build(),
        );
    }
    Ok(held)
}

/// The `Tags` of a call, read as the key/value pairs the service takes.
fn tags(input: &Input) -> Result<Vec<::aws_sdk_cloudformation::types::Tag>, Refusal> {
    let mut held = Vec::new();
    for tag in input.structures("Tags")? {
        tag.only(&["Key", "Value"])?;
        held.push(
            ::aws_sdk_cloudformation::types::Tag::builder()
                .key(tag.required_text("Key")?)
                .value(tag.required_text("Value")?)
                .build(),
        );
    }
    Ok(held)
}

/// The `Capabilities` of a call, read as the capability values the service takes.
fn capabilities(
    input: &Input,
) -> Result<Vec<::aws_sdk_cloudformation::types::Capability>, Refusal> {
    Ok(input
        .text_list("Capabilities")?
        .into_iter()
        .map(|capability| {
            ::aws_sdk_cloudformation::types::Capability::from(capability.as_str())
        })
        .collect())
}

/// The `RollbackConfiguration` of a call: the alarms CloudFormation watches during the
/// operation and how long it watches them for afterwards; `None` where the call carries none.
fn rollback_configuration(
    input: &Input,
) -> Result<Option<::aws_sdk_cloudformation::types::RollbackConfiguration>, Refusal> {
    let Some(configured) = input.structure("RollbackConfiguration")? else {
        return Ok(None);
    };
    configured.only(&["RollbackTriggers", "MonitoringTimeInMinutes"])?;
    let mut triggers = Vec::new();
    for trigger in configured.structures("RollbackTriggers")? {
        trigger.only(&["Arn", "Type"])?;
        triggers.push(
            ::aws_sdk_cloudformation::types::RollbackTrigger::builder()
                .arn(trigger.required_text("Arn")?)
                .r#type(trigger.required_text("Type")?)
                .build(),
        );
    }
    Ok(Some(
        ::aws_sdk_cloudformation::types::RollbackConfiguration::builder()
            .set_rollback_triggers(some_list(triggers))
            .set_monitoring_time_in_minutes(configured.integer("MonitoringTimeInMinutes")?)
            .build(),
    ))
}

/// Put a list-of-text member on an answer's document, where the answer carried entries.
fn insert_list(document: &mut Map<String, Value>, name: &str, values: Vec<String>) {
    if values.is_empty() {
        return;
    }
    document.insert(
        name.to_string(),
        Value::Array(values.into_iter().map(Value::String).collect()),
    );
}

/// One STACK as the answer reported it: the members the `Stack` shape names, each read off
/// that answer.
fn stack_document(stack: &::aws_sdk_cloudformation::types::Stack) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "StackId", stack.stack_id());
    insert_text(&mut held, "StackName", stack.stack_name());
    insert_text(&mut held, "ChangeSetId", stack.change_set_id());
    insert_text(&mut held, "Description", stack.description());
    insert_structures(
        &mut held,
        "Parameters",
        stack.parameters(),
        parameter_document,
    );
    insert_timestamp(&mut held, "CreationTime", stack.creation_time());
    insert_timestamp(&mut held, "DeletionTime", stack.deletion_time());
    insert_timestamp(&mut held, "LastUpdatedTime", stack.last_updated_time());
    if let Some(configured) = stack.rollback_configuration() {
        held.insert(
            "RollbackConfiguration".to_string(),
            rollback_configuration_document(configured),
        );
    }
    insert_text(
        &mut held,
        "StackStatus",
        stack.stack_status().map(|status| status.as_str()),
    );
    insert_text(&mut held, "StackStatusReason", stack.stack_status_reason());
    insert_flag(&mut held, "DisableRollback", stack.disable_rollback());
    insert_list(
        &mut held,
        "NotificationARNs",
        stack.notification_arns().to_vec(),
    );
    insert_number(
        &mut held,
        "TimeoutInMinutes",
        stack.timeout_in_minutes().map(i64::from),
    );
    insert_list(
        &mut held,
        "Capabilities",
        stack
            .capabilities()
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect(),
    );
    insert_structures(&mut held, "Outputs", stack.outputs(), output_document);
    insert_text(&mut held, "RoleARN", stack.role_arn());
    insert_structures(&mut held, "Tags", stack.tags(), tag_document);
    insert_flag(
        &mut held,
        "EnableTerminationProtection",
        stack.enable_termination_protection(),
    );
    insert_text(&mut held, "ParentId", stack.parent_id());
    insert_text(&mut held, "RootId", stack.root_id());
    if let Some(drift) = stack.drift_information() {
        let mut reported = Map::new();
        insert_text(
            &mut reported,
            "StackDriftStatus",
            drift.stack_drift_status().map(|status| status.as_str()),
        );
        insert_timestamp(
            &mut reported,
            "LastCheckTimestamp",
            drift.last_check_timestamp(),
        );
        held.insert("DriftInformation".to_string(), Value::Object(reported));
    }
    insert_flag(
        &mut held,
        "RetainExceptOnCreate",
        stack.retain_except_on_create(),
    );
    insert_text(
        &mut held,
        "DeletionMode",
        stack.deletion_mode().map(|mode| mode.as_str()),
    );
    insert_text(
        &mut held,
        "DetailedStatus",
        stack.detailed_status().map(|status| status.as_str()),
    );
    Value::Object(held)
}

/// One stack SUMMARY as `ListStacks` reported it.
fn stack_summary_document(summary: &::aws_sdk_cloudformation::types::StackSummary) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "StackId", summary.stack_id());
    insert_text(&mut held, "StackName", summary.stack_name());
    insert_text(
        &mut held,
        "TemplateDescription",
        summary.template_description(),
    );
    insert_timestamp(&mut held, "CreationTime", summary.creation_time());
    insert_timestamp(&mut held, "LastUpdatedTime", summary.last_updated_time());
    insert_timestamp(&mut held, "DeletionTime", summary.deletion_time());
    insert_text(
        &mut held,
        "StackStatus",
        summary.stack_status().map(|status| status.as_str()),
    );
    insert_text(&mut held, "StackStatusReason", summary.stack_status_reason());
    insert_text(&mut held, "ParentId", summary.parent_id());
    insert_text(&mut held, "RootId", summary.root_id());
    if let Some(drift) = summary.drift_information() {
        let mut reported = Map::new();
        insert_text(
            &mut reported,
            "StackDriftStatus",
            drift.stack_drift_status().map(|status| status.as_str()),
        );
        insert_timestamp(
            &mut reported,
            "LastCheckTimestamp",
            drift.last_check_timestamp(),
        );
        held.insert("DriftInformation".to_string(), Value::Object(reported));
    }
    Value::Object(held)
}

/// One stack EVENT as `DescribeStackEvents` reported it.
fn stack_event_document(event: &::aws_sdk_cloudformation::types::StackEvent) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "StackId", event.stack_id());
    insert_text(&mut held, "EventId", event.event_id());
    insert_text(&mut held, "StackName", event.stack_name());
    insert_text(&mut held, "LogicalResourceId", event.logical_resource_id());
    insert_text(&mut held, "PhysicalResourceId", event.physical_resource_id());
    insert_text(&mut held, "ResourceType", event.resource_type());
    insert_timestamp(&mut held, "Timestamp", event.timestamp());
    insert_text(
        &mut held,
        "ResourceStatus",
        event.resource_status().map(|status| status.as_str()),
    );
    insert_text(
        &mut held,
        "ResourceStatusReason",
        event.resource_status_reason(),
    );
    insert_text(&mut held, "ResourceProperties", event.resource_properties());
    insert_text(&mut held, "ClientRequestToken", event.client_request_token());
    insert_text(&mut held, "HookType", event.hook_type());
    insert_text(
        &mut held,
        "HookStatus",
        event.hook_status().map(|status| status.as_str()),
    );
    insert_text(&mut held, "HookStatusReason", event.hook_status_reason());
    insert_text(
        &mut held,
        "HookInvocationPoint",
        event.hook_invocation_point().map(|point| point.as_str()),
    );
    insert_text(
        &mut held,
        "HookFailureMode",
        event.hook_failure_mode().map(|mode| mode.as_str()),
    );
    insert_text(
        &mut held,
        "DetailedStatus",
        event.detailed_status().map(|status| status.as_str()),
    );
    Value::Object(held)
}

/// One deployed RESOURCE as `DescribeStackResources` reported it.
fn stack_resource_document(
    resource: &::aws_sdk_cloudformation::types::StackResource,
) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "StackName", resource.stack_name());
    insert_text(&mut held, "StackId", resource.stack_id());
    insert_text(
        &mut held,
        "LogicalResourceId",
        resource.logical_resource_id(),
    );
    insert_text(
        &mut held,
        "PhysicalResourceId",
        resource.physical_resource_id(),
    );
    insert_text(&mut held, "ResourceType", resource.resource_type());
    insert_timestamp(&mut held, "Timestamp", resource.timestamp());
    insert_text(
        &mut held,
        "ResourceStatus",
        resource.resource_status().map(|status| status.as_str()),
    );
    insert_text(
        &mut held,
        "ResourceStatusReason",
        resource.resource_status_reason(),
    );
    insert_text(&mut held, "Description", resource.description());
    insert_resource_drift(&mut held, resource.drift_information());
    insert_module(&mut held, resource.module_info());
    Value::Object(held)
}

/// The same record as `DescribeStackResource` reported it — the DETAIL shape, which carries
/// the resource's metadata beside what the summary and the resource shapes carry.
fn stack_resource_detail_document(
    resource: &::aws_sdk_cloudformation::types::StackResourceDetail,
) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "StackName", resource.stack_name());
    insert_text(&mut held, "StackId", resource.stack_id());
    insert_text(
        &mut held,
        "LogicalResourceId",
        resource.logical_resource_id(),
    );
    insert_text(
        &mut held,
        "PhysicalResourceId",
        resource.physical_resource_id(),
    );
    insert_text(&mut held, "ResourceType", resource.resource_type());
    insert_timestamp(
        &mut held,
        "LastUpdatedTimestamp",
        resource.last_updated_timestamp(),
    );
    insert_text(
        &mut held,
        "ResourceStatus",
        resource.resource_status().map(|status| status.as_str()),
    );
    insert_text(
        &mut held,
        "ResourceStatusReason",
        resource.resource_status_reason(),
    );
    insert_text(&mut held, "Description", resource.description());
    insert_text(&mut held, "Metadata", resource.metadata());
    insert_resource_drift(&mut held, resource.drift_information());
    insert_module(&mut held, resource.module_info());
    Value::Object(held)
}

/// One deployed resource as `ListStackResources` reported it — the SUMMARY shape.
fn stack_resource_summary_document(
    resource: &::aws_sdk_cloudformation::types::StackResourceSummary,
) -> Value {
    let mut held = Map::new();
    insert_text(
        &mut held,
        "LogicalResourceId",
        resource.logical_resource_id(),
    );
    insert_text(
        &mut held,
        "PhysicalResourceId",
        resource.physical_resource_id(),
    );
    insert_text(&mut held, "ResourceType", resource.resource_type());
    insert_timestamp(
        &mut held,
        "LastUpdatedTimestamp",
        resource.last_updated_timestamp(),
    );
    insert_text(
        &mut held,
        "ResourceStatus",
        resource.resource_status().map(|status| status.as_str()),
    );
    insert_text(
        &mut held,
        "ResourceStatusReason",
        resource.resource_status_reason(),
    );
    if let Some(drift) = resource.drift_information() {
        let mut reported = Map::new();
        insert_text(
            &mut reported,
            "StackResourceDriftStatus",
            drift
                .stack_resource_drift_status()
                .map(|status| status.as_str()),
        );
        insert_timestamp(
            &mut reported,
            "LastCheckTimestamp",
            drift.last_check_timestamp(),
        );
        held.insert("DriftInformation".to_string(), Value::Object(reported));
    }
    insert_module(&mut held, resource.module_info());
    Value::Object(held)
}

/// A resource's own DRIFT result, where the answer carried one.
fn insert_resource_drift(
    document: &mut Map<String, Value>,
    drift: Option<&::aws_sdk_cloudformation::types::StackResourceDriftInformation>,
) {
    let Some(drift) = drift else {
        return;
    };
    let mut reported = Map::new();
    insert_text(
        &mut reported,
        "StackResourceDriftStatus",
        drift
            .stack_resource_drift_status()
            .map(|status| status.as_str()),
    );
    insert_timestamp(
        &mut reported,
        "LastCheckTimestamp",
        drift.last_check_timestamp(),
    );
    document.insert("DriftInformation".to_string(), Value::Object(reported));
}

/// The MODULE a resource came from, where the answer carried one.
fn insert_module(
    document: &mut Map<String, Value>,
    module: Option<&::aws_sdk_cloudformation::types::ModuleInfo>,
) {
    let Some(module) = module else {
        return;
    };
    let mut reported = Map::new();
    insert_text(&mut reported, "TypeHierarchy", module.type_hierarchy());
    insert_text(
        &mut reported,
        "LogicalIdHierarchy",
        module.logical_id_hierarchy(),
    );
    document.insert("ModuleInfo".to_string(), Value::Object(reported));
}

/// One stack PARAMETER as the answer reported it.
fn parameter_document(parameter: &::aws_sdk_cloudformation::types::Parameter) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "ParameterKey", parameter.parameter_key());
    insert_text(&mut held, "ParameterValue", parameter.parameter_value());
    insert_flag(
        &mut held,
        "UsePreviousValue",
        parameter.use_previous_value(),
    );
    insert_text(&mut held, "ResolvedValue", parameter.resolved_value());
    Value::Object(held)
}

/// One parameter a TEMPLATE declares, as `ValidateTemplate` reported it.
fn template_parameter_document(
    parameter: &::aws_sdk_cloudformation::types::TemplateParameter,
) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "ParameterKey", parameter.parameter_key());
    insert_text(&mut held, "DefaultValue", parameter.default_value());
    insert_flag(&mut held, "NoEcho", parameter.no_echo());
    insert_text(&mut held, "Description", parameter.description());
    Value::Object(held)
}

/// One stack OUTPUT as the answer reported it.
fn output_document(output: &::aws_sdk_cloudformation::types::Output) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "OutputKey", output.output_key());
    insert_text(&mut held, "OutputValue", output.output_value());
    insert_text(&mut held, "Description", output.description());
    insert_text(&mut held, "ExportName", output.export_name());
    Value::Object(held)
}

/// One TAG as the answer reported it.
fn tag_document(tag: &::aws_sdk_cloudformation::types::Tag) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "Key", tag.key());
    insert_text(&mut held, "Value", tag.value());
    Value::Object(held)
}

/// One EXPORTED value as `ListExports` reported it.
fn export_document(export: &::aws_sdk_cloudformation::types::Export) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "ExportingStackId", export.exporting_stack_id());
    insert_text(&mut held, "Name", export.name());
    insert_text(&mut held, "Value", export.value());
    Value::Object(held)
}

/// The ROLLBACK CONFIGURATION a stack reported, as the shape's own members.
fn rollback_configuration_document(
    configured: &::aws_sdk_cloudformation::types::RollbackConfiguration,
) -> Value {
    let mut held = Map::new();
    insert_structures(
        &mut held,
        "RollbackTriggers",
        configured.rollback_triggers(),
        |trigger| {
            let mut reported = Map::new();
            insert_text(&mut reported, "Arn", trigger.arn());
            insert_text(&mut reported, "Type", trigger.r#type());
            Value::Object(reported)
        },
    );
    insert_number(
        &mut held,
        "MonitoringTimeInMinutes",
        configured.monitoring_time_in_minutes().map(i64::from),
    );
    Value::Object(held)
}

#[cfg(test)]
mod tests {
    use super::BOUND_OPERATIONS;

    /// RULE: the table names each bound operation once, so a handler naming one reaches one
    /// arm of the call above.
    #[test]
    fn each_bound_operation_is_named_once() {
        let mut names = BOUND_OPERATIONS.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            BOUND_OPERATIONS.len(),
            "case {BOUND_OPERATIONS:?}: expected each bound operation once, got {names:?}"
        );
        for bound in BOUND_OPERATIONS {
            assert!(
                bound
                    .starts_with(|first: char| first.is_ascii_uppercase()),
                "case {bound:?}: expected the operation's own spelling"
            );
        }
    }
}
