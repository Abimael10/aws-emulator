// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-redshift-redshiftserviceversion20121201` service: how it claims requests and how it words the
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
//!   before any handler ran. Taken here: every input refusal carries this
//!   service's one input-refusal code ([`INPUT_REFUSAL_CODE`]) over the shared
//!   rendering of AWS's envelope.
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
//! serving it, and the [`RedshiftDefinition`] this file hands back from `definition` adds
//! it to a routing failure, a transport failure and a pre-dispatch refusal. Everything
//! else about those paths — status, `<Type>`, `<Code>`, `<Message>` — stays with the
//! framework.

pub mod query_error_envelope;
pub mod stated_enum_value_sets;

use query_error_envelope::{RedshiftDefinition, RequestIdEnvelopeOperation};
use smithy_server_runtime::{
    http, ErasedOperation, ErasedServiceDefinition, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The error code Amazon Redshift names a refusal of a request's INPUT with: a
/// `@required` member the request did not carry, and a value outside a bound,
/// a pattern or an enumerated set the model declares.
///
/// It is the code the query protocols name a refused VALUE with
/// (`vendor/smithy-server-runtime/src/pre_dispatch.rs`), and it is stated here for
/// every input refusal because that is what the recordings show: the framework
/// names a request whose absent required member the codec found while reading it
/// with the other query code (`MalformedInput`), while Amazon Redshift answers an
/// absent required member with the same validation code a bad value gets. A client
/// branches on the code, so the two layers' findings are not told apart by it.
const INPUT_REFUSAL_CODE: &str = "ValidationError";

/// The expressions this API publishes for a shape EXACTLY as the model declares them,
/// whole-value anchors included, rather than as the body inside that pair.
///
/// One API can publish both forms, and which of the two a shape takes cannot be read off
/// the text: it is reference data about the shape
/// ([`::service_common::validation::unanchored_unless_published_as_declared`], which owns
/// the rule). The row here is the zero-ETL source ARN
/// (`smithy_query(query="operation",operation="CreateIntegration")` reports `SourceArn`
/// with this `@pattern`), whose refusal this surface is recorded answering with the
/// model's text; every other shape of this model is published as the body alone, which is
/// the fleet's default rendering.
const PUBLISHED_AS_DECLARED: &[&str] = &[SOURCE_ARN_PATTERN];

/// The modelled `@pattern` of `SourceArn`, the shape a zero-ETL integration's source is
/// named by (`smithy_query(query="operation",operation="CreateIntegration")`), spelled out
/// here because it is the key [`PUBLISHED_AS_DECLARED`] names the shape by.
const SOURCE_ARN_PATTERN: &str = r"^arn:aws[a-z\-]*:(s3|dynamodb):.*:.*:[a-zA-Z0-9._\-\/]+$";

/// How this API quotes a `@pattern` requirement in a refusal: the expression the SERVICE
/// publishes for the member's shape.
///
/// For a shape [`PUBLISHED_AS_DECLARED`] names that is the model's text verbatim, and for
/// every other shape it is that text without the whole-value pair the model wrapped it in.
/// The RULE is the fleet's one home for it, called rather than restated, so this API cannot
/// drift from another recorded stating both forms; what is this crate's is the row above,
/// which is about this model's shapes. The CHECK is always the model's pattern as written.
fn quoted_pattern(pattern: &str) -> String {
    ::service_common::validation::unanchored_unless_published_as_declared(
        pattern,
        PUBLISHED_AS_DECLARED,
    )
}

/// The `com-amazonaws-redshift-redshiftserviceversion20121201` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: RedshiftDefinition<Eng>,
    /// The envelope wrapper every routable request is served through. Held in a field,
    /// built once, because `override_operation` hands back a reference to it.
    envelope: RequestIdEnvelopeOperation,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: RedshiftDefinition::new(engine),
            envelope: RequestIdEnvelopeOperation,
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

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this
        // service state it. Every such refusal of this service carries ONE code,
        // [`INPUT_REFUSAL_CODE`], whichever layer of the walk found it — which is
        // what this seam is for: the framework tells an absent required member
        // apart from a value outside a bound, because the codec finds the first
        // while READING the request and the walk finds the second afterwards, and
        // names them with two different query-protocol codes. Amazon Redshift
        // names both with the one validation code, so a client branching on the
        // code cannot see which layer refused it.
        //
        // The MESSAGE is the shared rendering of AWS's own envelope over the
        // framework's clauses (`service_common::validation_message`), so nothing
        // about the sentence — the count word, the member paths or the
        // requirements — is spelled per service. TWO things inside a clause are
        // this service's own and not the model's: the ORDER an enumerated type's
        // value set is listed in ([`stated_enum_value_sets`]), which is restated
        // on the VIOLATIONS before that rendering runs, and how a `@pattern`
        // requirement is quoted ([`quoted_pattern`]) — so the clause, the
        // envelope and the code all stay where they are written.
        //
        // An authentication refusal is declined: it is a 403 under its own code
        // rather than an input refusal, and this seam is handed no detail of
        // which credential failure it was.
        let stated = stated_enum_value_sets::in_stated_order(refusal);
        let message = ::service_common::validation_message::aws_validation_refusal_with(
            &stated,
            quoted_pattern,
        )?
        .message;
        Some(Refused {
            code: INPUT_REFUSAL_CODE.to_string(),
            message,
        })
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
