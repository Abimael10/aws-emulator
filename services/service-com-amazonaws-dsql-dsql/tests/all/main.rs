// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is
//! compiled as a suite of it, declared by `automod` rather than by hand.
//!
//! Put a new suite at `tests/all/<name>.rs` and there is nothing to declare: the
//! macro below emits one `mod <name>;` per file, so module paths, test names and
//! `use crate::<sibling>` imports are what they were. A suite left at the top
//! level of `tests/` fails the build.

automod::dir!("tests/all");
