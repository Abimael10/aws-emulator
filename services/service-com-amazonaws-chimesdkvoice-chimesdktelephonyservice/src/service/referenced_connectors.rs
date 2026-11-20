// SPDX-License-Identifier: CC0-1.0

//! The Voice Connectors a group's routing REFERENCES, resolved before the rest of the
//! request's input constraints are enforced.
//!
//! # The rule
//!
//! `CreateVoiceConnectorGroup` carries the connectors inbound calls are routed to in its
//! body (`VoiceConnectorItems`), and the wire resolves those references FIRST: a request
//! naming a `VoiceConnectorId` the account does not hold is answered as the lookup failure
//! it is — HTTP 404, the `NotFound` code, and the identifier that resolved to nothing — even
//! when another value of the same request is outside a bound its own member declares. A
//! caller is told which of the two it is: the reference does not exist, so there is nothing
//! to fix about the request's form.
//!
//! # Why a seam and not a line in the handler
//!
//! The framework refuses every modelled constraint violation BEFORE any handler runs, so a
//! request that both names an absent connector and carries, say, an item priority outside
//! the modelled range never reaches `handle` at all: the walk answers 400 first, which is
//! the opposite order from the wire's. [`Service::override_operation`] is the one hook that
//! sees the whole request ahead of that walk, which is why the reference is resolved here.
//!
//! Nothing else about the request is judged here: the request is read through the PROTOCOL's
//! own binding against the operation's model-derived input schema (deserialization does not
//! enforce constraints, which is what lets a reference be resolved ahead of them), and every
//! request this layer does not answer is served by the operation the service's own routing
//! table names for it, so the framework still owns deserialize -> validate -> handle ->
//! serialize and every status, code and message it answers with.
//!
//! The QUESTION asked of the store is the one the handler asks
//! ([`crate::state::ComAmazonawsChimesdkvoiceChimesdktelephonyserviceState::first_absent_voice_connector`]),
//! and the REFUSAL is the one payload that states it
//! ([`crate::scaffolded::unresolved_reference`]), so this layer and the handler cannot
//! disagree about either. The handler keeps its own check: it is that operation's contract
//! that a stored group only ever routes to connectors that exist.

use ::std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::query_error_envelope::RequestIdReportedOperation;
use ::smithy_server_runtime::{
    BoxFuture, DeserializableStruct, ErasedOperation, Operation, OperationContext, Service,
};

use crate::scaffolded::unresolved_reference::{
    UnresolvedReference, UNRESOLVED_REFERENCE_SCHEMA, UNRESOLVED_REFERENCE_SHAPE,
    UNRESOLVED_REFERENCE_STATUS,
};

/// Serves every modelled operation of this service, resolving a group's connector
/// references first.
///
/// Holds the shared engine because the question is about STORED state: whether the
/// connectors a group would route to exist. It holds no operation of its own — the one it
/// delegates to is the one the routing table names for the request, reached through the
/// shared request-id report so every answer still names the request it answered.
pub(super) struct ResolvedReferenceOperation<Eng> {
    engine: Arc<Eng>,
    served: RequestIdReportedOperation,
}

impl<Eng: Send + Sync + 'static> ResolvedReferenceOperation<Eng> {
    /// Serve this service's operations from `engine`, reporting the request id through
    /// `served`.
    pub(super) fn new(engine: Arc<Eng>, served: RequestIdReportedOperation) -> Self {
        Self { engine, served }
    }

    /// The first `VoiceConnectorId` this request's group routing references that no stored
    /// connector holds, or `None` when there is nothing for this layer to answer.
    ///
    /// `None` covers every request that is not a group create, a body the protocol cannot
    /// bind to that operation's input (an absent `@required` member among them, which the
    /// framework's own refusal answers), a group that references no connector, and a group
    /// whose every reference resolves.
    fn unresolved_reference(&self, svc: &dyn Service, req: &RtRequest) -> Option<String> {
        let protocol = svc.definition().protocol();
        let key = protocol.routing_key(req).ok()?;
        if key
            != <crate::ops::create_voice_connector_group::CreateVoiceConnectorGroup<Eng> as Operation>::NAME
        {
            return None;
        }
        let mut deserializer = protocol
            .deserialize_input(
                req,
                crate::ops::types::CREATE_VOICE_CONNECTOR_GROUP_INPUT_SCHEMA,
            )
            .ok()?;
        let input =
            crate::ops::types::CreateVoiceConnectorGroupInput::deserialize(&mut *deserializer)
                .ok()?;
        let items = input.voice_connector_items?;
        let state = crate::state::service_state(&*self.engine)?;
        let store = state.lock();
        store.first_absent_voice_connector(items.iter().map(|item| item.voice_connector_id.as_str()))
    }

    /// The answer for a group whose routing references `absent`.
    ///
    /// Rendered through this service's own protocol, so the refusal carries the same
    /// envelope and the same `x-amzn-errortype` header as every other error it answers, and
    /// it names the request it answers through the same report every other response passes
    /// through.
    fn refused(&self, svc: &dyn Service, req: &RtRequest, absent: &str) -> RtResponse {
        let payload = UnresolvedReference::new(absent);
        super::report_request_id(
            svc.definition()
                .protocol()
                .serialize_error(
                    &payload,
                    &UNRESOLVED_REFERENCE_SCHEMA,
                    UNRESOLVED_REFERENCE_SHAPE,
                    UNRESOLVED_REFERENCE_STATUS,
                )
                .unwrap_or_else(|e| svc.definition().framework_error_response(e, req)),
        )
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for ResolvedReferenceOperation<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // Scoped so the borrowed definition — which is not `Sync`, while this future
            // is — is dropped before the delegation below awaits.
            let refusal = {
                self.unresolved_reference(svc, req)
                    .map(|absent| self.refused(svc, req, &absent))
            };
            match refusal {
                Some(answer) => answer,
                None => self.served.run(svc, req, ctx).await,
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so resolving a
        // reference ahead of the walk cannot weaken the schemes that sign the request.
        None
    }
}
