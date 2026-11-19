// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-comprehendmedical-comprehendmedical-20181030` service: how it claims requests and how it words the
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
//!   generated routing table. Here it serves every modelled operation through the
//!   layer that reports an error's sentence in the member this model's error shapes
//!   declare (`error_message_member`), without changing which operation is routed to.
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

use smithy_server_runtime::{http, ErasedOperation, Refusal, Refused, Service};
use std::sync::Arc;

/// How this service words a request the framework refused before any handler ran. The
/// wording lives there rather than in [`EmulatedService::override_refusal`] so every
/// constrained member of every operation is stated by ONE formatter.
pub(crate) mod validation;

/// How this service refuses a request that names an asynchronous job it cannot find:
/// the modelled error shape, its one fixed sentence, and the status this service answers
/// that refusal with. One place, so no per-id job operation words it on its own.
pub(crate) mod job_refusal;

/// Which body member an error of this service reports its sentence in. The framework's own
/// pre-handler renderer writes a synthetic `message`, while this model's error shapes all
/// declare `Message`, so the one member every error body of this service carries is stated
/// there.
mod error_message_member;

/// The `com-amazonaws-comprehendmedical-comprehendmedical-20181030` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
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
        // Serve the operation the generated routing table names, through the layer that
        // reports an error's sentence in the `Message` member every error shape of this
        // model declares (see `error_message_member`, whose wrapper is
        // `service_common::restated_refusal`'s): the framework's own refusal renderer
        // writes that sentence under its synthetic `message` for every service, while this
        // service names one of its MODELLED error shapes as the code of every refusal.
        // The routing decision itself is NOT overridden — the key is resolved by the
        // model's own protocol — and a request the table cannot serve is declined here, so
        // the framework's routing-error path stays exactly as it was.
        ::service_common::restated_refusal::routes(self.definition(), req).then_some(
            &error_message_member::SERVED_OPERATION_LAYER as &dyn ErasedOperation,
        )
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // Amazon Comprehend Medical does not answer `ValidationException` for a bad
        // value: its client-input error is `InvalidRequestException` ("The request that
        // you made is invalid"), which every operation in the model declares, and text
        // longer than the limit has its own error, `TextSizeLimitExceededException`
        // ("The size of the text you submitted exceeds the size limit"). Both are
        // modelled 400s, which is the status the framework renders a refusal with, so
        // only the code and the message change. The wording is NOT written here: it is
        // `validation::refused`, the one formatter this service states every refused
        // request with.
        validation::refused(refusal)
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
