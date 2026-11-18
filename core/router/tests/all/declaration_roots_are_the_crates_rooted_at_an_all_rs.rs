// SPDX-License-Identifier: CC0-1.0

//! A crate's declaration root is the `tests/all.rs` its one `all` target is rooted in, and no
//! crate has two roots.
//!
//! Cargo builds one test binary per top-level `tests/*.rs` and one per `tests/<dir>/main.rs`,
//! each statically linking the crate and its whole dependency closure, so a crate keeps one
//! `all` target and roots it in exactly one file. There are two ways to root it: a
//! `tests/all.rs` holding one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite, or a
//! `tests/all/main.rs` holding `automod::dir!("tests/all")`, which declares one module per
//! suite file when the target compiles. Only the first has a declaration list, so only the
//! first is a declaration root; the vendored layout check this crate's `build.rs` calls
//! refuses the second beside the first, because the two together are two cargo targets named
//! `all`.
//!
//! The sibling suites read the rules of a declaration root — that its list holds one
//! declaration per suite, that each names a file, that nothing is appended past the last of
//! them, that its header block is written once — against
//! [`super::all_rs_declares_each_suite_once::declaration_roots`]. This suite pins what that
//! enumerator answers: a crate's `tests/all.rs` exactly when it has one, so every path it
//! yields is a file a reader can open, and none is the root of a crate whose target is rooted
//! at `tests/all/main.rs` — a path that does not exist, which any reader built on it panics
//! on rather than reporting anything about the layout.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// The enumerator this suite is about, this crate's own test directory, and the reporting
// helper, from the sibling that owns them rather than from second copies here.
use crate::all_rs_declares_each_suite_once::{
    declaration_roots, relative, tests_dir, workspace_root,
};

/// Where a crate roots its single `all` integration-test target, read from the two files that
/// can root one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// A `tests/all.rs`: the hand-written declaration list, which is a declaration root.
    HandDeclared,
    /// A `tests/all/main.rs`: `automod` declares one module per suite file, so the crate keeps
    /// no declaration list and has no root to read.
    Automod,
    /// Neither file: the crate roots no `all` target, and so has no root either.
    Neither,
    /// Both, which cargo rejects as two targets named `all` and the layout check refuses.
    Both,
}

/// The layout of a crate whose `tests/` directory holds these two files.
fn layout_of(has_an_all_rs: bool, has_an_all_main_rs: bool) -> Layout {
    match (has_an_all_rs, has_an_all_main_rs) {
        (true, true) => Layout::Both,
        (true, false) => Layout::HandDeclared,
        (false, true) => Layout::Automod,
        (false, false) => Layout::Neither,
    }
}

/// Whether a crate with `layout` owes a declaration root — a `tests/all.rs` the rules about a
/// declaration list are read against.
fn has_a_declaration_root(layout: Layout) -> bool {
    matches!(layout, Layout::HandDeclared | Layout::Both)
}

/// The layout of the crate rooted at `crate_dir`, read from the files in its `tests/`.
fn layout_on_disk(crate_dir: &Path) -> Layout {
    let tests = crate_dir.join("tests");
    layout_of(
        tests.join("all.rs").is_file(),
        tests.join("all").join("main.rs").is_file(),
    )
}

/// This crate, and each service crate: the crates of the workspace that hold an
/// integration-test target.
///
/// Shared with [`super::automod_rooted_suites_keep_their_inner_doc_comments_first`], which
/// enumerates the macro-rooted half of the same set, so the two readings are taken over the
/// same crates.
pub(crate) fn crate_dirs() -> Vec<PathBuf> {
    let root = workspace_root();
    let services = root.join("services");
    let entries = fs::read_dir(&services)
        .unwrap_or_else(|error| panic!("read {}: {error}", services.display()));
    let mut dirs = vec![root.join("core").join("router")];
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|error| panic!("read an entry of {}: {error}", services.display()));
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    dirs
}

#[test]
fn the_layout_a_crate_roots_its_target_in_decides_whether_it_has_a_declaration_root() {
    // One row per combination of the two files that can root an `all` target — more
    // combinations than the workspace holds at once, so each is stated rather than placed on
    // disk — with the layout it is and whether that layout owes a declaration root. The rule
    // itself is read against the crates on disk by the three tests below; this row set is the
    // vocabulary they are written in.
    let cases: [(&str, bool, bool, Layout, bool); 4] = [
        (
            "a tests/all.rs and no tests/all/main.rs",
            true,
            false,
            Layout::HandDeclared,
            true,
        ),
        (
            "a tests/all/main.rs and no tests/all.rs",
            false,
            true,
            Layout::Automod,
            false,
        ),
        ("neither file", false, false, Layout::Neither, false),
        (
            "both files, which cargo rejects as two targets named `all`",
            true,
            true,
            Layout::Both,
            true,
        ),
    ];
    for (case, has_an_all_rs, has_an_all_main_rs, expected, owes_a_root) in cases {
        let layout = layout_of(has_an_all_rs, has_an_all_main_rs);
        assert_eq!(
            layout, expected,
            "case {case:?}: expected {expected:?}, got {layout:?}"
        );
        let owed = has_a_declaration_root(layout);
        assert_eq!(
            owed, owes_a_root,
            "case {case:?}: expected has_a_declaration_root to be {owes_a_root}, got {owed}"
        );
    }
}

