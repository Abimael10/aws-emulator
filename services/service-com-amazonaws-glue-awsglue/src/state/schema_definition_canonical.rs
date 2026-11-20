// SPDX-License-Identifier: CC0-1.0

//! The CANONICAL form of a `SchemaDefinition`: what makes two definitions the same schema
//! document even when their text differs.
//!
//! The schema registry does not compare submitted text: "the schema definition is sent to
//! the Schema Registry, canonicalized, and hashed" (`GetSchemaByDefinition`), which is what
//! lets "the schema ID of the existing schema [be] returned to the caller" when "the same
//! schema definition is already stored in Schema Registry as a version"
//! (`RegisterSchemaVersion`) for a definition that was re-spaced, re-indented or
//! re-commented on its way back.
//!
//! What is dropped is the LAYOUT of the document and nothing else, and which parts of a
//! document are layout is the data format's own question, so each format's canonical form is
//! produced by that format's own parser:
//!
//! * `AVRO` — Avro's own Parsing Canonical Form, as `apache-avro` renders it.
//! * `JSON` — the document re-rendered from its parse, so whitespace between tokens is
//!   dropped and the members are rendered in one order.
//! * `PROTOBUF` — the parsed descriptor of the `.proto` source rendered structurally, with
//!   the source positions of its declarations dropped
//!   ([`super::canonical_definition_protobuf`]).
//!
//! A definition the format's parser does not read has NO canonical form: it is no document
//! of the format, so the only thing it can be the same as is text identical to it.

use super::*;

/// Ceiling on how much stored schema text ONE lookup by definition will canonicalize.
///
/// A lookup compares the submitted definition against the versions a schema set holds, each
/// of which the modelled `SchemaDefinitionString` admits 170000 characters of, and a set
/// holds up to [`MAX_VERSIONS_PER_SCHEMA`] of them — so what one request COSTS is the
/// product of the two, which is what is capped here, exactly as a compatibility check's
/// parsing is ([`MAX_COMPATIBILITY_DEFINITION_BYTES`]). A lookup whose budget runs out
/// compares the versions it did not reach by their text alone, which is the stricter
/// comparison: it can miss a re-spacing, never report a version of another document.
pub const MAX_CANONICAL_DEFINITION_BYTES: usize = 1 << 20;

/// The canonical form of `definition` as a document of `data_format`, or `None` when that
/// format's parser does not read it as one — or when the format's documents are not parsed
/// here, so no canonical form is claimed for text this emulator cannot read.
#[must_use]
pub fn canonical_schema_definition(data_format: &str, definition: &str) -> Option<String> {
    match data_format {
        DATA_FORMAT_AVRO => ::apache_avro::Schema::parse_str(definition)
            .ok()
            .map(|schema| schema.canonical_form()),
        // A JSON Schema document is JSON, and what a document of that format SAYS about a
        // schema is not read here, so its canonical form is its parse rendered back: the
        // whitespace between its tokens is dropped and its members are rendered in one
        // order, because the order an object's members are written in is not part of what a
        // JSON document says.
        DATA_FORMAT_JSON => ::serde_json::from_str::<::serde_json::Value>(definition)
            .ok()
            .map(|document| members_in_one_order(document).to_string()),
        DATA_FORMAT_PROTOBUF => super::canonical_definition_protobuf(definition),
        _ => None,
    }
}

/// `document` with the members of every object it holds, at every depth, in ONE order:
/// lexicographic by member name, so two documents that differ only in the order their
/// members were written in render the same text.
///
/// The depth this walks is the depth the JSON parse admitted, which that parser bounds with
/// its own recursion limit — so a document a caller submits cannot choose how deep this
/// recursion goes.
fn members_in_one_order(document: ::serde_json::Value) -> ::serde_json::Value {
    match document {
        ::serde_json::Value::Object(members) => {
            let mut ordered: Vec<(String, ::serde_json::Value)> = members.into_iter().collect();
            ordered.sort_by(|(one, _), (other, _)| one.cmp(other));
            ::serde_json::Value::Object(
                ordered
                    .into_iter()
                    .map(|(name, value)| (name, members_in_one_order(value)))
                    .collect(),
            )
        }
        // A list's order IS what the document says, so it is kept as written.
        ::serde_json::Value::Array(items) => ::serde_json::Value::Array(
            items.into_iter().map(members_in_one_order).collect(),
        ),
        scalar => scalar,
    }
}

