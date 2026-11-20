// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is one of
//! its suites, declared by `automod` and needing no declaration of its own.

automod::dir!("tests/all");
