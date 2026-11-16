// SPDX-License-Identifier: CC0-1.0

//! `tests/all.rs` declares every suite exactly once, and every declaration names a file.
//!
//! The integration-test target has ONE crate root, so each `mod <name>;` in it introduces
//! `<name>` into a single module namespace: a second declaration of the same name is
//! `error[E0428]: the name ... is defined multiple times` and the whole `all` target fails
//! to compile, taking every suite in it down with it. Suites land concurrently and each
//! appends its own `#[path]`/`mod` pair to this list, so the same name arriving twice — once
//! in the sorted span beside its own name and once in the appended tail — is the collision
//! this suite pins.
//!
//! It also pins the other half of a declaration: the `#[path]` attribute that points the
//! module into `tests/all/` (a bare `mod` would resolve to a sibling of `tests/all.rs`,
//! which the layout check refuses), and the file that attribute names.
//!
//! The collision is a property of the NAME, not of how the declaration is written, so the
//! reader below takes a declaration in every spelling the compiler does — with or without a
//! visibility, with or without a trailing line comment — and counts them all against the one
//! namespace.
//!
//! It reads files only; it starts no router and reaches no network.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One `#[path = "..."]` / `mod <name>;` pair as `tests/all.rs` spells it, with the line the
/// `mod` sits on so a report can point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Declaration {
    name: String,
    path: Option<String>,
    line: usize,
}

/// This crate's `tests` directory, which holds `all.rs` and the `all/` suites it declares.
pub(crate) fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// The module name `line` declares, read the way the compiler reads it: a `mod <name>;`
/// item in column one, whatever visibility precedes it and whatever line comment trails
/// it. Every one of those spellings introduces `<name>` into the root's single namespace,
/// so a reader blind to one of them misses the collision that spelling makes.
///
/// A `mod` that does not start in column one is nested inside an item and belongs to
/// another namespace; a line whose code is empty — a commented-out declaration — declares
/// nothing.
pub(crate) fn declaration_name(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let code = line.split("//").next().unwrap_or(line).trim_end();
    let item = match code.strip_prefix("pub") {
        None => code,
        // `pub` is a token, so only a space or a `(scope)` may follow it: a run-together
        // `pubmod x;`, which the compiler rejects, declares nothing here either.
        Some(rest) => match rest.strip_prefix('(') {
            None => rest.strip_prefix(' ')?.trim_start(),
            Some(scoped) => scoped.split_once(')')?.1.trim_start(),
        },
    };
    let name = item.strip_prefix("mod ")?.trim().strip_suffix(';')?.trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then_some(name)
}

/// Every module `source` declares at its top level, in file order, each carrying the
/// `#[path]` attribute that precedes it.
fn declarations(source: &str) -> Vec<Declaration> {
    let mut declared = Vec::new();
    let mut pending: Option<String> = None;
    for (index, line) in source.lines().enumerate() {
        if let Some(attribute) = line.strip_prefix("#[path = \"") {
            pending = attribute.strip_suffix("\"]").map(str::to_string);
            continue;
        }
        let Some(name) = declaration_name(line) else {
            if !line.trim_start().starts_with("//") {
                pending = None;
            }
            continue;
        };
        declared.push(Declaration {
            name: name.to_string(),
            path: pending.take(),
            line: index + 1,
        });
    }
    declared
}

/// One report per module name, and per `#[path]` target, that `declared` carries more than
/// once: either is a suite declared twice, and the first of them does not compile.
fn repeated_declarations(declared: &[Declaration]) -> Vec<String> {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut by_path: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for declaration in declared {
        by_name
            .entry(&declaration.name)
            .or_default()
            .push(declaration.line);
        if let Some(path) = &declaration.path {
            by_path.entry(path).or_default().push(declaration.line);
        }
    }
    let mut reports = Vec::new();
    for (name, lines) in by_name {
        if lines.len() > 1 {
            reports.push(format!(
                "case module {name:?}: expected exactly 1 declaration, got {} on lines \
                 {lines:?}",
                lines.len()
            ));
        }
    }
    for (path, lines) in by_path {
        if lines.len() > 1 {
            reports.push(format!(
                "case path {path:?}: expected exactly 1 declaration, got {} on lines \
                 {lines:?}",
                lines.len()
            ));
        }
    }
    reports
}

/// One report per declaration that does not carry a `#[path]` naming `all/<name>.rs`, or
/// whose named file is absent from `tests`.
fn unreachable_declarations(tests: &Path, declared: &[Declaration]) -> Vec<String> {
    let mut reports = Vec::new();
    for declaration in declared {
        let name = &declaration.name;
        let line = declaration.line;
        let expected = format!("all/{name}.rs");
        match &declaration.path {
            None => reports.push(format!(
                "case module {name:?} (line {line}): expected a `#[path = {expected:?}]` \
                 attribute above it, got none"
            )),
            Some(path) if *path != expected => reports.push(format!(
                "case module {name:?} (line {line}): expected the path {expected:?}, got \
                 {path:?}"
            )),
            Some(path) if !tests.join(path).is_file() => reports.push(format!(
                "case module {name:?} (line {line}): expected the suite file {path:?} to \
                 exist under tests/, got no such file"
            )),
            Some(_) => {}
        }
    }
    reports
}

