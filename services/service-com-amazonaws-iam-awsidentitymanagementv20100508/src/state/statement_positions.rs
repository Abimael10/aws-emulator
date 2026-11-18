// SPDX-License-Identifier: CC0-1.0

//! WHERE each statement of a policy document sits in the text that document was read
//! from: the row and column it BEGINS at and the row and column it ENDS at, which are those
//! one character past its last — so a statement occupies the columns from the one up to, and
//! not including, the other.
//!
//! The spans of the statements of ONE document are CONTIGUOUS: a statement begins where the
//! statement before it ended, so the separator between two statements belongs to the span of
//! the one that follows it, and the FIRST statement of a document begins at its own first
//! character. That is what makes the span of a statement independent of how the document is
//! written: a document laid out `},{` and one laid out `}, {` both report the statement
//! after the separator as beginning where the one before it ended, and no position of the
//! document is attributed to no statement.
//!
//! A simulation reports the statements that determined its decision
//! (`EvaluationResult$MatchedStatements`), and each entry of that report locates the
//! statement in the document it was read from — "the row and column of the beginning of
//! the `Statement` in an IAM policy" (`Statement$StartPosition`) and "the row and column
//! of the end of a `Statement` in an IAM policy" (`Statement$EndPosition`), each a
//! `Position` of `Line` and `Column`. So the span is a fact about the DOCUMENT TEXT, not
//! about the decision: it is measured here, once per document, and the decision's own
//! report ([`super::simulated_decisions`]) names the statement its evaluation matched by
//! its position in that document's `Statement` element.
//!
//! The span is measured with a span-preserving JSON reader rather than by searching the
//! text, so the statement a position names is the statement the evaluation read: the
//! element is resolved exactly as the evaluation resolves it
//! (`service_common::iam_authorization`) — the `Statement` member of the top-level
//! object, as an array of statements or as the single statement a document that states one
//! object carries — and a document this cannot read as JSON at all locates nothing, which
//! is what a document the simulation decides nothing from needs.

use jsonc_parser::common::Ranged;

use crate::ops::types::Position;

/// The document's AST as the span-preserving reader hands it back. Aliased so the names
/// cannot collide with a modelled shape of `crate::ops::types`.
type JsonValue<'a> = jsonc_parser::ast::Value<'a>;

/// Strict JSON, not JSONC: an IAM policy document is plain JSON, so a comment, a trailing
/// comma or an unquoted property name is no document at all — the same reading the
/// validator that admitted the document took of it.
const STRICT_JSON: jsonc_parser::ParseOptions = jsonc_parser::ParseOptions {
    allow_comments: false,
    allow_loose_object_property_names: false,
    allow_trailing_commas: false,
};

/// The element of a policy document that carries its statements, spelled as the evaluation
/// that decides from them reads it (`service_common::iam_authorization`).
const STATEMENT_ELEMENT: &str = "Statement";

/// How many statements of ONE document are located.
///
/// The spans of a document are measured once per request and kept for the whole page it
/// answers, so their number is a factor of what one request retains; the statements come
/// out of a document the request itself may make 131,072 characters long, which is why the
/// count is bounded by a constant here rather than by what the document carries. It bounds
/// the MEASUREMENT only — how many statements one PAGE may report is bounded separately, by
/// the report's own bound (`super::simulated_decisions`'s
/// `MAX_REPORTED_STATEMENTS_PER_PAGE`), which is what bounds the response.
///
/// No document a simulation decides from can reach it: the evaluation reads a document only
/// up to `service_common::iam_authorization::MAX_POLICY_BYTES` (64 KiB) and reads a longer
/// one as stating nothing, while a statement that can contribute to a decision — one
/// carrying an effect, an action and a resource — is tens of characters. A statement past it
/// is reported WITHOUT a position rather than with one nothing measured, since the entry's
/// job of naming the policy does not depend on it.
const MAX_LOCATED_STATEMENTS: usize = 10_000;

/// Where ONE statement begins and ends in the document it was read from.
#[derive(Debug, Clone)]
pub struct StatementSpan {
    /// The row and column the statement BEGINS at: where the statement before it ended, or
    /// those of its own first character when it is the first statement of the document.
    pub start: Position,
    /// The row and column the statement ENDS at: those one character past its last, so a
    /// statement occupies the columns from [`Self::start`] up to but not including this
    /// one.
    pub end: Position,
}

