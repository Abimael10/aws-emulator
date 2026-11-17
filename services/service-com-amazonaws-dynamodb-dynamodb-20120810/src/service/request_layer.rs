// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a body value whose JSON class the
//! modelled member it is bound to cannot be read from is refused.
//!
//! Two refusals happen before a handler runs, and they are NOT the same error class:
//!
//! * a value whose JSON class the modelled type of its member admits no reading of — a
//!   number where the model declares a string, a string where it declares a long, a scalar
//!   where it declares a list — never becomes a value at all. That is a failure of the
//!   MARSHALLING, which AWS answers at the deserialization layer: HTTP 400 under the
//!   protocol's own [`SERIALIZATION_EXCEPTION`], with the reason under the capitalized
//!   [`MESSAGE_MEMBER`] — the member a protocol-level error of this service states its prose
//!   in, which is not the lowercase `message` its MODELLED exceptions declare. Nothing is
//!   read, so nothing is looked up and nothing is written;
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers as its `ValidationException`
//!   ([`super::EmulatedService::override_refusal`]).
//!
//! The framework renders both in the constraint-violation envelope
//! (`vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code and the lowercase member name), so a client branching on the error class
//! to decide whether to fix its serializer or fix a value takes the wrong branch, and a tool
//! that reads the capital-`Message` slot of this class finds nothing there. The vendored
//! tree is read-only from a service crate, so the separation is made here, inside the one
//! scope every request this service serves already passes through
//! ([`super::OperationScopedClauseForm`]) — as the request layers of
//! `services/service-com-amazonaws-emr-elasticmapreduce`,
//! `services/service-com-amazonaws-xray-awsxray` and
//! `services/service-com-amazonaws-kinesis-kinesis-20131202` do for their own services.
//!
//! Nothing about the model is written down here. WHETHER a body can be read is decided by
//! running the framework's OWN deserializer over it ([`probe`]): the same protocol, the same
//! input schema and the same generated `DeserializableStruct` the dispatch sequence uses, so
//! this layer cannot disagree with the walk that follows it. [`Probe`] wraps that
//! deserializer only to learn WHICH read failed — the modelled type that read was for, and
//! the path in the body it stood at, neither of which the codec's error carries — and leaves
//! the error itself untouched. Because the real codec descends the whole body, a value
//! nested in a structure, a list or a map is reached exactly as a top-level member is.
//!
//! # What is left to another layer, deliberately
//!
//! * bytes that are not a body of this protocol at all: there is then no value whose class
//!   could be named, and such a body is a defect of the BODY rather than of one value in it;
//! * an explicit `null`: absence is what a `null` states on this protocol, so what it earns
//!   is decided by whether the member it stands for is required, which is the constraint
//!   layer's question rather than this one's ([`value_class`] names no class for it);
//! * a read that failed for a reason that is not a value's JSON class — text that is not a
//!   timestamp, an absent `@required` member — which the framework and this service's own
//!   constraint wording answer, unchanged;
//! * a target type this layer states no name for ([`named_type`]): the sentence's two halves
//!   are the rule, so a half this service is not known to state is never invented.

use std::cell::RefCell;

use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};
use service_common::json_wire_values::{ARRAY_VALUE, OBJECT_VALUE};
use smithy_server_runtime::{DeserializableStruct, Operation, ServerHttpProtocol, Service};

/// The error code a value the body could not be read from is refused with, as this
/// service's protocol spells it: the absolute id of the PROTOCOL's own error shape, not one
/// of this service's modelled exceptions — the request never became an input, so no
/// operation ran to answer with an error of its own. What it must NOT be is the
/// constraint-refusal code, which is the answer to a value that WAS read.
pub const SERIALIZATION_EXCEPTION: &str = "com.amazon.coral.service#SerializationException";

/// The body member a protocol-level error of this service carries its sentence in: the
/// capitalized `Message`, which is where a client of this service reads the prose of this
/// error class. A MODELLED exception keeps the lowercase `message` its model declares and is
/// rendered by the framework, untouched by this layer.
pub const MESSAGE_MEMBER: &str = "Message";

