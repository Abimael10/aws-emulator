// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-drs-elasticdisasterrecoveryservice` service: how it claims requests and how it words the
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
//!   but named no modeled operation. This service answers it itself: such a
//!   request names no action for the layer ahead of the service to authorize, so
//!   it is refused as an authorization failure rather than as an operation this
//!   emulator has yet to write (see that method, and `unauthorizable_request`).
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

use aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The error code this service answers a request body that does not satisfy the
/// model's own rules with.
///
/// Elastic Disaster Recovery validates a request body against the schema its model
/// publishes before the request reaches the operation, and states every failure of
/// that check as this one code — not as the modelled `ValidationException`, which is
/// what an OPERATION answers for a rule of its own (see
/// `crate::ops::create_replication_configuration_template`).
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";

/// The whole message that refusal carries: fixed text, naming neither the member that
/// failed nor the rule it failed, because the check is made against the body as a
/// whole rather than member by member.
const INVALID_REQUEST_BODY: &str = "Invalid request body";

/// The error code a request this endpoint cannot place on one of its operations is
/// refused with: the authorization layer in front of this service derives the action to
/// authorize from the request's method and URI, so a request it cannot place names no
/// action, and what it answers is an authorization failure. The code is one this
/// service's model declares (`AccessDeniedException`, `@httpError(403)`), which is what
/// a client reads out of `x-amzn-errortype`.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The whole sentence that refusal carries: fixed text, naming neither the method, the
/// URI nor any operation, because the layer got far enough to look for an action and no
/// further.
const UNAUTHORIZABLE_REQUEST: &str = "Unable to determine service/operation name to be authorized";

/// The status it carries: the request was not authorized, so it is not a report that the
/// emulator has yet to implement something.
const UNAUTHORIZABLE_REQUEST_STATUS: u16 = 403;

/// The media type that refusal's body carries: this service speaks `restJson1`, and the
/// refusal is made ahead of any operation, so it is written here rather than through a
/// modelled error shape.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The refusal of a request that reached this service's endpoint but names no operation
/// of its model.
///
/// The body carries the single lower-case `message` member and nothing else: the shape
/// of this refusal is the authorization layer's, not one of the shapes this service's
/// model declares, and `restJson1` resolves the error class from the
/// `x-amzn-errortype` header. The sentence is a constant, so nothing of the request is
/// echoed back into it.
fn unauthorizable_request() -> RtResponse {
    let body = ::serde_json::json!({ "message": UNAUTHORIZABLE_REQUEST }).to_string();
    let mut response = RtResponse::new(
        StatusCode::try_from(UNAUTHORIZABLE_REQUEST_STATUS)
            .expect("the status is a constant of this module"),
        SdkBody::from(body),
    );
    response.headers_mut().insert("content-type", JSON_MEDIA_TYPE);
    response
        .headers_mut()
        .insert("x-amzn-errortype", ACCESS_DENIED_EXCEPTION);
    response
}

/// The `com-amazonaws-drs-elasticdisasterrecoveryservice` emulated service: the generated [`ServiceDefinition`] plus
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
        // Return `Some(op)` to serve a request the model's routing key cannot name
        // correctly; `None` (the default) lets the generated routing table resolve
        // it. `req` is the whole request, so a decision that depends on the body —
        // the `Action` form field on a query protocol, say — is reachable here.
        let _ = req;
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it.
        //
        // Every such refusal is one answer here: this service checks the request body
        // against its model's schema up front, so an absent required member, a value
        // off a pattern, outside a range, off an enumeration or a list of the wrong
        // length are ONE failure of that check and are all stated the same way — the
        // code and the message below, with the protocol's own 400. A refusal this
        // service makes for a rule its model does not carry is a different answer and
        // is made by the operation, as the modelled `ValidationException`.
        //
        // Anything else (an unauthenticated caller) keeps the framework's wording.
        //
        // It is stated once here rather than per operation because the check is the
        // service's, not any one operation's: the same body-schema refusal is what a
        // caller gets from every operation, and a recorded create whose policy list
        // was one element short is the step between a create refused for a missing
        // member and a create that succeeds — neither could be served correctly while
        // this one was worded as a per-constraint report.
        match refusal {
            Refusal::Invalid(_) => Some(Refused {
                code: BAD_REQUEST_EXCEPTION.to_string(),
                message: INVALID_REQUEST_BODY.to_string(),
            }),
            _ => None,
        }
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. The framework's default for a `rest` service is
        // a 501 `NotImplemented`, which reads as "this emulator has not written that
        // operation yet"; for THIS endpoint that is the wrong claim, because the
        // request named no operation of the service AT ALL.
        //
        // Elastic Disaster Recovery authorizes a request before it dispatches it, and
        // the action it authorizes comes from the request's method and URI: a request
        // those cannot place on one of the service's operations has no action to
        // authorize, so the layer ahead of the service refuses it as the authorization
        // failure it is ([`unauthorizable_request`]) rather than reporting on what the
        // service implements. It is the SAME answer for every such request, whatever
        // method and path it carried, which is why nothing of `req` is read.
        //
        // This says nothing about the operations this emulator has yet to implement: a
        // request that DOES name one is routed to it and answered by its handler, which
        // is where a scaffolded operation's 501 comes from.
        let _ = req;
        Some(unauthorizable_request())
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
