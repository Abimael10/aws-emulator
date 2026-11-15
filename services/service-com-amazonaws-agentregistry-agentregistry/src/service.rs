// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-agentregistry-agentregistry` service: how it claims requests and how it words the
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

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse, StatusCode};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::ViolationKind;
use aws_smithy_schema_server::{ProtocolError, ServerProtocol};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::sync::Arc;

/// The content type `restJson1` frames every response in, success or failure.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The modelled client error this service refuses a request it cannot bind to one of
/// its routes with: `ValidationException`, `httpError` 400
/// (`smithy_query(query="operation")` lists it on all three operations).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The HTTP status `ValidationException` declares.
const VALIDATION_EXCEPTION_STATUS: u16 = 400;

/// What this service states for a request addressed to its endpoint that names none of
/// its routes: RECORDED AWS behaviour for this rejection class is HTTP 400
/// `ValidationException` with this fixed sentence, which names no part of the request —
/// the condition is the same whatever path, method or body produced it.
const REQUEST_INVALID: &str = "Request invalid";

/// The member the search response reports the search mode in.
///
/// NOT MODELLED, in either direction: `SearchDiscoverableRegistryRecordsRequest` declares
/// no member that selects a mode and `SearchDiscoverableRegistryRecordsResponse` declares
/// only `registryRecords` (`smithy_query(query="shape",
/// shape="SearchDiscoverableRegistryRecordsRequest,SearchDiscoverableRegistryRecordsResponse")`
/// — the name appears nowhere in this model). So nothing in a request can choose it and no
/// modelled value varies it; RECORDED AWS behaviour carries it, with the same value, on
/// every successful search. It is a property of the search PROCEDURE that ran, and this
/// service runs exactly one.
const SEARCH_TYPE_MEMBER: &str = "searchType";

/// The one search mode this service runs, and the mode the recorded answers report: a
/// query is matched against each record's stored text, most specific field first, rather
/// than by one of keyword or vector search alone.
const SEARCH_TYPE_HYBRID: &str = "HYBRID";

/// The operation whose answer carries [`SEARCH_TYPE_MEMBER`]: the routing key, which for
/// a REST protocol is the operation's shape name.
const SEARCH_OPERATION: &str = "SearchDiscoverableRegistryRecords";

/// The `com-amazonaws-agentregistry-agentregistry` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// The codec every modelled body of this service is read with and written with, built
    /// ONCE here because [`Service::override_protocol`] hands back a borrow of `&self` and
    /// must never build a protocol per request. It is the model's own `restJson1` with the
    /// timestamp form this model declares; see the [`ServerProtocol`] impl below.
    body_codec: service_common::date_time_json::DateTimeRestJson,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
            body_codec: service_common::date_time_json::DateTimeRestJson::new(),
        }
    }
}

/// The protocol this service answers modelled requests in: the model's own `restJson1`,
/// with the two corrections this service's RECORDED answers show.
///
/// * A timestamp goes onto the wire in the form its target timestamp shape declares. Every
///   timestamp member of this model targets `DateTimestamp`, which carries
///   `@timestampFormat("date-time")` (`smithy_query(query="shape", shape="DateTimestamp")`),
///   so each one is an RFC 3339 date-time STRING and not the epoch-seconds number
///   `restJson1` defaults to — `service_common::date_time_json` holds that rule, and why a
///   generated member schema cannot carry the format itself.
/// * A successful `SearchDiscoverableRegistryRecords` carries `searchType` beside the
///   modelled `registryRecords`, and no output shape of this model declares that member, so
///   the serializer cannot write it: this is where it is added.
///
/// ROUTING stays the generated entry's — `protocol_id` and `routing_key` are read off the
/// model's own protocol, whose table is the model's — so no second routing table can drift
/// from the model, and every modelled error is rendered by the corrected codec exactly as
/// every modelled success is.
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
        // Read through the same codec the answer is written with, so a timestamp a request
        // carries is parsed in the form this model declares for it.
        self.body_codec.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn aws_smithy_schema::serde::SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<WireResponse, ProtocolError> {
        let response = self.body_codec.serialize_output(
            output,
            output_schema,
            operation_name,
            success_status,
        )?;
        if operation_name != SEARCH_OPERATION {
            return Ok(response);
        }
        Ok(with_search_type(response))
    }

    fn serialize_error(
        &self,
        error: &dyn aws_smithy_schema::serde::SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<WireResponse, ProtocolError> {
        self.body_codec
            .serialize_error(error, error_schema, error_code, http_status)
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn aws_smithy_schema::serde::SerializableStruct,
        event_schema: &Schema,
    ) -> Result<WireResponse, ProtocolError> {
        self.body_codec.serialize_event_stream_message(
            event_type,
            is_exception,
            event,
            event_schema,
        )
    }
}

