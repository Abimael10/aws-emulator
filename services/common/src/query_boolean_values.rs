// SPDX-License-Identifier: CC0-1.0

//! How a BOOLEAN member of an AWS Query (`awsQuery`) request is read, for every service
//! whose front door is that protocol.
//!
//! A boolean member of such a request is carried as the TEXT of a form field, and the values
//! that text may hold are the lexical space of an XML Schema boolean: `true`, `false`, `1`
//! and `0` (W3C XML Schema Part 2, §3.2.2), the two numerals spelling the same values the
//! two words do, read after the `whiteSpace = collapse` facet that type carries has stripped
//! the whitespace around the value. The framework's own form reader is vendored and reads the
//! two WORDS only, verbatim, so two things about it diverge from the wire:
//!
//! * a request spelling a boolean the numeric way is refused, where the protocol admits it;
//! * text in no spelling of the space is refused with the READER's report — a sentence about
//!   the literals a parser accepts, which names the value the request carried and the set
//!   that parser takes. The protocol layer states that refusal as one fixed sentence
//!   ([`NON_BOOLEAN_VALUE_MESSAGE`]) instead, with no member name, no value and no
//!   accepted-literal list in it, under the error code the framework already answers a body
//!   it could not read with.
//!
//! Both are facts of the PROTOCOL rather than of any operation or any one service: every
//! boolean member of every operation of such a front door is read the same way, and the
//! refusal happens while the request is read, ahead of the handler. So the rule lives here,
//! in the crate the services share, and each of them reaches it the same way — by serving
//! [`QueryBooleanValues`] from `Service::override_protocol`, which is the framework's own
//! `awsQuery` protocol with this one reading replaced.
//!
//! [`reading_boolean_values`] is the reader itself, for a service that assembles its own
//! protocol and wants only this reading of it. It wraps the reader the framework positioned
//! at the request — through [`InnerDeserializer`], this crate's one seam for a wrapper that
//! replaces ONE read of a codec — so a boolean at ANY depth — inside a structure, inside an
//! element of a list of structures — is read by the rule: a member schema does not carry the
//! shape it targets, so only the walk itself knows which text is a boolean's, and scanning
//! the form keys instead could not find the nested ones.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::{ProtocolError, QueryServerProtocol, ServerProtocol};
use aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};

use crate::date_time_json::InnerDeserializer;

/// The sentence the protocol layer refuses a boolean member's text that spells no boolean
/// with.
///
/// Entirely fixed text: it names neither the member nor the value the request supplied, so
/// there is nothing to interpolate and no set of accepted literals is advertised to the
/// caller — in particular not one narrower than the lexical space.
pub const NON_BOOLEAN_VALUE_MESSAGE: &str = "boolean must follow xsd1.1 definition";

/// The spellings of `true` in the lexical space of an XML Schema boolean (W3C XML Schema
/// Part 2, §3.2.2).
const TRUE_SPELLINGS: [&str; 2] = ["true", "1"];

/// The spellings of `false` in that same lexical space.
const FALSE_SPELLINGS: [&str; 2] = ["false", "0"];

/// The whitespace the `whiteSpace = collapse` facet strips from around a value: the four
/// characters XML counts as whitespace (space, tab, carriage return, line feed; W3C XML
/// Schema Part 2, §4.3.6), and no other code point — a value padded with a non-breaking
/// space is text the space does not hold.
const XML_WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];

/// The boolean `text` spells, or `None` when it spells none.
///
/// The whole lexical space and nothing beside it: the numerals read as the values they stand
/// for, and text in no spelling of the space is no boolean at all — whatever a reader of
/// Rust literals would make of it. The whitespace AROUND the value is stripped first, which
/// is the `whiteSpace = collapse` facet the type carries (W3C XML Schema Part 2, §3.2.2 and
/// §4.3.6) and only the characters that facet counts ([`XML_WHITESPACE`]); whitespace INSIDE
/// a value is not stripped, so a space in the middle of a word spells no boolean either.
pub fn boolean_spelled(text: &str) -> Option<bool> {
    let text = text.trim_matches(|character| XML_WHITESPACE.contains(&character));
    if TRUE_SPELLINGS.contains(&text) {
        return Some(true);
    }
    if FALSE_SPELLINGS.contains(&text) {
        return Some(false);
    }
    None
}

// =============================================================================
// Reading a boolean's text
// =============================================================================

/// The reader this module wraps around the framework's: every read is the framework's,
/// except a boolean's, and every child it hands a container's walk is wrapped the same way.
///
/// Generic over HOW the inner reader is held ([`InnerDeserializer`], this crate's one seam
/// for exactly this wrapper shape): the protocol hands back an owned box, while each nested
/// walk hands over a borrow, and both wear this one wrapper, so the forwarding body is
/// written once.
struct BooleanTextReader<D>(D);

