// SPDX-License-Identifier: CC0-1.0

//! How this service's `restJson1` answers report the request id of a FAILED call.
//!
//! RULE: the request id of a failed call travels in the `x-amzn-RequestId` RESPONSE
//! HEADER, and the error document carries the error shape's other modelled members only —
//! a rendered error body of this service never carries a `RequestId` member, whatever the
//! handler that refused the call put in it. Every exception shape of this model declares
//! `RequestId` as an OPTIONAL member (`smithy_query(query="shape",
//! shape="ResourceNotFoundException,ResourceExistsException,LimitExceededException",
//! model_path="services/service-com-amazonaws-quicksight-quicksight-20180401/smithy-model/com-amazonaws-quicksight-quicksight-20180401.json")`),
//! so writing it is the renderer's choice rather than the model's requirement, and the
//! wire this service emulates leaves it out of the document: a client correlating a
//! failure with a service-side request id reads the header, which is the one place the id
//! is reported. A SUCCESS is a different contract — the output shapes declare `RequestId`
//! as a body member and the answer carries it there — so nothing here touches one.
//!
//! The framework's generic serializer writes every member a handler set, and `vendor/` is
//! read-only, so the correction lives at the one seam a service owns over the answers it
//! writes: [`RequestIdInHeaderRestJson`] is the protocol `service.rs` hands the dispatch
//! sequence through [`smithy_server_runtime::Service::override_protocol`], so EVERY
//! modelled failure of EVERY operation goes out the same way and no handler, shape
//! declaration or error constructor mentions the rendering at all. The id itself is the
//! one the refusing handler allocated — it is moved from the document to the header, never
//! re-minted here — and an error shape that declares no such member is rendered exactly as
//! the framework writes it.
//!
//! Everything else is the framework's own `restJson1` protocol, delegated to unchanged:
//! routing, request deserialization, success bodies, the status, `x-amzn-errortype` and
//! every other member of the error document. One class of failure is not reachable from
//! this seam and is left as the framework writes it: a refusal rendered BEFORE a handler
//! runs (an absent required member, a value outside a modelled bound, an unauthenticated
//! caller), whose synthetic shape has no request-id member to withhold in the first place.

use std::cell::RefCell;

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::RestJsonServerProtocol;
use aws_smithy_types::{BigDecimal, BigInteger, DateTime, Document};

/// The response header AWS reports the request id of a call in, and the only place a
/// failure of this service reports it.
const REQUEST_ID_HEADER: &str = "x-amzn-RequestId";

/// The member this model's exception shapes declare the request id under.
const REQUEST_ID_MEMBER: &str = "RequestId";

/// The error shape's request-id member, or `None` for a shape that declares none.
///
/// Only a plain string member of the BODY qualifies: a member the model binds to a header,
/// to the query string or to the payload is not part of the JSON object this rule is about,
/// and one already bound to a header is already reported where the rule wants it.
fn request_id_member(error_schema: &Schema) -> Option<&Schema> {
    error_schema.members().iter().copied().find(|member| {
        member
            .member_name()
            .is_some_and(|name| name == REQUEST_ID_MEMBER)
            && matches!(member.shape_type(), ShapeType::String)
            && member.http_header().is_none()
            && member.http_payload().is_none()
            && member.http_query().is_none()
    })
}

/// An error structure serialized with its own members except the request id, which is
/// captured in `withheld` for the header to carry instead.
struct RequestIdWithheld<'a> {
    inner: &'a dyn SerializableStruct,
    request_id: &'a Schema,
    withheld: &'a RefCell<Option<String>>,
}

impl SerializableStruct for RequestIdWithheld<'_> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        let mut withholding = Withholding {
            inner: &mut *serializer,
            member_name: self.request_id.member_name(),
            withheld: self.withheld,
        };
        self.inner.serialize_members(&mut withholding)
    }
}

/// The serializer an error's members are written through: every write is the wrapped
/// serializer's, except a write of the request-id member, which is kept out of the
/// document and noted so the response can report it in its header.
struct Withholding<'a> {
    inner: &'a mut dyn ShapeSerializer,
    member_name: Option<&'a str>,
    withheld: &'a RefCell<Option<String>>,
}

