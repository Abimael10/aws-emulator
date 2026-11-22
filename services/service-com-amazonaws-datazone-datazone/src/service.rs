// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-datazone-datazone` service: how it claims requests and how it words the
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
//!   before any handler ran. This service DOES take it, for the one part of a
//!   constraint violation's sentence the shared renderer cannot derive: the form a
//!   modelled `@pattern` is REPORTED in (see [`validation_message`]). Every refusal
//!   that reports no pattern keeps the framework's wording.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. This service DOES take it: the entry's own
//!   protocol-correct answer, with the one difference a FAILURE's body carries
//!   (see [`rest_json_error_body`]), so a request this service does not serve
//!   answers in the same envelope every other failure of it does.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! A sixth seam, `override_protocol`, is one this service DOES take: it serves the
//! model's own `restJson1` codec with the differences the shared renderer cannot
//! express — what a FAILURE's body carries (see [`rest_json_error_body`]) and the member
//! name the live wire renders a domain listing's summaries under (see
//! [`recorded_response_members`]). Those differences are properties of this service's wire
//! envelope, not of one renderer, so both seams that WRITE a body here — the protocol (a
//! failure a handler raised, and every success body) and the routing error (a request no
//! modeled operation serves) — answer through it. (The refusal seam writes no body: it
//! chooses only the code and the message inside the envelope the framework renders.)
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// What a failure carries on the wire — the one difference from the model's own
/// `restJson1` renderer this service takes.
pub mod rest_json_error_body;

/// What a SUCCESS carries on the wire that this checkout's model snapshot does not declare:
/// the member name the live wire renders a domain listing's summaries under.
pub mod recorded_response_members;

/// Replacing the body of a response this service has already rendered, which both of the
/// seams above do.
pub mod response_body;

/// How a refusal of the INPUT is worded: the one part of a constraint violation's sentence
/// the shared renderer cannot derive, the reported `@pattern`.
pub mod validation_message;

/// The route that answers at the data portal address a domain reports, which this emulator
/// publishes on its own endpoint.
pub mod data_portal;

/// The `com-amazonaws-datazone-datazone` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// This service's wire protocol, built ONCE here because
    /// [`Service::override_protocol`] hands back a borrow of it.
    protocol: rest_json_error_body::DataZoneRestJson,
    /// The shared engine, for the ONE answer this file serves itself rather than through an
    /// operation: the data portal page of a stored domain ([`data_portal`]).
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            protocol: rest_json_error_body::DataZoneRestJson::new(),
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
        // The data portal a domain reports (`portalUrl`) is opened by whatever browser,
        // probe or test holds that URL: anonymously, with no credential scope and no target
        // header naming this service — only the path this service itself minted, under the
        // plane segment that is this service's alone. A fetch of exactly that path is
        // therefore claimed whatever host the client addressed, so the portal is reachable at
        // the address the domain advertised; the key a signed request resolves names no
        // service for an endpoint reached by address, which is the only address a client on
        // this machine can reach.
        if data_portal::claimed(parts) {
            return true;
        }
        // Every other request: claim it when its resolved route key is this service's
        // signing name. Edit this for anything the model does not express — a signing
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
        // TAKEN for the one part of a violation's sentence the shared renderer cannot
        // derive: a modelled `@pattern` is a FIND expression, while the expression a
        // refusal REPORTS is that constraint's whole-value form (see
        // [`validation_message`]). Everything else about the refusal — the envelope, every
        // other clause, and withholding a `@sensitive` member's value — stays the
        // framework's, and a refusal reporting no pattern keeps its wording untouched,
        // which is what `None` asks for.
        validation_message::reworded(refusal)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it.
        //
        // The ONE address this service serves that the Smithy model does not describe: the
        // data portal of a stored domain, which `portalUrl` publishes for a client to
        // follow. It is answered here because the router consults this seam before it
        // authenticates anything, which is how AWS serves its own portal host —
        // unauthenticated, and as a web page rather than a modelled API call.
        if let Some(served) = data_portal::answer(&*self.engine, req) {
            return Some(served);
        }
        //
        // For every OTHER unrouted request the STATUS, the headers and the wording stay the
        // generated entry's: this service states nothing of its own about a request it does
        // not serve. Only the BODY is this service's, and by the same rule every other
        // failure of it follows ([`rest_json_error_body`]): a `restJson1` failure names its
        // error shape in `x-amzn-errortype`, so its body carries the shape's own members and
        // no discriminator. The shared pre-dispatch renderer writes one for every service it
        // serves, and this is the seam that owns a routing failure's answer here.
        Some(rest_json_error_body::with_served_error_body(
            smithy_server_runtime::ErasedServiceDefinition::routing_error_response(
                &self.entry,
                req,
            ),
        ))
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering.
        //
        // NOT taken, unlike the routing seam above: this one is handed the failure by
        // shared reference and no request, so the entry's own renderer — which consumes
        // the failure and reads the request's protocol — cannot be called from here, and
        // re-deriving a body would state the failure a second way. The body that renderer
        // writes stays as it writes it.
        let _ = error;
        None
    }

    fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
        // The model's own restJson1 codec, with the differences the shared renderer cannot
        // express: a failure's body carries the error shape's own members and nothing else,
        // because this protocol names the shape in a header (see `rest_json_error_body`),
        // and a success body carries the member the live wire renders that the model
        // snapshot does not declare (see `recorded_response_members`). Borrowed from the
        // field built in `new`, never constructed per request.
        Some(&self.protocol)
    }
}