/// The HTTP status a body this service could not read is refused with: the caller sent
/// something no operation could be given, so it is a client error.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// What the refusal states when the member's modelled type is an AGGREGATE — a list, a map,
/// a structure or a union — and the body carried something else there, or when a member
/// modelled as a single value carried a collection: no conversion between two single values
/// was even attempted, so what the sentence states is that the FIELD's type was unexpected,
/// naming neither a class nor a type.
const UNEXPECTED_FIELD_TYPE: &str = "Unexpected field type";

/// Dispatch [`probe`] on the routing key, one line per modelled operation.
///
/// The arm's key is the operation type's own name, because this service's protocol
/// (`smithy_query(query="service_info")`: awsJson1_0) resolves its routing key to the
/// operation shape name, which is also the name of the generated operation type: there is no
/// second spelling of the surface to drift from the first.
macro_rules! probe_by_key {
    ($key:expr, $protocol:expr, $req:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        match $key {
            $(
                stringify!($operation) => {
                    probe::<crate::ops::$module::$operation<Eng>>($protocol, $req)
                }
            )+
            _ => None,
        }
    };
}

/// The sentence refusing `req`'s body, when a value it carries cannot be read into the
/// modelled type of the member it is bound to, for the operation `key` names.
///
/// `None` is "nothing for THIS layer to refuse", which covers four different outcomes
/// deliberately: the body read cleanly; the body could not be reached at all (there is no
/// value to describe, so the framework's own rendering stands); the read failed for a reason
/// that is NOT a value's JSON class; and a key naming no modelled operation, which is left
/// to the routing table's own refusal.
pub fn coercion_refusal<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    probe_by_key!(
        key,
        protocol,
        req,
        batch_execute_statement::BatchExecuteStatement,
        batch_get_item::BatchGetItem,
        batch_write_item::BatchWriteItem,
        create_backup::CreateBackup,
        create_global_table::CreateGlobalTable,
        create_table::CreateTable,
        delete_backup::DeleteBackup,
        delete_item::DeleteItem,
        delete_resource_policy::DeleteResourcePolicy,
        delete_table::DeleteTable,
        describe_backup::DescribeBackup,
        describe_continuous_backups::DescribeContinuousBackups,
        describe_contributor_insights::DescribeContributorInsights,
        describe_endpoints::DescribeEndpoints,
        describe_export::DescribeExport,
        describe_global_table::DescribeGlobalTable,
        describe_global_table_settings::DescribeGlobalTableSettings,
        describe_import::DescribeImport,
        describe_kinesis_streaming_destination::DescribeKinesisStreamingDestination,
        describe_limits::DescribeLimits,
        describe_table::DescribeTable,
        describe_table_replica_auto_scaling::DescribeTableReplicaAutoScaling,
        describe_time_to_live::DescribeTimeToLive,
        disable_kinesis_streaming_destination::DisableKinesisStreamingDestination,
        enable_kinesis_streaming_destination::EnableKinesisStreamingDestination,
        execute_statement::ExecuteStatement,
        execute_transaction::ExecuteTransaction,
        export_table_to_point_in_time::ExportTableToPointInTime,
        get_item::GetItem,
        get_resource_policy::GetResourcePolicy,
        import_table::ImportTable,
        list_backups::ListBackups,
        list_contributor_insights::ListContributorInsights,
        list_exports::ListExports,
        list_global_tables::ListGlobalTables,
        list_imports::ListImports,
        list_tables::ListTables,
        list_tags_of_resource::ListTagsOfResource,
        put_item::PutItem,
        put_resource_policy::PutResourcePolicy,
        query::Query,
        restore_table_from_backup::RestoreTableFromBackup,
        restore_table_to_point_in_time::RestoreTableToPointInTime,
        scan::Scan,
        search_vectors::SearchVectors,
        tag_resource::TagResource,
        transact_get_items::TransactGetItems,
        transact_write_items::TransactWriteItems,
        untag_resource::UntagResource,
        update_continuous_backups::UpdateContinuousBackups,
        update_contributor_insights::UpdateContributorInsights,
        update_global_table::UpdateGlobalTable,
        update_global_table_settings::UpdateGlobalTableSettings,
        update_item::UpdateItem,
        update_kinesis_streaming_destination::UpdateKinesisStreamingDestination,
        update_table::UpdateTable,
        update_table_replica_auto_scaling::UpdateTableReplicaAutoScaling,
        update_time_to_live::UpdateTimeToLive,
    )
}

