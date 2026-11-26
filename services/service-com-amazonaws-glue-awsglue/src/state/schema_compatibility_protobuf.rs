// SPDX-License-Identifier: CC0-1.0

//! Whether a reader using one `PROTOBUF` schema document can read data written with another
//! — the ONE comparison every `Compatibility` mode of a `PROTOBUF` schema set is built from
//! ([`super::schema_compatibility`]).
//!
//! A `PROTOBUF` `SchemaDefinition` is the source text of a `.proto` file, read here into the
//! descriptor the protobuf parser compiles it to ([`super::unreadable_definition_protobuf`]
//! answers whether it is one at all). The question a mode asks of two of them is whether a
//! reader built from one parses every message written with the other, which the protobuf
//! language answers through its own field rules:
//!
//! * a field the reader declares `required` must be present in the bytes, and the writer
//!   writes it only if the writer declares it `required` as well — so a field the reader
//!   requires and the writer does not is a field a message of the writer can arrive without,
//!   and the reader's parse of that message fails. That is one rule for both directions:
//!   under `BACKWARD` the registered definition is the reader, so ADDING a required field is
//!   incompatible; under `FORWARD` the version held is the reader, so REMOVING one is.
//! * a field number declared by both must carry the same wire type: the bytes a writer emits
//!   for a field are read against the type the reader declares for that number, so a changed
//!   type is a changed reading of the same bytes.
//!
//! A field the WRITER declares and the reader does not is not a failure: protobuf readers
//! carry unknown fields rather than rejecting them.
//!
//! `optional` and `repeated` fields are never required of the bytes, so adding or removing
//! one changes nothing about whether a message parses — which is exactly the difference the
//! registry's recorded verdicts draw between an `optional` and a `required` field.

/// The descriptor-proto `label` of a field the bytes must carry (`LABEL_REQUIRED`).
///
/// Source: the protobuf descriptor's own `FieldDescriptorProto.Label` numbering, which is
/// wire-format contract: `LABEL_OPTIONAL = 1`, `LABEL_REQUIRED = 2`, `LABEL_REPEATED = 3`.
/// Named by its number because the comparison reads the parsed descriptor's raw fields and so
/// depends on no type name of the parser's own crate.
const LABEL_REQUIRED: i32 = 2;

/// How many declarations deep the descriptor walk below descends.
///
/// The walk is iterative — it holds a work list rather than recursing — so this bounds what
/// one comparison READS rather than the stack it uses: the same nesting bound the registry
/// applies before a definition is parsed at all
/// ([`super::MAX_PROTOBUF_NESTING_DEPTH`]), so a definition this comparison reads has
/// already been held to it.
const MAX_MESSAGE_NESTING: usize = super::MAX_PROTOBUF_NESTING_DEPTH;

/// One field of one message, as a comparison reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtobufField {
    /// The field's name, which a difference names it by.
    name: String,
    /// Whether the bytes must carry it: the field's `label` is [`LABEL_REQUIRED`].
    required: bool,
    /// The field's declared type, as the descriptor's `type` number states it, beside the
    /// message or enum name it names for a type that has one — the pair that decides how the
    /// bytes for this field number are read.
    reading: (i32, String),
}

/// A `PROTOBUF` `SchemaDefinition` as a comparison reads it: every message the document
/// declares, nested ones included, keyed by the name the document gives it, each carrying its
/// fields keyed by FIELD NUMBER — which is what a reader and a writer agree on, names being
/// the source's own.
#[derive(Debug, Clone)]
pub struct ProtobufDefinition {
    /// The messages, keyed by their qualified name within the document.
    messages: ::std::collections::BTreeMap<String, ::std::collections::BTreeMap<i32, ProtobufField>>,
}

/// `definition` read into the messages and fields a comparison asks about, or `None` when
/// the protobuf parser does not read the text as a document at all — the same parse
/// [`super::unreadable_definition_protobuf`] answers a registration's refusal from, so a
/// definition that is no document is never compared.
#[must_use]
pub fn protobuf_definition(definition: &str) -> Option<ProtobufDefinition> {
    if super::unreadable_definition_protobuf(definition).is_some() {
        return None;
    }
    let file = ::protox_parse::parse("", definition).ok()?;
    let mut messages = ::std::collections::BTreeMap::new();
    // One flat work list of (qualified name, message), so a document nesting messages is
    // walked without this walk recursing into it.
    let mut pending: Vec<_> = file
        .message_type
        .iter()
        .map(|message| (qualified(file.package.as_deref(), message.name()), message))
        .collect();
    let mut read = 0usize;
    while let Some((name, message)) = pending.pop() {
        // The document's own nesting is bounded before it is parsed; this counts what the
        // walk reads, so a document declaring more messages than one comparison will read is
        // read no further.
        read += 1;
        if read > MAX_MESSAGE_NESTING * MAX_MESSAGE_NESTING {
            break;
        }
        let fields = message
            .field
            .iter()
            .map(|field| {
                (
                    field.number(),
                    ProtobufField {
                        name: field.name().to_string(),
                        required: field.label.unwrap_or_default() == LABEL_REQUIRED,
                        reading: (
                            field.r#type.unwrap_or_default(),
                            field.type_name().to_string(),
                        ),
                    },
                )
            })
            .collect();
        messages.insert(name.clone(), fields);
        for nested in &message.nested_type {
            pending.push((qualified(Some(&name), nested.name()), nested));
        }
    }
    Some(ProtobufDefinition { messages })
}

