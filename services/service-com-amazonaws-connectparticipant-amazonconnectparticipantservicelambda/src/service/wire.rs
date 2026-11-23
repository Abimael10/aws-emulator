// SPDX-License-Identifier: CC0-1.0

//! The wire form of this service's FAILURES: the body member a modelled exception reports
//! its explanatory text in.
//!
//! RULE: every error document this service answers with reports the text of the failure
//! under the lowercase wire key `message`, whatever member name the model declares that
//! text under. Each error shape of this model declares exactly one text member and spells
//! it `Message`
//! (`smithy_query(query="shape",shape="AccessDeniedException",model_path="services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json")`
//! reports `{"Message": {"target": "Message", "required": true}}`), and the JSON codecs key
//! a member by its `@jsonName` or else by its member name — neither of which this model
//! makes lowercase — so a model-derived error shape would put `Message` on the wire. Every
//! recorded refusal of this service keys the same text `message` instead: an Amazon Connect
//! participant endpoint is served through Amazon API Gateway, whose error documents key
//! their text that way, and it is the key an SDK surfaces as the exception's user-facing
//! text. Keyed otherwise, a client raises the modelled exception with an EMPTY message —
//! an SDK substitutes the empty string for a key the document does not carry — so nothing
//! can be told from it, and the requiredness the model states for that member is not met
//! on the wire.
//!
//! The key is therefore a property of THIS SERVICE's wire rather than of the model's
//! spelling, and it is stated exactly once, here, so no error shape and no operation of
//! this service can drift from the others. The framework's own pre-handler refusals —
//! a constraint violation, the scaffold channel's 501 — are rendered from synthetic shapes
//! that already spell the member lowercase, so after this both layers of this service
//! answer one logical failure in one member.
//!
//! Neither the renaming nor the keeping of that member is this service's alone, so neither
//! is written here: [`service_common::error_message_member::MessagePresentRestJson`] is the
//! inner protocol (it carries the member even for a class that supplies no text, as JSON
//! `null`) and [`service_common::error_detail_member::with_renamed_detail_member`] is the
//! renaming this module composes onto it.
//!
//! [`ParticipantRestJson`] is the protocol `service.rs` hands the dispatch sequence through
//! [`smithy_server_runtime::Service::override_protocol`], so EVERY modelled failure of
//! EVERY operation goes out keyed that way and no handler, shape declaration or error
//! constructor mentions the rendering at all. Everything else — routing, request
//! deserialization, success bodies, the status, `x-amzn-errortype` and every other member
//! of an error shape — is the framework's own restJson1 protocol, delegated to unchanged.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use service_common::error_detail_member::with_renamed_detail_member;
use service_common::error_message_member::MessagePresentRestJson;

/// The member name this model declares the text of a failure under, in every one of its
/// error shapes.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The wire key this service reports that text under.
const WIRE_MESSAGE_KEY: &str = "message";

/// The error shapes this model declares, by the wire code they render under — the bodies
/// the rule above owns.
///
/// Source: this service's own model. Each of the seven declares exactly one text member,
/// spelled `Message`
/// (`smithy_query(query="shape",shape="AccessDeniedException,ConflictException,InternalServerException,ResourceNotFoundException,ServiceQuotaExceededException,ThrottlingException,ValidationException")`),
/// which is the precondition for reporting that member on every one of them. A failure
/// whose class this model does NOT declare keeps the member the layer that rendered it
/// wrote its text in.
pub const MODELLED_ERROR_SHAPES: &[&str] = &[
    "AccessDeniedException",
    "ConflictException",
    "InternalServerException",
    "ResourceNotFoundException",
    "ServiceQuotaExceededException",
    "ThrottlingException",
    "ValidationException",
];

/// This service's wire protocol: the model's own `restJson1`, with the one difference the
/// generic renderer cannot express — the wire key a modelled failure's text travels under.
#[derive(Debug)]
pub struct ParticipantRestJson {
    inner: MessagePresentRestJson,
}

impl ParticipantRestJson {
    /// Build the protocol once, at service construction: `override_protocol` hands back a
    /// borrow of it and must never build one per request.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: MessagePresentRestJson::new(),
        }
    }
}

impl Default for ParticipantRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for ParticipantRestJson {
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
        // A SUCCESS is the framework's own, byte for byte.
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
        // The member is carried by the inner codec whether or not the class supplied a
        // text; which key it is carried under is this service's.
        let rendered = self
            .inner
            .serialize_error(error, error_schema, error_code, http_status)?;
        Ok(with_renamed_detail_member(
            rendered,
            MODELLED_ERROR_SHAPES,
            MODELLED_MESSAGE_MEMBER,
            WIRE_MESSAGE_KEY,
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