/// `message` as the PROTOCOL's own refusal of a body that could not be read answers it, in
/// the envelope of the protocol `req` is served in — which also writes the `__type`
/// discriminator and the media type.
///
/// The shape is synthetic because no model declares it: the request never became an input,
/// so no operation ran to answer with one of its own errors. `None` when the body cannot be
/// encoded at all, which leaves the framework's own rendering to answer.
pub fn serialization_refusal(
    svc: &dyn Service,
    req: &RtRequest,
    message: &str,
) -> Option<aws_smithy_runtime_api::http::Response> {
    svc.definition()
        .wire_protocol(req)
        .protocol()
        .serialize_error(
            &SerializationException {
                message: message.to_string(),
            },
            &SERIALIZATION_EXCEPTION_SCHEMA,
            SERIALIZATION_EXCEPTION,
            SERIALIZATION_EXCEPTION_STATUS,
        )
        .ok()
}

/// Run the framework's own deserializer for `O` over the request, and report the first value
/// whose JSON class could not be read into the modelled type of the member it is bound to.
///
/// A body that READS is this layer's business no further: whatever the model's own
/// constraints then say about its values is a constraint refusal, which the walk that
/// follows answers.
///
/// The CLASS the sentence names is the one the codec's own complaint named the value under,
/// which is read off the vendored codec's wording ([`value_class`]); the path the probe
/// recorded is what makes the submitted value itself reachable, and is where a class that
/// the codec states as a framing token rather than a value is recovered from.
fn probe<O: Operation>(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> Option<String> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let found: RefCell<Option<Unreadable>> = RefCell::new(None);
    let read = {
        let mut probe = Probe {
            inner: &mut *deserializer,
            found: &found,
        };
        <O::Input as DeserializableStruct>::deserialize(&mut probe)
    };
    if read.is_ok() {
        return None;
    }
    let unreadable = found.into_inner()?;
    sentence(unreadable.class, unreadable.target)
}

/// The sentence one unreadable value is refused with, or `None` when this service states no
/// name for the type the failing read was for.
///
/// Two sentences, because a caller has two different defects to fix. A single value sent
/// where the model declares another single value is a CONVERSION that could not be
/// performed, and the sentence names the two facts to reconcile: the class of value sent and
/// the type it could not become. Anything with a collection on either side — a scalar where
/// the model declares a list, a structure or a map, or a collection where it declares one
/// value — is not a conversion at all but a field of the wrong TYPE
/// ([`UNEXPECTED_FIELD_TYPE`]).
fn sentence(class: &str, target: ShapeType) -> Option<String> {
    if is_aggregate(target) || class == ARRAY_VALUE || class == OBJECT_VALUE {
        return Some(UNEXPECTED_FIELD_TYPE.to_string());
    }
    Some(format!("{class} cannot be converted to {}", named_type(target)?))
}

/// Whether the modelled type is one that holds OTHER values rather than being one.
fn is_aggregate(target: ShapeType) -> bool {
    matches!(
        target,
        ShapeType::List | ShapeType::Map | ShapeType::Structure | ShapeType::Union
    )
}

/// What this service calls the type a value could not be converted to.
///
/// A member's modelled type is named by its own Smithy shape type, with no article in front
/// of it, which is what the recorded refusals of a string member and of a long member state
/// ("NUMBER_VALUE cannot be converted to String", "STRING_VALUE cannot be converted to
/// Long") — an enumerated type IS a string shape in these schemas, so it is named the same.
/// A target this service is not known to state a name for answers `None`, which leaves that
/// value to the codec rather than refusing it under a name nothing here could ground.
fn named_type(target: ShapeType) -> Option<&'static str> {
    match target {
        ShapeType::String => Some("String"),
        ShapeType::Boolean => Some("Boolean"),
        ShapeType::Byte => Some("Byte"),
        ShapeType::Short => Some("Short"),
        ShapeType::Integer => Some("Integer"),
        ShapeType::Long => Some("Long"),
        ShapeType::Float => Some("Float"),
        ShapeType::Double => Some("Double"),
        _ => None,
    }
}

