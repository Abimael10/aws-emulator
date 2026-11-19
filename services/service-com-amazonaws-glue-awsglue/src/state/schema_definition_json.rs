// SPDX-License-Identifier: CC0-1.0

//! WHY a `SchemaDefinition` is not the JSON document its `DataFormat` is written as, in the
//! words the schema registry answers.
//!
//! An `AVRO` schema document and a `JSON` one are both JSON text, so a definition that is
//! not JSON at all never reaches the question of whether it describes a schema: the
//! registry refuses it with its JSON parser's own diagnostic, framed by
//! [`super::messages::schema_definition_invalid`]. The diagnostic names what the parse
//! stopped ON, what could have stood there instead, and WHERE in the submitted definition
//! that was — the only reading a caller holding a 170000-character definition has — and it
//! names nothing of the definition itself, which this service does not echo back.
//!
//! The READINGS are not this service's: where a parse stopped, and what it stopped on, are
//! the same questions about the same document wherever a service takes an opaque JSON member,
//! so they come from [`::service_common::json_parse_fault`]. A fault class worded wrongly is
//! corrected THERE, for every service that quotes it.
//!
//! What is this service's own is WHERE the position of an unrecognised bare word is reported:
//! this registry's refusals place it just PAST the word's last character rather than at its
//! first, because the reader has consumed the whole word before it can say the word is no
//! value — the position reported is how far it had read (recorded: a definition whose first
//! three columns spell such a word is refused at column 4).

use ::service_common::json_parse_fault as fault;

/// Why `definition` is not JSON, as the registry's refusal states it, or `None` when it
/// parses — a definition that IS JSON and describes no schema of its format breaks a rule
/// about the SCHEMA, which that format's own parser words rather than this reading.
#[must_use]
pub fn unreadable_definition_json(definition: &str) -> Option<String> {
    let error = ::serde_json::from_str::<::serde_json::Value>(definition).err()?;
    Some(diagnostic(definition, &error))
}

/// The whole diagnostic for a definition the parse `error` refused: the fault, the start
/// marker of a container the definition left open, and the position the parse ended at, with
/// the source of each clause reported as redacted.
///
/// `error` is the parse that refused `definition`: call this where that parse failed, and
/// nowhere else.
fn diagnostic(definition: &str, error: &::serde_json::Error) -> String {
    // A bare word where a VALUE belongs is named WHOLE, at the position just past it (the
    // reader matched every character of the word before it could judge it); every other
    // fault names the character, or the end of the input, the parse stopped on, at the
    // position the parse itself reports.
    let (reading, line, column) = match fault::unrecognized_token(definition, error) {
        Some((word, line, column)) => (
            format!(
                "Unrecognized token '{word}': was expecting ({})",
                fault::VALUE_CLASSES
            ),
            line,
            column + word.chars().count(),
        ),
        None => {
            let (line, column) = fault::stopped_at(definition, error);
            (fault::unexpected_input(definition, error), line, column)
        }
    };
    // A definition that ran out of input inside a collection reports where that collection
    // OPENED as well, which is what says which brace was left unmatched.
    let framed = match fault::unclosed_container_start(definition, error) {
        Some((marker_line, marker_column)) => format!(
            "{reading} (start marker at {})",
            fault::location_clause(fault::REDACTED_SOURCE, marker_line, marker_column)
        ),
        None => reading,
    };
    format!("{framed}{}", fault::source_location(line, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a definition that is not JSON is read as the fault the parse stopped on, at the
    /// position it stopped — the position just PAST an unrecognised word — and one that IS
    /// JSON carries no fault here at all.
    ///
    /// The rows differ along what the reading generalises over: the class of fault (a bare
    /// word, a character no value begins, an unquoted member name, a collection left open, a
    /// string left open), how far into the word the parse got, where in the definition the
    /// fault sits, and which LINE it is on.
    #[test]
    fn a_definition_that_is_not_json_reads_as_the_fault_the_parse_stopped_on() {
        let redacted = fault::REDACTED_SOURCE;
        let classes = fault::VALUE_CLASSES;
        let cases: &[(&str, &str, Option<String>)] = &[
            (
                "an Avro record document",
                r#"{"type":"record","name":"User","fields":[]}"#,
                None,
            ),
            ("a JSON Schema document", r#"{"type":"object"}"#, None),
            ("a bare literal", "null", None),
            (
                "a bare word a literal starts like",
                "not a schema",
                Some(format!(
                    "Unrecognized token 'not': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 1, column: 4]"
                )),
            ),
            (
                "a longer bare word",
                "schema-registry",
                Some(format!(
                    "Unrecognized token 'schema': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 1, column: 7]"
                )),
            ),
            (
                "a bare word where a member's value belongs",
                r#"{"type": record}"#,
                Some(format!(
                    "Unrecognized token 'record': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 1, column: 16]"
                )),
            ),
            (
                "a bare word on the third line",
                "\n\n  nope",
                Some(format!(
                    "Unrecognized token 'nope': was expecting ({classes})\n at [Source: \
                     {redacted}; line: 3, column: 7]"
                )),
            ),
            (
                "a character no value begins",
                "*",
                Some(format!(
                    "Unexpected character ('*' (code 42)): was expecting a valid value \
                     ({classes})\n at [Source: {redacted}; line: 1, column: 1]"
                )),
            ),
            (
                "an unquoted member name",
                "{type: \"record\"}",
                Some(format!(
                    "Unexpected character ('t' (code 116)): was expecting double-quote to \
                     start field name\n at [Source: {redacted}; line: 1, column: 2]"
                )),
            ),
            (
                "an object left open",
                r#"{"type":"record""#,
                Some(format!(
                    "Unexpected end-of-input: expected close marker for Object (start marker \
                     at [Source: {redacted}; line: 1, column: 1])\n at [Source: {redacted}; \
                     line: 1, column: 17]"
                )),
            ),
            (
                "an array left open inside an object",
                r#"{"fields":[{"name":"id"}"#,
                Some(format!(
                    "Unexpected end-of-input: expected close marker for Array (start marker \
                     at [Source: {redacted}; line: 1, column: 11])\n at [Source: {redacted}; \
                     line: 1, column: 25]"
                )),
            ),
            (
                "a string left open",
                r#"{"type":"reco"#,
                Some(format!(
                    "Unexpected end-of-input: expected close marker for String\n at [Source: \
                     {redacted}; line: 1, column: 14]"
                )),
            ),
        ];
        for (case, definition, expected) in cases {
            let read = unreadable_definition_json(definition);
            assert_eq!(
                read.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
