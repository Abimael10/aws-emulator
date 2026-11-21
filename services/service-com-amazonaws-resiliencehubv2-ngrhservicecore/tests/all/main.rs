// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate: every `tests/all/<name>.rs` beside this
//! file is compiled as a suite of the one `all` binary, so a new suite is a new file there
//! and there is nothing to declare.

automod::dir!("tests/all");
