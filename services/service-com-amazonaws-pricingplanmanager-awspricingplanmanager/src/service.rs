// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-pricingplanmanager-awspricingplanmanager` service: how it claims requests and how it words the
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
//! A sixth seam, `override_protocol`, is the one this service DOES take: every timestamp
//! member of this model declares `@timestampFormat("date-time")`, so the service returns
//! the shared `date-time` `restJson1` codec from it. See the method's own note.
//!
//! `override_operation` is taken too, and NOT to re-route: the three operations whose
//! input declares a bounded collection member are served through the tree's shared
//! collection-quoting pass, so a `@length` clause about one states the collection the
//! request carried rather than a summary of its size. The value that clause needs can only
//! be read from the request, which `override_refusal` is never handed; which operations pay
//! for the pass, and the one fact this service states about its clauses, are in
//! [`constraint_refusal`].
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

mod constraint_refusal;

use smithy_server_runtime::{http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service};
use std::sync::Arc;

/// The `com-amazonaws-pricingplanmanager-awspricingplanmanager` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// This service's wire protocol, built ONCE here because
    /// [`Service::override_protocol`] hands back a borrow of it.
    protocol: service_common::date_time_json::DateTimeRestJson,
    /// The operations served through the shared collection-quoting pass
    /// ([`constraint_refusal::routing_table`]), keyed by the routing key the protocol
    /// resolves — the key the generated table uses — and built from the SAME shared engine,
    /// so a wrapped operation reads and writes the one store.
    stated: OperationRoutingTable,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            stated: constraint_refusal::routing_table(&engine),
            entry: crate::ServiceDefinition::new(engine),
            protocol: service_common::date_time_json::DateTimeRestJson::new(),
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
        // NOT a re-route: the key is the one the generated table's own protocol resolves,
        // and the operation it names is the one served — through the shared
        // collection-quoting pass for the operations [`constraint_refusal`] files there. A
        // key that table does not hold (every operation taking scalars only) resolves to
        // `None` here and is served by the generated table exactly as before, which is also
        // what a key the model does not name answers, so the router's routing-error path is
        // unchanged.
        let key = self
            .entry
            .wire_protocol(req)
            .protocol()
            .routing_key(req)
            .ok()?;
        self.stated.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent required
        // member, or a value outside a declared bound) to let this service reword it. It is
        // handed the violations ALONE — no request — so it cannot state the COLLECTION a
        // `@length` clause quotes; the shared pass [`constraint_refusal`] files the
        // collection-bearing operations through answers their refusals before this walk is
        // reached, with the request in hand. A refusal that gets here anyway keeps the
        // framework's own wording, which states the same envelope, code and requirement.
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

    fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
        // Every timestamp member of this model declares
        // `@timestampFormat("date-time")` — a subscription's `createdAt`/`updatedAt`, on
        // both the full `Subscription` and the `SubscriptionSummary` a list reports, and a
        // pending change's `effectiveDate` — so every instant this service reports is an
        // RFC 3339 string rather than the protocol's default epoch-seconds number, and an
        // instant sent to it is read the same way. The codec is otherwise the model's own
        // `restJson1`.
        Some(&self.protocol)
    }
}
