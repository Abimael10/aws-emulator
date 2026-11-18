// SPDX-License-Identifier: CC0-1.0

//! This crate's suites are declared by `automod::dir!`, and the crate that uses that macro
//! declares it as a dependency the committed `Cargo.lock` carries.
//!
//! A crate's `all` integration-test target names its suites in one of two ways. A service
//! crate keeps a declaration ROOT, `tests/all.rs`, holding one `#[path]`/`mod` pair per suite,
//! which every landing branch appends to by hand; this crate keeps `tests/all/main.rs`, which
//! holds `automod::dir!("tests/all")` and declares one module per `.rs` file of that directory
//! itself, so a new suite is a new file and no two landings write to one list. The build
//! script refuses a crate that keeps both roots, because cargo would compile two targets from
//! them.
//!
//! The macro is a dependency like any other, and the release build resolves with `--locked`:
//! a crate whose test root expands a macro the committed lock does not carry stops the build
//! before anything compiles, with `cannot update the lock file ... because --locked was
//! passed`. So the rule read here pairs the layout with the lock: whichever crate declares its
//! suites with the macro must declare that crate in a dependency table, and the lock must
//! carry it at a version the requirement accepts. The sibling
//! [`super::workspace_lockfile_covers_every_manifest`] reads that pairing for every dependency
//! of every manifest and owns both readers; this suite reads it for the one dependency a test
//! LAYOUT brings with it, which no manifest-shaped rule would connect to the layout that needs
//! it.
//!
//! It also pins what the change of layout means for the workspace-wide rules read over
//! declaration roots: a crate on the macro layout has no root, so the enumerator
//! [`super::all_rs_declares_each_suite_once::declaration_roots`] returns a file that is on
//! disk for every root it offers, and the rules about a hand-written list are read for the
//! crates that keep one.
//!
//! Each crate's own build script already refuses the pair of roots for ITS crate, and refuses
//! a suite file a hand-written root does not declare
//! (`emulator_core_build_support::check_integration_test_layout`, which this crate's
//! `build.rs` calls), and [`super::router_test_target_is_automod_rooted`] reads the layout of
//! THIS crate's root: that it holds the one call and nothing else, with no `tests/all.rs`
//! beside it, and that every file beside it is compiled under its own name. Those readings are
//! about one crate. What this suite adds, and does not repeat, is the workspace-wide view of
//! which layout each member keeps, the root that declares nothing — which no build-time check
//! reports — and the lock the macro layout needs.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// The roots the workspace-wide layout rules are read over, and the workspace root every
// sibling of this target reads paths against, from the sibling that owns them.
use crate::all_rs_declares_each_suite_once::{declaration_roots, workspace_root};
// The lock and the manifests are read through the sibling that owns one reader of each
// format, and the pairing of a declaration with the lock is that sibling's reading too, so
// this suite cannot disagree with it about what a manifest declares or what the lock carries.
use crate::workspace_lockfile_covers_every_manifest::{
    committed_lock, dependencies_in, is_member_dependency_table, lock_coverage_reports,
    locked_packages, resolve_declared, root_and_member_manifests, workspace_dependency_table,
    Declared, Locked,
};

/// The crate whose macro declares this crate's suites, as the lock and the manifests name it.
const DECLARING_MACRO_CRATE: &str = "automod";

/// How a crate's `all` integration-test target names its suites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// A hand-written `tests/all.rs` declaring one `#[path]`/`mod` pair per suite.
    DeclarationList,
    /// A `tests/all/main.rs` expanding `automod::dir!` over the directory beside it.
    Automod,
    /// A `tests/all/main.rs` that declares nothing: every suite beside it is compiled nowhere.
    SilentRoot,
    /// Both roots, which cargo compiles as two targets and the layout check refuses.
    Both,
    /// Neither root, so the crate has no `all` target at all.
    Absent,
}

