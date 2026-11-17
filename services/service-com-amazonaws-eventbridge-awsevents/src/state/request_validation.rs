// SPDX-License-Identifier: CC0-1.0

//! The submitted VALUES this service refuses, and the wording of each refusal.
//!
//! Every rule below is one EventBridge enforces while the model expresses none of
//! it — a reserved namespace, the service a referenced resource has to belong to,
//! a member another member makes required — so the boundary validator passes the
//! request and the check has to be made where the write happens. Each is stated
//! ONCE here and reached from every operation that writes the member in question,
//! so two writers of one member cannot admit different values: the dead-letter
//! ARN `CreateEventBus`, `UpdateEventBus` and `PutTargets` each write goes through
//! one check, and so does the tag set every tag write of this service merges
//! ([`super::merge_tags`]).
//!
//! Each refusal is worded as the service words that refusal: a sentence stating
//! the requirement the submitted value failed, in the member's own terms. Where
//! the service states only that a parameter is not valid, the refusal is that
//! subject clause alone ([`super::parameter_not_valid`]).

use crate::ops::types::{AwsVpcConfiguration, EcsParameters};

use super::{
    invalid_arn_violation, parameter_not_valid, schedule, DeadLetterConfig, Tag, Target,
    ARN_NAMESPACE, DEFAULT_EVENT_BUS_NAME,
};

/// The namespace EventBridge reserves for the event buses IT names.
///
/// Only one bus name in it is a customer's to create: the partner event bus,
/// whose name must exactly match the partner event source it is matched to and so
/// lies in `aws.partner/...` (`EventSourceName`'s modelled pattern,
/// `^aws\.partner(/[\.\-_A-Za-z0-9]+){2,}$`). The same namespace is reserved for
/// the `source` of a published event (`crate::ops::put_events`) and for the
/// placeholders an input transformation fills itself
/// ([`super::input_transform`]) — one reserved namespace, read at each member a
/// caller can write it into.
const RESERVED_BUS_NAMESPACE: &str = "aws.";

/// Why `name` is not a name a CUSTOM event bus may carry, or `None` when it is.
///
/// A custom bus is one this account names itself, and the reserved namespace is
/// not the account's to name in: a bus created there would shadow the buses
/// EventBridge itself delivers a service's events to. The partner event bus is
/// the exception, and it is not created by naming it — it is created by naming
/// the partner event source it matches (`CreateEventBus$EventSourceName`), which
/// the caller must already have been offered — so a request that names no source
/// may not reach into the namespace at all.
pub fn reserved_bus_name_violation(name: &str) -> Option<String> {
    name.starts_with(RESERVED_BUS_NAMESPACE)
        .then(|| format!("EventBus name starting with '{RESERVED_BUS_NAMESPACE}' is not valid."))
}

/// The ARN namespace of Amazon SQS, named once: it is the one service a
/// dead-letter queue is a queue of, and the service whose FIFO queues a target
/// may only be delivered to with a message group ([`target_parameters_violation`]).
const SQS_ARN_NAMESPACE: &str = "sqs";

/// What the service states for a dead-letter destination that is not a queue: the
/// SERVICE the submitted ARN named, which is the one thing wrong with it, and
/// that EventBridge supports no dead-letter resource of that service. Stated
/// wherever a `DeadLetterConfig` is written.
fn unsupported_dead_letter_resource(vendor: &str) -> String {
    format!("{vendor} is not supported as a dead letter resource.")
}

/// Why a `DeadLetterConfig` names no dead-letter queue, or `None` when it does.
///
/// `DeadLetterConfig$Arn` is "the ARN of the SQS queue specified as the target
/// for the dead-letter queue", and the structure itself is "configuration details
/// of the Amazon SQS queue for EventBridge to use as a dead-letter queue (DLQ)":
/// the destination is a queue of ONE service, so an ARN naming a resource of any
/// other service is one EventBridge supports no dead-letter write to. The member
/// is modelled as a plain 1..1600 `ResourceArn`, so the boundary validator passes
/// any text — and text that is no ARN at all names no service to report, so it is
/// refused as the malformed ARN it is ([`super::invalid_arn_violation`]).
pub fn dead_letter_arn_violation(config: &DeadLetterConfig) -> Option<String> {
    let arn = config.arn.as_deref()?;
    match ::service_common::arn::vendor(arn) {
        Some(SQS_ARN_NAMESPACE) => None,
        Some(vendor) => Some(unsupported_dead_letter_resource(vendor)),
        None => Some(invalid_arn_violation(arn)),
    }
}

