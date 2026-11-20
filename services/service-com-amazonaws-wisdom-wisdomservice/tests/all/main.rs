// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs`: `automod::dir!` declares one module per
//! file, so a suite needs no declaration of its own.
automod::dir!("tests/all");
