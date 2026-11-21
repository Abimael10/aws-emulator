// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` is a suite of this one target: `automod` declares one
//! module per file, so a new suite needs nothing but the file.
//!
//! A suite left at the top level of `tests/` fails the build.

automod::dir!("tests/all");