/// Why a `ScheduleExpression` is not one EventBridge accepts, or `None` when it
/// is.
///
/// A schedule expression is a rate or a cron expression — "The scheduling
/// expression. For example, `cron(0 20 * * ? *)` or `rate(5 minutes)`"
/// (`PutRule`'s `ScheduleExpression`) — so an expression EventBridge would fire
/// nothing on is refused rather than stored and silently never fired. Whether it
/// states a schedule at all is decided by the PARSER that fires one
/// ([`schedule::states_a_schedule`]): each form's grammar is read field by field
/// there, so an expression this service accepts is one the clock can make due,
/// and none is taken on the strength of its opening word alone.
///
/// The refusal names the PARAMETER and stops there: EventBridge discloses
/// nothing about what is wrong with a schedule expression it will not take — not
/// the text submitted, not which of the two forms was expected — so the message
/// is the subject clause alone, with no reason after it
/// ([`super::parameter_not_valid`]).
pub fn schedule_expression_violation(expression: &str) -> Option<String> {
    (!schedule::states_a_schedule(expression)).then(|| parameter_not_valid("ScheduleExpression"))
}

/// Why a rule carrying a `ScheduleExpression` cannot live on the event bus
/// `bus`, or `None` when it can.
///
/// A scheduled rule self-triggers — "rules with `ScheduleExpressions`
/// self-trigger based on the given schedule" (`PutRule`) — and EventBridge fires
/// such a rule on the account's default event bus only: a schedule on any other
/// bus would be a rule the service never triggers, so the request is refused
/// rather than answered with a rule that fires nowhere.
pub fn schedule_expression_bus_violation(bus: &str) -> Option<String> {
    (bus != DEFAULT_EVENT_BUS_NAME)
        .then(|| "ScheduleExpression is supported only on the default event bus.".to_string())
}

/// The reserved prefix a tag key a CUSTOMER writes may not carry.
///
/// The `aws:` namespace is the one Amazon Web Services itself tags resources in,
/// so a key in it is not a caller's to set or change on any resource of any
/// service.
const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// What a request naming a key in the reserved namespace is refused with: the
/// FIXED sentence the service states for one, whichever tag call carried it.
///
/// It names neither the key nor the operation nor the resource — the whole
/// message is this sentence — because what is wrong is the namespace the key
/// sits in, which the caller already holds, and the same sentence answers a key
/// being ADDED and a key being REMOVED.
const SYSTEM_TAGS_REFUSAL: &str = "Add/Remove system tags is not allowed.";

/// Whether `key` reaches into the namespace reserved for Amazon Web Services'
/// own tags.
///
/// Matched without regard to case: the reserved namespace is the prefix itself
/// rather than one spelling of it, so a key may not reach into it by changing
/// case.
fn is_reserved_tag_key(key: &str) -> bool {
    key.get(..RESERVED_TAG_KEY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_TAG_KEY_PREFIX))
}

/// Why a submitted tag list carries a key this service will not store, or `None`
/// when every key is the caller's own.
///
/// One reserved key refuses the whole write, so the tag set a resource carries
/// never holds half of a request ([`is_reserved_tag_key`] decides which keys
/// those are).
pub fn reserved_tag_key_violation(tags: &[Tag]) -> Option<String> {
    tags.iter()
        .any(|tag| is_reserved_tag_key(&tag.key))
        .then(|| SYSTEM_TAGS_REFUSAL.to_string())
}

/// Why a submitted list of tag KEYS cannot be removed, or `None` when every key
/// is the caller's own.
///
/// The REMOVE half of the same rule, and so the same sentence: a key in the
/// reserved namespace is not a caller's to delete any more than it is a
/// caller's to write, and one such key refuses the whole request so the
/// resource never loses half of it.
pub fn reserved_tag_key_removal_violation(keys: &[String]) -> Option<String> {
    keys.iter()
        .any(|key| is_reserved_tag_key(key))
        .then(|| SYSTEM_TAGS_REFUSAL.to_string())
}

/// Why one target of a `PutTargets` request cannot be installed as given, or
/// `None` when it can.
///
/// The per-target requirements that follow from the target's own TYPE and from
/// the members it carries, in the order a caller has to resolve them: the role
/// EventBridge needs to invoke the target at all, the parameter BLOCK the
/// target's own kind of resource can only be delivered to with, what that block
/// itself states (an ECS task's definition, count, launch type and network
/// configuration — [`ecs_parameters_violation`]), the service its dead-letter
/// destination has to belong to, and the JSON paths it reshapes its input with.
/// Each is a property of ONE target, so the request is refused whole
/// before any of its targets is stored — a rule carrying a target EventBridge
/// could never invoke would report back a healthy rule that delivers nothing.
///
/// This is the per-target counterpart of [`super::target_arn_violation`], which
/// checks the FORM of each ARN-valued member and runs first: a value that is no
/// ARN is reported as such rather than as a resource of the wrong service.
pub fn target_violation(target: &Target) -> Option<String> {
    target_role_violation(target)
        .or_else(|| target_parameters_violation(target))
        .or_else(|| ecs_parameters_violation(target))
        .or_else(|| {
            target
                .dead_letter_config
                .as_ref()
                .and_then(dead_letter_arn_violation)
        })
        .or_else(|| target_input_path_violation(target))
}

