// SPDX-License-Identifier: CC0-1.0

//! EMPTY — this file carries no test, the way the other empty suite files beside it do:
//! `automod` declares it as a module of this target with nothing in it.
//!
//! The rule it briefly read — that this crate's target is rooted at one `automod`
//! declaration of the directory beside it, and that every `.rs` file there is a suite of the
//! target — is pinned by [`super::router_test_target_is_automod_rooted`], which reads the
//! same layout more strictly and names each entry it is about.
