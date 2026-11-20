// SPDX-License-Identifier: CC0-1.0

//! WHY a `SchemaDefinition` is not the protobuf schema document its `DataFormat` says it
//! is, in the words the schema registry answers.
//!
//! A `PROTOBUF` schema definition is the source text of a `.proto` file, so whether a
//! definition is one is the protobuf language's own question — answered here by a protobuf
//! source parser (`protox-parse`) rather than by scanning the text.
//!
//! The READING is the one the registry states: `Syntax error in <name>:<line>:<column>:
//! <what the document was required to carry>` (recorded: a field left without its
//! terminator is read as `Syntax error in :5:4: expected ';'`). Two things about that
//! clause are the registry's and not this parser's:
//!
//! * the NAME is empty — a definition is submitted as text and no file, so the reader has
//!   no file name to report and the clause opens `Syntax error in :`;
//! * the POSITION is the one just PAST the first character the parse stopped on, because
//!   the reader has consumed that character before it can say the document was required to
//!   carry something else — the same reading of a position the registry's JSON refusals
//!   place just past an unrecognised word ([`super::unreadable_definition_json`]).
//!
//! Nothing of the definition is echoed back beyond the token the parser's own sentence
//! names.
//!
//! What one request spends here is bounded twice. Its SIZE is bounded by the definition's
//! own length: the modelled `SchemaDefinitionString` admits 170000 characters and that
//! constraint is enforced before any handler runs, so one parse costs at most that much
//! text. Its DEPTH is bounded by [`MAX_PROTOBUF_NESTING_DEPTH`], counted in one flat pass
//! BEFORE the parser is entered, because a `.proto` source is read by a recursive-descent
//! parser into a tree that is walked and dropped recursively as well: without that count the
//! depth of that recursion would be the caller's to choose. (The parser applies a recursion
//! limit of its own to nested messages, and reports reaching it as a fault like any other;
//! the flat count is what bounds every other nesting the source can carry, and what keeps a
//! definition nested thousands deep from being parsed at all.) A lookup that canonicalizes
//! the definitions a schema set already holds bounds the PRODUCT of the length and the
//! number of versions separately ([`super::MAX_CANONICAL_DEFINITION_BYTES`]).

use ::miette::Diagnostic;

/// The name the registry reports a definition under: none, because a `SchemaDefinition` is
/// submitted as text and no file. Recorded: the position clause opens `Syntax error in :`.
const DEFINITION_SOURCE_NAME: &str = "";

/// The deepest nesting a `PROTOBUF` definition may carry: how many declarations may be open
/// at once at any point in the source.
///
/// The bound exists so the depth of the parser's own recursion, and of the walk and the drop
/// of the tree it builds, is not a caller's to choose: the modelled length admits a
/// definition that opens thousands of declarations, and a Rust stack that runs out aborts
/// the process rather than failing the one request. Its VALUE is the recursion limit
/// protobuf's own readers apply to nested messages (100 levels), which is far deeper than a
/// schema document carries.
pub const MAX_PROTOBUF_NESTING_DEPTH: usize = 100;

/// The sentence the parser states for a document nested deeper than it will descend into.
///
/// It is the parser's own wording, pinned here because the flat count below refuses such a
/// definition on the parser's behalf — before the parse, where the parser would otherwise
/// report the very same fault once its own recursion limit is reached. Keeping ONE sentence
/// is what makes the bound invisible to a caller: a definition nested too deep reads the
/// same whichever of the two stopped it (the two readings are asserted to agree by this
/// module's tests).
const NESTING_BEYOND_READING: &str = "reached maximum recursion limit for nested messages";