/// This crate's declaration root, read from disk, or `None` where this crate has none.
///
/// The router declares the suites of its one integration-test target with `automod` from
/// the vendored `tests/all/main.rs`, so the hand-maintained root the rules of this suite
/// are read for need not exist here; where it does not, those rules have no subject in
/// this crate and the workspace-wide siblings read them for every crate that does have
/// one. A sibling suite checking another rule of the same file reads it from here rather
/// than opening it again.
pub(crate) fn all_rs() -> Option<String> {
    let path = tests_dir().join("all.rs");
    match fs::read_to_string(&path) {
        Ok(source) => Some(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

#[test]
fn every_suite_is_declared_exactly_once() {
    let Some(source) = all_rs() else {
        return;
    };
    let declared = declarations(&source);
    assert!(
        declared.len() > 200,
        "expected tests/all.rs to declare the router's suites, read {} declarations",
        declared.len()
    );
    let reports = repeated_declarations(&declared);
    assert!(
        reports.is_empty(),
        "expected each of the {} suites to be declared once, got {} declared twice, which \
         fails the `all` target with E0428: {reports:#?}",
        declared.len(),
        reports.len()
    );
}

#[test]
fn every_declaration_names_a_suite_file_that_exists() {
    let tests = tests_dir();
    let Some(source) = all_rs() else {
        return;
    };
    let declared = declarations(&source);
    let reports = unreachable_declarations(&tests, &declared);
    assert!(
        reports.is_empty(),
        "expected every declaration to point at a file under tests/all/, got {} that do \
         not: {reports:#?}",
        reports.len()
    );
}

/// The workspace root: this crate's manifest directory (`core/router`) sits two levels
/// below it.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("core/router lives two levels below the workspace root")
        .to_path_buf()
}

/// Every integration-test declaration root in the workspace: one `tests/all.rs` per
/// service crate that has one, plus this crate's own where it has one.
///
/// The rule the tests above pin belongs to the single-target layout rather than to this
/// crate, and the crate that breaks it cannot pin it itself: a repeated `mod` makes that
/// crate's own `all` target fail to compile, so no suite inside it is ever run to report
/// the repetition. This target is the one that links every service crate, so it is where
/// the rule can be checked for all of them. A sibling suite checking another rule of the
/// same layout reads the same roots, so this enumerator is shared rather than copied.
///
/// A root is listed only where it is a file: a crate whose target declares its suites
/// with `automod` keeps no hand-maintained root, and a rule about a root's contents has
/// nothing to read there.
pub(crate) fn declaration_roots() -> Vec<PathBuf> {
    let services = workspace_root().join("services");
    let entries = fs::read_dir(&services)
        .unwrap_or_else(|error| panic!("read {}: {error}", services.display()));
    let own = tests_dir().join("all.rs");
    let mut roots = Vec::new();
    if own.is_file() {
        roots.push(own);
    }
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|error| panic!("read an entry of {}: {error}", services.display()));
        let candidate = entry.path().join("tests").join("all.rs");
        if candidate.is_file() {
            roots.push(candidate);
        }
    }
    roots.sort();
    roots
}

/// Every suite file the declaration root `declaration_root` points a module at, as a path
/// under that crate's `tests/` directory.
///
/// These are the files, beside the root itself, that the crate's `all` target compiles, so a
/// sibling suite checking a rule about what such a file may contain reads them from here
/// rather than walking `tests/all/` — a file no root declares is compiled nowhere, and
/// [`every_declaration_names_a_suite_file_that_exists`] is what reports a declaration whose
/// file is absent, so one is skipped here instead of panicking.
pub(crate) fn declared_suite_files(declaration_root: &Path) -> Vec<PathBuf> {
    let source = fs::read_to_string(declaration_root)
        .unwrap_or_else(|error| panic!("read {}: {error}", declaration_root.display()));
    let tests = declaration_root
        .parent()
        .expect("a tests/all.rs has a tests/ parent");
    declarations(&source)
        .into_iter()
        .filter_map(|declaration| declaration.path)
        .map(|path| tests.join(path))
        .filter(|path| path.is_file())
        .collect()
}

