// SPDX-License-Identifier: CC0-1.0

//! Whether a document really is the file format the request declared it to be.
//!
//! The test-harness operations (`TestConversion`, `TestMapping`) are handed a document
//! inline together with the name of its format, and both have to refuse content that is
//! not that format before they do anything else with it. The check lives here, once, so
//! the two cannot disagree about what "valid JSON" or "well-formed XML" means.
//!
//! Declared as a submodule of [`crate::state`] because `src/lib.rs` is generated and
//! cannot take a new top-level `mod`.
//!
//! Both checks delegate to a maintained parser — `serde_json` for JSON, `quick-xml` for
//! XML — rather than to hand-written syntax rules, and report that parser's own message.
//! For JSON that message is the whole refusal ([`json_parse_diagnostic`]): the parser
//! states what the parse stopped on and where, and a service surfaces that diagnostic
//! rather than its own account of the member the document arrived in.
//!
//! Beside them sits [`to_document_text`], the JSON LAYOUT a generated mapping template is
//! written in: `CreateStarterMappingTemplate`'s `mappingTemplate` is modelled as a plain
//! string and carries a whole document as text, and that text is compared byte for byte by
//! the clients that read it back.

/// The `FileFormat` / `ConversionSourceFormat` value naming a JSON document.
pub const JSON: &str = "JSON";

/// The `FileFormat` / `ConversionSourceFormat` value naming an XML document.
pub const XML: &str = "XML";

/// The extra `FileFormat` value that names no document format at all
/// (`FileFormat` is `JSON | NOT_USED | XML`; `NOT_USED` exists for the deprecated
/// `CreateTransformer$fileFormat` member, which a transformer using `inputConversion`
/// leaves unset).
pub const NOT_USED: &str = "NOT_USED";

/// Parse `content` as a JSON document, answering the parser's own diagnostic of where the
/// parse stopped on failure ([`json_parse_diagnostic`]).
pub fn parse_json(content: &str) -> Result<::serde_json::Value, String> {
    ::serde_json::from_str(content).map_err(|failure| json_parse_diagnostic(content, &failure))
}

/// WHERE a JSON document the caller submitted stopped parsing, as the JSON parser states
/// it: what the parse stopped on, what belonged there instead, and the position — with the
/// document itself withheld.
///
/// This is the WHOLE sentence a request carrying an unparsable JSON document is refused
/// with: it names no input member, because the diagnostic is the parser's own rather than
/// this service's account of the member, and it reports the source as redacted rather than
/// echoing a document that may be sensitive back to the caller.
///
/// Every reading in it — the offending character and its character code, the word a parse
/// stopped on where a value belongs, the expectation at that position, and the line and
/// column — is the tree's shared one ([`::service_common::json_parse_fault`]), taken from
/// the parse that actually ran, so two services handed one document cannot disagree about
/// where it went wrong.
pub fn json_parse_diagnostic(content: &str, failure: &::serde_json::Error) -> String {
    use ::service_common::json_parse_fault as fault;

    // A document that stops on an unquoted word where a value belongs is reported by
    // naming that WORD, at the position the word starts; every other fault names the
    // character the parse stopped on, or the end of the input, at the position the parse
    // itself reports.
    let (diagnosis, line, column) = match fault::unrecognized_token(content, failure) {
        Some((word, line, column)) => (
            format!(
                "Unrecognized token '{word}': was expecting ({})",
                fault::VALUE_CLASSES
            ),
            line,
            column,
        ),
        None => {
            let (line, column) = fault::stopped_at(content, failure);
            (fault::unexpected_input(content, failure), line, column)
        }
    };
    format!(
        "{diagnosis} at {}",
        fault::location_clause(fault::REDACTED_SOURCE, line, column)
    )
}

