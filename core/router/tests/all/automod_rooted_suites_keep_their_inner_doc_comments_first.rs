// SPDX-License-Identifier: CC0-1.0

//! Every file a macro-rooted `all` target compiles keeps its inner doc comments ahead of its
//! first item.
//!
//! An inner doc comment (`//!`, `/*!`) documents the module it is written in, so the language
//! accepts it only before that module's first item: one written lower down is
//! `error[E0753]: expected outer doc comment`, and it fails the whole integration-test target
//! the file belongs to, taking every suite in it down with it. The rule is a property of a
//! FILE, not of a declaration list, so it holds for every file such a target compiles.
//!
//! [`super::all_rs_inner_doc_comments_precede_the_first_item`] owns the reading of that rule
//! and reads it for every crate's hand-written `tests/all.rs`, for the suite files such a root
//! declares, and for the files beside THIS crate's macro root. A crate whose target is rooted
//! at a `tests/all/main.rs` instead keeps no declaration list, so its suites are named by no
//! root and are outside all three of those sets: the service crates on that layout hold
//! suites the rule was read for nowhere. This suite reads it for them — for every crate of
//! the workspace that roots its target with the macro rather than with a list, this one
//! included.
//!
//! Which files such a target compiles is read from the root rather than assumed: the macro
//! declares one module per `.rs` file of the directory its call names, and a file that is
//! itself nothing but a call over a directory beside it groups the suites in that directory,
//! which the build-time layout check allows. So the walk follows the calls, and the rule is
//! read over the whole of what they reach: one row per compiled file, across every crate on
//! that layout.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// The workspace, its crates and the reporting helper, from the siblings that own them rather
// than from second copies here.
use crate::all_rs_declares_each_suite_once::{
    automod_suite_files, declaration_roots, relative, tests_dir, workspace_root,
};
// The rule itself: the same reader the sibling applies to a hand-written root and the suites it
// declares, applied here to the files a macro-rooted target compiles.
use crate::all_rs_inner_doc_comments_precede_the_first_item::{
    misplaced_inner_doc_comments, InnerDocComments,
};
use crate::declaration_roots_are_the_crates_rooted_at_an_all_rs::crate_dirs;
use crate::router_suites_are_declared_by_automod::automod_directory;

/// The file a crate roots its macro-declared `all` target in, under `tests/all/`.
const MACRO_ROOT: &str = "main.rs";

/// The file names under a declared directory that the macro declares no module for: the root
/// the call stands in, and the two names that spell a module's own source file rather than a
/// module of the directory. They are the names the build-time layout check exempts from the
/// rule that a suite file's stem is a plain module identifier, for the same reason.
const NOT_MODULES: &[&str] = &[MACRO_ROOT, "mod.rs", "lib.rs"];

/// Whether `file_name`, a file of a directory some `automod::dir!` call names, is one the
/// macro declares a module for.
fn is_declared_module(file_name: &str) -> bool {
    file_name.ends_with(".rs") && !NOT_MODULES.contains(&file_name)
}

/// The crate directory `root`, a `tests/all/main.rs`, belongs to.
fn crate_dir_of_macro_root(root: &Path) -> PathBuf {
    root.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("a tests/all/main.rs sits three levels below its crate")
        .to_path_buf()
}

/// The crate directory `root`, a `tests/all.rs`, belongs to.
fn crate_dir_of_list_root(root: &Path) -> PathBuf {
    root.parent()
        .and_then(Path::parent)
        .expect("a tests/all.rs sits two levels below its crate")
        .to_path_buf()
}

/// The `tests/all/main.rs` of every crate in the workspace whose `all` target is rooted there.
///
/// Read off the crates the workspace holds rather than off a list, so a crate migrated to the
/// macro layout is covered by the rule below the moment its root lands.
fn macro_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = crate_dirs()
        .into_iter()
        .map(|crate_dir| crate_dir.join("tests").join("all").join(MACRO_ROOT))
        .filter(|root| root.is_file())
        .collect();
    roots.sort();
    roots
}

/// The entries directly under `directory`, sorted by path, so a report reads the same way
/// whatever order the filesystem hands them back in. Panics, naming the directory, when it
/// cannot be read: a directory a root declares its suites out of has to be readable whole, or
/// the set the rule is read over quietly shrinks.
fn entries_of(directory: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read an entry of {}: {error}", directory.display()))
                .path()
        })
        .collect();
    entries.sort();
    entries
}

