// SPDX-License-Identifier: CC0-1.0

//! The `RecurrenceRule` every exclusion window this service reports carries, as AWS carries
//! it.
//!
//! An `ExclusionWindow` is reported with its recurrence rule built from the window's stored
//! recurrence: the structure is on EVERY window a read reports, and its `Expression` is the
//! cron or rate expression the window was added with — absent for a window that recurs on no
//! schedule. That is how the recorded `ListServiceLevelObjectiveExclusionWindows` 200 carries
//! a one-shot window: the `RecurrenceRule` member is present and states no expression.
//!
//! That is why the shape is declared here rather than beside the other shared shapes: the
//! published snapshot declares `Expression` `@required`, and that snapshot is kept as shipped
//! (see `smithy-model/LOCAL-ADDITIONS.md`), so the shape is generated from the small overlay
//! model `smithy-model/local-additions.json`, which restates it with that member optional —
//! against the same target, so the member's `length 1..1024` is captured exactly as the
//! snapshot spells it. Nothing about the wire is hand-written.
//!
//! The requirement the snapshot states is a requirement on a window that RECURS: a recurrence
//! rule carrying no expression is how this service states "this window recurs on no schedule",
//! on the way out and on the way in alike, so a window named back in the form a read reported
//! it in names the window the read reported ([`crate::state::recurrence_expression`]). An
//! expression a request does state is still held to the member's own `length 1..1024`, which
//! the overlay restates.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/local-additions.json");

/// The recurrence of one exclusion window: "a cron or rate expression that specifies the
/// schedule for the exclusion window".
///
/// `None` is a window that recurs on no schedule — the one-shot window a caller adds without
/// a recurrence, which this service reports the structure for all the same.
#[smithy_macros::smithy(shape = "com.amazonaws.applicationsignals#RecurrenceRule")]
#[derive(Debug, Clone, Default)]
pub struct RecurrenceRule {
    pub expression: Option<String>,
}
