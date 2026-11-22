// SPDX-License-Identifier: CC0-1.0

//! The ONE `restJson1` codec whose ERROR document CARRIES the error shape's MESSAGE member
//! even where the service supplies no message, for every service of that class — so two of
//! them cannot answer one message-less refusal with two different documents.
//!
//! # The rule
//!
//! An error document carries the error shape's message member whatever the service put in it:
//! a supplied message travels as that string, and a message the service supplied none of
//! travels as JSON `null` rather than being left out — so a client can tell "no message was
//! supplied" from "an empty message was supplied". An SDK reading a JSON error document
//! substitutes the EMPTY STRING for a message key the document does not carry and reports
//! "none" only for a key whose value is `null`, so dropping the key reports the opposite of
//! what such an error class states.
//!
//! # Why a service needs this
//!
//! The framework's generic serializer writes an absent optional member by writing NOTHING,
//! which is right for a success body — where an absent optional member stays absent, as in
//! every AWS JSON response — but drops the key out of the error document. `vendor/` is
//! read-only, so the correction lives at the one seam a service owns over the answers it
//! writes.
//!
//! # Where a service takes it
//!
//! At `Service::override_protocol`, which hands back a borrow, so the service holds one
//! [`MessagePresentRestJson`] in a field and builds it in its constructor:
//!
//! ```ignore
//! fn override_protocol(&self) -> Option<&dyn smithy_server_runtime::ServerHttpProtocol> {
//!     Some(&self.protocol)
//! }
//! ```
//!
//! Every modelled failure of every operation then goes out with the member present, and no
//! handler, shape declaration or error constructor mentions the rendering at all. Everything
//! else — routing, request deserialization, success bodies, the status, `x-amzn-errortype` and
//! an error whose message IS supplied — is the framework's own `restJson1` protocol, delegated
//! to unchanged.
//!
//! A service that applies a SECOND rule to its error answers — one that renames where the
//! rendered answer names its error shape, say — holds this codec as the INNER protocol of its
//! own wrapper and delegates to it, so the member rule is still stated once.
//!
//! A refusal rendered BEFORE a handler runs (an absent required member, a value outside a
//! modelled bound, an unauthenticated caller) is not reached from this seam: the dispatch walk
//! renders it from the protocol's pre-dispatch envelope rather than from a response a crate is
//! handed.
//!
//! [`message_member`] is the member lookup on its own, for a service that renders a body from
//! an error shape's schema itself and has to write the model's own member key.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::RestJsonServerProtocol;
use aws_smithy_types::{BigDecimal, BigInteger, DateTime, Document};

/// The member names an error document's message can occupy: the two spellings an AWS JSON
/// error document uses for that one member — the Smithy default for the detail member, which
/// the framework's own synthetic shapes spell lowercase, and the capitalised one many models
/// declare. An SDK reads whichever of the two is present, and no shape declares both, so
/// which name is looked for first does not decide anything.
const MESSAGE_MEMBER_NAMES: &[&str] = &["message", "Message"];

/// The error shape's message member, or `None` for a shape that declares none.
///
/// Only a plain string member in the BODY qualifies: a member bound to a header, to the query
/// string or to the payload is not part of the JSON object this rule is about, and writing
/// `null` for it would mean something else entirely.
#[must_use]
pub fn message_member(error_schema: &Schema) -> Option<&Schema> {
    error_schema.members().iter().copied().find(|member| {
        member
            .member_name()
            .is_some_and(|name| MESSAGE_MEMBER_NAMES.contains(&name))
            && matches!(member.shape_type(), ShapeType::String)
            && member.http_header().is_none()
            && member.http_payload().is_none()
            && member.http_query().is_none()
    })
}

/// An error structure serialized with its own members, plus its message member as JSON `null`
/// when it supplied none.
struct MessageAlwaysPresent<'a> {
    inner: &'a dyn SerializableStruct,
    message: &'a Schema,
}

impl SerializableStruct for MessageAlwaysPresent<'_> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        // The borrow of `serializer` ends with the tracker, so the null below is written
        // through the same serializer the members went to — after them, which is where the
        // absent key belongs in the object.
        let wrote_message = {
            let mut tracker = TrackMessage {
                inner: &mut *serializer,
                message_name: self.message.member_name(),
                wrote_message: false,
            };
            self.inner.serialize_members(&mut tracker)?;
            tracker.wrote_message
        };
        if !wrote_message {
            serializer.write_null(self.message)?;
        }
        Ok(())
    }
}

/// The serializer an error's members are written through: every write is the wrapped
/// serializer's, and a write of the message member is noted so the wrapper above knows whether
/// the key still has to be written as `null`.
struct TrackMessage<'a> {
    inner: &'a mut dyn ShapeSerializer,
    message_name: Option<&'a str>,
    wrote_message: bool,
}

impl TrackMessage<'_> {
    /// Note a write of the message member, whatever shape it was written as.
    fn note(&mut self, schema: &Schema) {
        if self.message_name.is_some() && schema.member_name() == self.message_name {
            self.wrote_message = true;
        }
    }
}

