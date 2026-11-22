// SPDX-License-Identifier: CC0-1.0

//! Whether the `awslogs` LOG DRIVER a launched task's containers configure can be
//! initialised, and the stop a task whose logger cannot earns.
//!
//! # The rule this module implements
//!
//! A container definition's `logConfiguration` names "the log driver to use for the
//! container", and the `awslogs` driver "sends container logs to Amazon CloudWatch Logs":
//! the driver is initialised BEFORE the container it is for runs — it creates the stream the
//! output will go to ([`super::container_logs`]) — and Amazon ECS makes that call AS the
//! task's EXECUTION role ("the task execution role that grants the Amazon ECS container
//! agent permission to make Amazon Web Services API calls on your behalf",
//! `TaskDefinition$executionRoleArn`).
//!
//! So a launch whose logger cannot be initialised launches nothing: no container starts,
//! nothing is delivered to the group the configuration names, and the task is stopped at the
//! instant it would otherwise have started, with the stop code of a task that never ran
//! ([`STOP_CODE_TASK_FAILED_TO_START`]) and the `stoppedReason` the agent words for a logger
//! it could not validate ([`unable_to_initialise_logger_message`]). A launch whose logger
//! can be initialised runs, and each container's output is delivered to its own stream
//! ([`super::container_logs`]).
//!
//! Every half asks the service that OWNS the log group: whether the execution role may
//! create the stream, whether there is a group to create it in, and whether that role may
//! create the group, are answered by CloudWatch Logs' own gate over its own store
//! ([`::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_stream_creation`]) —
//! so a launch is refused exactly where that service's own `CreateLogStream` or
//! `CreateLogGroup` would refuse the driver, and nothing is decided against a configuration
//! it says nothing about.
//!
//! The driver's own ORDER decides which of those is asked. The `awslogs` driver creates the
//! STREAM first; only when that finds no log group, and only when the configuration asked it
//! to create one (`awslogs-create-group`), does it create the GROUP and then the stream
//! again. So a driver asked to create the group is not refused for the group's absence —
//! that is the group it creates — and it IS refused when the role may not create it, which
//! is the step that stands between the configuration and a stream to deliver to; while a
//! driver whose group is already there never makes that call at all, so nothing is decided
//! against the role's permission to make it.
//!
//! A configuration this module decides nothing about is launched: one whose driver is not
//! `awslogs`, one naming no log group, and one whose task names no execution role at all —
//! such a launch states no principal for a policy to be read against, exactly as the other
//! gates over a role's documents in this crate state.
//!
//! # Where it happens
//!
//! In the pass that takes a launch's steps ([`evaluate_image_pulls`]), after the execution
//! resources the agent retrieves as that same role (the registry authentication and the
//! `secrets`, [`super::container_secrets`]): the agent retrieves those for the task and then
//! creates each container, and validating the logger's arguments is part of creating one, so
//! a task that fails both reports the retrieval failure it met first.
//!
//! LOCK ORDER: the cross-service read here takes the CloudWatch Logs store's lock (which
//! takes the IAM store's), holding no other while it does, and hands back owned values — so
//! it is called with NO guard of this service's store held, the same contract
//! [`evaluate_image_pulls`] has.

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_stream_creation::{
    self, DeliveryPrincipal, GroupCreationRefused, StreamCreationRefusal,
};

use super::*;

/// How many attempts at creating the stream the stop's reason reports: the agent states how
/// often it retried the call it could not complete, and this emulator makes that call ONCE —
/// the answer it gets is the emulated service's own and does not change between attempts.
const STREAM_CREATION_ATTEMPTS: usize = 1;

/// The `Task$stoppedReason` of a task whose containers could not be launched because the
/// `awslogs` driver one of them configures could not be initialised: the STREAM the driver
/// delivers to is what it could not create.
///
/// The error CLASS is the agent's `ResourceInitializationError`, which is what Amazon ECS
/// surfaces for a resource it could not bring up before starting a container, worded for the
/// LOGGER the driver could not validate its arguments for; `failure` is the answer CloudWatch
/// Logs gave, which carries the authorization refusal or its own account of the group it does
/// not hold.
pub fn unable_to_initialise_logger_message(failure: &str) -> String {
    uninitialised_logger_message("stream", failure)
}

/// The same stop for a driver that could not create the log GROUP it was configured to create
/// the stream in: the step of the driver's own creation that failed is the one the stop names,
/// and the step the agent RETRIED is still the stream creation that group creation is part of.
pub fn unable_to_create_log_group_message(failure: &str) -> String {
    uninitialised_logger_message("group", failure)
}

