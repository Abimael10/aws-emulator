// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-endusermessaging-endusermessaging` service: how it claims requests and how it words the
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
//!   before any handler ran. This service states that refusal in its own modelled
//!   `ValidationException` instead, which carries a `fieldList` the framework's
//!   two-field refusal body cannot ([`validation`]), so the seam itself keeps the
//!   framework's wording for the one refusal class that is not about a member.
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
//! One data plane is mounted here as well: the presigned `mediaDownloadUrl` an attribute
//! read hands out names no modelled operation, so it is claimed through
//! [`Service::override_operation`] and served by [`brand_profile_attribute_media`].
//!
//! Every MODELLED operation is claimed through that same seam too, so a request refused
//! before its handler runs is stated in this service's own modelled shape — see
//! [`validation`], which hands every request it does not refuse straight to the generated
//! operation.

use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

mod brand_profile_attribute_media;
mod validation;

/// The `com-amazonaws-endusermessaging-endusermessaging` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// Serves the presigned addresses an attribute read hands out for the attribute's
    /// media. Claimed through [`Service::override_operation`], which hands it the
    /// requests it claims (see [`brand_profile_attribute_media`]).
    attribute_media: brand_profile_attribute_media::AttributeMediaRoute<Eng>,
    /// One modelled-refusal wrapper per modelled operation, keyed by the routing key the
    /// model names that operation by ([`smithy_server_runtime::Operation::NAME`], the same
    /// key the generated routing table uses). Built once here, never per request; each
    /// entry is zero-sized, so this is a table of NAMES rather than a second set of
    /// operations — the operation that serves a request is still the one the generated
    /// table built from the shared engine.
    refusals: std::collections::HashMap<&'static str, Box<dyn ErasedOperation>>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            attribute_media: brand_profile_attribute_media::AttributeMediaRoute::new(engine),
            refusals: validation::modelled_validation_table::<Eng>(),
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
        //
        // The fetch of an attribute's MEDIA, which the generated table cannot serve: the
        // model binds no operation to that path, because the media is the artifact
        // `mediaDownloadUrl` points at rather than an API call (see
        // [`brand_profile_attribute_media`]).
        if self.attribute_media.claims(req).is_some() {
            return Some(&self.attribute_media as &dyn ErasedOperation);
        }
        // Every MODELLED operation is served through its modelled-refusal wrapper, so a
        // request this service refuses before the handler runs is stated in the service's
        // OWN modelled `ValidationException` — with the `fieldList` the model declares —
        // instead of the framework's two-field refusal body. The wrapper hands every request
        // it does not refuse to the generated operation in the routing table, which remains
        // the only thing that serves one.
        //
        // Keyed by the protocol's own routing key, so a request the model names no operation
        // for resolves to nothing here and keeps the framework's routing failure.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.refusals.get(key).map(|wrapper| &**wrapper)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it. This service does not reword one HERE, because the
        // answer it owes such a request is a modelled `ValidationException` whose
        // `fieldList` names every member at fault, and this seam carries an error
        // code and a message and nothing else: the refusal is stated in that shape
        // by [`validation`], in front of every modelled operation, and the few
        // requests that reach this seam at all (a body no codec can read, an
        // unauthenticated caller) keep the framework's own code and wording.
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
