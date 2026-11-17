// SPDX-License-Identifier: CC0-1.0

//! The AMAZON ECS TASK half of the delivery pass: the `RunTask` a rule makes when an event
//! it matched reaches a target naming an ECS cluster.
//!
//! The rule AWS follows: a rule target whose `Arn` names an Amazon ECS cluster is invoked by
//! RUNNING the task its `EcsParameters` describe in that cluster — "contains the Amazon ECS
//! task definition and task count to be used, if the event target is an Amazon ECS task"
//! (`Target$EcsParameters`) — acting as the role the target names. Every member of that
//! parameter block is a member of the run: the task definition, the count ("the number of
//! tasks to create based on `TaskDefinition`. The default is 1"), the launch type, the
//! network configuration an `awsvpc` task joins, the platform version, the group, the
//! capacity-provider strategy, the placement constraints and strategy, the managed-tag and
//! execute-command switches, the tag propagation, the tags and the reference id. The run is
//! `startedBy` the RULE that made it ([`started_by`]), which is how a client tells the tasks
//! one rule launched from every other task of the cluster.
//!
//! What the target asked to RECEIVE is the run's `overrides`: "a list of container overrides
//! in JSON format that specify the name of a container in the specified task definition and
//! the overrides it should receive" (`RunTask$overrides`) — so the constant `Input`, the part
//! an `InputPath` selects and the rendering of an `InputTransformer` are each read as that
//! `TaskOverride` document ([`task_override`]), which is what carries a rendered event value
//! into the overridden command of a container. A target that asked for nothing of the kind
//! overrides nothing: the whole event envelope is not an override document, so a run with no
//! target input of its own runs the revision as it is registered.
//!
//! Running the task HANDS Amazon ECS the roles that task assumes, which is a second
//! authorization the delivery needs ([`crate::state::ecs_target`]): the roles come off the
//! run's own overrides where it names them and off the REGISTERED revision otherwise, which
//! is read back through that service's own `DescribeTaskDefinition`.
//!
//! The service is reached the way every other target of this pass is: through the in-process
//! callback with the SDK's generated client, so this crate names no other service crate.

use ::serde_json::{Map, Value};

use super::*;

/// The ARN namespace of Amazon ECS, whose clusters a rule runs tasks in
/// (`smithy_query(query="service_info")` of that service: `arnNamespace` `ecs`).
pub(super) const ECS_ARN_NAMESPACE: &str = "ecs";

/// The resource type an Amazon ECS cluster ARN names
/// (`arn:<partition>:ecs:<region>:<account>:cluster/<name>`).
const CLUSTER_RESOURCE_TYPE: &str = "cluster";

/// How many tasks a run places when the target's parameters name no count:
/// `EcsParameters$TaskCount` is "the number of tasks to create based on `TaskDefinition`. The
/// default is 1."
const DEFAULT_TASK_COUNT: i32 = 1;

/// What `Task$startedBy` carries for a task an EventBridge rule launched: the service's own
/// prefix and the rule's name, which is what `ListTasks$startedBy` filters such tasks by.
///
/// `RunTask$startedBy` takes "up to 128 letters (uppercase and lowercase), numbers, hyphens
/// (-), forward slash (/), and underscores (_)", which a rule name (`RuleName`, modelled at
/// 64 characters) and this prefix stay inside.
const STARTED_BY_PREFIX: &str = "events-rule/";

/// The cluster `arn` names, or `None` when the ARN names no cluster of Amazon ECS.
///
/// A cluster's ARN names the cluster directly (`cluster/<name>`): a resource carrying a
/// further segment addresses something WITHIN a cluster (one of its tasks, one of its
/// services) rather than the cluster, so no task is run for it.
pub(super) fn cluster_in_arn(arn: &str) -> Option<&str> {
    ::service_common::arn::resource_id_of_type(arn, CLUSTER_RESOURCE_TYPE)
        .filter(|name| !name.is_empty() && !name.contains(['/', ':']))
}

