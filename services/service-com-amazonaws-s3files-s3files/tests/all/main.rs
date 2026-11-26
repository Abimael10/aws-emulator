// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` is compiled as a suite of this one target, so a new suite is
//! a new file and nothing is declared here.

automod::dir!("tests/all");
