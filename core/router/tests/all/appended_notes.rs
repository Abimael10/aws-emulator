// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module is empty on purpose.
//!
//! It held thousands of lines of notes that successive landings had appended to the END of
//! `tests/all.rs` — second declarations of suites already declared above, and fragments of
//! that file's own module header — each recording its own removal. They said nothing the
//! header of `tests/all.rs` does not, and the two `all_rs_*` guard suites check the two
//! rules they repeated, so they are gone rather than moved again. The declaration list in
//! `tests/all.rs` is the only content that root holds.
//!
//! The module stays declared because it is a file under `tests/all/`, which this crate's
//! build script requires a declaration for.
