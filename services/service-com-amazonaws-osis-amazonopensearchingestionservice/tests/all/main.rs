// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` is compiled as a suite of this one target: put a new suite
//! there and there is nothing to declare here.

automod::dir!("tests/all");