impl ShapeSerializer for TrackMessage<'_> {
    fn write_struct(
        &mut self,
        schema: &Schema,
        value: &dyn SerializableStruct,
    ) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_struct(schema, value)
    }

    fn write_list(
        &mut self,
        schema: &Schema,
        write_elements: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_list(schema, write_elements)
    }

    fn write_map(
        &mut self,
        schema: &Schema,
        write_entries: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_map(schema, write_entries)
    }

    fn write_boolean(&mut self, schema: &Schema, value: bool) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_boolean(schema, value)
    }

    fn write_byte(&mut self, schema: &Schema, value: i8) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_byte(schema, value)
    }

    fn write_short(&mut self, schema: &Schema, value: i16) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_short(schema, value)
    }

    fn write_integer(&mut self, schema: &Schema, value: i32) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_integer(schema, value)
    }

    fn write_long(&mut self, schema: &Schema, value: i64) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_long(schema, value)
    }

    fn write_float(&mut self, schema: &Schema, value: f32) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_float(schema, value)
    }

    fn write_double(&mut self, schema: &Schema, value: f64) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_double(schema, value)
    }

    fn write_big_integer(&mut self, schema: &Schema, value: &BigInteger) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_big_integer(schema, value)
    }

    fn write_big_decimal(&mut self, schema: &Schema, value: &BigDecimal) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_big_decimal(schema, value)
    }

    fn write_string(&mut self, schema: &Schema, value: &str) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_string(schema, value)
    }

    fn write_blob(&mut self, schema: &Schema, value: &[u8]) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_blob(schema, value)
    }

    fn write_timestamp(&mut self, schema: &Schema, value: &DateTime) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_timestamp(schema, value)
    }

    fn write_document(&mut self, schema: &Schema, value: &Document) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_document(schema, value)
    }

    fn write_null(&mut self, schema: &Schema) -> Result<(), SerdeError> {
        self.note(schema);
        self.inner.write_null(schema)
    }
}

/// The `restJson1` codec whose error document always carries the error shape's message member:
/// the model's own protocol in every other respect.
#[derive(Debug)]
pub struct MessagePresentRestJson {
    inner: RestJsonServerProtocol,
}

impl MessagePresentRestJson {
    /// Build the protocol once, at service construction: `Service::override_protocol` hands
    /// back a borrow of it and must never build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// [`ServerProtocol::routing_key`] alone, and the router resolves a request's operation
    /// through the generated service definition's own protocol (whose table is the model's);
    /// an author-overridden protocol drives only the modelled sequence — `deserialize_input`,
    /// `serialize_output`, `serialize_error` — none of which consults routes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for MessagePresentRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for MessagePresentRestJson {
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
        match message_member(error_schema) {
            Some(message) => self.inner.serialize_error(
                &MessageAlwaysPresent {
                    inner: error,
                    message,
                },
                error_schema,
                error_code,
                http_status,
            ),
            // A shape with no message member has nothing to keep present.
            None => self
                .inner
                .serialize_error(error, error_schema, error_code, http_status),
        }
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
    use super::{message_member, MessagePresentRestJson};
    use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
    use aws_smithy_schema::{Schema, ShapeId, ShapeType};
    use aws_smithy_schema_server::protocol::ServerProtocol;
    use serde_json::Value;

    /// The lowercase spelling of the detail member, as the Smithy default declares it.
    static LOWERCASE_MESSAGE: Schema = Schema::new_member(
        ShapeId::from_static("example#Refused$message", "example", "message"),
        ShapeType::String,
        "message",
        0,
    );

    /// The capitalised spelling of the same member, as many models declare it.
    static CAPITALISED_MESSAGE: Schema = Schema::new_member(
        ShapeId::from_static("example#Named$Message", "example", "Message"),
        ShapeType::String,
        "Message",
        0,
    );

    /// A member that is not the message: the resource an error class names beside it.
    static RESOURCE_ID: Schema = Schema::new_member(
        ShapeId::from_static("example#Named$ResourceId", "example", "ResourceId"),
        ShapeType::String,
        "ResourceId",
        1,
    );