/// What one unreadable value is refused with: the type the failing read was FOR, and the
/// class the codec's own complaint named the submitted value under.
#[derive(Debug)]
struct Unreadable {
    target: ShapeType,
    class: &'static str,
}

/// The framework's deserializer, watched.
///
/// Every method forwards to the real deserializer and returns its answer unchanged — this is
/// not a codec, and it reimplements no part of one. The ONE thing it adds is that a read
/// which fails on a JSON class mismatch is recorded with the modelled type that read was
/// for, which the error does not carry. The first such read wins: it is the one the caller
/// has to fix first, and the reads after it are reading a body the parser has already left.
struct Probe<'d, 'f> {
    inner: &'d mut (dyn ShapeDeserializer + 'd),
    found: &'f RefCell<Option<Unreadable>>,
}

/// Record `error` as a coercion failure into `target`, when that is what it is, and return
/// it untouched.
///
/// A free function rather than a method, so a failing read can hand it the recorder while
/// the deserializer it is reading through is still borrowed. The first record wins, which is
/// why a nested read's own failure is never overwritten by the container it propagates
/// through.
///
/// `target` is the type the FAILING READ was for, not the shape type of the `Schema` that
/// read was handed: the two agree for a member of a structure, and only the read knows the
/// element type of a collection, because the JSON codec hands a collection's elements the
/// collection member's own schema.
fn note_in(
    found: &RefCell<Option<Unreadable>>,
    target: ShapeType,
    error: SerdeError,
) -> SerdeError {
    let Some(class) = value_class(&error) else {
        return error;
    };
    let mut held = found.borrow_mut();
    if held.is_none() {
        *held = Some(Unreadable { target, class });
    }
    error
}

/// The CLASS of JSON value a codec refusal is about, or `None` when the refusal is not about
/// a value's type.
///
/// The codec states a type mismatch as "expected <the type it wanted>, got <the JSON type it
/// found>" and a number outside the modelled integral width as "number … does not fit in
/// <width>" (`vendor/smithy-schema-server/src/json.rs` — `TokenKind`'s `Display` is the
/// vocabulary of the tail, chosen there precisely so it names a JSON type a caller can act
/// on). Those are the two refusals a value's own type earns; every other codec failure — an
/// absent member, text that is not a timestamp, a truncated body — says nothing about a JSON
/// type and is left to the framework.
///
/// JSON `null` is deliberately NOT a class: a null is absence, not a type error, and what an
/// absent member earns is the constraint layer's answer.
///
/// The wordings this reads are PINNED by [`tests::the_codec_wordings_this_layer_reads`]
/// below, so a change to them in the vendored codec fails loudly here instead of silently
/// sending every coercion failure back to `ValidationException`.
fn value_class(error: &SerdeError) -> Option<&'static str> {
    use service_common::json_wire_values as classes;
    let stated = error.to_string();
    if stated.contains("does not fit in") {
        return Some(classes::NUMBER_VALUE);
    }
    let found = stated.strip_prefix("expected ")?.split(", got ").nth(1)?;
    match found {
        "string" => Some(classes::STRING_VALUE),
        "number" => Some(classes::NUMBER_VALUE),
        "boolean" => Some(classes::BOOLEAN_VALUE),
        "array start" => Some(classes::ARRAY_VALUE),
        "object start" => Some(classes::OBJECT_VALUE),
        // "null", and the framing tokens ("object key", "array end", …), which describe
        // where the parser is rather than a value a caller sent.
        _ => None,
    }
}

