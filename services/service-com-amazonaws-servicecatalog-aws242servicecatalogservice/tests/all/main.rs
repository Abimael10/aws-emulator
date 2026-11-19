// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` beside
//! this file is compiled as a suite of it, with no declaration to keep in step.

automod::dir!("tests/all");
