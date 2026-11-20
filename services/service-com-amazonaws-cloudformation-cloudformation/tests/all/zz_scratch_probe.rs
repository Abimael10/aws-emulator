// SPDX-License-Identifier: CC0-1.0

//! EMPTY — this file carries no test, so nothing here compiles or runs.
//!
//! It held a throwaway probe that read what a stack whose create failed with its state kept
//! answers next (the update, and the delete that follows it), used to locate where the
//! teardown of such a stack was being refused. What it established is now asserted by
//! `kept_failed_create_deployments.rs`. Its content is gone rather than the file because this
//! fixer's tool set can write a file but not remove one; the next change to touch this
//! directory should delete it.
