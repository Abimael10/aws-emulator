// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-sagemaker-sagemaker` service: how it claims requests and how it words the
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

// How this service words a request that breaks a constraint — the one place its
// validation dialect lives, reached both from `override_refusal` below and from the
// handlers that enforce a constraint the model cannot express.
pub(crate) mod validation;

/// The one refusal this service states from the REQUEST rather than from the violations
/// alone — a constraint a list's ELEMENTS break, stated at the list member quoting the list
/// the request sent — and the operation wrapper it is applied in
/// ([`list_element_refusals::StatedFromRequest`]).
mod list_element_refusals;

/// Whether IAM lets a request perform the action it names, which the ML Lineage reads decide
/// before they report anything ([`authorization::authorize`]).
pub mod authorization;

/// Serves the authenticated session a presigned URL this service handed out opens, at the
/// address it published for it ([`presigned_session::PresignedSessionRoute`]).
pub mod presigned_session;

/// Serves the Studio landing address a domain publishes as its `Url`
/// ([`studio_landing::StudioLandingRoute`]).
pub mod studio_landing;

/// What the routes that serve an ADDRESS this service published share: how such an address's
/// path is read, and how one fetch of it is answered ([`published_address`]).
mod published_address;

/// This service's own Smithy shape id, whose LOCAL name (`SageMaker`) is what an
/// awsJson1_1 request names its target service by
/// (`X-Amz-Target: SageMaker.<Operation>`).
///
/// The SigV4 signing name cannot tell this service from its data-plane peers — they all
/// sign as `sagemaker` — so the shape id is what `service_common::claims_request` keys
/// this service's RPC traffic on.
const RPC_SERVICE_SHAPE_ID: &str = "com.amazonaws.sagemaker#SageMaker";

