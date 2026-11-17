// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`; `automod` declares one module per file
//! here, so nothing else has to be edited.
automod::dir!("tests/all");