impl Withholding<'_> {
    /// Whether `schema` is the member this rule keeps out of the document.
    fn withholds(&self, schema: &Schema) -> bool {
        self.member_name.is_some() && schema.member_name() == self.member_name
    }
}

impl ShapeSerializer for Withholding<'_> {
    fn write_struct(
        &mut self,
        schema: &Schema,
        value: &dyn SerializableStruct,
    ) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_struct(schema, value)
    }

    fn write_list(
        &mut self,
        schema: &Schema,
        write_elements: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_list(schema, write_elements)
    }

    fn write_map(
        &mut self,
        schema: &Schema,
        write_entries: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_map(schema, write_entries)
    }

    fn write_boolean(&mut self, schema: &Schema, value: bool) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_boolean(schema, value)
    }

    fn write_byte(&mut self, schema: &Schema, value: i8) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_byte(schema, value)
    }

    fn write_short(&mut self, schema: &Schema, value: i16) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_short(schema, value)
    }

    fn write_integer(&mut self, schema: &Schema, value: i32) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_integer(schema, value)
    }

    fn write_long(&mut self, schema: &Schema, value: i64) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_long(schema, value)
    }

    fn write_float(&mut self, schema: &Schema, value: f32) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_float(schema, value)
    }

    fn write_double(&mut self, schema: &Schema, value: f64) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_double(schema, value)
    }

    fn write_big_integer(&mut self, schema: &Schema, value: &BigInteger) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_big_integer(schema, value)
    }

    fn write_big_decimal(&mut self, schema: &Schema, value: &BigDecimal) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_big_decimal(schema, value)
    }

    fn write_string(&mut self, schema: &Schema, value: &str) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            // The id the handler allocated for this call: kept out of the document and
            // reported in the response header instead.
            *self.withheld.borrow_mut() = Some(value.to_string());
            return Ok(());
        }
        self.inner.write_string(schema, value)
    }

    fn write_blob(&mut self, schema: &Schema, value: &[u8]) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_blob(schema, value)
    }

    fn write_timestamp(&mut self, schema: &Schema, value: &DateTime) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_timestamp(schema, value)
    }

    fn write_document(&mut self, schema: &Schema, value: &Document) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_document(schema, value)
    }

    fn write_null(&mut self, schema: &Schema) -> Result<(), SerdeError> {
        if self.withholds(schema) {
            return Ok(());
        }
        self.inner.write_null(schema)
    }
}

/// The `restJson1` codec that reports a failure's request id in the `x-amzn-RequestId`
/// response header rather than in the error document: the model's own protocol in every
/// other respect.
#[derive(Debug)]
pub struct RequestIdInHeaderRestJson {
    inner: RestJsonServerProtocol,
}

impl RequestIdInHeaderRestJson {
    /// Build the protocol once, at service construction:
    /// [`smithy_server_runtime::Service::override_protocol`] hands back a borrow of it and
    /// must never build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// [`ServerProtocol::routing_key`] alone, and the router resolves a request's operation
    /// through the generated service definition's own protocol (whose table is the
    /// model's); an author-overridden protocol drives only the modelled sequence —
    /// `deserialize_input`, `serialize_output`, `serialize_error`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for RequestIdInHeaderRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for RequestIdInHeaderRestJson {
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
        // Nothing about a REQUEST changes: it is read by the model's own codec.
        self.inner.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        // A SUCCESS is the framework's own, byte for byte: an output shape declares its
        // `RequestId` as a body member and the answer carries it there.
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
        let Some(request_id) = request_id_member(error_schema) else {
            // A shape with no request-id member has nothing to move.
            return self
                .inner
                .serialize_error(error, error_schema, error_code, http_status);
        };
        let withheld: RefCell<Option<String>> = RefCell::new(None);
        let mut response = self.inner.serialize_error(
            &RequestIdWithheld {
                inner: error,
                request_id,
                withheld: &withheld,
            },
            error_schema,
            error_code,
            http_status,
        )?;
        // Dropped rather than fatal if the HTTP layer cannot carry the value: `try_insert`
        // is the fallible half of the insert the framework itself uses on the error path,
        // where a panic would become a 500 an SDK retries.
        if let Some(id) = withheld.into_inner() {
            let _ = response.headers_mut().try_insert(REQUEST_ID_HEADER, id);
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
