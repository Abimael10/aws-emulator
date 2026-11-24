// SPDX-License-Identifier: CC0-1.0

//! @op ListFlowExecutionEvents
//!
//! `GET /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions/{executionIdentifier}/events
//! -> 200`. Lists the events that occurred during one flow execution: "node inputs and
//! outputs, flow inputs and outputs, condition results, and failure events".
//!
//! REPORTING, so nothing a later caller can observe changes here. The one write on its path
//! is `crate::state::flow_execution::materialise_due_transitions`, which materialises the
//! status the clock has already made due — owed at that instant whether or not anybody calls
//! — so a run whose span has passed reports the events of its ending to whoever reads it
//! first.
//!
//! What it reports is the trace the run left on the execution
//! (`crate::state::flow_execution_event`): the node the input entered at, the field of each
//! node a value left by and arrived at, and the failure of a run that came to one — all read
//! from the stored flow rather than composed here. `eventType` picks which side of the run is
//! listed, and the modelled `FlowExecutionEvents` bound caps one page; a run with more events
//! than fit hands out a `nextToken` into the rest, bound to this execution and this event
//! type. An `executionIdentifier` naming no execution of the addressed alias is the modelled
//! `ResourceNotFoundException`, in either form the shape admits, exactly as a read of one is.
//!
//! ## Input
//!
//! * `eventType` (FlowExecutionEventType, one of: Flow | Node) **required** — The type of
//!   events to retrieve. Specify Node for node-level events or Flow for flow-level events.
//! * `executionIdentifier` (FlowExecutionIdentifier, len ..2048, pattern
//!   ^[a-zA-Z0-9-]{1,36}$|^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10}/execution/[a
//!   …) **required** — The unique identifier of the flow execution.
//! * `flowAliasIdentifier` (FlowAliasIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10}/alias/[0-9a-zA-Z]{10})|(\bTSTALIASID\b|[0-9a-zA-Z]+)$)
//!   **required** — The unique identifier of the flow alias used for the execution.
//! * `flowIdentifier` (FlowIdentifier, len ..2048, pattern
//!   ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$)
//!   **required** — The unique identifier of the flow.
//! * `maxResults` (MaxResults, range 1..1000) — The maximum number of events to return in a
//!   single response. If more events exist than the specified maxResults value, a token is
//!   included in the response so that the remaining results can be retrieved.
//! * `nextToken` (NextToken, len 1..2048, pattern ^\S*$) — A token to retrieve the next set
//!   of results. This value is returned in the response if more results are available.
//!
//! ## Output
//!
//! * `flowExecutionEvents` (FlowExecutionEvents, len ..10) **required** — A list of events
//!   that occurred during the flow execution. Events can include node inputs and outputs,
//!   flow inputs and outputs, condition results, and failure events.
//! * `nextToken` (NextToken, len 1..2048, pattern ^\S*$) — A token to retrieve the next set
//!   of results. This value is returned if more results are available.
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
use crate::state::flow_execution_event as events;
use crate::state::flow_invocation as flows;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListFlowExecutionEventsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListFlowExecutionEventsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentruntime#ListFlowExecutionEvents")]
#[derive(Debug)]
pub enum ListFlowExecutionEventsErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_flow_execution_events::ListFlowExecutionEventsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListFlowExecutionEventsError = crate::scaffolded::Scaffolded<ListFlowExecutionEventsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListFlowExecutionEventsErrorModeled> for ListFlowExecutionEventsError {
    fn from(modeled: ListFlowExecutionEventsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListFlowExecutionEvents` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListFlowExecutionEvents<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListFlowExecutionEvents<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListFlowExecutionEvents<Eng>
{
    type Input = ListFlowExecutionEventsInput;
    type Output = ListFlowExecutionEventsOutput;
    type Error = ListFlowExecutionEventsError;

    const NAME: &'static str = "ListFlowExecutionEvents";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_FLOW_EXECUTION_EVENTS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_FLOW_EXECUTION_EVENTS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(ListFlowExecutionEventsErrorModeled::InternalServerException(
                InternalServerException {
                    message: Some("the flow execution store is unavailable".to_string()),
                    reason: None,
                },
            )
            .into());
        };

        // Decided against the flow alias the execution runs on, before any store is read,
        // exactly as the start, the read and the stop of one are.
        let resource =
            flows::alias_resource_arn(ctx, &input.flow_identifier, &input.flow_alias_identifier);
        if let Err(denied) = crate::state::authorize(&*self.engine, ctx, Self::NAME, &resource) {
            return Err(ListFlowExecutionEventsErrorModeled::AccessDeniedException(denied).into());
        }

        // The side of the run this listing asks for. The member is modelled as an enum, so
        // the boundary validator has already refused a value it does not declare; this names
        // the member a value outside it would be about rather than unwrapping.
        let Some(plane) = events::EventPlane::from_wire(&input.event_type) else {
            return Err(crate::scaffolded::Scaffolded::validation(
                "eventType names no kind of flow execution event",
            ));
        };

        let not_found = || {
            ListFlowExecutionEventsErrorModeled::ResourceNotFoundException(
                executions::execution_not_found(),
            )
        };
        // The flow and the alias the labels name, resolved without reading the build-time
        // plane: this operation addresses a resource of this plane's OWN store, as the read
        // of one execution does.
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
        // The status the clock already owes, materialised before the events are composed, so
        // a run whose span has passed reports the events of its ending.
        executions::materialise_due_transitions(&mut guard, ::std::time::SystemTime::now());
        let key = executions::FlowExecutionKey {
            flow_id: placed.flow_id,
            alias_id: placed.alias_id,
            name: name.to_string(),
        };
        let Some(execution) = guard.flow_executions.get(&key) else {
            return Err(not_found().into());
        };
        let occurred = events::events_of(execution, plane);

        // The cursor is the position of the last event already handed out, so a page resumes
        // strictly after it. The events of one execution are only ever appended to as its run
        // progresses (`crate::state::flow_execution_event`), so that position still names the
        // same event on the next page. It is bound to THIS listing — this execution and this
        // event type — so a token issued for another one is refused rather than silently
        // resuming at an unrelated offset, and it is decoded after the execution is resolved,
        // so a request naming neither a known execution nor a valid token answers 404 first.
        let scope = crate::state::pagination_scope(
            Self::NAME,
            &execution.execution_arn,
            Some(plane.as_wire()),
        );
        let after = match input.next_token.as_deref() {
            None => 0,
            Some(token) => match crate::state::decode_next_token(&scope, token) {
                Some(position) => position as usize,
                // See `state::invalid_next_token`: the service's one invalid-cursor refusal,
                // stated identically by every listing it serves.
                None => {
                    return Err(ListFlowExecutionEventsErrorModeled::ValidationException(
                        crate::state::invalid_next_token(),
                    )
                    .into());
                }
            },
        };

        // `maxResults` is modelled 1..1000 and the boundary validator has already refused a
        // value outside it, but the OUTPUT list is modelled `@length(max: 10)`, so a page
        // carries at most that many however many were asked for
        // (`state::flow_execution_event::MAX_EVENTS_PER_PAGE`), which is also what a request
        // naming no `maxResults` takes.
        let limit = input
            .max_results
            .filter(|requested| *requested > 0)
            .map_or(events::MAX_EVENTS_PER_PAGE, |requested| {
                (requested as usize).min(events::MAX_EVENTS_PER_PAGE)
            });
        let remaining = occurred.len().saturating_sub(after);
        let page: Vec<FlowExecutionEvent> = occurred.into_iter().skip(after).take(limit).collect();
        // A token is emitted ONLY when a further event exists.
        let next_token = (remaining > page.len())
            .then(|| crate::state::encode_next_token(&scope, (after + page.len()) as u64));

        Ok(ListFlowExecutionEventsOutput {
            flow_execution_events: page,
            next_token,
        })
    }
}
