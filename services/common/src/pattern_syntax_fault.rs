// SPDX-License-Identifier: CC0-1.0

//! The syntax fault a value that is supposed to be a REGULAR EXPRESSION carries, stated the
//! way the engine behind these services' pattern languages states one.
//!
//! A service whose API takes a regular expression from the caller — a catalog listing's
//! filter, a query language's `/…/` literal — does not word a syntax fault itself: it relays
//! what the engine compiling the expression said about it. That statement is three facts
//! about the CALLER's own text and nothing of the engine reading it: what is wrong, the index
//! into the expression where it was found, and the expression itself with a caret under that
//! index ([`PatternFault::stated_in`]).
//!
//! Which means a relayed statement never carries the name of the engine this emulator
//! compiles with, nor an expression this emulator rewrote the caller's into: an index that
//! points into a rewritten expression points at the wrong character of the one the caller
//! sent, and an engine's own diagnostic names an implementation the service it is relayed
//! from has none of.
//!
//! This is the workspace's ONE reading of that fault, so two services taking a caller's
//! expression place and name one syntax fault the same way. The parse is the `regex-syntax`
//! parser's — the structured error of the parser behind the matcher these services compile
//! with — and what it reports is translated into the description the relaying engine states
//! for that class of fault.
//!
//! WHAT BOUNDS IT: the expression is PARSED and never built into a matcher here or run over
//! any data, so what one call costs is the length of the expression, and the parser's own
//! nest limit bounds how deeply the groups of one expression may nest.

/// Why a value is not a regular expression: what is wrong, and where in the expression it
/// was found.
#[derive(Debug, Clone)]
pub struct PatternFault {
    /// What is wrong, as the engine compiling the expression names that fault.
    description: String,
    /// The index into the expression, counted in CHARACTERS, the parse stopped at. A fault
    /// the parse reaches at the END of an expression is reported one past its last
    /// character, which is a position no caret can mark.
    index: usize,
}

impl PatternFault {
    /// What is wrong, as the engine compiling the expression names that fault.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The index into the expression the parse stopped at, counted in characters.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// The same fault placed at `index` instead, for a caller that found it in a text it
    /// TRANSLATED from the caller's own: the fault is stated at the character of the text
    /// the CALLER sent that the faulting character came from, because an index into a
    /// translated text points at the wrong character of the original.
    #[must_use]
    pub fn placed_at(self, index: usize) -> Self {
        Self { index, ..self }
    }

    /// The whole statement about `pattern`: the fault's own account, the index it was found
    /// at, the expression on a line of its own, and a caret under that index when the index
    /// is INSIDE the expression — a fault reached at the end of an expression has no
    /// character there to point at.
    ///
    /// `pattern` is the expression this fault was read from, so a caller states the text it
    /// handed [`pattern_fault`] and never a different one: the index counts characters of
    /// THAT text.
    #[must_use]
    pub fn stated_in(&self, pattern: &str) -> String {
        let mut stated = format!(
            "{} near index {}\n{pattern}",
            self.description, self.index
        );
        if self.index < pattern.chars().count() {
            stated.push('\n');
            for _ in 0..self.index {
                stated.push(' ');
            }
            stated.push('^');
        }
        stated
    }
}

