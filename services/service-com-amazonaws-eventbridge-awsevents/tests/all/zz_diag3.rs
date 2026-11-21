// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway reproduction of a function target invoked under the target's `RoleArn`
//! whose handler echoes `detail.requestId` onto a queue, written while tracing that report.
//! What it established now lives in
//! `tests/all/role_granted_function_target_runs_its_handler.rs`, which states the rule as a
//! table over what the role trusts and what it is allowed.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write a
//! file but not remove one. `automod` declares it as an empty module, which runs no test; the
//! next change to touch this directory should delete it.
