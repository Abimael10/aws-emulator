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
//! A second run of those copies had grown back below that root's last declaration — ordinary
//! comments this time, so the target compiled, but still prose in the list, which
//! `all_rs_is_a_canonical_declaration_list.rs` reports and which fails every suite of this
//! target. They are dropped here for the same reason as the first run.
//!
//! A THIRD run had grown back the same way, again as ordinary comments below the last
//! declaration, and two landings had left their own declaration pair inside it rather than in
//! the sorted list. The pairs were moved into the places their names sort to and the prose was
//! dropped, for the same reason as the two runs before it.
//!
//! A FOURTH run had grown back below that root's last declaration, with five landings' own
//! declaration pairs scattered through it. The pairs were moved into the places their names
//! sort to and the prose was dropped, for the same reason as the three runs before it.
//!
//! A FIFTH run had grown back the same way while this note was being written — each landing
//! that follows one of these cleanups re-appends the run — with several more landings' pairs
//! inside it, one of them a SECOND declaration of a suite already declared in the sorted list,
//! which compiles neither copy. Every such pair was moved into the place its name sorts to, the
//! duplicate was left at that one place, and the prose was dropped, for the same reason as the
//! four runs before it.
//!
//! A SIXTH run had grown back below that root's last declaration while this repair was under
//! review, again carrying a second declaration of a suite the sorted list already declared.
//! That stranded pair was dropped with the prose around it, the sorted one left where it is,
//! for the same reason as the five runs before it.
//!
//! The module stays declared because it is a file under `tests/all/`, which this crate's
//! build script requires a declaration for.
