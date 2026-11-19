// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-iotdataplane-iotmoonrakerservice` service: how it claims requests and how it words the
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
//! ## The media type a shadow document is announced with
//!
//! The data plane answers a request whose response body is a shadow state document
//! with `content-type: application/json`: the body IS a JSON document, and a client
//! or proxy decides whether to decode it from that header. On the wire that document
//! is the `JsonDocument` blob these operations bind as their `@httpPayload`
//! (`smithy_query(query="shape", shape="GetThingShadowResponse,UpdateThingShadowResponse,DeleteThingShadowResponse,JsonDocument")`),
//! and restJson1's generic default for a bound blob — `application/octet-stream` —
//! describes opaque bytes, not a JSON document. `override_protocol` below therefore
//! serves a protocol that is the model's restJson1 in every respect except one: a
//! response whose `@httpPayload` is this service's `JsonDocument` shape is announced
//! as `application/json`. Every other response, including the blob members
//! `GetRetainedMessage` carries in its JSON body, keeps exactly what the model's
//! protocol produces.
//!
//! ## The authorization decision in front of every operation
//!
//! AWS answers the IAM decision at the layer in FRONT of a service, before the request's
//! own shape is looked at, so every request this service serves passes through
//! [`AuthorizationGate`] first: a caller whose identity-based policies do not allow
//! `iot:<OperationName>` on the resource the request addresses is refused
//! `ForbiddenException` (HTTP 403) and the call performs nothing — no message is
//! published, no rule runs and no shadow moves ([`authorization`]). `override_operation`
//! is the only seam that can run ahead of the framework's deserialize -> validate ->
//! handle walk, which is why the gate is picked there; every request it does not refuse
//! is served by the modelled operation the request's own routing key names.
//!
//! ## How a request the front door refuses is worded
//!
//! A request the framework refuses before any handler runs — a value outside a modelled
//! bound, a `@required` member that is absent — is worded by this plane itself, and the
//! whole of that is [`input_validation`]: the error class and the sentence are chosen at
//! `override_refusal` and the body and status are completed over the rendered response,
//! because that seam is handed the violations alone. The names this plane addresses a
//! resource by each have their own fixed sentence there, and every failure body it answers
//! carries the `traceId` member beside `message`.

use smithy_server_runtime::{
    http, BoxFuture, ErasedOperation, OperationContext, Refusal, Refused, Service,
};
use std::sync::Arc;

/// Whether IAM lets the principal a request speaks for perform the action it asks for, taken
/// in front of every operation of this service ([`AuthorizationGate`]).
pub mod authorization;

/// How this plane words a request the framework refused before any handler ran, and the
/// envelope every failure it answers is completed in ([`input_validation`]).
pub mod input_validation;

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};
use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_schema_server::{ProtocolError, ServerProtocol};

/// The media type the data plane announces for a shadow state document.
const JSON_DOCUMENT_MEDIA_TYPE: &str = "application/json";

/// Every output shape of this service whose `@httpPayload` member targets the
/// `JsonDocument` shape.
///
/// Source: `smithy_query(query="shape", shape="GetThingShadowResponse,
/// UpdateThingShadowResponse,DeleteThingShadowResponse,JsonDocument")` — each
/// declares the single member `payload` with `target: JsonDocument`,
/// `httpPayload: true`, `type: blob`, and `JsonDocument` is a `blob`. No other
/// operation of this service binds a `@httpPayload` in its output
/// (`smithy_query(query="digest")` lists all 11), so this is the complete set. The
/// set is recorded as output shapes because a runtime `Schema` member carries its
/// target's TYPE but not its target's shape id, so `JsonDocument` cannot be told
/// from any other blob at serialize time.
const JSON_DOCUMENT_PAYLOAD_OUTPUTS: &[&str] = &[
    "DeleteThingShadowResponse",
    "GetThingShadowResponse",
    "UpdateThingShadowResponse",
];

