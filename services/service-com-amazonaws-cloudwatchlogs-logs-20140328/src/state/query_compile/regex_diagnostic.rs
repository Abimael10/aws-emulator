// SPDX-License-Identifier: CC0-1.0

//! The REGULAR EXPRESSION a CloudWatch Logs Insights QL query carries: the form the query
//! compiler reads it in, and the diagnostic a literal that is not a regular expression at all
//! is refused with.
//!
//! A `/…/` literal is not compiled as the query wrote it. The compiler REWRITES it first —
//! every NAMED capture group is renamed to the positional name the compiler gives it, and each
//! renamed group opens with the inline flag that lets `.` match a newline, because the message
//! of a log event may carry one — and the expression a refusal reports is that REWRITTEN one,
//! which is the text the reported index points into.
//!
//! The diagnostic is the compile failure of that expression, as the compiler words one
//! (recorded): what is wrong, the index into the rewritten expression the compiler stopped at,
//! the expression on a line of its own, and a caret under that index when the index is INSIDE
//! the expression — a fault the compiler reaches at the END of an expression has no character
//! there to point at. Naming and placing that fault is the workspace's one reading of a syntax
//! fault in a caller's expression ([`::service_common::pattern_syntax_fault`]), so this
//! service and every other that relays such a statement place one fault alike; what is this
//! module's own is the form the compiler reads the expression in and the sentence the
//! diagnostic opens with.
//!
//! WHAT BOUNDS IT: the expression is part of a query string the model constrains to
//! `@length(0..10000)`; it is PARSED — to decide whether it is an expression, and to place the
//! fault when it is not — and never built into a matcher here or run over any data, and the
//! parser's own nest limit bounds how deeply the groups of one expression may nest.

use std::fmt::Write as _;

use ::service_common::pattern_syntax_fault::{pattern_fault, PatternFault};

/// Why a regular-expression literal is not a regular expression, in the terms the query
/// compiler reports a compile failure in.
pub(super) struct InvalidExpression {
    /// The expression as the compiler reads it ([`rewritten_expression`]).
    rewritten: String,
    /// What is wrong and where in that expression it was found.
    fault: PatternFault,
}

impl InvalidExpression {
    /// The whole diagnostic: the rule the query broke, the expression the compiler read, and
    /// the compile failure of that expression — the failure's own account, the index it
    /// stopped at, the expression again on a line of its own, and a caret under that index
    /// when the index is inside it.
    pub(super) fn diagnostic(&self) -> String {
        let mut diagnostic = String::new();
        let _ = write!(
            diagnostic,
            "{INVALID_EXPRESSION_PREFIX}'{}' — {}",
            self.rewritten,
            self.fault.stated_in(&self.rewritten)
        );
        diagnostic
    }
}

/// The fixed text such a diagnostic opens with, the expression the compiler read following it
/// in quotes.
const INVALID_EXPRESSION_PREFIX: &str = "Invalid regular expression in query: ";

/// Why `pattern` is not a regular expression, or `None` when it is one.
///
/// What the parser calls UNSUPPORTED is not a fault of the query: a look-around and a
/// back-reference are constructs the service's own expression engine reads, so an expression
/// using one is an expression. This judges whether the text IS an expression, not which
/// constructs one parser implements.
pub(super) fn invalid_expression(pattern: &str) -> Option<InvalidExpression> {
    let rewritten = rewritten_expression(pattern);
    let fault = pattern_fault(&rewritten)?;
    Some(InvalidExpression { rewritten, fault })
}

/// `pattern` as the compiler reads it: every named capture group renamed to the positional
/// name the compiler gives it — `capture000`, counted from zero in the order the groups open —
/// and opening with the inline flag that lets `.` match a newline.
///
/// A group is named `(?<name>`, the form `StartQuery`'s own `parse` examples write, or
/// `(?P<name>`, the form the Python flavour spells the same group with. Text the expression
/// ESCAPES, and text inside a character class, is left exactly as written: a `(` there opens
/// no group. A `(?<` the expression never closes with a `>` names no group either, so it is
/// left as written for the parser to place the fault in.
pub(super) fn rewritten_expression(pattern: &str) -> String {
    let characters: Vec<char> = pattern.chars().collect();
    let mut rewritten = String::with_capacity(pattern.len());
    let mut groups = 0usize;
    let mut index = 0usize;
    let mut inside_class = false;
    while index < characters.len() {
        let character = characters[index];
        if character == '\\' {
            rewritten.push(character);
            if let Some(escaped) = characters.get(index + 1) {
                rewritten.push(*escaped);
            }
            index += 2;
            continue;
        }
        if inside_class {
            inside_class = character != ']';
            rewritten.push(character);
            index += 1;
            continue;
        }
        if character == '[' {
            inside_class = true;
            rewritten.push(character);
            index += 1;
            continue;
        }
        if let Some((name_at, name)) = group_name(&characters, index) {
            let _ = write!(rewritten, "(?<{}>{DOT_MATCHES_NEWLINE}", positional_name(groups));
            groups += 1;
            index = name_at + name.chars().count() + 1;
            continue;
        }
        rewritten.push(character);
        index += 1;
    }
    rewritten
}

/// The inline flag a renamed group opens with, which makes `.` match a newline as well: the
/// message of a log event the expression is matched against may carry one.
const DOT_MATCHES_NEWLINE: &str = "(?s)";