/// The task definition an ECS target's run names, which is the resource the `ecs:RunTask`
/// the invocation performs is performed ON — the cluster is carried as a condition key
/// instead ([`crate::state::ecs_target::run_task_refusal`]).
///
/// A target carrying no `EcsParameters` names no task definition, in which case there is no
/// run to decide and the target's own ARN is all the attempt can be about.
pub(super) fn run_resource<'a>(handover: &Handover<'a>) -> &'a str {
    handover
        .target
        .ecs_parameters
        .as_ref()
        .map_or(handover.target.arn.as_str(), |parameters| {
            parameters.task_definition_arn.as_str()
        })
}

/// Why the policies in force on the target's role bar the RUN this handover would make, or
/// `None` when nothing of the kind can be shown.
///
/// The role-governed half of this target kind's authorization that needs no call to make:
/// the `ecs:RunTask` itself. The hand-over of the task's own roles needs the registered
/// revision read first, so it is decided where the run is made ([`run`]).
pub(super) fn run_authorization_refusal(
    iam: Option<&::engine::ServiceState<crate::state::IamState>>,
    role_arn: Option<&str>,
    handover: &Handover<'_>,
    attempt: &crate::state::DeliveryAttempt<'_>,
) -> Option<(String, String)> {
    let (iam, role_arn) = (iam?, role_arn?);
    // The guard is held for the read alone, never across the invocation.
    let guard = iam.lock();
    crate::state::ecs_target::run_task_refusal(
        role_arn,
        attempt,
        run_resource(handover),
        &handover.target.arn,
        &guard,
    )
}

/// Run the task this handover's target describes, and answer what came of it.
pub(super) async fn run(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    stores: &Stores<'_>,
    region: &str,
    handover: &Handover<'_>,
    body: &str,
) -> crate::state::InvocationOutcome {
    let target = handover.target;
    let Some(parameters) = target.ecs_parameters.as_ref() else {
        return crate::state::InvocationOutcome::NotAttempted(
            "the target carries no EcsParameters, so it names no task to run".to_string(),
        );
    };
    // What the target asked to RECEIVE is the run's overrides, and only a target that asked
    // for something other than the event as published has one: the envelope is the event,
    // not an override document.
    let overrides = target
        .reshapes()
        .then(|| task_override(body))
        .flatten();
    // One client for both calls this delivery makes on Amazon ECS, for the reason the other
    // multi-call branches of the delivery pass give.
    let client = clients.ecs(callback, region).clone();
    // The roles this run HANDS Amazon ECS have to be ones the target's role may pass, which
    // is decided before anything is placed: a run AWS refuses places no task.
    let passed = passed_roles(clients, &client, parameters, overrides.as_ref()).await;
    if let Some(refusal) = passed_roles_refusal(stores.iam, handover, &passed) {
        let (code, message) = refusal;
        return crate::state::InvocationOutcome::Failed { code, message };
    }
    let request = run_request(&client, parameters, handover, overrides);
    match request.send().await {
        Ok(answered) => placed(&answered),
        Err(error) => outcome(Err::<(), _>(error)),
    }
}

