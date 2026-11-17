// SPDX-License-Identifier: CC0-1.0

//! Where this crate's suite declarations come from, and why no test here checks them.
//!
//! This crate's one test target is rooted at `tests/all/main.rs`, which holds
//! `automod::dir!("tests/all")`: it declares one module per `.rs` file in this directory, so
//! a new suite is a new file and nothing lists it. There is therefore no hand-kept module
//! list in this crate that could bind a name twice — a second `mod <name>;` is
//! `error[E0428]` and would take the whole `all` target with it, which is why such a rule
//! could never be checked from a suite inside the target it protects.
//!
//! What remains to be checked about the layout is checked from the one target that links
//! every service crate: `core/router/tests/all/router_suites_are_declared_by_automod.rs` and
//! its siblings read every crate's root and require each crate holding suites to keep one of
//! the two roots that compile them, and the build-time layout check
//! (`emulator_core_build_support`, run from this crate's `build.rs`) refuses a top-level
//! `tests/*.rs`, a second test target, and a file under `tests/all/` whose stem `automod`
//! could not use as a module name. This file holds no test of its own; it records where
//! those checks live for whoever adds the next suite beside it.
