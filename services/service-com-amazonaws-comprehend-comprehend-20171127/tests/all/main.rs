// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate, `all`.
//!
//! Every `tests/all/<name>.rs` is a suite of this target: `automod` declares one
//! module per `.rs` file in this directory, so a new suite is a new file and needs no
//! declaration. Put a new suite at `tests/all/<name>.rs`.
//!
//! A suite left at the top level of `tests/`, or a `tests/all/<name>/mod.rs`, fails
//! the build.

automod::dir!("tests/all");
