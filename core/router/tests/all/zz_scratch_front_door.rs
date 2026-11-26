// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch measurement, kept only because it could not be
//! deleted.
//!
//! It held a throwaway measurement of what one request costs over the started binary's
//! TLS front door, taken while tracing a report that attributed a client's timeout to the
//! emulator's latency. What it established is recorded in the resolved report rather than
//! as a test: how fast the host runs is not a rule a suite may assert.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write
//! a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
