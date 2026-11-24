// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Every `tests/all/<name>.rs` beside this file is a suite of this one target, declared
//! by the `automod` line below — a new suite is a new file there and nothing else.
//!
//! A suite left at the top level of `tests/` fails the build.

automod::dir!("tests/all");
