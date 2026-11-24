// SPDX-License-Identifier: CC0-1.0

//! The claim a REST service makes on a request that names NO service at all and addresses
//! an operation its own model serves UNSIGNED.
//!
//! # Why such a request names no service
//!
//! The framework resolves a request's routing key from a SigV4 credential scope, an
//! `X-Amz-Target` or a service-naming endpoint host. An operation whose model lists NO auth
//! scheme is sent unsigned — it is authenticated by something in its own payload, not by
//! SigV4 — so it carries no credential scope; a REST operation names no target; and a client
//! pointed at this emulator's own address reaches it on a loopback host, which names no
//! service. The request then resolves to no key at all, every service's default claim (the
//! key is my signing name) is false, and the router answers `404 UnknownService` — so an
//! operation the model publishes as anonymous is unreachable over the wire.
//!
//! # The claim
//!
//! [`claims_anonymous_modeled_route`] admits exactly those requests: nothing in the request
//! names a service, AND this service's own model binds a route for the request whose
//! operation the model serves unsigned. That is how AWS itself dispatches such a call — the
//! endpoint is addressed by its HTTP route, because the request has no credential to name it
//! by — and it is as narrow as the model makes it: a request naming ANOTHER service is that
//! service's, a route this model does not bind is still nobody's (the router's honest
//! `404`), and a route whose operation the model requires a signature for is not claimed
//! here, so an unsigned call to a signed operation is refused as before rather than served.
//!
//! The route half and the auth half are both read off the service's generated
//! `ServiceDefinition` — the same protocol and routing table the router itself dispatches
//! through ([`crate::modeled_operation`]) and the same
//! [`ErasedOperation::auth_schemes`](smithy_server_runtime::ErasedOperation::auth_schemes)
//! the router's own authentication step reads — so the claim cannot drift from the model.
//!
//! A service takes it beside its own routing-key claim, which keeps every signed request
//! exactly where it was:
//!
//! ```ignore
//! fn can_handle(&self, parts: &http::request::Parts) -> bool {
//!     ::service_common::routes_to(parts, self.entry.identity().signing_name())
//!         || ::service_common::anonymous_route_claim::claims_anonymous_modeled_route(
//!             parts,
//!             &self.entry,
//!         )
//! }
//! ```
//!
//! # Services still carrying their own copy
//!
//! This rule predates this module in the service crates that needed it before there was
//! anywhere shared to put it — `grep -rln 'fn claims_unattributed_route' services/*/src/`
//! lists them (today: `ssooidc`, whose copy omits the auth-scheme conjunct and so claims an
//! unattributed request to a SIGNED route as well). That copy is to be MIGRATED onto this
//! module; it is left in place only because this change does not own that service's issues
//! and its claim is covered by that crate's own suite. Nothing new should inline the rule
//! again — call this module.
//!
//! # Where the rule is exercised
//!
//! The route half is read through the service's OWN generated protocol and routing table,
//! which exist only in a service crate, so this rule is exercised end to end there rather
//! than unit-tested here — as [`crate::addresses_modeled_route`] is. The anonymous claim's
//! own cover is
//! `services/service-com-amazonaws-signin-signin/tests/all/oauth2_token_acceptance.rs`,
//! which drives the anonymous `POST /v1/token` with no routing signal at all, beside that
//! crate's `operation_reachability.rs`, where every signed row still reaches the service by
//! its routing key.

use smithy_server_runtime::ErasedServiceDefinition;

/// Whether `definition`'s service claims `parts`: the request names no service at all, and
/// this model binds a route for it whose operation is served unsigned.
///
/// See the [module docs](self) for the rule and for why it is the narrowest one that makes
/// an anonymous operation reachable.
#[must_use]
pub fn claims_anonymous_modeled_route(
    parts: &http::request::Parts,
    definition: &dyn ErasedServiceDefinition,
) -> bool {
    anonymous_modeled_operation(parts, definition).is_some()
}

/// The operation `definition`'s model binds for `parts` when the request names no service
/// and that operation is served unsigned, or `None` in every other case.
///
/// The named half of [`claims_anonymous_modeled_route`], for a service that also has to
/// state WHICH anonymous operation it is serving.
#[must_use]
pub fn anonymous_modeled_operation(
    parts: &http::request::Parts,
    definition: &dyn ErasedServiceDefinition,
) -> Option<String> {
    if ::emulator_server::resolve_route_key(parts).is_some() {
        // The request names a service. Whichever service that is, this rule does not
        // decide: a request that names one is claimed by its routing key, exactly as
        // before.
        return None;
    }
    let operation = crate::modeled_operation(parts, definition)?;
    // An EMPTY scheme list is what the model says "served unsigned" with (an empty
    // `@auth`, or `@optionalAuth`); a list with schemes in it, and an operation the table
    // states none for, are both signed as far as this claim is concerned.
    definition
        .routing_table()
        .get(&operation)?
        .auth_schemes()
        .is_some_and(<[_]>::is_empty)
        .then_some(operation)
}
