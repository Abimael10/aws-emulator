// SPDX-License-Identifier: CC0-1.0

//! This crate's single integration-test target, `all`.
//!
//! Every `tests/all/<name>.rs` is a suite of this target: `automod` declares one module per
//! `.rs` file in this directory, so a new suite is a new file and needs no declaration. Put
//! a new suite at `tests/all/<name>.rs`; to group suites, add `tests/all/<group>.rs` holding
//! exactly `automod::dir!("tests/all/<group>");` and put them in `tests/all/<group>/`.
//!
//! A file at the top level of `tests/`, or a `tests/all/<name>/mod.rs`, fails the build.

automod::dir!("tests/all");
