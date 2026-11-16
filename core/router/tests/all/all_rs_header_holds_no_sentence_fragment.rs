// SPDX-License-Identifier: CC0-1.0

//! No line of the router's `tests/all.rs` header begins in the middle of a sentence.
//!
//! The header ahead of the first declaration is the one region every list guard skips, so it
//! is where each landing's re-typed copy of the module's prose accumulates. The sibling
//! [`super::all_rs_carries_one_header_block`] reads the half of that damage a reader can see
//! by comparison: a line written twice, character for character. The other half is what a
//! merge of two re-wordings leaves behind — the tail of one paragraph followed by the middle
//! of another, each line unique and so invisible to an exact-repeat reader, and together a
//! block that says the same thing several times in sentences that never finish.
//!
//! The rule read here is that the block is PROSE: a header line that starts a new sentence —
//! the first line of a paragraph, or one following a line that ended a sentence — opens that
//! sentence, rather than continuing one no line of the file began. A line wrapping a sentence
//! its predecessor left open is how prose is written and is passed over; it is the line
//! standing after a full stop and opening lower-case that only a cut-and-merged paragraph
//! produces. Paragraph separators (a blank line, a comment with nothing after its slashes) and
//! the inner attribute that closes the header contribute no sentence, so they only break a
//! paragraph.
//!
//! It reads one file; it starts no router and reaches no network.

// The declaration root this rule is about, the header boundary it is read against, and which
// lines of that header say something, all taken from the siblings that own them rather than
// opened, decided or read a second time.
use crate::all_rs_carries_one_header_block::{header_of, prose_of};
use crate::all_rs_declares_each_suite_once::all_rs;

/// The prose `line` contributes to the paragraph it stands in, or `None` for a line that
/// contributes none and so breaks the paragraph: a blank line, a marker with nothing after it,
/// or the inner attribute that closes a header, which is a line of the block but no sentence of
/// it.
fn paragraph_prose_of(line: &str) -> Option<&str> {
    prose_of(line).filter(|_| line.trim_start().starts_with("//"))
}

/// Whether `prose` ends a sentence, so that the line below it has to open the next one.
///
/// A full stop closing a one-letter word closes an abbreviation (`e.g.`) or a span of inline
/// code (`` `all.rs`. ``) rather than a sentence, and prose wraps after either, so the stop has
/// to follow a word of its own.
fn ends_a_sentence(prose: &str) -> bool {
    match prose.chars().next_back() {
        Some('!' | '?') => true,
        Some('.') => {
            let stem = &prose[..prose.len() - 1];
            stem.chars()
                .rev()
                .take_while(|character| character.is_alphanumeric())
                .count()
                > 1
        }
        _ => false,
    }
}

/// Whether `prose` opens a sentence: it starts with a capital, a digit, or one of the
/// characters a sentence can open with ahead of its first word — a bracket, a quote, or the
/// backtick of an inline-code span, which is how a paragraph naming a path begins.
fn opens_a_sentence(prose: &str) -> bool {
    prose.chars().next().is_some_and(|first| {
        first.is_uppercase() || first.is_ascii_digit() || "`([\"'".contains(first)
    })
}

/// One report per header line of `source` that begins in the middle of a sentence: a line
/// which opens a paragraph, or follows a line that ended a sentence, and does not open a
/// sentence of its own.
fn header_lines_beginning_mid_sentence(source: &str) -> Vec<String> {
    let mut reports = Vec::new();
    let mut previous: Option<&str> = None;
    for (index, line) in header_of(source).into_iter().enumerate() {
        let Some(prose) = paragraph_prose_of(line) else {
            previous = None;
            continue;
        };
        if previous.is_none_or(ends_a_sentence) && !opens_a_sentence(prose) {
            reports.push(format!(
                "line {}: expected the header line {prose:?} to open the sentence it starts, \
                 got one continuing a sentence no earlier line of the block began",
                index + 1
            ));
        }
        previous = Some(prose);
    }
    reports
}

/// How many header lines of `source` carry prose, counted so that a file read as having no
/// header cannot satisfy the rule by holding nothing to break it.
fn header_prose_lines(source: &str) -> usize {
    header_of(source)
        .into_iter()
        .filter(|line| paragraph_prose_of(line).is_some())
        .count()
}