/// The directory an `automod::dir!` call in `source` names, if it holds one.
///
/// The call is read as an item of the file it stands in, whatever indentation it is formatted
/// with: leading whitespace is layout, not meaning. What a comment of either style holds
/// declares nothing, so a line inside a block comment, and one opening with `//`, name no
/// directory. A root that reached the macro under another path — through a `use` of it, or an
/// alias — is read as declaring nothing, which is the report this suite makes: that root
/// declares its suites in a way nothing here pins.
///
/// [`super::router_test_target_is_automod_rooted`] owns the stronger rule for THIS crate's
/// root — that it holds the one call and nothing else, with no `tests/all.rs` beside it — and
/// is where that rule is read. This reader answers a different question, for any member of the
/// workspace: which layout a crate keeps, which is what the classification below is made of.
///
/// Shared with [`super::automod_rooted_suites_keep_their_inner_doc_comments_first`], which
/// walks the directories the calls of a macro-rooted target name to reach the files that
/// target compiles, so the two agree on what a call declares.
pub(crate) fn automod_directory(source: &str) -> Option<&str> {
    let mut commented = false;
    for line in source.lines() {
        let code = line.trim_start();
        if commented {
            commented = !code.contains("*/");
            continue;
        }
        let named = code
            .strip_prefix("automod::dir!(\"")
            .and_then(|rest| rest.split_once("\")"))
            .map(|(directory, _)| directory);
        if named.is_some() {
            return named;
        }
        if code.starts_with("/*") && !code.contains("*/") {
            commented = true;
        }
    }
    None
}

/// Which layout a crate keeps, from whether it has a `tests/all.rs` and the source of its
/// `tests/all/main.rs` when it has one.
fn layout_of(has_a_declaration_list: bool, main_rs: Option<&str>) -> Layout {
    match (has_a_declaration_list, main_rs) {
        (true, Some(_)) => Layout::Both,
        (_, Some(source)) if automod_directory(source).is_some() => Layout::Automod,
        (false, Some(_)) => Layout::SilentRoot,
        (true, None) => Layout::DeclarationList,
        (false, None) => Layout::Absent,
    }
}

/// Which layout the crate rooted at `crate_dir` keeps, read off the files it holds.
fn on_disk_layout(crate_dir: &Path) -> Layout {
    let tests = crate_dir.join("tests");
    let main_rs = fs::read_to_string(tests.join("all").join("main.rs")).ok();
    layout_of(tests.join("all.rs").is_file(), main_rs.as_deref())
}

/// The suites the crate rooted at `crate_dir` holds: every `.rs` file directly under
/// `tests/all/` other than the `main.rs` that declares them.
///
/// [`super::all_rs_carries_no_inner_doc_comment`] counts the same directory for one crate, to
/// pair a hand-written root's declarations with the files beside it; that reading has to count
/// every file the root owes a declaration for, so it cannot drop `main.rs`, and it is read for
/// one crate rather than for every member. This one answers "how many suites does this member
/// hold", for any member, so it leaves out the root that declares them and reads a directory
/// that is not there as no suites rather than as a panic.
fn suites_beside_the_root(crate_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(crate_dir.join("tests").join("all")) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().is_some_and(|kind| kind == "rs")
                && path.file_name().is_some_and(|name| name != "main.rs")
        })
        .count()
}

/// The declaration of `wanted` a manifest carries in any of its dependency tables, resolved
/// through the root `[workspace.dependencies]` when it defers there, or `None` when the
/// manifest declares it nowhere.
fn declaration_of(
    manifest: &str,
    wanted: &str,
    workspace_deps: &BTreeMap<String, Declared>,
) -> Option<Declared> {
    dependencies_in(manifest, is_member_dependency_table)
        .into_iter()
        .find(|(_table, name, _spec)| name == wanted)
        .and_then(|(_table, name, spec)| resolve_declared(&name, &spec, workspace_deps))
}

/// Every workspace member, as the path of its manifest relative to the workspace root, that
/// manifest's text, and the directory the manifest sits in.
fn members() -> Vec<(String, String, PathBuf)> {
    let root = workspace_root();
    let (_, manifests) = root_and_member_manifests();
    manifests
        .into_iter()
        .map(|(manifest_path, manifest)| {
            let crate_dir = root
                .join(&manifest_path)
                .parent()
                .expect("a member manifest sits in the crate's directory")
                .to_path_buf();
            (manifest_path, manifest, crate_dir)
        })
        .collect()
}

