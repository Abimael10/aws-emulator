// SPDX-License-Identifier: CC0-1.0

//! The refusal that comes BEFORE every constraint: a value in the request body that the
//! modelled member it is bound to cannot be read from at all.
//!
//! Such a value never becomes a value, so no bound on it is ever reached: it is a
//! SERIALIZATION fault, not a constraint violation, and the two are different error classes
//! to a caller. A client branching on the error code decides from it whether to fix its
//! serializer or fix a value, so this service answers the first class as the protocol's own
//! [`SERIALIZATION_EXCEPTION`] (HTTP 400) carrying the sentence under the capitalized
//! [`MESSAGE_MEMBER`] — the member a protocol-level error of this service states its prose
//! in, which is not the lowercase `message` its MODELLED exceptions declare — while a value
//! that was read and then broke a declared rule stays this service's `ValidationException`
//! ([`super::validation`]).
//!
//! The framework renders BOTH as a constraint refusal (`vendor/smithy-server-runtime/src/
//! pre_dispatch.rs`, where every `ProtocolError::Serde` takes that code and the envelope's
//! member name is the lowercase one), and the vendored tree is read-only from a service
//! crate, so the separation is made here, at the one seam a service has before dispatch
//! ([`super::StatedPerOperation`]) — as the request layers of
//! `services/service-com-amazonaws-xray-awsxray` and
//! `services/service-com-amazonaws-emr-elasticmapreduce` do for their own services.
//!
//! The sentence states the two facts a caller has to reconcile: the VALUE it sent, quoted,
//! and the type that value could not be read as. Both halves are the tree's shared ones
//! ([`service_common::json_wire_values`] for the sentence and for which classes read into
//! which modelled type, [`service_common::validation::quoted_echo`] for the ceiling one
//! quoted value is held to), so nothing of the wording is spelled twice.
//!
//! Nothing about the model is written down here: the member a body key names and its
//! modelled target type come off the operation's own input schema, which is the schema the
//! codec reads the body with.
//!
//! # The two representations
//!
//! The body arrives in whichever encoding of `awsJson1_1` the caller asked for
//! ([`super::cbor`]), and the one thing an encoding changes about reading a value is the
//! wire form of a BLOB: JSON carries one as base64 TEXT, CBOR as a byte string. So the
//! classes are read per encoding ([`submitted_values`]) and that single difference is stated
//! once ([`reads_into`]); everything after it — which class reads into which modelled type,
//! the sentence, the error class, the envelope — is shared.
//!
//! # What is left to the codec, deliberately
//!
//! * bytes that are not a body of that encoding at all — there is no value whose class could
//!   be named, so the codec's own refusal stands;
//! * a value nested inside a list, a map or a structure: a member `Schema` in this framework
//!   carries its target's KIND but not its target's own schema, so an element shape is not
//!   reachable from a top-level member. Such a value is refused by the codec;
//! * text at a blob member of a JSON body: a blob IS base64 text there, and the rule reads the
//!   CLASS of a value rather than its contents, so text that is not valid base64 is left to
//!   the codec — which answers it in the framework's own envelope;
//! * a value with no quotable text (a byte string, a collection) and a target type with no
//!   name this service states ([`named_type`]): the sentence's halves are the rule, so a
//!   half this layer cannot state is never invented.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use serde_json::{Number, Value};
use service_common::json_wire_values::{member_by_wire_name, reads_as, unreadable_message, value_text};
use smithy_server_runtime::{Operation, ServerHttpProtocol};

/// The error code a value the body could not be read from is refused with.
///
/// It is the PROTOCOL's error, not one of this service's modelled shapes: the request never
/// became an input, so no operation ran to answer with an error of its own. It is also the
/// code the framework itself names for a body it cannot process at all
/// (`pre_dispatch.rs`'s catch-all arm), which is the same refusal one unreadable value
/// earns. What it must NOT be is the constraint-refusal code, which is the answer to a value
/// that WAS read.
pub const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The body member a protocol-level error of this service carries its sentence in: the
/// capitalized `Message`, which is where a client of this service reads the prose of this
/// error class. A MODELLED exception of this service keeps the lowercase `message` its model
/// declares and is rendered by the framework, untouched by this layer.
pub const MESSAGE_MEMBER: &str = "Message";

