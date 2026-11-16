// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module is empty on purpose, like `appended_notes.rs` and
//! `appended_notes_tail.rs` beside it.
//!
//! It held the copies of its own module header that successive landings had appended PAST the
//! declaration list of `tests/all.rs`, where an inner doc comment is `E0753` and compiles no
//! suite of this target. They said nothing that root's header does not, and the guard suites
//! beside this one check the rules they restated, so they are gone rather than kept.
//!
//! A later run of the same copies had grown INSIDE that root's header, above its first item,
//! where no guard suite looks: seven re-typed or truncated repetitions of its opening
//! paragraphs, each restating a rule the suites beside this one already check. The header was
//! reduced to one copy of each paragraph and they are gone rather than kept, for the same
//! reason as the run before them.
//!
//! A second such run grew back inside that header while the first repair was being reviewed,
//! and was reduced the same way. So did a third, which had left seven more copies and several
//! lines truncated mid-sentence; it was reduced to the one copy of each paragraph that stands
//! there now, for the same reason as the runs before it.
//!
//! A THIRD such run came of merging two branches that had each reduced the header: the merge
//! kept both reductions, so the block said each paragraph twice over. It was reduced to one
//! copy of each paragraph again, which is the rule `all_rs_carries_one_header_block.rs` reads.
//!
//! A FOURTH run had grown by the time the suite that reads the rule ran again: eight more
//! re-typed copies of those paragraphs inside the header, two hundred more lines of them past
//! the last declaration — where `all/appended_notes_tail.rs` and `all/appended_prose.rs` record
//! the earlier ones — and one declaration pair appended after that prose instead of into the
//! sorted list. The header was reduced to one copy of each paragraph again, the appended prose
//! is gone rather than kept, and the pair stands where its name sorts to, for the same reason as
//! the runs before it.
//!
//! A FIFTH run came the way the third had: two branches that had each reduced the header were
//! merged, and the merge kept all three reductions one after another, so two lines of the block
//! stood three times over and the target's own guard suite reported them. It was reduced to one
//! copy of each paragraph again, which is the rule `all_rs_carries_one_header_block.rs` reads.
//!
//! The module stays declared because it is a file under `tests/all/`, which this crate's
//! build script requires a declaration for.
