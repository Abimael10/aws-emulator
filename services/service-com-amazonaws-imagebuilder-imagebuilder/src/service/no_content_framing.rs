// SPDX-License-Identifier: CC0-1.0

//! The wire protocol this service serves: its model's own `restJson1`, with the framing an
//! answer that carries NO CONTENT needs.
//!
//! An operation whose modelled output structure declares no member leaves its caller nothing
//! to parse, and such a success answers `204 No Content`. The STATUS is stated on the
//! operation (`Operation::SUCCESS_STATUS`); the BODY cannot be, because the protocol's
//! serializer renders a members-less output structure as the empty JSON object — it is a
//! `structure`, not `smithy.api#Unit`, which is the one shape the renderers treat as
//! bodyless. `service_common::no_content_responses` is this tree's one reading of that
//! framing, and [`NoContentRestJson`] is where this service takes it: the one seam every
//! response a modelled operation produces passes through.
//!
//! Everything else is the protocol's own. An answer that DOES carry content — every reader's,
//! every create's and every modelled refusal's — is returned exactly as the renderer wrote
//! it, status, headers and every body byte.
//!
//! The length such an answer states is the last word only up to the transport: the HTTP/1.1
//! writer the vendored listener is built on drops `content-length` from a status that cannot
//! carry a body, which is below anything a service crate decides. What this file owns is the
//! response the emulator hands out — what the in-process front door, the inter-service
//! callback path and the SDK connector all serve unchanged.

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::{ProtocolError, RestJsonServerProtocol, ServerProtocol};
use service_common::no_content_responses::without_content;

/// This service's `restJson1`, with a response at the no-content status framed as one that
/// carries no content.
#[derive(Debug)]
pub struct NoContentRestJson {
    inner: RestJsonServerProtocol,
}

impl NoContentRestJson {
    /// Build the protocol once, at service construction: [`crate::service::EmulatedService`]
    /// holds it because `Service::override_protocol` hands back a borrow of it and must never
    /// build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// [`ServerProtocol::routing_key`] alone, and the router resolves a request's operation
    /// through the generated service definition's own protocol (whose table is the model's);
    /// an author-overridden protocol drives only the modelled sequence —
    /// `deserialize_input`, `serialize_output`, `serialize_error` — and the URI template each
    /// of those reads comes from the operation's own input schema.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for NoContentRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for NoContentRestJson {
    type Request = WireRequest;
    type Response = WireResponse;

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
        // The request side is the protocol's own.
        self.inner.deserialize_input(request, input_schema)
    }

    /// The protocol's own rendering of the modelled success, framed as an answer that carries
    /// no content when that is what its status says it is. A success whose output carries
    /// members is returned exactly as rendered.
    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        let rendered =
            self.inner
                .serialize_output(output, output_schema, operation_name, success_status)?;
        Ok(without_content(rendered))
    }

    /// A modelled failure is returned exactly as the protocol rendered it: the framing above is
    /// the SUCCESS rule — an output structure with no member — and an error body is content, at
    /// whatever status its shape declares.
    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_error(error, error_schema, error_code, http_status)
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
