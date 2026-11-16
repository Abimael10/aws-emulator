// SPDX-License-Identifier: CC0-1.0

//! How this engine WORDS a query it could not read, for either query language.
//!
//! A parse refusal is a client-visible contract: the caller fixes the query from the
//! message, so the message has to say where the parse stopped and what could have stood
//! there, in the words the endpoint's own parser uses. Neptune's two query endpoints word
//! it differently, because each is a different parser:
//!
//! * the Gremlin endpoint reports the line, the 0-based character position in that line
//!   and the input the parser could not continue past, as its ANTLR-generated parser does
//!   ([`gremlin_parse_failure`]);
//! * the openCypher endpoint reports the input it could not read, the alternatives it
//!   expected, and the line, column and 0-based offset it stopped at
//!   ([`open_cypher_parse_failure`]).
//!
//! Every variable part of either message comes from the submitted query and the parser's
//! own report — the offset it stopped at, the text at that offset, and what it expected —
//! so no value of a recorded run is baked in anywhere.

/// Where in a submitted query a parse stopped, counted in CHARACTERS so a query carrying
/// multi-byte text reports the position a reader of that text would count to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryPosition {
    /// The line the position is on, counting from 1.
    pub line: usize,
    /// The position within that line, counting from 0 — what a parser built on ANTLR
    /// reports as the character position in the line.
    pub character_in_line: usize,
    /// The position within the whole query, counting from 0.
    pub offset: usize,
}

impl QueryPosition {
    /// Where `byte_offset` is in `query`. An offset past the end of the query, or inside a
    /// character, is clamped to the nearest character boundary at or before it, so a
    /// position is always one a reader could point at.
    pub fn of(query: &str, byte_offset: usize) -> Self {
        let mut bounded = byte_offset.min(query.len());
        while bounded > 0 && !query.is_char_boundary(bounded) {
            bounded -= 1;
        }
        let read = &query[..bounded];
        let line = 1 + read.matches('\n').count();
        let in_line = match read.rfind('\n') {
            Some(newline) => &read[newline + 1..],
            None => read,
        };
        Self {
            line,
            character_in_line: in_line.chars().count(),
            offset: read.chars().count(),
        }
    }
}

/// What the Gremlin endpoint tells a caller whose query it could not read.
///
/// The wording is the Gremlin endpoint's own: the line and the 0-based character position
/// in that line where the parse stopped, and then the parser's message — the input it
/// could not continue past, which is the submitted query up to and including the character
/// at that position.
pub fn gremlin_parse_failure(query: &str, byte_offset: usize) -> String {
    let position = QueryPosition::of(query, byte_offset);
    // The input the parser read before it gave up, INCLUDING the character it stopped on:
    // the next character boundary after the offset is where that prefix ends.
    let mut through = byte_offset.min(query.len());
    if through < query.len() {
        through += 1;
        while through < query.len() && !query.is_char_boundary(through) {
            through += 1;
        }
    }
    format!(
        "Query parsing failed at line {}, character position at {}, error message : \
         no viable alternative at input '{}'",
        position.line,
        position.character_in_line,
        quoted_input(&query[..through])
    )
}

/// At most this many characters of the submitted query one parse message quotes. The
/// quoted input is a prefix of the caller's own query, so the length is the caller's
/// choice and a message is not the place to carry a whole query.
const QUOTED_INPUT_LIMIT: usize = 256;

/// A piece of the submitted query as a message may quote it: every character as it was
/// submitted — the quotation marks a query is written with included, because a parser
/// quotes the input it read rather than a re-escaped copy of it — except a CONTROL
/// character, which is escaped so a message cannot forge a line in anything that renders
/// it, and bounded in length.
fn quoted_input(text: &str) -> String {
    let mut quoted = String::new();
    for character in text.chars().take(QUOTED_INPUT_LIMIT) {
        if character.is_control() {
            quoted.extend(character.escape_debug());
        } else {
            quoted.push(character);
        }
    }
    quoted
}