/// Forward one scalar read to the real deserializer, recording a JSON class mismatch against
/// the type that read is FOR — which is the read's own target type, named beside it here,
/// rather than the shape type of the schema it is handed.
///
/// `found` is copied out of `self` BEFORE the read starts, so the recorder is still reachable
/// while the deserializer underneath is mutably borrowed.
macro_rules! watched_reads {
    ($($method:ident -> $value:ty as $target:ident),+ $(,)?) => {
        $(
            fn $method(&mut self, schema: &Schema) -> Result<$value, SerdeError> {
                let found = self.found;
                self.inner
                    .$method(schema)
                    .map_err(|error| note_in(found, ShapeType::$target, error))
            }
        )+
    };
}

impl ShapeDeserializer for Probe<'_, '_> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let found = self.found;
        let mut watched = |member: &Schema, inner: &mut dyn ShapeDeserializer| {
            let mut probe = Probe { inner, found };
            consumer(member, &mut probe)
        };
        self.inner
            .read_struct(schema, &mut watched)
            .map_err(|error| note_in(found, schema.shape_type(), error))
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let found = self.found;
        let mut watched = |inner: &mut dyn ShapeDeserializer| {
            let mut probe = Probe { inner, found };
            consumer(&mut probe)
        };
        self.inner
            .read_list(schema, &mut watched)
            .map_err(|error| note_in(found, schema.shape_type(), error))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let found = self.found;
        let mut watched = |key: String, inner: &mut dyn ShapeDeserializer| {
            let mut probe = Probe { inner, found };
            consumer(key, &mut probe)
        };
        self.inner
            .read_map(schema, &mut watched)
            .map_err(|error| note_in(found, schema.shape_type(), error))
    }

    // Every value read, forwarded and watched. The list is the trait's own scalar surface,
    // each read beside the modelled type it is for.
    watched_reads! {
        read_boolean -> bool as Boolean,
        read_byte -> i8 as Byte,
        read_short -> i16 as Short,
        read_integer -> i32 as Integer,
        read_long -> i64 as Long,
        read_float -> f32 as Float,
        read_double -> f64 as Double,
        read_big_integer -> BigInteger as BigInteger,
        read_big_decimal -> BigDecimal as BigDecimal,
        read_string -> String as String,
        read_blob -> Blob as Blob,
        read_document -> Document as Document,
        read_timestamp -> DateTime as Timestamp,
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.inner.read_null()
    }

    fn is_null(&self) -> bool {
        self.inner.is_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.inner.container_size()
    }
}

/// The `Message` member of the synthetic serialization-refusal shape.
static SERIALIZATION_EXCEPTION_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    MESSAGE_MEMBER,
    0,
);

/// The synthetic shape of a protocol-level serialization refusal: no model declares it and
/// no operation lists it, because it is the PROTOCOL's own error rather than one of this
/// service's.
static SERIALIZATION_EXCEPTION_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#SerializationException",
        "smithy.framework",
        "SerializationException",
    ),
    ShapeType::Structure,
    &[&SERIALIZATION_EXCEPTION_MESSAGE],
);

/// The payload of a serialization refusal: what the request could not be read as.
#[derive(Debug, Clone)]
struct SerializationException {
    message: String,
}