#[test]
fn the_declaration_root_holds_no_header_fragment() {
    let source = all_rs();
    let reports = header_lines_beginning_mid_sentence(&source);
    assert!(
        reports.is_empty(),
        "expected every line of the tests/all.rs header to open the sentence it starts, got \
         {} report(s), each a paragraph left half-written: {reports:#?}",
        reports.len()
    );
    let prose = header_prose_lines(&source);
    assert!(
        prose > 10,
        "expected tests/all.rs to open with the header block this rule is read for, read \
         {prose} line(s) of prose in it"
    );
}

#[test]
fn the_reader_reports_a_header_line_that_begins_mid_sentence() {
    // One synthetic root per shape a header block can take, with how many reports the reader
    // owes it. The rows differ in how many paragraphs the block holds, in how a line relates
    // to the one above it (a wrap, a new sentence, a fragment), in what the fragment follows
    // (a full stop, a question mark, an abbreviation, a separator), in what the line opens with
    // (a word, a path, a number), and in whether the line stands in the header at all.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let paragraph = "// The target, and what belongs in it. A new suite goes under\n\
                     // `tests/all/`, declared below by one pair, in the place its name\n\
                     // sorts to.\n";
    let list = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let cases: [(&str, String, usize); 14] = [
        ("a root with no header at all", list.to_string(), 0),
        ("one wrapped paragraph", format!("{license}{paragraph}{list}"), 0),
        (
            "two paragraphs split by a separator",
            format!("{license}{paragraph}//\n// A second paragraph, on its own.\n{list}"),
            0,
        ),
        (
            "a paragraph opening with an inline-code path",
            format!("{license}{paragraph}//\n// `all/boot_serve.rs` is declared below.\n{list}"),
            0,
        ),
        (
            "a paragraph opening with a number",
            format!("{license}{paragraph}//\n// 632 suites are declared below.\n{list}"),
            0,
        ),
        (
            "a sentence beginning mid-line and wrapping",
            format!(
                "{license}// The target. Put a new suite under `tests/all/` and declare\n\
                 // it below.\n{list}"
            ),
            0,
        ),
        (
            // The shape this suite exists for: a merge of two re-wordings, which leaves the
            // middle of one paragraph standing after the end of another.
            "a fragment after a line that ended a sentence",
            format!("{license}{paragraph}// it, and then one pair per file under `tests/all/`.\n{list}"),
            1,
        ),
        (
            "three fragments in a row",
            format!(
                "{license}{paragraph}// it, and then one pair per file.\n\
                 // suite.\n// what points the declaration into `tests/all/`.\n{list}"
            ),
            3,
        ),
        (
            "a fragment opening a paragraph after a separator",
            format!("{license}{paragraph}//\n// and nothing after the last pair.\n{list}"),
            1,
        ),
        (
            "a fragment opening the whole block",
            format!("{license}// and nothing after the last pair.\n{list}"),
            1,
        ),
        (
            "a fragment after a question mark",
            format!(
                "{license}// Which suite declares a file under `tests/all/`?\n\
                 // one of the pairs below, in the place its name sorts to.\n{list}"
            ),
            1,
        ),
        (
            "a wrap after a line ending in an abbreviation",
            format!(
                "{license}// A harness or a probe is declared below too, e.g.\n\
                 // the one `all/boot_serve.rs` holds.\n{list}"
            ),
            0,
        ),
        (
            "a wrap after a line ending in an inline-code path",
            format!(
                "{license}// Every suite of this target is declared in `tests/all.rs`.\n\
                 // the file this rule is read for.\n{list}"
            ),
            0,
        ),
        (
            // Past the first declaration the line belongs to the LIST, whose own rule the
            // canonical-list sibling reads.
            "a fragment below the first declaration",
            format!("{license}{paragraph}{list}// it, and then one pair per file.\n"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = header_lines_beginning_mid_sentence(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_reads_the_prose_a_header_line_contributes() {
    // Each kind of line a header holds, and the prose it says: a line carrying none breaks a
    // paragraph rather than opening or continuing one, which is what lets a separator or the
    // inner attribute stand between two paragraphs.
    let cases: [(&str, &str, Option<&str>); 8] = [
        ("a blank line", "", None),
        ("a separator comment", "//", None),
        ("a separator comment with a space", "//  ", None),
        ("an inner-doc separator", "//!", None),
        ("the inner attribute", "#![allow(clippy::doc_lazy_continuation)]", None),
        ("a plain comment", "// The target.", Some("The target.")),
        ("an inner doc comment", "//! The target.", Some("The target.")),
        (
            "a comment with trailing whitespace",
            "// The target.  ",
            Some("The target."),
        ),
    ];
    for (name, line, expected) in cases {
        let actual = paragraph_prose_of(line);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected the prose {expected:?}, got {actual:?}"
        );
    }
}
