// SPDX-License-Identifier: CC0-1.0

//! REMOVED — this file is intentionally empty. It carries no test; it stays only because
//! `automod` declares one module per file under `tests/all/`, and an empty module compiles to
//! nothing.
//!
//! It briefly held a throw-away probe of an evaluation-driven metric alarm's notification on a
//! STARTED emulator — the shape a recorded warm-up scenario reaches an alarm's SNS action
//! through — written to find out whether the delivery really was absent. It was not: the
//! control alarm reached `ALARM` and its notification arrived on the subscribed queue within
//! seconds. What the probe established is now pinned, with rows spanning the wire, the queue's
//! grant, the time-stamp spelling and how much of the look-back was already filled, by
//! `all/cloudwatch_client_evaluation_driven_alarm_action_on_a_started_emulator.rs`.
//!
//! The content is gone rather than the file because this fixer's tool set can write a file but
//! not remove one; the next change to touch this directory should delete it.
