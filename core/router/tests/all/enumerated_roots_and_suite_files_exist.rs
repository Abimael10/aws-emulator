// SPDX-License-Identifier: CC0-1.0

//! What the declaration-root rules are read against is what the workspace HOLDS: every root
//! the enumerator answers is a file that exists and declares a suite.
//!
//! The crates of this workspace root their one `all` target in one of two ways. A crate that
//! HAND-DECLARES its suites keeps a `tests/all.rs` listing one `#[path]`/`mod` pair each, and
//! the `all_rs_*` and `declaration_roots_*` suites read the rules of that list and of its
//! header. A crate rooted at `tests/all/main.rs` declares nothing instead: `automod::dir!`
//! declares one module per `.rs` file beside it, which is how this crate's target is rooted
//! ([`super::router_test_target_is_automod_rooted`] pins that layout), and the build script
//! refuses a `tests/all.rs` standing next to that file.
//!
//! The rule read here is the one every one of those readings rests on: a rule about a
//! declaration list is read against the lists that are there, so the enumerator reports a path
//! only where the file is on disk. One that answers the path of a root no crate keeps makes
//! every suite reading it panic on a missing file instead of reporting anything about any
//! crate — which is what the whole target did when this crate's own root was removed and its
//! enumerator went on listing it.
//!
//! [`super::all_rs_declares_each_suite_once`] owns the enumerator; this suite pins what it
//! answers, root by root, against the filesystem.
//!
//! It reads files only; it writes nothing, starts no router and reaches no network.

use crate::all_rs_declares_each_suite_once::{
    declaration_roots, declared_suite_files, workspace_root,
};

#[test]
fn every_enumerated_declaration_root_is_a_file_on_disk() {
    // The cases are every root the enumerator answers — one per crate that hand-declares its
    // suites, differing in how many suites they declare, in how their declarations are spelled
    // and in how their header is written.
    let root = workspace_root();
    let roots = declaration_roots();
    // A floor rather than a census: a crate that migrates to the macro layout takes one root
    // out of this walk.
    assert!(
        roots.len() > 100,
        "expected one tests/all.rs per crate that hand-declares its suites under {}, read {} \
         of them",
        root.display(),
        roots.len()
    );
    for declaration_root in &roots {
        let case = declaration_root
            .strip_prefix(&root)
            .unwrap_or(declaration_root)
            .display()
            .to_string();
        assert!(
            declaration_root.is_file(),
            "case {case}: expected an enumerated declaration root to be a file on disk, got no \
             such file, which panics every suite that reads it"
        );
        let declared = declared_suite_files(declaration_root);
        assert!(
            !declared.is_empty(),
            "case {case}: expected the root to declare at least one suite file that exists, got \
             none"
        );
    }
}
