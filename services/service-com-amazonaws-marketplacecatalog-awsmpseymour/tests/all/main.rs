// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is a suite,
//! picked up automatically. Put a new suite there and declare nothing.

automod::dir!("tests/all");
