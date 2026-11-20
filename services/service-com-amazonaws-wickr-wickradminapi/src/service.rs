// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-wickr-wickradminapi` service: how it claims requests and how it words the
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
//! - `override_operation` — EDITED HERE. The routing key, the handler and the store
//!   stay the generated table's; `GetNetwork` alone is served through
//!   [`network_ownership::OwnershipFirst`], because this API resolves ownership of a
//!   `networkId` path label before it validates the label's shape. See "Why this file
//!   denies a network id" below.
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
//! FOUR seams are taken here. Three of them are for one reason: every response of this
//! service carries the envelope stated in [`response_envelope`] — the charset-qualified
//! media type and an entity tag over the body — which is a property of the channel rather
//! than of any one operation, so it is applied at every seam that can answer: the
//! centralized walk (`override_operation`), an unroutable request
//! (`override_routing_error`) and a transport-level failure (`override_framework_error`).
//! Routing, status, codes, messages and every body byte stay with the framework.
//!
//! ## Why this file denies a network id
//!
//! The fourth is `override_operation`'s other half. Reading one network is an ownership
//! question before it is anything else: this API resolves ownership of the `networkId`
//! path label before it validates the label's shape, so a label it cannot own is denied
//! rather than reported back as a malformed value. The framework's request walk runs the
//! other way round — the model's constraint traits are checked before any handler runs —
//! so without a seam a label outside `NetworkId` is answered with the boundary's refusal
//! and the denial is never reached.
//!
//! [`network_ownership::OwnershipFirst`] restores that order for `GetNetwork`, and only
//! for it: the rest of the ownership question (a network of this account that is gone, an
//! id this store cannot place) is decided in the handler, from the store where that answer
//! lives. Every other operation keeps the framework's order and its own refusals. The
//! denial it answers, and everything it delegates, carry the same response envelope as
//! every other response.

pub mod network_ownership;

pub mod response_envelope;

use response_envelope::ResponseEnvelopeOperation;
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Operation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The `com-amazonaws-wickr-wickradminapi` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The wrapper every routed request is served through, which runs the routing table's
    /// own entry and answers it in this service's envelope (see [`response_envelope`]).
    /// Held in a field, built once, because `override_operation` hands back a reference to
    /// it.
    envelope: ResponseEnvelopeOperation,
    /// The ownership-first answer for `GetNetwork`, built once here and never per request.
    /// It holds the operation's TYPE only — no second operation instance and no second
    /// store — and fronts the same [`ResponseEnvelopeOperation`] every other request is
    /// served through, so a denial and a served read carry one envelope.
    get_network: network_ownership::OwnershipFirst<
        crate::ops::get_network::GetNetwork<Eng>,
        ResponseEnvelopeOperation,
    >,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
            envelope: ResponseEnvelopeOperation::new(),
            get_network: network_ownership::OwnershipFirst::new(ResponseEnvelopeOperation::new()),
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
        // Every request the generated routing table can serve is served through the
        // envelope wrapper, which delegates to that same routing-table entry and then puts
        // this service's response envelope on what it answered (see
        // [`response_envelope`]). Routing stays the model's: a request the table cannot
        // serve is declined here, so the framework answers it as the unroutable request it
        // is — through `override_routing_error` below, which puts the same envelope on it.
        let definition: &dyn ErasedServiceDefinition = &self.entry;
        if !service_common::response_media_type::routes(definition, req) {
            return None;
        }
        // `GetNetwork` is served through the ownership-first answer in front of that
        // wrapper: this API denies a `networkId` it cannot own before it judges the label's
        // shape, which is the reverse of the framework's own order (validate, then
        // dispatch). Every other operation reaches the wrapper directly, so their refusals
        // are each their own operation's contract.
        let key = self.entry.protocol().routing_key(req).ok()?;
        if key == <crate::ops::get_network::GetNetwork<Eng> as Operation>::NAME {
            return Some(&self.get_network as _);
        }
        Some(&self.envelope as _)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // This service names a request that failed the model's own rules
        // `ValidationError` — the shape every operation declares for "one or more
        // fields in the request failed validation" — rather than the framework's
        // generic `ValidationException`, so a client branching on the Wickr error
        // code sees one code whether the value was refused at the boundary (a
        // `networkId` that is not 8 digits) or by a handler (a `networkName`
        // longer than the 20 characters the model documents). The HTTP status of a
        // boundary refusal stays with the protocol (400); only a handler-returned
        // `ValidationError` carries the modelled 422.
        //
        // `GetNetwork` never reaches this seam for its path label: that operation
        // resolves ownership of the label first and denies it, which is why it is
        // served through `network_ownership::OwnershipFirst` above.
        match refusal {
            Refusal::Invalid(violations) => Some(Refused {
                code: "ValidationError".to_string(),
                message: format!(
                    "{} validation error{} detected: {}",
                    violations.len(),
                    if violations.len() == 1 { "" } else { "s" },
                    violations
                        .iter()
                        .map(|violation| violation.path.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
            // An authentication refusal is not a field-level validation failure,
            // so it keeps the framework's wording.
            _ => None,
        }
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated routing table
        // resolved it. The entry's own protocol-correct rendering is kept, word for word
        // and status for status; only this service's response envelope is put on it (see
        // [`response_envelope`]).
        let definition: &dyn ErasedServiceDefinition = &self.entry;
        Some(response_envelope::enveloped(
            definition.routing_error_response(req),
        ))
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. It is handed the failure ALONE, with no request to resolve an
        // envelope from, so the one variant the router raises here is rebuilt, rendered in
        // the JSON pre-dispatch envelope and answered with this service's response
        // envelope on it; every other variant declines to `None`, keeping the framework's
        // own rendering of a failure that cannot be rebuilt rather than losing what it
        // said. That rebuild is `service_common`'s, not restated here.
        service_common::response_media_type::reannounced_framework_error(
            response_envelope::RESPONSE_MEDIA_TYPE,
            error,
        )
        .map(response_envelope::enveloped)
    }
}
