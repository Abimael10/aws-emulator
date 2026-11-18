// SPDX-License-Identifier: CC0-1.0

//! A module DIRECTORY under a crate's `tests/all/` is a suite its `tests/all.rs` owes a
//! declaration, like any `<name>.rs` file.
//!
//! The build-time layout check counts both shapes — a `<name>.rs` file and a `<name>/`
//! directory carrying the `mod.rs` that makes it a module
//! (`emulator_core_build_support::check_integration_test_layout`, which this crate's
//! `build.rs` calls) — and refuses a root that has fallen behind either, for the crate being
//! built. The sibling [`super::every_suite_file_is_declared`] reads the workspace view of the
//! FILE half from this target, the one target that links every service crate; that half is
//! not repeated here. What this suite adds is the DIRECTORY half, which a file-only reader
//! skips: a module directory no `mod` names is compiled nowhere and its tests never run, and
//! no compiler says so.
//!
//! The rule it reads is one classifier — what, if anything, one entry directly under
//! `tests/all/` owes a declaration for — so a reader of a root cannot disagree with the walk
//! about which shapes count. What the declaration must then look like, that there is exactly
//! one of it, and that nothing but declarations sits in the list are the three `all_rs_*`
//! siblings' rules.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// The roots, and the files one of them declares, come from the sibling suite that owns both
// rather than from a second copy here: that reader takes a declaration in every spelling the
// compiler does, so a suite declared in an unusual one is not reported as undeclared.
use super::all_rs_declares_each_suite_once::{
    declaration_roots, declared_suite_files, relative, workspace_root,
};

/// What an entry directly under `tests/all/` is, as far as the rule cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// A file, whatever its extension.
    File,
    /// A directory, and whether it carries the `mod.rs` that makes it a module.
    Directory { carries_a_mod_rs: bool },
}

/// The file a declaration root owes a declaration for, given one entry directly under its
/// `tests/all/`, or `None` when the root owes that entry nothing.
///
/// A `<name>.rs` file is a suite. A `<name>/` directory is one only when it carries a
/// `mod.rs`, and then that `mod.rs` is the file the declaration reaches; without one the
/// directory is no module at all, whatever Rust source sits inside it. Anything else — a
/// fixture of some other kind — is no suite either.
fn suite_file_of(path: &Path, entry: Entry) -> Option<PathBuf> {
    match entry {
        Entry::File => path
            .extension()
            .is_some_and(|extension| extension == "rs")
            .then(|| path.to_path_buf()),
        Entry::Directory { carries_a_mod_rs } => carries_a_mod_rs.then(|| path.join("mod.rs")),
    }
}

/// The `mod.rs` of every module directory directly under `all_dir`, each read through
/// [`suite_file_of`].
///
/// A file deeper in the tree is declared by that module rather than by the root, so the walk
/// does not descend; a directory with no `mod.rs` yields nothing.
fn directory_modules_under_all(all_dir: &Path) -> BTreeSet<PathBuf> {
    let mut modules = BTreeSet::new();
    // A root whose `tests/all/` cannot be read declares suites that are all missing, which is
    // what the sibling guard reports; the walk itself answers empty.
    let Ok(entries) = fs::read_dir(all_dir) else {
        return modules;
    };
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|error| panic!("read an entry of {}: {error}", all_dir.display()));
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let kind = Entry::Directory {
            carries_a_mod_rs: path.join("mod.rs").is_file(),
        };
        modules.extend(suite_file_of(&path, kind));
    }
    modules
}

