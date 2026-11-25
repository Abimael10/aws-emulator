// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` beside
//! this file is compiled as a suite, so a new suite is a new file and nothing else.
automod::dir!("tests/all");
