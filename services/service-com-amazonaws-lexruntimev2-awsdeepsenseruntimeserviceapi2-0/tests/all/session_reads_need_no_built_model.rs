// SPDX-License-Identifier: CC0-1.0

//! Superseded by `session_address_resolution.rs`.
//!
//! This suite asserted that reading or removing a session needed no model behind
//! the locale it is addressed in, which the model contradicts: `GetSession` and
//! `DeleteSession` document the same resolution of the bot, the alias and the
//! locale that the three calls interpreting input document, so an address the
//! runtime cannot serve is refused AS the address by all five. The rule, and the
//! unbuilt-locale rows that used to live here, are asserted in
//! `session_address_resolution.rs`; nothing is left to assert here.
