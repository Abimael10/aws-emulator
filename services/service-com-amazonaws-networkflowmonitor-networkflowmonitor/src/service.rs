// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-networkflowmonitor-networkflowmonitor` service: how it claims requests and how it words the
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
//! - `override_operation` — EDITED HERE. It serves the operation the generated routing
//!   table names, under the layer that finishes every response of this service: one
//!   request id, reported in the header and stated inside the envelope a client-error
//!   refusal words (see [`sdk_envelope`]). The routing decision itself is still the
//!   model's.
//! - `override_refusal` — EDITED HERE. How this service words a request the
//!   framework refused before any handler ran: AWS's own envelope and clauses, with
//!   an enum's admissible value set listed in ascending order of its values. See
//!   [`sorted_enum_value_sets`] and the inline note on that method.
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

use aws_smithy_schema_server::{envelope, Violation, ViolationKind};
use service_common::query_error_envelope::RequestIdEnvelopeDefinition;
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Refusal, Refused, Service,
};
use std::sync::Arc;

pub mod sdk_envelope;

/// The error code this service answers a boundary constraint refusal with.
///
/// It is the code the framework's own restJson1 default already picks, and the code
/// every operation of this service declares for a bad request
/// (`smithy_query(query="operation",operation="ListMonitors")`:
/// `ValidationException`, HTTP 400). It is named here because a chosen message has
/// to travel with its matching code — a reworded message under a different code
/// would change what a client branches on.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `violations` with every enum requirement's value set listed in ASCENDING ORDER of
/// its values, and every other violation exactly as the framework stated it.
///
/// THE RULE: a constraint-validation message states the admissible set of an
/// enumerated member sorted by value, not in the order the model happens to declare
/// the enum in. The order is derived from the values themselves, so it holds for
/// every enum this service declares — the monitor status a listing filters on, the
/// metric and destination category a query is started with — rather than for the one
/// member a recording covers. Nothing else about the clause is this service's to
/// choose: the envelope, the member path, the echoed value and every other
/// requirement come straight back from [`envelope`], so they cannot drift from the
/// framework's.
///
/// The structurally right home for this rule is the framework's own
/// `Violation::requirement` (`vendor/smithy-schema-server/src/constraints.rs`), which
/// a fix may not change from here, or the shared `override_refusal` body in
/// `services/common/src/validation_message.rs` — whose seam today chooses only how a
/// `@pattern` is quoted, and whose services' recordings do not all agree that a value
/// set is sorted. Until one of the two learns the rule, it is stated here, for the
/// one service whose recordings show it.
fn sorted_enum_value_sets(violations: &[Violation]) -> Vec<Violation> {
    violations
        .iter()
        .map(|violation| match &violation.kind {
            ViolationKind::EnumValue(allowed) => {
                let mut sorted = allowed.clone();
                sorted.sort_unstable();
                Violation {
                    path: violation.path.clone(),
                    kind: ViolationKind::EnumValue(sorted),
                    value: violation.value.clone(),
                    sensitive: violation.sensitive,
                }
            }
            _ => violation.clone(),
        })
        .collect()
}

/// The `com-amazonaws-networkflowmonitor-networkflowmonitor` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    /// The generated entry, with its three framework-level error renderers wrapped so a
    /// response no operation produced still names the request it answers. Identity, the
    /// protocol and the routing table are that entry's own, untouched.
    entry: RequestIdEnvelopeDefinition<crate::ServiceDefinition<Eng>>,
    /// The dispatch `override_operation` hands back: the operation the generated routing
    /// table names, under the layer that finishes every response this service's operations
    /// produce ([`sdk_envelope`]). Held on the service because that seam returns a borrow of
    /// `self`.
    served: sdk_envelope::SdkEnvelopeOperation,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            // The SAME report for the responses an operation produced and the responses no
            // operation produced, so one request is named one way whichever layer answered.
            entry: RequestIdEnvelopeDefinition::reporting(
                crate::ServiceDefinition::new(engine),
                sdk_envelope::report,
            ),
            served: sdk_envelope::SdkEnvelopeOperation,
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry: what is
        // handed back is the shared `RequestIdEnvelopeDefinition`, which reads identity,
        // protocol and the routing table straight off that entry and names the request each
        // of the three framework-level error renderers answers ([`sdk_envelope::report`]).
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
        // Serve the operation the generated routing table names, under the layer that
        // finishes the response ([`sdk_envelope`]). The routing DECISION is not overridden:
        // the key is resolved by the model's own protocol, and a request the table cannot
        // serve is declined here so the framework's routing-error path stays as it was.
        self.entry
            .routes(req)
            .then_some(&self.served as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this service
        // reword it.
        //
        // THE CODE is `ValidationException` — the modelled 400 for a bad request on
        // every operation of this service, and the code the framework's own default
        // picks here, so what a client branches on is unchanged.
        //
        // THE MESSAGE is the framework's own envelope and clauses, with ONE rule of
        // this service's applied on top: an enum's admissible value set is listed in
        // ascending order of its values (`sorted_enum_value_sets`).
        //
        // An authentication refusal keeps the framework's wording and its 403: it is
        // not an input-constraint failure.
        //
        // A boundary refusal also keeps the framework's SHAPE whole: it closes at the
        // constraint clause, with none of the envelope a refusal of this service's own
        // carries (`sdk_envelope`). That envelope is stated by the layer that words a
        // refusal this service DECIDED, and a constraint clause is not one of those: it
        // is the framework's sentence about the request, reordered here in one respect
        // and otherwise passed through.
        match refusal {
            Refusal::Invalid(violations) => Some(Refused {
                code: VALIDATION_EXCEPTION.to_string(),
                message: envelope(&sorted_enum_value_sets(violations))?,
            }),
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
