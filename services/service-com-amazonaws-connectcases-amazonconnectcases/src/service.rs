// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-connectcases-amazonconnectcases` service: how it claims requests and how it words the
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
//! A sixth seam, `override_protocol`, is the one this service DOES take: every timestamp
//! member of this model declares `@timestampFormat("date-time")`, so each instant it
//! reports is an RFC 3339 string rather than the protocol's default epoch-seconds number.
//!
//! ## How a refusal of a request BODY is stated
//!
//! Most of this service's operations have their request BODY validated against the
//! operation's modelled input by a layer AHEAD of the code that owns the modelled errors,
//! and that layer states ONE fixed sentence for every member and every constraint class it
//! refuses — it names neither the member, nor the value, nor the clause it broke, nor how
//! many clauses failed — under the error code that layer owns, which this service's model
//! does not declare. So each of those operations is served behind this service's front
//! door ([`::service_common::front_door::FrontDoorTable`], wired in [`body_door`]), which
//! reads each violated member's HTTP binding off that
//! operation's own input schema: a value the request carries OUTSIDE its body (a
//! `@httpLabel` path segment) is NOT a body rejection and is left to the layers behind the
//! door, which state it naming the member that failed. Nothing is re-implemented and no
//! check is repeated in other words: the violations are the framework's own, the status and
//! the envelope stay the framework's, and the code and the sentence are the shared ones
//! every service reached through such a layer answers.
//!
//! `CreateCase` is NOT behind that door, and the recordings are what say so: a create
//! whose body omits a required member is refused there with the modelled
//! `ValidationException` and the per-member clause that names the member and the constraint
//! it broke — the wording the framework's own walk states — rather than with the fixed
//! sentence the operations above answer. So its body keeps the framework's refusal, and the
//! two wordings stay with the operations whose wire shows each.
//!
//! A rule the model states only in prose is neither of those: it is refused by the
//! operation that owns it, as its own modelled `ValidationException` naming what failed (a
//! field value whose type the field does not declare, a create that supplies no value for
//! a field the service or the chosen template requires).

pub mod body_door;

use smithy_server_runtime::{
    http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The `com-amazonaws-connectcases-amazonconnectcases` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The modelled operations the body-validating layer fronts, each served behind this
    /// service's front door so a request BODY that breaks a modelled rule is refused the
    /// way this service refuses one ([`body_door::table`]). Keyed by the routing key the
    /// protocol resolves — the key the generated table uses — and built from the SAME shared
    /// engine, so a wrapped operation reads and writes the one store.
    front_door: OperationRoutingTable,
    /// This service's wire protocol, built ONCE here because
    /// [`Service::override_protocol`] hands back a borrow of it.
    protocol: ::service_common::date_time_json::DateTimeRestJson,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            front_door: body_door::table(&engine),
            protocol: ::service_common::date_time_json::DateTimeRestJson::new(),
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
        // and the operation it names is the one served — wrapped in the front door so a
        // request BODY that breaks a modelled rule is refused the way this service refuses
        // one. A key the model does not name resolves to `None` here exactly as it does in
        // the generated table, so the router's routing-error path is unchanged, and so does
        // an operation the door does not front (`CreateCase`), which the generated table
        // then serves.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.front_door.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The front door above answers every refusal of a request BODY it fronts, so what
        // reaches this seam is a refusal of a value the request carries OUTSIDE its body
        // (the `@httpLabel` `domainId`, `caseId`, `fieldId`, `templateId` and `layoutId`
        // path segments, the `@httpQuery` members of the listings), a body the codec itself
        // could not read at all and so reported no constraint violation for, or the body of
        // the one operation the door does not front. Each of those names the member that
        // failed and the constraint it broke — which is what the framework's own wording
        // already does — so this service rewords none of them.
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
        // Every timestamp this model declares carries `@timestampFormat("date-time")` —
        // a domain's, field's, template's and layout's `createdTime`/`lastModifiedTime`,
        // an audit event's `performedTime`, a related item's `associationTime`, an SLA's
        // `targetTime`/`completionTime` and a contact's `connectedToSystemTime` — so
        // every instant this service reports is an RFC 3339 string rather than the
        // protocol's default epoch-seconds number, and a date-time string a request
        // carries for one of them is read back the same way.
        // `tests/all/response_and_refusal_fidelity.rs` reads that claim off the shipped
        // model, so a member that later declared another format would fail there rather
        // than go out in the wrong form. The codec is otherwise the model's own
        // `restJson1`.
        Some(&self.protocol)
    }
}
