// SPDX-License-Identifier: CC0-1.0

//! Superseded by `session_address_resolution.rs`.
//!
//! This suite asserted that a session read or delete resolved the session
//! against this runtime's own store and nothing else, which the model
//! contradicts: both operations document that the bot, the alias and the locale
//! a call addresses are resolved, so an address naming no alias of that bot or no
//! locale the alias's version can serve is refused AS the address rather than as
//! a session that is not there. That rule, the address rows that used to live
//! here and the round trip through a resolving address are asserted in
//! `session_address_resolution.rs`; nothing is left to assert here.