/// The `RunTask` call this handover makes: the target's whole parameter block, the rule that
/// started it, and the overrides the target asked for.
fn run_request(
    client: &::aws_sdk_ecs::Client,
    parameters: &EcsParameters,
    handover: &Handover<'_>,
    overrides: Option<::aws_sdk_ecs::types::TaskOverride>,
) -> ::aws_sdk_ecs::operation::run_task::builders::RunTaskFluentBuilder {
    let mut request = client
        .run_task()
        .cluster(&handover.target.arn)
        .task_definition(&parameters.task_definition_arn)
        .count(parameters.task_count.unwrap_or(DEFAULT_TASK_COUNT))
        .started_by(started_by(handover.rule))
        .enable_ecs_managed_tags(parameters.enable_ecs_managed_tags)
        .enable_execute_command(parameters.enable_execute_command)
        .set_group(parameters.group.clone())
        .set_platform_version(parameters.platform_version.clone())
        .set_reference_id(parameters.reference_id.clone())
        .set_overrides(overrides);
    if let Some(launch_type) = parameters.launch_type.as_deref() {
        request = request.launch_type(::aws_sdk_ecs::types::LaunchType::from(launch_type));
    }
    if let Some(propagate) = parameters.propagate_tags.as_deref() {
        request = request.propagate_tags(::aws_sdk_ecs::types::PropagateTags::from(propagate));
    }
    if let Some(network) = network_configuration(parameters.network_configuration.as_ref()) {
        request = request.network_configuration(network);
    }
    // Each list is carried only when the target named one: an empty list is not the same
    // request as none — "if a `capacityProviderStrategy` is specified, the `launchType`
    // parameter must be omitted" — so a target that named no list leaves the member absent.
    let strategy: Vec<_> = parameters
        .capacity_provider_strategy
        .iter()
        .flatten()
        .filter_map(capacity_provider_item)
        .collect();
    if !strategy.is_empty() {
        request = request.set_capacity_provider_strategy(Some(strategy));
    }
    let constraints: Vec<_> = parameters
        .placement_constraints
        .iter()
        .flatten()
        .map(placement_constraint)
        .collect();
    if !constraints.is_empty() {
        request = request.set_placement_constraints(Some(constraints));
    }
    let placement: Vec<_> = parameters
        .placement_strategy
        .iter()
        .flatten()
        .map(placement_strategy)
        .collect();
    if !placement.is_empty() {
        request = request.set_placement_strategy(Some(placement));
    }
    let tags: Vec<_> = parameters.tags.iter().flatten().map(tag).collect();
    if !tags.is_empty() {
        request = request.set_tags(Some(tags));
    }
    request
}

/// What `Task$startedBy` carries for a task the rule `rule` launched.
fn started_by(rule: &str) -> String {
    format!("{STARTED_BY_PREFIX}{rule}")
}

/// What a `RunTask` that was ACCEPTED means for the delivery.
///
/// "Any failures associated with the call" (`RunTaskResponse$failures`): a call that placed
/// at least one task delivered the event, and one that placed none placed nothing — which is
/// a FAILED invocation carrying the account Amazon ECS gave of the failure, so it is
/// dead-lettered and counted on the rule's `FailedInvocations` like any other delivery the
/// target refused. A placement failure is Amazon ECS's own `reason`/`detail` pair rather than
/// an error code, so it carries the condition for a refusal the classification does not place
/// ([`delivery_condition`] answers the same for an unplaced code).
fn placed(answered: &::aws_sdk_ecs::operation::run_task::RunTaskOutput) -> crate::state::InvocationOutcome {
    if !answered.tasks().is_empty() {
        return crate::state::InvocationOutcome::Invoked;
    }
    let reported: Vec<String> = answered
        .failures()
        .iter()
        .map(|failure| match (failure.reason(), failure.detail()) {
            (Some(reason), Some(detail)) => format!("{reason}: {detail}"),
            (Some(reason), None) => reason.to_string(),
            (None, Some(detail)) => detail.to_string(),
            (None, None) => String::new(),
        })
        .filter(|reported| !reported.is_empty())
        .collect();
    crate::state::InvocationOutcome::Failed {
        code: crate::state::UNKNOWN_CONDITION.to_string(),
        message: if reported.is_empty() {
            "Amazon ECS placed no task for this run.".to_string()
        } else {
            format!("Amazon ECS placed no task for this run: {}", reported.join("; "))
        },
    }
}

