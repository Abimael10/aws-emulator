// SPDX-License-Identifier: CC0-1.0

//! The STATE MACHINE target stage: what a pipe does with the events it is delivering when
//! its `Target` names a Step Functions state machine.
//!
//! How many CALLS a batch comes to is this target's own, as it is for every other kind
//! ([`super::deliver_batch`]): a state machine is started with ONE `input` — "the string
//! that contains the JSON input data for the execution" (`StartExecution$input`) — and
//! `PipeTargetStateMachineParameters` carries no batch member, so the whole batch the target
//! stage was handed is started as ONE execution whose input is the JSON array of those
//! events, exactly as a function target receives one invocation carrying every event. That
//! is what makes the source's `BatchSize` the count of executions a backlog comes to, and
//! the count of elements one execution's input array holds.
//!
//! WHICH call starts it is `InvocationType`'s: "`FIRE_AND_FORGET` - Invoke asynchronously
//! [...] `StartExecution`" and "`REQUEST_RESPONSE` (default) - Invoke synchronously [...]
//! `StartSyncExecution`", the latter noted as "not supported for `STANDARD` state machine
//! workflows" — a state machine type the called service refuses the synchronous start of,
//! which is reported where that refusal is made rather than guessed at here.
//!
//! What the two calls ANSWER about the records of the batch differs, which is the same
//! distinction every other target is read under: an asynchronous start has only accepted the
//! input at the point it answers, so its batch counts as wholly processed, while a
//! synchronous run answers the workflow's own output, which is read as the report of the
//! records it did not process (`service_common::batch_item_failures`) — a run that did not
//! SUCCEED took nothing, so the whole batch stays on its source and the pipe hands it over
//! again.
//!
//! The start is made over the in-process callback into THIS emulator ([`super::Clients`]),
//! so the only state machine a pipe can reach is one this emulator itself holds: no socket
//! is opened and no name is resolved for the ARN a pipe carries.

use super::{CallFailure, Clients, Delivery};
use crate::ops::types::PipeTargetStateMachineParameters;

/// The IAM action the ASYNCHRONOUS delivery to a state machine is authorized under: the API
/// call that starts the execution, in Step Functions' own namespace. The synchronous one is
/// authorized under [`super::START_SYNC_EXECUTION_ACTION`], the call it makes instead.
pub(super) const START_EXECUTION_ACTION: &str = "states:StartExecution";

/// The state machine a pipe's events are started as an execution of.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Machine {
    /// The state machine's ARN as the pipe configured it: what the start names it by, and the
    /// resource the start is authorized on.
    arn: String,
    /// The Region the ARN names — an execution is started in the state machine's own Region.
    region: String,
    /// Whether the start is SYNCHRONOUS. `REQUEST_RESPONSE` is the value the member
    /// documents as the default, so a pipe that configures no invocation type starts the
    /// workflow synchronously.
    synchronous: bool,
}

impl Machine {
    /// The ARN the pipe named this state machine by: the resource the start is authorized on.
    pub(super) fn arn(&self) -> &str {
        &self.arn
    }

    /// The IAM action the start of this execution is authorized under: the API call the
    /// configured invocation type makes.
    pub(super) fn delivery_action(&self) -> &'static str {
        if self.synchronous {
            super::START_SYNC_EXECUTION_ACTION
        } else {
            START_EXECUTION_ACTION
        }
    }
}

/// The state machine the `Target` ARN `arn` names, started as `parameters` configure.
pub(super) fn resolve(
    arn: &str,
    region: &str,
    parameters: Option<&PipeTargetStateMachineParameters>,
) -> Machine {
    Machine {
        arn: arn.to_string(),
        region: region.to_string(),
        synchronous: parameters
            .and_then(|configured| configured.invocation_type.as_deref())
            .is_none_or(|configured| configured != super::FIRE_AND_FORGET_INVOCATION),
    }
}

/// Start ONE execution of the state machine carrying every event of the batch as its input,
/// and answer what that start reported about the records of the batch.
///
/// `records` are the source records the batch was polled from, by the identifier their
/// source names each of them by: what a report of the records the run did not process names
/// them by.
pub(super) async fn start(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    machine: &Machine,
    payloads: &[Delivery<'_>],
    records: &[String],
) -> Result<::service_common::batch_item_failures::BatchReport, CallFailure> {
    // The events as ONE JSON array — the execution's whole input — serialized from the
    // payloads themselves rather than from a second copy of them.
    let events: Vec<&::serde_json::Value> = payloads
        .iter()
        .map(|delivery| delivery.payload().value())
        .collect();
    let arn = machine.arn();
    let input = ::serde_json::to_string(&events).map_err(|error| {
        format!("the batch for the target {arn} is not JSON ({error})")
    })?;
    // How big an input ONE execution carries is the CALLED service's own quota, enforced
    // where it is published: a batch past it is refused by the start below, which fails the
    // target stage and leaves the records on the source, exactly as any other refusal of
    // that call does.
    if !machine.synchronous {
        clients
            .sfn(callback, &machine.region)
            .start_execution()
            .state_machine_arn(arn)
            .input(input)
            .send()
            .await
            .map_err(|error| {
                super::called_service::call_failure(
                    format!(
                        "the target {arn} could not be started ({})",
                        ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                    ),
                    super::called_service::STATES,
                    arn,
                    &error,
                )
            })?;
        // "Invoke asynchronously": Step Functions has only accepted the input at the point
        // it answers, so what the execution then does with it is its own business and the
        // batch counts as wholly processed.
        return Ok(::service_common::batch_item_failures::BatchReport::WholeBatchProcessed);
    }
    let answer = clients
        .sfn(callback, &machine.region)
        .start_sync_execution()
        .state_machine_arn(arn)
        .input(input)
        .send()
        .await
        .map_err(|error| {
            super::called_service::call_failure(
                format!(
                    "the target {arn} could not be run ({})",
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                ),
                super::called_service::STATES,
                arn,
                &error,
            )
        })?;
    // A run that did not SUCCEED took nothing: nothing of the batch is deleted from the
    // source and the pipe hands it over again.
    if answer.status != ::aws_sdk_sfn::types::SyncExecutionStatus::Succeeded {
        return Err(CallFailure {
            detail: format!("the target {arn} ended {}", answer.status.as_str()),
            function_error: Some(answer.status.as_str().to_string()),
            refusal: None,
        });
    }
    // What the run RETURNED, read as its report of the records of the batch it did not
    // process: a response naming none is the whole batch processed, which is what a workflow
    // that knows nothing of the report answers.
    Ok(
        ::service_common::batch_item_failures::read_batch_item_failures(
            answer.output.as_deref().map(str::as_bytes),
            records,
        ),
    )
}
