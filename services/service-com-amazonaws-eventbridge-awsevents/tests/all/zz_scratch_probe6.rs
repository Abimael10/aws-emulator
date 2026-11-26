// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway probe of several long-polling receives in flight at once. What it
//! established — that a receive that waits holds nothing but its own request open, so
//! several consumers watching several queues wait side by side rather than one after
//! another — is the rule
//! `core/router/tests/all/sqs_long_poll_does_not_block_the_emulator.rs` pins on the
//! started binary.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