impl SchemaEntry {
    /// The version of this schema set holding the same schema document as `definition`, or
    /// `None` when it holds no version of it.
    ///
    /// Two definitions are the same document when their canonical forms agree
    /// ([`canonical_schema_definition`]), so a re-spaced or re-commented copy of a version's
    /// definition resolves to that version. Text identical to a stored definition is the
    /// same document with no parse at all, and a definition with no canonical form — one
    /// this format's parser does not read — can only be matched by such text.
    pub fn version_with_definition(&self, definition: &str) -> Option<&SchemaVersionEntry> {
        if let Some(held) = self
            .versions
            .iter()
            .find(|version| version.definition == definition)
        {
            return Some(held);
        }
        let canonical = canonical_schema_definition(&self.data_format, definition)?;
        // The submitted definition is parsed once above, so it is charged to the budget the
        // comparisons spend.
        let mut budget = MAX_CANONICAL_DEFINITION_BYTES.saturating_sub(definition.len());
        for version in &self.versions {
            if version.definition.len() > budget {
                break;
            }
            budget -= version.definition.len();
            if canonical_schema_definition(&self.data_format, &version.definition)
                .is_some_and(|held| held == canonical)
            {
                return Some(version);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: two definitions have the same canonical form exactly when they are the same
    /// schema document written out differently, and a definition the format's parser does
    /// not read has no canonical form at all.
    ///
    /// The rows differ along what the rule generalises over: the `DataFormat`, which part of
    /// the layout differs (whitespace between tokens, indentation, line breaks, a comment,
    /// member order), and whether the two documents really are the same one.
    #[test]
    fn the_canonical_form_drops_the_layout_of_a_document_and_nothing_else() {
        // (case, format, one definition, the other, whether they are the same document)
        let cases: &[(&str, &str, &str, &str, bool)] = &[
            (
                "an Avro record re-spaced",
                DATA_FORMAT_AVRO,
                r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"}]}"#,
                "{ \"type\" : \"record\" ,\n  \"name\" : \"User\" ,\n  \"fields\" : [ { \
                 \"name\" : \"id\" , \"type\" : \"string\" } ] }",
                true,
            ),
            (
                "an Avro record whose fields differ",
                DATA_FORMAT_AVRO,
                r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"}]}"#,
                r#"{"type":"record","name":"User","fields":[{"name":"id","type":"long"}]}"#,
                false,
            ),
            (
                "a JSON Schema document re-spaced and re-ordered",
                DATA_FORMAT_JSON,
                r#"{"type":"object","properties":{"id":{"type":"string"}}}"#,
                "{\n  \"properties\" : { \"id\" : { \"type\" : \"string\" } },\n  \"type\" : \
                 \"object\"\n}",
                true,
            ),
            (
                "a JSON Schema document with another property",
                DATA_FORMAT_JSON,
                r#"{"type":"object","properties":{"id":{"type":"string"}}}"#,
                r#"{"type":"object","properties":{"id":{"type":"number"}}}"#,
                false,
            ),
            (
                "a protobuf message re-spaced onto several lines",
                DATA_FORMAT_PROTOBUF,
                "syntax = \"proto3\"; message User { string id = 1; int64 age = 2; }",
                "syntax = \"proto3\";\n\nmessage User {\n    string   id  = 1;\n    int64 age \
                 = 2;\n}\n",
                true,
            ),
            (
                "a protobuf message carrying a comment",
                DATA_FORMAT_PROTOBUF,
                "syntax = \"proto3\"; message User { string id = 1; }",
                "// the user\nsyntax = \"proto3\";\nmessage User {\n  string id = 1; // its id\n}",
                true,
            ),
            (
                "a protobuf message whose field numbers differ",
                DATA_FORMAT_PROTOBUF,
                "syntax = \"proto3\"; message User { string id = 1; }",
                "syntax = \"proto3\"; message User { string id = 2; }",
                false,
            ),
            (
                "a protobuf message whose field names differ",
                DATA_FORMAT_PROTOBUF,
                "syntax = \"proto3\"; message User { string id = 1; }",
                "syntax = \"proto3\"; message User { string key = 1; }",
                false,
            ),
        ];
        for (case, data_format, one, other, same) in cases {
            let left = canonical_schema_definition(data_format, one);
            let right = canonical_schema_definition(data_format, other);
            assert!(
                left.is_some() && right.is_some(),
                "case {case:?}: both documents must have a canonical form, got {left:?} and \
                 {right:?}"
            );
            assert_eq!(
                left == right,
                *same,
                "case {case:?}: expected the same document {same}, got {left:?} and {right:?}"
            );
        }

        // Text no parser of the format reads is no document of it, so it has no canonical
        // form to compare.
        for (case, data_format, definition) in [
            ("prose as AVRO", DATA_FORMAT_AVRO, "not a schema"),
            ("prose as JSON", DATA_FORMAT_JSON, "not a schema"),
            ("prose as PROTOBUF", DATA_FORMAT_PROTOBUF, "not a schema"),
            ("a format this emulator does not read", "ORC", "whatever"),
        ] {
            let read = canonical_schema_definition(data_format, definition);
            assert!(
                read.is_none(),
                "case {case:?}: expected no canonical form, got {read:?}"
            );
        }
    }
}