/// Why the policies in force on the target's role bar HANDING Amazon ECS one of the roles
/// this run passes, as the invocation outcome that records it.
fn passed_roles_refusal(
    iam: Option<&::engine::ServiceState<crate::state::IamState>>,
    handover: &Handover<'_>,
    passed: &[String],
) -> Option<(String, String)> {
    if passed.is_empty() {
        return None;
    }
    let iam = iam?;
    let role_arn = handover
        .target
        .role_arn
        .as_deref()
        .filter(|role_arn| !role_arn.is_empty())?;
    let attempt = crate::state::DeliveryAttempt {
        rule: handover.rule,
        target_id: &handover.target.id,
        action: crate::state::pass_role::PASS_ROLE_ACTION,
        resource: run_resource(handover),
    };
    // The guard is held for the read alone, never across the invocation.
    let guard = iam.lock();
    crate::state::ecs_target::passed_roles_refusal(role_arn, &attempt, passed, &guard)
}

/// The roles this run HANDS Amazon ECS, in the order a run hands them over: the task role its
/// containers assume and the execution role its agent acts as, each taken from the run's own
/// overrides where it names one and from the REGISTERED revision otherwise.
///
/// A revision this emulator cannot read back shows nothing about the roles it declares, so
/// only the roles the run itself names are handed over for it — the emulator refuses only
/// what it can show.
async fn passed_roles(
    clients: &mut Clients,
    client: &::aws_sdk_ecs::Client,
    parameters: &EcsParameters,
    overrides: Option<&::aws_sdk_ecs::types::TaskOverride>,
) -> Vec<String> {
    let declared = clients
        .task_definition_roles(client, &parameters.task_definition_arn)
        .await;
    let mut handed: Vec<String> = Vec::new();
    for role in [
        overrides
            .and_then(::aws_sdk_ecs::types::TaskOverride::task_role_arn)
            .map(str::to_string)
            .or(declared.task_role_arn),
        overrides
            .and_then(::aws_sdk_ecs::types::TaskOverride::execution_role_arn)
            .map(str::to_string)
            .or(declared.execution_role_arn),
    ]
    .into_iter()
    .flatten()
    {
        if !role.is_empty() && !handed.contains(&role) {
            handed.push(role);
        }
    }
    handed
}

/// The roles a REGISTERED task-definition revision declares, which a run of it hands Amazon
/// ECS.
#[derive(Debug, Clone, Default)]
pub(super) struct TaskDefinitionRoles {
    /// `TaskDefinition$taskRoleArn`: the role the task's containers assume.
    pub(super) task_role_arn: Option<String>,
    /// `TaskDefinition$executionRoleArn`: the role the container agent acts as.
    pub(super) execution_role_arn: Option<String>,
}

/// The roles the revision `task_definition` names declares, read back through Amazon ECS's
/// own `DescribeTaskDefinition`.
///
/// A revision this account does not hold, and an engine with no Amazon ECS mounted to ask,
/// each answer that nothing is known about the roles rather than that there are none: the run
/// itself is what then reports the refusal, in that service's own words.
pub(super) async fn task_definition_roles(
    client: &::aws_sdk_ecs::Client,
    task_definition: &str,
) -> TaskDefinitionRoles {
    let answered = client
        .describe_task_definition()
        .task_definition(task_definition)
        .send()
        .await;
    let described = match answered {
        Ok(described) => described,
        Err(error) => {
            ::tracing::debug!(
                task_definition = %task_definition,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the task definition a rule's ECS target runs could not be read back, so the \
                 roles its run hands over are the ones the run itself names"
            );
            return TaskDefinitionRoles::default();
        }
    };
    let Some(definition) = described.task_definition() else {
        return TaskDefinitionRoles::default();
    };
    TaskDefinitionRoles {
        task_role_arn: definition.task_role_arn().map(str::to_string),
        execution_role_arn: definition.execution_role_arn().map(str::to_string),
    }
}