/// The stop of a logger whose `resource_kind` — the `stream` the driver delivers to, or the
/// `group` it creates that stream in — could not be created, carrying `failure`, the answer
/// CloudWatch Logs gave.
fn uninitialised_logger_message(resource_kind: &str, failure: &str) -> String {
    format!(
        "ResourceInitializationError: failed to validate logger args: create stream has been \
         retried {STREAM_CREATION_ATTEMPTS} times: failed to create Cloudwatch log \
         {resource_kind}: {failure}: exceeded maximum number of attempts, \
         {STREAM_CREATION_ATTEMPTS}"
    )
}

/// Why the `awslogs` driver of one of `launch`'s containers could not be initialised — the
/// `stoppedReason` of the FIRST configuration whose stream the execution role may not create,
/// whose log group this account does not hold and the driver was not asked to create, or
/// whose log group that role may not create — and `None` when every one of them can be
/// initialised, or describes a delivery this module does not decide against (see the module
/// docs).
///
/// Called with no guard of this service's store held: each verdict is the owning service's,
/// taken under that service's own lock ([`log_stream_creation::serve_log_stream_creation`]).
pub fn uninitialisable_logger<Eng: Send + Sync + 'static>(
    engine: &Eng,
    launch: &TaskLaunch,
) -> Option<String> {
    let task_id = launch
        .task_arn
        .rsplit('/')
        .next()
        .unwrap_or(launch.task_arn.as_str());
    // The principal the agent creates the stream as: an assumed-role session of the task's
    // execution role, named after the task it is for — the same session the other execution
    // resources are retrieved in ([`super::container_secrets::execution_role_session_arn`]).
    let role_arn = launch.execution_role_arn.as_deref()?;
    let session_arn = execution_role_session_arn(role_arn, task_id);
    let principal = DeliveryPrincipal {
        role_arn,
        session_arn: session_arn.as_deref(),
    };
    launch
        .log_targets
        .iter()
        .find_map(|target| uninitialisable_target(engine, target, principal))
}

/// Why the `awslogs` driver could not be initialised for ONE delivery, taking the driver's
/// own steps in the driver's own order (see the module docs), and `None` when it can be.
fn uninitialisable_target<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target: &AwsLogsTarget,
    principal: DeliveryPrincipal<'_>,
) -> Option<String> {
    match log_stream_creation::serve_log_stream_creation(
        engine,
        &target.region,
        &target.log_group,
        &target.log_stream,
        Some(principal),
    ) {
        Ok(()) => None,
        Err(StreamCreationRefusal::NotAuthorized { message }) => Some(
            unable_to_initialise_logger_message(&format!("{ACCESS_DENIED_CLASS}: {message}")),
        ),
        // No group to deliver into: a driver configured to create one creates it, as the same
        // role, and a role that may not create it has reached no stream either.
        Err(StreamCreationRefusal::NotFound { .. }) if target.create_group => {
            match log_stream_creation::serve_log_group_creation(
                engine,
                &target.region,
                &target.log_group,
                Some(principal),
            ) {
                Ok(()) => None,
                Err(GroupCreationRefused { message }) => Some(unable_to_create_log_group_message(
                    &format!("{ACCESS_DENIED_CLASS}: {message}"),
                )),
            }
        }
        Err(StreamCreationRefusal::NotFound { message }) => Some(
            unable_to_initialise_logger_message(&format!("{RESOURCE_NOT_FOUND_CLASS}: {message}")),
        ),
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Where the `awslogs` driver of each container of `task` delivers that container's
    /// output ([`AwsLogsTarget::resolve`]) — the deliveries a launch has to initialise.
    ///
    /// Empty for a task whose revision this store no longer holds, and for one whose
    /// containers configure no `awslogs` delivery: a revision that is gone configures nothing,
    /// so nothing is decided against it — the same reading the task role such a task runs
    /// under is resolved by ([`super::container_secrets`]).
    pub(super) fn launched_task_log_targets(&self, task: &TaskRecord) -> Vec<AwsLogsTarget> {
        let Some(definition) = self.revision_of_arn(&task.task_definition_arn) else {
            return Vec::new();
        };
        // The Region the delivery is addressed in when the configuration names none: the one
        // the task's own ARN carries, which is where it was launched
        // ([`super::container_runtime`] resolves the run's target the same way).
        let launch_region = super::request_arn::RequestArn::parse(&task.task_arn)
            .map(|arn| arn.region.to_string())
            .unwrap_or_default();
        task.containers
            .iter()
            .filter_map(|container| {
                let name = container.name.as_deref()?;
                let stored = definition
                    .container_definitions
                    .iter()
                    .find(|candidate| candidate.name.as_deref() == Some(name))?;
                let container_id = container
                    .container_arn
                    .rsplit('/')
                    .next()
                    .unwrap_or(container.container_arn.as_str());
                AwsLogsTarget::resolve(
                    stored.log_configuration.as_ref(),
                    name,
                    &task.task_id,
                    container_id,
                    &launch_region,
                )
            })
            .collect()
    }
}
