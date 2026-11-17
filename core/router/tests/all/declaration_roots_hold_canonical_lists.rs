// SPDX-License-Identifier: CC0-1.0

//! No declaration root the workspace keeps BY HAND says a line of its LIST twice.
//!
//! A declaration root is appended to by every landing branch, and it is the one file in a
//! target whose breakage takes every suite down with it: a copy of a header line that a later
//! landing merges in as `//!` past the first item is `E0753` and compiles no suite of that
//! crate at all. A line of the list that repeats an earlier line of the file is where such a
//! copy comes from — what a landing leaves behind when it appends its paragraph to the end of
//! a root instead of inserting its `#[path]`/`mod` pair into the list.
//!
//! The READER of that rule is the sibling's ([`super::all_rs_holds_no_line_twice`]), which
//! reads it for THIS crate's own root; what this suite adds is the OTHER roots — every
//! `tests/all.rs` a service crate keeps — read from the one target that links them all, since a
//! crate that breaks the rule cannot report it (its own `all` target no longer compiles). It is
//! the same reason the sibling over the header half of the duplication rule
//! ([`super::declaration_roots_carry_no_duplicated_comment_line`]) reads those roots from here.
//!
//! It is read for a root whose list is PROSE-FREE: a root that keeps a note beside each
//! declaration was written by repeating one sentence per note, so its list carries copies by
//! its own style rather than by a merge, and reading the rule there would report how that root
//! is written rather than the hazard the rule is about. Folding those notes into the suites
//! they explain brings such a root under this rule by itself. The HEADER half of the same
//! duplication rule needs no such restriction — a line of the list is no line of a header — so
//! [`super::declaration_roots_carry_no_duplicated_comment_line`] reads it for every root, and
//! keeps the selector this suite reads itself under.
//!
//! The OTHER two rules of a canonical list — that the list holds nothing but pairs, and that
//! they are sorted — are not read here, and the sibling that owns their reader says so: the
//! roots this workspace keeps separate their pairs with blank lines and append a pair past
//! their sorted span, so neither is a property they have, and bringing one under either rule
//! means changing that root first.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::fs;
use std::path::Path;

// The roots, the reader and the restriction it is read under all come from the siblings that
// own them, so no two suites of this target can disagree about any of them.
use crate::all_rs_declares_each_suite_once::{declaration_roots, workspace_root};
use crate::all_rs_holds_no_line_twice::repeated_list_lines;
use crate::declaration_roots_carry_no_duplicated_comment_line::the_list_is_prose_free;

/// The workspace-relative name of `path`, for a report that names the file it is about.
fn relative(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn no_prose_free_declaration_list_says_a_line_its_root_already_said() {
    // The cases are the workspace's declaration roots whose list holds no prose: they differ in
    // how many suites they declare, in how long their header block is, in whether it is written
    // as an inner doc comment or as ordinary comments, and in whether they carry an inner
    // attribute at all.
    let roots = declaration_roots();
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate under {}, read {} of them",
        workspace_root().display(),
        roots.len()
    );

    let mut read = 0_usize;
    let mut reports = Vec::new();
    for root in &roots {
        let source = fs::read_to_string(root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        if !the_list_is_prose_free(&source) {
            continue;
        }
        read += 1;
        let case = relative(root);
        for report in repeated_list_lines(&source) {
            reports.push(format!("case {case}: {report}"));
        }
    }

    assert!(
        read > 100,
        "expected most service crates to keep a prose-free declaration list, read {read} of the \
         {} roots",
        roots.len()
    );
    assert!(
        reports.is_empty(),
        "expected none of the {read} prose-free declaration lists to repeat a line its root had \
         already said, got {} copied line(s), each one a later landing can merge in as an inner \
         doc comment past the first item: {reports:#?}",
        reports.len()
    );
}
