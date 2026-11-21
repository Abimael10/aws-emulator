// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-sagemakermetrics-sagemakermetricsservice` service: how it claims requests and how it words the
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
//!   correctly. EDITED HERE: a request whose ENVELOPE this service refuses is answered
//!   in front of the modelled operation, because the status, the error code and the body
//!   member each such refusal carries are this service's own and the refusal-wording
//!   seam below chooses none of them. See [`envelope`].
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
//! A sixth seam, `override_protocol`, is one this service DOES take: the codec every
//! modelled request is read with is the model's own `restJson1`, reading a batch's items
//! the way this service reads them ([`unset_members`]).
//!
//! # The authorization step every operation runs first
//!
//! AWS decides authorization at the IAM layer, in front of the service: a caller whose
//! identity-based policies do not allow the action — or that an explicit `Deny` covers — is
//! refused `AccessDeniedException` and the call performs nothing, so a refused ingest stores
//! no point and a refused read reports none. That decision is a property of the request's
//! identity, the action it names and the resources it addresses rather than of what a handler
//! goes on to do, so it is stated ONCE in [`authorization`] and taken by each operation
//! before it reads or writes the metric store.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

pub mod authorization;
mod envelope;
mod unset_members;

use aws_smithy_schema::Schema;
use smithy_server_runtime::{http, ErasedOperation, Refusal, Refused, Service};
use std::sync::Arc;

/// The input shape of the operation a routing key names, for the layers in this module
/// that read a request against the model rather than against a written-out list of
/// members.
///
/// The ONE mapping from this service's routing keys to its modelled inputs, so the
/// envelope layer and anything added beside it cannot come to different answers. `None`
/// for a key this service does not serve.
pub(crate) fn input_schema(routing_key: &str) -> Option<&'static Schema> {
    match routing_key {
        "BatchGetMetrics" => Some(crate::ops::types::BATCH_GET_METRICS_INPUT_SCHEMA),
        "BatchPutMetrics" => Some(crate::ops::types::BATCH_PUT_METRICS_INPUT_SCHEMA),
        _ => None,
    }
}

/// The `com-amazonaws-sagemakermetrics-sagemakermetricsservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The codec every modelled request of this service is read with, built ONCE here
    /// because [`Service::override_protocol`] hands back a borrow of it.
    protocol: unset_members::ItemsReadLeniently,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
            protocol: unset_members::ItemsReadLeniently::new(),
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
        // This service signs as `sagemaker` — a signing name it SHARES with every other
        // SageMaker-family service, while serving its own endpoint
        // (`metrics.sagemaker`). The router resolves the routing key from the SigV4
        // credential scope first, so every one of them sees the key `sagemaker`: claiming
        // on the signing name ALONE claims the siblings' traffic too, and the router —
        // which refuses rather than guesses — answers `500 AmbiguousService` for ALL of
        // it.
        //
        // So the claim is narrowed to this service's own modelled wire surface, by the
        // one rule `service-common` states for every service under a shared signing
        // name. This model is `restJson1`: its surface is the URI path space its
        // operations bind (`/BatchGetMetrics`, `/BatchPutMetrics`), which is what
        // `claims_request` matches against the generated `ROUTE_PATH_SEGMENTS` when a
        // service declares no RPC protocol.
        service_common::claims_request(
            parts,
            self.entry.identity().signing_name(),
            crate::ROUTE_PATH_SEGMENTS,
            None,
        )
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // A request whose ENVELOPE this service refuses — a member whose JSON type its
        // shape cannot be read from, a call that names no trial component, a call that
        // carries nothing to work on — is answered in front of the modelled operation,
        // which is the only seam that owns the status, the error code and the body member
        // such a refusal carries ([`envelope`] says why). Everything else resolves
        // through the generated routing table and is served by the operation itself.
        envelope::refuses(self.definition(), req).then_some(&envelope::REFUSED_ENVELOPE as _)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) so this service can
        // state it its own way. AWS states it as the `ValidationException` envelope the
        // framework already renders, except that it quotes a `@pattern` requirement
        // WITHOUT the `^`/`$` anchors the Smithy pattern carries. That wording is shared
        // by every service on a JSON protocol, so it lives in `service-common`; the
        // check itself still uses the model's pattern as written.
        //
        // This service's own refusals do not come through here at all: the modelled
        // constraints of both its operations are judged per item by their handlers
        // (`skip_validation` in `crate::ops::types`), the one reference it refuses the
        // whole call for is refused by the handler that resolves it
        // (`crate::ops::batch_get_metrics`), and an envelope it cannot serve is answered
        // by [`envelope`] in front of the operation. What is left for this seam is a
        // refusal of something this service does not word itself — a member added to the
        // model without `skip_validation`, say — which keeps the framework's wording.
        service_common::aws_validation_refusal(refusal)
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
        // The model's own `restJson1`, reading a batch's items the way this service reads
        // them: a member an item leaves out, or sends as `null`, is UNSET rather than a
        // refusal of the whole call. Built once in `new` and borrowed from `self`, never
        // per request.
        Some(&self.protocol)
    }
}
