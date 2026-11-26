// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway probe of a drain written as receive-then-`DeleteMessageBatch`,
//! under a receive that sets its own visibility timeout and one that does not, written
//! while tracing a report about a queue drain that would not end. What it established —
//! that a batch delete removes exactly the messages its receipt handles name, whatever
//! visibility the receive asked for — is the rule
//! `services/service-com-amazonaws-sqs-amazonsqs/tests/` pins for that operation.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
