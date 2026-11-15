// SPDX-License-Identifier: CC0-1.0

//! What this service reports for a string member whose stored value is UNSET: the
//! member is left out of the response, never filled with the empty string.
//!
//! # The rule
//!
//! A response never carries a value a member's own constraints forbid. A string
//! member whose `@length` minimum is 1 or more cannot legally hold the empty
//! string, so "no value" for such a member is expressed by omitting it: a client
//! can then tell "unset" from "set to something", and what it reads back is a
//! value it can send straight into the write that sets the member
//! (`smithy_query(query="shape", shape="Description")` — `@length` min 1, max
//! 2048, pattern `^[^\u0000-\u001F\u007F]+$`).
//!
//! Every description this model declares targets that one `Description` shape and
//! is modelled `@required` on the read side even though the member that writes it
//! is optional on every create and update — an asset model's, an asset's, a
//! dataset's, a composite model's, and the same member on each summary. So a
//! resource created without one has no description to report.
//!
//! # Why the correction lives at the protocol seam
//!
//! The generated shapes cannot express it. `#[smithy_macros::smithy(shape = ..)]`
//! requires a `@required` member to be a NON-`Option` field
//! (`vendor/smithy-macros`: "is @required or @default in Smithy, so it must not be
//! Option<T>"), and the generated serializer writes every non-`Option` field
//! unconditionally — so a handler can only hand the struct a string, and the empty
//! string is the one that stands for "unset". Dropping it on the way out is
//! therefore done once, here, for every response of this service, rather than by
//! hand-writing the codec of each shape that holds a description.
//!
//! Only a STRUCTURE MEMBER is subject to the rule. A list element and a map value
//! are written against the COLLECTION member's schema, whose `@length` bounds the
//! element count rather than the element
//! (`vendor/smithy-schema-server/src/constraints.rs`), so an empty string inside a
//! collection is written exactly as the handler produced it and no element is ever
//! dropped. Nothing else about the walk changes: a member's wire name, its order
//! and its schema stay the ones the model gave it, and the request side is
//! untouched.
//!
//! The wrapper costs one stack frame per nesting level and nothing else — no
//! buffer, no copy of the body and no caller-sized allocation. The nesting it
//! descends is the OUTPUT SHAPE's, which the model fixes, so no request can choose
//! how deep it goes.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::{LengthTrait, RestJsonServerProtocol};
use aws_smithy_types::{BigDecimal, BigInteger, DateTime, Document};

/// Whether `schema`'s own `@length` forbids the empty string: a minimum of 1 or
/// more. A member carrying no `@length` at all, or one whose minimum is 0, admits
/// the empty string and keeps it.
fn forbids_the_empty_string(schema: &Schema) -> bool {
    schema
        .traits()
        .and_then(|traits| traits.get(&LengthTrait::TRAIT_ID))
        .and_then(|length| length.as_any().downcast_ref::<LengthTrait>())
        .and_then(|length| length.min)
        .is_some_and(|min| min >= 1)
}

/// One shape, serialized with every unset string member left out.
struct Projected<'a>(&'a dyn SerializableStruct);

impl SerializableStruct for Projected<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.0.serialize_members(&mut MemberWriter {
            inner: s,
            of_a_structure: true,
        })
    }
}

/// The serializer a shape's members are written through: every call is the codec's
/// own, except a structure member holding a string its `@length` forbids, which is
/// not written at all.
///
/// A nested structure re-enters the wrapper, so the rule reaches a description at
/// any depth. A list or a map re-enters it with `of_a_structure` cleared, because
/// the values written inside one are elements, not members.
struct MemberWriter<'a> {
    inner: &'a mut dyn ShapeSerializer,
    /// Whether what is being written is a MEMBER of a structure — and so whether
    /// the schema each call carries is that member's own, constraints and all.
    of_a_structure: bool,
}

