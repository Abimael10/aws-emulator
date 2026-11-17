// SPDX-License-Identifier: CC0-1.0

//! Every declaration root carries one header block too.
//!
//! The sibling [`super::all_rs_carries_one_header_block`] owns the reader for that rule: the
//! header ahead of the first item is the region every list guard skips, so it is where each
//! landing's re-typed copy of the module's prose accumulates, until one copy lands below the
//! first item and the language refuses it — which is how a target comes to compile no suite at
//! all. The rule is a property of a declaration ROOT rather than of one file, and every
//! service crate's root is appended to by the same branches, so this suite reads it for every
//! root on disk, from the one target that links them all (the crate that breaks it cannot
//! report it: its own target no longer compiles).
//!
//! No shape of the list beneath that header changes the reading. The reader takes the block
//! from [`super::all_rs_carries_one_header_block::header_of`], which ends at the first item, so
//! a note a root keeps among its declarations contributes no line to what the reader compares
//! and cannot bring that root under the rule by its own style — the same reading under which
//! [`super::declaration_roots_state_each_header_paragraph_once`] takes the whole set of roots,
//! one paragraph at a time. The LIST half of this duplication rule is the half that does have
//! to be read under that restriction, and
//! [`super::declaration_roots_hold_canonical_lists`] reads it there, through the selector this
//! module keeps for it.
//!
//! The reader, and the header boundary it reads a root against, are the sibling's: one rule
//! has one reading, so no root is held to a different rule from another's.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::fs;

use crate::all_rs_carries_one_header_block::{header_of, repeated_header_lines};
use crate::all_rs_declares_each_suite_once::{declaration_roots, relative, workspace_root};
use crate::all_rs_inner_doc_comments_precede_the_first_item::first_item_line;

/// Whether the declaration list of `source` — everything from its first item on — holds no
/// prose.
///
/// The two header rules — this one and
/// [`super::declaration_roots_hold_no_header_fragment`]'s — are read for every root, because a
/// line of the list is no line of the header. The selector is kept here for the sibling that
/// reads the LIST half of this duplication rule,
/// [`super::declaration_roots_hold_canonical_lists`], where a note standing among the
/// declarations does repeat a line of the file by a root's own style rather than by a merge.
pub(crate) fn the_list_is_prose_free(source: &str) -> bool {
    let Some(item_line) = first_item_line(source) else {
        return true;
    };
    !source.lines().skip(item_line - 1).any(|line| {
        !line
            .trim_start()
            .trim_start_matches('/')
            .trim()
            .is_empty()
            && line.trim_start().starts_with("//")
    })
}

#[test]
fn every_declaration_root_carries_one_header_block() {
    // The cases are the workspace's declaration roots: they differ in how long their header
    // block is, in whether it is written as an inner doc comment or as ordinary comments, in how
    // many suites they declare, in whether they carry an inner attribute at all, and in whether
    // they keep a note among those declarations.
    let roots = declaration_roots();
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate under {}, read {} of them",
        workspace_root().display(),
        roots.len()
    );

    let mut header_lines = 0_usize;
    let mut reports = Vec::new();
    for root in &roots {
        let source = fs::read_to_string(root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        header_lines += header_of(&source).len();
        let case = relative(root);
        for report in repeated_header_lines(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    // Without this, roots read as holding no header at all would satisfy the report below by
    // having nothing to break it.
    assert!(
        header_lines > 100,
        "expected the {} declaration roots to open with their header blocks, read \
         {header_lines} header line(s) across all of them",
        roots.len()
    );
    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to write its header block once, got {} \
         report(s), each a paragraph the next landing has two places to keep: {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reads_the_roots_whose_list_holds_no_prose() {
    // One synthetic root per shape a list can take, and whether that list holds prose: it is
    // what the sibling reading the LIST half of this rule selects on, since a root keeping notes
    // among its declarations repeats a sentence per note there by its own style. The rows differ
    // in where a comment sits relative to the declarations, in its kind, and in whether the file
    // has an item at all.
    let license = "// SPDX-License-Identifier: CC0-1.0\n";
    let first = "#[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n";
    let second = "#[path = \"all/query_wire.rs\"]\nmod query_wire;\n";
    let header = "// The declaration list of this target.\n";
    let note = "// Declared here, for the reason the entry above records.\n";
    let cases: [(&str, String, bool); 8] = [
        ("an empty root", String::new(), true),
        ("a header and nothing else", format!("{license}{header}"), true),
        (
            "a header and a prose-free list",
            format!("{license}{header}{first}{second}"),
            true,
        ),
        (
            "a list with a note between two pairs",
            format!("{license}{header}{first}{note}{second}"),
            false,
        ),
        (
            "a list with a note past its last pair",
            format!("{license}{header}{first}{second}{note}"),
            false,
        ),
        (
            "a list with an inner doc comment in it",
            format!("{license}{header}{first}//! A note typed as a doc comment.\n{second}"),
            false,
        ),
        (
            "a list with a marker-only line in it, which carries no prose",
            format!("{license}{header}{first}//\n{second}"),
            true,
        ),
        (
            "a note between an outer attribute and its mod, which opens the list",
            format!("{license}{header}#[path = \"all/boot_serve.rs\"]\n{note}mod boot_serve;\n"),
            false,
        ),
    ];

    for case in &cases {
        let (name, source, expected) = case;
        let actual = the_list_is_prose_free(source);
        assert_eq!(
            actual, *expected,
            "case {name:?}: expected the_list_is_prose_free to be {expected}, got {actual}"
        );
    }
}

#[test]
fn the_rule_is_read_against_the_header_the_sibling_reads() {
    // The reading this suite borrows, stated against the roots on disk rather than assumed: a
    // root's header ends at its first item, and that item opens the declaration list. A root
    // whose header ran past it would be read for lines belonging to the list, which is the
    // canonical-list rule rather than this one.
    for root in declaration_roots() {
        let case = relative(&root);
        let source =
            fs::read_to_string(&root).unwrap_or_else(|error| panic!("read {case}: {error}"));
        let first_item = first_item_line(&source);
        let opens_the_list = first_item.and_then(|line| source.lines().nth(line - 1));
        assert!(
            opens_the_list.is_some_and(|line| line.starts_with("#[path = \"all/")
                || line.starts_with("mod ")
                || line.starts_with("pub mod ")
                || line.starts_with("pub(crate) mod ")),
            "case {case}: expected the header to end at the first declaration of the list, read \
             {opens_the_list:?} as its first item"
        );
        assert_eq!(
            header_of(&source).len(),
            first_item.map(|line| line - 1).unwrap_or_default(),
            "case {case}: expected the header to be every line ahead of the first item"
        );
    }
}