/// The HTTP status a body the service could not read is refused with: the caller sent
/// something no operation could be given, so it is a client error.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// Which encoding of this service's `awsJson1_1` protocol a body is in — the one thing an
/// encoding changes about reading a value out of it ([`reads_into`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// `application/x-amz-json-1.1`: a blob is base64 text.
    Json,
    /// [`super::cbor::CONTENT_TYPE`]: a blob is a byte string.
    Cbor,
}

impl Wire {
    /// The encoding `req` asked for.
    #[must_use]
    pub fn of(req: &RtRequest) -> Self {
        if super::cbor::is_requested(req) {
            Self::Cbor
        } else {
            Self::Json
        }
    }
}

/// The answer to `req` when a value its body carries cannot be read into the modelled type
/// of the member it is bound to, for the operation `O` it was routed to: `400`
/// [`SERIALIZATION_EXCEPTION`] stating that value and that type, encoded by `protocol` — the
/// protocol of the encoding the request arrived in, so the refusal is readable by the client
/// that sent it.
///
/// `None` is "nothing for THIS layer to refuse", which covers every other outcome
/// deliberately: the body reads cleanly, carries no value this layer can name, or fails for
/// a reason that is not a coercion — all of which the framework and this service's own
/// constraint wording answer, unchanged.
#[must_use]
pub fn refusal_for<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    wire: Wire,
    req: &RtRequest,
) -> Option<RtResponse> {
    let message = unreadable_value(O::INPUT_SCHEMA, wire, req.body().bytes()?)?;
    serialization_exception(protocol, &message)
}

/// The FIRST value in `body` whose wire class cannot be read into the modelled target type
/// of the member it is bound to, as the sentence refusing it; `None` when every value the
/// body carries for a member of `input_schema` reads into it.
fn unreadable_value(input_schema: &Schema, wire: Wire, body: &[u8]) -> Option<String> {
    submitted_values(wire, body)?
        .iter()
        // A key naming no member is ignored, exactly as the codec ignores it.
        .filter_map(|(key, value)| Some((member_by_wire_name(input_schema, key)?, value)))
        .filter(|(member, value)| !reads_into(value, member.shape_type(), wire))
        .find_map(|(member, value)| unreadable_sentence(value, member.shape_type()))
}

/// The most top-level members this layer reads out of one body.
///
/// It bounds the entries this layer collects per request by a constant rather than by the
/// request, so no caller can make the walk hold an entry per key of an arbitrarily large body.
/// It is far past the number of members any input shape of this service's model declares (the
/// largest declare fewer than a dozen,
/// `smithy_query(query="operation",operation="CreateChannel")`), so every body a client of a
/// modelled operation sends is walked whole; past it the codec's own refusal stands.
///
/// The CBOR reader honours it as it streams, so it never holds more than that many entries.
/// The JSON reader is handed a value tree the JSON parser already built from the whole body —
/// the same parse the codec itself performs on the same bytes, whose size the framework's own
/// body limit bounds — and the cap applies to what this layer then keeps.
const MAX_WALKED_MEMBERS: usize = 256;

/// The value the body carries for each of its top-level keys, in the body's own order and at
/// most [`MAX_WALKED_MEMBERS`] of them, or `None` when the bytes are not a body of this
/// encoding at all.
fn submitted_values(wire: Wire, body: &[u8]) -> Option<Vec<(String, WireValue)>> {
    match wire {
        Wire::Json => json_values(body),
        Wire::Cbor => cbor_values(body),
    }
}

/// The top-level members of a JSON body, by the classes JSON distinguishes.
fn json_values(body: &[u8]) -> Option<Vec<(String, WireValue)>> {
    let Value::Object(members) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };
    Some(
        members
            .into_iter()
            .take(MAX_WALKED_MEMBERS)
            .map(|(key, held)| (key, WireValue::of_json(held)))
            .collect(),
    )
}

