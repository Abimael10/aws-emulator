// SPDX-License-Identifier: CC0-1.0

//! How this service's `restJson1` ERROR response NAMES the error shape and carries a
//! modelled exception's `message` member.
//!
//! RULE: every error document Amazon Polly answers with CARRIES the error shape's
//! `message` member — when the service supplies no message the member is written with
//! JSON `null` rather than left out — so a client can tell "no message was supplied"
//! from "an empty message was supplied".
//!
//! RULE: the `x-amzn-errortype` header of an error answer names the error shape
//! QUALIFIED by the service's own error namespace — the shape name, a colon, and the
//! namespace URI the service's model declares — because the shape name is the only
//! variable part of that header value and the namespace is a property of the service
//! that answered. An SDK or a proxy that matches the whole header value reads the
//! qualified form AWS sends, and matching the shape name alone still works because the
//! qualified value begins with it. It is the namespace of THIS SERVICE'S MODEL, so only
//! a shape that model declares carries it: the framework's synthetic shapes render as
//! the request boundary renders them (see [`declared_by_this_service`]).
//!
//! Every error shape in this service's model carries exactly one optional member,
//! `message` (target `ErrorMessage`, a string with no HTTP binding:
//! `smithy_query(query="shape",shape="SynthesisTaskNotFoundException",
//! model_path="services/service-com-amazonaws-polly-parrot-v1/smithy-model/com-amazonaws-polly-parrot-v1.json")`),
//! and one code — `SynthesisTaskNotFoundException` — is answered with no message at
//! all (see [`crate::service::errors`]). The framework's generic serializer writes an
//! absent optional member by writing NOTHING, which is right for a success body but
//! drops the key out of the error document: every SDK then reports the error's message
//! as the empty string it substitutes for an absent key, where Amazon Polly's own
//! answer reads as "none".
//!
//! The FIRST rule is not this service's alone, so the codec implementing it is the
//! shared [`service_common::error_message_member`]'s, held here as the inner protocol;
//! the SECOND is this service's own and is all this module adds to it.
//!
//! [`NullMessageRestJson`] is the protocol `service.rs` hands the dispatch sequence
//! through [`smithy_server_runtime::Service::override_protocol`], so EVERY modelled
//! failure of EVERY operation goes out with the member present and with the qualified
//! errortype header, and no handler, shape declaration or error constructor mentions
//! either rendering at all. Everything else —
//! routing, request deserialization, success bodies, and an error whose message IS
//! supplied — is the framework's own restJson1 protocol, delegated to unchanged.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use service_common::error_message_member::MessagePresentRestJson;

/// The header an error answer names its shape in.
const ERRORTYPE_HEADER: &str = "x-amzn-errortype";

/// The namespace every shape of this service's own model lives in, as a shape id
/// spells it (`com.amazonaws.polly#SynthesisTaskNotFoundException`).
const MODEL_NAMESPACE: &str = "com.amazonaws.polly";

/// The error namespace this service qualifies that header's shape name with.
///
/// Source: the service's own `@xmlNamespace` URI — `smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-polly-parrot-v1/smithy-model/com-amazonaws-polly-parrot-v1.json")`
/// reports `xmlNamespace: {uri: "http://polly.amazonaws.com/doc/v1"}`. It is a property
/// of the SERVICE, so it is the same text behind every error code this service answers
/// and only the shape name before it varies. The generated service identity does not
/// carry it for a JSON protocol, which is why it is named here.
const ERROR_NAMESPACE: &str = "http://polly.amazonaws.com/doc/v1";

/// What separates the shape name from the namespace in that header value.
const ERROR_NAMESPACE_SEPARATOR: char = ':';

/// Whether `error_schema` is an error THIS SERVICE'S MODEL declares, as opposed to one
/// of the framework's synthetic shapes (the scaffold channel's 501, the request-shape
/// refusal), which live in `smithy.framework`.
///
/// Only the former is named in the service's error namespace. A synthetic shape is not
/// an error of this service's model and is rendered on the request-boundary path too,
/// where this protocol never runs, so qualifying it here would give one logical failure
/// two different header values depending on which layer refused the request.
fn declared_by_this_service(error_schema: &Schema) -> bool {
    error_schema
        .shape_id()
        .to_string()
        .split_once('#')
        .is_some_and(|(namespace, _)| namespace == MODEL_NAMESPACE)
}

/// Qualify the errortype header of an error response the framework has already
/// rendered: the shape name it wrote, this service's namespace behind it.
///
/// The header is REWRITTEN rather than appended to, so passing one response through
/// here twice cannot qualify it twice. Only a response that ALREADY names its shape
/// there is touched, so a protocol that sends no errortype header is not given one.
///
/// Set FALLIBLY, for the reason the framework's own insert is: a header value rejects
/// bytes a shape name could carry, and a panic on the ERROR path would become a 500 an
/// SDK retries. On a rejected value the header keeps the name the framework wrote.
fn qualify_errortype_header(response: &mut Response) {
    let Some(shape) = response
        .headers()
        .get(ERRORTYPE_HEADER)
        .map(str::to_string)
    else {
        return;
    };
    if shape.contains(ERROR_NAMESPACE_SEPARATOR) {
        return;
    }
    let qualified = format!("{shape}{ERROR_NAMESPACE_SEPARATOR}{ERROR_NAMESPACE}");
    if ::http::HeaderValue::from_str(&qualified).is_ok() {
        response.headers_mut().insert(ERRORTYPE_HEADER, qualified);
    }
}

/// This service's wire protocol: the shared codec whose error document always carries
/// the error shape's `message` member, with the errortype header of a modelled failure
/// qualified by this service's error namespace.
#[derive(Debug)]
pub struct NullMessageRestJson {
    inner: MessagePresentRestJson,
}

impl NullMessageRestJson {
    /// Build the protocol once, at service construction: `override_protocol` hands back
    /// a borrow of it and must never build one per request.
    pub fn new() -> Self {
        Self {
            inner: MessagePresentRestJson::new(),
        }
    }
}

impl Default for NullMessageRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for NullMessageRestJson {
    type Request = Request;
    type Response = Response;

    fn protocol_id(&self) -> &ShapeId {
        self.inner.protocol_id()
    }

    fn routing_key<'a>(&self, request: &'a Self::Request) -> Result<&'a str, ProtocolError> {
        self.inner.routing_key(request)
    }

    fn deserialize_input<'a>(
        &self,
        request: &'a Self::Request,
        input_schema: &Schema,
    ) -> Result<Box<dyn ShapeDeserializer + 'a>, ProtocolError> {
        self.inner.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_output(output, output_schema, operation_name, success_status)
    }

    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        // The member rule is the shared codec's, whether or not the shape had a message
        // to render.
        let mut response = self
            .inner
            .serialize_error(error, error_schema, error_code, http_status)?;
        // Every error this service's MODEL declares names its shape qualified.
        if declared_by_this_service(error_schema) {
            qualify_errortype_header(&mut response);
        }
        Ok(response)
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn SerializableStruct,
        event_schema: &Schema,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_event_stream_message(event_type, is_exception, event, event_schema)
    }
}
