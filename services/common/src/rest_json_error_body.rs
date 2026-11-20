// SPDX-License-Identifier: CC0-1.0

//! The ONE `restJson1` codec that answers a failure with the error shape's own modelled
//! members and nothing else, for every service of that class — so two of them cannot answer
//! one refusal with two different envelopes.
//!
//! # The rule
//!
//! `restJson1` conveys a failure's error SHAPE in the `x-amzn-errortype` response header, so
//! the BODY of a failure carries the error shape's own modelled members and nothing else: no
//! shape DISCRIMINATOR member. The `__type` member belongs to the AWS JSON protocol family,
//! which conveys the shape in the body because it has no such header; a client generated from
//! a `restJson1` model resolves the shape from the header and cannot read an unmodelled member
//! out of the body at all. A shape that declares no member therefore serializes as the empty
//! JSON document.
//!
//! # Why a service needs this
//!
//! The framework's shared `restJson1` renderer writes the discriminator into every JSON error
//! body it renders (`vendor/smithy-schema-server/src/rest_json.rs`'s `serialize_error`), and
//! `vendor/` is read-only, so the correction lives at the seams a service owns over the
//! answers it writes. The rewriting itself is [`crate::error_type_locator`]'s — the one place
//! in the tree that owns where a rendered JSON failure names its shape; this module is the
//! protocol wrapper around it, so a service takes the rule by holding one protocol rather than
//! by copying a codec.
//!
//! # Where a service takes it
//!
//! At `Service::override_protocol`, which hands back a borrow, so the service holds one
//! [`ServedErrorBodyRestJson`] in a field and builds it in its constructor:
//!
//! ```ignore
//! fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
//!     Some(&self.protocol)
//! }
//! ```
//!
//! A service that also answers requests naming no modelled operation in this envelope passes
//! its routing failure through [`crate::error_type_locator::without_error_shape_in_body`] at
//! `Service::override_routing_error`, which is the other seam a service owns.
//!
//! Nothing else about an answer changes: the status, every header including
//! `x-amzn-errortype`, request deserialization and the whole SUCCESS path stay with the
//! framework renderer that owns them. One class of failure cannot be reached from any service
//! seam and is left as the framework writes it: a refusal rendered BEFORE a handler runs (an
//! absent required member, a value outside a modelled bound, an unauthenticated caller), which
//! the dispatch walk renders from the protocol's pre-dispatch ENVELOPE rather than from a
//! response a crate is handed.
//!
//! # Services still carrying their own copy
//!
//! This wrapper predates the module in the service crates that needed it before there was
//! anywhere shared to put it — `grep -rln 'rest_json_error_body' services/*/src` lists them
//! (today the Amazon API Gateway Management API crate, spelling the protocol
//! `ConnectionsRestJson`, and the Amazon DataZone crate, whose `DataZoneRestJson` also rewrites
//! a SUCCESS body and so needs a wrapper of its own around this one). Those copies are to be
//! MIGRATED onto this module and are left in place only because this change does not own those
//! services' issues. Nothing new should inline the wrapper again — hold this one.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::RestJsonServerProtocol;

/// The `restJson1` codec whose failures carry the error shape's own modelled members and
/// nothing else: the model's own protocol in every other respect.
#[derive(Debug)]
pub struct ServedErrorBodyRestJson {
    inner: RestJsonServerProtocol,
}

impl ServedErrorBodyRestJson {
    /// Build the protocol once, at service construction: `Service::override_protocol` hands
    /// back a borrow of it and must never build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// [`ServerProtocol::routing_key`] alone, and the router resolves a request's operation
    /// through the generated service definition's own protocol (whose table is the model's);
    /// an author-overridden protocol drives only the modelled sequence —
    /// `deserialize_input`, `serialize_output`, `serialize_error`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for ServedErrorBodyRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for ServedErrorBodyRestJson {
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
        let resp = self
            .inner
            .serialize_error(error, error_schema, error_code, http_status)?;
        Ok(crate::error_type_locator::without_error_shape_in_body(resp))
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

#[cfg(test)]
mod tests {
    use super::ServedErrorBodyRestJson;
    use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
    use aws_smithy_schema::{Schema, ShapeId, ShapeType};
    use aws_smithy_schema_server::protocol::ServerProtocol;
    use serde_json::Value;

    /// The `Message` member every error shape of a service of this class declares.
    static MESSAGE_MEMBER: Schema = Schema::new_member(
        ShapeId::from_static("example#Refused$Message", "example", "Message"),
        ShapeType::String,
        "Message",
        0,
    );

    /// An error shape carrying the one member.
    static REFUSED_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("example#Refused", "example", "Refused"),
        ShapeType::Structure,
        &[&MESSAGE_MEMBER],
    );

    /// An error shape declaring no member at all.
    static MEMBERLESS_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("example#Memberless", "example", "Memberless"),
        ShapeType::Structure,
        &[],
    );

    /// The payload of a modelled failure: the text it carries, or none.
    struct Refused(Option<&'static str>);

    impl SerializableStruct for Refused {
        fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
            match self.0 {
                Some(message) => s.write_string(&MESSAGE_MEMBER, message),
                None => Ok(()),
            }
        }
    }

    /// RULE: the body of a failure this codec renders carries the error shape's own modelled
    /// members and nothing else — never the AWS JSON families' `__type` discriminator — while
    /// the shape travels in `x-amzn-errortype`. The rows differ along what the rule
    /// generalises over: whether the shape declares a member at all, whether a declared
    /// member carries text, and the status the failure is rendered under.
    #[test]
    fn a_failures_body_carries_the_shapes_modelled_members_and_nothing_else() {
        let protocol = ServedErrorBodyRestJson::new();
        // (case, the shape, the payload, the code, the status, the body on the wire)
        let rows: Vec<(&str, &Schema, Refused, &str, u16, Value)> = vec![
            (
                "a refusal carrying its text",
                &REFUSED_SCHEMA,
                Refused(Some("Invalid path for user, arn: a")),
                "InvalidInputException",
                400,
                serde_json::json!({ "Message": "Invalid path for user, arn: a" }),
            ),
            (
                "a refusal carrying no text",
                &REFUSED_SCHEMA,
                Refused(None),
                "InvalidInputException",
                400,
                serde_json::json!({}),
            ),
            (
                "a shape that declares no member",
                &MEMBERLESS_SCHEMA,
                Refused(None),
                "GoneException",
                410,
                serde_json::json!({}),
            ),
            (
                "a server fault",
                &REFUSED_SCHEMA,
                Refused(Some("no state")),
                "InternalServiceException",
                500,
                serde_json::json!({ "Message": "no state" }),
            ),
        ];

        for (case, schema, payload, code, status, expected) in rows {
            let answered = protocol
                .serialize_error(&payload, schema, code, status)
                .unwrap_or_else(|e| panic!("case {case:?}: the failure must render ({e})"));
            assert_eq!(
                answered.status().as_u16(),
                status,
                "case {case:?}: the status is the renderer's own"
            );
            assert_eq!(
                answered.headers().get("x-amzn-errortype"),
                Some(code),
                "case {case:?}: the shape travels in the header"
            );
            let body = answered.body().bytes().expect("an in-memory body");
            let document: Value = serde_json::from_slice(body)
                .unwrap_or_else(|e| panic!("case {case:?}: the body must be JSON ({e})"));
            assert_eq!(
                document, expected,
                "case {case:?}: expected the body {expected}, got {document}"
            );
        }
    }
}
