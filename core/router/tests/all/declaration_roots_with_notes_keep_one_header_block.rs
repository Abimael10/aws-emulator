// SPDX-License-Identifier: CC0-1.0

//! Empty on purpose, and declared because the build script refuses a file under `tests/all/`
//! that no `mod` names.
//!
//! This file was added to read the "one header block" and "no header fragment" rules for the
//! declaration roots that keep a note among their declarations, the one class the two siblings
//! owning those rules passed over. Both rules read the block ahead of a root's first item,
//! where a line of the list contributes nothing, so such a root was never a case either rule
//! had to be read differently for: the siblings now read each of them for every root on disk,
//! as [`super::declaration_roots_state_each_header_paragraph_once`] already did one paragraph
//! at a time, and the tests here were removed rather than kept as a second copy of their
//! readers and their roots.
