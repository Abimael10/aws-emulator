// SPDX-License-Identifier: CC0-1.0

//! A declaration root carries no inner doc comment at all.
//!
//! Read against THIS crate's root only while its target keeps one: where the suites are
//! declared with `automod` from the vendored `tests/all/main.rs` instead, the rule has no
//! subject here, and the rule that an inner doc comment stands only ahead of a module's first
//! item is read for every root the workspace keeps, and for every suite file, by
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs`.
//!
//! An inner doc comment (`//!`, `/*!`) documents the module it stands in, so the language
//! accepts it only ahead of that module's first item: one written lower down is `E0753` and
//! compiles no suite of the target it is the root of. The sibling
//! [`super::all_rs_inner_doc_comments_precede_the_first_item`] reads that rule for every
//! declaration root in the workspace, and a root whose header is written with `//!` satisfies
//! it only as long as every landing keeps its own prose below the list in the other comment
//! style.
//!
//! This crate's own root is the one every cross-service landing used to append to, and it lost
//! that bet: its tail collected re-typed fragments of a `//!` header, past the first
//! declaration, until the whole target failed to compile and no suite in it ran — including the
//! siblings that pin the rule. What this suite pins is the stronger property that takes the bet
//! away for such a file: with no inner doc comment ANYWHERE in it, no appended, re-typed or
//! merged copy of a line of it can be one past the first item, because there is no such line to
//! copy.
//!
//! It is read for this crate's own root while its target keeps one; where its suites are
//! declared with `automod` instead, the rule has no subject here, and the workspace's hand-kept
//! roots cannot be held to it as written: each one's header IS an inner doc comment, which is
//! the weaker bet the sibling above reads them under.
//!
//! It reads one file; it starts no router and reaches no network.

use std::fs;

// The root this rule is about, read through the sibling that owns the reader rather than
// opened a second time, and the line classifier from the sibling that owns the rule about
// WHERE an inner doc comment may stand, so the two readings cannot drift apart.
use crate::all_rs_declares_each_suite_once::{all_rs, workspace_root};
use crate::all_rs_inner_doc_comments_precede_the_first_item::opens_an_inner_doc_comment;

/// One report per line of `source` that opens with an inner doc comment, whatever stands above
/// it, each naming the line it is on.
///
/// The reading is line-based, as the rule is: a line whose first non-whitespace characters are
/// `//!` or `/*!` is reported wherever it sits, header included, since this file holds nothing
/// but ordinary comments and declaration pairs and so can carry neither a nested module's own
/// doc comment nor a multi-line string literal.
fn inner_doc_comments(source: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| opens_an_inner_doc_comment(line.trim_start()))
        .map(|(index, line)| {
            format!(
                "line {}: expected an ordinary comment (`//`), got the inner doc comment {:?}",
                index + 1,
                line.trim_start()
            )
        })
        .collect()
}

/// How many `#[path = "all/<name>.rs"]` attributes `source` holds — the declarations a root
/// exists for, counted so that a file read as empty cannot satisfy the rule above by holding
/// nothing.
fn declaration_count(source: &str) -> usize {
    source
        .lines()
        .filter(|line| line.starts_with("#[path = \"all/"))
        .count()
}

/// How many `*.rs` files sit directly under this crate's `tests/all/` — the suites the root
/// owes a declaration each, counted so the assertion below is bounded by the directory rather
/// than by a number written here that a shrinking list would still satisfy.
fn suite_file_count() -> usize {
    let directory = workspace_root().join("core/router/tests/all");
    let entries = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|kind| kind == "rs"))
        .count()
}

#[test]
fn the_declaration_root_carries_no_inner_doc_comment() {
    // No subject where this crate's target declares its suites with `automod` instead of a
    // hand-maintained root; the workspace-wide siblings read this rule for every crate
    // that keeps one.
    let Some(source) = all_rs() else {
        return;
    };
    let reports = inner_doc_comments(&source);
    assert!(
        reports.is_empty(),
        "expected tests/all.rs to be written with ordinary comments throughout, so that no \
         appended or merged copy of one of its lines can be an inner doc comment past the \
         first item, got {} report(s): {reports:#?}",
        reports.len()
    );
    // Without this, a root that had lost its list — or most of it — would satisfy the rule
    // above by holding nothing to break it.
    let declared = declaration_count(&source);
    let on_disk = suite_file_count();
    assert_eq!(
        declared, on_disk,
        "expected tests/all.rs to carry one declaration per suite file under tests/all/, got \
         {declared} declaration(s) for {on_disk} file(s)"
    );
}

#[test]
fn the_reader_reports_every_inner_doc_comment_wherever_it_stands() {
    // One synthetic root per place and style a comment can take, with how many reports the
    // reader owes it. The rows differ in comment style (`//`, `//!`, `/*!`), in where the block
    // stands relative to the declarations (header, between two pairs, past the last one), in
    // how many lines it runs to, and in its indentation.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let cases: [(&str, String, usize); 10] = [
        ("an empty root", String::new(), 0),
        (
            "an ordinary header above the declarations",
            format!("{license}// The target.\n//\n// Its rules.\n{first}{second}"),
            0,
        ),
        (
            // Legal, and what the sibling rule allows; this suite reports it, because a header
            // in this style is what every re-typed fragment below the list was copied from.
            "an inner doc header above the declarations",
            format!("{license}//! The target.\n{first}{second}"),
            1,
        ),
        (
            "a three-line inner doc header",
            format!("{license}//! The target.\n//!\n//! Its rules.\n{first}"),
            3,
        ),
        (
            "an inner doc comment between two pairs",
            format!("{license}// The target.\n{first}//! A note.\n{second}"),
            1,
        ),
        (
            // The shape this file's tail had collected: fragments of its own header, appended
            // past the last declaration.
            "a header fragment appended past the last declaration",
            format!(
                "{license}//! The target.\n{first}{second}//! The target.\n//!\n//! Its rules.\n"
            ),
            4,
        ),
        (
            "the same fragment appended as ordinary comments",
            format!("{license}// The target.\n{first}{second}// The target.\n//\n// Its rules.\n"),
            0,
        ),
        (
            "an inner block doc comment past the last declaration",
            format!("{license}// The target.\n{first}/*! A note. */\n"),
            1,
        ),
        (
            "an indented inner doc comment",
            format!("{license}// The target.\n{first}    //! A note.\n"),
            1,
        ),
        (
            "an outer doc comment, which documents the item below it",
            format!("{license}// The target.\n/// A note.\n{first}"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = inner_doc_comments(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_counts_the_declarations_a_root_holds() {
    // Each shape a root can take, and the declarations it holds: the count is what keeps a file
    // read as empty from satisfying the rule by holding nothing at all.
    let cases: [(&str, &str, usize); 5] = [
        ("nothing", "", 0),
        ("a header only", "// The target.\n", 0),
        (
            "one pair",
            "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n",
            1,
        ),
        (
            "two pairs",
            "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n#[path = \"all/query_wire.rs\"]\nmod query_wire;\n",
            2,
        ),
        (
            "a path attribute named in prose rather than written in column one",
            "// Add a `#[path = \"all/<name>.rs\"]` attribute.\n",
            0,
        ),
    ];
    for (name, source, expected) in cases {
        let actual = declaration_count(source);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected {expected} declaration(s), got {actual}"
        );
    }
}