/// The top-level members of a CBOR body, by the classes CBOR distinguishes — which include
/// the byte string a blob is carried as, the one class JSON has no form for.
///
/// The reader is the CBOR implementation the framework's own codec is built on
/// (`aws-smithy-cbor`), so this cannot disagree with that codec about where one item ends
/// and the next begins. A body it cannot read is `None`: the codec's own refusal stands.
fn cbor_values(body: &[u8]) -> Option<Vec<(String, WireValue)>> {
    let mut decoder = ::aws_smithy_cbor::Decoder::new(body);
    // A CBOR map is written either with its length (`{...}` of n entries) or as an
    // indefinite-length map terminated by a break; both are valid, and an encoder picks.
    let declared = decoder.map().ok()?;
    let mut members = Vec::new();
    while members.len() < MAX_WALKED_MEMBERS {
        match declared {
            Some(entries) if members.len() as u64 == entries => break,
            Some(_) => {}
            None => {
                if matches!(decoder.datatype().ok()?, ::aws_smithy_cbor::data::Type::Break) {
                    break;
                }
            }
        }
        let key = decoder.string().ok()?;
        members.push((key, WireValue::read_cbor(&mut decoder)?));
    }
    Some(members)
}

/// One value a request body carries, as the CLASSES a representation of this protocol
/// distinguishes on the wire.
#[derive(Debug, Clone, PartialEq)]
enum WireValue {
    /// A text string.
    Text(String),
    /// A number, as the text the body spells it with.
    Number(Number),
    /// A boolean.
    Flag(bool),
    /// A byte string — the form CBOR carries a blob in, and one JSON has no form for.
    Bytes,
    /// An array.
    List,
    /// A map or object.
    Map,
    /// An explicit null, which is ABSENCE rather than a type error: an absent member the
    /// model requires is the constraint layer's refusal.
    Absent,
    /// A form this layer does not classify, which it therefore refuses nothing for.
    Unclassified,
}

impl WireValue {
    /// One JSON value by its class. JSON has no byte string: a blob is base64 text there.
    fn of_json(value: Value) -> Self {
        match value {
            Value::Null => Self::Absent,
            Value::Bool(flag) => Self::Flag(flag),
            Value::Number(number) => Self::Number(number),
            Value::String(text) => Self::Text(text),
            Value::Array(_) => Self::List,
            Value::Object(_) => Self::Map,
        }
    }

    /// The next CBOR item's class, consuming the item; `None` when it cannot be read at all.
    fn read_cbor(decoder: &mut ::aws_smithy_cbor::Decoder<'_>) -> Option<Self> {
        use ::aws_smithy_cbor::data::Type;
        let value = match decoder.datatype().ok()? {
            Type::String | Type::StringIndef => Self::Text(decoder.string().ok()?),
            Type::Bool => Self::Flag(decoder.boolean().ok()?),
            Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::I8 | Type::I16 | Type::I32
            | Type::I64 | Type::Int => match decoder.long() {
                Ok(whole) => Self::Number(Number::from(whole)),
                // A whole number past the widest signed form this reader has. The read itself
                // consumed the item, so the walk over the rest of the body continues and
                // nothing is refused for it (pinned by the row for it in the tests below,
                // which reads a member AFTER such a value).
                Err(_) => Self::Unclassified,
            },
            Type::F16 | Type::F32 | Type::F64 => Number::from_f64(decoder.double().ok()?)
                .map_or(Self::Unclassified, Self::Number),
            Type::Null => {
                decoder.null().ok()?;
                Self::Absent
            }
            Type::Bytes | Type::BytesIndef => {
                decoder.skip().ok()?;
                Self::Bytes
            }
            Type::Array | Type::ArrayIndef => {
                decoder.skip().ok()?;
                Self::List
            }
            Type::Map | Type::MapIndef => {
                decoder.skip().ok()?;
                Self::Map
            }
            // A tagged item (the form CBOR carries an instant in) and anything else: the
            // item is consumed so the walk over the rest of the body continues, and nothing
            // is refused for it.
            _ => {
                decoder.skip().ok()?;
                Self::Unclassified
            }
        };
        Some(value)
    }

