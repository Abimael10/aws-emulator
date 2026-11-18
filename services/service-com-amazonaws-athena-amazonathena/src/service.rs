// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-athena-amazonathena` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.
//!
//! Two of those seams are wired here, and both to the ONE rule in
//! [`input_validation`]: a request that breaks a modelled input constraint is refused as
//! this service's own `InvalidRequestException`. `override_refusal` chooses that code and
//! AWS's validation sentence, and `override_operation` hands back the wrapper that completes
//! the body the framework renders from them — the seam that sees a whole rendered response,
//! which is the only one that can.
//!
//! ## Naming the request a response answers
//!
//! Every response this service sends names the request it answered, in the response header
//! this protocol reports that id in (`x-amzn-RequestId`) — a success and a failure alike. It
//! is a property of the RESPONSE, not of any modelled shape: no output or error shape of
//! this model declares it, so no handler can set it, and the framework's awsJson1_1
//! renderers do not write it. A client reads it as the failed call's request id, and a layer
//! that RELAYS such a failure (a Step Functions `Task` state reporting an SDK exception as
//! its `cause`) carries that id through into the sentence it reports.
//!
//! That rule is not stated here: `service_common::query_error_envelope` owns it for every
//! emulated service, and this file only chooses the report this protocol takes (the header
//! alone — awsJson1_1 has no envelope element for the id) and wires it at the two layers
//! every response of this service passes through:
//!
//! * [`RequestIdReportingOperation::header`] composed AROUND the refusal-body wrapper, which
//!   is what `override_operation` hands the router in place of each modelled operation, so
//!   the report covers everything an operation answers — a success, a modelled error, the
//!   scaffold's 501, and a constraint refusal raised while that operation was being served;
//! * [`RequestIdEnvelopeDefinition::header_only`], the definition wrapper
//!   [`Service::definition`] hands back, covers a response NO operation produced: a routing
//!   failure, a transport failure detected up front, and a pre-dispatch authentication
//!   refusal.
//!
//! Nothing else about a response changes: the status, the other headers and every byte of
//! the body stay as the renderer wrote them.

pub mod input_validation;

/// Serves the signed addresses `CreatePresignedNotebookUrl` hands out for a session's
/// notebook. See the module docs of that file: the notebook file is an artifact an API
/// member points at rather than an API call, so no modelled route can serve it.
pub(crate) mod notebook_editor;

use service_common::query_error_envelope::{
    RequestIdEnvelopeDefinition, RequestIdReportingOperation,
};
use service_common::restated_refusal::RestatedRefusalOperation;
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The `com-amazonaws-athena-amazonathena` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    /// The generated entry, with its framework-level error renderers wrapped so a response
    /// no operation produced still names the request it answers.
    entry: RequestIdEnvelopeDefinition<crate::ServiceDefinition<Eng>>,
    /// Serves every modelled operation: the refusal-body wrapper this service's own rule
    /// needs ([`input_validation::REFUSAL_BODY`]), composed inside the request-id report so
    /// whatever that wrapper answered names the request it answered. Held, not built per
    /// request: `override_operation` returns a reference to it.
    refusal_body: RequestIdReportingOperation<RestatedRefusalOperation>,
    /// Serves the signed addresses `CreatePresignedNotebookUrl` hands out for a session's
    /// notebook. Claimed through [`Service::override_operation`], which hands it the
    /// requests it claims (see [`notebook_editor`]).
    notebook_editor: notebook_editor::NotebookEditorRoute<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: RequestIdEnvelopeDefinition::header_only(crate::ServiceDefinition::new(
                Arc::clone(&engine),
            )),
            refusal_body: RequestIdReportingOperation::header(input_validation::REFUSAL_BODY),
            notebook_editor: notebook_editor::NotebookEditorRoute::new(engine),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry: what is
        // handed back is the shared `RequestIdEnvelopeDefinition`, which reads identity,
        // protocol and the routing table straight off that entry and names the request on
        // the answers no operation produced (see the module docs).
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // Every request the generated routing table can serve is claimed for the
        // wrapper that completes a refusal body ([`input_validation::REFUSAL_BODY`]):
        // it serves the very operation that table names, so the framework keeps the
        // whole deserialize -> validate -> handle -> serialize walk and every status and
        // code it answers with, and only the BODY of a refusal the framework rendered is
        // completed afterwards. A request the table cannot serve is declined here, so
        // the router answers it as the unroutable request it is.
        //
        // That wrapper is served INSIDE the shared request-id report, so no operation can
        // answer without naming the request it answered (see the module docs).
        if service_common::restated_refusal::routes(self.definition(), req) {
            return Some(&self.refusal_body);
        }

        // The open of a session's NOTEBOOK, which the generated table cannot serve: the model
        // binds no operation to that path, because the notebook file is the artifact a
        // `CreatePresignedNotebookUrlResponse` member points at rather than an API call (see
        // [`notebook_editor`]).
        if self.notebook_editor.claims(req).is_some() {
            return Some(&self.notebook_editor as &dyn ErasedOperation);
        }
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it. A request that breaks a modelled input constraint is
        // refused the way THIS service refuses a bad request — code and sentence
        // together, since a client branches on the code — which
        // [`input_validation`] states; every other class keeps the framework's own
        // wording.
        input_validation::stated_refusal(refusal)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501) through the shared
        // `RequestIdEnvelopeDefinition`, which names the request that failure answers.
        // Return `Some(resp)` to word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}
