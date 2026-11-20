// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-chimesdkvoice-chimesdktelephonyservice` service: how it claims requests and how it words the
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
//! ## The two things every answer of this service carries
//!
//! Both are stated once here rather than per operation, because both hold for every
//! response this service renders:
//!
//! - a request the framework refused before any handler ran is stated in AWS's standard
//!   validation report — the COUNTED envelope `N validation error(s) detected: …` — which
//!   `service_common::validation_message` words for every emulated service, under the
//!   framework's own validation error class rather than under one of this model's shapes;
//! - every client-error body names the request it answers in a `RequestId` member, which
//!   the shared request-id report writes ([`query_error_envelope`]): the member belongs to
//!   this service's error ENVELOPE and no model declares it, so no handler can set it.
//!
//! One thing more is answered ahead of the modelled walk, for the reason its own module
//! gives: the Voice Connectors a group's routing references ([`referenced_connectors`]).

use service_common::query_error_envelope::{
    with_request_id_error_member_named, RequestIdEnvelopeDefinition, RequestIdReportedOperation,
};
use smithy_server_runtime::{http, ErasedOperation, Refusal, Refused, Service, Violation};
use std::sync::Arc;

/// The Voice Connectors a group's routing references, resolved ahead of the modelled walk.
mod referenced_connectors;

/// The member this service's JSON error bodies name the request they answer in.
///
/// Capitalized, beside the `Code` and `Message` the model declares capitalized
/// (`smithy_query(query="shape", shape="NotFoundException")`), which is how this service's
/// recorded error bodies spell it. The id itself is minted per response by the shared
/// report; nothing here states its form.
const REQUEST_ID_MEMBER: &str = "RequestId";

/// How one response of this service names the request it answers: the `RequestId` member of
/// its JSON error body.
///
/// A `Report` the service hands BOTH seams that can answer — the wrapper around every
/// modelled operation and the wrapper around the framework-level renderers — so a response
/// an operation produced and a response no operation produced name their request the same
/// way. A success is left untouched: its body is the modelled output.
fn report_request_id(
    resp: aws_smithy_runtime_api::http::Response,
) -> aws_smithy_runtime_api::http::Response {
    with_request_id_error_member_named(resp, REQUEST_ID_MEMBER)
}

/// This service's own `aws.api#service$endpointPrefix`, the name that identifies it
/// among the six services that sign as `chime`.
///
/// Source: `smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-chimesdkvoice-chimesdktelephonyservice/\
/// smithy-model/com-amazonaws-chimesdkvoice-chimesdktelephonyservice.json")` —
/// `awsService.endpointPrefix`.
const OWN_ENDPOINT_PREFIX: &str = "voice-chime";

/// The `@pattern` expressions this API publishes exactly as the model declares them, anchors
/// included; every other expression is quoted without the whole-value `^`/`$` pair.
///
/// Keyed by the EXPRESSION, because what is published belongs to the member's SHAPE and so
/// holds for every member targeting it, in any operation. The one row is the ARN expression
/// `Arn` declares (`smithy_query(query="shape", shape="Arn")`), which every ARN-valued member
/// of this model targets — a voice profile domain's `KmsKeyArn` among them.
const PATTERNS_PUBLISHED_AS_DECLARED: &[&str] = &[r"^arn[\/\:\-\_\.a-zA-Z0-9]+$"];