/// Where each statement of `document` sits in it, in document order — so the entry at
/// index `n` is the span of the statement the evaluation counts as `n`.
///
/// The spans TILE the statement list: the statement at index `n` begins where the statement
/// at `n - 1` ended, which is one character past that statement's last, and the statement at
/// index `0` begins at its own first character. So the separator a document puts between two
/// statements — a comma, and whatever space is written around it — is part of the span of
/// the statement that follows it, and how the document is laid out around a statement does
/// not change where that statement is reported to begin relative to the one before it.
///
/// Empty for a document that locates nothing: one this cannot read as JSON, one whose
/// top-level value is not an object, and one whose `Statement` element is neither an array
/// nor an object — all of which the evaluation reads as stating no statement either.
pub fn statement_spans(document: &str) -> Vec<StatementSpan> {
    let ranges = statement_byte_ranges(document);
    // The byte range of each statement, with its start moved back to where the statement
    // before it ended: the statements' spans then tile the list, leaving no position of the
    // document attributed to no statement.
    let tiled: Vec<(usize, usize)> = ranges
        .iter()
        .enumerate()
        .map(|(at, (start, end))| {
            match at.checked_sub(1).and_then(|before| ranges.get(before)) {
                Some((_, ended)) => (*ended, *end),
                None => (*start, *end),
            }
        })
        .collect();
    // The ranges are in document order and do not overlap, so the offsets they are made of
    // ascend — which is what lets one walk of the text resolve all of them.
    let wanted: Vec<usize> = tiled
        .iter()
        .flat_map(|(start, end)| [*start, *end])
        .collect();
    let located = positions_at(document, &wanted);
    located
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| StatementSpan {
            start: pair[0].clone(),
            end: pair[1].clone(),
        })
        .collect()
}

/// Where each statement of each of `documents` sits in it: one entry per document, in the
/// order the caller holds them.
pub fn statement_spans_of(documents: &[String]) -> Vec<Vec<StatementSpan>> {
    documents
        .iter()
        .map(|document| statement_spans(document))
        .collect()
}

/// The `(first byte, first byte past the last)` of each statement of `document`, in
/// document order.
///
/// The LAST `Statement` element wins when a document repeats the key, because that is the
/// value a JSON reader keeps — `serde_json`, which the evaluation reads the same document
/// with — so the spans locate the statements that were actually decided from.
///
/// The text handed here is one the operation's own validator already admitted as a policy
/// document (`crate::state::policy_document_defect`), so the reading below is a SECOND
/// reading of a document that parsed once: it adds the positions that reading does not
/// keep, and answers nothing for a text no reader admits.
fn statement_byte_ranges(document: &str) -> Vec<(usize, usize)> {
    let Ok(parsed) = jsonc_parser::parse_to_ast(
        document,
        &jsonc_parser::CollectOptions::default(),
        &STRICT_JSON,
    ) else {
        return Vec::new();
    };
    let Some(JsonValue::Object(policy)) = parsed.value else {
        return Vec::new();
    };
    let Some(element) = policy
        .properties
        .iter()
        .rfind(|property| property.name.as_str() == STATEMENT_ELEMENT)
    else {
        return Vec::new();
    };
    match &element.value {
        JsonValue::Array(entries) => entries
            .elements
            .iter()
            .take(MAX_LOCATED_STATEMENTS)
            .map(byte_range)
            .collect(),
        single @ JsonValue::Object(_) => vec![byte_range(single)],
        // An element that is neither states no statement, which locates nothing.
        _ => Vec::new(),
    }
}

/// The `(first byte, first byte past the last)` one value occupies, as the reader reports
/// it.
fn byte_range(value: &JsonValue<'_>) -> (usize, usize) {
    let range = value.range();
    (range.start, range.end)
}

