// SPDX-License-Identifier: CC0-1.0

//! REMOVED — this file is intentionally empty. It carries no test; it stays only because
//! `automod` declares one module per file under `tests/all/`, and an empty module compiles to
//! nothing.
//!
//! It briefly held a throw-away probe that drove the recorded warm-up scenario — a breaching
//! custom series, a control alarm with no `WarmUpConfiguration` and two gated arms, all acting
//! on one SNS topic with an SQS subscriber — in process over both of this service's wires, to
//! find out whether the control's notification really was absent. It was not: the control
//! reached `ALARM` and its notification arrived on the queue, over the `awsQuery` form wire as
//! well as the RPC one. The chain is pinned end to end, against the emulator as a client starts
//! it, by `core/router/tests/all/cloudwatch_client_evaluation_driven_alarm_action_on_a_started_emulator.rs`.
//!
//! The content is gone rather than the file because this fixer's tool set can write a file but
//! not remove one; the next change to touch this directory should delete it.
