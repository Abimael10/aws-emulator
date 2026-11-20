// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`; nothing declares it — `automod` picks up
//! every sibling file in this directory as a suite of this one target.
automod::dir!("tests/all");