/// The byte offset of the declaration that opens one level deeper than
/// [`MAX_PROTOBUF_NESTING_DEPTH`] admits, or `None` when the definition stays within it.
///
/// ONE flat pass over the characters, entering no recursion of its own. It counts only the
/// declarations the SOURCE opens, so it follows the same three places the protobuf lexer
/// reads text rather than structure — a quoted string, a line comment and a block comment —
/// and counts no brace inside any of them: a brace in a field's default value or in a
/// comment is text. A string literal ends at a raw newline as the language says one does, so
/// a lone quotation mark cannot turn the count off for the rest of the document.
fn nesting_beyond_limit(definition: &str) -> Option<usize> {
    /// Where in the source the pass currently is.
    enum Reading {
        /// Source text, where a brace is structure.
        Source,
        /// Inside a string literal opened with this quotation.
        Quoted(char),
        /// Inside a `//` comment, which ends at the next newline.
        LineComment,
        /// Inside a `/* */` comment, which ends at the next `*/`.
        BlockComment,
    }

    let mut open = 0usize;
    let mut reading = Reading::Source;
    let mut escaped = false;
    let mut characters = definition.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        match reading {
            Reading::Quoted(delimiter) => {
                // A raw newline ends a string literal whatever precedes it, exactly as the
                // protobuf lexer ends one — a trailing backslash included, so no sequence a
                // caller can write carries the quoted state past the line it opened on and
                // turns the count off for the rest of the document.
                if character == '\n' {
                    reading = Reading::Source;
                    escaped = false;
                } else if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == delimiter {
                    reading = Reading::Source;
                }
            }
            Reading::LineComment => {
                if character == '\n' {
                    reading = Reading::Source;
                }
            }
            Reading::BlockComment => {
                if character == '*' && matches!(characters.peek(), Some((_, '/'))) {
                    characters.next();
                    reading = Reading::Source;
                }
            }
            Reading::Source => match character {
                // Both quotations the protobuf language writes a string in.
                '"' | '\'' => {
                    reading = Reading::Quoted(character);
                    escaped = false;
                }
                '/' => match characters.peek() {
                    Some((_, '/')) => {
                        characters.next();
                        reading = Reading::LineComment;
                    }
                    Some((_, '*')) => {
                        characters.next();
                        reading = Reading::BlockComment;
                    }
                    _ => {}
                },
                '{' => {
                    open += 1;
                    if open > MAX_PROTOBUF_NESTING_DEPTH {
                        return Some(offset);
                    }
                }
                '}' => open = open.saturating_sub(1),
                _ => {}
            },
        }
    }
    None
}

/// Why `definition` is not a protobuf schema document, as the registry's refusal states
/// it, or `None` when the protobuf parser accepts it.
#[must_use]
pub fn unreadable_definition_protobuf(definition: &str) -> Option<String> {
    // The depth bound is answered first, so a definition that would drive the parser's
    // recursion deeper than this emulator reads one is refused before the parse begins, in
    // the parser's own words for that fault.
    if let Some(offset) = nesting_beyond_limit(definition) {
        let (line, column) = position_past(definition, offset);
        return Some(format!(
            "Syntax error in {DEFINITION_SOURCE_NAME}:{line}:{column}: {NESTING_BEYOND_READING}"
        ));
    }
    let error = ::protox_parse::parse(DEFINITION_SOURCE_NAME, definition).err()?;
    let (line, column) = position_past(definition, stopped_at(&error));
    Some(format!(
        "Syntax error in {DEFINITION_SOURCE_NAME}:{line}:{column}: {}",
        reading(&error)
    ))
}

/// The canonical form of a protobuf `SchemaDefinition`: the descriptor the parser reads the
/// `.proto` source into, rendered structurally, or `None` when the parser does not read the
/// text as a document at all.
///
/// The LAYOUT of the source — the whitespace between its tokens, where its declarations sit
/// and the comments around them — is carried by the descriptor's source-code information
/// alone, which is dropped here: everything left is what the document SAYS, so two sources
/// that declare the same messages render the same form however they were written out.
///
/// A definition nested deeper than [`MAX_PROTOBUF_NESTING_DEPTH`] is not read at all, so
/// this parse's recursion is bounded exactly as [`unreadable_definition_protobuf`]'s is.
///
/// The form is a comparison KEY and nothing else: it is computed where two definitions are
/// compared, never stored and never reported, so nothing outside that comparison depends on
/// how it renders — a stored version's `SchemaDefinition` is always the text its
/// registration submitted.
#[must_use]
pub fn canonical_definition_protobuf(definition: &str) -> Option<String> {
    if nesting_beyond_limit(definition).is_some() {
        return None;
    }
    let mut descriptor = ::protox_parse::parse(DEFINITION_SOURCE_NAME, definition).ok()?;
    descriptor.source_code_info = None;
    Some(format!("{descriptor:?}"))
}

