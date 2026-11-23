// SPDX-License-Identifier: CC0-1.0

//! An EMPTY module — the husk of a scratch trace, kept only because it could not be deleted.
//!
//! It briefly held a process-wide step log, written while tracing which deliveries the Auto
//! Scaling event path actually makes against a running emulator. What it established is pinned
//! by `core/router/tests/all/asg_launch_events_filtered_by_group_name.rs`, which drives the same
//! path through the public wire instead of instrumenting it.
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write a file
//! but not remove one. It is declared by nothing, so it is not compiled; the next change to touch
//! this directory should delete it.