impl ShapeSerializer for MemberWriter<'_> {
    fn write_struct(
        &mut self,
        schema: &Schema,
        value: &dyn SerializableStruct,
    ) -> Result<(), SerdeError> {
        self.inner.write_struct(schema, &Projected(value))
    }

    fn write_list(
        &mut self,
        schema: &Schema,
        write_elements: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.inner.write_list(schema, &|inner| {
            write_elements(&mut MemberWriter {
                inner,
                of_a_structure: false,
            })
        })
    }

    fn write_map(
        &mut self,
        schema: &Schema,
        write_entries: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.inner.write_map(schema, &|inner| {
            write_entries(&mut MemberWriter {
                inner,
                of_a_structure: false,
            })
        })
    }

    fn write_boolean(&mut self, schema: &Schema, value: bool) -> Result<(), SerdeError> {
        self.inner.write_boolean(schema, value)
    }

    fn write_byte(&mut self, schema: &Schema, value: i8) -> Result<(), SerdeError> {
        self.inner.write_byte(schema, value)
    }

    fn write_short(&mut self, schema: &Schema, value: i16) -> Result<(), SerdeError> {
        self.inner.write_short(schema, value)
    }

    fn write_integer(&mut self, schema: &Schema, value: i32) -> Result<(), SerdeError> {
        self.inner.write_integer(schema, value)
    }

    fn write_long(&mut self, schema: &Schema, value: i64) -> Result<(), SerdeError> {
        self.inner.write_long(schema, value)
    }

    fn write_float(&mut self, schema: &Schema, value: f32) -> Result<(), SerdeError> {
        self.inner.write_float(schema, value)
    }

    fn write_double(&mut self, schema: &Schema, value: f64) -> Result<(), SerdeError> {
        self.inner.write_double(schema, value)
    }

    fn write_big_integer(&mut self, schema: &Schema, value: &BigInteger) -> Result<(), SerdeError> {
        self.inner.write_big_integer(schema, value)
    }

    fn write_big_decimal(&mut self, schema: &Schema, value: &BigDecimal) -> Result<(), SerdeError> {
        self.inner.write_big_decimal(schema, value)
    }

    /// The one rewrite on the way out: a structure member whose value is the empty
    /// string its own `@length` forbids is skipped, so the response carries no
    /// member rather than a value the shape rejects. Every other string — a value
    /// the member admits, or one written as a collection element — is the codec's
    /// own.
    fn write_string(&mut self, schema: &Schema, value: &str) -> Result<(), SerdeError> {
        if self.of_a_structure && value.is_empty() && forbids_the_empty_string(schema) {
            return Ok(());
        }
        self.inner.write_string(schema, value)
    }

    fn write_blob(&mut self, schema: &Schema, value: &[u8]) -> Result<(), SerdeError> {
        self.inner.write_blob(schema, value)
    }

    fn write_timestamp(&mut self, schema: &Schema, value: &DateTime) -> Result<(), SerdeError> {
        self.inner.write_timestamp(schema, value)
    }

    fn write_document(&mut self, schema: &Schema, value: &Document) -> Result<(), SerdeError> {
        self.inner.write_document(schema, value)
    }

    fn write_null(&mut self, schema: &Schema) -> Result<(), SerdeError> {
        self.inner.write_null(schema)
    }
}

/// This service's `restJson1` codec, with an unset string member omitted from
/// every response rather than reported as the empty string.
#[derive(Debug)]
pub struct UnsetStringMembersRestJson {
    inner: RestJsonServerProtocol,
}

impl UnsetStringMembersRestJson {
    /// Build the protocol once, at service construction:
    /// [`smithy_server_runtime::Service::override_protocol`] hands back a borrow of
    /// it and must never build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// `ServerProtocol::routing_key` alone, and the router resolves a request's
    /// operation through the generated service definition's own protocol (whose
    /// table is the model's); an author-overridden protocol drives only the
    /// modelled sequence — `deserialize_input`, `serialize_output`,
    /// `serialize_error`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for UnsetStringMembersRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for UnsetStringMembersRestJson {
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
        // The REQUEST side is the framework's own: the rule is about what a read
        // reports, and a request carrying a value outside a member's `@length` is
        // refused by the validating walk rather than quietly dropped here.
        //
        // This is also the one place a request's INPUT SHAPE is in hand, so what that
        // shape declares is recorded here for the refusal the walk may produce: the
        // walk cannot apply an expression written with look-around, and a clause
        // stated at a collection member lists the element shape's whole constraint
        // set, which no violation carries (`super::input_shape`).
        super::input_shape::reading_input(input_schema);
        self.inner.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner.serialize_output(
            &Projected(output),
            output_schema,
            operation_name,
            success_status,
        )
    }

    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        // Wrapped for the same reason the success body is: the seam is uniform, so
        // no shape of this model can report a string its own constraints forbid.
        // Every error shape of this service carries a message it always has, so the
        // wrapper is a no-op for them.
        self.inner
            .serialize_error(&Projected(error), error_schema, error_code, http_status)
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn SerializableStruct,
        event_schema: &Schema,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner.serialize_event_stream_message(
            event_type,
            is_exception,
            &Projected(event),
            event_schema,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::forbids_the_empty_string;

    /// RULE: a member forbids the empty string exactly when its own `@length`
    /// minimum is 1 or more. Asked of the REAL member schemas of one of this
    /// model's read responses, so the predicate is pinned against the model rather
    /// than against a schema written here: source
    /// `smithy_query(query="shape", shape="DescribeAssetModelResponse")` — the
    /// description targets `Description` (`@length` min 1), the external id
    /// `ExternalId` (min 2), the ARN `ARN` (min 1), while the type is an enum and
    /// the creation date a timestamp, neither carrying a `@length` at all.
    #[test]
    fn only_a_minimum_of_one_or_more_forbids_the_empty_string() {
        let rows: [(&str, bool); 5] = [
            ("assetModelDescription", true),
            ("assetModelExternalId", true),
            ("assetModelArn", true),
            ("assetModelType", false),
            ("assetModelCreationDate", false),
        ];
        let schema = crate::ops::types::DESCRIBE_ASSET_MODEL_OUTPUT_SCHEMA;
        for (case, expected) in rows {
            let member = schema
                .members()
                .iter()
                .find(|member| member.member_name() == Some(case))
                .unwrap_or_else(|| panic!("case {case:?}: the response declares this member"));
            let forbids = forbids_the_empty_string(member);
            assert_eq!(
                forbids, expected,
                "case {case:?}: expected forbids_the_empty_string {expected}, got {forbids}"
            );
        }
    }
}