#[test]
fn every_crate_that_holds_suites_keeps_exactly_one_kind_of_declaration_root() {
    // The cases are the workspace's members that hold suites under `tests/all/`: this crate,
    // whose suites the macro declares, and every service crate, which declares its own by
    // hand. They differ in the layout they keep and in how many suites sit under it.
    let mut macro_crates = 0_usize;
    let mut list_crates = 0_usize;
    let mut reports = Vec::new();
    for (manifest_path, _manifest, crate_dir) in members() {
        let suites = suites_beside_the_root(&crate_dir);
        let layout = on_disk_layout(&crate_dir);
        match layout {
            // How many suites sit beside a root is the root's own business: a crate may keep
            // them all in grouped directories, which is a shape its own layout check reads.
            Layout::Automod => macro_crates += 1,
            Layout::DeclarationList => list_crates += 1,
            // No root and no suite beside one is a crate with no `all` target at all, which
            // breaks no rule; suites under a root the crate does not keep compile nowhere.
            Layout::Absent if suites == 0 => {}
            _ => reports.push(format!(
                "case {manifest_path:?}: expected a tests/all.rs declaring its suites or a \
                 tests/all/main.rs expanding the macro over them, got {layout:?} beside \
                 {suites} suite file(s)"
            )),
        }
    }

    assert!(
        reports.is_empty(),
        "expected every crate holding suites to keep exactly one kind of declaration root, \
         got {} that do not: {reports:#?}",
        reports.len()
    );
    // Without these, a workspace where neither layout was in use would satisfy the rule above
    // by holding nothing to break it. A floor rather than a census: a crate that migrates to
    // the macro layout moves from one of these counts to the other.
    assert!(
        macro_crates > 0,
        "expected at least one crate to declare its suites with the macro, read none beside \
         {list_crates} on the hand-written layout"
    );
    assert!(
        list_crates > 100,
        "expected one declaration list per service crate, read {list_crates}"
    );
}

#[test]
fn the_crate_declaring_its_suites_with_the_macro_has_it_locked() {
    let (root_manifest, _) = root_and_member_manifests();
    let workspace_deps = workspace_dependency_table(&root_manifest);
    let locked = locked_packages(&committed_lock());

    let mut read = 0_usize;
    let mut reports = Vec::new();
    for (manifest_path, manifest, crate_dir) in members() {
        if on_disk_layout(&crate_dir) != Layout::Automod {
            continue;
        }
        read += 1;
        let Some(declared) = declaration_of(&manifest, DECLARING_MACRO_CRATE, &workspace_deps)
        else {
            reports.push(format!(
                "case {manifest_path:?}: declares its suites with {DECLARING_MACRO_CRATE:?}, \
                 expected a […dependencies] table to declare that crate, got none"
            ));
            continue;
        };
        for report in lock_coverage_reports(DECLARING_MACRO_CRATE, &declared, &locked) {
            reports.push(format!("case {manifest_path:?}: {report}"));
        }
    }

    assert!(
        read > 0,
        "expected at least one crate to declare its suites with the macro, read none"
    );
    assert!(
        reports.is_empty(),
        "expected every crate on that layout to declare the macro crate and the lock to carry \
         it, got {} report(s): {reports:#?}",
        reports.len()
    );
}

#[test]
fn every_declaration_root_the_workspace_rules_read_belongs_to_a_crate_that_keeps_one() {
    // What the change of layout means for the rules read over declaration roots: the
    // enumerator offers a root only where the file is there, so what is left to read is the
    // crate it belongs to — one that keeps a hand-written list, rather than one on the macro
    // layout or one keeping both roots.
    let roots = declaration_roots();
    // A floor rather than a census: a crate that migrates to the macro layout takes one root
    // out of this walk.
    assert!(
        roots.len() > 100,
        "expected one declaration root per service crate, read {} of them",
        roots.len()
    );
    for root in &roots {
        let crate_dir = root
            .parent()
            .and_then(Path::parent)
            .expect("a tests/all.rs sits two levels below its crate");
        let case = root
            .strip_prefix(workspace_root())
            .unwrap_or(root)
            .display()
            .to_string();
        assert_eq!(
            on_disk_layout(crate_dir),
            Layout::DeclarationList,
            "case {case}: expected a root only for a crate that keeps a hand-written list"
        );
    }
}