/// True when this output shape's response body is a `JsonDocument` — a
/// `@httpPayload` blob member on one of the shapes [`JSON_DOCUMENT_PAYLOAD_OUTPUTS`]
/// names. Both halves are required: the shape must be one the model gives a
/// `JsonDocument` payload, and the schema it is serialized against must still bind
/// that payload as a blob.
fn carries_json_document(output_schema: &Schema) -> bool {
    let id = output_schema.shape_id();
    if id.namespace() != "com.amazonaws.iotdataplane"
        || !JSON_DOCUMENT_PAYLOAD_OUTPUTS.contains(&id.shape_name())
    {
        return false;
    }
    output_schema
        .members()
        .iter()
        .any(|member| member.http_payload().is_some() && member.shape_type() == ShapeType::Blob)
}

/// The `com-amazonaws-iotdataplane-iotmoonrakerservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// THE AUTHORIZATION GATE every request of this service passes through, which serves the
    /// modelled operation the request named once IAM permits the action
    /// ([`AuthorizationGate`]).
    gate: AuthorizationGate<Eng>,
}

/// THE GATE every request this service serves passes through, as an operation the dispatch
/// path can run: whether IAM permits the ACTION the request named
/// ([`authorization::authorize`]).
///
/// The order is AWS's: authorization is decided by the IAM layer in FRONT of the service, so it
/// is answered before anything about the request's own resource is resolved, let alone
/// deserialized or constraint-checked. A request it does not refuse is handed to the modelled
/// operation the request named, through the envelope wrapper that completes what the walk
/// renders ([`input_validation::REFUSAL_ENVELOPE`], which resolves the same routing table), so
/// it is served by exactly the centralized deserialize -> validate -> handle walk every other
/// request takes: the gate changes WHICH refusal comes first, not whether the modelled
/// constraints are enforced.
///
/// It names no operation of its own, because the operation is the routing key the request
/// carries: ONE gate stands in front of all of this service's operations, and the action it
/// judges is that key under AWS IoT Core's own IAM prefix.
pub struct AuthorizationGate<Eng> {
    /// The same shared engine the entry constructed each operation from, so the decision reads
    /// the account's own IAM and STS stores rather than a second copy of them.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> AuthorizationGate<Eng> {
    /// A gate on the shared engine.
    fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for AuthorizationGate<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, WireResponse> {
        Box::pin(async move {
            // The operation the request named, read exactly as dispatch reads it: the routing
            // key this service's protocol resolves from the request line. It is the ACTION the
            // IAM decision is about, and the key the modelled operation is wired under.
            let Ok(operation) = svc.definition().protocol().routing_key(req) else {
                // `override_operation` picks this gate only for a request whose key resolves,
                // so this is unreachable; answered as the request naming no modelled
                // operation, which is what it would be.
                return svc.definition().routing_error_response(req);
            };
            // The resource the request ADDRESSES, in the Region it was signed for, which is
            // the Region segment of the ARN a statement about it names. The Region is read the
            // way the registry beside this plane reads it, so a thing or a topic is named by
            // the ARN that service mints for the same request.
            let resource = authorization::judged_resource(
                &::service_com_amazonaws_iot_awsiotservice::state::request_region(ctx.parts()),
                operation,
                ::service_common::request_path(req.uri()),
            );
            // IAM's decision, taken before the operation reads or writes anything. A refused
            // request never reaches the delegate, so it publishes nothing and moves nothing.
            // The answer states no reason — see [`authorization`] — so the sentence IAM
            // refused with goes to this service's own log instead.
            if let Err(denial) = authorization::authorize(&*self.engine, ctx, operation, &resource)
            {
                ::tracing::debug!(
                    operation,
                    reason = %denial.reason,
                    "an iot data plane request was refused by the account's identity policies"
                );
                return authorization::refusal_response();
            }
            // The modelled operation the request named, served through the wrapper that
            // completes the envelope this plane writes every failure in: it resolves the
            // same routing table this gate would, so the whole deserialize -> validate ->
            // handle walk, every handler's answer and every modelled status stay the
            // framework's ([`input_validation`]).
            input_validation::REFUSAL_ENVELOPE.run(svc, req, ctx).await
        })
    }

    /// `None`, NOT an empty list: the request is still signed as the operation the MODEL names,
    /// so wrapping every operation in this gate changes which schemes sign none of them.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            gate: AuthorizationGate::new(engine),
        }
    }
}

