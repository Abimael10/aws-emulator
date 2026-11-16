// SPDX-License-Identifier: CC0-1.0

//! Empty on purpose, and declared because the build script refuses a file under `tests/all/`
//! that no `mod` names.
//!
//! This file was added to read the "one header block" rule for every declaration root in the
//! workspace, which [`super::declaration_roots_carry_no_duplicated_comment_line`] now does —
//! for the roots whose declaration list is prose-free, the shape that rule can be read against
//! without reporting how a root is written. Its reader and the roots it reads are the sibling's
//! too, so the test here was removed rather than kept as a second copy of them.
