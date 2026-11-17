// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`: `automod` declares one module per file here,
//! so nothing has to be declared by hand. To group suites, add `tests/all/<group>.rs`
//! holding exactly `automod::dir!("tests/all/<group>");` and put them in
//! `tests/all/<group>/`.
automod::dir!("tests/all");
