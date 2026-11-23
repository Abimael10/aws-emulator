// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs` and there is nothing to declare: the macro
//! below compiles one module per file in that directory.
//!
//! A suite left at the top level of `tests/` fails the build.

automod::dir!("tests/all");