impl SerializableStruct for SerializationException {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        serializer.write_string(&SERIALIZATION_EXCEPTION_MESSAGE, self.message.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::aws_smithy_schema::codec::Codec;
    use ::aws_smithy_schema_server::json::JsonCodec;

    /// Read `body` into `T` through the same watching probe the request layer drives, and
    /// report the sentence it would be refused with — the JSON codec alone, so the case is
    /// about the codec's refusal and the class of the submitted value and nothing else.
    fn recorded<T: DeserializableStruct>(body: &str) -> Option<String> {
        let codec = JsonCodec::new();
        let mut deserializer = codec.create_deserializer(body.as_bytes());
        let found: RefCell<Option<Unreadable>> = RefCell::new(None);
        {
            let mut probe = Probe {
                inner: &mut deserializer,
                found: &found,
            };
            if T::deserialize(&mut probe).is_ok() {
                return None;
            }
        }
        let unreadable = found.into_inner()?;
        sentence(unreadable.class, unreadable.target)
    }

    /// RULE: a value of a class the modelled type of its member admits no reading of earns
    /// the sentence of its defect — the conversion that could not be performed between two
    /// single values, and the unexpected field type wherever a collection is involved — at
    /// whatever depth of the body it stands.
    ///
    /// This layer learns WHETHER a read failed on a value's JSON class from the text of the
    /// vendored codec's own refusal ("expected <type>, got <json type>", "number … does not
    /// fit in <width>"), which is the only place it is stated. The tree it comes from is
    /// read-only from here, so this case is also what makes a change to those wordings fail
    /// loudly instead of quietly sending every coercion failure back to the framework's
    /// `ValidationException`.
    #[test]
    fn the_codec_wordings_this_layer_reads() {
        use crate::ops::types::{CreateTableInput, ListTablesInput, TagResourceInput};

        // Every class, where the model declares a STRING: `CreateTable$TableName` targets
        // `TableArn` (`smithy_query(query="operation", operation="CreateTable")`). A
        // collection sent for it is not a conversion at all.
        let string_rows = [
            ("17", "NUMBER_VALUE cannot be converted to String"),
            ("17.5", "NUMBER_VALUE cannot be converted to String"),
            ("true", "BOOLEAN_VALUE cannot be converted to String"),
            ("false", "BOOLEAN_VALUE cannot be converted to String"),
            (r#"["t"]"#, UNEXPECTED_FIELD_TYPE),
            (r#"{"Name":"t"}"#, UNEXPECTED_FIELD_TYPE),
        ];
        for (sent, expected) in string_rows {
            let body = format!(r#"{{"TableName":{sent}}}"#);
            let stated = recorded::<CreateTableInput>(&body);
            assert_eq!(
                stated.as_deref(),
                Some(expected),
                "case {sent:?}: expected the refusal {expected:?}, got {stated:?}"
            );
        }

        // A value nested inside a STRUCTURE, reached by the codec's own descent:
        // `ProvisionedThroughput$ReadCapacityUnits` targets `PositiveLongObject` (long).
        let nested = recorded::<CreateTableInput>(
            r#"{"TableName":"t","ProvisionedThroughput":{"ReadCapacityUnits":"5"}}"#,
        );
        assert_eq!(
            nested.as_deref(),
            Some("STRING_VALUE cannot be converted to Long"),
            "a string where a nested long is declared: got {nested:?}"
        );

        // A value nested inside a LIST's element structure: `Tag$Key` targets `TagKeyString`
        // (string), and the element itself is a structure.
        let in_element = recorded::<TagResourceInput>(
            r#"{"ResourceArn":"arn","Tags":[{"Key":1,"Value":"v"}]}"#,
        );
        assert_eq!(
            in_element.as_deref(),
            Some("NUMBER_VALUE cannot be converted to String"),
            "a number where a list element's string member is declared: got {in_element:?}"
        );
        let element = recorded::<TagResourceInput>(r#"{"ResourceArn":"arn","Tags":["k=v"]}"#);
        assert_eq!(
            element.as_deref(),
            Some(UNEXPECTED_FIELD_TYPE),
            "a scalar where a list of structures is declared: got {element:?}"
        );

        // A scalar where the model declares the LIST itself, and a number past the modelled
        // integral width (`ListTables$Limit` targets `ListTablesInputLimit`, an integer).
        let whole_list = recorded::<TagResourceInput>(r#"{"ResourceArn":"arn","Tags":"k=v"}"#);
        assert_eq!(
            whole_list.as_deref(),
            Some(UNEXPECTED_FIELD_TYPE),
            "a scalar where a list is declared: got {whole_list:?}"
        );
        let wide = recorded::<ListTablesInput>(r#"{"Limit":2147483648}"#);
        assert_eq!(
            wide.as_deref(),
            Some("NUMBER_VALUE cannot be converted to Integer"),
            "a number past the modelled integer width: got {wide:?}"
        );

        // And the reads that are NOT about a value's JSON class: an explicit null, and a
        // body whose every value reads. Neither is this layer's to answer.
        for body in [r#"{"TableName":null}"#, r#"{"TableName":"t","Limit":7}"#] {
            let stated = recorded::<CreateTableInput>(body);
            assert_eq!(
                stated, None,
                "case {body:?}: nothing here is about a value's JSON class, got {stated:?}"
            );
        }
    }
}
