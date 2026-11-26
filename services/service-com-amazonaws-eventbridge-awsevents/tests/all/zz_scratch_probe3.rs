// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway probe of a queue policy whose `aws:SourceArn` condition names the
//! rule ARN the CLIENT constructs before the rule exists. What it established — that the
//! ARN this service mints for a rule of a custom bus is the one a client builds from the
//! documented form, so such a policy admits that rule's deliveries — is asserted in
//! `core/router/tests/all/events_empty_operand_matrix_on_the_assembled_router.rs`.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