/// `ServerProtocol` requires `Debug`; the generated entry is not `Debug`, so this names
/// the protocol being served rather than the service's internals.
impl<Eng: Send + Sync + 'static> std::fmt::Debug for EmulatedService<Eng> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmulatedService")
            .field("protocol", &self.entry.protocol().protocol_id())
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
        // AWS dispatches a REST request by the ENDPOINT it was addressed to, and this
        // service's endpoint is what the routing key `agent-registry` names: its
        // `endpointPrefix` IS its SigV4 signing name (`smithy_query(query="service_info")`),
        // so a host-addressed request and a signed one resolve that one key and the
        // identity's own signing name is the whole of what this endpoint answers on.
        //
        // That key is SHARED with the Agent Registry CONTROL plane
        // (`services/service-com-amazonaws-agentregistrycontrol-agentregistrycontrol`),
        // which signs as `agent-registry` while its endpoint is `agent-registry-control`:
        // every control-plane request therefore resolves this key too, and both services
        // claim it. The router then keeps the claimant whose own model names an operation
        // for the request, and the two models bind no identical method + URI pattern, so a
        // request either plane binds a route for is served by that plane alone.
        //
        // What is left is a request that resolves THIS endpoint and that NEITHER model
        // binds a route for — an unbindable path, a method no route carries. AWS answers
        // that at the endpoint, with the endpoint's own modelled client error, so this
        // service claims it rather than letting it fall through to the router's
        // cross-protocol `404 UnknownService` (a code no model of this service declares,
        // framed as `application/x-amz-json-1.1` although this service is `restJson1`).
        // [`EmulatedService::override_routing_error`] is what words it.
        service_common::routes_to(parts, self.entry.identity().signing_name())
    }

    fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
        // Every modelled answer of this service goes through the protocol returned here:
        // the model's own, with the two corrections the [`ServerProtocol`] impl above
        // states — every timestamp in the `date-time` form this model's one timestamp
        // shape declares, and the `searchType` member a recorded successful search carries.
        Some(self)
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
        // required member, or a value outside a declared bound) to let this service
        // reword it. What this service states, and all it states, is in
        // [`stated_refusal`]: the envelope, the error count, the member paths and
        // every requirement sentence stay the framework's, and an authentication
        // refusal keeps its own code and wording.
        stated_refusal(refusal)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no modelled
        // operation — which, with the claim rule above, is every request addressed to this
        // endpoint whose method + URI neither Agent Registry model binds a route for.
        // AWS refuses that class at the endpoint with the service's own modelled client
        // error, so this service answers `400 ValidationException` with the fixed
        // [`REQUEST_INVALID`] sentence, framed as `application/json` like every other
        // answer of a `restJson1` service. The default here would be the entry's REST
        // rendering (a 501, which claims the route exists but is unimplemented).
        let _ = req;
        Some(request_invalid_response())
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

