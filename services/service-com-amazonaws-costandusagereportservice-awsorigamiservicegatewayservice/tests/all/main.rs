// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`: the declaration below compiles every sibling
//! file of this directory as a suite of the one `all` binary, so a new suite needs no
//! declaration anywhere.

automod::dir!("tests/all");