/// The name `name` carries under `parent`: the document's package or the enclosing message,
/// or the name alone for a top-level message of a document declaring no package.
fn qualified(parent: Option<&str>, name: &str) -> String {
    match parent.filter(|parent| !parent.is_empty()) {
        Some(parent) => format!("{parent}.{name}"),
        None => name.to_string(),
    }
}

/// Why a reader using `reader` cannot parse every message written with `writer`, or `None`
/// when it can.
///
/// Reports the FIRST difference found, over the messages both documents declare: a message
/// the reader declares and the writer does not is never written, so there is nothing about it
/// to decide.
#[must_use]
pub fn protobuf_reader_cannot_read(
    writer: &ProtobufDefinition,
    reader: &ProtobufDefinition,
) -> Option<String> {
    for (message, read_fields) in &reader.messages {
        let Some(written_fields) = writer.messages.get(message) else {
            continue;
        };
        for (number, read) in read_fields {
            match written_fields.get(number) {
                Some(written) => {
                    if read.required && !written.required {
                        return Some(format!(
                            "the reader requires field {number} ({}) of message {message}, \
                             which the writer does not",
                            read.name
                        ));
                    }
                    if read.reading != written.reading {
                        return Some(format!(
                            "field {number} ({}) of message {message} is declared with a \
                             different type by the reader and the writer",
                            read.name
                        ));
                    }
                }
                None if read.required => {
                    return Some(format!(
                        "the reader requires field {number} ({}) of message {message}, which \
                         the writer does not declare",
                        read.name
                    ))
                }
                None => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A proto2 document declaring `fields` on one message.
    fn proto2(fields: &[&str]) -> String {
        format!(
            "syntax = \"proto2\";\npackage com.example;\nmessage User {{\n{}\n}}\n",
            fields.join("\n")
        )
    }

    /// RULE: a reader parses every message a writer emits exactly when every field the reader
    /// REQUIRES is required by the writer too, and every field number they share is declared
    /// with the same type — so adding or dropping an `optional` field changes nothing while
    /// adding or dropping a `required` one does.
    ///
    /// The rows differ along what the rule generalises over: which side declares the extra
    /// field, the field's label (`required`, `optional`, `repeated`), the direction the
    /// comparison runs in, a field's type changed, and a message the other side does not
    /// declare at all.
    #[test]
    fn a_protobuf_reader_parses_a_writer_exactly_when_every_required_field_is_written() {
        let one = proto2(&["  required string id = 1;"]);
        let add_required = proto2(&[
            "  required string id = 1;",
            "  required string email = 2;",
        ]);
        let add_optional = proto2(&[
            "  required string id = 1;",
            "  optional string email = 2;",
        ]);
        let add_repeated = proto2(&[
            "  required string id = 1;",
            "  repeated string tags = 2;",
        ]);
        let retyped = proto2(&["  required int64 id = 1;"]);
        let other_message =
            "syntax = \"proto2\";\npackage com.example;\nmessage Order { required string sku = 1; }"
                .to_string();

        // (case, the writer, the reader, whether the reader parses the writer's messages)
        let rows: &[(&str, &String, &String, bool)] = &[
            ("the same document", &one, &one, true),
            ("a required field added", &one, &add_required, false),
            ("a required field dropped", &add_required, &one, true),
            ("an optional field added", &one, &add_optional, true),
            ("an optional field dropped", &add_optional, &one, true),
            ("a repeated field added", &one, &add_repeated, true),
            ("a repeated field dropped", &add_repeated, &one, true),
            (
                "an optional field made required",
                &add_optional,
                &add_required,
                false,
            ),
            (
                "a required field made optional",
                &add_required,
                &add_optional,
                true,
            ),
            ("a field's type changed", &one, &retyped, false),
            (
                "a message the writer does not declare",
                &one,
                &other_message,
                true,
            ),
        ];

        for (case, writer, reader, readable) in rows {
            let writer = protobuf_definition(writer)
                .unwrap_or_else(|| panic!("case {case:?}: the writer must be a document"));
            let reader = protobuf_definition(reader)
                .unwrap_or_else(|| panic!("case {case:?}: the reader must be a document"));
            let difference = protobuf_reader_cannot_read(&writer, &reader);
            assert_eq!(
                difference.is_none(),
                *readable,
                "case {case:?}: expected readable={readable}, got {difference:?}"
            );
        }
    }

    /// RULE: a definition the protobuf parser does not read as a document is no document to
    /// compare, and a NESTED message is read with the same rule as a top-level one.
    #[test]
    fn a_nested_message_is_read_and_text_that_is_no_document_is_not() {
        for case in ["not a protobuf document", "syntax = \"proto2\"; message {"] {
            assert!(
                protobuf_definition(case).is_none(),
                "case {case:?}: text that is no document must not be read as one"
            );
        }

        let writer = protobuf_definition(
            "syntax = \"proto2\"; message Order { message Line { required string sku = 1; } \
             optional Line line = 1; }",
        )
        .expect("the writer must be a document");
        let reader = protobuf_definition(
            "syntax = \"proto2\"; message Order { message Line { required string sku = 1; \
             required int64 quantity = 2; } optional Line line = 1; }",
        )
        .expect("the reader must be a document");
        let difference = protobuf_reader_cannot_read(&writer, &reader);
        assert!(
            difference
                .as_deref()
                .is_some_and(|difference| difference.contains("quantity")),
            "case \"a required field added to a nested message\": expected the nested field to \
             be reported, got {difference:?}"
        );
        assert!(
            protobuf_reader_cannot_read(&reader, &writer).is_none(),
            "case \"a required field dropped from a nested message\": the reader requires \
             nothing the writer withholds"
        );
    }
}