impl<D: InnerDeserializer> ShapeDeserializer for BooleanTextReader<D> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        // Every member the framework's walk visits is visited through this reader too, so a
        // boolean nested inside a structure — at any depth — is read by the rule below.
        self.0.get().read_struct(schema, &mut |member, child| {
            consumer(member, &mut BooleanTextReader(child))
        })
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0
            .get()
            .read_list(schema, &mut |element| consumer(&mut BooleanTextReader(element)))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.0.get().read_map(schema, &mut |key, value| {
            consumer(key, &mut BooleanTextReader(value))
        })
    }

    /// The one read this reader decides itself: the boolean the request's text spells.
    ///
    /// The text is read as the text it is and judged against the lexical space
    /// ([`boolean_spelled`]), so the numeric spellings are the booleans they stand for rather
    /// than values the wire does not admit; text in no spelling of the space is refused with
    /// the protocol's own sentence ([`NON_BOOLEAN_VALUE_MESSAGE`]).
    ///
    /// A position carrying no text at all is left to the framework's own reader, whose report
    /// of an absent value is unchanged by any of this. That is what the fallback below is:
    /// on this protocol a value IS a form field's text, so reading it as a STRING is the one
    /// read that cannot fail for a value that is there — a failure means the position carries
    /// no scalar — and the reader is POSITIONAL (a read neither consumes nor advances it), so
    /// asking the same position for a boolean afterwards is the same read, answered by the
    /// reader that owns the diagnostic for an absent value.
    fn read_boolean(&mut self, schema: &Schema) -> Result<bool, SerdeError> {
        let Ok(text) = self.0.get().read_string(schema) else {
            return self.0.get().read_boolean(schema);
        };
        boolean_spelled(&text).ok_or_else(|| SerdeError::custom(NON_BOOLEAN_VALUE_MESSAGE))
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

    /// Delegated whole, as every read but a boolean's is. The Query protocols have no wire
    /// representation for an open document and this reader is served on them alone, so no
    /// boolean can reach a handler through this read; a protocol that did carry one would have
    /// to walk the document's own leaves here.
    fn read_document(&mut self, schema: &Schema) -> Result<Document, SerdeError> {
        self.0.get().read_document(schema)
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.0.get().read_null()
    }

    fn is_null(&self) -> bool {
        self.0.peek().is_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.0.peek().container_size()
    }
}

/// `inner`, reading a boolean member's text as the Query wire carries one.
///
/// Everything else is `inner`'s own reading, byte for byte; what changes is the lexical space
/// a boolean is read from and the sentence text outside it is refused with.
pub fn reading_boolean_values<'a>(
    inner: Box<dyn ShapeDeserializer + 'a>,
) -> Box<dyn ShapeDeserializer + 'a> {
    Box::new(BooleanTextReader(inner))
}

// =============================================================================
// The protocol
// =============================================================================

/// The framework's own `awsQuery` protocol, reading a boolean member's text as the Query wire
/// carries one ([`reading_boolean_values`]).
///
/// Routing, the response envelope, the error envelope and the model's XML namespace are the
/// framework's unchanged: a service serves this from `Service::override_protocol` and nothing
/// else about its wire changes.
#[derive(Debug)]
pub struct QueryBooleanValues {
    inner: QueryServerProtocol,
}

impl QueryBooleanValues {
    /// Build the protocol once, as a generated service entry builds its own: `awsQuery` with
    /// the XML namespace the service's model declares (`@xmlNamespace`), which is part of the
    /// envelope and so must be the same one.
    pub fn aws_query(xml_namespace: &'static str) -> Self {
        Self {
            inner: QueryServerProtocol::aws_query().with_namespace(xml_namespace),
        }
    }
}

impl ServerProtocol for QueryBooleanValues {
    type Request = RtRequest;
    type Response = RtResponse;

    fn protocol_id(&self) -> &ShapeId {
        self.inner.protocol_id()
    }