#[test]
fn the_reader_classifies_the_layout_a_crate_keeps() {
    // One row per pair of roots a crate can hold, and the layout it is read as. The rows differ
    // in which roots are present, in whether the root expands the macro, in where the call
    // stands, and in whether the text that looks like one is a comment.
    let call = "automod::dir!(\"tests/all\");\n";
    let head = "// SPDX-License-Identifier: CC0-1.0\n\n//! The target.\n";
    let cases: [(&str, bool, Option<String>, Layout); 8] = [
        ("no root at all", false, None, Layout::Absent),
        ("a declaration list only", true, None, Layout::DeclarationList),
        ("a main.rs holding the call", false, Some(call.to_string()), Layout::Automod),
        (
            "a main.rs holding a header and the call",
            false,
            Some(format!("{head}\n{call}")),
            Layout::Automod,
        ),
        (
            // The vendored root names the directory in its header too; the call in column one
            // is the one that declares anything.
            "a header naming the call in prose, then the call",
            false,
            Some(format!("{head}//! `automod::dir!(\"tests/all\")` declares them.\n{call}")),
            Layout::Automod,
        ),
        (
            "a main.rs that declares nothing",
            false,
            Some(format!("{head}//! `automod::dir!(\"tests/all\")` would declare them.\n")),
            Layout::SilentRoot,
        ),
        (
            "both roots, which cargo compiles as two targets",
            true,
            Some(call.to_string()),
            Layout::Both,
        ),
        (
            "both roots, the second declaring nothing",
            true,
            Some(head.to_string()),
            Layout::Both,
        ),
    ];
    for case in &cases {
        let (name, has_a_list, main_rs, expected) = case;
        let actual = layout_of(*has_a_list, main_rs.as_deref());
        assert_eq!(
            actual, *expected,
            "case {name:?}: expected the layout {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn the_reader_reads_the_directory_the_call_names() {
    // Each form the call can take, and the directory it declares: the rows differ in the
    // directory named, in the whitespace around the call, in the kind of comment around it, and
    // in whether the text is a call at all. Where the call is the one this crate's root holds,
    // the stronger rule for that root — that it holds the call and nothing else — is
    // [`super::router_test_target_is_automod_rooted`]'s; these rows pin what the classifier
    // below reads for ANY member.
    let cases: [(&str, &str, Option<&str>); 11] = [
        ("nothing", "", None),
        (
            "the call on its own",
            "automod::dir!(\"tests/all\");\n",
            Some("tests/all"),
        ),
        (
            "a call naming a sub-directory",
            "automod::dir!(\"tests/all/sfn\");\n",
            Some("tests/all/sfn"),
        ),
        (
            "a call under a header",
            "//! The target.\n\nautomod::dir!(\"tests/all\");\n",
            Some("tests/all"),
        ),
        (
            "the call named in an inner doc comment",
            "//! `automod::dir!(\"tests/all\")` declares them.\n",
            None,
        ),
        (
            "the call commented out",
            "// automod::dir!(\"tests/all\");\n",
            None,
        ),
        (
            "an indented call, which the formatting of a file does not change the meaning of",
            "    automod::dir!(\"tests/all\");\n",
            Some("tests/all"),
        ),
        (
            "the call inside a block comment on one line",
            "/* automod::dir!(\"tests/all\"); */\n",
            None,
        ),
        (
            "the call inside a block comment that runs over three lines",
            "/*\nautomod::dir!(\"tests/all\");\n*/\n",
            None,
        ),
        (
            "a block comment above the call",
            "/*\nThe target.\n*/\nautomod::dir!(\"tests/all\");\n",
            Some("tests/all"),
        ),
        (
            "a one-line block comment above the call",
            "/* The target. */\nautomod::dir!(\"tests/all\");\n",
            Some("tests/all"),
        ),
    ];
    for (name, source, expected) in cases {
        let actual = automod_directory(source);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected the directory {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn the_lock_reader_reports_a_macro_crate_the_lock_does_not_cover() {
    // One row per way a declaration can reach the lock, against a lock carrying the macro crate
    // at 1.0.17 only. The rows differ in the requirement, in whether the declaration renames
    // the crate, and in whether the lock carries it at all. The reader is the sibling's, so
    // these rows pin the shared rule rather than a second copy of it.
    let locked: Locked = [(
        DECLARING_MACRO_CRATE.to_string(),
        vec!["1.0.17".to_string()],
    )]
    .into_iter()
    .collect();
    let cases: [(&str, Declared, usize); 6] = [
        (
            "the locked version, under a caret requirement",
            Declared {
                req: Some("1.0.17".to_string()),
                ..Declared::default()
            },
            0,
        ),
        (
            "a requirement on the major only",
            Declared {
                req: Some("1".to_string()),
                ..Declared::default()
            },
            0,
        ),
        (
            "a requirement the locked version is below",
            Declared {
                req: Some("1.0.18".to_string()),
                ..Declared::default()
            },
            1,
        ),
        (
            "a requirement in another major",
            Declared {
                req: Some("2".to_string()),
                ..Declared::default()
            },
            1,
        ),
        (
            "a path declaration carrying no requirement",
            Declared::default(),
            0,
        ),
        (
            // The lock knows a renamed declaration by the crate it points at, which this lock
            // does not carry.
            "a declaration renaming another crate",
            Declared {
                package: Some("absent-crate".to_string()),
                req: Some("1".to_string()),
                ..Declared::default()
            },
            1,
        ),
    ];
    for case in &cases {
        let (name, declared, expected) = case;
        let reports = lock_coverage_reports(DECLARING_MACRO_CRATE, declared, &locked);
        assert_eq!(
            reports.len(),
            *expected,
            "case {name:?}: expected {expected} report(s), got {reports:#?}"
        );
    }
}