/// Render `document` as the pretty-printed JSON TEXT a GENERATED mapping template is
/// recorded with, answering `serde_json`'s own message on failure.
///
/// `CreateStarterMappingTemplate`'s `mappingTemplate` is modelled as a plain `String` and
/// carries a whole JSON document as text, and the layout of that text is part of what a
/// caller reads back and compares, so it is written here, once, rather than inside the
/// generator:
///
/// * two spaces of indentation per level of nesting;
/// * a space on BOTH sides of the colon between a member's name and its value;
/// * an array on the line it opens on, its elements separated by a comma and a space.
///
/// `serde_json`'s own pretty layout — no space before the colon, every array element on a
/// line of its own — is what the MAPPED document of a `TestMapping` answer is recorded
/// with, so that operation writes its text with [`to_text_in_member_order`] and the two
/// layouts stay apart.
pub fn to_document_text<T: ::serde::Serialize + ?Sized>(document: &T) -> Result<String, String> {
    let mut text = Vec::new();
    let mut serializer =
        ::serde_json::Serializer::with_formatter(&mut text, DocumentFormatter::default());
    document
        .serialize(&mut serializer)
        .map_err(|failure| failure.to_string())?;
    String::from_utf8(text).map_err(|failure| failure.to_string())
}

/// The layout [`to_document_text`] writes, as a `serde_json` formatter.
///
/// `serde_json`'s own pretty formatter writes no space before the colon and puts every
/// element of an array on a line of its own, so the layout is spelled out here instead.
#[derive(Debug, Default)]
struct DocumentFormatter {
    /// How deeply the value being written nests.
    depth: usize,
    /// Whether the container being closed wrote a member.
    wrote_member: bool,
}

impl DocumentFormatter {
    /// Two spaces per level of nesting.
    fn indent<W: ?Sized + ::std::io::Write>(&self, writer: &mut W) -> ::std::io::Result<()> {
        for _ in 0..self.depth {
            writer.write_all(b"  ")?;
        }
        Ok(())
    }
}

impl ::serde_json::ser::Formatter for DocumentFormatter {
    fn begin_object<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> ::std::io::Result<()> {
        self.depth += 1;
        self.wrote_member = false;
        writer.write_all(b"{")
    }

    fn begin_object_key<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> ::std::io::Result<()> {
        writer.write_all(if first { b"\n" } else { b",\n" })?;
        self.indent(writer)
    }

    fn begin_object_value<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> ::std::io::Result<()> {
        writer.write_all(b" : ")
    }

    fn end_object_value<W: ?Sized + ::std::io::Write>(
        &mut self,
        _writer: &mut W,
    ) -> ::std::io::Result<()> {
        self.wrote_member = true;
        Ok(())
    }

    fn end_object<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> ::std::io::Result<()> {
        self.depth -= 1;
        if self.wrote_member {
            writer.write_all(b"\n")?;
            self.indent(writer)?;
        }
        self.wrote_member = true;
        writer.write_all(b"}")
    }

    fn begin_array<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> ::std::io::Result<()> {
        // An array stays on the line it opens on, so it adds no level of indentation to
        // the objects inside it.
        self.wrote_member = false;
        writer.write_all(b"[")
    }

    fn begin_array_value<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> ::std::io::Result<()> {
        writer.write_all(if first { b" " } else { b", " })
    }

    fn end_array_value<W: ?Sized + ::std::io::Write>(
        &mut self,
        _writer: &mut W,
    ) -> ::std::io::Result<()> {
        self.wrote_member = true;
        Ok(())
    }

    fn end_array<W: ?Sized + ::std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> ::std::io::Result<()> {
        if self.wrote_member {
            writer.write_all(b" ")?;
        }
        self.wrote_member = true;
        writer.write_all(b"]")
    }
}

