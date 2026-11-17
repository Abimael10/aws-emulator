// SPDX-License-Identifier: CC0-1.0

//! Every suite FILE under a crate's `tests/all/` is declared by its `tests/all.rs`.
//!
//! A suite no declaration names is compiled nowhere and its tests never run: it passes by not
//! existing, which is the one failure a reviewer cannot see in a diff and no compiler reports.
//!
//! Each crate's own build script refuses that for ITS crate
//! (`emulator_core_build_support::check_integration_test_layout`, which this crate's
//! `build.rs` calls), so the rule is enforced crate by crate at build time and is not
//! re-implemented here. What this suite adds is the WORKSPACE view: one report naming every
//! crate whose declaration root has fallen behind its directory, read in one place rather
//! than one crate at a time — the same reason the sibling
//! [`super::all_rs_declares_each_suite_once`] checks ITS half of the rule
//! (every declaration unique and naming a file that exists) for every crate's root from this
//! target, which is the one target that links them all.
//!
//! It reads files only; it starts no router and reaches no network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::all_rs_declares_each_suite_once::{declaration_roots, declared_suite_files};

/// Every `*.rs` file directly under one crate's `tests/all/` directory — the suites that
/// crate's `tests/all.rs` has to declare.
///
/// A sub-DIRECTORY is skipped: a module directory is declared by its own parent module, not by
/// the crate root, so the scan stops at this level (the build-time check scans the same one).
fn suite_files(declaration_root: &Path) -> BTreeSet<PathBuf> {
    let directory = declaration_root
        .parent()
        .expect("a tests/all.rs has a tests/ parent")
        .join("all");
    let Ok(entries) = fs::read_dir(&directory) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|kind| kind == "rs"))
        .collect()
}

#[test]
fn every_crate_declares_every_suite_file_it_holds() {
    let roots = declaration_roots();
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per service crate, read {} of them",
        roots.len()
    );

    let mut reports = Vec::new();
    let mut declared_total = 0_usize;
    for root in &roots {
        // The files the root points a module at, read through the same reader the sibling
        // guard reports a declaration with, so the two cannot disagree about what is declared.
        let declared: BTreeSet<PathBuf> = declared_suite_files(root).into_iter().collect();
        declared_total += declared.len();
        for undeclared in suite_files(root).difference(&declared) {
            reports.push(format!(
                "case {}: the suite {} is declared nowhere, so nothing compiles or runs it",
                root.display(),
                undeclared
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ));
        }
    }
    assert!(
        declared_total > 200,
        "expected the workspace's declaration roots to declare its suites, read {declared_total}"
    );
    assert!(
        reports.is_empty(),
        "expected every suite file of every crate to be declared in that crate's tests/all.rs, \
         got {} that is/are not: {reports:#?}",
        reports.len()
    );
}
