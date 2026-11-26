// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway probe of a rule scoped to the publication's `source` alone and of
//! a `PutRule` that names no `State`. What it established — that a rule carrying no
//! `detail` clause matches every event of its source, and that a rule created without a
//! `State` matches published events as an `ENABLED` one does — is asserted by the control
//! row of `core/router/tests/all/events_empty_operand_matrix_on_the_assembled_router.rs`
//! and by this crate's `tests/all/rule_state_control.rs`.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