/// The services EventBridge invokes exactly ONE kind of resource of, and the
/// resource type a target of such a service has to name.
///
/// `Target$Arn` is "the Amazon Resource Name (ARN) of the target", and for a
/// service of this class that is the resource EventBridge makes the call ON rather
/// than the work it asks for: an Amazon ECS target names the CLUSTER a task runs on
/// and carries the task itself in its parameter block ("contains the Amazon ECS
/// task definition and task count to be used, if the event target is an Amazon ECS
/// task", `Target$EcsParameters`). So an ARN of that service naming any other
/// resource of it addresses nothing a rule could invoke, and the member is modelled
/// a plain 1..1600 string with no pattern, so the boundary validator passes it and
/// the class is read here.
///
/// It holds the ONE service whose single target class is established — Amazon ECS,
/// where the parameter block's own documentation says what the ARN names. A service
/// EventBridge addresses several kinds of resource of (Systems Manager, whose
/// targets are both a Run Command document and an Automation definition) must not be
/// entered at all, since naming one type would refuse a target AWS accepts, and
/// neither must one whose ARNs carry no resource type (an Amazon SQS queue's ARN is
/// its name); an entry is added for another service when ITS single target class is
/// established the same way.
const SINGLE_TARGET_RESOURCE_TYPES: [(&str, &str); 1] = [(ECS_ARN_NAMESPACE, ECS_CLUSTER_RESOURCE)];

/// Why a target's `Arn` names no resource of its service that a rule could invoke,
/// or `None` when it names one — or names a service whose targets this rule does
/// not narrow.
///
/// What is wrong with such a value is the resource it names, so it is refused as
/// the ARN it is not: the same sentence every ARN-valued member of this service
/// that holds no usable ARN is refused with ([`super::invalid_arn_violation`]).
pub fn uninvocable_target_arn(arn: &str) -> Option<String> {
    let parsed = ::service_common::arn::parse(arn)?;
    SINGLE_TARGET_RESOURCE_TYPES
        .iter()
        .any(|(vendor, resource_type)| {
            parsed.vendor == *vendor && parsed.resource_type() != *resource_type
        })
        .then(|| invalid_arn_violation(arn))
}

/// Why the ECS parameters a target carries describe no task EventBridge could
/// start, or `None` when they describe one — or when the target carries none.
///
/// The block's own members, judged after the block's PRESENCE
/// ([`missing_parameter_block`]): how many tasks a firing may start, the members
/// the launch type requires or rules out, and the sizes of the collections the
/// task's network configuration carries. Every one of them is a requirement the
/// model does not express, so the request is refused here rather than stored as a
/// target whose every firing would fail.
///
/// The block describes work on an ECS CLUSTER, so it is judged for a target that
/// NAMES one: its members are read against that cluster (the Region the task
/// definition has to share with it), which a target of another class does not have.
fn ecs_parameters_violation(target: &Target) -> Option<String> {
    let parameters = target.ecs_parameters.as_ref()?;
    ::service_common::arn::parse_of(&target.arn, ECS_ARN_NAMESPACE)
        .filter(|cluster| cluster.resource_type() == ECS_CLUSTER_RESOURCE)?;
    task_definition_violation(target, parameters)
        .or_else(|| task_definition_region_violation(target, parameters))
        .or_else(|| task_count_violation(target, parameters))
        .or_else(|| launch_type_parameters_violation(target, parameters))
        .or_else(|| vpc_collection_violation(parameters))
}

/// What the service states when the task a target names lives in another Region
/// than the cluster it would run on: the two ARNs, cluster first, as the request
/// gave them.
fn region_mismatch(cluster_arn: &str, task_definition_arn: &str) -> String {
    format!("Region of {cluster_arn} does not match region of {task_definition_arn}")
}

/// Why the task a target names could not run on the cluster the target addresses,
/// or `None` when the two are in one Region.
///
/// A task definition is a REGIONAL resource of Amazon ECS and a cluster runs the
/// tasks of its own Region, so a pair naming two Regions describes a task
/// EventBridge could never start — the cluster's own `RunTask` resolves the
/// definition in the cluster's Region. Both Regions are read through the one ARN
/// parse (`service_common::arn`), and the refusal states the pair the caller
/// submitted.
fn task_definition_region_violation(
    target: &Target,
    parameters: &EcsParameters,
) -> Option<String> {
    let cluster = ::service_common::arn::parse(&target.arn)?;
    let task_definition = ::service_common::arn::parse(&parameters.task_definition_arn)?;
    (cluster.region != task_definition.region)
        .then(|| region_mismatch(&target.arn, &parameters.task_definition_arn))
}

