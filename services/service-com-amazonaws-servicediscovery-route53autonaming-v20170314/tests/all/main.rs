// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs` and it is compiled as a module of this
//! target: `automod::dir!` declares one `mod <name>;` per file, so there is nothing to
//! declare by hand. A suite left at the top level of `tests/` fails the build.

automod::dir!("tests/all");