    /// The same value as the JSON class it corresponds to, so the question "does this class
    /// read into that modelled type" is answered by the tree's shared rule. The two classes
    /// JSON has no form for ([`Self::Bytes`], [`Self::Unclassified`]) are answered before
    /// this is reached ([`reads_into`]).
    fn as_json(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Number(number) => Value::Number(number.clone()),
            Self::Flag(flag) => Value::Bool(*flag),
            // The rule reads the CLASS of the value, never its contents, so an empty
            // collection stands for every collection.
            Self::List => Value::Array(Vec::new()),
            Self::Map => Value::Object(serde_json::Map::new()),
            Self::Absent | Self::Bytes | Self::Unclassified => Value::Null,
        }
    }

    /// The value as a refusal QUOTES it, held to the ceiling one quoted value in a message
    /// carries; `None` for a class that has no text a caller wrote (a byte string, a
    /// collection, an absent member), for which this layer states no sentence.
    fn quoted(&self) -> Option<String> {
        match self {
            Self::Text(_) | Self::Number(_) | Self::Flag(_) => {
                let text = value_text(&self.as_json());
                Some(::service_common::validation::quoted_echo(&text).into_owned())
            }
            Self::Bytes | Self::List | Self::Map | Self::Absent | Self::Unclassified => None,
        }
    }
}

/// Whether `value`'s wire class can be READ into a member the model types as `target`.
///
/// Every class but one is the tree's shared rule
/// ([`service_common::json_wire_values::reads_as`]): a number reads into a numeric shape, a
/// string into a string-shaped one, an array into a list, an object into a map, a structure
/// or a union, anything into a document, and a null is absence rather than a type error.
///
/// The one difference is the BLOB, and it is the encoding's: JSON carries a blob as base64
/// text, so text reads into one there; CBOR carries it as a byte string, so a byte string
/// reads into one and text does not. A class this layer does not classify reads into
/// anything, because refusing on it would be refusing on a form it cannot name.
fn reads_into(value: &WireValue, target: ShapeType, wire: Wire) -> bool {
    match value {
        WireValue::Unclassified => true,
        WireValue::Bytes => matches!(target, ShapeType::Blob | ShapeType::Document),
        WireValue::Text(_) if wire == Wire::Cbor && matches!(target, ShapeType::Blob) => false,
        other => reads_as(target, &other.as_json()),
    }
}

/// The sentence a value that could not be read states — the value the request sent and the
/// type that value could not be converted to — or `None` when either half is one this layer
/// does not state ([`WireValue::quoted`], [`named_type`]).
///
/// The sentence itself is the tree's shared one
/// ([`service_common::json_wire_values::unreadable_message`]); what this service states is
/// its two halves, which are built FROM THE REQUEST and the model, so a recorded instance of
/// it is one row of the rule rather than the rule.
fn unreadable_sentence(value: &WireValue, target: ShapeType) -> Option<String> {
    let quoted = value.quoted()?;
    Some(unreadable_message(
        &format!("'{quoted}'"),
        named_type(target)?,
    ))
}

