// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` is one suite of it, declared by the line below rather
//! than by a hand-kept list, so adding a suite is adding the file.

automod::dir!("tests/all");
