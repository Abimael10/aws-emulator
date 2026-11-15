// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module is empty on purpose, like `appended_notes.rs` and
//! `appended_notes_tail.rs` beside it.
//!
//! It held the copies of its own module header that successive landings had appended PAST the
//! declaration list of `tests/all.rs`, where an inner doc comment is `E0753` and compiles no
//! suite of this target. They said nothing that root's header does not, and the guard suites
//! beside this one check the rules they restated, so they are gone rather than kept.
//!
//! The module stays declared because it is a file under `tests/all/`, which this crate's
//! build script requires a declaration for.
