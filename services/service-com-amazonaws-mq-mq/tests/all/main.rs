// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is a suite
//! of it, declared by `automod` rather than by hand.

automod::dir!("tests/all");
