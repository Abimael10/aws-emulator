// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`: the declaration below compiles every one of
//! them as a module of this one binary, so nothing has to be listed by hand.
automod::dir!("tests/all");
