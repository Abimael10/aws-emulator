// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: `automod` compiles every
//! `tests/all/<name>.rs` beside this file as a suite, so a new suite is a new file and
//! nothing has to be declared.

automod::dir!("tests/all");
