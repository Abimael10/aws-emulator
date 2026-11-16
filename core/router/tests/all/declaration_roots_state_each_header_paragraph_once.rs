// SPDX-License-Identifier: CC0-1.0

//! Every declaration root's header states each PARAGRAPH once, however that copy is wrapped.
//!
//! A declaration root is appended to by every landing branch, and the header ahead of its first
//! item is the one region the list guards skip, so it is where a landing's re-typed copy of the
//! module's prose accumulates. [`super::all_rs_carries_one_header_block`] reads that damage one
//! LINE at a time, for this crate's root, and
//! [`super::declaration_roots_carry_no_duplicated_comment_line`] reads the same lines for the
//! other roots: a copy is found when a line of it is written twice, character for character.
//!
//! A copy re-wrapped on its way in — the same sentences broken at another column, or re-indented
//! — shares no line with the paragraph it duplicates, so a line reader passes it and the block
//! goes on saying the same thing twice. The rule read here is the one both readings serve, one
//! level up from a line: a paragraph of a root's header stands in one place, where a paragraph
//! is the run of prose lines between two separators and its text is those lines joined, so the
//! column it happens to be wrapped at does not change what it says. A separator (a blank line,
//! a comment with nothing after its slashes) carries no prose and an inner attribute is no
//! sentence of the block, so each of those breaks a paragraph rather than joining one.
//!
//! It is read for EVERY root in the workspace, this crate's own included, from the one target
//! that links them all — the crate whose root breaks the rule cannot report it, because its own
//! `all` target is what stops compiling.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::fs;

// The roots this rule is read for, where each one's header ends, and which of its lines
// contribute prose to a paragraph, all taken from the siblings that own them rather than
// decided a second time here.
use crate::all_rs_carries_one_header_block::header_of;
use crate::all_rs_declares_each_suite_once::{declaration_roots, workspace_root};
use crate::all_rs_header_holds_no_sentence_fragment::paragraph_prose_of;

/// One paragraph of a header as the reader below reads it: the line it opens on, and the prose
/// it states with its wrapping gone.
type Paragraph<'a> = (usize, &'a str);

/// The paragraphs of `source`'s header: each the line it opens on and the prose it states, its
/// lines joined by single spaces so that the same sentences wrapped at another column read as
/// the same paragraph.
///
/// A line contributing no prose to a paragraph — a blank line, a separator comment, or the inner
/// attribute that closes a header — ends the paragraph above it and opens none.
fn header_paragraphs(source: &str) -> Vec<(usize, String)> {
    let mut paragraphs: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (index, line) in header_of(source).into_iter().enumerate() {
        match paragraph_prose_of(line) {
            Some(prose) => match &mut open {
                Some((_, text)) => {
                    text.push(' ');
                    text.push_str(prose);
                }
                None => open = Some((index + 1, prose.to_string())),
            },
            None => {
                if let Some(paragraph) = open.take() {
                    paragraphs.push(paragraph);
                }
            }
        }
    }
    paragraphs.extend(open);
    paragraphs
}

/// One report per header paragraph of `source` that states what an earlier paragraph of the same
/// header already states, naming the line each of them opens on.
fn repeated_header_paragraphs(source: &str) -> Vec<String> {
    let mut seen: Vec<(usize, String)> = Vec::new();
    let mut reports = Vec::new();
    for (line, text) in header_paragraphs(source) {
        match seen.iter().find(|(_, earlier)| *earlier == text) {
            Some((first, _)) => reports.push(format!(
                "line {line}: expected the header to state {text:?} once, got it again here, \
                 having already read it on line {first}"
            )),
            None => seen.push((line, text)),
        }
    }
    reports
}