/// The row and column of each byte offset of `offsets`, which ASCEND — two offsets may be
/// the same one, where a statement begins where the statement before it ended — in one walk
/// of `document`.
///
/// The row is counted from one at the document's first line and the column from one at each
/// line's first character, which is how a `Position` of a policy document's text reads
/// ("the line containing the specified position in the document", "the column in the line
/// containing the specified position"). Columns are counted in CHARACTERS, so a document
/// holding text outside ASCII locates a statement at the character it starts at rather than
/// at a byte of it; an offset that lands inside a character resolves to that character, and
/// the offset one past the document's last byte to the column its last line ended at.
fn positions_at(document: &str, offsets: &[usize]) -> Vec<Position> {
    let mut located: Vec<Position> = Vec::with_capacity(offsets.len());
    let mut wanted = offsets.iter().peekable();
    let mut line = 1i32;
    let mut column = 1i32;
    for (at, character) in document.char_indices() {
        // Every offset asked about is the first byte of a character, since both ends of a
        // span come from a character boundary — so an offset is answered at the character
        // it opens, and one that somehow landed inside a character at the next one.
        while wanted.next_if(|offset| **offset <= at).is_some() {
            located.push(Position { line, column });
        }
        if character == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    // The walk ended just PAST the document's last character, which is the position of an
    // offset at its end — where the span of a statement the document closes with ends.
    for _ in wanted {
        located.push(Position { line, column });
    }
    located
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement's span, as `(start line, start column, end line, end column)`, for the
    /// rows below to state compactly.
    type Span = (i32, i32, i32, i32);

    /// One row of the location table: the case, the document and the span of each of its
    /// statements.
    type Row = (&'static str, &'static str, &'static [Span]);

    /// The span of each statement of `document`, as the rows state them.
    fn located(document: &str) -> Vec<Span> {
        statement_spans(document)
            .into_iter()
            .map(|span| (span.start.line, span.start.column, span.end.line, span.end.column))
            .collect()
    }

    /// RULE: each statement ENDS at the row and column one character past its last and
    /// BEGINS where the statement before it ended, both counted from one — the first
    /// statement of a document at its own first character.
    #[test]
    fn a_statement_is_located_from_its_first_character_to_past_its_last() {
        // (case, the document, the span of each of its statements)
        let rows: &[Row] = &[
            (
                "one statement of a minified document",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow"}]}"#,
                // `{` of the statement is the 38th character and `}` the 55th, so the
                // statement ends at the 56th — the `]` after it.
                &[(1, 38, 1, 56)],
            ),
            (
                "one statement of a spaced document",
                r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow"}]}"#,
                &[(1, 41, 1, 60)],
            ),
            (
                "two statements on one line",
                r#"{"Statement":[{"Effect":"Allow"},{"Effect":"Deny"}]}"#,
                // The second begins where the first ended, at the `,` between them.
                &[(1, 15, 1, 33), (1, 33, 1, 51)],
            ),
            (
                "two statements written with a space after the separator",
                r#"{"Statement":[{"Effect":"Allow"}, {"Effect":"Deny"}]}"#,
                // The second still begins where the first ended, two characters before its
                // own `{`: the separator and the space after it are part of its span.
                &[(1, 15, 1, 33), (1, 33, 1, 52)],
            ),
            (
                "a statement spanning several lines",
                "{\n  \"Statement\": [\n    {\n      \"Effect\": \"Allow\"\n    }\n  ]\n}",
                &[(3, 5, 5, 6)],
            ),
            (
                "a single statement stated as an object",
                r#"{"Statement":{"Effect":"Allow"}}"#,
                &[(1, 14, 1, 32)],
            ),
            (
                "a document that is not JSON",
                "{\"Statement\": [",
                &[],
            ),
            ("a document that is not an object", r#"["Statement"]"#, &[]),
            ("a document holding no statement", r#"{"Version":"2012-10-17"}"#, &[]),
            (
                "a document whose statement element is a string",
                r#"{"Statement":"Allow"}"#,
                &[],
            ),
            (
                "a repeated statement element, of which the last is read",
                r#"{"Statement":[{"Effect":"Allow"}],"Statement":[{"Effect":"Deny"},{"Effect":"Allow"}]}"#,
                &[(1, 48, 1, 65), (1, 65, 1, 84)],
            ),
            (
                "a statement the document ends with",
                r#"{"Statement":{"Effect":"Allow"}"#,
                // Unbalanced, so no reader admits it: a document that locates nothing.
                &[],
            ),
        ];

        for (case, document, expected) in rows {
            let spans = located(document);
            assert_eq!(
                spans, *expected,
                "case {case:?}: expected the spans {expected:?}, got {spans:?}"
            );
        }
    }

    /// RULE: the column is a CHARACTER count, so text outside ASCII before a statement
    /// moves it by one column per character and not per byte.
    #[test]
    fn a_column_counts_characters_and_not_bytes() {
        let document = r#"{"Sid":"ü","Statement":[{"Effect":"Allow"}]}"#;
        let spans = located(document);
        // `{` of the statement is the 25th CHARACTER of the document; the `ü` before it is
        // two bytes, so a byte count would have said 26.
        let expected = vec![(1, 25, 1, 43)];
        assert_eq!(
            spans, expected,
            "case 'a document holding a two-byte character': expected {expected:?}, got {spans:?}"
        );
    }
}
