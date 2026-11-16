// SPDX-License-Identifier: CC0-1.0

//! The router's `all` target is rooted at `tests/all/main.rs`, which declares every suite
//! beside it from the directory.
//!
//! This crate used to root that target at a hand-written `tests/all.rs` whose `#[path]`/`mod`
//! pairs every landing branch appended to, and the `all_rs_*` siblings pin what such a list
//! may hold — each of them now reads it only for a crate that still keeps one. What replaced
//! it is a layout rather than a list, and this suite pins that layout, so the properties the
//! list used to carry are still checked for this crate: one cargo target rather than one per
//! top-level file, every `.rs` file beside the root compiled as a suite, and each declared
//! under exactly its own file name.
//!
//! The rules are the build-time layout check's (`emulator_core_build_support`, which this
//! crate's `build.rs` calls): it refuses a top-level `tests/*.rs`, a missing
//! `tests/all/main.rs`, a suite directory with no grouping file beside it, and a file whose
//! stem is not a plain module identifier. A build-script error is reported where the crate is
//! built, so this suite reads the same layout from the test binary, where a report names the
//! file it is about.
//!
//! Each rule is read over a TABLE: the entries of `tests/` and of `tests/all/` as they are on
//! disk — files and directories, suites and the fixtures beside them — rather than one named
//! file.
//!
//! It reads directories only; it starts no router and reaches no network.

use std::fs;
use std::path::{Path, PathBuf};

use crate::all_rs_declares_each_suite_once::{all_rs, workspace_root};

/// This crate's `tests` directory.
fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// The root of this crate's one integration-test target.
fn automod_root() -> PathBuf {
    tests_dir().join("all").join("main.rs")
}

/// The workspace-relative name of `path`, so a report names the file it is about.
fn relative(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The entries directly under `directory`, sorted by name, so a report reads the same way
/// whatever order the filesystem hands them back in.
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

/// The words a module cannot be named after: Rust's keywords, which no `mod` item may
/// spell. A suite file named after one is renamed by `automod` for the same reason a stem
/// that is no identifier is.
const KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if",
    "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
    "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Whether `stem` is a plain module name — `[A-Za-z_][A-Za-z0-9_]*` and not a keyword —
/// which is what makes the module `automod` declares for a file carry exactly that file's
/// name.
fn is_module_identifier(stem: &str) -> bool {
    let mut characters = stem.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && stem != "_"
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !KEYWORDS.contains(&stem)
}

#[test]
fn the_target_is_rooted_at_one_automod_declaration() {
    let root = automod_root();
    let source = fs::read_to_string(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", relative(&root)));
    let items: Vec<&str> = source
        .lines()
        .map(str::trim)
        .filter(|line| !(line.is_empty() || line.starts_with("//")))
        .collect();
    assert_eq!(
        items,
        vec!["automod::dir!(\"tests/all\");"],
        "expected {} to hold the one declaration that makes every .rs file beside it a suite \
         of this target, got {items:?}",
        relative(&root)
    );
    assert!(
        all_rs().is_none(),
        "expected no hand-declared tests/all.rs beside {}: cargo rejects two targets named \
         `all`, and the layout check refuses the pair",
        relative(&root)
    );
}

#[test]
fn no_top_level_file_under_tests_is_a_target_of_its_own() {
    // Every entry directly under tests/: the `all/` directory this target is rooted in, the
    // vendored target beside it, and anything else a landing may have left there.
    let tests = tests_dir();
    let stray: Vec<String> = entries_of(&tests)
        .into_iter()
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        })
        .map(|path| relative(&path))
        .collect();
    assert!(
        stray.is_empty(),
        "expected every suite of this crate to live under tests/all/, since cargo builds one \
         test binary per top-level tests/*.rs and each links the whole dependency closure, \
         got {} top-level file(s): {stray:?}",
        stray.len()
    );
}

#[test]
fn every_suite_beside_the_root_is_declared_under_its_own_name() {
    // The table is the whole directory: one row per entry directly under tests/all/, which is
    // every suite file of this target plus any directory of grouped suites beside them.
    let all_dir = tests_dir().join("all");
    let entries = entries_of(&all_dir);
    assert!(
        entries.len() > 100,
        "expected the suites of this target to sit under {}, read {} entry/entries",
        relative(&all_dir),
        entries.len()
    );

    let mut reports = Vec::new();
    for entry in &entries {
        let case = relative(entry);
        if entry.is_file() {
            // `automod` declares a module per .rs file, named after the file: a stem that is
            // no identifier is renamed, and the suite's tests then answer to another name.
            let Some(stem) = entry.file_stem().map(|stem| stem.to_string_lossy().to_string()) else {
                continue;
            };
            if entry.extension().is_some_and(|extension| extension == "rs")
                && !is_module_identifier(&stem)
            {
                reports.push(format!(
                    "case {case}: expected a plain module identifier as the file stem, got \
                     {stem:?}"
                ));
            }
            continue;
        }
        // A directory of grouped suites is reached through the `<name>.rs` beside it holding
        // its own `automod::dir!`; without that file its suites compile nowhere.
        let holds_rust = fs::read_dir(entry)
            .unwrap_or_else(|error| panic!("read {case}: {error}"))
            .flatten()
            .any(|inside| {
                inside
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "rs")
            });
        let name = entry
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let grouping = all_dir.join(format!("{name}.rs"));
        if holds_rust && !grouping.is_file() {
            reports.push(format!(
                "case {case}: expected the grouping file {} beside it, got none, so the suites \
                 inside it are compiled nowhere",
                relative(&grouping)
            ));
        }
    }

    assert!(
        reports.is_empty(),
        "expected every entry under {} to be a suite this target compiles under its own name, \
         got {} report(s): {reports:#?}",
        relative(&all_dir),
        reports.len()
    );
}

#[test]
fn the_identifier_reader_accepts_what_automod_declares_unchanged() {
    // One row per shape a suite file's stem can take: the names the tree uses, and the ones
    // `automod` would have to rename. The rows differ in the first character, in the
    // characters after it, in case, and in length.
    let cases: [(&str, &str, bool); 12] = [
        ("a snake_case suite name", "boot_serve", true),
        ("a name carrying digits", "apigatewayv2_websocket", true),
        ("a single letter", "a", true),
        ("a leading underscore", "_scratch", true),
        ("the lone underscore, which names no module", "_", false),
        ("a leading digit", "2_phase", false),
        ("a keyword, which no mod item may spell", "type", false),
        ("another keyword", "mod", false),
        ("a hyphen, which no identifier holds", "boot-serve", false),
        ("a dot inside the stem", "query_wire.v2", false),
        ("a space inside the stem", "boot serve", false),
        ("nothing at all", "", false),
    ];
    for (case, stem, expected) in cases {
        let actual = is_module_identifier(stem);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected is_module_identifier({stem:?}) to be {expected}, got \
             {actual}"
        );
    }
}