#[test]
fn every_crate_in_the_workspace_declares_each_suite_once() {
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
        let crate_root = declaration_root
            .strip_prefix(&root)
            .unwrap_or(declaration_root)
            .display()
            .to_string();
        let source = fs::read_to_string(declaration_root)
            .unwrap_or_else(|error| panic!("read {}: {error}", declaration_root.display()));
        let declared = declarations(&source);
        let tests = declaration_root
            .parent()
            .expect("a tests/all.rs has a tests/ parent");
        if declared.is_empty() {
            reports.push(format!(
                "case {crate_root}: expected at least one declared suite, got none"
            ));
            continue;
        }
        for report in repeated_declarations(&declared)
            .into_iter()
            .chain(unreachable_declarations(tests, &declared))
        {
            reports.push(format!("case {crate_root}: {report}"));
        }
    }

    assert!(
        reports.is_empty(),
        "expected each of the {} declaration roots to declare every suite once and to name \
         a file that exists, got {} report(s): {reports:#?}",
        roots.len(),
        reports.len()
    );
}

#[test]
fn the_reader_reports_a_declaration_the_target_cannot_compile() {
    // One synthetic `all.rs` per way a declaration lands, with how many reports each half of
    // the check owes it. `present` is this suite's own file, so the existence half has a
    // file that really is there to accept.
    let present = "all_rs_declares_each_suite_once";
    let cases: [(&str, String, usize, usize); 16] = [
        (
            "one declaration",
            format!("#[path = \"all/{present}.rs\"]\nmod {present};\n"),
            0,
            0,
        ),
        (
            "the same name declared twice",
            format!(
                "#[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/{present}.rs\"]\nmod {present};\n"
            ),
            2,
            0,
        ),
        (
            "the same name declared three times",
            format!(
                "#[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/{present}.rs\"]\nmod {present};\n"
            ),
            2,
            0,
        ),
        (
            // The shape every collision in this file has taken: one copy in the sorted span
            // beside the suite's own name, a second appended to the tail under its own
            // explanatory comment block.
            "a sorted declaration repeated in the commented tail",
            format!(
                "// a sorted span\n#[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/boot_serve.rs\"]\nmod boot_serve;\n\
                 // Declared in the appended tail, which is where every new suite inserts\n\
                 // its lines.\n#[path = \"all/{present}.rs\"]\nmod {present};\n"
            ),
            2,
            0,
        ),
        (
            "two names pointed at one file",
            format!("#[path = \"all/{present}.rs\"]\nmod one;\n#[path = \"all/{present}.rs\"]\nmod two;\n"),
            1,
            2,
        ),
        (
            "a declaration with no path attribute",
            format!("mod {present};\n"),
            0,
            1,
        ),
        (
            "a path attribute naming another module's file",
            format!("#[path = \"all/{present}.rs\"]\nmod other_name;\n"),
            0,
            1,
        ),
        (
            "a path attribute outside all/",
            format!("#[path = \"{present}.rs\"]\nmod {present};\n"),
            0,
            1,
        ),
        (
            "a declaration of a file that does not exist",
            "#[path = \"all/absent_suite.rs\"]\nmod absent_suite;\n".to_string(),
            0,
            1,
        ),
        (
            "a commented-out declaration between the attribute and the mod",
            format!("#[path = \"all/{present}.rs\"]\n// mod {present};\nmod {present};\n"),
            0,
            0,
        ),
        (
            "an indented mod inside an item",
            format!(
                "#[path = \"all/{present}.rs\"]\nmod {present};\nfn helper() {{\n    mod inner;\n}}\n"
            ),
            0,
            0,
        ),
        // The spellings below declare the same module as a bare `mod <name>;` does, so each
        // repetition is the same E0428 and owes the same reports.
        (
            "one declaration carrying a trailing line comment",
            format!("#[path = \"all/{present}.rs\"]\nmod {present}; // the suite above\n"),
            0,
            0,
        ),
        (
            "the second declaration carrying a trailing line comment",
            format!(
                "#[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/{present}.rs\"]\nmod {present}; // appended in the tail\n"
            ),
            2,
            0,
        ),
        (
            "one declaration behind a visibility",
            format!("#[path = \"all/{present}.rs\"]\npub mod {present};\n"),
            0,
            0,
        ),
        (
            "the second declaration behind a scoped visibility",
            format!(
                "#[path = \"all/{present}.rs\"]\nmod {present};\n\
                 #[path = \"all/{present}.rs\"]\npub(crate) mod {present};\n"
            ),
            2,
            0,
        ),
        (
            "a run-together `pubmod`, which the compiler rejects",
            format!("#[path = \"all/{present}.rs\"]\npubmod {present};\n"),
            0,
            0,
        ),
    ];
    let tests = tests_dir();
    for case in &cases {
        let (name, source, repeated, unreachable) = case;
        let declared = declarations(source);
        let repeated_reports = repeated_declarations(&declared);
        assert_eq!(
            repeated_reports.len(),
            *repeated,
            "case {name:?}: expected {repeated} repetition report(s), got \
             {repeated_reports:#?}"
        );
        let unreachable_reports = unreachable_declarations(&tests, &declared);
        assert_eq!(
            unreachable_reports.len(),
            *unreachable,
            "case {name:?}: expected {unreachable} reachability report(s), got \
             {unreachable_reports:#?}"
        );
    }
}
