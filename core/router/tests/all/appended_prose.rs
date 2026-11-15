// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module is empty on purpose, like `appended_header_notes.rs` and
//! `appended_notes.rs` and `appended_notes_tail.rs` beside it.
//!
//! It is where the copies of its own header that landings had appended PAST the declaration
//! list of `tests/all.rs` were taken out of that root and dropped. Appended there they were
//! inner doc comments and an inner attribute past the first item, which compiles no suite of
//! this target at all; they said nothing that root's header does not, and the guard suites
//! beside this one check every rule they restated, so they are gone rather than kept.
//!
//! The module stays declared because it is a file under `tests/all/`, which this crate's
//! build script requires a declaration for.
