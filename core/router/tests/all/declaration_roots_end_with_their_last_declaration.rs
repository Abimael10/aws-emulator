// SPDX-License-Identifier: CC0-1.0

//! Every `tests/all.rs` in the workspace ENDS with its last declaration: nothing is appended
//! past the list.
//!
//! A declaration root is the one file in an integration-test target every landing branch
//! writes to, and the end of that file is where a branch that does not insert into the sorted
//! list puts its lines instead. Prose appended there says nothing the header does not, it is
//! re-worded and mistyped by the next merge, and one copy written as an inner doc comment is
//! `E0753` — which compiles no suite of that target at all rather than failing one test.
//!
//! So the rule read here is the tail: past the last `#[path]`/`mod` pair a root holds no
//! further line. Prose BETWEEN the pairs is the sibling
//! [`super::all_rs_is_a_canonical_declaration_list`]'s rule; this one is read for every root
//! in the workspace, because the crate whose root grows such a tail cannot report it — a tail
//! that does not compile compiles no suite of that crate either.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::fs;

// The roots this rule holds for, and the reading of a `mod` item that takes it in every
// spelling the compiler does, come from the sibling that owns them rather than from second
// copies here.
use crate::all_rs_declares_each_suite_once::{
    declaration_name, declaration_roots, workspace_root,
};

/// Whether `line` is half of a declaration pair — the `#[path = "..."]` attribute in column
/// one, or the `mod <name>;` item under it in any spelling the sibling's reader takes.
///
/// A line that only mentions a declaration in prose is a comment, and a `mod` that does not
/// start in column one is nested in an item rather than declared by the root: neither is part
/// of the list, so neither ends it.
fn is_half_of_a_declaration(line: &str) -> bool {
    line.starts_with("#[path = \"") || declaration_name(line).is_some()
}

/// One report per non-blank line of `source` that stands past its last declaration, each
/// naming the line it is on and the declaration it was appended behind.
///
/// A file with no declaration at all has no tail to be past, and that it declares the suites
/// beside it is the sibling guards' rule, so such a file is reported nothing here.
fn lines_past_the_last_declaration(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let Some(last) = lines.iter().rposition(|&line| is_half_of_a_declaration(line)) else {
        return Vec::new();
    };
    lines[last + 1..]
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(offset, line)| {
            format!(
                "line {}: expected nothing past the declaration on line {}, got {line:?}",
                last + offset + 2,
                last + 1
            )
        })
        .collect()
}

#[test]
fn every_declaration_root_ends_with_its_last_declaration() {
    // The cases are the workspace's declaration roots: one per crate that roots its `all`
    // target in a `tests/all.rs`, differing in how many suites they declare, in how those
    // declarations are spelled and in how long a header they open with.
    let root = workspace_root();
    let roots = declaration_roots();
    // A floor rather than a census: every crate that migrates to the macro layout takes one
    // root out of this walk, so the number proves only that the walk reached the crates that
    // keep a hand-written root.
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate under {}, read {} of them",
        root.display(),
        roots.len()
    );

    let mut reports = Vec::new();
    for declaration_root in &roots {
        let case = declaration_root
            .strip_prefix(&root)
            .unwrap_or(declaration_root)
            .display()
            .to_string();
        let source = fs::read_to_string(declaration_root)
            .unwrap_or_else(|error| panic!("read {}: {error}", declaration_root.display()));
        for report in lines_past_the_last_declaration(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to hold nothing past its last declaration, \
         got {} report(s): {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_a_line_appended_past_the_last_declaration() {
    // One synthetic root per shape a tail can take, with how many reports the reader owes it.
    // The rows differ in what is appended (nothing, a comment, a block of them, an inner doc
    // comment, an inner attribute), in how many lines it is, in whether a blank line separates
    // it, in how the last declaration is spelled, and in whether prose sits between the pairs
    // instead of after them.
    let head = "// SPDX-License-Identifier: CC0-1.0\n\n// The target.\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let cases: [(&str, String, usize); 12] = [
        ("an empty root", String::new(), 0),
        ("a header and no declaration at all", head.to_string(), 0),
        ("one pair under a header", format!("{head}{first}"), 0),
        ("two pairs under a header", format!("{head}{first}{second}"), 0),
        (
            "two pairs and a trailing blank line",
            format!("{head}{first}{second}\n"),
            0,
        ),
        (
            // The shape this suite exists for: a landing appending its note to the end of the
            // root instead of inserting its pair into the list.
            "one appended comment",
            format!("{head}{first}{second}// Declared above, for the reason that entry records.\n"),
            1,
        ),
        (
            "an appended comment block",
            format!("{head}{first}// Declared here:\n// it spans two services,\n// so it lives here.\n"),
            3,
        ),
        (
            "an appended comment behind a blank line",
            format!("{head}{first}\n// A note about this file.\n"),
            1,
        ),
        (
            "an appended inner doc comment, which compiles no suite of the target",
            format!("{head}{first}{second}//! A note typed as a doc comment.\n"),
            1,
        ),
        (
            "an appended inner attribute, which compiles no suite of the target",
            format!("{head}{first}#![allow(clippy::doc_lazy_continuation)]\n"),
            1,
        ),
        (
            // Prose between the pairs is the canonical-list sibling's rule: the tail is clean.
            "prose between two pairs and nothing after the last one",
            format!("{head}{first}// Appended between the pairs.\n{second}"),
            0,
        ),
        (
            "a last declaration behind a visibility and a trailing comment, then a note",
            format!("{head}{first}#[path = \"all/query_wire.rs\"]\npub mod query_wire; // shared\n// A note.\n"),
            1,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = lines_past_the_last_declaration(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_classifies_the_lines_a_declaration_is_written_with() {
    // Each form a line of a declaration root can take, and whether it is half of a declaration
    // pair: the classifier is what the tail is measured from, so its vocabulary is stated here
    // rather than assumed. The spellings a `mod` item takes are the sibling reader's, read
    // here through the classifier that uses it.
    let cases: [(&str, &str, bool); 13] = [
        ("a path attribute", "#[path = \"all/boot_serve.rs\"]", true),
        ("a module declaration", "mod boot_serve;", true),
        (
            "a declaration carrying a trailing comment",
            "mod boot_serve; // the suite above",
            true,
        ),
        ("a declaration behind a visibility", "pub mod boot_serve;", true),
        (
            "a declaration behind a scoped visibility",
            "pub(crate) mod boot_serve;",
            true,
        ),
        ("an ordinary comment", "// The target.", false),
        ("an inner doc comment", "//! The target.", false),
        (
            "a comment naming a declaration",
            "// `mod boot_serve;` stands above.",
            false,
        ),
        (
            "a commented-out declaration",
            "// mod boot_serve;",
            false,
        ),
        ("an inner attribute", "#![allow(dead_code)]", false),
        ("a blank line", "", false),
        ("an indented declaration, nested in an item", "    mod inner;", false),
        (
            "a run-together `pubmod`, which the compiler rejects",
            "pubmod boot_serve;",
            false,
        ),
    ];
    for (name, line, expected) in cases {
        let actual = is_half_of_a_declaration(line);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected is_half_of_a_declaration({line:?}) to be {expected}, got \
             {actual}"
        );
    }
}