/// The byte offset in the submitted definition the parse stopped at: the start of the span
/// the error carries, or the end of the definition for a parse that ran out of input
/// without one.
fn stopped_at(error: &::protox_parse::ParseError) -> usize {
    error
        .labels()
        .and_then(|mut labels| labels.next())
        .map_or(usize::MAX, |label| label.offset())
}

/// The line and column just PAST the character at `offset` of `definition`, both counted
/// from one — the position the registry reports a definition fault at.
///
/// An `offset` beyond the definition (a parse that ran out of input) is read as its end,
/// and one inside a character as the boundary before it, so no reading here can split one.
fn position_past(definition: &str, offset: usize) -> (usize, usize) {
    let read = (0..=offset.min(definition.len()))
        .rev()
        .find_map(|end| definition.get(..end))
        .unwrap_or_default();
    let line = read.matches('\n').count() + 1;
    let last_line = read.rsplit('\n').next().unwrap_or_default();
    // One for counting columns from one, and one more for the character the reader had
    // already consumed when it stopped.
    let column = last_line.chars().count() + 2;
    (line, column)
}

/// What the document was required to carry, as the registry states it.
///
/// A parse that stopped on a token the grammar does not admit there is read as what the
/// document was required to carry — `expected ';'` — and not as what stood there instead:
/// the parser lists every token its own grammar would have taken, of which the first is
/// the one the declaration being read was required to end with. Every other class of fault
/// is reported in the parser's own sentence.
fn reading(error: &::protox_parse::ParseError) -> String {
    let sentence = error.to_string();
    let Some(alternatives) = sentence
        .strip_prefix("expected ")
        .and_then(|rest| rest.split(", but found ").next())
    else {
        return sentence;
    };
    let required = alternatives
        .split(" or ")
        .next()
        .unwrap_or(alternatives)
        .split(", ")
        .next()
        .unwrap_or(alternatives)
        .trim();
    format!("expected {required}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a definition the protobuf parser accepts carries no fault here, and one it
    /// does not is read as `Syntax error in :<line>:<column>: <what the document was
    /// required to carry>`, the position counted from one and placed just past the
    /// character the parse stopped on.
    ///
    /// The rows differ along what the reading generalises over: the syntax level the
    /// document declares (proto3, proto2, none at all), what the document holds (a message,
    /// an enum, a service, an import, nested messages), the class of fault (text that is no
    /// protobuf statement, a document of another format, a declaration left open, a field
    /// with no number or no name, a field number the language does not admit), and WHERE in
    /// the definition the fault sits — which line, and which column of it.
    #[test]
    fn a_definition_that_is_not_protobuf_reads_as_the_registrys_syntax_error() {
        let accepted: &[(&str, &str)] = &[
            (
                "a proto3 message",
                "syntax = \"proto3\"; message User { string id = 1; }",
            ),
            (
                "a proto2 message with a required field",
                "syntax = \"proto2\"; message User { required string id = 1; }",
            ),
            (
                "a document declaring no syntax, which is proto2",
                "message User { optional string id = 1; }",
            ),
            (
                "a package, an enum and a nested message",
                "syntax = \"proto3\"; package com.example; enum Kind { UNKNOWN = 0; FULL = 1; } \
                 message Order { message Line { string sku = 1; } Kind kind = 1; repeated Line \
                 lines = 2; }",
            ),
            (
                "a service and an import",
                "syntax = \"proto3\"; import \"google/protobuf/empty.proto\"; message Ping { \
                 string id = 1; } service Pinger { rpc Send (Ping) returns (Ping); }",
            ),
            (
                "a document whose statements are spread over several lines",
                "syntax = \"proto3\";\n\nmessage User {\n  string id = 1;\n  int64 age = 2;\n}\n",
            ),
        ];
        for (case, definition) in accepted {
            let read = unreadable_definition_protobuf(definition);
            assert!(
                read.is_none(),
                "case {case:?}: expected no fault, got {read:?}"
            );
        }

        // Each row states the WHOLE reading, so the position clause is pinned beside the
        // sentence. A field left without its terminator is the recorded class.
        let refused: &[(&str, &str, &str)] = &[
            (
                "a field left without its terminator, the next field on the line below",
                "syntax = \"proto3\";\npackage com.example;\nmessage Order {\n  int64 id = 1\n  \
                 string name = 2;\n}\n",
                "Syntax error in :5:4: expected ';'",
            ),
            (
                "the same fault one line higher",
                "syntax = \"proto3\";\nmessage Order {\n  int64 id = 1\n  string name = 2;\n}\n",
                "Syntax error in :4:4: expected ';'",
            ),
            (
                "the same fault indented one column further",
                "syntax = \"proto3\";\nmessage Order {\n   int64 id = 1\n   string name = 2;\n}\n",
                "Syntax error in :4:5: expected ';'",
            ),
            (
                "a field left without its terminator, all on one line",
                "syntax = \"proto3\"; message Order { int64 id = 1 string name = 2; }",
                "Syntax error in :1:50: expected ';'",
            ),
        ];
        for (case, definition, expected) in refused {
            let read = unreadable_definition_protobuf(definition);
            assert_eq!(
                read.as_deref(),
                Some(*expected),
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }

        // Classes whose sentence is the parser's own: the reading still opens with the
        // registry's clause, and still names no part of the definition but the token the
        // parser quotes.
        let other_faults: &[(&str, &str)] = &[
            ("prose", "not a protobuf document"),
            ("a JSON document", r#"{"type":"record","name":"User"}"#),
            (
                "a message left open",
                "syntax = \"proto3\"; message User { string id = 1;",
            ),
            (
                "a field with no name",
                "syntax = \"proto3\"; message User { string = 1; }",
            ),
            (
                "a syntax level the language does not have",
                "syntax = \"proto4\"; message User { string id = 1; }",
            ),
            (
                "a field number outside the range the language admits",
                "syntax = \"proto3\"; message User { string id = 0; }",
            ),
        ];
        for (case, definition) in other_faults {
            let read = unreadable_definition_protobuf(definition);
            let fault = read.unwrap_or_else(|| {
                panic!("case {case:?}: expected the registry's reading, got no fault")
            });
            assert!(
                fault.starts_with("Syntax error in :"),
                "case {case:?}: expected the registry's clause, got {fault:?}"
            );
            assert!(
                !fault.contains(definition),
                "case {case:?}: the reading echoed the whole definition back: {fault:?}"
            );
        }

        // A definition whose text is multi-byte: the reading answers rather than failing on
        // a character boundary.
        let multi_byte = "syntax = \"proto3\"; message Ünïcödé { string ïd = 1 }";
        assert!(
            unreadable_definition_protobuf(multi_byte).is_some(),
            "a multi-byte definition must read as a fault, not fail"
        );
    }

    /// One definition nesting `levels` declarations, the innermost holding one field.
    fn nested(levels: usize) -> String {
        let mut definition = String::from("syntax = \"proto3\";\n");
        for level in 0..levels {
            definition.push_str(&format!("message M{level} {{\n"));
        }
        definition.push_str("string id = 1;\n");
        for _ in 0..levels {
            definition.push_str("}\n");
        }
        definition
    }

    /// RULE: a definition is read only as deep as it can be read without the depth of any
    /// recursion being the caller's to choose: nesting past
    /// [`MAX_PROTOBUF_NESTING_DEPTH`] is refused by the flat count BEFORE the parser is
    /// entered, and nesting the parser itself will not descend is refused by the parser's own
    /// recursion limit — either way the request is ANSWERED, and a nesting as deep as the
    /// modelled length admits never reaches a parse.
    ///
    /// The rows are a document, a nesting the parser stops at, the published bound, one past
    /// it, and a nesting as deep as the modelled `SchemaDefinitionString` length admits
    /// (170000 characters, some 7000 levels) — which is what a caller could really submit.
    #[test]
    fn a_definition_is_read_only_as_deep_as_the_bound_on_its_nesting() {
        let shallow = nested(5);
        let read = unreadable_definition_protobuf(&shallow);
        assert!(
            read.is_none(),
            "case \"a nesting a document carries\": expected a document, got {read:?}"
        );
        assert!(
            canonical_definition_protobuf(&shallow).is_some(),
            "case \"a nesting a document carries\": a document must have a canonical form"
        );

        // Deeper than the parser will descend, but within the flat bound: the parser's own
        // recursion limit answers, in the registry's clause, rather than recursing on.
        let parser_limited = nested(MAX_PROTOBUF_NESTING_DEPTH);
        let fault = unreadable_definition_protobuf(&parser_limited).unwrap_or_else(|| {
            panic!("case \"a nesting the parser stops at\": expected a refusal, got no fault")
        });
        assert!(
            fault.starts_with("Syntax error in :") && fault.ends_with(NESTING_BEYOND_READING),
            "case \"a nesting the parser stops at\": expected the registry's clause ending \
             {NESTING_BEYOND_READING:?}, got {fault:?}"
        );
        assert!(
            canonical_definition_protobuf(&parser_limited).is_none(),
            "case \"a nesting the parser stops at\": text that is no document has no canonical \
             form"
        );

        for (case, levels) in [
            ("one level past the flat bound", MAX_PROTOBUF_NESTING_DEPTH + 1),
            ("as deep as the modelled length admits", 7_000),
        ] {
            let definition = nested(levels);
            let read = unreadable_definition_protobuf(&definition);
            let fault = read.unwrap_or_else(|| {
                panic!("case {case:?}: expected a refusal of the nesting, got no fault")
            });
            let expected = NESTING_BEYOND_READING;
            assert!(
                fault.ends_with(expected),
                "case {case:?}: expected a reading ending {expected:?}, got {fault:?}"
            );
            // The refusal names the line the declaration that crossed the bound opens on:
            // the syntax statement, then one declaration per line.
            let at = format!(":{}:", MAX_PROTOBUF_NESTING_DEPTH + 2);
            assert!(
                fault.contains(&at),
                "case {case:?}: expected the position {at:?}, got {fault:?}"
            );
            assert!(
                canonical_definition_protobuf(&definition).is_none(),
                "case {case:?}: text that is no document has no canonical form"
            );
        }

        // Braces inside a quoted string are text, not structure, so a default value holding
        // them is not read as nesting. (Written in proto2, the syntax level that admits a
        // default value.)
        let quoted = format!(
            "syntax = \"proto2\";\nmessage User {{\n  optional string id = 1 [default = \
             \"{}\"];\n}}\n",
            "{".repeat(MAX_PROTOBUF_NESTING_DEPTH * 2)
        );
        let read = unreadable_definition_protobuf(&quoted);
        assert!(
            read.is_none(),
            "case \"braces inside a string\": expected a document, got {read:?}"
        );

        // The text a COMMENT holds is text too, in both the forms the language writes a
        // comment in: braces in one are not nesting, and a quotation mark in one does not
        // turn the count off for what follows it.
        let braces = "{".repeat(MAX_PROTOBUF_NESTING_DEPTH * 2);
        for (case, definition) in [
            (
                "braces in a line comment",
                format!("syntax = \"proto3\";\n// {braces}\nmessage User {{ string id = 1; }}\n"),
            ),
            (
                "braces in a block comment",
                format!("syntax = \"proto3\";\n/* {braces} */\nmessage User {{ string id = 1; }}\n"),
            ),
            (
                "a quotation mark in a comment",
                "syntax = \"proto3\";\n// the user's own record\nmessage User { string id = 1; }\n"
                    .to_string(),
            ),
        ] {
            let read = unreadable_definition_protobuf(&definition);
            assert!(
                read.is_none(),
                "case {case:?}: expected a document, got {read:?}"
            );
        }

        // And the depth bound still holds past a comment that carries a lone quotation mark,
        // or past a string one was left open in: neither can disable the count, and neither
        // can an escape at the end of such a line.
        for (case, preamble) in [
            ("a comment carrying a lone quotation mark", "// the user's\n"),
            (
                "a block comment carrying a lone quotation mark",
                "/* the user's */\n",
            ),
            (
                "a string left open on its line",
                "option java_package = \"com.example\n",
            ),
            (
                "a string left open on a line ending in a backslash",
                "option java_package = \"com.example\\\n",
            ),
            (
                "a string left open on a line ending in two backslashes",
                "option java_package = \"com.example\\\\\n",
            ),
        ] {
            let definition = format!("{preamble}{}", nested(7_000));
            let read = unreadable_definition_protobuf(&definition);
            let fault = read.unwrap_or_else(|| {
                panic!("case {case:?}: expected a refusal of the nesting, got no fault")
            });
            let expected = NESTING_BEYOND_READING;
            assert!(
                fault.ends_with(expected),
                "case {case:?}: expected a reading ending {expected:?}, got {fault:?}"
            );
        }
    }
}
