// SPDX-License-Identifier: CC0-1.0

//! How this control plane READS a `ContentHandlingStrategy` a request carries: a value the
//! enum does not declare names no payload conversion, so the member is read as carrying no
//! value at all.
//!
//! # The rule
//!
//! `ContentHandlingStrategy` names the conversion a WebSocket integration — or one of its
//! integration responses — performs on a payload, and the member it is carried in is
//! OPTIONAL with a documented meaning for carrying nothing: "If this property is not
//! defined, the response payload will be passed through from the integration response to
//! the route response or method response without modification"
//! (`smithy_query(query="doc", shape="UpdateIntegrationResponseRequest",
//! member="ContentHandlingStrategy")`). So a value that names none of the declared
//! strategies states something this API has a reading for — no conversion — rather than
//! something it cannot read: the recorded service SERVES such a write (HTTP 200, the record
//! answered with the strategy it already held) instead of refusing it, and a later read
//! reports the strategy the resource had before.
//!
//! Every member of this model that targets the enum is read this way, which is the four
//! writes that carry one: `CreateIntegration`, `UpdateIntegration`,
//! `CreateIntegrationResponse` and `UpdateIntegrationResponse`. A write is a PATCH or a
//! create like any other afterwards: carrying no strategy leaves a stored one alone and
//! gives a new resource none.
//!
//! # Why the reading lives at the protocol seam
//!
//! A member is judged against its enum's value set by the framework's validating walk
//! (`aws_smithy_schema_server::constraints`), which runs after the body is deserialized and
//! BEFORE any handler — so a handler cannot decide this: the request was already refused
//! `400 BadRequestException` with nothing stored. The walk judges the value the
//! DESERIALIZER produced, so the one place to state what such a value names is the protocol
//! seam [`crate::service`] already owns, the same seam
//! `services/service-com-amazonaws-eks-awswesleyfrontend/src/service/lenient_json.rs` reads
//! its own enum-backed members at, for the same reason.
//!
//! The reading is this ONE enum's, recognised by the value set the member's own schema
//! declares, so it cannot drift from the model. Nothing else about either walk changes:
//! every other constrained member of this model — `ProtocolType`, `IntegrationType`,
//! `ConnectionType`, `AuthorizationType`, `AuthorizerType`, `IpAddressType`, the numeric
//! windows — is judged by its own declared constraint exactly as before, and a request
//! outside one is refused as `tests/all/constraint_refusal.rs` pins. A value that is no
//! string at all is no strategy and no reading of one either: it is left to the codec, which
//! refuses it in the words it already uses.
//!
//! The wrapper costs one stack frame per nesting level and nothing else — no buffer, no copy
//! of the body and no caller-sized allocation. The one value it holds is the single string a
//! content-handling member states, read through the codec itself, which is also what refuses
//! a value of any other type there before this reading sees it.

use aws_smithy_schema::serde::{SerdeError, ShapeDeserializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::EnumTrait;
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};

use crate::ops::types::CONTENT_HANDLING_STRATEGY_VALUES;

/// The input deserializer `request` is read through, reading a content-handling member the
/// way this service reads one (see the module header). Called from the protocol seam, which
/// hands the framework's own deserializer in.
pub(super) fn reading<'a>(inner: Box<dyn ShapeDeserializer + 'a>) -> Box<dyn ShapeDeserializer + 'a> {
    Box::new(Reading(inner))
}

/// The strategies `member` declares, when it is a member that carries a content-handling
/// strategy — `None` for every other member.
///
/// A content-handling member is recognised by the value set its OWN schema declares, which
/// the generated schema carries for the enum shape it targets: a member schema holds the
/// member's own shape id rather than its target's (`vendor/smithy-macros/src/lib.rs` emits
/// `Schema::new_member(...)` with the member's own id), so the value set is what identifies
/// the enum from here. `ContentHandlingStrategy` is the only enum of this model declaring
/// these two values, and the set comes from the model-derived catalog
/// ([`CONTENT_HANDLING_STRATEGY_VALUES`]), so this reading and the framework's validating
/// walk cannot disagree about what the model declares. Were the model ever to gain a second
/// enum declaring exactly these two conversions, it would be read this way too; the
/// reconciliation is for a member schema to carry its target's shape id, which a service
/// crate cannot add.
///
/// A member whose value comes from the request itself — a URI label, the query string, a
/// header, the raw payload — is left out: such a value is TEXT read by the binding
/// deserializer, and no member of this model carries a strategy there.
fn declared_strategies(member: &Schema) -> Option<&'static [&'static str]> {
    if member.http_label().is_some()
        || member.http_query().is_some()
        || member.http_query_params().is_some()
        || member.http_header().is_some()
        || member.http_prefix_headers().is_some()
        || member.http_payload().is_some()
    {
        return None;
    }
    let values = member
        .traits()?
        .get(&EnumTrait::TRAIT_ID)?
        .as_any()
        .downcast_ref::<EnumTrait>()?
        .values;
    (values == CONTENT_HANDLING_STRATEGY_VALUES).then_some(values)
}

