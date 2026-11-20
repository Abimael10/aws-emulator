// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-neptunedata-amazonneptunedataplane` service: how it claims requests and how it words the
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
//! ## The media type this service announces
//!
//! The model declares the `restJson1` protocol trait, whose generic default response media
//! type is the bare `application/json` the framework's renderers stamp, while this data
//! plane's recorded responses announce `application/json; charset=UTF-8` — the media type
//! AND the encoding its body is in — on every status, a success and a refusal alike. The
//! media type a client reads is what it selects a decoder by, so this service re-announces
//! that ONE header value and leaves every other response header alone.
//!
//! The rule, the one function that applies it and the operation-seam wrapper that reaches
//! the refusal tail with it all live in [`service_common::response_media_type`];
//! [`RESPONSE_MEDIA_TYPE`] is this service's own pair, and the seams it is applied at are
//! `override_operation` (every routed request, including the refusal the framework renders
//! inside the request walk), `override_routing_error` and `override_framework_error`.
//!
//! One response is announced by the framework alone, because no author seam reaches it: the
//! authentication refusal, which the router renders through
//! `definition().refusal_response(..)` before this service is asked anything.

use service_common::response_media_type::{ResponseMediaType, ResponseMediaTypeOperation};
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The media type the `restJson1` protocol trait this service's model declares implies,
/// which the framework's renderers stamp on every response — the one value
/// [`RESPONSE_MEDIA_TYPE`] replaces.
const REST_JSON_MEDIA_TYPE: &str = "application/json";

/// The media type this data plane's recorded responses announce: the protocol's own JSON
/// media type WITH the `charset` parameter stating the encoding of the body (see the module
/// header).
const ANNOUNCED_MEDIA_TYPE: &str = "application/json; charset=UTF-8";

/// The pair this service's responses are re-announced across. Named once, so every seam that
/// re-announces reads the same two values.
pub const RESPONSE_MEDIA_TYPE: ResponseMediaType =
    ResponseMediaType::new(REST_JSON_MEDIA_TYPE, ANNOUNCED_MEDIA_TYPE);

/// The `com-amazonaws-neptunedata-amazonneptunedataplane` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The wrapper every routable request is served through, so a refusal raised inside the
    /// request walk announces this service's media type too. Held in a field, built once,
    /// because [`Service::override_operation`] hands back a borrow of it.
    media_type: ResponseMediaTypeOperation,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
            media_type: ResponseMediaTypeOperation::new(RESPONSE_MEDIA_TYPE),
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
        // Every request the generated routing table can serve is served through the shared
        // media-type wrapper, which delegates to that same table entry and then re-announces
        // the media type of what it answered — reaching the refusal the framework renders
        // inside the request walk (see the module header). The routing stays the model's: a
        // request whose routing key names no modelled operation is declined here, so the
        // framework answers it as the unroutable request it is, through
        // `override_routing_error` below, which re-announces that answer too.
        service_common::response_media_type::routes(&self.entry, req)
            .then_some(&self.media_type as _)
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
        // The router calls this when a request reached this service but named no modeled
        // operation — neither `override_operation` nor the generated routing table resolved
        // it. The framework's protocol-correct default for this service's envelope is kept
        // word for word, status, code and body alike; only the media type is re-announced,
        // so an unroutable request is answered under the same media type as a routed one.
        Some(RESPONSE_MEDIA_TYPE.announce(self.entry.routing_error_response(req)))
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front — before
        // (or instead of) naming an operation — such as a malformed `aws-chunked` body. The
        // entry's protocol-correct rendering is kept word for word; only the media type is
        // re-announced. The whole body is the shared rule's: that seam is handed the error
        // ALONE, with no request to resolve an envelope from, and the rebuild-and-render
        // that answer takes lives once in
        // [`service_common::response_media_type::reannounced_framework_error`].
        service_common::response_media_type::reannounced_framework_error(
            RESPONSE_MEDIA_TYPE,
            error,
        )
    }
}
