// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: `automod::dir!` declares one module per
//! `tests/all/<name>.rs`, so a new suite is a new file and nothing lists it.
//!
//! To group suites, add `tests/all/<group>.rs` containing exactly
//! `automod::dir!("tests/all/<group>");` and put them in `tests/all/<group>/`.

automod::dir!("tests/all");