/// The ECS resource a task definition is, and the `EcsParameters` member naming
/// the one a firing runs.
const ECS_TASK_DEFINITION_RESOURCE: &str = "task-definition";
const TASK_DEFINITION_MEMBER: &str = "TaskDefinitionArn";

/// Why the task a target's ECS parameters name is not one EventBridge could run,
/// or `None` when they name one.
///
/// "The ARN of the task definition to use if the event target is an Amazon ECS
/// task" (`EcsParameters$TaskDefinitionArn`, `@required`): the member holds the ARN
/// of an Amazon ECS TASK DEFINITION and nothing else — not the `family:revision`
/// reference the Amazon ECS API itself also takes, and no resource of another
/// service or of another kind — and it is modelled a plain 1..1600 string with no
/// pattern, so the boundary validator passes any text and the form is read here.
/// A reference EventBridge could not resolve to a task definition is a target whose
/// every firing would fail, so it is refused where the target is written.
fn task_definition_violation(target: &Target, parameters: &EcsParameters) -> Option<String> {
    let names_a_task_definition =
        ::service_common::arn::parse_of(&parameters.task_definition_arn, ECS_ARN_NAMESPACE)
            .is_some_and(|arn| arn.resource_type() == ECS_TASK_DEFINITION_RESOURCE);
    (!names_a_task_definition)
        .then(|| invalid_target_parameters(TASK_DEFINITION_MEMBER, &target.id))
}

/// The most tasks one firing of a target may start.
///
/// `EcsParameters$TaskCount` is "the number of tasks to create based on
/// `TaskDefinition`" and is modelled `@range(min: 1)` alone, so the boundary
/// validator passes any figure above zero; the ceiling is the one the ECS call a
/// firing makes states — "the number of instantiations of the specified task to
/// place on your cluster. You can specify up to 10 tasks for each call"
/// (Amazon ECS `RunTask$count`) — so a target asking for more names a firing ECS
/// itself would refuse.
const MAX_ECS_TASK_COUNT: i32 = 10;

/// The `EcsParameters` member carrying how many tasks a firing starts.
const TASK_COUNT_MEMBER: &str = "TaskCount";

/// What the service states for a target parameter whose VALUE it will not take:
/// the member, and the `Id` of the target carrying it. The value itself is not
/// echoed — what is wrong with it is the member's own range, which the caller
/// already has.
fn invalid_target_parameters(member: &str, target_id: &str) -> String {
    format!("Parameter(s) {member} not valid for target: {target_id}.")
}

/// Why the task count a target asks for is not one a firing may start, or `None`
/// when it is within [`MAX_ECS_TASK_COUNT`] or the block names none.
///
/// The modelled minimum is left to the boundary validator, which refuses a figure
/// below it with the constraint clause every modelled bound is refused with; only
/// the ceiling the model does not carry is charged here.
fn task_count_violation(target: &Target, parameters: &EcsParameters) -> Option<String> {
    let count = parameters.task_count?;
    (count > MAX_ECS_TASK_COUNT)
        .then(|| invalid_target_parameters(TASK_COUNT_MEMBER, &target.id))
}

/// The launch type whose network mode EventBridge can only start a task in with a
/// network configuration of its own, and the only one the Fargate-specific members
/// of a parameter block state anything for.
///
/// `EcsParameters$NetworkConfiguration` is "required if `LaunchType` is `FARGATE`
/// because the `awsvpc` mode is required for Fargate tasks": a task started under
/// it has to be given the subnets and security groups it joins, so there is
/// nothing for EventBridge to start without them.
const AWSVPC_ONLY_LAUNCH_TYPE: &str = "FARGATE";

/// The `EcsParameters` member carrying the VPC configuration a task joins.
const NETWORK_CONFIGURATION_MEMBER: &str = "NetworkConfiguration";

/// One `EcsParameters` member that states something only for a task of
/// [`AWSVPC_ONLY_LAUNCH_TYPE`]: the member the refusal names, and whether a
/// submitted block carries it.
struct LaunchTypeOnlyMember {
    member: &'static str,
    carried: fn(&EcsParameters) -> bool,
}

/// The `EcsParameters` members that state something only for a task of
/// [`AWSVPC_ONLY_LAUNCH_TYPE`], and so are not supported under any other launch
/// type.
///
/// Read off each member's own documentation
/// (`smithy_query(query="shape",shape="EcsParameters")`): a member documented as
/// used only under one launch type describes a task of no other kind, so a request
/// naming it beside another launch type asks for two different things.
const LAUNCH_TYPE_ONLY_MEMBERS: [LaunchTypeOnlyMember; 2] = [
    // "Specifies the platform version for the task. […] This structure is used
    // only if LaunchType is FARGATE" (`EcsParameters$PlatformVersion`).
    LaunchTypeOnlyMember {
        member: "PlatformVersion",
        carried: |parameters| parameters.platform_version.is_some(),
    },
    // "Specifies whether the task's elastic network interface receives a public IP
    // address. You can specify ENABLED only when LaunchType in EcsParameters is
    // set to FARGATE" (`AwsVpcConfiguration$AssignPublicIp`) — a member of the VPC
    // configuration rather than of the block itself, and the only value it states
    // for one launch type alone is [`PUBLIC_IP_ASSIGNED`].
    LaunchTypeOnlyMember {
        member: "AssignPublicIp",
        carried: assigns_a_public_ip,
    },
];