/// The strategy a content-handling member states, or `None` when it states none — a value
/// `declared` does not name.
///
/// The value is read AS A STRING through the codec itself, which is both where a strategy
/// comes from and what refuses a value that is no string at all: such a value is no strategy
/// and no reading of one either, and it is refused in the codec's own words rather than in
/// any written here. Reading it here is what keeps the body's own reading in step whichever
/// branch is taken — the value is consumed either way, so the one this answers `None` for
/// leaves the reader exactly where the one it hands back does.
fn stated_strategy(
    member: &Schema,
    inner: &mut dyn ShapeDeserializer,
    declared: &[&str],
) -> Result<Option<String>, SerdeError> {
    let stated = inner.read_string(member)?;
    Ok(declared.contains(&stated.as_str()).then_some(stated))
}

/// The inner deserializer one [`Reading`] forwards to, however it is held.
///
/// The protocol seam hands back an owned `Box<dyn ShapeDeserializer>` while a nested walk
/// hands each level a `&mut dyn ShapeDeserializer`, and both wear the same wrapper — so the
/// wrapper is generic over this and the forwarding body below is written once.
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

/// The deserializer an input is read with: every call is the codec's own, except a structure
/// member carrying a content-handling strategy, which is read as the module header states.
///
/// A nested structure, list or map re-enters the wrapper, because the codec walks each level
/// with its own deserializer.
struct Reading<D>(D);

impl<D: InnerDeserializer> ShapeDeserializer for Reading<D> {
    /// The one place the reading is applied: a member stating a strategy none of its enum's
    /// values names is not handed to the consumer at all, so the input carries no value for
    /// it and the request is served as if it had been omitted.
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0.get().read_struct(schema, &mut |member, inner| {
            let Some(declared) = declared_strategies(member) else {
                return consumer(member, &mut Reading(inner));
            };
            match stated_strategy(member, inner, declared)? {
                Some(strategy) => consumer(member, &mut Stated(strategy)),
                None => Ok(()),
            }
        })
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_list(schema, &mut |inner| consumer(&mut Reading(inner)))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_map(schema, &mut |key, inner| consumer(key, &mut Reading(inner)))
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

/// The deserializer the strategy a content-handling member stated is handed to the consumer
/// through: it reads nothing further of the request and answers with that one value.
///
/// The member targets an enum, which is encoded as a STRING, so the string is the only thing
/// asked of it and every other read states that this is not a value of another type — a
/// sentence no request reaches, because the codec itself has already refused a value that is
/// no string ([`stated_strategy`]).
struct Stated(String);

impl Stated {
    /// What a read of another type is answered with: a content-handling strategy is the
    /// string it is.
    fn not_a_strategy(&self, asked: &str) -> SerdeError {
        SerdeError::custom(format!(
            "a content-handling strategy is read as a string, not as {asked}"
        ))
    }
}

impl ShapeDeserializer for Stated {
    fn read_struct(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Err(self.not_a_strategy("a structure"))
    }

    fn read_list(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Err(self.not_a_strategy("a list"))
    }

    fn read_map(
        &mut self,
        _schema: &Schema,
        _consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        Err(self.not_a_strategy("a map"))
    }

    fn read_boolean(&mut self, _schema: &Schema) -> Result<bool, SerdeError> {
        Err(self.not_a_strategy("a boolean"))
    }

