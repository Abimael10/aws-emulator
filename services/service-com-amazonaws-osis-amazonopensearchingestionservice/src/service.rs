// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-osis-amazonopensearchingestionservice` service: how it claims requests and how it words the
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
//!   request's resolved route key against this service's signing name; EDITED HERE to
//!   claim a PIPELINE's own ingest traffic too (see [`ingest_plane`]).
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. EDITED HERE to serve a request addressed to the ingestion
//!   endpoint a pipeline published, so the router verifies the credential it carries.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). EDITED HERE for an ingest address naming a pipeline this service does not
//!   hold: nothing is served there, which is not an operation it fails to implement.
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
//! Three of those seams are taken for one reason: a PIPELINE this service creates publishes
//! the address records are sent to (`Pipeline$IngestEndpointUrls`, "the ingestion endpoints
//! for the pipeline, which you can send data to"), and what is spoken there is the pipeline's
//! own Data Prepper `http` source — which this model does not describe, so a POST to that
//! address named no modelled operation of this service. The address a pipeline publishes is
//! now one this emulator answers on ([`crate::state::ingest_plane_base`]), and
//! [`ingest_plane`] is what claims and serves it.

use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The PIPELINE's own ingest data plane: the surface served at the address a pipeline
/// publishes as its `IngestEndpointUrls`, which this service's model does not describe and
/// which no modelled route names. See that module for why three of the seams below take it
/// into account.
pub mod ingest_plane;

mod validation;

/// The `com-amazonaws-osis-amazonopensearchingestionservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The SAME shared engine the generated entry threads into every operation.
    ///
    /// Held here because a PIPELINE's own ingest traffic ([`ingest_plane`]) is claimed by
    /// `can_handle` and served from `override_operation`, neither of which is an operation
    /// with a `self.engine` of its own: both must read the one store the control-plane
    /// operations write, not a second copy.
    engine: Arc<Eng>,
    /// The ingest plane as an [`ErasedOperation`], so the router verifies the SigV4
    /// credential a signed ingest request carries before any record is accepted. Held in a
    /// field, built once, because `override_operation` hands back a reference borrowed from
    /// `&self`.
    ingest: ingest_plane::IngestPlane<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            ingest: ingest_plane::IngestPlane::new(Arc::clone(&engine)),
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
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        let claims_control_plane = match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        };
        claims_control_plane
            // And a PIPELINE's own ingest traffic: a request addressed to the ingestion
            // endpoint a pipeline this service holds published
            // ([`ingest_plane::claims_parts`]). Such a request names no modelled operation —
            // its path is the pipeline's own Data Prepper source path — so one carrying no
            // credential scope of this service could resolve no claimant at all, for the one
            // address this service told the caller to send data to.
            //
            // The claim is narrowed to a pipeline this service HOLDS, read from the one
            // shared store, rather than to a path shape alone, so one request keeps one
            // claimant.
            || ingest_plane::claims_parts(&*self.engine, parts)
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
        // A request to a pipeline's own ingestion endpoint is named as an operation here, so
        // the router verifies the SigV4 credential it carries before any record is accepted
        // — the router authenticates only what it names an operation for, and an ingest
        // request to this service is signed exactly as every other call to it is.
        if self.ingest.claims(req) {
            return Some(&self.ingest);
        }
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // OpenSearch Ingestion states a `@pattern` requirement with the pattern
        // it PUBLISHES for that string shape — unanchored, and with a trailing
        // one-or-more repetition bounded by the shape's `@length` — rather than
        // the model's regex literal. It also reports a member that is a
        // COLLECTION of constrained scalars once per MEMBER, not once per
        // offending element: the entry names the collection member's own path,
        // renders the refused values as a bracketed list and enumerates the
        // element shape's whole constraint set. Every other refusal keeps the
        // framework's wording,
        // which already matches. See `validation` for the rule and where its
        // constraints come from; it answers `None` for everything it does not
        // reword, and returns the framework's own error code with the message it
        // does.
        validation::refusal(refusal)
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
        //
        // A path-style INGEST address naming a pipeline this service does not hold is
        // exactly that case by construction — the plane's claim is narrowed to a pipeline
        // the store holds — so this is where it is answered: nothing is served there.
        ingest_plane::serve_unclaimed(req)
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