/// The `AssignPublicIp` value that states a task of [`AWSVPC_ONLY_LAUNCH_TYPE`]
/// alone; the other value of that enum asks for the behaviour every launch type
/// has.
const PUBLIC_IP_ASSIGNED: &str = "ENABLED";

/// Whether `parameters` ask for the task's network interface to receive a public
/// IP address.
fn assigns_a_public_ip(parameters: &EcsParameters) -> bool {
    parameters
        .network_configuration
        .as_ref()
        .and_then(|configuration| configuration.awsvpc_configuration.as_ref())
        .and_then(|configuration| configuration.assign_public_ip.as_deref())
        == Some(PUBLIC_IP_ASSIGNED)
}

/// What the service states for a parameter another parameter's VALUE makes
/// required: the member that was not specified, the `Id` of the target it was
/// required for, and the launch type that required it.
fn missing_launch_type_parameter(member: &str, target_id: &str, launch_type: &str) -> String {
    format!(
        "Parameter {member} must be specified for target {target_id} when launch type is \
         {launch_type}"
    )
}

/// What the service states for a parameter that states nothing for the launch type
/// the request named: the member, the `Id` of the target carrying it, and the
/// launch type it is not supported under.
fn unsupported_launch_type_parameter(member: &str, target_id: &str, launch_type: &str) -> String {
    format!(
        "Parameter {member} for target {target_id} is not supported when launch type is \
         {launch_type}"
    )
}

/// Why a target's ECS parameters and the LAUNCH TYPE they name do not describe one
/// task, or `None` when they do.
///
/// Which members a parameter block has to carry, and which it may carry at all, is
/// decided by the launch type the block names — a requirement no model can express,
/// since each member is optional on its own — so it is refused where the target is
/// written: a task EventBridge could never start is not a target it stores. Both
/// halves of the rule are read off the launch type the REQUEST named, so a block
/// naming none asks for the cluster's own default and is judged on neither.
fn launch_type_parameters_violation(
    target: &Target,
    parameters: &EcsParameters,
) -> Option<String> {
    let launch_type = parameters.launch_type.as_deref()?;
    if launch_type == AWSVPC_ONLY_LAUNCH_TYPE {
        return parameters.network_configuration.is_none().then(|| {
            missing_launch_type_parameter(NETWORK_CONFIGURATION_MEMBER, &target.id, launch_type)
        });
    }
    LAUNCH_TYPE_ONLY_MEMBERS
        .iter()
        .find(|only| (only.carried)(parameters))
        .map(|only| unsupported_launch_type_parameter(only.member, &target.id, launch_type))
}

/// One collection of the VPC configuration an ECS task joins, and the SIZE its own
/// documentation states it may carry.
struct BoundedCollection {
    /// The member the refusal names.
    member: &'static str,
    /// The fewest elements it may carry.
    fewest: usize,
    /// The most elements it may carry.
    most: usize,
    /// How many elements a submitted configuration carries for it — the absent
    /// optional member carrying none.
    count: fn(&AwsVpcConfiguration) -> usize,
}

/// The collections of a task's VPC configuration whose size this service charges.
///
/// Neither list carries a modelled `@length` (`StringList` is a bare list of
/// `String`, `smithy_query(query="shape",shape="AwsVpcConfiguration,StringList")`),
/// so the boundary validator passes any number of elements and the documented
/// bound is charged where the target is written. The minimum follows the member's
/// own modelling: `Subnets` is `@required`, so a task joins at least one subnet,
/// while `SecurityGroups` is optional — "if you do not specify a security group,
/// the default security group for the VPC is used".
const BOUNDED_VPC_COLLECTIONS: [BoundedCollection; 2] = [
    // "These subnets must all be in the same VPC. You can specify as many as 16
    // subnets" (`AwsVpcConfiguration$Subnets`).
    BoundedCollection {
        member: "Subnets",
        fewest: 1,
        most: 16,
        count: |configuration| configuration.subnets.len(),
    },
    // "These security groups must all be in the same VPC. You can specify as many
    // as five security groups" (`AwsVpcConfiguration$SecurityGroups`).
    BoundedCollection {
        member: "SecurityGroups",
        fewest: 0,
        most: 5,
        count: |configuration| {
            configuration
                .security_groups
                .as_ref()
                .map_or(0, Vec::len)
        },
    },
];

