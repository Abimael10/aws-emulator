// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch measurement, kept only because it could not be
//! deleted.
//!
//! It held a throwaway comparison of the per-request claim scan against an allocation-free
//! form of the same match, taken while tracing a report that attributed a client's timeout
//! to that scan. What it established — that the two cost the same, so the scan is not what
//! a client's timeout is made of — is recorded in the resolved report rather than as a
//! test: how fast the host runs is not a rule a suite may assert.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
