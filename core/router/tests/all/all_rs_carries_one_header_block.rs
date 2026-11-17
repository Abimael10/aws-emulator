// SPDX-License-Identifier: CC0-1.0

//! ONE header block: no line of a declaration root's header is written twice.
//!
//! Read against THIS crate's root only while its target keeps one: where the suites are
//! declared with `automod` from the vendored `tests/all/main.rs` instead, the rule has no
//! subject here and the workspace-wide siblings read it for every crate that does keep a
//! hand-maintained root.
//!
//! A declaration root is appended to by every landing branch, and the sibling guards read the
//! declaration LIST: that it holds nothing but pairs, that they are sorted, that each suite is
//! declared once, and that nothing stands past the last of them. The header ahead of the first
//! item is the one region all of them skip, because that is where the module's own prose
//! belongs — and so it is where re-typed copies of that prose accumulate instead, each landing
//! adding its own wording of the same paragraph until the block says the same thing six times
//! and half the lines begin mid-sentence.
//!
//! The rule read here is that the block is written once: no line of the header repeats an
//! earlier line of it. Exact repetition is what a re-typed or merged copy of a paragraph
//! leaves behind, and it is a rule a reader can answer from the file alone. Paragraph
//! separators (a blank line, or a comment with nothing after the slashes) carry no prose, so
//! they may repeat; everything else, an inner attribute included, is owed one place.
//!
//! It is read for this crate's own root while its target keeps one; where its suites are
//! declared with `automod` instead, the rule has no subject here, and the roots the workspace
//! keeps by hand are read against it — through this reader — by
//! [`super::declaration_roots_carry_no_duplicated_comment_line`].
//!
//! It reads one file; it starts no router and reaches no network.

// Where a root's header ends comes from the sibling that owns that boundary for the other
// inner construct, so no two suites of this target can disagree about it.
use crate::all_rs_inner_doc_comments_precede_the_first_item::first_item_line;

/// The header of `source`: every line ahead of its first item.
///
/// A root with no item at all is all header, which is what the sibling guards report; this
/// reader answers it like any other.
///
/// Shared with the sibling that reads this rule for the declaration roots on disk, so the two
/// cannot disagree about which lines are a root's header.
pub(crate) fn header_of(source: &str) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    match first_item_line(source) {
        Some(line) => lines[..line - 1].to_vec(),
        None => lines,
    }
}

/// The prose `line` carries: its text with the comment marker — `//`, or the `//!` spelling a
/// service crate's root is written in — and the surrounding whitespace removed. `None` is a line
/// that says nothing: a blank one, or a marker with nothing after it, which separates two
/// paragraphs rather than stating anything.
///
/// A line that is no comment at all carries its own text, since an inner attribute standing in a
/// header is owed one place in the block like any other line; a reader that asks instead for the
/// prose a PARAGRAPH is written in filters that line out itself.
///
/// Shared with the sibling that reads the other rule of a header block from the same lines, so
/// the two cannot disagree about which of them say something.
pub(crate) fn prose_of(line: &str) -> Option<&str> {
    let marked = line.trim();
    let prose = match marked.strip_prefix("//!").or_else(|| marked.strip_prefix("//")) {
        Some(comment) => comment.trim(),
        None => marked,
    };
    (!prose.is_empty()).then_some(prose)
}

/// One report per header line of `source` that repeats an earlier header line, naming the line
/// it is on and the line it repeats.
///
/// A line carrying no prose — empty, or a comment with nothing after its slashes and, in the
/// inner-doc spelling a service crate's root uses, the `!` that follows them — separates
/// paragraphs rather than saying anything, so it is passed over; every other line, an inner
/// attribute included, is owed exactly one place in the block.
///
/// Shared with the sibling that reads the same rule for the declaration roots on disk rather
/// than copied there, and with [`super::all_rs_holds_no_line_twice`], which reads the other
/// half of a root — the LIST, where a copy of a header line lands when a branch appends it
/// instead of inserting its pair — through this reader rather than a second copy of it.
pub(crate) fn repeated_header_lines(source: &str) -> Vec<String> {
    let mut seen: Vec<(usize, &str)> = Vec::new();
    let mut reports = Vec::new();
    for (index, line) in header_of(source).into_iter().enumerate() {
        let text = line.trim_end();
        if prose_of(line).is_none() {
            continue;
        }
        match seen.iter().find(|(_, earlier)| *earlier == text) {
            Some((first, _)) => reports.push(format!(
                "line {}: expected the header to hold {text:?} once, got it again here, having \
                 already read it on line {}",
                index + 1,
                first + 1
            )),
            None => seen.push((index, text)),
        }
    }
    reports
}

