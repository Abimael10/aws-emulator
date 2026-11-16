// SPDX-License-Identifier: CC0-1.0

//! How this service READS one item of a batch: a member the request leaves out is UNSET,
//! not a reason to refuse the call.
//!
//! # The rule
//!
//! Each item of a batch this service takes — a `MetricQuery`, a `RawMetricData` — is
//! answered for ITSELF inside a 200 response, so an item that says less than the model
//! requires is an item with nothing in those members rather than a refusal of the whole
//! call: a query that names no series selects nothing, and a data point reads as whatever
//! it did say. The request's own ENVELOPE is the only thing this service refuses, which
//! [`super::envelope`] states.
//!
//! JSON `null` is the wire's way of saying the same thing, so a member sent as `null` is
//! read as unset too, at every depth — which is also what the codec does for a `null` list
//! or structure.
//!
//! # Why it has to be applied here
//!
//! A `@required` member absent from the request is caught while READING the request: the
//! generated input struct holds such a member as a non-`Option` field and reports
//! `MissingMember` when the body carried nothing for it
//! (`vendor/smithy-macros`), which the framework renders as its own HTTP 400
//! `ValidationException` before any handler runs. `skip_validation` cannot reach it — that
//! keeps CONSTRAINT traits off a member schema, while absence is not a constraint — and the
//! codec is in the read-only vendored tree.
//!
//! So the reading is corrected once, here, at the protocol seam `service.rs` owns:
//! [`ItemsReadLeniently`] is the model's own `restJson1` with the input deserializer
//! wrapped, the same way
//! `services/service-com-amazonaws-eks-awswesleyfrontend/src/service/lenient_json.rs`
//! wraps it for the scalar forms that service reads.
//!
//! Nothing else about either walk changes: a member's wire name, its order, its schema and
//! every constraint the model puts on it stay as the model gave them, and a response is
//! written exactly as before. The wrapper adds one stack frame per nesting level and
//! nothing else — no buffer, no copy of the document, no caller-sized allocation and no
//! second pass; nesting depth stays bounded by the codec itself.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::RestJsonServerProtocol;
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};

/// The members of each ITEM shape of this service's inputs that the model requires, as
/// `smithy_query(query="shape",shape="MetricQuery,RawMetricData")` reports them — the
/// members read as unset when an item carries nothing for them.
///
/// The two REQUEST shapes are deliberately absent: a request that carries no value for one
/// of THEIR members is an envelope this service refuses, with its own status and sentence
/// per member ([`super::envelope`]), so nothing is filled in for them.
const UNSET_ITEM_MEMBERS: &[(&str, &[&str])] = &[
    (
        "MetricQuery",
        &[
            "MetricName",
            "ResourceArn",
            "MetricStat",
            "Period",
            "XAxisType",
        ],
    ),
    ("RawMetricData", &["MetricName", "Timestamp", "Value"]),
];

/// The members of `schema` this service reads as unset when the request carries nothing for
/// them, or an empty list for a shape that is not one of its batch items.
fn unset_members_of(schema: &Schema) -> &'static [&'static str] {
    let name = schema.shape_id().shape_name();
    UNSET_ITEM_MEMBERS
        .iter()
        .find(|(shape_name, _)| *shape_name == name)
        .map_or(&[], |(_, members)| *members)
}

/// The inner deserializer one [`ItemReader`] forwards to, however it is held.
///
/// The protocol seam hands back an owned `Box<dyn ShapeDeserializer>` while the nested
/// walks hand each level a `&mut dyn ShapeDeserializer`, and both have to wear the same
/// wrapper — so the wrapper is generic over this and the forwarding body is written once.
trait InnerDeserializer {
    fn get(&mut self) -> &mut dyn ShapeDeserializer;
    fn peek(&self) -> &dyn ShapeDeserializer;
}

