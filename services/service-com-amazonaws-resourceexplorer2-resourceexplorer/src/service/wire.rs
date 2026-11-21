// SPDX-License-Identifier: CC0-1.0

//! The wire form this service answers a modelled request in: the model's own `restJson1`,
//! with the two corrections this service's recorded answers show.
//!
//! * A TIMESTAMP goes onto the wire in the form the model declares for it. Every timestamp
//!   member of this model carries `@timestampFormat("date-time")` — on `Resource` and
//!   `ResourceProperty` (`$LastReportedAt`), on `View` and `ManagedView`
//!   (`$LastUpdatedAt`), on `GetIndexOutput`, `CreateIndexOutput`, `DeleteIndexOutput` and
//!   `UpdateIndexTypeOutput` (`$CreatedAt`/`$LastUpdatedAt`) and on `StreamingAccessDetails`
//!   (`$CreatedAt`), and the model declares no other timestamp form anywhere
//!   (`smithy_query(query="shape",shape="Resource,ResourceProperty,View")`). So each one is
//!   an RFC 3339 / ISO-8601 date-time STRING and not the epoch-seconds number `restJson1`
//!   defaults to. [`service_common::date_time_json`] holds that rule for every service of
//!   this class, and records why a generated member schema cannot carry the format itself.
//! * A FAILURE reports its detail text in the body member `message`. This service's
//!   framework-rendered refusals (a body that breaks a modelled constraint, an
//!   unauthenticated caller) already carry it under that name, while a modelled error a
//!   handler returns is serialized from the model's own shape and so would report it under
//!   the Smithy member name `Message` — leaving one service answering its two failure paths
//!   with two different member names, and a client that reads one of them finding nothing in
//!   the other's answers. [`service_common::error_detail_member`] owns that renaming.
//!
//! ROUTING stays the generated entry's: `protocol_id` and `routing_key` are read off the
//! model's own protocol, whose table is the model's, so no second routing table can drift
//! from the model.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use service_common::date_time_json::DateTimeRestJson;
use service_common::error_detail_member::with_renamed_detail_member;

/// Every error shape this model declares, by the code it renders under
/// (`smithy_query(query="digest")`, whose `common_errors` are the two every operation
/// carries). Each one declares the detail member the rename below moves, so no error body
/// gains or loses a member its shape does not have.
const MODELLED_ERROR_SHAPES: &[&str] = &[
    "AccessDeniedException",
    "ConflictException",
    "InternalServerException",
    "ResourceNotFoundException",
    "ServiceQuotaExceededException",
    "ThrottlingException",
    "UnauthorizedException",
    "ValidationException",
];

/// The member the model names an error's detail text (`Message` on every shape above).
const MODELLED_DETAIL_MEMBER: &str = "Message";

/// The member this service's failures report that text in.
const SERVED_DETAIL_MEMBER: &str = "message";

/// The `restJson1` codec this service serves: the model's own, with the timestamp form the
/// model declares and the detail member this service's failures carry.
#[derive(Debug)]
pub struct ResourceExplorerRestJson {
    /// The model's own `restJson1` with every declared `date-time` timestamp in that form.
    inner: DateTimeRestJson,
}

impl ResourceExplorerRestJson {
    /// Build the codec once, at service construction: `Service::override_protocol` hands
    /// back a borrow of it and must never build one per request.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: DateTimeRestJson::new(),
        }
    }
}

impl Default for ResourceExplorerRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for ResourceExplorerRestJson {
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
        // Read through the same codec the answer is written with, so a timestamp a request
        // carries is parsed in the form this model declares for it.
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
        let answered = self
            .inner
            .serialize_error(error, error_schema, error_code, http_status)?;
        Ok(with_renamed_detail_member(
            answered,
            MODELLED_ERROR_SHAPES,
            MODELLED_DETAIL_MEMBER,
            SERVED_DETAIL_MEMBER,
        ))
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
