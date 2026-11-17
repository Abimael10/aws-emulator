// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` is
//! one suite of it, picked up from its file with no declaration to keep in sync.
automod::dir!("tests/all");
