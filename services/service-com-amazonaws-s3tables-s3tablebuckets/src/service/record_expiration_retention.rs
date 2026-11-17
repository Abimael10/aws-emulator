// SPDX-License-Identifier: CC0-1.0

//! The one modelled bound of this service that refuses no request: the retention
//! a record-expiration configuration names.
//!
//! `TableRecordExpirationSettings.days` targets `PositiveInteger`, bounded
//! `@range(1, 2147483647)` (`smithy_query(query="shape",
//! shape="TableRecordExpirationSettings,PositiveInteger")`). The two ends of that
//! range are not answered alike:
//!
//! * a retention BELOW the minimum is a refusal of the request's SHAPE, answered
//!   by the parameter validator in front of the service in its own envelope (see
//!   [`super::parameter_validation`]), because a table cannot retain its records
//!   for no days at all;
//! * a retention ABOVE the maximum is no refusal. The longest retention the
//!   member holds is the longest one there is, so a larger day count asks for
//!   the same thing — keep the records — and is read AS that maximum. The
//!   request then reaches the operation, and the operation's own gates decide it:
//!   a table that takes no record-expiration configuration is refused for that,
//!   with the `MethodNotAllowedException` its handler states, rather than for the
//!   number its configuration carried.
//!
//! Reading the member is what the second half needs, so it is done here, at the
//! codec: the wire carries a day count as a JSON number of any width, while the
//! member is an `integer`, and a number no `i32` holds would otherwise make the
//! whole body one this service cannot read at all
//! ([`super::request_layer`]) — a refusal about the body where the model
//! has a bound on one member.
//!
//! Nothing else about the codec changes. The cap is read off the MEMBER's own
//! `@range` trait, found by that member's shape id, so no other member of this
//! model — including every other one the model bounds at the same
//! `PositiveInteger` range, which this service refuses on its own terms — is
//! touched, and the number the cap uses is the model's rather than a constant
//! restated here.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::constraints::RangeTrait;
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};

use service_common::date_time_json::DateTimeRestJson;

/// The shape id of the retention member — `<namespace>#<Shape>$<member>`, the id
/// a member schema carries — so the cap below is keyed to the one member of this
/// model it belongs to and not to every member that happens to be spelled the
/// same. Compared as an id rather than as text, so the read path allocates
/// nothing.
const RETENTION_MEMBER: ShapeId = ShapeId::from_static(
    "com.amazonaws.s3tables#TableRecordExpirationSettings$days",
    "com.amazonaws.s3tables",
    "days",
);

/// The longest retention `schema` holds, when `schema` is the retention member
/// and the model bounds it above; `None` for every other member, which is read
/// exactly as the codec reads it.
fn retention_maximum(schema: &Schema) -> Option<i64> {
    if *schema.shape_id() != RETENTION_MEMBER {
        return None;
    }
    let range = schema
        .traits()?
        .get(&RangeTrait::TRAIT_ID)?
        .as_any()
        .downcast_ref::<RangeTrait>()?;
    let max = range.max?;
    // The trait carries every bound as `f64` because one type has to cover a
    // floating-point member too; this member is an `integer`, so its bound is one
    // an `i32` holds, which is what the narrowing below undoes.
    Some(max.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i64)
}

/// The inner deserializer one wrapper forwards to, however it is held.
///
/// The protocol seam hands back an owned `Box<dyn ShapeDeserializer>` while the
/// nested walks hand each level a `&mut dyn ShapeDeserializer`, and both wear the
/// same wrapper — so the wrapper is generic over this and the forwarding body is
/// written once (the seam
/// `service_common::date_time_json` wears for the same reason).
trait Held {
    fn get(&mut self) -> &mut dyn ShapeDeserializer;
    fn peek(&self) -> &dyn ShapeDeserializer;
}