    fn read_byte(&mut self, _schema: &Schema) -> Result<i8, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_short(&mut self, _schema: &Schema) -> Result<i16, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_integer(&mut self, _schema: &Schema) -> Result<i32, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_long(&mut self, _schema: &Schema) -> Result<i64, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_float(&mut self, _schema: &Schema) -> Result<f32, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_double(&mut self, _schema: &Schema) -> Result<f64, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_big_integer(&mut self, _schema: &Schema) -> Result<BigInteger, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    fn read_big_decimal(&mut self, _schema: &Schema) -> Result<BigDecimal, SerdeError> {
        Err(self.not_a_strategy("a number"))
    }

    /// The one read a content-handling member makes: the strategy the body stated.
    fn read_string(&mut self, _schema: &Schema) -> Result<String, SerdeError> {
        Ok(self.0.clone())
    }

    fn read_blob(&mut self, _schema: &Schema) -> Result<Blob, SerdeError> {
        Err(self.not_a_strategy("a blob"))
    }

    fn read_timestamp(&mut self, _schema: &Schema) -> Result<DateTime, SerdeError> {
        Err(self.not_a_strategy("a timestamp"))
    }

    fn read_document(&mut self, _schema: &Schema) -> Result<Document, SerdeError> {
        Ok(Document::String(self.0.clone()))
    }

    /// The member stated a value, so it is not absent: a reader that called itself null would
    /// have the generated read skip the strategy the body does carry.
    fn is_null(&self) -> bool {
        false
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        Err(self.not_a_strategy("null"))
    }

    fn container_size(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::declared_strategies;
    use crate::ops::types::CONTENT_HANDLING_STRATEGY_VALUES;

    /// RULE: a content-handling member is the one whose own schema declares the
    /// content-handling value set; every other member of the same input shape — a URI label,
    /// a member of another enum, a map, a plain string — is not one, and is read by the codec
    /// alone.
    ///
    /// Asked of the REAL member schemas of this model's writes, so the predicate is pinned
    /// against the model rather than against a schema written here: sources
    /// `smithy_query(query="shape", shape="UpdateIntegrationResponseRequest")` and
    /// `smithy_query(query="shape", shape="CreateIntegrationRequest")`.
    #[test]
    fn only_a_member_declaring_the_content_handling_value_set_is_read_as_a_strategy() {
        let rows: [(&'static aws_smithy_schema::Schema, &str, bool); 8] = [
            (
                crate::ops::types::UPDATE_INTEGRATION_RESPONSE_INPUT_SCHEMA,
                "ContentHandlingStrategy",
                true,
            ),
            (
                crate::ops::types::UPDATE_INTEGRATION_RESPONSE_INPUT_SCHEMA,
                "IntegrationResponseKey",
                false,
            ),
            (
                crate::ops::types::UPDATE_INTEGRATION_RESPONSE_INPUT_SCHEMA,
                "ApiId",
                false,
            ),
            (
                crate::ops::types::UPDATE_INTEGRATION_RESPONSE_INPUT_SCHEMA,
                "ResponseTemplates",
                false,
            ),
            (
                crate::ops::types::CREATE_INTEGRATION_RESPONSE_INPUT_SCHEMA,
                "ContentHandlingStrategy",
                true,
            ),
            (
                crate::ops::types::CREATE_INTEGRATION_INPUT_SCHEMA,
                "ContentHandlingStrategy",
                true,
            ),
            // Another enum of the same input shape: judged by its own value set, not read as
            // a strategy.
            (
                crate::ops::types::CREATE_INTEGRATION_INPUT_SCHEMA,
                "IntegrationType",
                false,
            ),
            (
                crate::ops::types::CREATE_INTEGRATION_INPUT_SCHEMA,
                "PassthroughBehavior",
                false,
            ),
        ];

        for (schema, member_name, expected) in rows {
            let member = schema
                .members()
                .iter()
                .find(|member| member.member_name() == Some(member_name))
                .unwrap_or_else(|| panic!("case {member_name:?}: the shape declares this member"));
            let read = declared_strategies(member);
            assert_eq!(
                read.is_some(),
                expected,
                "case {member_name:?}: expected a content-handling member {expected}, got \
                 {read:?}"
            );
            if expected {
                assert_eq!(
                    read,
                    Some(CONTENT_HANDLING_STRATEGY_VALUES),
                    "case {member_name:?}: expected the model's own value set, got {read:?}"
                );
            }
        }
    }
}