impl InnerDeserializer for Box<dyn ShapeDeserializer + '_> {
    fn get(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn peek(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

impl InnerDeserializer for &mut (dyn ShapeDeserializer + '_) {
    fn get(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn peek(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

/// The deserializer an input is read with: every call is the codec's own, except that a
/// member sent as `null` is read as unset and a member of a batch item the request left out
/// is read as unset too.
struct ItemReader<D>(D);

impl<D: InnerDeserializer> ShapeDeserializer for ItemReader<D> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        // Which members the body carried a value for, by member index — a bitmask rather
        // than a collection, because a shape's members are few and this is per request.
        let mut carried: u64 = 0;
        self.0.get().read_struct(schema, &mut |member, d| {
            if d.is_null() {
                // A member sent as `null` carries no value: the token is read so the codec
                // stays in step with the document, and the member is left unset.
                return d.read_null();
            }
            if let Some(index) = member.member_index() {
                carried |= 1u64 << (index as u64 & 63);
            }
            consumer(member, &mut ItemReader(d))
        })?;
        for member in unset_members_of(schema) {
            let Some(declared) = schema
                .members()
                .iter()
                .copied()
                .find(|candidate| candidate.member_name() == Some(*member))
            else {
                continue;
            };
            let was_carried = declared
                .member_index()
                .is_some_and(|index| carried & (1u64 << (index as u64 & 63)) != 0);
            if was_carried {
                continue;
            }
            consumer(declared, &mut Unset)?;
        }
        Ok(())
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_list(schema, &mut |d| consumer(&mut ItemReader(d)))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_map(schema, &mut |key, d| consumer(key, &mut ItemReader(d)))
    }

    fn read_boolean(&mut self, schema: &Schema) -> Result<bool, SerdeError> {
        self.0.get().read_boolean(schema)
    }

    fn read_byte(&mut self, schema: &Schema) -> Result<i8, SerdeError> {
        self.0.get().read_byte(schema)
    }

    fn read_short(&mut self, schema: &Schema) -> Result<i16, SerdeError> {
        self.0.get().read_short(schema)
    }

    fn read_integer(&mut self, schema: &Schema) -> Result<i32, SerdeError> {
        self.0.get().read_integer(schema)
    }

    fn read_long(&mut self, schema: &Schema) -> Result<i64, SerdeError> {
        self.0.get().read_long(schema)
    }

    fn read_float(&mut self, schema: &Schema) -> Result<f32, SerdeError> {
        self.0.get().read_float(schema)
    }

    fn read_double(&mut self, schema: &Schema) -> Result<f64, SerdeError> {
        self.0.get().read_double(schema)
    }

    fn read_big_integer(&mut self, schema: &Schema) -> Result<BigInteger, SerdeError> {
        self.0.get().read_big_integer(schema)
    }

    fn read_big_decimal(&mut self, schema: &Schema) -> Result<BigDecimal, SerdeError> {
        self.0.get().read_big_decimal(schema)
    }

    fn read_string(&mut self, schema: &Schema) -> Result<String, SerdeError> {
        self.0.get().read_string(schema)
    }

    fn read_blob(&mut self, schema: &Schema) -> Result<Blob, SerdeError> {
        self.0.get().read_blob(schema)
    }

    fn read_timestamp(&mut self, schema: &Schema) -> Result<DateTime, SerdeError> {
        self.0.get().read_timestamp(schema)
    }

    fn read_document(&mut self, schema: &Schema) -> Result<Document, SerdeError> {
        self.0.get().read_document(schema)
    }

    fn is_null(&self) -> bool {
        self.0.peek().is_null()
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.0.get().read_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.0.peek().container_size()
    }
}

/// The value a member the request carried nothing for is read as: nothing at all, in
/// whatever type the member holds — no text, the epoch, nothing in a collection, and NOT A
/// NUMBER where a number was owed, so a point that states no measurement is reported at its
/// own index rather than stored as a number the caller never sent.
///
/// It reads from no document, so it consumes nothing and can be handed to any member: the
/// handler that reads the item then sees a member with nothing in it and reports what it
/// could do with the item.
struct Unset;

impl ShapeDeserializer for Unset {
    fn read_struct(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Ok(())
    }

    fn read_list(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Ok(())
    }

    fn read_map(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Ok(())
    }

    fn read_boolean(&mut self, _schema: &Schema) -> Result<bool, SerdeError> {
        Ok(false)
    }

    fn read_byte(&mut self, _schema: &Schema) -> Result<i8, SerdeError> {
        Ok(0)
    }

    fn read_short(&mut self, _schema: &Schema) -> Result<i16, SerdeError> {
        Ok(0)
    }

    fn read_integer(&mut self, _schema: &Schema) -> Result<i32, SerdeError> {
        Ok(0)
    }

    fn read_long(&mut self, _schema: &Schema) -> Result<i64, SerdeError> {
        Ok(0)
    }

    fn read_float(&mut self, _schema: &Schema) -> Result<f32, SerdeError> {
        Ok(f32::NAN)
    }

    /// NOT A NUMBER: a measurement the request did not state is not a number, and this
    /// service reports a point whose `Value` is not a number at its own index
    /// ([`crate::ops::batch_put_metrics`]) rather than storing a number the caller never
    /// sent.
    fn read_double(&mut self, _schema: &Schema) -> Result<f64, SerdeError> {
        Ok(f64::NAN)
    }

    /// Unreachable: no shape of this service declares an arbitrary-precision number, so no
    /// member can be read as an unset one. Answered as a read that found nothing rather
    /// than invented.
    fn read_big_integer(&mut self, _schema: &Schema) -> Result<BigInteger, SerdeError> {
        Err(SerdeError::custom(
            "this service declares no arbitrary-precision integer member".to_string(),
        ))
    }

    /// Unreachable, for the reason given on [`Unset::read_big_integer`].
    fn read_big_decimal(&mut self, _schema: &Schema) -> Result<BigDecimal, SerdeError> {
        Err(SerdeError::custom(
            "this service declares no arbitrary-precision decimal member".to_string(),
        ))
    }

    fn read_string(&mut self, _schema: &Schema) -> Result<String, SerdeError> {
        Ok(String::new())
    }

    fn read_blob(&mut self, _schema: &Schema) -> Result<Blob, SerdeError> {
        Ok(Blob::new(Vec::new()))
    }

    /// The epoch: a point whose instant the request did not state is the one instant that
    /// states nothing of its own.
    fn read_timestamp(&mut self, _schema: &Schema) -> Result<DateTime, SerdeError> {
        Ok(DateTime::from_secs(0))
    }

    fn read_document(&mut self, _schema: &Schema) -> Result<Document, SerdeError> {
        Ok(Document::Null)
    }

    fn is_null(&self) -> bool {
        true
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        Ok(())
    }

    fn container_size(&self) -> Option<usize> {
        Some(0)
    }
}

/// This service's wire protocol: `restJson1` exactly as the model declares it, reading a
/// request body the way this service reads one (see the module header).
#[derive(Debug)]
pub struct ItemsReadLeniently {
    inner: RestJsonServerProtocol,
}

impl ItemsReadLeniently {
    /// Build the protocol once, at service construction:
    /// [`smithy_server_runtime::Service::override_protocol`] hands back a borrow of it and
    /// must never build one per request.
    ///
    /// The route table is EMPTY by design. A protocol's routes are read by
    /// [`ServerProtocol::routing_key`] alone, and the router resolves a request's operation
    /// through the generated `ServiceDefinition`'s own protocol (whose table is the
    /// model's); an author-overridden protocol drives only the modelled sequence —
    /// `deserialize_input`, `serialize_output`, `serialize_error`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RestJsonServerProtocol::new(&[]),
        }
    }
}

impl Default for ItemsReadLeniently {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for ItemsReadLeniently {
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
        let inner = self.inner.deserialize_input(request, input_schema)?;
        Ok(Box::new(ItemReader(inner)))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the members read as unset are the ones the model requires of a batch ITEM, and
    /// no member of a REQUEST shape is filled in — those are the envelope's own refusals.
    #[test]
    fn only_a_batch_items_required_members_are_read_as_unset() {
        // (case, the shape, how many of its members are read as unset)
        let rows: [(&str, &Schema, usize); 4] = [
            ("a query", &crate::ops::types::MetricQuery::SCHEMA, 5),
            ("a data point", &crate::ops::types::RawMetricData::SCHEMA, 3),
            (
                "the get request",
                crate::ops::types::BATCH_GET_METRICS_INPUT_SCHEMA,
                0,
            ),
            (
                "the put request",
                crate::ops::types::BATCH_PUT_METRICS_INPUT_SCHEMA,
                0,
            ),
        ];
        for (case, schema, expected) in rows {
            let actual = unset_members_of(schema).len();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected} member(s) read as unset, got {actual}"
            );
        }
        for member in unset_members_of(&crate::ops::types::MetricQuery::SCHEMA) {
            assert!(
                crate::ops::types::MetricQuery::SCHEMA
                    .members()
                    .iter()
                    .any(|candidate| candidate.member_name() == Some(*member)),
                "member {member:?} is not declared by the shape it is named for"
            );
        }
    }
}
