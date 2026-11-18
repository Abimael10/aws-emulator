// SPDX-License-Identifier: CC0-1.0

//! Every declaration root opens each header sentence it writes.
//!
//! The sibling [`super::all_rs_header_holds_no_sentence_fragment`] owns the READING of that
//! rule: the header ahead of the first item is the region every list guard skips, so it is
//! where a landing's re-worded copy of the module's prose accumulates, and a merge of two such
//! wordings leaves the tail of one paragraph standing after the end of another — a line that
//! begins in the middle of a sentence no line of the file began. Each line of such a block is
//! unique, so the readers that compare lines or whole paragraphs pass it, and the block goes on
//! saying the same thing twice.
//!
//! The rule is a property of a declaration ROOT rather than of one file: a service crate's root
//! is appended to by exactly the same branches, carries the same kind of header, and loses its
//! whole target to the same damage. So it is read here for every root of the workspace, from
//! the one target that links them all — the crate whose root breaks the rule cannot report it,
//! because its own `all` target is what stops running.
//!
//! No shape of the list beneath that header changes the reading: the reader takes the block
//! from [`super::all_rs_carries_one_header_block::header_of`], which ends at the first item, so
//! a note a root keeps among its declarations states no sentence of the block this rule is
//! about.
//!
//! The reader, the roots, and which of their lines contribute prose to a paragraph are the
//! siblings' own, so one rule keeps one reading.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::fs;

use crate::all_rs_carries_one_header_block::header_of;
use crate::all_rs_declares_each_suite_once::{declaration_roots, relative, workspace_root};
use crate::all_rs_header_holds_no_sentence_fragment::{
    header_lines_beginning_mid_sentence, paragraph_prose_of,
};

/// How many lines of `source`'s header carry prose, counted — through the same two readers the
/// rule itself is read with — so that roots read as holding no header at all cannot satisfy the
/// rule below by having nothing to break it.
fn header_prose_lines(source: &str) -> usize {
    header_of(source)
        .into_iter()
        .filter(|line| paragraph_prose_of(line).is_some())
        .count()
}

#[test]
fn every_declaration_root_opens_each_header_sentence() {
    // The cases are every declaration root in the workspace: they differ in how long their
    // header block is, in whether it is written as an inner doc comment or as ordinary
    // comments, in how many paragraphs it holds, in whether it closes with an inner attribute,
    // in how many suites they declare, and in whether they keep a note among those
    // declarations.
    let roots = declaration_roots();
    // A floor rather than a census: every crate that migrates to the macro layout takes one
    // root out of this walk, so the number proves only that the walk reached the crates that
    // hand-declare their suites.
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per crate that hand-declares its suites under {}, read {} of \
         them",
        workspace_root().display(),
        roots.len()
    );

    let mut prose = 0_usize;
    let mut reports = Vec::new();
    for root in &roots {
        let source = fs::read_to_string(root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        prose += header_prose_lines(&source);
        let case = relative(root);
        for report in header_lines_beginning_mid_sentence(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    // Without this, roots read as holding no header at all would satisfy the report below by
    // having nothing to break it.
    assert!(
        prose > 100,
        "expected the {} declaration roots to open with their header blocks, read {prose} \
         line(s) of prose across all of them",
        roots.len()
    );
    assert!(
        reports.is_empty(),
        "expected every header line of the {} declaration roots to open the sentence it starts, \
         got {} report(s), each a paragraph left half-written: {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_a_fragment_in_a_root_of_any_spelling() {
    // One synthetic root per shape another crate's header can take, with how many reports the
    // reader owes it. The sibling's own rows are written as ordinary comments; these are the
    // inner-doc spelling a service crate's root uses, and they differ in how many paragraphs
    // the block holds, in what a fragment follows (a full stop, a separator, nothing), in what
    // the line opens with (a word, a path, a bracket), and in whether the fragment stands in
    // the header at all.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let paragraph = "//! Single integration-test target for this crate. Put a new suite at\n\
                     //! `tests/all/<name>.rs` and declare it below, keeping the list sorted.\n";
    let list = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let cases: [(&str, String, usize); 10] = [
        ("a root with no header at all", list.to_string(), 0),
        (
            "one wrapped inner-doc paragraph",
            format!("{license}{paragraph}{list}"),
            0,
        ),
        (
            "two inner-doc paragraphs split by a marker-only separator",
            format!("{license}{paragraph}//!\n//! A suite left under `tests/` fails the build.\n{list}"),
            0,
        ),
        (
            "a paragraph opening with an inline-code path",
            format!("{license}{paragraph}//!\n//! `tests/all.rs` is a test-target crate root.\n{list}"),
            0,
        ),
        (
            "a paragraph opening with a bracket",
            format!("{license}{paragraph}//!\n//! (The suites below drive this crate's ops.)\n{list}"),
            0,
        ),
        (
            // The shape this suite exists for, in the spelling these roots are written in: a
            // merge of two re-wordings leaves the middle of one paragraph after the end of
            // another.
            "a fragment after a line that ended a sentence",
            format!("{license}{paragraph}//! declare it below, keeping the list sorted.\n{list}"),
            1,
        ),
        (
            "two fragments in a row",
            format!(
                "{license}{paragraph}//! declare it below, keeping the list sorted.\n\
                 //! sorted, so two branches landing at once insert two lines.\n{list}"
            ),
            2,
        ),
        (
            "a fragment opening a paragraph after a separator",
            format!("{license}{paragraph}//!\n//! and its tests never run.\n{list}"),
            1,
        ),
        (
            "a fragment opening the whole block",
            format!("{license}//! and its tests never run.\n{list}"),
            1,
        ),
        (
            // Past the first declaration a line belongs to the LIST, whose own rules the
            // canonical-list siblings read.
            "a fragment below the first declaration",
            format!("{license}{paragraph}{list}//! and its tests never run.\n"),
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
