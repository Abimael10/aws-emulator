// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`; nothing declares it — `automod` picks up
//! every file in this directory as one module per file, so module paths, test names and
//! `use crate::<sibling>` imports are the file names themselves.
//!
//! A suite left at the top level of `tests/` fails the build: each one would be a test
//! target of its own, statically linking this crate and its whole dependency closure.

automod::dir!("tests/all");
