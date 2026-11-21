// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! `automod::dir!` declares one module per `tests/all/<name>.rs`, so a new suite is a
//! new file under `tests/all/` and nothing lists it. A suite left at the top level of
//! `tests/` fails the build.
automod::dir!("tests/all");
