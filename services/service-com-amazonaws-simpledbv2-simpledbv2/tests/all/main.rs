// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is one
//! suite, declared automatically.
//!
//! Put a new suite at `tests/all/<name>.rs` — there is nothing to declare here.

automod::dir!("tests/all");
