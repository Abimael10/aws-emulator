// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-autoscaling-autoscaling-2011-01-01` service: how it claims requests and how it words the
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
//! - `override_protocol` — the codec this service reads a request with and answers in.
//!   The default returns `None`, keeping the framework's model-derived one. Taken here
//!   for the lexical space of a boolean member's text (see [`xsd_boolean`]).
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
//! Two of those seams are taken here, both for the one reason named in
//! [`query_error_envelope`]: an awsQuery error response carries a `<RequestId>` element
//! in its `<ErrorResponse>` envelope, which the framework's renderers omit.
//! `override_operation` wraps the operation the routing table names, so the element
//! reaches a handler-returned error, the scaffold's 501 and a refusal raised while
//! serving it, and the [`AutoScalingDefinition`] this file hands back from `definition`
//! adds it to a routing failure, a transport failure and a pre-dispatch refusal.
//! Everything else about those paths — status, `<Type>`, `<Code>`, `<Message>` — stays
//! with the framework.
//!
//! That wrapper is also where the operations whose response carries a floating-point
//! member are served behind the renderer that writes one as AWS does ([`decimal_floats`]):
//! the rule is the XML protocol's, so it is applied once for every such operation rather
//! than per handler.
//!
//! That same wrapper is where the AUTHORIZATION gate runs ([`authorization`]): whether the
//! caller may perform the action at all is a fact of the CREDENTIAL the request presented
//! and the IAM policies in force on the principal it speaks for, not of the input any one
//! operation reads, so it is decided once for every request this service serves rather
//! than per handler.
//!
//! `override_refusal` is taken for one reason beyond the error code: the value set a
//! refusal of an enumerated member STATES, and the order it lists it in, are the service's
//! own rather than the model's ([`enum_value_sets`]).

pub mod authorization;
pub mod decimal_floats;
pub mod enum_value_sets;
pub mod malformed_input;
pub mod query_error_envelope;
pub mod xsd_boolean;

use query_error_envelope::{AutoScalingDefinition, RequestIdEnvelopeOperation};
use service_common::query_boolean_values::QueryBooleanValues;
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Refusal, Refused, ServerHttpProtocol, Service,
};
use std::sync::Arc;

/// The error code Amazon EC2 Auto Scaling answers every refusal of a request's ARGUMENTS
/// with.
///
/// One code for the whole class: a request whose values do not satisfy the rules the model
/// declares, whichever rule and whichever member. None of this service's operations models
/// an input-validation fault, so this is the awsQuery common client error every one of them
/// reports an argument problem through. The handlers in `src/ops/` answer it through
/// `crate::scaffolded::Scaffolded::validation_error`, and the framework's own constraint
/// walk answers it through [`EmulatedService::override_refusal`] below.
///
/// A member whose TEXT the front door could not read at all is a different layer, and only
/// one of its classes is stated under this code — the boolean one, as recorded behaviour
/// has it (see [`malformed_input`]).
pub(crate) const VALIDATION_ERROR_CODE: &str = "ValidationError";

/// The `com-amazonaws-autoscaling-autoscaling-2011-01-01` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: AutoScalingDefinition<Eng>,
    /// The envelope wrapper every routable request is served through. Held in a field,
    /// built once, because `override_operation` hands back a reference to it.
    envelope: RequestIdEnvelopeOperation<Eng>,
    /// The protocol this service reads a request with: the framework's `awsQuery`, reading a
    /// boolean member's text in the lexical space the Query wire carries one in (see
    /// [`xsd_boolean`]). Held in a field, built once, because `override_protocol` hands back
    /// a reference to it.
    protocol: QueryBooleanValues,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: AutoScalingDefinition::new(Arc::clone(&engine)),
            envelope: RequestIdEnvelopeOperation::new(engine),
            protocol: xsd_boolean::protocol(),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn ErasedServiceDefinition {
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
        // envelope wrapper, which delegates to that same table entry and then adds the
        // `<RequestId>` the awsQuery error envelope carries (see
        // [`query_error_envelope`]). The routing stays the model's: a request whose
        // `Action` names no modelled operation is declined here, so the framework answers
        // it as the unroutable request it is — through the definition above, which adds
        // the element to that answer too.
        self.entry.routes(req).then_some(&self.envelope as _)
    }

    fn override_protocol(&self) -> Option<&dyn ServerHttpProtocol> {
        // The one thing this service reads differently from the framework's own `awsQuery`
        // codec: the TEXT of a boolean member, whose lexical space on this wire is wider
        // than the two words that codec reads. Everything else about the protocol — routing,
        // the response envelope, the error envelope, the namespace — is the framework's
        // unchanged. See [`xsd_boolean`].
        Some(&self.protocol)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // Amazon EC2 Auto Scaling answers EVERY problem with a request's arguments it READ
        // under one error code: `ValidationError`. An absent required member is one of them
        // — the recorded refusal of a request omitting `ShouldDecrementDesiredCapacity`
        // carries that code — and absence reaches this hook as a refusal of the value layer,
        // so it is stated here with every other rule the model declares. A member whose text
        // could not be READ at all does not reach this hook: that layer is answered in
        // [`malformed_input`], where the recorded code of each of its classes is stated.
        //
        // The message stays the framework's own: its constraint envelope
        // (`N validation error(s) detected: <clause>`) is the wording this service states
        // such a refusal in, so it is rendered from the violations rather than reworded —
        // with one exception, which is a fact of the SERVICE rather than of the model: the
        // value set a refusal of an enumerated member states, and the order it lists it in
        // ([`enum_value_sets`]). That set is restated on the VIOLATION, so the clause, the
        // member path and the echoed value stay with the framework.
        match refusal {
            Refusal::Invalid(violations) => Some(Refused {
                code: VALIDATION_ERROR_CODE.to_string(),
                message: aws_smithy_schema_server::envelope(&enum_value_sets::in_published_order(
                    violations,
                ))?,
            }),
            // An authentication refusal is not about the input at all, and the framework's
            // own code and wording for it are what AWS answers.
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
