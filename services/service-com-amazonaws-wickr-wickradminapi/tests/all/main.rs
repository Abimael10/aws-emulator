// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` file beside this one is compiled as a suite; there is
//! nothing to declare when a suite is added.
automod::dir!("tests/all");