/// The answer to a request addressed to this endpoint that names none of its routes: the
/// modelled `ValidationException` discriminator at its declared status, with the fixed
/// sentence this rejection class carries.
///
/// Rendered here rather than through the modelled serializer because no operation was
/// resolved, so there is no error schema to serialize against — the body is the two
/// members a `restJson1` error envelope carries (`__type` and `message`) and the header
/// an SDK resolves the error shape from. `reason` is left out: it is modelled with a
/// `@default`, which a client reads an absent member as, and this refusal is about no
/// field of the request.
fn request_invalid_response() -> WireResponse {
    let body = ::serde_json::json!({
        service_common::error_type_locator::SHAPE_DISCRIMINATOR: VALIDATION_EXCEPTION,
        "message": REQUEST_INVALID,
    });
    let mut response = WireResponse::new(
        StatusCode::try_from(VALIDATION_EXCEPTION_STATUS)
            .expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response.headers_mut().insert(
        service_common::error_type_locator::ERROR_TYPE_HEADER,
        VALIDATION_EXCEPTION,
    );
    response
}

/// The search answer with the `searchType` member the recorded responses carry added to
/// the modelled body.
///
/// The serialized body is re-read as JSON and the member inserted, because the member is
/// in no schema the serializer can be driven with. The codec renders every answer of this
/// protocol into memory as a JSON object, so the two guards below cannot be taken by a
/// rendered search answer; they are there so nothing here can turn a well-formed answer
/// into a malformed one. An existing member is never overwritten.
fn with_search_type(mut response: WireResponse) -> WireResponse {
    let Ok(::serde_json::Value::Object(mut members)) = response
        .body()
        .bytes()
        .map(::serde_json::from_slice::<::serde_json::Value>)
        .unwrap_or_else(|| Ok(::serde_json::Value::Null))
    else {
        return response;
    };
    members
        .entry(SEARCH_TYPE_MEMBER.to_string())
        .or_insert_with(|| ::serde_json::Value::String(SEARCH_TYPE_HYBRID.to_string()));
    let rendered = ::serde_json::Value::Object(members).to_string();
    // A body that is replaced states the length it now has, when the renderer stated one
    // at all: a stale `content-length` would describe the body that was there before. A
    // decimal length is a valid header value, so this cannot fail.
    if response.headers().get("content-length").is_some() {
        response
            .headers_mut()
            .insert("content-length", rendered.len().to_string());
    }
    *response.body_mut() = SdkBody::from(rendered);
    response
}

/// How this service states the framework's constraint refusal, or `None` to keep the
/// framework's own wording for anything that is not one.
///
/// Three rules, and nothing else:
///
/// * NO clause quotes a value. Every one names the member it was stated at and the
///   requirement that member's value had to satisfy, whatever the request carried —
///   so the sentence a caller reads back is the same for every value outside the same
///   bound, and nothing of a request is echoed into it.
/// * a constraint the ELEMENTS of a collection member declare is stated at that
///   MEMBER, with the element shape's whole declared set nested inside it
///   ([`element_set`]) rather than at the one element that offended.
/// * a `@pattern` is quoted as this service advertises the expression: without the
///   pair of whole-value anchors the model wraps it in, so a caller that re-applies
///   the expression it was handed back uses the same one. The CHECK still runs the
///   model's pattern as written; only the rendering drops the pair.
///
/// The envelope, its error count, the clause, the member paths, the grouping and every
/// requirement sentence belong to the shared home for this wording and to the framework
/// behind it — none of them is spelled here, so this service cannot drift from the
/// others that state the same sentences. An authentication refusal is a different
/// failure with its own code and status and nothing about it is this service's to word.
fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    service_common::validation_message::aws_validation_refusal_over_elements_stating_withholding(
        refusal,
        service_common::validation::unanchored,
        // Every clause, whatever member or class it is about.
        |_| true,
        element_set,
    )
}

/// Every shape of this service whose own members a refused COLLECTION is looked up in:
/// the three operation inputs, and the structures they nest.
///
/// A member schema carries its own traits and not the members of the shape it targets,
/// so a list declared inside a structure is reached by handing that structure over
/// itself. `RegistryRecordsEntry` declares `recordIds` and `RegistryRecordFilter`
/// declares `values`; they are the only structures this model's inputs nest
/// (`smithy_query(query="closure")`).
const COLLECTION_MEMBER_SHAPES: &[&::aws_smithy_schema::Schema] = &[
    crate::ops::types::BATCH_GET_DISCOVERABLE_REGISTRY_RECORD_INPUT_SCHEMA,
    crate::ops::types::LIST_DISCOVERABLE_REGISTRY_RECORDS_INPUT_SCHEMA,
    crate::ops::types::SEARCH_DISCOVERABLE_REGISTRY_RECORDS_INPUT_SCHEMA,
    crate::ops::types::REGISTRY_RECORDS_ENTRY_SCHEMA,
    crate::ops::types::REGISTRY_RECORD_FILTER_SCHEMA,
];

/// The requirement a clause states at the collection member `collection` for a
/// constraint one of its ELEMENTS broke — the element shape's whole declared set,
/// nested — or `None` when no shape of this model declares one for that member, which
/// leaves the framework's own per-element clause standing (a list of STRUCTURES, whose
/// elements declare no constraint of their own, is one such member).
///
/// The set is the element shape's WHOLE set and not just the constraint a value
/// happened to break, because it is a property of the model rather than of the request.
/// `collection` is the member path the clause is stated at, so a member declared inside
/// a nested structure is looked up under the last segment of that path.
fn element_set(collection: &str) -> Option<String> {
    use service_common::validation_message::{element_constraint_set, element_set_requirement};

    let member = collection
        .rsplit_once('.')
        .map_or(collection, |(_, member)| member);
    let declared = element_constraint_set(COLLECTION_MEMBER_SHAPES, member)?;
    let requirements: Vec<String> = declared.into_iter().map(stated_requirement).collect();
    Some(element_set_requirement(&requirements))
}

/// One requirement of an element shape as this service states it: the sentence is the
/// framework's own, with the one rendering this service chooses applied first, so a
/// requirement nested in a collection's clause reads exactly like the same requirement
/// stated at a member the walk reported.
fn stated_requirement(declared: ViolationKind) -> String {
    service_common::validation_message::requirement(match declared {
        // The CHECK always ran the model's pattern as written; only the rendering of
        // the expression drops the whole-value anchors.
        ViolationKind::Pattern(pattern) => {
            ViolationKind::Pattern(service_common::validation::unanchored(&pattern))
        }
        other => other,
    })
}