/// What the openCypher endpoint tells a caller whose query it could not read: the input it
/// could not use, the alternatives it expected there, and where it stopped.
///
/// `message` is the parser's own report and `span` the range of the input it could not
/// use. A word that is a proper prefix of a keyword is a special case of that span: the
/// keyword is read character by character, so the parse stops at the first character that
/// cannot continue it and the alternatives are that character in either case — which is
/// why `CREAT ` is refused for the space after it rather than for the word.
pub fn open_cypher_parse_failure(
    query: &str,
    message: &str,
    span: ::std::ops::Range<usize>,
) -> String {
    let found = query.get(span.start..span.end).unwrap_or_default();
    let (byte_offset, input, expected) = match keyword_continuation(found) {
        Some(next) => (
            span.end,
            character_at(query, span.end),
            format!(
                "{}/{}",
                next.to_ascii_lowercase(),
                next.to_ascii_uppercase()
            ),
        ),
        None if found.is_empty() => (
            span.start,
            END_OF_INPUT.to_string(),
            expectation(message).to_string(),
        ),
        None => (
            span.start,
            quoted_input(found),
            expectation(message).to_string(),
        ),
    };
    let position = QueryPosition::of(query, byte_offset);
    format!(
        "Invalid input '{input}': expected '{expected}' (line {}, column {} (offset: {}))",
        position.line,
        position.character_in_line + 1,
        position.offset
    )
}

/// What a parser names the end of the input it was reading.
const END_OF_INPUT: &str = "EOI";

/// The character at `byte_offset`, or [`END_OF_INPUT`] when the query ends there.
fn character_at(query: &str, byte_offset: usize) -> String {
    match query.get(byte_offset..).and_then(|rest| rest.chars().next()) {
        Some(character) => quoted_input(&character.to_string()),
        None => END_OF_INPUT.to_string(),
    }
}

/// What the parser expected, out of its report: everything it said before naming what it
/// found instead, with the quotation the report put around it removed — the message quotes
/// it again in its own place.
fn expectation(message: &str) -> &str {
    let expected = message
        .split_once(", found ")
        .map_or(message, |(expected, _)| expected);
    expected
        .strip_prefix("expected ")
        .unwrap_or(expected)
        .trim_matches('\'')
}

/// The character that has to follow `word` for it to become a keyword, when `word` is a
/// proper prefix of exactly one keyword's spelling and is not a keyword itself.
///
/// `None` when the word is a keyword, is no keyword's prefix, or is the prefix of two
/// keywords that continue differently — in which case the parse did not stop on one
/// expected character and the parser's own report stands.
fn keyword_continuation(word: &str) -> Option<char> {
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut continuation = None;
    for keyword in OPEN_CYPHER_KEYWORDS {
        if keyword.len() == word.len() && keyword.eq_ignore_ascii_case(word) {
            // The word IS a keyword, so nothing was missing from it.
            return None;
        }
        if keyword.len() <= word.len() || !keyword[..word.len()].eq_ignore_ascii_case(word) {
            continue;
        }
        let next = keyword[word.len()..].chars().next()?;
        match continuation {
            None => continuation = Some(next),
            // Two keywords continue this word differently, so the parse did not stop on
            // one expected character.
            Some(first) if !first.eq_ignore_ascii_case(&next) => return None,
            Some(_) => {}
        }
    }
    continuation
}

/// The keywords an openCypher query is written with.
///
/// Source: the reserved words of the openCypher grammar (openCypher 9), which is the
/// language this endpoint's parser reads. They are listed in their upper-case spelling and
/// matched without regard to case, because openCypher keywords are case-insensitive.
const OPEN_CYPHER_KEYWORDS: &[&str] = &[
    "ALL",
    "AND",
    "AS",
    "ASC",
    "ASCENDING",
    "BY",
    "CALL",
    "CASE",
    "CONTAINS",
    "COUNT",
    "CREATE",
    "DELETE",
    "DESC",
    "DESCENDING",
    "DETACH",
    "DISTINCT",
    "ELSE",
    "END",
    "ENDS",
    "EXISTS",
    "FALSE",
    "FOREACH",
    "IN",
    "IS",
    "LIMIT",
    "MANDATORY",
    "MATCH",
    "MERGE",
    "NOT",
    "NULL",
    "ON",
    "OPTIONAL",
    "OR",
    "ORDER",
    "REMOVE",
    "RETURN",
    "SCALAR",
    "SET",
    "SKIP",
    "STARTS",
    "THEN",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UNWIND",
    "WHEN",
    "WHERE",
    "WITH",
    "XOR",
    "YIELD",
];