/// What this service calls the type a value could not be converted to, with the article that
/// goes in front of it.
///
/// These are the types the service's own API model binds its members to, named as that API
/// names them to a caller: a string member is read into a character sequence and a blob
/// member into a byte buffer, which is what the recorded refusals of those two members state
/// ("a CharSequence", "a ByteBuffer"), and the rest follow the same naming of the same API's
/// types. A target with no such name — a structure, a union, a document — is left to the
/// codec rather than given a name this service is not known to state.
fn named_type(target: ShapeType) -> Option<&'static str> {
    match target {
        // An enumerated type IS a string shape in these schemas, so it is read into the
        // same character sequence.
        ShapeType::String => Some("a CharSequence"),
        ShapeType::Blob => Some("a ByteBuffer"),
        ShapeType::Boolean => Some("a Boolean"),
        ShapeType::Byte => Some("a Byte"),
        ShapeType::Short => Some("a Short"),
        ShapeType::Integer => Some("an Integer"),
        ShapeType::Long => Some("a Long"),
        ShapeType::Float => Some("a Float"),
        ShapeType::Double => Some("a Double"),
        ShapeType::BigInteger => Some("a BigInteger"),
        ShapeType::BigDecimal => Some("a BigDecimal"),
        ShapeType::Timestamp => Some("a Date"),
        ShapeType::List => Some("a List"),
        ShapeType::Map => Some("a Map"),
        _ => None,
    }
}