impl Held for Box<dyn ShapeDeserializer + '_> {
    fn get(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn peek(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

impl Held for &mut (dyn ShapeDeserializer + '_) {
    fn get(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn peek(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

/// The deserializer an input is read with: every call is the codec's own, except
/// the retention member, which is read capped at the longest retention the model
/// declares for it.
///
/// A nested structure, list or map re-enters the wrapper, because the codec walks
/// each level with its own deserializer and the retention sits two structures
/// below the input.
struct CappedRetention<D>(D);

impl<D: Held> ShapeDeserializer for CappedRetention<D> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0.get().read_struct(schema, &mut |member, d| {
            consumer(member, &mut CappedRetention(d))
        })
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_list(schema, &mut |d| consumer(&mut CappedRetention(d)))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_map(schema, &mut |key, d| consumer(key, &mut CappedRetention(d)))
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

    /// The one read this wrapper replaces: the retention member, read at the
    /// width the wire carries a number in and then capped at the model's own
    /// maximum. A day count below the member's minimum stays below it — the cap
    /// is one-sided — so the validator still refuses that one; the only other
    /// narrowing is at the width an `i32` holds, which no bound of this model
    /// exceeds.
    fn read_integer(&mut self, schema: &Schema) -> Result<i32, SerdeError> {
        let Some(longest) = retention_maximum(schema) else {
            return self.0.get().read_integer(schema);
        };
        let sent = self.0.get().read_long(schema)?;
        // Inside the `i32` range by construction: the model's maximum for this
        // member is one an `i32` holds (see `retention_maximum`), and the floor
        // is that type's own minimum.
        let capped = sent.clamp(i64::from(i32::MIN), longest) as i32;
        Ok(capped)
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

/// This service's codec: the `restJson1` protocol with every timestamp in the
/// `date-time` form the model declares
/// (`service_common::date_time_json::DateTimeRestJson`), reading the retention
/// member capped at the model's own maximum.
///
/// Only the REQUEST side differs: what this service reports is serialized by the
/// inner codec unchanged, so a retention a read reports is the one the store
/// holds.
#[derive(Debug)]
pub struct CappedRetentionRestJson {
    inner: DateTimeRestJson,
}

impl CappedRetentionRestJson {
    /// Build the codec once, at service construction:
    /// `Service::override_protocol` hands back a borrow of it and must never
    /// build one per request.
    pub fn new() -> Self {
        Self {
            inner: DateTimeRestJson::new(),
        }
    }
}

impl Default for CappedRetentionRestJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for CappedRetentionRestJson {
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
        Ok(Box::new(CappedRetention(inner)))
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
    //! The one thing about the cap a wire test cannot see: WHICH member it is
    //! read off, and that the number it caps at is the model's own.

    use super::{retention_maximum, RETENTION_MEMBER};
    use crate::ops::types::{
        IcebergCompactionSettings, IcebergSnapshotManagementSettings,
        IcebergUnreferencedFileRemovalSettings, TableRecordExpirationConfigurationValue,
        TableRecordExpirationSettings, PUT_TABLE_RECORD_EXPIRATION_CONFIGURATION_INPUT_SCHEMA,
    };

    /// RULE: the cap belongs to the retention member alone, and the number it
    /// caps at is the maximum the MODEL declares for that member.
    #[test]
    fn the_cap_is_the_retention_members_own_modelled_maximum() {
        let retention = TableRecordExpirationSettings::SCHEMA
            .members()
            .iter()
            .copied()
            .find(|member| member.member_name() == Some("days"))
            .expect("the record expiration settings declare their retention member");
        assert_eq!(
            retention.shape_id(),
            &RETENTION_MEMBER,
            "case \"the retention member\": expected {RETENTION_MEMBER}, got {actual}",
            actual = retention.shape_id()
        );
        assert_eq!(
            retention_maximum(retention),
            Some(2_147_483_647),
            "case \"the retention member\": expected the modelled maximum, got {actual:?}",
            actual = retention_maximum(retention)
        );
    }

    /// RULE, the other way round: no other member is capped — not the input's
    /// own members, not the configuration's, and not the members of the other
    /// structures the model bounds at the SAME positive-integer range, which this
    /// service refuses on its own terms.
    #[test]
    fn no_other_member_of_this_model_is_capped() {
        let mut checked = 0usize;
        for schema in [
            PUT_TABLE_RECORD_EXPIRATION_CONFIGURATION_INPUT_SCHEMA,
            &TableRecordExpirationConfigurationValue::SCHEMA,
            &IcebergSnapshotManagementSettings::SCHEMA,
            &IcebergUnreferencedFileRemovalSettings::SCHEMA,
            &IcebergCompactionSettings::SCHEMA,
        ] {
            for member in schema.members().iter().copied() {
                checked += 1;
                assert_eq!(
                    retention_maximum(member),
                    None,
                    "case {id}: no member but the retention is capped, got {actual:?}",
                    id = member.shape_id(),
                    actual = retention_maximum(member)
                );
            }
        }
        assert!(
            checked >= 8,
            "case \"the members walked\": expected the input's and the configuration's members \
             beside the counted members of the maintenance settings, got {checked}"
        );
    }
}