/// The positional name the compiler gives the `ordinal`-th named group of an expression,
/// counted from zero.
fn positional_name(ordinal: usize) -> String {
    format!("capture{ordinal:03}")
}

/// Where the name of the group opening at `index` starts, and that name — `None` when nothing
/// opens a NAMED group there, or when the name is not closed by a `>`.
fn group_name(characters: &[char], index: usize) -> Option<(usize, String)> {
    if characters[index] != '(' || characters.get(index + 1) != Some(&'?') {
        return None;
    }
    let name_at = if characters.get(index + 2) == Some(&'<') {
        index + 3
    } else if characters.get(index + 2) == Some(&'P') && characters.get(index + 3) == Some(&'<') {
        index + 4
    } else {
        return None;
    };
    let name: String = characters
        .get(name_at..)?
        .iter()
        .take_while(|character| **character != '>')
        .collect();
    if name.is_empty() || characters.get(name_at + name.chars().count()) != Some(&'>') {
        return None;
    }
    Some((name_at, name))
}

#[cfg(test)]
mod tests {
    use super::{invalid_expression, rewritten_expression};

    /// The form the compiler reads an expression in: the groups renamed in the order they
    /// open, each opening with the inline flag, and everything else left as written.
    #[test]
    fn a_named_group_is_renamed_to_its_positional_name() {
        // (case, the expression as a query writes it, the expression the compiler reads)
        let cases: [(&str, &str, &str); 7] = [
            ("no group at all", r"lat=\d+", r"lat=\d+"),
            (
                "one named group",
                r"lat=(?<lat>\d+)",
                r"lat=(?<capture000>(?s)\d+)",
            ),
            (
                "the Python spelling of the same group",
                r"lat=(?P<lat>\d+)",
                r"lat=(?<capture000>(?s)\d+)",
            ),
            (
                "two named groups, renamed in the order they open",
                r"(?<a>\w+) (?<b>\w+)",
                r"(?<capture000>(?s)\w+) (?<capture001>(?s)\w+)",
            ),
            (
                "an unnamed group, which names no field and is left as written",
                r"(\d+)-(?<tail>\w+)",
                r"(\d+)-(?<capture000>(?s)\w+)",
            ),
            (
                "an escaped parenthesis, which opens no group",
                r"\(?<a>\)",
                r"\(?<a>\)",
            ),
            (
                "a name the expression never closes, left for the parser to place",
                r"(?<lat\d+",
                r"(?<lat\d+",
            ),
        ];
        for (case, written, read) in cases {
            let rewritten = rewritten_expression(written);
            assert_eq!(
                rewritten, read,
                "case {case:?}: expected the compiler to read {read:?}, got {rewritten:?}"
            );
        }
    }

    /// One row of the table below: the expression, and the diagnostic's own account of the
    /// fault with the index it stopped at — `None` for an expression that compiles.
    type Row = (&'static str, &'static str, Option<(&'static str, usize)>);

    /// An expression that does not compile is placed and named; one that compiles is not.
    #[test]
    fn a_fault_is_named_and_placed_in_the_expression_the_compiler_reads() {
        // (case, the expression, the diagnostic's own account and index, or None when the
        // expression compiles)
        let cases: [Row; 9] = [
            ("an expression", r"lat=\d+", None),
            ("an expression with a named group", r"(?<lat>\d+)", None),
            // A look-around is a construct the expression engine reads.
            ("a look-ahead", r"lat=(?=\d)", None),
            (
                "a group the expression never closes, so the compiler reads to the end",
                r"lat=(?<lat>\d+",
                Some(("Unclosed group", 25)),
            ),
            (
                "a closing parenthesis nothing opened",
                r"lat=\d+)",
                Some(("Unmatched closing ')'", 7)),
            ),
            (
                "a character class the expression never closes",
                r"lat=[0-9",
                Some(("Unclosed character class", 7)),
            ),
            (
                "a quantifier with nothing to repeat",
                r"+",
                Some(("Dangling meta character '+'", 0)),
            ),
            (
                "a counted repetition the expression never closes",
                r"lat=\d{2",
                Some(("Unclosed counted closure", 6)),
            ),
            (
                "an escape the expression stops in the middle of",
                "lat=\\",
                Some(("Illegal/unsupported escape sequence", 4)),
            ),
        ];
        for (case, pattern, expected) in cases {
            let found = invalid_expression(pattern);
            match expected {
                None => assert!(
                    found.is_none(),
                    "case {case:?}: {pattern:?} is an expression, got {:?}",
                    found.map(|invalid| invalid.diagnostic())
                ),
                Some((description, index)) => {
                    let invalid = found.unwrap_or_else(|| {
                        panic!("case {case:?}: {pattern:?} does not compile, got no fault")
                    });
                    let rewritten = rewritten_expression(pattern);
                    let said = invalid.diagnostic();
                    let opening = format!(
                        "Invalid regular expression in query: '{rewritten}' — {description} near \
                         index {index}\n{rewritten}"
                    );
                    assert!(
                        said.starts_with(&opening),
                        "case {case:?}: expected {opening:?}, got {said:?}"
                    );
                    // The caret marks the index only when the index is inside the expression.
                    let characters = rewritten.chars().count();
                    assert_eq!(
                        said.len() > opening.len(),
                        index < characters,
                        "case {case:?}: the caret marks an index inside the expression, got \
                         {said:?}"
                    );
                }
            }
        }
    }
}
