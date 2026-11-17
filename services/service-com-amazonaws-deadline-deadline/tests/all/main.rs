// SPDX-License-Identifier: CC0-1.0

//! The crate's one integration-test target: every `tests/all/<name>.rs` beside this file
//! is a suite of it, declared by `automod`, so a new suite is a new file and nothing else.
automod::dir!("tests/all");