/// The protocol this service answers modeled requests in: the model's restJson1,
/// with the one media-type correction the module docs state.
///
/// It is implemented on the service itself because the model-derived protocol it
/// delegates to is owned by the generated entry and reached by reference
/// (`self.entry.protocol()`), and [`Service::override_protocol`] hands back a borrow
/// of `&self` — so there is one protocol instance, the generated one, and no second
/// route table to drift from the model.
impl<Eng: Send + Sync + 'static> ServerProtocol for EmulatedService<Eng> {
    type Request = WireRequest;
    type Response = WireResponse;

    fn protocol_id(&self) -> &aws_smithy_schema::ShapeId {
        self.entry.protocol().protocol_id()
    }

    fn routing_key<'a>(&self, request: &'a WireRequest) -> Result<&'a str, ProtocolError> {
        self.entry.protocol().routing_key(request)
    }

    fn deserialize_input<'a>(
        &self,
        request: &'a WireRequest,
        input_schema: &Schema,
    ) -> Result<Box<dyn aws_smithy_schema::serde::ShapeDeserializer + 'a>, ProtocolError> {
        self.entry
            .protocol()
            .deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn aws_smithy_schema::serde::SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<WireResponse, ProtocolError> {
        let mut response = self.entry.protocol().serialize_output(
            output,
            output_schema,
            operation_name,
            success_status,
        )?;
        // The body the protocol just wrote is the `JsonDocument` payload verbatim, so
        // only the media type announcing it changes — never the bytes, the status or
        // any other header.
        if carries_json_document(output_schema) {
            response
                .headers_mut()
                .insert("content-type", JSON_DOCUMENT_MEDIA_TYPE);
        }
        Ok(response)
    }

    fn serialize_error(
        &self,
        error: &dyn aws_smithy_schema::serde::SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<WireResponse, ProtocolError> {
        // A modelled failure carries no `@httpPayload`: it is the protocol's JSON
        // error envelope, which already announces `application/json`.
        self.entry
            .protocol()
            .serialize_error(error, error_schema, error_code, http_status)
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn aws_smithy_schema::serde::SerializableStruct,
        event_schema: &Schema,
    ) -> Result<WireResponse, ProtocolError> {
        self.entry.protocol().serialize_event_stream_message(
            event_type,
            is_exception,
            event,
            event_schema,
        )
    }
}

/// `ServerProtocol` requires `Debug`; the generated entry is not `Debug`, so this
/// names the protocol being served rather than the service's internals.
impl<Eng: Send + Sync + 'static> std::fmt::Debug for EmulatedService<Eng> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmulatedService")
            .field("protocol", &self.entry.protocol().protocol_id())
            .field("json_document_payloads", &JSON_DOCUMENT_PAYLOAD_OUTPUTS)
            .field("action_prefix", &authorization::ACTION_PREFIX)
            .finish()
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
        // THE AUTHORIZATION GATE, and the only seam it can run in: a request is authorized
        // before its shape is validated, because the framework's centralized walk is
        // deserialize -> validate -> handle and a handler therefore runs only AFTER
        // modelled-constraint enforcement, while AWS answers the IAM decision in front of all
        // of it ([`AuthorizationGate`]). NOT a re-route: the gate serves the very operation
        // the model's own routing key names, so a key the model does not name resolves to
        // `None` here exactly as it does in the generated table.
        self.entry.protocol().routing_key(req).ok()?;
        Some(&self.gate as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this service
        // reword it. WHAT this plane states for each class of refusal, and the envelope
        // it completes the rendered answer in, are [`input_validation`] — the one place
        // that wording lives, because the body it chooses here is finished at the other
        // seam.
        //
        // The conditions a HANDLER reports are a different thing and keep their own
        // specific text (`crate::state` holds that catalog): they are the service's own
        // rules, not the front door's.
        input_validation::stated_refusal(refusal)
    }

    fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
        // Every response a modeled operation of this service produces is serialized
        // by the protocol returned here, so this is the one place the media type it
        // announces is decided. See the module docs: a shadow state document — the
        // `JsonDocument` blob these operations bind as their `@httpPayload` — is
        // announced `application/json`, and every other response is left exactly as
        // the model's restJson1 protocol renders it.
        Some(self)
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