#[test]
fn the_reader_reports_a_header_line_written_twice() {
    // One synthetic root per shape a header can take, with how many reports the reader owes it.
    // The rows differ in how long the block is, in whether a repeat is one line or a whole
    // paragraph, in whether the repeated line is prose, a separator or an inner attribute, and
    // in whether the repetition stands in the header at all or below the first declaration.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let paragraph = "// The target, and what belongs in it.\n// Put a new suite beside it.\n";
    let list = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let cases: [(&str, String, usize); 10] = [
        ("a root with no header at all", list.to_string(), 0),
        ("one header block", format!("{license}{paragraph}{list}"), 0),
        (
            "one header block and an inner attribute",
            format!("{license}{paragraph}#![allow(dead_code)]\n{list}"),
            0,
        ),
        (
            "two paragraphs split by a separator comment",
            format!("{license}{paragraph}//\n// A second paragraph, on its own.\n{list}"),
            0,
        ),
        (
            // The shape this suite exists for: a landing re-typing the paragraph that is
            // already there instead of leaving the block alone.
            "the same paragraph written twice",
            format!("{license}{paragraph}{paragraph}{list}"),
            2,
        ),
        (
            "one line of a paragraph written twice",
            format!("{license}{paragraph}// Put a new suite beside it.\n{list}"),
            1,
        ),
        (
            "the same inner attribute written twice",
            format!("{license}{paragraph}#![allow(dead_code)]\n#![allow(dead_code)]\n{list}"),
            1,
        ),
        (
            // Two separators and two blank lines say nothing, so neither is a repeat.
            "separators and blank lines repeated",
            format!("{license}//\n{paragraph}//\n\n{list}"),
            0,
        ),
        (
            // The same, in the inner-doc spelling every service crate's root is written in:
            // `//!` with nothing after it separates two paragraphs of that header.
            "inner-doc separators repeated",
            format!(
                "{license}//! The target.\n//!\n//! Put a new suite beside it.\n//!\n{list}"
            ),
            0,
        ),
        (
            // Past the first declaration the line belongs to the LIST, which is the canonical
            // -list sibling's rule, not this one's.
            "a header line repeated below the first declaration",
            format!("{license}{paragraph}{list}// Put a new suite beside it.\n"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = repeated_header_lines(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_reads_the_header_up_to_the_first_item() {
    // Each shape a root can take, and how many lines of it are header: the block this rule is
    // about is measured from the first item, so where that boundary falls — the sibling's
    // reading of it — is stated here rather than assumed.
    let cases: [(&str, &str, usize); 7] = [
        ("an empty root", "", 0),
        ("a header and nothing else", "// The target.\n// Its rules.\n", 2),
        (
            "a header then one pair",
            "// The target.\n#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            1,
        ),
        (
            "a pair with no header at all",
            "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            0,
        ),
        (
            "an inner attribute inside the header",
            "// The target.\n#![allow(dead_code)]\n#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            2,
        ),
        (
            // A comment below the list is NOT header: the header ends at the first item,
            // wherever prose stands afterwards.
            "prose below the first pair",
            "// The target.\n#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n// A note.\n",
            1,
        ),
        (
            // Any item ends the header, not only a `#[path]` attribute: the boundary is the
            // sibling's, which the compiler agrees with.
            "a declaration with no attribute above it",
            "// The target.\nmod boot_serve;\n",
            1,
        ),
    ];
    for (name, source, expected) in cases {
        let header = header_of(source);
        assert_eq!(
            header.len(),
            expected,
            "case {name:?}: expected {expected} header line(s), got {header:?}"
        );
    }
}