/// The `TaskOverride` the text `body` carries, or `None` when it is no override document at
/// all.
///
/// The text is a CALLER's — the target's `Input`, or what its `InputPath`/`InputTransformer`
/// rendered — so every member is read as what `TaskOverride` models it as and a member the
/// document does not carry stays absent. A required member of a nested shape the document
/// leaves out is passed on as it stands rather than invented: what such a document earns is
/// Amazon ECS's own answer about it, which is what the delivery then reports.
fn task_override(body: &str) -> Option<::aws_sdk_ecs::types::TaskOverride> {
    let document: Value = ::serde_json::from_str(body).ok()?;
    let fields = document.as_object()?;
    let mut builder = ::aws_sdk_ecs::types::TaskOverride::builder()
        .set_cpu(text(fields, "cpu"))
        .set_memory(text(fields, "memory"))
        .set_task_role_arn(text(fields, "taskRoleArn"))
        .set_execution_role_arn(text(fields, "executionRoleArn"));
    if let Some(storage) = object(fields, "ephemeralStorage") {
        builder = builder.ephemeral_storage(
            ::aws_sdk_ecs::types::EphemeralStorage::builder()
                .size_in_gib(number(storage, "sizeInGiB").unwrap_or_default())
                .build(),
        );
    }
    if let Some(containers) = list(fields, "containerOverrides") {
        builder = builder.set_container_overrides(Some(
            containers.iter().filter_map(container_override).collect(),
        ));
    }
    Some(builder.build())
}

/// One entry of an override document's `containerOverrides`.
fn container_override(value: &Value) -> Option<::aws_sdk_ecs::types::ContainerOverride> {
    let fields = value.as_object()?;
    let mut builder = ::aws_sdk_ecs::types::ContainerOverride::builder()
        .set_name(text(fields, "name"))
        .set_command(strings(fields, "command"))
        .set_cpu(number(fields, "cpu"))
        .set_memory(number(fields, "memory"))
        .set_memory_reservation(number(fields, "memoryReservation"));
    if let Some(pairs) = list(fields, "environment") {
        builder = builder.set_environment(Some(pairs.iter().filter_map(key_value_pair).collect()));
    }
    if let Some(files) = list(fields, "environmentFiles") {
        builder =
            builder.set_environment_files(Some(files.iter().filter_map(environment_file).collect()));
    }
    if let Some(requirements) = list(fields, "resourceRequirements") {
        builder = builder.set_resource_requirements(Some(
            requirements
                .iter()
                .filter_map(resource_requirement)
                .collect(),
        ));
    }
    Some(builder.build())
}

/// One entry of a container override's `environment`.
fn key_value_pair(value: &Value) -> Option<::aws_sdk_ecs::types::KeyValuePair> {
    let fields = value.as_object()?;
    Some(
        ::aws_sdk_ecs::types::KeyValuePair::builder()
            .set_name(text(fields, "name"))
            .set_value(text(fields, "value"))
            .build(),
    )
}

/// One entry of a container override's `environmentFiles`.
fn environment_file(value: &Value) -> Option<::aws_sdk_ecs::types::EnvironmentFile> {
    let fields = value.as_object()?;
    ::aws_sdk_ecs::types::EnvironmentFile::builder()
        .value(text(fields, "value").unwrap_or_default())
        .r#type(::aws_sdk_ecs::types::EnvironmentFileType::from(
            text(fields, "type").unwrap_or_default().as_str(),
        ))
        .build()
        .ok()
}

/// One entry of a container override's `resourceRequirements`.
fn resource_requirement(value: &Value) -> Option<::aws_sdk_ecs::types::ResourceRequirement> {
    let fields = value.as_object()?;
    ::aws_sdk_ecs::types::ResourceRequirement::builder()
        .value(text(fields, "value").unwrap_or_default())
        .r#type(::aws_sdk_ecs::types::ResourceType::from(
            text(fields, "type").unwrap_or_default().as_str(),
        ))
        .build()
        .ok()
}