/// Why `pattern` is not a regular expression, or `None` when it is one.
///
/// What the parser calls UNSUPPORTED is not a fault of the expression: a look-around and a
/// back-reference are constructs the engines these services' expressions are compiled by
/// read, so an expression using one IS an expression. This answers whether the text is an
/// expression at all, not which constructs one parser implements.
#[must_use]
pub fn pattern_fault(pattern: &str) -> Option<PatternFault> {
    let error = ::regex_syntax::ast::parse::Parser::new().parse(pattern).err()?;
    let characters = pattern.chars().count();
    // The parser places a fault by BYTE offset; an index into the expression counts
    // CHARACTERS, the way the expression itself is written out beside it.
    let at = char_index(pattern, error.span().start.offset);
    let ends_at = char_index(pattern, error.span().end.offset);
    let faulting = pattern.chars().nth(at).unwrap_or(' ');
    let (description, index) = match error.kind() {
        // A group the expression never closes is read to the END of the expression, so that
        // is where the parse stopped.
        ::regex_syntax::ast::ErrorKind::GroupUnclosed => {
            ("Unclosed group".to_string(), characters)
        }
        ::regex_syntax::ast::ErrorKind::GroupUnopened => {
            ("Unmatched closing ')'".to_string(), at)
        }
        // A character class the expression never closes is read to the end too, and the
        // parse stops on its last character.
        ::regex_syntax::ast::ErrorKind::ClassUnclosed => (
            "Unclosed character class".to_string(),
            characters.saturating_sub(1),
        ),
        // A quantifier with nothing to quantify is a meta character left dangling where the
        // expression it repeats was owed.
        ::regex_syntax::ast::ErrorKind::RepetitionMissing => {
            (format!("Dangling meta character '{faulting}'"), at)
        }
        ::regex_syntax::ast::ErrorKind::RepetitionCountUnclosed => {
            ("Unclosed counted closure".to_string(), at)
        }
        // A counted repetition whose bounds run backwards is read to its CLOSING brace
        // before the two can be compared, so the parse stopped there rather than on the
        // brace that opened it.
        ::regex_syntax::ast::ErrorKind::RepetitionCountInvalid => (
            "Illegal repetition range".to_string(),
            ends_at.saturating_sub(1),
        ),
        ::regex_syntax::ast::ErrorKind::RepetitionCountDecimalEmpty => {
            ("Illegal repetition".to_string(), at)
        }
        ::regex_syntax::ast::ErrorKind::EscapeUnrecognized
        | ::regex_syntax::ast::ErrorKind::EscapeUnexpectedEof
        | ::regex_syntax::ast::ErrorKind::ClassEscapeInvalid => {
            ("Illegal/unsupported escape sequence".to_string(), at)
        }
        ::regex_syntax::ast::ErrorKind::EscapeHexEmpty
        | ::regex_syntax::ast::ErrorKind::EscapeHexInvalid
        | ::regex_syntax::ast::ErrorKind::EscapeHexInvalidDigit => {
            ("Illegal hexadecimal escape sequence".to_string(), at)
        }
        // A range is read from its low bound to its HIGH bound, and the bound that made the
        // range impossible is the second of the two — so the parse stopped on the last
        // character of the range rather than on its first.
        ::regex_syntax::ast::ErrorKind::ClassRangeInvalid
        | ::regex_syntax::ast::ErrorKind::ClassRangeLiteral => (
            "Illegal character range".to_string(),
            ends_at.saturating_sub(1),
        ),
        ::regex_syntax::ast::ErrorKind::GroupNameEmpty
        | ::regex_syntax::ast::ErrorKind::GroupNameInvalid
        | ::regex_syntax::ast::ErrorKind::GroupNameUnexpectedEof => (
            "named capturing group is missing trailing '>'".to_string(),
            at,
        ),
        ::regex_syntax::ast::ErrorKind::FlagUnrecognized
        | ::regex_syntax::ast::ErrorKind::FlagUnexpectedEof
        | ::regex_syntax::ast::ErrorKind::FlagDanglingNegation => {
            ("Unknown inline modifier".to_string(), at)
        }
        // Constructs the engines behind these expressions read: the text IS an expression.
        ::regex_syntax::ast::ErrorKind::UnsupportedLookAround
        | ::regex_syntax::ast::ErrorKind::UnsupportedBackreference => return None,
        // Every other fault the parser reports is one the relaying engine has no account of
        // its own for, so it states that it has none and still places the index.
        _ => ("Unexpected internal error".to_string(), at),
    };
    Some(PatternFault { description, index })
}

/// How many characters of `pattern` come before its byte offset `byte`, which is the index a
/// fault at that offset is placed at. A byte offset past the expression's last character — as
/// the end of a parse that ran out of text is — counts every character of it.
fn char_index(pattern: &str, byte: usize) -> usize {
    pattern
        .char_indices()
        .position(|(offset, _)| offset == byte)
        .unwrap_or_else(|| pattern.chars().count())
}

#[cfg(test)]
mod tests {
    use super::{pattern_fault, PatternFault};

