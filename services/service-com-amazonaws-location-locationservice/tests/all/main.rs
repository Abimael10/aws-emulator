// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! A new suite is a new `tests/all/<name>.rs` file: `automod` declares one module
//! per file, so nothing is declared by hand. A suite left at the top level of
//! `tests/` fails the build.

automod::dir!("tests/all");
