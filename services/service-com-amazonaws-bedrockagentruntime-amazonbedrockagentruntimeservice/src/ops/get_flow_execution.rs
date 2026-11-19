// SPDX-License-Identifier: CC0-1.0

//! @op GetFlowExecution
//!
//! `GET /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions/{executionIdentifier}
//! -> 200`. Reports one flow execution: "its status, start and end times, and any errors
//! that occurred during execution".
//!
//! REPORTING, so nothing a later caller can observe changes here. The one write on its path
//! is `crate::state::flow_execution::materialise_due_transitions`, which materialises the
//! status the clock has already made due — owed at that instant whether or not anybody calls
//! — so a run whose span has passed is reported `Succeeded` by whoever reads it first.
//!
//! What it reports is the execution's OWN record (`crate::state::flow_execution`): the
//! version the alias routed to when it started, so an alias repointed since does not change
//! what this execution ran, the identifiers it was started with, and — for a run that came
//! to `Failed` — the error of that run. An `executionIdentifier` naming no execution of the
//! addressed alias is the modelled `ResourceNotFoundException`, in either form the shape
//! admits.
//!
//! ## Input
//!
//! * `executionIdentifier` (FlowExecutionIdentifier, len ..2048, pattern
//!   ^[a-zA-Z0-9-]{1,36}$|^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10}/execution/[a
//!   …) **required** — The unique identifier of the flow execution to retrieve.
//! * `flowAliasIdentifier` (FlowAliasIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10})|(\bTSTALIASID\b|[0-9a-zA-Z]+)$)
//!   **required** — The unique identifier of the flow alias used for the execution.
//! * `flowIdentifier` (FlowIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$)
//!   **required** — The unique identifier of the flow.
//!
//! ## Output
//!
//! * `endedAt` (DateTimestamp) — The timestamp when the flow execution ended. This field is
//!   only populated when the execution has completed, failed, timed out, or been aborted.
//! * `errors` (FlowExecutionErrors) — A list of errors that occurred during the flow
//!   execution. Each error includes an error code, message, and the node where the error
//!   occurred, if applicable.
//! * `executionArn` (FlowExecutionIdentifier, len ..2048, pattern
//!   ^[a-zA-Z0-9-]{1,36}$|^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10}/execution/[a
//!   …) **required** — The Amazon Resource Name (ARN) that uniquely identifies the flow
//!   execution.
//! * `flowAliasIdentifier` (FlowAliasIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10})|(\bTSTALIASID\b|[0-9a-zA-Z]+)$)
//!   **required** — The unique identifier of the flow alias used for the execution.
//! * `flowIdentifier` (FlowIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$)
//!   **required** — The unique identifier of the flow.
//! * `flowVersion` (Version, len 1..5, pattern ^(DRAFT|[0-9]{0,4}[1-9][0-9]{0,4})$)
//!   **required** — The version of the flow used for the execution.
//! * `startedAt` (DateTimestamp) **required** — The timestamp when the flow execution
//!   started.
//! * `status` (FlowExecutionStatus, one of: Aborted | Failed | Running | Succeeded |
//!   TimedOut) **required** — The current status of the flow execution. Flow executions time
//!   out after 24 hours.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — The request is denied because of missing access
//!   permissions. Check your permissions and retry your request.
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
// `GetFlowExecutionError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetFlowExecutionError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentruntime#GetFlowExecution")]
#[derive(Debug)]
pub enum GetFlowExecutionErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_flow_execution::GetFlowExecutionError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetFlowExecutionError = crate::scaffolded::Scaffolded<GetFlowExecutionErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetFlowExecutionErrorModeled> for GetFlowExecutionError {
    fn from(modeled: GetFlowExecutionErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetFlowExecution` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetFlowExecution<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetFlowExecution<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetFlowExecution<Eng>
{
    type Input = GetFlowExecutionInput;
    type Output = GetFlowExecutionOutput;
    type Error = GetFlowExecutionError;

    const NAME: &'static str = "GetFlowExecution";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_FLOW_EXECUTION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_FLOW_EXECUTION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(GetFlowExecutionErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some("the flow execution store is unavailable".to_string()),
                    reason: None,
                },
            )
            .into());
        };

        // Decided against the flow alias the execution runs on, before any store is read,
        // exactly as the start of one is.
        let resource = flows::alias_resource_arn(
            ctx,
            &input.flow_identifier,
            &input.flow_alias_identifier,
        );
        if let Err(denied) = crate::state::authorize(&*self.engine, ctx, Self::NAME, &resource) {
            return Err(GetFlowExecutionErrorModeled::AccessDeniedException(denied).into());
        }

        let not_found =
            || GetFlowExecutionErrorModeled::ResourceNotFoundException(executions::execution_not_found());
        // The flow and the alias the labels name, resolved without reading the build-time
        // plane: this operation addresses a resource of this plane's OWN store, and an
        // execution of a flow that plane does not hold is one nothing here started.
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

        let mut guard = state.lock();
        // The status the clock already owes, materialised before the answer is composed.
        executions::materialise_due_transitions(&mut guard, ::std::time::SystemTime::now());
        let key = executions::FlowExecutionKey {
            flow_id: placed.flow_id,
            alias_id: placed.alias_id,
            name: name.to_string(),
        };
        let Some(execution) = guard.flow_executions.get(&key) else {
            return Err(not_found().into());
        };
        Ok(GetFlowExecutionOutput {
            execution_arn: execution.execution_arn.clone(),
            status: execution.status.as_wire().to_string(),
            started_at: execution.started_at,
            ended_at: execution.ended_at,
            // The error of a run that HAS come to `Failed`: nothing is reported while the
            // execution is still running, because the failure of a run is something that
            // "occurred during the flow execution" and that one has not finished. The
            // modelled `FlowExecutionErrorType` carries one code (`ExecutionTimedOut`),
            // which is not what such a run came to, so the error states the message of the
            // failure and no code it would not be — every member of `FlowExecutionError` is
            // optional. A run that delivered the flow's outputs reports no error at all
            // rather than an empty list the service does not send.
            errors: execution
                .failure
                .as_ref()
                .filter(|_| execution.status == executions::FlowExecutionStatus::Failed)
                .map(|message| {
                    vec![FlowExecutionError {
                        node_name: None,
                        error: None,
                        message: Some(message.clone()),
                    }]
                }),
            flow_alias_identifier: execution.alias_id.clone(),
            flow_identifier: execution.flow_id.clone(),
            flow_version: execution.flow_version.clone(),
        })
    }
}
