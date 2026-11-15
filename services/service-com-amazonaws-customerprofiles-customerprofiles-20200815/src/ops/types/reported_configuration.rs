// SPDX-License-Identifier: CC0-1.0

//! The configuration structures a calculated-attribute definition response carries even
//! when the definition configured none of them.
//!
//! RULE: a response that reports a stored calculated attribute definition carries the
//! definition's configuration structures — `Conditions` and `Filter` — PRESENT for as
//! long as the definition exists, with every member the definition left unset written as
//! JSON `null`. A caller can then tell "this definition configures no conditions" from
//! "conditions were not reported", which is the whole point of reading a definition back.
//!
//! [`ReportedConditions`] and [`ReportedFilter`] are the response field types that say
//! so. Each wraps the configuration the definition stored (`None` for a definition that
//! configured none), writes the members that configuration DID set through the modelled
//! shape's own serializer, and writes JSON `null` for each of the shape's remaining
//! members. The modelled shapes and their schemas stay exactly as the model declares
//! them: nothing here redeclares a shape, and a definition that DID configure a filter or
//! conditions is reported through the generated serializer unchanged.
//!
//! Writing `null` is the only way to report `Filter` as present-but-unconfigured at all:
//! `Filter$Include` and `Filter$Groups` are both `@required`, so the derived
//! [`Filter`] field types are non-`Option` and the derived serializer writes both members
//! unconditionally — a `Filter::default()` would report an empty `Include` and an empty
//! `Groups`, neither of which is inside the member's own value space.

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::Schema;
use ::aws_smithy_schema_server::DeserializableStruct;

use super::{Conditions, Filter, CONDITIONS_SCHEMA, FILTER_SCHEMA};

/// The member of `schema` the model names `name`, or `None` when the shape declares no
/// such member — in which case there is no member schema to write a value under and
/// nothing is written, rather than a key the model does not declare being invented.
fn member<'s>(schema: &'s Schema, name: &str) -> Option<&'s Schema> {
    schema
        .members()
        .iter()
        .copied()
        .find(|member| member.member_name() == Some(name))
}

/// Write each of `names` as JSON `null` under `schema`'s member of that name.
fn write_nulls(
    serializer: &mut dyn ShapeSerializer,
    schema: &Schema,
    names: &[&str],
) -> Result<(), SerdeError> {
    for name in names {
        if let Some(member) = member(schema, name) {
            serializer.write_null(member)?;
        }
    }
    Ok(())
}

/// `Conditions` as a calculated-attribute definition projection reports it: the conditions
/// the definition configured, or the structure with every member `null` for a definition
/// that configured none.
#[derive(Debug, Clone, Default)]
pub struct ReportedConditions(pub Option<Conditions>);

impl SerializableStruct for ReportedConditions {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        // A definition that configured no conditions reports the same structure as one
        // whose every condition member is unset, so both go through one path.
        let unconfigured = Conditions::default();
        let conditions = self.0.as_ref().unwrap_or(&unconfigured);
        conditions.serialize_members(serializer)?;

        // The three members `Conditions` declares, each reported null when the definition
        // set none: `smithy_query(query="shape", shape="Conditions")` reports `Range`,
        // `ObjectCount` and `Threshold`, all optional.
        let mut unset: Vec<&str> = Vec::new();
        if conditions.range.is_none() {
            unset.push("Range");
        }
        if conditions.object_count.is_none() {
            unset.push("ObjectCount");
        }
        if conditions.threshold.is_none() {
            unset.push("Threshold");
        }
        write_nulls(serializer, CONDITIONS_SCHEMA, &unset)
    }
}

impl DeserializableStruct for ReportedConditions {
    fn deserialize(deserializer: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        Ok(Self(Some(Conditions::deserialize(deserializer)?)))
    }
}

/// `Filter` as a calculated-attribute definition projection reports it: the filter the
/// definition configured, or the structure with every member `null` for a definition that
/// configured none.
#[derive(Debug, Clone, Default)]
pub struct ReportedFilter(pub Option<Filter>);

impl SerializableStruct for ReportedFilter {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        match &self.0 {
            // Both of `Filter`'s members are `@required`, so a configured filter has both
            // and the modelled serializer writes both.
            Some(filter) => filter.serialize_members(serializer),
            // The two members `Filter` declares:
            // `smithy_query(query="shape", shape="Filter")` reports `Include` and
            // `Groups`.
            None => write_nulls(serializer, FILTER_SCHEMA, &["Include", "Groups"]),
        }
    }
}

impl DeserializableStruct for ReportedFilter {
    fn deserialize(deserializer: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        Ok(Self(Some(Filter::deserialize(deserializer)?)))
    }
}