/// The `com-amazonaws-chimesdkvoice-chimesdktelephonyservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    /// The generated entry, with its three framework-level error renderers wrapped so a
    /// response no operation produced names the request it answers too.
    entry: RequestIdEnvelopeDefinition<crate::ServiceDefinition<Eng>>,
    /// Serves every modelled operation, so each response passes the one place the request
    /// id is reported and a group's connector references are resolved ahead of the modelled
    /// walk. Held, not built per request: `override_operation` hands back a reference to it.
    served: referenced_connectors::ResolvedReferenceOperation<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: RequestIdEnvelopeDefinition::reporting(
                crate::ServiceDefinition::new(Arc::clone(&engine)),
                report_request_id,
            ),
            served: referenced_connectors::ResolvedReferenceOperation::new(
                engine,
                RequestIdReportedOperation::new(report_request_id),
            ),
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
        // This service signs as `chime` — a signing name it SHARES with the five other
        // Amazon Chime services in this workspace, each of which serves its OWN endpoint
        // (`chime`, `identity-chime`, `media-pipelines-chime`, `meetings-chime`,
        // `messaging-chime`). The router resolves a request's routing key from the SigV4
        // credential scope first, so all six see the key `chime`: claiming on the signing
        // name alone makes every one of them a claimant for every one of the others'
        // requests, and the router — which refuses rather than guesses — answers
        // `500 AmbiguousService` for ALL of it, including this service's own
        // `/voice-connectors` traffic.
        //
        // So the claim is keyed on what identifies ONE service: the endpoint the request
        // addresses, and failing that this model's own URI path space. That rule, and the
        // readers for the path spaces the family shares, live once in `service-common`.
        //
        // This model claims NONE of the shared spaces on a request that names no
        // endpoint: its telephony URIs (`/phone-numbers`, `/phone-number-orders`,
        // `/phone-number-countries`, `/settings`, `/search`) are the legacy Amazon Chime
        // endpoint's too, operation for operation, and `chime` — the key such a request
        // resolves to — is that endpoint's `endpointPrefix`. A caller reaches THIS
        // endpoint's half of that space by addressing `voice-chime`, which the first
        // conjunct answers. (`/tags` is shared by five of the six: a request that names
        // no endpoint is served by the family's declared tagging owner, and this service
        // serves that space at its OWN endpoint from the same shared ARN-keyed tag
        // directory, so both answer alike — see `chime_family`.)
        service_common::chime_family::claims_chime_family_request(
            parts,
            OWN_ENDPOINT_PREFIX,
            crate::ROUTE_PATH_SEGMENTS,
            |_| false,
        )
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // NOT a re-route: every request is served by the operation this service's OWN
        // routing table names for it, through the two things stated once for the whole
        // service — the request-id report, the one place every operation's response passes,
        // so no answer can leave without naming the request it answered (a success, a
        // modelled error, or a constraint refusal raised while the operation was being
        // served); and the reference resolution the modelled walk runs too late for
        // ([`referenced_connectors`]). A request the table does not answer declines here
        // exactly as it would there, leaving the router's routing-error path unchanged.
        self.entry
            .routes(req)
            .then_some(&self.served as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // A request the framework refused before any handler ran — an absent `@required`
        // member, a value outside a declared bound — is refused by AWS's own input
        // validation layer, which answers the COUNTED report
        // `N validation error(s) detected: <clause>; …` under the validation error class
        // that layer owns. That class is NOT one of this model's shapes: the modelled
        // `BadRequestException` ("The input parameters don't match the service's
        // restrictions.") is what an OPERATION answers for a rule it enforces itself, and
        // labelling a pre-handler validation failure with it tells a client that dispatches
        // on the error type that the service refused the request, not that the request never
        // reached it.
        //
        // The envelope, the per-violation clauses, the member paths and the count all belong
        // to `service_common::validation_message`, the shared home for this wording, so this
        // service cannot word a constraint differently from the services that state the same
        // sentence; and the code travels with the message from there too. TWO renderings
        // inside it are this service's own, and nothing else is:
        //
        // * a `@pattern` requirement quotes the expression this API PUBLISHES for the
        //   member's shape, which is the model's text without the whole-value `^`/`$` pair
        //   for most shapes and the text as declared for the shapes in
        //   [`PATTERNS_PUBLISHED_AS_DECLARED`]. The shared home for that keying decides it;
        //   the CHECK is always the model's pattern as written.
        // * an enumerated type's value set is listed in LEXICOGRAPHIC order
        //   (`in_sorted_enum_order`, the shared home for that order), whatever order this
        //   model declares the type's members in. Re-ordered BEFORE the clauses are
        //   rendered, so the clause itself still comes from the one place that writes it.
        //
        // An authentication refusal keeps the framework's wording: this service's modelled
        // `UnauthorizedClientException` is a 401 while that refusal is a 403, so renaming it
        // here would answer a 401 code with a 403 status.
        let Refusal::Invalid(violations) = refusal else {
            return None;
        };
        let stated: Vec<Violation> = violations
            .iter()
            .map(service_common::validation_message::in_sorted_enum_order)
            .collect();
        service_common::validation_message::aws_validation_refusal_with(
            &Refusal::Invalid(stated),
            |pattern| {
                service_common::validation::unanchored_unless_published_as_declared(
                    pattern,
                    PATTERNS_PUBLISHED_AS_DECLARED,
                )
            },
        )
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