    /// An error shape declaring the lowercase member alone.
    static REFUSED_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("example#Refused", "example", "Refused"),
        ShapeType::Structure,
        &[&LOWERCASE_MESSAGE],
    );

    /// An error shape declaring the capitalised member beside another one.
    static NAMED_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("example#Named", "example", "Named"),
        ShapeType::Structure,
        &[&CAPITALISED_MESSAGE, &RESOURCE_ID],
    );

    /// An error shape declaring no member at all.
    static MEMBERLESS_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("example#Memberless", "example", "Memberless"),
        ShapeType::Structure,
        &[],
    );

    /// The payload of a modelled failure: the message it supplied, under the member its own
    /// shape declares, and whatever else that shape carries.
    struct Failure {
        message: Option<&'static str>,
        message_schema: &'static Schema,
        resource: Option<&'static str>,
    }

    impl SerializableStruct for Failure {
        fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
            if let Some(message) = self.message {
                s.write_string(self.message_schema, message)?;
            }
            if let Some(resource) = self.resource {
                s.write_string(&RESOURCE_ID, resource)?;
            }
            Ok(())
        }
    }

    /// RULE: the error document carries the shape's message member whatever the service put in
    /// it — the string it supplied, or JSON `null` where it supplied none — while every other
    /// modelled member stays as the renderer wrote it. The rows differ along what the rule
    /// generalises over: which spelling the shape declares the member under, whether a message
    /// was supplied, whether the supplied message is empty, whether the shape carries other
    /// members, and the status the failure is rendered under.
    #[test]
    fn an_error_document_carries_its_message_member_whether_or_not_one_was_supplied() {
        let protocol = MessagePresentRestJson::new();
        // (case, the shape, the payload, the member it is read back under, the value)
        let rows: Vec<(&str, &Schema, Failure, &str, u16, Value)> = vec![
            (
                "a supplied message under the lowercase member",
                &REFUSED_SCHEMA,
                Failure {
                    message: Some("Tenant 'marketing' does not exist"),
                    message_schema: &LOWERCASE_MESSAGE,
                    resource: None,
                },
                "message",
                404,
                Value::String("Tenant 'marketing' does not exist".to_string()),
            ),
            (
                "no message supplied under the lowercase member",
                &REFUSED_SCHEMA,
                Failure {
                    message: None,
                    message_schema: &LOWERCASE_MESSAGE,
                    resource: None,
                },
                "message",
                404,
                Value::Null,
            ),
            (
                "a supplied EMPTY message, which is not the same as none",
                &REFUSED_SCHEMA,
                Failure {
                    message: Some(""),
                    message_schema: &LOWERCASE_MESSAGE,
                    resource: None,
                },
                "message",
                400,
                Value::String(String::new()),
            ),
            (
                "no message supplied under the capitalised member",
                &NAMED_SCHEMA,
                Failure {
                    message: None,
                    message_schema: &CAPITALISED_MESSAGE,
                    resource: None,
                },
                "Message",
                409,
                Value::Null,
            ),
            (
                "no message supplied beside another member that was",
                &NAMED_SCHEMA,
                Failure {
                    message: None,
                    message_schema: &CAPITALISED_MESSAGE,
                    resource: Some("fsap-0123456789abcdef0"),
                },
                "Message",
                409,
                Value::Null,
            ),
            (
                "a supplied message beside another member",
                &NAMED_SCHEMA,
                Failure {
                    message: Some("Access point already exists"),
                    message_schema: &CAPITALISED_MESSAGE,
                    resource: Some("fsap-0123456789abcdef0"),
                },
                "Message",
                409,
                Value::String("Access point already exists".to_string()),
            ),
        ];

        for (case, schema, payload, member, status, expected) in rows {
            let answered = protocol
                .serialize_error(&payload, schema, "Refused", status)
                .unwrap_or_else(|e| panic!("case {case:?}: the failure must render ({e})"));
            assert_eq!(
                answered.status().as_u16(),
                status,
                "case {case:?}: the status is the renderer's own"
            );
            let body = answered.body().bytes().expect("an in-memory body");
            let document: Value = serde_json::from_slice(body)
                .unwrap_or_else(|e| panic!("case {case:?}: the body must be JSON ({e})"));
            assert_eq!(
                document.get(member),
                Some(&expected),
                "case {case:?}: expected the member {member:?} to be {expected:?}, got {document}"
            );
        }
    }

    /// RULE: only a plain string member of the BODY is the message member, so a shape that
    /// declares none has nothing to keep present and its document is the renderer's own. The
    /// rows differ along what the rule generalises over: the spelling, the presence of other
    /// members, and a shape with no member at all.
    #[test]
    fn the_message_member_is_the_shapes_own_plain_string_body_member() {
        // (case, the shape, the member name the lookup must find)
        let rows: Vec<(&str, &Schema, Option<&str>)> = vec![
            ("the lowercase spelling", &REFUSED_SCHEMA, Some("message")),
            (
                "the capitalised spelling beside another member",
                &NAMED_SCHEMA,
                Some("Message"),
            ),
            ("a shape declaring no member", &MEMBERLESS_SCHEMA, None),
        ];

        for (case, schema, expected) in rows {
            let found = message_member(schema).and_then(Schema::member_name);
            assert_eq!(
                found, expected,
                "case {case:?}: expected the message member {expected:?}, got {found:?}"
            );
        }

        // A shape with no message member is rendered exactly as the framework writes it: no
        // key is invented for it.
        let protocol = MessagePresentRestJson::new();
        let answered = protocol
            .serialize_error(
                &Failure {
                    message: None,
                    message_schema: &LOWERCASE_MESSAGE,
                    resource: None,
                },
                &MEMBERLESS_SCHEMA,
                "Memberless",
                410,
            )
            .expect("the failure must render");
        let body = answered.body().bytes().expect("an in-memory body");
        let document: Value = serde_json::from_slice(body).expect("the body must be JSON");
        assert!(
            document.get("message").is_none() && document.get("Message").is_none(),
            "a shape declaring no message member gains none, got {document}"
        );
    }
}