/// What the service states for a collection member carrying a number of elements
/// outside the size it admits: the member, and the size it has to be within.
fn collection_size_violation(bound: &BoundedCollection) -> String {
    super::parameter_violation(
        bound.member,
        &format!(
            "size must be in between {} and {}.",
            bound.fewest, bound.most
        ),
    )
}

/// Why the VPC configuration a target's ECS parameters carry names more (or fewer)
/// of something than a task may join, or `None` when every collection it carries
/// is within the size its documentation states.
///
/// The collections are judged in the order [`BOUNDED_VPC_COLLECTIONS`] lists them,
/// so a configuration getting two of them wrong names the same member every time.
fn vpc_collection_violation(parameters: &EcsParameters) -> Option<String> {
    let configuration = parameters
        .network_configuration
        .as_ref()?
        .awsvpc_configuration
        .as_ref()?;
    BOUNDED_VPC_COLLECTIONS
        .iter()
        .find(|bound| {
            let count = (bound.count)(configuration);
            count < bound.fewest || count > bound.most
        })
        .map(collection_size_violation)
}

/// The suffix the name of an Amazon SQS FIFO queue carries — a queue whose name
/// does not end in it is a standard queue. Source: Amazon SQS requires the name
/// of a FIFO queue to end with `.fifo` (`CreateQueue$QueueName`), and a queue
/// name is case-sensitive, so the suffix is matched as written.
const FIFO_QUEUE_SUFFIX: &str = ".fifo";

/// The `Target` member carrying the parameters an Amazon SQS queue target is
/// delivered with.
const SQS_PARAMETERS_MEMBER: &str = "SqsParameters";

/// What the service states for a target whose own resource can only be delivered
/// to with parameters the request did not specify: the `Target` member that
/// carries them, and the `Id` of the target they were required for. Stated once,
/// so every such requirement is refused in the same words.
fn missing_target_parameters(member: &str, target_id: &str) -> String {
    format!("Parameter(s) {member} must be specified for target: {target_id}.")
}

/// Why a target names none of the parameters its own kind of resource can only be
/// delivered to with, or `None` when it names them or needs none.
///
/// Two readings of one rule — a target EventBridge could never invoke as
/// configured is refused when the target is CONFIGURED rather than stored and
/// found undeliverable at every firing: the parameter BLOCK a target of a given
/// resource class is invoked through ([`missing_parameter_block`]), and the one
/// member inside a block that a class needs beside the block itself (a FIFO
/// queue's message group, [`fifo_queue_group_violation`]).
fn target_parameters_violation(target: &Target) -> Option<String> {
    fifo_queue_group_violation(target).or_else(|| {
        missing_parameter_block(target)
            .map(|member| missing_target_parameters(member, &target.id))
    })
}

/// Why an Amazon SQS FIFO queue target names no message group, or `None` when it
/// names one or is no FIFO queue.
///
/// `Target$SqsParameters` "contains the message group ID to use when the target is
/// an Amazon SQS fair or FIFO queue", and a message sent to a FIFO queue without
/// one is a send that queue refuses (Amazon SQS `SendMessage$MessageGroupId`): a
/// FIFO queue target carrying no message group is a target EventBridge could never
/// deliver to.
///
/// A FIFO queue is one whose NAME carries [`FIFO_QUEUE_SUFFIX`], which is the
/// resource field of its ARN — read through the one ARN parse
/// (`service_common::arn`), so a queue of this service's own reading. The member
/// is required as the parameter BLOCK: `SqsParameters$MessageGroupId` is modelled
/// `@length(min: 0)`, so a group id of no characters is a value the caller
/// specified, while a `SqsParameters` carrying no `MessageGroupId` at all
/// specifies none.
fn fifo_queue_group_violation(target: &Target) -> Option<String> {
    let fifo_queue = ::service_common::arn::parse_of(&target.arn, SQS_ARN_NAMESPACE)
        .is_some_and(|queue| queue.resource.ends_with(FIFO_QUEUE_SUFFIX));
    let names_a_group = target
        .sqs_parameters
        .as_ref()
        .is_some_and(|parameters| parameters.message_group_id.is_some());
    (fifo_queue && !names_a_group)
        .then(|| missing_target_parameters(SQS_PARAMETERS_MEMBER, &target.id))
}

/// One target class EventBridge invokes only THROUGH a parameter block of the
/// `Target` structure: what the ARN names, and the member carrying the block.
struct ParameterisedTarget {
    /// The service namespace of the target's ARN.
    vendor: &'static str,
    /// The resource TYPE within that namespace a target of this class is addressed
    /// by — the resource EventBridge invokes the work ON, which is never the work
    /// itself (an ECS target names a cluster, the task it runs is in the block).
    resource_type: &'static str,
    /// The `Target` member carrying the block, as the refusal names it.
    member: &'static str,
    /// Whether the target CARRIES that block.
    carried: fn(&Target) -> bool,
}

