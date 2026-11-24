// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-chimesdkmeetings-chimemeetingssdkservice` service: how it claims requests and how it words the
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

use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// What answers when a client dials one of the addresses a meeting's `MediaPlacement`
/// published to it. Mounted below: claimed in `can_handle` and served from
/// `override_operation`.
pub mod media_plane;

/// This service's own `aws.api#service$endpointPrefix`, the name that identifies it
/// among the six services that sign as `chime`.
///
/// Source: `smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/\
/// smithy-model/com-amazonaws-chimesdkmeetings-chimemeetingssdkservice.json")` —
/// `awsService.endpointPrefix`.
const OWN_ENDPOINT_PREFIX: &str = "meetings-chime";

/// The `com-amazonaws-chimesdkmeetings-chimemeetingssdkservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The media plane, built once because `override_operation` hands back a reference to it.
    media_plane: media_plane::MediaPlane<Eng>,
    /// The one shared engine, which the media plane's CLAIM reads the store through:
    /// `can_handle` is answered from the meetings this service holds.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            media_plane: media_plane::MediaPlane::new(Arc::clone(&engine)),
            engine,
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
        // This service signs as `chime` — a signing name it SHARES with every other
        // Amazon Chime SDK service in this workspace (`service-com-amazonaws-chime-*`),
        // each of which serves its OWN endpoint (`chime`, `identity-chime`,
        // `media-pipelines-chime`, `messaging-chime`, `voice-chime`). The router's
        // routing key comes from the SigV4 credential scope first, so all of them resolve
        // to `chime` and matching on the signing name ALONE makes each one claim every
        // `chime`-signed request.
        //
        // So the claim is keyed on what identifies ONE service: the endpoint the request
        // addresses, and failing that this model's own URI path space (`/meetings…`,
        // `/tags`). That rule, and the readers for the path spaces the family shares,
        // live once in `service-common` — this file used to carry its own copy of the
        // path half, which could not tell this service's `/meetings` space from the
        // legacy Amazon Chime endpoint's `/meetings/{MeetingId}/dial-outs`.
        //
        // This model owns the `/meetings` resource space; the legacy endpoint binds only
        // the dial-out sub-space of it, so this service claims the COMPLEMENT of that one
        // sub-space. (`/tags` is shared by five of the six: a request that names no
        // endpoint is served by the family's declared tagging owner, and this service
        // serves that space at its OWN endpoint from the same shared ARN-keyed tag
        // directory, so both answer alike — see `chime_family`.)
        //
        // And one request that rule cannot place at all: a client opening one of the MEDIA
        // sessions a meeting's `MediaPlacement` published to it ([`media_plane`]). Such a
        // request names no modelled operation and carries no SigV4 credential scope — a
        // client of a published media address has none to sign with — so neither the family's
        // endpoint rule nor this model's path space reaches it, and the front door answered a
        // routing failure for an address this service itself handed out. The claim is narrowed
        // to this service's own published addresses, and to a meeting this store holds or a
        // WebSocket handshake, so no other service's traffic is taken.
        if media_plane::claims_parts(&*self.engine, parts) {
            return true;
        }
        service_common::chime_family::claims_chime_family_request(
            parts,
            OWN_ENDPOINT_PREFIX,
            crate::ROUTE_PATH_SEGMENTS,
            |parts| {
                let path = parts.uri.path();
                service_common::chime_family::addresses_meeting_space(path)
                    && !service_common::chime_family::addresses_meeting_dial_outs(path)
            },
        )
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // A meeting's MEDIA session is no modelled operation of this service by construction
        // — no modelled route is bound at a `MediaPlacement` address — so it is served the way
        // an emulated data plane is: claimed by `can_handle` and served here, by the one rule
        // ([`media_plane::claims_request`]), so the claim and the serving cannot disagree.
        if self.media_plane.claims(req) {
            return Some(&self.media_plane as _);
        }
        // Every other request resolves through the generated routing table.
        None
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