    fn routing_key<'a>(&self, request: &'a RtRequest) -> Result<&'a str, ProtocolError> {
        self.inner.routing_key(request)
    }

    fn deserialize_input<'a>(
        &self,
        request: &'a RtRequest,
        input_schema: &Schema,
    ) -> Result<Box<dyn ShapeDeserializer + 'a>, ProtocolError> {
        Ok(reading_boolean_values(
            self.inner.deserialize_input(request, input_schema)?,
        ))
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<RtResponse, ProtocolError> {
        self.inner
            .serialize_output(output, output_schema, operation_name, success_status)
    }

    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<RtResponse, ProtocolError> {
        self.inner
            .serialize_error(error, error_schema, error_code, http_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_schema::ShapeType;
    use aws_smithy_schema_server::QueryShapeDeserializer;

    /// A boolean member of a request shape, as a generated input carries one.
    static FLAG: Schema = Schema::new_member(
        ShapeId::from_static(
            "smithy.example#Input$Flag",
            "smithy.example",
            "Flag",
        ),
        ShapeType::Boolean,
        "Flag",
        0,
    );

    /// The request shape that member belongs to.
    static INPUT: Schema = Schema::new_struct(
        ShapeId::from_static("smithy.example#Input", "smithy.example", "Input"),
        ShapeType::Structure,
        &[&FLAG],
    );

    /// The boolean a form body's `Flag` field is read as, through the wrapped reader.
    fn flag_of(body: &[u8]) -> Result<bool, SerdeError> {
        let mut reader = reading_boolean_values(Box::new(QueryShapeDeserializer::new(body, false)));
        let mut read: Option<Result<bool, SerdeError>> = None;
        reader
            .read_struct(&INPUT, &mut |member, child| {
                read = Some(child.read_boolean(member));
                Ok(())
            })
            .expect("the walk itself cannot fail");
        read.expect("the body carries the member")
    }

    /// The lexical space, and nothing beside it.
    #[test]
    fn the_lexical_space_is_the_four_spellings() {
        for (case, text, expected) in [
            ("the word true", "true", Some(true)),
            ("the word false", "false", Some(false)),
            ("the numeral one", "1", Some(true)),
            ("the numeral zero", "0", Some(false)),
            ("a word of assent", "yes", None),
            ("a word of refusal", "no", None),
            ("the word in upper case", "TRUE", None),
            ("the word in title case", "False", None),
            ("a number outside the space", "2", None),
            ("a negative number", "-1", None),
            ("a padded numeral", "01", None),
            ("a decimal", "1.0", None),
            ("the empty value", "", None),
            // The `whiteSpace = collapse` facet: the whitespace AROUND a value is not part of
            // it, and the whitespace INSIDE one leaves nothing the space holds.
            ("a word with spaces around it", " true ", Some(true)),
            ("a numeral with a tab and a newline around it", "\t0\n", Some(false)),
            ("whitespace alone", "   ", None),
            ("a word with a space inside it", "tr ue", None),
            // Only the four characters XML counts as whitespace are stripped: a value padded
            // with another code point the Unicode property covers is text the space does not
            // hold.
            ("a word padded with a non-breaking space", "\u{a0}true", None),
            ("a numeral padded with a line separator", "0\u{2028}", None),
        ] {
            assert_eq!(
                boolean_spelled(text),
                expected,
                "case {case:?}: expected {expected:?} for {text:?}, got {:?}",
                boolean_spelled(text)
            );
        }
    }

    /// Every spelling of the space is READ through the wrapped reader, over a real form body.
    #[test]
    fn a_form_value_in_the_space_reads_as_the_boolean_it_spells() {
        for (case, body, expected) in [
            ("the word true", "Flag=true", true),
            ("the word false", "Flag=false", false),
            ("the numeral one", "Flag=1", true),
            ("the numeral zero", "Flag=0", false),
            ("a word the facet strips the padding of", "Flag=%20true%20", true),
            ("a numeral the facet strips the padding of", "Flag=+0+", false),
        ] {
            let read = flag_of(body.as_bytes()).expect("a spelling of the space must be read");
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }

    /// Text in no spelling of the space is refused with the one fixed sentence — not with a
    /// report naming the value or the literals a parser takes.
    #[test]
    fn a_form_value_outside_the_space_is_refused_with_the_fixed_sentence() {
        for (case, body) in [
            ("a word of assent", "Flag=yes"),
            ("the word in upper case", "Flag=TRUE"),
            ("a number outside the space", "Flag=2"),
            ("the empty value", "Flag="),
            ("whitespace alone", "Flag=+"),
            ("a word with a space inside it", "Flag=tr+ue"),
        ] {
            let refusal = flag_of(body.as_bytes()).expect_err("text outside the space is refused");
            let stated = refusal.to_string();
            assert!(
                stated.contains(NON_BOOLEAN_VALUE_MESSAGE),
                "case {case:?}: expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {stated:?}"
            );
            assert!(
                !stated.contains("Flag"),
                "case {case:?}: the sentence must not name the member, got {stated:?}"
            );
        }
    }
}