/// The network an `awsvpc` task of this run joins, as Amazon ECS takes it:
/// `EcsParameters$NetworkConfiguration` is "this structure if the Amazon ECS task uses the
/// `awsvpc` network mode", and `None` for a target that named none.
fn network_configuration(
    configuration: Option<&NetworkConfiguration>,
) -> Option<::aws_sdk_ecs::types::NetworkConfiguration> {
    let vpc = configuration?.awsvpc_configuration.as_ref()?;
    let mut builder = ::aws_sdk_ecs::types::AwsVpcConfiguration::builder()
        .set_subnets(Some(vpc.subnets.clone()))
        .set_security_groups(vpc.security_groups.clone());
    if let Some(public_ip) = vpc.assign_public_ip.as_deref() {
        builder = builder.assign_public_ip(::aws_sdk_ecs::types::AssignPublicIp::from(public_ip));
    }
    let built = builder.build().ok()?;
    Some(
        ::aws_sdk_ecs::types::NetworkConfiguration::builder()
            .awsvpc_configuration(built)
            .build(),
    )
}

/// One item of the run's `capacityProviderStrategy`.
fn capacity_provider_item(
    item: &CapacityProviderStrategyItem,
) -> Option<::aws_sdk_ecs::types::CapacityProviderStrategyItem> {
    ::aws_sdk_ecs::types::CapacityProviderStrategyItem::builder()
        .capacity_provider(&item.capacity_provider)
        .weight(item.weight)
        .base(item.base)
        .build()
        .ok()
}

/// One of the run's `placementConstraints`.
fn placement_constraint(
    constraint: &PlacementConstraint,
) -> ::aws_sdk_ecs::types::PlacementConstraint {
    let mut builder = ::aws_sdk_ecs::types::PlacementConstraint::builder()
        .set_expression(constraint.expression.clone());
    if let Some(kind) = constraint.r#type.as_deref() {
        builder = builder.r#type(::aws_sdk_ecs::types::PlacementConstraintType::from(kind));
    }
    builder.build()
}

/// One of the run's `placementStrategy` rules.
fn placement_strategy(strategy: &PlacementStrategy) -> ::aws_sdk_ecs::types::PlacementStrategy {
    let mut builder =
        ::aws_sdk_ecs::types::PlacementStrategy::builder().set_field(strategy.field.clone());
    if let Some(kind) = strategy.r#type.as_deref() {
        builder = builder.r#type(::aws_sdk_ecs::types::PlacementStrategyType::from(kind));
    }
    builder.build()
}

/// One of the run's `tags`.
fn tag(tag: &Tag) -> ::aws_sdk_ecs::types::Tag {
    ::aws_sdk_ecs::types::Tag::builder()
        .key(&tag.key)
        .value(&tag.value)
        .build()
}

/// The text member `member` of `fields`, and `None` when the document does not carry it as
/// one.
fn text(fields: &Map<String, Value>, member: &str) -> Option<String> {
    fields.get(member)?.as_str().map(str::to_string)
}

/// The integer member `member` of `fields`, within what the member is modelled at.
fn number(fields: &Map<String, Value>, member: &str) -> Option<i32> {
    i32::try_from(fields.get(member)?.as_i64()?).ok()
}

/// The list of text member `member` of `fields` carries, with every element that is text.
fn strings(fields: &Map<String, Value>, member: &str) -> Option<Vec<String>> {
    Some(
        list(fields, member)?
            .iter()
            .filter_map(|element| element.as_str().map(str::to_string))
            .collect(),
    )
}

/// The list member `member` of `fields`.
fn list<'a>(fields: &'a Map<String, Value>, member: &str) -> Option<&'a Vec<Value>> {
    fields.get(member)?.as_array()
}

/// The object member `member` of `fields`.
fn object<'a>(fields: &'a Map<String, Value>, member: &str) -> Option<&'a Map<String, Value>> {
    fields.get(member)?.as_object()
}
