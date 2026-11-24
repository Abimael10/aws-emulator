// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-signin-signin` service: how it claims requests and how it words the
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
//! ## The OAuth 2.0 token endpoints
//!
//! Two of this service's operations are the two forms of `POST /v1/token`, and both are
//! served behind [`oauth2_token_endpoint`], which owns that endpoint's wire contract: how a
//! token request's parameters are read, and the RFC 6749 §5.2 error object a request it
//! cannot accept is refused with. The anonymous form is also what the claim below reaches:
//! `CreateOAuth2Token` lists NO auth scheme, so its request carries no SigV4 credential
//! scope and names no service at all, and is claimed by the route its model binds.

pub mod oauth2_token_endpoint;

use oauth2_token_endpoint::{Endpoint, OAuth2TokenEndpoint};
use smithy_server_runtime::{
    http, ErasedOperation, OperationRoutingTable, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The `com-amazonaws-signin-signin` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The two `/v1/token` operations the generated table also holds, each wrapped in
    /// [`OAuth2TokenEndpoint`] as the form of the endpoint it serves. Keyed by the routing
    /// key the protocol resolves — the key the generated table uses — and built from the
    /// SAME shared engine, so a wrapped operation reads and writes the one store.
    token_endpoints: OperationRoutingTable,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        let mut token_endpoints = OperationRoutingTable::new();
        token_endpoints.add_operation(
            "CreateOAuth2Token",
            Box::new(OAuth2TokenEndpoint::new(
                crate::ops::create_o_auth2_token::CreateOAuth2Token::new(Arc::clone(&engine)),
                Endpoint::ClientAuthenticated,
            )) as Box<dyn ErasedOperation>,
        );
        token_endpoints.add_operation(
            "CreateOAuth2TokenWithIAM",
            Box::new(OAuth2TokenEndpoint::new(
                crate::ops::create_o_auth2_token_with_iam::CreateOAuth2TokenWithIAM::new(
                    Arc::clone(&engine),
                ),
                Endpoint::IamSigned,
            )) as Box<dyn ErasedOperation>,
        );
        Self {
            entry: crate::ServiceDefinition::new(engine),
            token_endpoints,
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
        // A request whose resolved route key is this service's signing name, as every
        // service claims its own traffic — PLUS a request that names no service at all and
        // addresses an operation this model serves UNSIGNED. `CreateOAuth2Token` is that
        // operation: it lists no auth scheme (it is authenticated by the OAuth 2.0 client
        // credentials in its body), so it carries no credential scope to resolve a service
        // from, and without this claim the anonymous `POST /v1/token` reaches nobody. The
        // rule is `service_common`'s, so every service that publishes an anonymous
        // operation takes the same one; it is as narrow as the model makes it — a request
        // naming another service is that service's, and a route this model does not bind,
        // or one whose operation the model requires a signature for, is not claimed here.
        ::service_common::routes_to(parts, self.entry.identity().signing_name())
            || ::service_common::anonymous_route_claim::claims_anonymous_modeled_route(
                parts,
                &self.entry,
            )
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // NOT a re-route: the key is the one the generated table's own protocol resolves,
        // and the operation it names is the one served — wrapped in `OAuth2TokenEndpoint`
        // for the two `/v1/token` forms, so a token request is read and refused the way
        // that endpoint's OAuth 2.0 wire contract states. Every other key resolves to
        // `None` here and is served by the generated table, exactly as before.
        let key = self.entry.protocol().routing_key(req).ok()?;
        self.token_endpoints.get(key)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // This service answers the `ValidationException` envelope the framework already
        // renders — same 400, same code, same `N validation error(s) detected:` prefix,
        // same member paths, same echoed value — with ONE rendering of its own: a
        // `@pattern` requirement is quoted as the expression a value has to satisfy,
        // WITHOUT the `^` / `$` the model wraps it in to turn Smithy's find into a
        // whole-value match. The anchoring is how the model spells that match, not part
        // of the character grammar this service publishes, so a client that lifts the
        // expression out of the message and re-validates locally gets the one the
        // service advertises. The CHECK is unchanged: it still runs the model's pattern
        // as written.
        //
        // That rendering is shared by every service on a JSON protocol (this one
        // answers restJson1), so it is reached in `service-common` rather than spelled
        // here, and it reaches every pattern-constrained member of every operation this
        // service models rather than the one `@pattern` a recording happened to break.
        ::service_common::validation_message::aws_validation_refusal(refusal)
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
