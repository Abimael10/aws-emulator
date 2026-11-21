// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-pinpoint-pinpoint` service: how it claims requests and how it words the
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
//! ## Naming the request a failure answers
//!
//! Every exception shape of this model is a structure of TWO members, `Message` and
//! `RequestID` ("The unique identifier for the request or response.") —
//! `smithy_query(query="shape",shape="NotFoundException,BadRequestException,ForbiddenException,InternalServerErrorException,MethodNotAllowedException,PayloadTooLargeException,TooManyRequestsException")`
//! lists exactly those two on each of the seven — so every failure this service answers
//! names the request it answers in [`REQUEST_ID_MEMBER`] of its JSON error body.
//!
//! The id belongs to the RESPONSE and not to the resource the request named: it is a
//! correlation id a client logs and an SDK's retry and telemetry layers read, so it is
//! minted per response, carries nothing out of the request, and two failures never report
//! one id. Nothing a handler holds is it, and a request refused BEFORE any handler runs
//! (a missing required member, a value outside a modelled bound) or refused with no
//! handler to run at all (a path naming no modelled operation, a transport failure
//! detected up front) has no handler to set it — which is why it is reported at the
//! layers every response of this service passes through rather than per exception.
//!
//! The rule itself is not restated here: `service_common::query_error_envelope` owns it
//! for every emulated service, and this file chooses only the report this protocol takes
//! (the body member — restJson1 has no envelope element for the id) and wires it at the
//! two layers:
//!
//! * [`RequestIdReportedOperation`], the wrapper [`Service::override_operation`] hands the
//!   router in place of each modelled operation, covers every failure an operation answers
//!   — a modelled error, and a constraint refusal raised while that operation was being
//!   served;
//! * [`RequestIdEnvelopeDefinition::reporting`], the definition wrapper
//!   [`Service::definition`] hands back, covers a response NO operation produced: a
//!   routing failure, a transport failure detected up front, and a pre-dispatch
//!   authentication refusal.
//!
//! A SUCCESS is left exactly as the renderer wrote it: its body is the modelled output,
//! which states whatever the model declares for it. Nothing else about a failure changes
//! either — the status, the error code and every other byte of the body stay as the
//! renderer wrote them.

use service_common::query_error_envelope::{
    with_request_id_error_member_named, RequestIdEnvelopeDefinition, RequestIdReportedOperation,
};
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The member this service's JSON error bodies name the request they answer in: the
/// `RequestID` member every exception shape of this model declares beside `Message`
/// (see the module docs for the `smithy_query` call that lists them).
const REQUEST_ID_MEMBER: &str = "RequestID";

/// How one response of this service reports the request it answers — the one report both
/// layers below are handed, so a failure an operation produced and a failure no operation
/// produced name their request the same way.
fn with_request_id_member(
    resp: ::aws_smithy_runtime_api::http::Response,
) -> ::aws_smithy_runtime_api::http::Response {
    with_request_id_error_member_named(resp, REQUEST_ID_MEMBER)
}

/// The `com-amazonaws-pinpoint-pinpoint` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    /// The generated entry, with its framework-level error renderers wrapped so a failure
    /// NO operation produced still names the request it answers (see the module docs).
    entry: RequestIdEnvelopeDefinition<crate::ServiceDefinition<Eng>>,
    /// Serves the modelled operation this service's own routing table names for a request,
    /// so every failure an operation answers names the request it answers. Held, not built
    /// per request: `override_operation` hands back a reference to it.
    served: RequestIdReportedOperation,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: RequestIdEnvelopeDefinition::reporting(
                crate::ServiceDefinition::new(engine),
                with_request_id_member,
            ),
            served: RequestIdReportedOperation::new(with_request_id_member),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry: what is
        // handed back is the shared `RequestIdEnvelopeDefinition` over that entry, which
        // reads identity, protocol and the routing table straight off it and adds the
        // request-id member to the three framework-level error renderers (see the module
        // docs).
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
        // Return `Some(op)` to serve a request the model's routing key cannot name
        // correctly; `None` (the default) lets the generated routing table resolve
        // it. `req` is the whole request, so a decision that depends on the body —
        // the `Action` form field on a query protocol, say — is reachable here.
        //
        // NOT a re-route: the operation served is the one this service's own routing
        // table names for the request, reached through the shared
        // `RequestIdReportedOperation` — the one place every operation's response passes,
        // so no operation can answer a failure without naming the request it answered. A
        // request the table does not answer declines here exactly as it would there,
        // leaving the router's routing-error path (reported by the entry above) unchanged.
        self.entry
            .routes(req)
            .then_some(&self.served as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it. Returning `None` keeps the framework's default
        // wording. To state a refusal the way the emulated service states it,
        // replace this body with a `match refusal { .. }` that returns
        // `Some(Refused { code, message })` for the cases you reword and `None`
        // for the rest. Replace BOTH the code and the message: a client branches
        // on the code, so a message without its matching code is worse than
        // leaving both alone.
        let _ = refusal;
        None
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
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
