// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is a suite
//! of it, declared by the macro below, so a new suite is a new file and nothing lists it.

automod::dir!("tests/all");