/// The member names of every object in the JSON document `text`, in the order the document
/// WRITES them, depth first — each object's names in turn, a nested document's names after
/// the name it arrives under.
///
/// A mapped document's objects are answered in the order of the document they were built
/// from ([`to_text_in_member_order`]), and `serde_json::Value` holds an object's members
/// sorted, so the order has to be read off the parse itself: this records each member name
/// as the parser reaches it. A document that does not parse writes no member, and answers
/// an empty list.
///
/// What this holds is bounded by the document it reads: one name per object member, and the
/// caller of this function bounds that document's size before anything parses it
/// (`crate::ops::test_mapping`'s document ceiling). The walk recurses once per level of
/// nesting, which `serde_json`'s own recursion limit — the same one that refuses an
/// over-nested document on the parse before this one — is what bounds.
pub fn member_order(text: &str) -> Vec<String> {
    ::serde_json::from_str::<MemberOrder>(text)
        .map(|order| order.0)
        .unwrap_or_default()
}

/// The member names a JSON document writes, in document order, as a `serde` target.
struct MemberOrder(Vec<String>);

impl<'de> ::serde::Deserialize<'de> for MemberOrder {
    fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MemberOrderVisitor)
    }
}

/// The visitor behind [`MemberOrder`]: a container records the names it writes, in order,
/// and a scalar records none.
struct MemberOrderVisitor;

impl<'de> ::serde::de::Visitor<'de> for MemberOrderVisitor {
    type Value = MemberOrder;

    fn expecting(&self, formatter: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        formatter.write_str("a JSON document")
    }

    fn visit_map<A: ::serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> Result<Self::Value, A::Error> {
        let mut names = Vec::new();
        while let Some(name) = map.next_key::<String>()? {
            names.push(name);
            names.extend(map.next_value::<MemberOrder>()?.0);
        }
        Ok(MemberOrder(names))
    }

    fn visit_seq<A: ::serde::de::SeqAccess<'de>>(
        self,
        mut sequence: A,
    ) -> Result<Self::Value, A::Error> {
        let mut names = Vec::new();
        while let Some(nested) = sequence.next_element::<MemberOrder>()? {
            names.extend(nested.0);
        }
        Ok(MemberOrder(names))
    }

    fn visit_unit<E: ::serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }

    fn visit_bool<E: ::serde::de::Error>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }

    fn visit_i64<E: ::serde::de::Error>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }

    fn visit_u64<E: ::serde::de::Error>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }

    fn visit_f64<E: ::serde::de::Error>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }

    fn visit_str<E: ::serde::de::Error>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(MemberOrder(Vec::new()))
    }
}

/// `value` as the pretty-printed JSON TEXT a mapped document is carried as — two-space
/// indentation, a space AFTER each member's colon, one array element per line — with every
/// object's members in the order `order` writes them.
///
/// A member `order` does not name follows the ones it does, by name, so one request always
/// answers with one text: the interpreter holds an object's members in a hash map, whose own
/// iteration order varies between evaluations, so SOME order has to be imposed and this is
/// the one the document the mapping read was written in.
///
/// `serde_json::Value` would sort the members, so the ordered tree is serialized through
/// [`InMemberOrder`] instead.
pub fn to_text_in_member_order(
    value: ::serde_json::Value,
    order: &[String],
) -> Result<String, String> {
    let mut ranks: ::std::collections::HashMap<&str, usize> =
        ::std::collections::HashMap::with_capacity(order.len());
    for (rank, name) in order.iter().enumerate() {
        ranks.entry(name.as_str()).or_insert(rank);
    }
    ::serde_json::to_string_pretty(&in_member_order(value, &ranks)).map_err(|f| f.to_string())
}

/// A JSON document whose objects keep the member order they were given, rather than the
/// sorted order a `serde_json::Value` holds them in.
enum InMemberOrder {
    /// A value with no member of its own: a string, a number, a boolean or null.
    Scalar(::serde_json::Value),
    /// A list, each element ordered in turn.
    Array(Vec<InMemberOrder>),
    /// An object's members, in the order they are to be written.
    Object(Vec<(String, InMemberOrder)>),
}