/// The wire answer: `400` [`SERIALIZATION_EXCEPTION`] carrying `message` under
/// [`MESSAGE_MEMBER`], encoded by `protocol` — the protocol of the encoding the request
/// arrived in, which also writes the `__type` discriminator and the media type of that
/// encoding. `None` when the body cannot be encoded at all, which leaves the framework's own
/// rendering to answer.
fn serialization_exception(
    protocol: &dyn ServerHttpProtocol,
    message: &str,
) -> Option<RtResponse> {
    protocol
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

/// The `Message` member of the synthetic [`SERIALIZATION_EXCEPTION`] shape.
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
/// no operation lists it, because it is the PROTOCOL's own error rather than one of the
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

    /// A member of a test input shape, typed as `shape_type`.
    const fn member(name: &'static str, shape_type: ShapeType, index: usize) -> Schema {
        Schema::new_member(
            ShapeId::from_static("com.test#Input$member", "com.test", "Input"),
            shape_type,
            name,
            index,
        )
    }

    static STREAM_NAME: Schema = member("StreamName", ShapeType::String, 0);
    static DATA: Schema = member("Data", ShapeType::Blob, 1);
    static LIMIT: Schema = member("Limit", ShapeType::Integer, 2);
    static RECORDS: Schema = member("Records", ShapeType::List, 3);
    static INPUT_SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("com.test#Input", "com.test", "Input"),
        ShapeType::Structure,
        &[&STREAM_NAME, &DATA, &LIMIT, &RECORDS],
    );

    /// RULE: a blob member is read from the form its ENCODING carries a blob in — base64
    /// text in JSON, a byte string in CBOR — and from nothing else; every other class reads
    /// into the same targets in both encodings.
    #[test]
    fn a_blob_is_read_from_the_form_its_encoding_carries_it_in() {
        // (case, the value, the modelled target, the encoding, whether it reads)
        let rows: &[(&str, WireValue, ShapeType, Wire, bool)] = &[
            ("base64 text into a JSON blob", WireValue::Text("YQ==".into()), ShapeType::Blob, Wire::Json, true),
            ("text into a CBOR blob", WireValue::Text("notbytes".into()), ShapeType::Blob, Wire::Cbor, false),
            ("a byte string into a CBOR blob", WireValue::Bytes, ShapeType::Blob, Wire::Cbor, true),
            ("a byte string into a string member", WireValue::Bytes, ShapeType::String, Wire::Cbor, false),
            ("a byte string into a document", WireValue::Bytes, ShapeType::Document, Wire::Cbor, true),
            ("text into a string member", WireValue::Text("orders".into()), ShapeType::String, Wire::Cbor, true),
            ("a number into a string member", WireValue::Number(Number::from(12)), ShapeType::String, Wire::Cbor, false),
            ("a number into a string member, in JSON", WireValue::Number(Number::from(12)), ShapeType::String, Wire::Json, false),
            ("a number into an integer member", WireValue::Number(Number::from(12)), ShapeType::Integer, Wire::Cbor, true),
            ("a number past an integer's width", WireValue::Number(Number::from(i64::from(i32::MAX) + 1)), ShapeType::Integer, Wire::Json, false),
            ("a flag into a boolean member", WireValue::Flag(true), ShapeType::Boolean, Wire::Json, true),
            ("a flag into a string member", WireValue::Flag(true), ShapeType::String, Wire::Json, false),
            ("an array into a list member", WireValue::List, ShapeType::List, Wire::Cbor, true),
            ("an array into a string member", WireValue::List, ShapeType::String, Wire::Cbor, false),
            ("a map into a structure member", WireValue::Map, ShapeType::Structure, Wire::Cbor, true),
            ("a null into any member", WireValue::Absent, ShapeType::Integer, Wire::Cbor, true),
            ("a form this layer does not classify", WireValue::Unclassified, ShapeType::String, Wire::Cbor, true),
        ];
        for (case, value, target, wire, expected) in rows.iter().cloned() {
            let actual = reads_into(&value, target, wire);
            assert_eq!(
                actual, expected,
                "case {case:?} ({value:?} into {target:?} over {wire:?}): expected \
                 reads_into == {expected}, got {actual}"
            );
        }
    }

    /// RULE: the sentence quotes the value the request sent and names the type that value
    /// could not be converted to; a value with no text a caller wrote, or a target with no
    /// name this service states, yields no sentence at all.
    #[test]
    fn the_sentence_quotes_the_value_and_names_the_type() {
        // (case, the value, the modelled target, the sentence)
        let rows: &[(&str, WireValue, ShapeType, Option<&str>)] = &[
            (
                "text at a blob member",
                WireValue::Text("notbytes".into()),
                ShapeType::Blob,
                Some("'notbytes' can not be converted to a ByteBuffer"),
            ),
            (
                "a number at a string member",
                WireValue::Number(Number::from(12)),
                ShapeType::String,
                Some("'12' can not be converted to a CharSequence"),
            ),
            (
                "a flag at a string member",
                WireValue::Flag(true),
                ShapeType::String,
                Some("'true' can not be converted to a CharSequence"),
            ),
            (
                "text at an integer member",
                WireValue::Text("lots".into()),
                ShapeType::Integer,
                Some("'lots' can not be converted to an Integer"),
            ),
            (
                "text at a timestamp member",
                WireValue::Text("half past four".into()),
                ShapeType::Timestamp,
                Some("'half past four' can not be converted to a Date"),
            ),
            (
                "text at a list member",
                WireValue::Text("one".into()),
                ShapeType::List,
                Some("'one' can not be converted to a List"),
            ),
            ("a byte string, which has no text", WireValue::Bytes, ShapeType::String, None),
            ("a collection, which has no text", WireValue::List, ShapeType::String, None),
            (
                "a target this service does not name",
                WireValue::Text("one".into()),
                ShapeType::Structure,
                None,
            ),
        ];
        for (case, value, target, expected) in rows.iter().cloned() {
            let actual = unreadable_sentence(&value, target);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A quoted value is held to the ceiling one message carries, so no refusal is sized by
    /// the request.
    #[test]
    fn a_quoted_value_is_held_to_the_messages_ceiling() {
        let submitted = "x".repeat(64 * 1024);
        let quoted = WireValue::Text(submitted.clone())
            .quoted()
            .expect("text is quotable");
        assert!(
            quoted.len() < submitted.len(),
            "case an oversized value: expected the shared ceiling to hold it, got {} bytes \
             for a {}-byte value",
            quoted.len(),
            submitted.len()
        );
        let short = "orders";
        assert_eq!(
            WireValue::Text(short.into()).quoted().as_deref(),
            Some(short),
            "case a short value: expected it quoted whole"
        );
    }

    /// RULE over a whole body: the FIRST value the body carries for a member of the input
    /// shape that the member cannot be read from is the one refused, keys naming no member
    /// are ignored, and a body every member reads from is refused nothing.
    #[test]
    fn the_first_unreadable_value_of_a_json_body_is_the_one_refused() {
        // (case, the body, the sentence)
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a body that reads", r#"{"StreamName":"orders","Limit":2}"#, None),
            (
                "a number at a string member",
                r#"{"StreamName":12}"#,
                Some("'12' can not be converted to a CharSequence"),
            ),
            (
                "a key naming no member",
                r#"{"Nonesuch":12,"StreamName":"orders"}"#,
                None,
            ),
            (
                "text at an integer member",
                r#"{"StreamName":"orders","Limit":"two"}"#,
                Some("'two' can not be converted to an Integer"),
            ),
            // base64 text IS how JSON carries a blob, so this reads.
            ("base64 text at a blob member", r#"{"Data":"bm90"}"#, None),
            ("an array at a list member", r#"{"Records":[]}"#, None),
            ("a body that is not an object", r#"["orders"]"#, None),
            ("bytes that are not JSON at all", "not json", None),
        ];
        for &(case, body, expected) in rows {
            let actual = unreadable_value(&INPUT_SCHEMA, Wire::Json, body.as_bytes());
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?} ({body}): expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The same rule over a CBOR body, read with the CBOR implementation the framework's own
    /// codec is built on: the classes CBOR distinguishes include the byte string a blob is
    /// carried as, and a map written with its length reads like one written indefinitely.
    #[test]
    fn the_first_unreadable_value_of_a_cbor_body_is_the_one_refused() {
        // Each row builds its own body with the encoder, so the bytes are a real CBOR map.
        let definite = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.map(1).str("StreamName").long(12);
            encoder.into_writer()
        };
        let indefinite = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.begin_map();
            encoder.str("StreamName");
            encoder.long(12);
            encoder.end();
            encoder.into_writer()
        };
        let text_at_a_blob = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.begin_map();
            encoder.str("StreamName");
            encoder.str("orders");
            encoder.str("Data");
            encoder.str("notbytes");
            encoder.end();
            encoder.into_writer()
        };
        let bytes_at_a_blob = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.begin_map();
            encoder.str("Data");
            encoder.blob(&::aws_smithy_types::Blob::new(b"abc".to_vec()));
            encoder.end();
            encoder.into_writer()
        };
        let past_the_widest_signed_form = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.begin_map();
            encoder.str("Limit");
            let mut body = encoder.into_writer();
            // An unsigned integer whose argument travels in the eight following bytes (RFC 8949
            // section 3, major type 0, argument 27): a whole number no signed form this reader
            // has can hold, written where the encoder's own `long` cannot reach.
            body.push(0x1b);
            body.extend_from_slice(&u64::MAX.to_be_bytes());
            let mut tail = ::aws_smithy_cbor::Encoder::new(Vec::new());
            tail.str("StreamName");
            tail.long(12);
            tail.end();
            body.extend_from_slice(&tail.into_writer());
            body
        };
        let reads = {
            let mut encoder = ::aws_smithy_cbor::Encoder::new(Vec::new());
            encoder.begin_map();
            encoder.str("StreamName");
            encoder.str("orders");
            encoder.str("Limit");
            encoder.long(2);
            encoder.end();
            encoder.into_writer()
        };

        // (case, the body, the sentence)
        let rows: &[(&str, &[u8], Option<&str>)] = &[
            (
                "a number at a string member, length-prefixed",
                &definite,
                Some("'12' can not be converted to a CharSequence"),
            ),
            (
                "a number at a string member, indefinite",
                &indefinite,
                Some("'12' can not be converted to a CharSequence"),
            ),
            (
                "text at a blob member",
                &text_at_a_blob,
                Some("'notbytes' can not be converted to a ByteBuffer"),
            ),
            ("a byte string at a blob member", &bytes_at_a_blob, None),
            (
                // A value this reader cannot narrow does not end the walk: the member after it
                // is still judged.
                "a whole number past the widest signed form, before an unreadable member",
                &past_the_widest_signed_form,
                Some("'12' can not be converted to a CharSequence"),
            ),
            ("a body that reads", &reads, None),
            ("bytes that are not CBOR at all", b"not cbor", None),
        ];
        for (case, body, expected) in rows.iter().copied() {
            let actual = unreadable_value(&INPUT_SCHEMA, Wire::Cbor, body);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