#[test]
fn every_declaration_root_states_each_header_paragraph_once() {
    // The cases are every declaration root in the workspace: they differ in how long the header
    // block is, in whether it is written as ordinary comments or as an inner doc comment, in how
    // many paragraphs it holds, in whether it closes with an inner attribute, and in how many
    // suites they declare.
    let roots = declaration_roots();
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate plus this crate's own under {}, read {} of \
         them",
        workspace_root().display(),
        roots.len()
    );

    let mut read = 0_usize;
    let mut reports = Vec::new();
    for root in &roots {
        let case = root
            .strip_prefix(workspace_root())
            .unwrap_or(root)
            .display()
            .to_string();
        let source =
            fs::read_to_string(root).unwrap_or_else(|error| panic!("read {case}: {error}"));
        read += header_paragraphs(&source).len();
        for report in repeated_header_paragraphs(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    // Without this, roots read as holding no header at all would satisfy the rule by having
    // nothing to break it.
    assert!(
        read > 100,
        "expected the {} declaration roots to open with their header blocks, read {read} \
         paragraph(s) across all of them",
        roots.len()
    );
    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to state every header paragraph once, got {} \
         report(s), each a paragraph the next landing has two places to keep: {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_a_header_paragraph_stated_twice() {
    // One synthetic root per shape a header block can take, with how many reports the reader
    // owes it. The rows differ in how many paragraphs the block holds, in whether a copy is
    // wrapped as the original is or at another column, in whether it is re-indented, in what
    // separates the two copies, and in whether the copy stands in the header at all.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let paragraph = "// The target, and what belongs in it. A new suite goes under\n\
                     // `tests/all/`, declared below by one pair.\n";
    let rewrapped = "// The target, and what belongs in it. A new suite\n\
                     // goes under `tests/all/`, declared below by\n\
                     // one pair.\n";
    let reindented = "//   The target, and what belongs in it. A new suite goes under\n\
                      //  `tests/all/`, declared below by one pair.\n";
    let other = "// Each suite of this target drives the assembled router.\n";
    let list = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let cases: [(&str, String, usize); 12] = [
        ("a root with no header at all", list.to_string(), 0),
        ("one paragraph", format!("{license}{paragraph}{list}"), 0),
        (
            "two paragraphs that say different things",
            format!("{license}{paragraph}//\n{other}{list}"),
            0,
        ),
        (
            "three paragraphs that say different things",
            format!("{license}{paragraph}//\n{other}//\n// A third, on its own.\n{list}"),
            0,
        ),
        (
            // The shape the line-reading siblings already catch, read here as one paragraph
            // rather than as two lines.
            "the same paragraph written twice, wrapped as the first is",
            format!("{license}{paragraph}//\n{paragraph}{list}"),
            1,
        ),
        (
            // The shape this suite exists for: a copy broken at another column shares no line
            // with the paragraph it duplicates, so a line reader passes it.
            "the same paragraph re-wrapped at another column",
            format!("{license}{paragraph}//\n{rewrapped}{list}"),
            1,
        ),
        (
            "the same paragraph re-indented inside its comments",
            format!("{license}{paragraph}//\n{reindented}{list}"),
            1,
        ),
        (
            "a re-wrapped copy separated by a blank line rather than a separator comment",
            format!("{license}{paragraph}\n{rewrapped}{list}"),
            1,
        ),
        (
            "a re-wrapped copy separated by the inner attribute",
            format!("{license}{paragraph}#![allow(dead_code)]\n{rewrapped}{list}"),
            1,
        ),
        (
            "a paragraph re-wrapped twice over",
            format!("{license}{paragraph}//\n{rewrapped}//\n{reindented}{list}"),
            2,
        ),
        (
            // Two copies with no separator between them are ONE paragraph: it says its
            // sentences twice, which is a line repeat for the siblings and no paragraph
            // repeat here.
            "a copy run together with the paragraph above it",
            format!("{license}{paragraph}{rewrapped}{list}"),
            0,
        ),
        (
            // Past the first declaration the lines belong to the LIST, whose own rules the
            // canonical-list siblings read.
            "a copy below the first declaration",
            format!("{license}{paragraph}{list}//\n{rewrapped}"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = repeated_header_paragraphs(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_reads_the_paragraphs_a_header_holds() {
    // Each shape a header block can take, and the paragraphs read from it: the line each opens
    // on, and the prose it states with its wrapping gone. The rows differ in what separates two
    // paragraphs, in whether the block is written as ordinary comments or in the inner-doc
    // spelling a service crate's root uses, and in where the first item stands.
    let cases: [(&str, &str, &[Paragraph<'_>]); 9] = [
        ("an empty root", "", &[]),
        (
            "a pair with no header at all",
            "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            &[],
        ),
        (
            "one wrapped paragraph",
            "// The target. A suite goes\n// under `tests/all/`.\n#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            &[(1, "The target. A suite goes under `tests/all/`.")],
        ),
        (
            "a paragraph under the license line",
            "// SPDX-License-Identifier: CC0-1.0\n\n// The target.\nmod boot_serve;\n",
            &[(1, "SPDX-License-Identifier: CC0-1.0"), (3, "The target.")],
        ),
        (
            "two paragraphs split by a separator comment",
            "// The target.\n//\n// Its rules.\nmod boot_serve;\n",
            &[(1, "The target."), (3, "Its rules.")],
        ),
        (
            "two paragraphs split by a blank line",
            "// The target.\n\n// Its rules.\nmod boot_serve;\n",
            &[(1, "The target."), (3, "Its rules.")],
        ),
        (
            "two paragraphs split by the inner attribute",
            "// The target.\n#![allow(dead_code)]\n// Its rules.\nmod boot_serve;\n",
            &[(1, "The target."), (3, "Its rules.")],
        ),
        (
            // The inner-doc spelling every service crate's root is written in: `//!` with
            // nothing after it separates two paragraphs of that header.
            "an inner-doc header",
            "//! The target.\n//!\n//! Its rules, which\n//! wrap.\nmod boot_serve;\n",
            &[(1, "The target."), (3, "Its rules, which wrap.")],
        ),
        (
            // A comment below the first item is no header line, so it opens no paragraph.
            "prose below the first declaration",
            "// The target.\nmod boot_serve;\n// A note.\n",
            &[(1, "The target.")],
        ),
    ];
    for (name, source, expected) in cases {
        let paragraphs = header_paragraphs(source);
        let read: Vec<Paragraph<'_>> = paragraphs
            .iter()
            .map(|(line, text)| (*line, text.as_str()))
            .collect();
        assert_eq!(
            read, expected,
            "case {name:?}: expected the paragraphs {expected:?}, got {read:?}"
        );
    }
}
