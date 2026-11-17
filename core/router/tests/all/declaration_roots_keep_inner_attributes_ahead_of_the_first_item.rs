// SPDX-License-Identifier: CC0-1.0

//! Every `tests/all.rs` in the workspace keeps its inner attributes ahead of its first item.
//!
//! An inner attribute (`#![...]`) applies to the module it is written in, so the language
//! accepts it only before that module's first item: one written lower down is a hard parse
//! error and the whole integration-test target fails to compile, taking every suite in it
//! down with it. That is the same breakage the sibling
//! [`super::all_rs_inner_doc_comments_precede_the_first_item`] reads for the other inner
//! construct, and a declaration root is where both land: every branch appends to that one
//! file, so a lint allowance or a header line it carries is a line a merge can leave below
//! the list — where it compiles no suite of that target rather than failing one test.
//!
//! The rule is about a FILE, so it is read for every declaration root in the workspace: each
//! crate that roots its one `all` target in a `tests/all.rs`. The reader is line-based, like
//! the rule, and shares the sibling's reading of where a file's header ends so the two cannot
//! disagree about which lines are past the first item.
//!
//! It reads files only; it starts no router and reaches no network.

use std::fs;

// The roots this rule holds for, and the header boundary it is read against, come from the
// siblings that own them rather than from second copies here.
use crate::all_rs_declares_each_suite_once::{declaration_roots, workspace_root};
use crate::all_rs_inner_doc_comments_precede_the_first_item::first_item_line;

/// Whether `line` opens an inner attribute — the construct this rule is about, in the one
/// spelling the language has for it.
fn opens_an_inner_attribute(line: &str) -> bool {
    line.starts_with("#![")
}

/// One report per line of `source` that opens an inner attribute after the first item, each
/// naming the line it is on and the line that item starts on.
///
/// A declaration root holds nothing but comments and `#[path]`/`mod` pairs, so an indented
/// inner attribute is reported too: there is no later item in such a file for one to belong
/// to legally.
fn misplaced_inner_attributes(source: &str) -> Vec<String> {
    let Some(item_line) = first_item_line(source) else {
        return Vec::new();
    };
    source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim_start();
            (opens_an_inner_attribute(line) && index + 1 > item_line).then(|| {
                format!(
                    "line {}: expected the inner attribute {line:?} to stand ahead of the item \
                     starting on line {item_line}, got it below",
                    index + 1
                )
            })
        })
        .collect()
}

#[test]
fn every_declaration_root_keeps_its_inner_attributes_ahead_of_its_first_item() {
    let root = workspace_root();
    let roots = declaration_roots();
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
        for report in misplaced_inner_attributes(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to carry no inner attribute after its \
         first item, got {} report(s), each of which compiles no suite of that target: \
         {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_an_inner_attribute_the_target_cannot_compile() {
    // One synthetic declaration root per place an inner attribute can land, with how many
    // reports the reader owes it. The rows differ in where the attribute sits relative to the
    // declarations (header, between two pairs, past the last one), in how many of them there
    // are, in their indentation, and in whether the file has an item at all.
    let license = "// SPDX-License-Identifier: CC0-1.0\n\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let allowance = "#![allow(clippy::doc_lazy_continuation)]\n";
    let cases: [(&str, String, usize); 10] = [
        ("an empty root", String::new(), 0),
        (
            "a header and nothing else",
            format!("{license}// The target.\n{allowance}"),
            0,
        ),
        (
            "an inner attribute in the header",
            format!("{license}{allowance}// The target.\n{first}{second}"),
            0,
        ),
        (
            "two inner attributes in the header",
            format!("{license}{allowance}#![allow(dead_code)]\n{first}"),
            0,
        ),
        (
            "no inner attribute at all",
            format!("{license}// The target.\n{first}{second}"),
            0,
        ),
        (
            "an inner attribute between two pairs",
            format!("{license}// The target.\n{first}{allowance}{second}"),
            1,
        ),
        (
            // The shape this rule exists for: a landing branch re-appending the header's
            // allowance below the list it inserted into.
            "an inner attribute past the last declaration",
            format!("{license}{allowance}// The target.\n{first}{second}{allowance}"),
            1,
        ),
        (
            "two inner attributes past the last declaration",
            format!("{license}// The target.\n{first}{allowance}#![allow(dead_code)]\n"),
            2,
        ),
        (
            "an indented inner attribute past the last declaration",
            format!("{license}// The target.\n{first}    {allowance}"),
            1,
        ),
        (
            // An outer attribute documents the item below it, so it opens an item rather than
            // applying to the file: the first one is where the list starts.
            "an outer attribute past the last declaration",
            format!("{license}// The target.\n{first}#[allow(dead_code)]\nmod query_wire;\n"),
            0,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let reports = misplaced_inner_attributes(source);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}

#[test]
fn the_reader_classifies_the_line_an_inner_attribute_opens() {
    // Each form a line of a declaration root can take, and whether it opens an inner
    // attribute: the classifier is what the rule above is read with, so its vocabulary is
    // stated here rather than assumed.
    let cases: [(&str, &str, bool); 8] = [
        ("an inner attribute", "#![allow(dead_code)]", true),
        (
            "an inner attribute carrying a path",
            "#![cfg_attr(test, allow(dead_code))]",
            true,
        ),
        ("an outer attribute", "#[path = \"all/boot_serve.rs\"]", false),
        ("a module declaration", "mod boot_serve;", false),
        ("an ordinary comment", "// The target.", false),
        ("an inner doc comment", "//! The target.", false),
        ("a comment naming an inner attribute", "// `#![allow(..)]`", false),
        ("a blank line", "", false),
    ];
    for (name, line, expected) in cases {
        let actual = opens_an_inner_attribute(line);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected opens_an_inner_attribute({line:?}) to be {expected}, got \
             {actual}"
        );
    }
}