/// The ARN namespace of Amazon ECS, and the resource a target of it names: the
/// cluster a task runs on.
const ECS_ARN_NAMESPACE: &str = "ecs";
const ECS_CLUSTER_RESOURCE: &str = "cluster";

/// The target classes EventBridge can only invoke through their own parameter
/// block, read off the `Target` members that document each class
/// (`smithy_query(query="shape",shape="Target")`, and each block's own shape for
/// the members it marks `@required`).
///
/// A block whose own members are `@required` states what the invocation needs to
/// happen at all — the ECS task definition to run, the Batch job definition to
/// submit, the database to run a statement against, the instances to send a
/// command to — so a target of such a class carrying NO block names none of them,
/// and no delivery to it could ever be made. The block is what the refusal names,
/// because that is the member the caller did not specify.
const PARAMETERISED_TARGETS: [ParameterisedTarget; 4] = [
    // "Contains the Amazon ECS task definition and task count to be used, if the
    // event target is an Amazon ECS task" (`Target$EcsParameters`), whose
    // `TaskDefinitionArn` is `@required`. An ECS target is addressed by the
    // CLUSTER the task runs on.
    ParameterisedTarget {
        vendor: ECS_ARN_NAMESPACE,
        resource_type: ECS_CLUSTER_RESOURCE,
        member: "EcsParameters",
        carried: |target| target.ecs_parameters.is_some(),
    },
    // "If the event target is an Batch job, this contains the job definition, job
    // name, and other parameters" (`Target$BatchParameters`), whose
    // `JobDefinition` and `JobName` are both `@required`. A Batch target is
    // addressed by the job QUEUE the job is submitted to.
    ParameterisedTarget {
        vendor: "batch",
        resource_type: "job-queue",
        member: "BatchParameters",
        carried: |target| target.batch_parameters.is_some(),
    },
    // "These are custom parameters to be used when the target is a Amazon Redshift
    // cluster to invoke the Amazon Redshift Data API ExecuteStatement"
    // (`Target$RedshiftDataParameters`), whose `Database` is `@required`.
    ParameterisedTarget {
        vendor: "redshift",
        resource_type: "cluster",
        member: "RedshiftDataParameters",
        carried: |target| target.redshift_data_parameters.is_some(),
    },
    // "Parameters used when you are using the rule to invoke Amazon EC2 Run
    // Command" (`Target$RunCommandParameters`), whose `RunCommandTargets` is
    // `@required`: it "contains the criteria (either InstanceIds or a tag) used to
    // specify which EC2 instances are to be sent the command". Such a target is
    // addressed by the Systems Manager DOCUMENT the command runs.
    ParameterisedTarget {
        vendor: "ssm",
        resource_type: "document",
        member: "RunCommandParameters",
        carried: |target| target.run_command_parameters.is_some(),
    },
];

/// The `Target` member of the parameter block `target`'s own resource class is
/// invoked through and the target does not carry, or `None` when it carries it or
/// names a class that needs none.
///
/// The class is read off the target's ARN through the one ARN parse
/// (`service_common::arn`), so the service namespace and the resource type decide
/// it and nothing is inferred from a name.
fn missing_parameter_block(target: &Target) -> Option<&'static str> {
    let arn = ::service_common::arn::parse(&target.arn)?;
    PARAMETERISED_TARGETS
        .iter()
        .find(|class| arn.vendor == class.vendor && arn.resource_type() == class.resource_type)
        .filter(|class| !(class.carried)(target))
        .map(|class| class.member)
}

/// Why a target names no role for an invocation EventBridge makes AS one, or
/// `None` when the target needs none or names it.
///
/// "To be able to make API calls against the resources that you own, Amazon
/// EventBridge needs the appropriate permissions: for Lambda and Amazon SNS
/// resources, EventBridge relies on resource-based policies; for EC2 instances,
/// Kinesis Data Streams, Step Functions state machines and API Gateway APIs,
/// EventBridge relies on IAM roles that you specify in the `RoleARN` argument in
/// `PutTargets`" (this operation's own documentation) — and an event bus of
/// another account is in that second class too ("you must specify a `RoleArn`
/// with proper permissions in the `Target` structure"). A target of that class
/// carrying no `RoleArn` names nothing for EventBridge to assume, so the
/// delivery it asks for could never be made.
///
/// The classes are the ones EventBridge delivers to BY MAKING that API call on a
/// resource of the account — the ones the sentence above enumerates, plus the
/// Amazon ECS task target, whose delivery is an `ecs:RunTask` on the cluster the
/// target names — read off the service and the resource type of the target's ARN
/// ([`ROLE_INVOKED_TARGETS`]). A target of any other service is one this
/// operation accepts without a role, whether it is invoked under a
/// resource-based policy or under a role the delivery assumes when one is given
/// ([`crate::ops::put_events`]'s own reading of the same sentence).
fn target_role_violation(target: &Target) -> Option<String> {
    if target.role_arn.is_some() || !invoked_under_a_named_role(&target.arn) {
        return None;
    }
    Some(format!("RoleArn is required for target {}.", target.arn))
}

