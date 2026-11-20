// SPDX-License-Identifier: CC0-1.0

//! The layer that COUNTS every call this service answers into the usage telemetry it publishes
//! about its own API ([`crate::state::api_usage`]).
//!
//! One wrapper round each dispatch this service hands the router, rather than a line in each
//! handler: the count is owed by every answer, whichever layer built it — the handler, a
//! modelled error, the authorization refusal, or a constraint refusal raised before any
//! handler ran — so it is taken where every request passes.
//!
//! The operation counted is named by the routing key this service's own protocol resolves —
//! the modelled operation's shape name, the same key the routing table is read with — so the
//! series a reader asks for by operation name is the series the call was counted in. A request
//! that names no modelled operation is counted into nothing: there is no operation for the
//! series to be about, and the framework's routing failure answers it.
//!
//! The count is taken AFTER the answer is built and is written off it, so the response a
//! caller sees is the wrapped dispatch's own and a count that could not be stored costs the
//! telemetry a datapoint and never the answer. Counting is a pair of appends to the metric
//! store and nothing more: a metered call delivers nothing and waits for nothing.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};
use ::std::sync::Arc;

/// Serves the dispatch this service wraps and counts the call it answered.
pub struct Metered<Eng, O> {
    /// The shared engine, so the count reaches the metric store every other metric this
    /// service publishes lands in ([`crate::state::emf`]).
    engine: Arc<Eng>,
    /// The dispatch whose answer this one is.
    operation: O,
}

impl<Eng, O> Metered<Eng, O> {
    /// Meter `operation`, already constructed from the shared engine.
    pub fn new(engine: Arc<Eng>, operation: O) -> Self {
        Self { engine, operation }
    }
}

impl<Eng, O> ErasedOperation for Metered<Eng, O>
where
    Eng: Send + Sync + 'static,
    O: ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The protocol is read, used and dropped inside this one statement:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the awaits
            // below would make this future non-`Send`.
            let operation = svc
                .definition()
                .protocol()
                .routing_key(req)
                .ok()
                .map(ToString::to_string);
            let response = self.operation.run(svc, req, ctx).await;
            if let Some(operation) = operation {
                crate::state::api_usage::count_api_call(
                    &*self.engine,
                    &operation,
                    response.status().as_u16(),
                );
            }
            response
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapped dispatch's own answer: wrapping must not change which schemes sign an
        // operation.
        self.operation.auth_schemes()
    }
}