#[test]
fn no_crate_roots_two_all_targets() {
    // The cases are every crate in the workspace, which differ in which of the two layouts
    // they root their `all` target in.
    let crates = crate_dirs();
    assert!(
        crates.len() > 400,
        "expected this crate and one per service crate under {}, read {} of them",
        workspace_root().display(),
        crates.len()
    );
    for crate_dir in &crates {
        let case = relative(crate_dir);
        let layout = layout_on_disk(crate_dir);
        assert_ne!(
            layout,
            Layout::Both,
            "case {case}: expected one of tests/all.rs and tests/all/main.rs to root its \
             `all` target, got {layout:?}, which cargo rejects as two targets named `all`"
        );
    }
}

#[test]
fn the_enumerated_roots_are_the_all_rs_of_every_crate_that_has_one() {
    let expected: BTreeSet<PathBuf> = crate_dirs()
        .into_iter()
        .filter(|crate_dir| has_a_declaration_root(layout_on_disk(crate_dir)))
        .map(|crate_dir| crate_dir.join("tests").join("all.rs"))
        .collect();
    // Without this, an enumerator that answered nothing would satisfy the comparison below.
    // A floor rather than a census: every crate that migrates to the macro layout takes one
    // out of this set, so what the number proves is that the walk reached the service crates
    // at all.
    assert!(
        expected.len() > 100,
        "expected one declaration root per service crate under {}, read {} of them",
        workspace_root().display(),
        expected.len()
    );
    let enumerated: BTreeSet<PathBuf> = declaration_roots().into_iter().collect();
    let missing: Vec<String> = expected
        .difference(&enumerated)
        .map(|root| relative(root))
        .collect();
    assert!(
        missing.is_empty(),
        "expected every crate rooted at a tests/all.rs to be enumerated, got {} that \
         is/are not: {missing:#?}",
        missing.len()
    );
    let surplus: Vec<String> = enumerated
        .difference(&expected)
        .map(|root| relative(root))
        .collect();
    assert!(
        surplus.is_empty(),
        "expected no root beside those, got {} enumerated for a crate that roots no \
         declaration list: {surplus:#?}",
        surplus.len()
    );
}

#[test]
fn every_enumerated_root_is_a_file_a_reader_can_open() {
    // The rule the sibling readers rest on: each of them opens every enumerated root and
    // panics on a path that is absent, so a root that names no file reports nothing about the
    // layout it was enumerated to check.
    let roots = declaration_roots();
    // A floor rather than a census: a crate that migrates to the macro layout takes one root
    // out of this walk.
    assert!(
        roots.len() > 100,
        "expected one declaration root per service crate under {}, read {} of them",
        workspace_root().display(),
        roots.len()
    );
    for root in &roots {
        let case = relative(root);
        assert!(
            root.is_file(),
            "case {case}: expected an enumerated declaration root to be a file on disk"
        );
        assert!(
            root.file_name().is_some_and(|name| name == "all.rs"),
            "case {case}: expected an enumerated root to be a crate's tests/all.rs"
        );
        let source = fs::read_to_string(root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        assert!(
            !source.is_empty(),
            "case {case}: expected a declaration root to hold its list, read nothing"
        );
    }
}

#[test]
fn this_crate_contributes_no_declaration_root() {
    // What this crate's `all` target is rooted in, and that the enumerator therefore answers
    // nothing for it. That the root it IS rooted in holds the one `automod` declaration is
    // the sibling `router_test_target_is_automod_rooted`'s rule, read there rather than a
    // second time here.
    let tests = tests_dir();
    let layout = layout_on_disk(
        tests
            .parent()
            .expect("this crate's tests/ directory has a crate root above it"),
    );
    assert_eq!(
        layout,
        Layout::Automod,
        "expected this crate's `all` target to be rooted at tests/all/main.rs, got {layout:?}"
    );
    let own = tests.join("all.rs");
    let enumerated = declaration_roots();
    assert!(
        !enumerated.contains(&own),
        "expected this crate to contribute no declaration root, got {} among the {} \
         enumerated",
        relative(&own),
        enumerated.len()
    );
}