/// Every `.rs` file the target rooted at `root` compiles, besides `root` itself.
///
/// The walk starts at the root and follows every `automod::dir!` call it reaches: the files of
/// the directory the root's call names, and then, for each of those that is itself nothing but
/// a call over another directory, the files of that one too. A directory already walked is not
/// walked again, so two calls naming one directory yield its files once.
fn compiled_suite_files(root: &Path) -> Vec<PathBuf> {
    let crate_dir = crate_dir_of_macro_root(root);
    let mut pending = vec![root.to_path_buf()];
    let mut walked: BTreeSet<PathBuf> = BTreeSet::new();
    let mut compiled: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(caller) = pending.pop() {
        let source = fs::read_to_string(&caller)
            .unwrap_or_else(|error| panic!("read {}: {error}", caller.display()));
        let Some(named) = automod_directory(&source) else {
            continue;
        };
        // The call names a directory relative to the crate's manifest, which is how the macro
        // resolves it when the target compiles.
        let directory = crate_dir.join(named);
        if !walked.insert(directory.clone()) {
            continue;
        }
        for entry in entries_of(&directory) {
            let declared = entry.file_name().is_some_and(|name| {
                name.to_str().is_some_and(is_declared_module)
            });
            if entry.is_file() && declared {
                compiled.insert(entry.clone());
                pending.push(entry);
            }
        }
    }
    compiled.into_iter().collect()
}

/// The `.rs` files the macro declares out of the directory `root` stands in, listed from that
/// directory itself rather than reached through the walk, so the walk can be checked against
/// them.
fn modules_beside(root: &Path) -> Vec<PathBuf> {
    let directory = root
        .parent()
        .expect("a root sits in the directory whose files it declares");
    entries_of(directory)
        .into_iter()
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_declared_module)
        })
        .collect()
}

/// Whether the crate rooted at `crate_dir` holds suites at all: a `.rs` file under
/// `tests/all/` that is not the root a macro-rooted target stands in.
fn holds_suites(crate_dir: &Path) -> bool {
    let all_dir = crate_dir.join("tests").join("all");
    let Ok(entries) = fs::read_dir(&all_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_declared_module)
    })
}