/// The `com-amazonaws-sagemaker-sagemaker` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The route that serves the authenticated session a presigned URL this service
    /// published opens. Held rather than built per request because
    /// [`Service::override_operation`] hands the router a BORROW of it.
    presigned_session: presigned_session::PresignedSessionRoute<Eng>,
    /// The route that serves the Studio landing address a domain publishes as its `Url`.
    /// Held for the same reason the presigned-session route is.
    studio_landing: studio_landing::StudioLandingRoute<Eng>,
    /// The wrapper every routable request is served through, so a constraint a list's
    /// ELEMENTS break is stated at the list member quoting the list the request sent; see
    /// [`list_element_refusals`]. Held for the same reason the route above is.
    stated_from_request: list_element_refusals::StatedFromRequest,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            presigned_session: presigned_session::PresignedSessionRoute::new(Arc::clone(&engine)),
            studio_landing: studio_landing::StudioLandingRoute::new(Arc::clone(&engine)),
            stated_from_request: list_element_refusals::StatedFromRequest,
            entry: crate::ServiceDefinition::new(engine),
        }
    }

    /// The presigned-session route, when it is what serves this request.
    ///
    /// The ONE place that decision is made, so `can_handle` claims exactly the requests
    /// `override_operation` then serves.
    fn presigned_session_route(&self, method: &str, uri: &str) -> Option<&dyn ErasedOperation> {
        self.presigned_session
            .claims(method, uri)
            .then_some(&self.presigned_session as &dyn ErasedOperation)
    }

    /// The Studio landing route, when it is what serves this request — the same one-place
    /// decision the presigned-session route above is resolved through.
    fn studio_landing_route(&self, method: &str, uri: &str) -> Option<&dyn ErasedOperation> {
        self.studio_landing
            .claims(method, uri)
            .then_some(&self.studio_landing as &dyn ErasedOperation)
    }

    /// The route that serves a request addressed at one of the ADDRESSES this service
    /// published for a caller to follow, rather than at a modelled operation.
    fn published_address_route(&self, method: &str, uri: &str) -> Option<&dyn ErasedOperation> {
        self.presigned_session_route(method, uri)
            .or_else(|| self.studio_landing_route(method, uri))
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
        // This service signs as `sagemaker` — a signing name it SHARES with every
        // SageMaker-family data-plane service (`runtime.sagemaker`,
        // `a2i-runtime.sagemaker`, `edge.sagemaker`, `featurestore-runtime.sagemaker`,
        // `metrics.sagemaker`, `training-session-runtime.sagemaker` and
        // `job-runtime.sagemaker`), while its own endpoint is `api.sagemaker`. The router
        // resolves the routing key from the SigV4 credential scope first, so all of them
        // see the key `sagemaker`: claiming on the signing name ALONE makes this service
        // claim every sibling's traffic, and the router — which refuses rather than
        // guesses — answers `500 AmbiguousService` for ALL of it.
        //
        // So the claim is narrowed to this service's own modelled wire surface, by the
        // one rule `service-common` states for every service under a shared signing
        // name. This model is `awsJson1_1` and carries no `smithy.api#http` binding, so
        // its surface is the RPC form: the request must NAME this service in
        // `X-Amz-Target: SageMaker.<Operation>` and arrive on the path that protocol
        // posts to (the root). Each data-plane sibling's own URI path space is then
        // claimed by it alone — and a root-path request targeted at some other RPC peer
        // sharing the signing name is not claimed here either.
        // The ADDRESSES this service published for a caller to follow — a presigned URL, a
        // Studio domain's landing `Url` — are claimed by their PATH: such an address is
        // fetched by a browser, which presents no credential scope and names no route key,
        // so nothing else would claim it (see [`presigned_session::PresignedSessionRoute`]
        // and [`studio_landing::StudioLandingRoute`]).
        if self
            .published_address_route(parts.method.as_str(), &parts.uri.to_string())
            .is_some()
        {
            return true;
        }
        service_common::claims_request(
            parts,
            self.entry.identity().signing_name(),
            crate::ROUTE_PATH_SEGMENTS,
            Some(RPC_SERVICE_SHAPE_ID),
        )
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
        // The model binds no operation to the addresses this service published for a caller
        // to follow — an opened session is not an API call, and neither is a landing page:
        // both are what the API's URL leads to — so those addresses are served here.
        //
        // Every OTHER request the generated routing table can serve is served through the
        // wrapper that states the one refusal this service words from the REQUEST — a
        // constraint a list's ELEMENTS break, stated at the list member quoting the list the
        // request sent (see [`list_element_refusals`]) — which delegates to that same table
        // entry for everything else. The routing stays the model's: a request whose target
        // names no modelled operation is declined here, so the framework answers it as the
        // unroutable request it is.
        if let Some(route) = self.published_address_route(req.method(), req.uri()) {
            return Some(route);
        }
        let routes = self
            .entry
            .protocol()
            .routing_key(req)
            .ok()
            .and_then(|key| self.entry.routing_table().get(key))
            .is_some();
        routes.then_some(&self.stated_from_request as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service reword it. SageMaker states several of those facts differently from
        // the framework — a `@pattern` without its whole-value anchors, an enum
        // value set over the values the service holds and in the order its own table
        // of them is walked, and the clauses of one refusal in the order that same
        // findings table is walked — so a constraint failure is reworded through
        // `validation`, which restates the code (`ValidationException`)
        // together with the message: a chosen `Refused` replaces both.
        //
        // It is handed the violations ALONE — no request — so the one clause whose value
        // half is the request's own (the list a failed ELEMENT belongs to) cannot be
        // stated here: `list_element_refusals` answers that before this walk is reached,
        // through the SAME renderer, so the wording cannot differ between the two paths.
        //
        // Every other refusal class keeps the framework's wording: an unauthenticated
        // caller is answered by the framework's own 403.
        match refusal {
            Refusal::Invalid(violations) => {
                validation::refusal(violations, &[], &std::collections::HashMap::new())
            }
            _ => None,
        }
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
