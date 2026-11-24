// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! A new suite is a new `tests/all/<name>.rs` file and there is nothing to declare:
//! `automod` declares one module per file beside this one, so a suite cannot be added and
//! then forgotten.
//!
//! A suite left at the top level of `tests/` fails the build.

automod::dir!("tests/all");