#[test]
fn every_suite_a_macro_root_compiles_keeps_its_inner_doc_comments_ahead_of_its_first_item() {
    // The table is every file the macro-rooted targets of the workspace compile: one row per
    // file, spanning the crates on that layout — this one and each service crate that has
    // migrated — and spanning the positions a file's comments can sit in, since each file is
    // read as it stands on disk.
    let root = workspace_root();
    let roots = macro_roots();
    assert!(
        roots.len() > 1,
        "expected this crate's macro root and one per service crate on that layout under {}, \
         read {} of them",
        root.display(),
        roots.len()
    );

    let mut read = 0_usize;
    let mut reports = Vec::new();
    for macro_root in &roots {
        // The root is compiled by the target as well, so the rule is read for it too.
        for compiled in std::iter::once(macro_root.clone()).chain(compiled_suite_files(macro_root))
        {
            read += 1;
            let case = relative(&compiled);
            let source = fs::read_to_string(&compiled)
                .unwrap_or_else(|error| panic!("read {}: {error}", compiled.display()));
            for report in misplaced_inner_doc_comments(&source, InnerDocComments::InColumnOne) {
                reports.push(format!("case {case}: {report}"));
            }
        }
    }

    // Without this, a walk that reached nothing would satisfy the rule by holding nothing to
    // break it.
    assert!(
        read > 200,
        "expected each of the {} macro roots to compile the suites beside it, which \
         the_walk_reaches_every_file_beside_a_macro_root reads independently, read {read} \
         file(s) in all",
        roots.len()
    );
    assert!(
        reports.is_empty(),
        "expected each of the {read} files these {} macro-rooted targets compile to carry no \
         file-level inner doc comment after its first item, got {} report(s), each of which \
         fails its target with E0753: {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_two_layouts_together_cover_every_crate_that_holds_suites() {
    // What makes the reading above worth taking: the rule is read for the files of a
    // hand-written root's crate by the sibling and for the files of a macro root's crate here,
    // so a crate holding suites under neither reading would hold files no rule is read for.
    let listed: BTreeSet<PathBuf> = declaration_roots()
        .iter()
        .map(|root| crate_dir_of_list_root(root))
        .collect();
    let macro_rooted: BTreeSet<PathBuf> = macro_roots()
        .iter()
        .map(|root| crate_dir_of_macro_root(root))
        .collect();
    // A floor rather than a census: every crate that migrates to the macro layout takes one
    // crate out of this set and puts it in the other, so what the number has to prove is only
    // that the walk reached the service crates at all — a walk that reached nothing would
    // satisfy the rules below by holding nothing to break them.
    assert!(
        listed.len() > 100,
        "expected one hand-written root per service crate that keeps one, read {}",
        listed.len()
    );
    assert!(
        !macro_rooted.is_empty(),
        "expected at least this crate to root its target with the macro, read none"
    );

    let both: Vec<String> = listed
        .intersection(&macro_rooted)
        .map(|crate_dir| relative(crate_dir))
        .collect();
    assert!(
        both.is_empty(),
        "expected each crate to root its one `all` target in one of the two files, got {} \
         crate(s) holding both, which cargo rejects as two targets named `all`: {both:#?}",
        both.len()
    );

    let uncovered: Vec<String> = crate_dirs()
        .into_iter()
        .filter(|crate_dir| {
            holds_suites(crate_dir)
                && !listed.contains(crate_dir)
                && !macro_rooted.contains(crate_dir)
        })
        .map(|crate_dir| relative(&crate_dir))
        .collect();
    assert!(
        uncovered.is_empty(),
        "expected every crate holding suites to root them in one of the two files, so the rule \
         is read for each of its compiled files, got {} crate(s) rooted in neither: \
         {uncovered:#?}",
        uncovered.len()
    );
}

#[test]
fn the_walk_reaches_every_file_beside_a_macro_root() {
    // The walk is what decides which files the rule above is read over: a crate whose root it
    // read nothing out of would pass that rule holding anything at all. So it is checked
    // against an independent listing of the directory every macro root stands in — one row per
    // crate on this layout — and, for this crate, against the set the sibling enumerates.
    let own_root = tests_dir().join("all").join(MACRO_ROOT);
    let own_walked: BTreeSet<PathBuf> = compiled_suite_files(&own_root).into_iter().collect();
    let enumerated: BTreeSet<PathBuf> = automod_suite_files().into_iter().collect();
    assert!(
        enumerated.len() > 100,
        "expected this crate's suites to sit beside its root, read {} of them",
        enumerated.len()
    );
    let missed: Vec<String> = enumerated
        .difference(&own_walked)
        .map(|path| relative(path))
        .collect();
    assert!(
        missed.is_empty(),
        "expected the walk to reach every suite file the sibling enumerates beside this \
         crate's root, got {} it did not: {missed:#?}",
        missed.len()
    );

    let mut listed = 0_usize;
    let mut reports = Vec::new();
    for macro_root in macro_roots() {
        let walked: BTreeSet<PathBuf> = compiled_suite_files(&macro_root).into_iter().collect();
        let beside = modules_beside(&macro_root);
        listed += beside.len();
        if beside.is_empty() {
            reports.push(format!(
                "case {}: expected the root of an `all` target to stand beside the suites it \
                 declares, got none",
                relative(&macro_root)
            ));
        }
        for suite in beside {
            if !walked.contains(&suite) {
                reports.push(format!(
                    "case {}: expected the walk from {} to reach it, read {} file(s) from that \
                     root",
                    relative(&suite),
                    relative(&macro_root),
                    walked.len()
                ));
            }
        }
    }
    assert!(
        listed > 200,
        "expected the macro-rooted targets of the workspace to hold the suites of this crate \
         and of each migrated service crate, listed {listed} file(s)"
    );

    // Every file the walk yields, for every crate on this layout, is a readable `.rs` file
    // under that crate's tests/ and is not a root: a path that is none of those would be a
    // file the rule above reports nothing about.
    for macro_root in macro_roots() {
        let tests = macro_root
            .parent()
            .and_then(Path::parent)
            .expect("a tests/all/main.rs sits under a tests/ directory")
            .to_path_buf();
        for compiled in compiled_suite_files(&macro_root) {
            let case = relative(&compiled);
            if !compiled.is_file() {
                reports.push(format!("case {case}: expected a file on disk"));
            }
            if !compiled.starts_with(&tests) {
                reports.push(format!(
                    "case {case}: expected a file under {}",
                    relative(&tests)
                ));
            }
            if compiled.file_name().is_some_and(|name| name == MACRO_ROOT) {
                reports.push(format!(
                    "case {case}: expected a suite rather than the root declaring them"
                ));
            }
        }
    }
    assert!(
        reports.is_empty(),
        "expected every file the walk yields to be a suite file of the crate it was walked \
         from, got {} report(s): {reports:#?}",
        reports.len()
    );
}

#[test]
fn the_reader_declares_a_module_for_the_files_the_macro_does() {
    // One row per kind of entry a declared directory holds, and whether the macro declares a
    // module for it. The rows differ in the extension, in whether the name is one that spells
    // a module's own source file, and in the shape of the stem.
    let cases: [(&str, &str, bool); 9] = [
        ("a snake_case suite", "boot_serve.rs", true),
        ("a suite whose name carries digits", "apigatewayv2_websocket.rs", true),
        ("a grouping file beside its directory", "sfn.rs", true),
        ("the root the call stands in", "main.rs", false),
        ("a directory module's own source", "mod.rs", false),
        ("a library root", "lib.rs", false),
        ("a fixture that is not Rust", "event.json", false),
        ("a backup a landing left behind", "boot_serve.rs.bk", false),
        ("nothing at all", "", false),
    ];
    for (case, file_name, expected) in cases {
        let actual = is_declared_module(file_name);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected is_declared_module({file_name:?}) to be {expected}, got \
             {actual}"
        );
    }
}
