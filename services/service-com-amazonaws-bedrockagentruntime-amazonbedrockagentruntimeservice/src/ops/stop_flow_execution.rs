// SPDX-License-Identifier: CC0-1.0

//! @op StopFlowExecution
//!
//! `POST /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions/{executionIdentifier}/stop
//! -> 200`. Stops one running flow execution: it "prevents further processing of the flow and
//! changes the execution status to Aborted".
//!
//! The execution is this plane's own resource (`crate::state::flow_execution`), so this is
//! the one operation that moves a started execution to a terminal status the clock did not
//! owe. Two rules follow, and both are the store's:
//!
//! * the status the clock has already made due is materialised FIRST, so a stop never
//!   reverses a run that had already finished when it arrived;
//! * an execution that has already reached a terminal status has no run left to stop, and is
//!   refused with the modelled `ConflictException` (HTTP 409) — including the second of two
//!   stops of the same execution, whichever of them arrived first.
//!
//! ## Input
//!
//! * `executionIdentifier` (FlowExecutionIdentifier, len ..2048, pattern
//!   ^[a-zA-Z0-9-]{1,36}$|^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10}/execution/[a
//!   …) **required** — The unique identifier of the flow execution to stop.
//! * `flowAliasIdentifier` (FlowAliasIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10})|(\bTSTALIASID\b|[0-9a-zA-Z]+)$)
//!   **required** — The unique identifier of the flow alias used for the execution.
//! * `flowIdentifier` (FlowIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$)
//!   **required** — The unique identifier of the flow.
//!
//! ## Output
//!
//! * `executionArn` (FlowExecutionIdentifier, len ..2048, pattern
//!   ^[a-zA-Z0-9-]{1,36}$|^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10}/execution/[a
//!   …) — The Amazon Resource Name (ARN) that uniquely identifies the flow execution that was
//!   stopped.
//! * `status` (FlowExecutionStatus, one of: Aborted | Failed | Running | Succeeded |
//!   TimedOut) **required** — The updated status of the flow execution after the stop
//!   request. This will typically be ABORTED if the execution was successfully stopped.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — The request is denied because of missing access
//!   permissions. Check your permissions and retry your request.
//! * `BadGatewayException` (HTTP 502) — There was an issue with a dependency due to a server
//!   issue. Retry your request.
//! * `ConflictException` (HTTP 409) — There was a conflict performing an operation. Resolve
//!   the conflict and retry your request.
//! * `DependencyFailedException` (HTTP 424) — There was an issue with a dependency. Check the
//!   resource configurations and retry the request.
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry your
//!   request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource Amazon Resource Name
//!   (ARN) was not found. Check the Amazon Resource Name (ARN) and try your request again.
//! * `ThrottlingException` (HTTP 429) — The number of requests exceeds the limit. Resubmit
//!   your request later.
//! * `ValidationException` (HTTP 400) — Input validation failed. Check your request
//!   parameters and retry the request.

#[allow(unused_imports)]
use super::types::*;

use crate::state::flow_execution as executions;
use crate::state::flow_invocation as flows;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `StopFlowExecutionError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `StopFlowExecutionError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentruntime#StopFlowExecution")]
#[derive(Debug)]
pub enum StopFlowExecutionErrorModeled {
    AccessDeniedException(AccessDeniedException),
    BadGatewayException(BadGatewayException),
    ConflictException(ConflictException),
    DependencyFailedException(DependencyFailedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::stop_flow_execution::StopFlowExecutionError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type StopFlowExecutionError = crate::scaffolded::Scaffolded<StopFlowExecutionErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<StopFlowExecutionErrorModeled> for StopFlowExecutionError {
    fn from(modeled: StopFlowExecutionErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `StopFlowExecution` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct StopFlowExecution<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> StopFlowExecution<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for StopFlowExecution<Eng>
{
    type Input = StopFlowExecutionInput;
    type Output = StopFlowExecutionOutput;
    type Error = StopFlowExecutionError;

    const NAME: &'static str = "StopFlowExecution";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::STOP_FLOW_EXECUTION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::STOP_FLOW_EXECUTION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(StopFlowExecutionErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some("the flow execution store is unavailable".to_string()),
                    reason: None,
                },
            )
            .into());
        };

        // Decided against the flow alias the execution runs on, before any store is read,
        // exactly as the start and the read of one are.
        let resource = flows::alias_resource_arn(
            ctx,
            &input.flow_identifier,
            &input.flow_alias_identifier,
        );
        if let Err(denied) = crate::state::authorize(&*self.engine, ctx, Self::NAME, &resource) {
            return Err(StopFlowExecutionErrorModeled::AccessDeniedException(denied).into());
        }

        let not_found = || {
            StopFlowExecutionErrorModeled::ResourceNotFoundException(
                executions::execution_not_found(),
            )
        };
        let Ok(placed) = flows::placement(
            &*self.engine,
            ctx,
            &input.flow_identifier,
            &input.flow_alias_identifier,
        ) else {
            return Err(not_found().into());
        };
        let Some(name) = executions::execution_name_from_identifier(
            &input.execution_identifier,
            &placed.flow_id,
            &placed.alias_id,
        ) else {
            return Err(not_found().into());
        };

        let stopped_at = ::std::time::SystemTime::now();
        let mut guard = state.lock();
        // The status the clock already owes comes first, so a run that had finished before
        // this stop arrived stays finished and this call conflicts with it.
        executions::materialise_due_transitions(&mut guard, stopped_at);
        let key = executions::FlowExecutionKey {
            flow_id: placed.flow_id,
            alias_id: placed.alias_id,
            name: name.to_string(),
        };
        let Some(execution) = guard.flow_executions.get_mut(&key) else {
            return Err(not_found().into());
        };
        if execution.status.is_terminal() {
            return Err(StopFlowExecutionErrorModeled::ConflictException(
                executions::execution_not_running(&execution.name, execution.status),
            )
            .into());
        }
        execution.status = executions::FlowExecutionStatus::Aborted;
        execution.ended_at = Some(::aws_smithy_types::DateTime::from(stopped_at));
        // The response is the record as it now stands, so a caller reads back exactly the
        // status the store holds.
        Ok(StopFlowExecutionOutput {
            execution_arn: Some(execution.execution_arn.clone()),
            status: execution.status.as_wire().to_string(),
        })
    }
}