#[test]
fn every_declaration_root_declares_the_module_directories_beside_it() {
    // The cases are the workspace's declaration roots: one per crate that roots its `all`
    // target in a `tests/all.rs`, differing in how many suites they declare, in how their
    // declarations are spelled, and in what sits beside them under `tests/all/`.
    let roots = declaration_roots();
    // A floor rather than a census: a crate that migrates to the macro layout takes one root
    // out of this walk.
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate under {}, read {} of them",
        workspace_root().display(),
        roots.len()
    );

    let mut reports = Vec::new();
    for declaration_root in &roots {
        let case = relative(declaration_root);
        let all_dir = declaration_root
            .parent()
            .expect("a tests/all.rs has a tests/ parent")
            .join("all");
        let declared: BTreeSet<PathBuf> = declared_suite_files(declaration_root)
            .into_iter()
            .map(|path| path.canonicalize().unwrap_or(path))
            .collect();
        for module in directory_modules_under_all(&all_dir) {
            let canonical = module.canonicalize().unwrap_or_else(|_| module.clone());
            if !declared.contains(&canonical) {
                reports.push(format!(
                    "case {case}: expected a declaration of the module directory {}, got \
                     none, so that suite is compiled nowhere and its tests never run",
                    relative(&module)
                ));
            }
        }
    }

    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to declare every module directory beside \
         it under tests/all/, got {} report(s): {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_rule_reads_the_entries_a_root_owes_a_declaration() {
    // One row per shape an entry directly under `tests/all/` can take — more shapes than the
    // workspace holds at once, so each is stated rather than placed on disk. They differ in
    // whether the entry is a file or a directory, in the extension, and in whether a directory
    // carries the `mod.rs` that makes it a module. The expectation is the file the root owes a
    // declaration for, relative to `tests/all/`.
    let all_dir = workspace_root().join("core/router/tests/all");
    let cases: [(&str, &str, Entry, Option<&str>); 7] = [
        (
            "a suite file",
            "boot_serve.rs",
            Entry::File,
            Some("boot_serve.rs"),
        ),
        (
            "a suite file whose name carries dots of its own",
            "query_wire.v2.rs",
            Entry::File,
            Some("query_wire.v2.rs"),
        ),
        (
            "a fixture that is not Rust source",
            "fixture.json",
            Entry::File,
            None,
        ),
        ("a file with no extension at all", "README", Entry::File, None),
        (
            "a directory module, which the root declares through its mod.rs",
            "ec2_network",
            Entry::Directory {
                carries_a_mod_rs: true,
            },
            Some("ec2_network/mod.rs"),
        ),
        (
            // Without a `mod.rs` the directory is no module, so the root owes it nothing —
            // whatever Rust source sits inside it, which is a level the walk never reaches.
            "a directory that is no module",
            "notes",
            Entry::Directory {
                carries_a_mod_rs: false,
            },
            None,
        ),
        (
            // The same name a suite file could have, read as the directory it is: the
            // extension is not what makes an entry a suite, its kind is.
            "a directory whose name ends in .rs and holds no mod.rs",
            "stale_notes.rs",
            Entry::Directory {
                carries_a_mod_rs: false,
            },
            None,
        ),
    ];
    for (case, entry, kind, expected) in cases {
        let expected = expected.map(|owed| all_dir.join(owed));
        let owed = suite_file_of(&all_dir.join(entry), kind);
        assert_eq!(
            owed, expected,
            "case {case:?}: expected the entry {entry:?} ({kind:?}) to owe the declaration \
             {expected:?}, got {owed:?}"
        );
    }
}

#[test]
fn the_walk_reads_only_the_top_level_of_a_tests_all_directory() {
    // The rule the walk adds to the classifier: it does not descend, so whatever it reports is
    // the `mod.rs` of a directory directly under `tests/all/` — never a file deeper in, which
    // the module there declares itself. Read against every root's directory on disk.
    let roots = declaration_roots();
    // A floor rather than a census: a crate that migrates to the macro layout takes one root
    // out of this walk.
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate under {}, read {} of them",
        workspace_root().display(),
        roots.len()
    );
    for declaration_root in &roots {
        let all_dir = declaration_root
            .parent()
            .expect("a tests/all.rs has a tests/ parent")
            .join("all");
        for module in directory_modules_under_all(&all_dir) {
            let case = relative(&module);
            assert!(
                module.is_file(),
                "case {case}: expected the walk to report a mod.rs that exists"
            );
            assert!(
                module.file_name().is_some_and(|name| name == "mod.rs"),
                "case {case}: expected the walk to report a directory module through its \
                 mod.rs"
            );
            let directory = module.parent().expect("a mod.rs has a directory");
            assert_eq!(
                directory.parent(),
                Some(all_dir.as_path()),
                "case {case}: expected a directory directly under {}, since the walk does \
                 not descend",
                all_dir.display()
            );
        }
    }
}