    /// One row of the table below: the expression, and the fault's own account with the index
    /// it was found at — `None` for an expression that IS an expression.
    type Row = (&'static str, &'static str, Option<(&'static str, usize)>);

    /// A fault is named and placed in the expression as the CALLER wrote it; a text that is an
    /// expression carries none.
    ///
    /// The rows differ along the CLASS of fault — an unclosed construct, a dangling
    /// quantifier, an impossible range, an escape that names nothing — and along where in the
    /// expression it sits: at its first character, inside it, and past its last.
    #[test]
    fn a_fault_is_named_and_placed_in_the_expression() {
        let cases: [Row; 14] = [
            ("an expression", r"events_\d+", None),
            ("an expression with a class", r"[a-z_]+", None),
            // A look-around and a back-reference are constructs these engines read.
            ("a look-ahead", r"events(?=_raw)", None),
            ("a back-reference", r"(a)\1", None),
            (
                "a range whose bounds run backwards",
                "t[b-a]",
                Some(("Illegal character range", 4)),
            ),
            (
                "a range at the very start of the expression",
                "[z-a]",
                Some(("Illegal character range", 3)),
            ),
            (
                "a character class the expression never closes",
                "events[0-9",
                Some(("Unclosed character class", 9)),
            ),
            (
                "a group the expression never closes",
                "(events",
                Some(("Unclosed group", 7)),
            ),
            (
                "a closing parenthesis nothing opened",
                "events)",
                Some(("Unmatched closing ')'", 6)),
            ),
            (
                "a quantifier with nothing to repeat",
                "+events",
                Some(("Dangling meta character '+'", 0)),
            ),
            (
                "a counted repetition the expression never closes",
                r"e\d{2",
                Some(("Unclosed counted closure", 3)),
            ),
            (
                "an escape the expression stops in the middle of",
                "events\\",
                Some(("Illegal/unsupported escape sequence", 6)),
            ),
            (
                "a counted repetition whose bounds run backwards",
                "e{2,1}",
                Some(("Illegal repetition range", 5)),
            ),
            (
                "a counted repetition that counts nothing",
                "e{,2}",
                Some(("Illegal repetition", 2)),
            ),
        ];
        for (case, pattern, expected) in cases {
            let found = pattern_fault(pattern);
            match expected {
                None => assert!(
                    found.is_none(),
                    "case {case:?}: {pattern:?} is an expression, got {:?}",
                    found.map(|fault| fault.stated_in(pattern))
                ),
                Some((description, index)) => {
                    let fault = found.unwrap_or_else(|| {
                        panic!("case {case:?}: {pattern:?} is no expression, got no fault")
                    });
                    assert_eq!(
                        (fault.description(), fault.index()),
                        (description, index),
                        "case {case:?}: expected {description:?} at {index}, got {:?} at {}",
                        fault.description(),
                        fault.index()
                    );
                }
            }
        }
    }

    /// The statement is the fault, the index, the expression, and a caret under that index —
    /// and the caret line is left OUT for an index past the expression's last character.
    #[test]
    fn a_statement_places_a_caret_only_inside_the_expression() {
        // (case, the expression, the statement it carries)
        let cases = [
            (
                "a fault inside the expression",
                "t[b-a]",
                "Illegal character range near index 4\nt[b-a]\n    ^",
            ),
            (
                "a fault at the first character",
                "+events",
                "Dangling meta character '+' near index 0\n+events\n^",
            ),
            (
                "a fault past the last character",
                "(events",
                "Unclosed group near index 7\n(events",
            ),
            (
                "an expression of multi-byte characters",
                "ä[b-a]",
                "Illegal character range near index 4\nä[b-a]\n    ^",
            ),
        ];
        for (case, pattern, expected) in cases {
            let stated = pattern_fault(pattern)
                .unwrap_or_else(|| panic!("case {case:?}: {pattern:?} is no expression"))
                .stated_in(pattern);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// The index counts CHARACTERS, so the caret of a statement about an expression carrying
    /// multi-byte characters stands under the character the parse stopped on.
    #[test]
    fn the_caret_stands_under_the_character_the_parse_stopped_on() {
        // (case, the expression, the character the caret must stand under)
        let cases = [
            ("a single-byte expression", "t[b-a]", 'a'),
            ("a multi-byte prefix", "ä[b-a]", 'a'),
            ("a dangling quantifier", "+events", '+'),
        ];
        for (case, pattern, expected) in cases {
            let fault: PatternFault = pattern_fault(pattern)
                .unwrap_or_else(|| panic!("case {case:?}: {pattern:?} is no expression"));
            let marked = pattern.chars().nth(fault.index());
            assert_eq!(
                marked,
                Some(expected),
                "case {case:?}: expected the caret under {expected:?}, got {marked:?}"
            );
        }
    }
}