/// The target classes EventBridge invokes under the role the target names: the
/// SERVICE of the ARN, and the resource TYPE within it that names a target of
/// that class (`None` where the service names one kind of target only).
///
/// An ARN of the same service naming another kind of resource is NOT one of them:
/// an `arn:aws:events:…:api-destination/…` is an HTTP endpoint this service
/// invokes through a connection's own credentials, not the event bus of another
/// account the documentation is about.
const ROLE_INVOKED_TARGETS: [(&str, Option<&str>); 6] = [
    // "Step Functions state machines"
    ("states", Some("stateMachine")),
    // "Kinesis Data Streams"
    ("kinesis", Some("stream")),
    // "EC2 instances"
    ("ec2", Some("instance")),
    // "API Gateway APIs", whose invocation ARNs are of the `execute-api` service
    ("execute-api", None),
    // The event bus of another account or Region
    (ARN_NAMESPACE, Some("event-bus")),
    // An Amazon ECS task, which EventBridge starts by calling Amazon ECS
    // `RunTask` on the CLUSTER the target names ("contains the Amazon ECS task
    // definition and task count to be used, if the event target is an Amazon ECS
    // task", `Target$EcsParameters`): the call is made against a resource of the
    // account, so it is made AS a role the account hands EventBridge, and a
    // target naming none names nothing for EventBridge to call ECS as.
    (ECS_ARN_NAMESPACE, Some(ECS_CLUSTER_RESOURCE)),
];

/// Whether the target `arn` names a resource of one of [`ROLE_INVOKED_TARGETS`].
fn invoked_under_a_named_role(arn: &str) -> bool {
    let Some(parsed) = ::service_common::arn::parse(arn) else {
        return false;
    };
    ROLE_INVOKED_TARGETS
        .iter()
        .any(|(vendor, resource_type)| {
            parsed.vendor == *vendor
                && resource_type.is_none_or(|kind| parsed.resource_type() == kind)
        })
}

/// Why a target reshapes its input through a path EventBridge does not evaluate,
/// or `None` when every path it carries is one.
///
/// "If `InputPath` is specified in the form of JSONPath (for example,
/// `$.detail`), then only the part of the event specified in the path is passed
/// to the target", `InputPathsMap` is a map "where each value is a valid JSON
/// path", and "when you specify `InputPath` or `InputTransformer`, you must use
/// JSON dot notation, not bracket notation" (`PutTargets`). So a path that is not
/// rooted at the event, that names its members in bracket notation, or that
/// selects through a construct the supported subset does not carry, is refused
/// when the target is CONFIGURED rather than stored and found undeliverable at
/// delivery time.
///
/// The supported subset is the one this service evaluates
/// (`service_common::json_reference_path`, which admits `$`, `.name`, `[index]`
/// and one many-place selector), so a target this operation accepts is one every
/// delivery can reshape.
///
/// The paths are judged in a stable order — `InputPath` first, then the map's
/// keys sorted — because `InputPathsMap` arrives as an unordered map and a
/// refusal must name the same target for the same request every time. Either
/// member is one way of stating the same thing — the JSON path a target's input
/// is selected by — so the refusal is worded for the TARGET rather than for the
/// member or the path that carried it.
fn target_input_path_violation(target: &Target) -> Option<String> {
    let unsupported = |path: &String| !supported_path(path);
    let carried = target.input_path.as_ref().filter(|path| unsupported(path));
    let mapped = || {
        let map = target
            .input_transformer
            .as_ref()
            .and_then(|transformer| transformer.input_paths_map.as_ref())?;
        let mut paths: Vec<&String> = map.values().collect();
        paths.sort();
        paths.into_iter().find(|path| unsupported(path))
    };
    carried
        .or_else(mapped)
        .map(|_| format!("InputPath for target {} is invalid.", target.id))
}

/// The bracket forms a MEMBER may be named with in a JSON path, which this
/// member's documentation rules out ("you must use JSON dot notation, not bracket
/// notation"): a quoted name inside a `[…]` selector.
///
/// An index selector is bracketed too and is not one of these: it names an
/// element of an array rather than a member of an object, which is the only way
/// an element can be named at all.
const BRACKETED_MEMBER_FORMS: [&str; 2] = ["['", "[\""];

/// Whether `path` is one this service reads: a reference path in the supported
/// subset, naming each member of the event in dot notation.
fn supported_path(path: &str) -> bool {
    !BRACKETED_MEMBER_FORMS
        .iter()
        .any(|bracketed| path.contains(bracketed))
        && ::service_common::json_reference_path::selection_segments(path).is_some()
}