impl ::serde::Serialize for InMemberOrder {
    fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            InMemberOrder::Scalar(value) => value.serialize(serializer),
            InMemberOrder::Array(items) => serializer.collect_seq(items.iter()),
            InMemberOrder::Object(members) => {
                serializer.collect_map(members.iter().map(|(name, value)| (name.as_str(), value)))
            }
        }
    }
}

/// `value` with every object's members ordered by the rank `ranks` gives their name, a
/// member it does not rank after the ones it does and ordered by name among themselves.
fn in_member_order(
    value: ::serde_json::Value,
    ranks: &::std::collections::HashMap<&str, usize>,
) -> InMemberOrder {
    match value {
        ::serde_json::Value::Array(items) => InMemberOrder::Array(
            items
                .into_iter()
                .map(|item| in_member_order(item, ranks))
                .collect(),
        ),
        ::serde_json::Value::Object(members) => {
            let mut ordered: Vec<(String, ::serde_json::Value)> = members.into_iter().collect();
            ordered.sort_by(|(left, _), (right, _)| {
                let rank = |name: &str| ranks.get(name).copied();
                match (rank(left), rank(right)) {
                    (Some(left_rank), Some(right_rank)) => left_rank.cmp(&right_rank),
                    (Some(_), None) => ::std::cmp::Ordering::Less,
                    (None, Some(_)) => ::std::cmp::Ordering::Greater,
                    (None, None) => left.cmp(right),
                }
            });
            InMemberOrder::Object(
                ordered
                    .into_iter()
                    .map(|(name, member)| (name, in_member_order(member, ranks)))
                    .collect(),
            )
        }
        scalar => InMemberOrder::Scalar(scalar),
    }
}

/// Check that `content` is a well-formed XML document: a prolog, exactly ONE root
/// element whose tags are balanced and correctly nested, and nothing but whitespace,
/// comments and processing instructions around it (XML 1.0 §2.1, `document ::= prolog
/// element Misc*`).
///
/// The balance and nesting come from `quick-xml`'s own `check_end_names`; this walk adds
/// the two conditions an event reader does not raise by itself, so that a document with
/// no element at all (`""`) or a bare string (`not xml`) is not mistaken for XML.
pub fn check_xml(content: &str) -> Result<(), String> {
    use ::quick_xml::events::Event;

    let mut reader = ::quick_xml::Reader::from_str(content);
    reader.config_mut().check_end_names = true;

    let mut depth: usize = 0;
    let mut roots: usize = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(_)) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Empty(_)) => {
                if depth == 0 {
                    roots += 1;
                }
            }
            Ok(Event::Text(text)) => {
                // The raw bytes are enough here: the only question is whether anything
                // other than whitespace sits OUTSIDE the root element.
                let raw = String::from_utf8_lossy(text.as_ref());
                if depth == 0 && !raw.trim().is_empty() {
                    return Err(format!(
                        "character data outside the root element: {:?}",
                        raw.trim()
                    ));
                }
            }
            // A CDATA section is character data too, and XML allows none around the root
            // element: handled beside `Text` so the two cannot be classified differently.
            Ok(Event::CData(section)) => {
                if depth == 0 {
                    return Err(format!(
                        "character data outside the root element: {:?}",
                        String::from_utf8_lossy(section.as_ref())
                    ));
                }
            }
            Ok(_) => {}
            Err(failure) => return Err(failure.to_string()),
        }
    }

    match roots {
        1 if depth == 0 => Ok(()),
        // Reaching the end of the document with elements still open: `check_end_names`
        // only catches a MISMATCHED end tag, not a missing one.
        1 => Err(format!(
            "the document ends with {depth} element(s) still open"
        )),
        0 => Err("the document has no root element".to_string()),
        found => Err(format!(
            "the document has {found} root elements, and XML allows exactly one"
        )),
    }
}
