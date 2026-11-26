// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is a
//! suite, declared here by `automod`, so a new suite is a new file and nothing else.

automod::dir!("tests/all");
