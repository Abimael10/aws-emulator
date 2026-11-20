// SPDX-License-Identifier: CC0-1.0

//! The WARM-UP window a metric alarm is created or updated with: when it ends, what it
//! withholds while it is in force, and what a read of an alarm inside it reports.
//!
//! "The configuration settings that define the warm-up behavior for an alarm. Use these
//! settings to delay alarm evaluation after you create or update the alarm […] During the
//! warm-up period, the alarm stays in `INSUFFICIENT_DATA` and does not perform alarm actions."
//! (`WarmUpConfiguration`) — so a warm-up is a span during which the alarm's own evaluation is
//! WITHHELD: nothing grades its series, so it moves nowhere, records no transition and owes no
//! notification, and the state the write left it in is the state every caller reads.
//!
//! Three rules, all of them the configuration's own:
//!
//! * the window lasts the configured span — "The length of the warm-up period, in minutes.
//!   After you create or update the alarm, the alarm stays in `INSUFFICIENT_DATA` for this
//!   duration." (`WarmUpConfiguration$WarmUpPeriodDurationInMinutes`) — measured from the write
//!   that configured it, and a later write does not bring back a window that has already passed:
//!   "You can change this value at any time, including after the warm-up period ends. If you
//!   change it after the warm-up period ends, the new value does not restart the warm-up
//!   period." ([`window_end_secs`]);
//! * it can end EARLY, unless the alarm asked to sit it out: "Specifies whether the alarm waits
//!   for the full warm-up period before it starts to evaluate. The default is `false`. If
//!   `true`, the alarm waits the entire `WarmUpPeriodDurationInMinutes` before it starts to
//!   evaluate, even if metric data arrives earlier. If `false`, the alarm ends the warm-up
//!   period early. Evaluation begins as soon as the alarm has enough metric data to fill its
//!   evaluation window." (`WarmUpConfiguration$OnlyStartEvaluatingAfterWarmUpPeriodEnds`) — the
//!   window its `EvaluationPeriods` ask for, filled with the datapoints the series really hold
//!   ([`State::evaluation_window_filled`]);
//! * while it is in force a READ of the alarm says so, as the evaluation state
//!   [`IN_WARM_UP`] — the one value a caller can tell a warming-up alarm from one that is
//!   simply being evaluated by, and the one a client waiting for the warm-up to pass polls for.
//!   RECORDED behaviour: the modelled `EvaluationState` enum declares the three values a LOG
//!   alarm's query execution reports (`smithy_query(query="shape",shape="EvaluationState")`) and
//!   not this one, which CloudWatch serves all the same, so the value is written as the string
//!   it is rather than clamped to that enum.
//!
//! The window is a stored INSTANT ([`crate::state::StoredAlarm::warm_up_ends_secs`]) and is
//! never slept on: the evaluation the clock has already made due is the one that finds the
//! warm-up over and hands the alarm back to its own grading.

use super::{
    evaluated_periods, evaluation_range_periods, graded_window_secs, EvaluationBudget,
    EvaluationGrid, State,
};
use crate::ops::types::{MetricAlarm, WarmUpConfiguration};
use crate::state::StoredAlarm;

/// The `EvaluationState` an alarm inside its warm-up window reports.
pub const IN_WARM_UP: &str = "IN_WARM_UP";

/// How many seconds in a minute, the unit a warm-up period is configured in
/// (`WarmUpConfiguration$WarmUpPeriodDurationInMinutes`).
const SECS_PER_MINUTE: f64 = 60.0;

/// How long the warm-up `configured` asks for lasts, in seconds.
///
/// The member is modelled `@range 1..2880`, so a validated configuration names a positive
/// number of minutes; a value outside it names no span at all rather than a negative one.
fn configured_secs(configured: &WarmUpConfiguration) -> f64 {
    f64::from(configured.warm_up_period_duration_in_minutes.max(0)) * SECS_PER_MINUTE
}

/// The instant, in epoch seconds, at which the warm-up of the alarm a write stores ends, or
/// `None` for a write that configures no warm-up at all.
///
/// `previous` is the alarm as the store holds it BEFORE this write, which is what decides
/// whether the write starts a window or only re-measures the one that is already standing:
///
/// * a write that configures a warm-up on an alarm standing in none starts one at itself, for
///   the span it names — "After you create or update the alarm, the alarm stays in
///   `INSUFFICIENT_DATA` for this duration";
/// * a write made while a window is still in force keeps that window's START and measures the
///   span it names from there, so changing the duration mid-warm-up lengthens or shortens the
///   window the alarm is already in rather than beginning a second one;
/// * a write made after the window has passed leaves it passed, whatever span it names: "If you
///   change it after the warm-up period ends, the new value does not restart the warm-up
///   period."
#[must_use]
pub fn window_end_secs(
    previous: Option<&StoredAlarm>,
    configured: Option<&WarmUpConfiguration>,
    now_secs: f64,
) -> Option<f64> {
    let span = configured_secs(configured?);
    let standing = previous.and_then(|stored| {
        stored.warm_up_ends_secs.map(|ends| {
            let previous_span = stored
                .alarm
                .warm_up_configuration
                .as_ref()
                .map_or(0.0, configured_secs);
            (ends, previous_span)
        })
    });
    match standing {
        // The window the alarm is already in, re-measured from the instant it began.
        Some((ends, previous_span)) if now_secs < ends => Some(ends - previous_span + span),
        // A window that has passed stays passed.
        Some((ends, _)) => Some(ends),
        None => Some(now_secs + span),
    }
}

/// Whether a warm-up window ending at `ends_secs` is in force at `now_secs`.
///
/// The one reading of that instant, so the write that reports [`IN_WARM_UP`] and the evaluation
/// that is withheld by the same window cannot disagree about whether it is standing.
#[must_use]
pub fn in_force(ends_secs: Option<f64>, now_secs: f64) -> bool {
    ends_secs.is_some_and(|ends| now_secs < ends)
}

/// The `EvaluationState` an alarm whose warm-up window ends at `ends_secs` reports at
/// `now_secs`: [`IN_WARM_UP`] while that window is in force, and nothing once it is over.
#[must_use]
pub fn evaluation_state(ends_secs: Option<f64>, now_secs: f64) -> Option<String> {
    in_force(ends_secs, now_secs).then(|| IN_WARM_UP.to_string())
}

impl State {
    /// Whether the warm-up of the metric alarm `name` WITHHOLDS the evaluation that is now due,
    /// materialising the end of a window the clock — or the data — has already made due.
    ///
    /// `true` leaves the alarm exactly as it stands: no grading, so no transition, no history
    /// item, no action and no notification, which is what "the alarm stays in
    /// `INSUFFICIENT_DATA` and does not perform alarm actions" says. `false` hands the alarm
    /// back to its own evaluation, and clears the warm-up evaluation state it was reporting.
    pub(super) fn warm_up_withholds_evaluation(
        &mut self,
        name: &str,
        now_secs: f64,
        budget: &mut EvaluationBudget,
    ) -> bool {
        let Some(stored) = self.alarms.get(name) else {
            return false;
        };
        if !in_force(stored.warm_up_ends_secs, now_secs) {
            // Either the alarm is configured with no warm-up at all, or the one it had has
            // passed: the span the window stood for is over, so the state that reported it is
            // too.
            self.end_warm_up(name, None);
            return false;
        }
        let alarm = stored.alarm.clone();
        // "If `true`, the alarm waits the entire `WarmUpPeriodDurationInMinutes` before it
        // starts to evaluate, even if metric data arrives earlier."
        let sits_out_the_period = alarm
            .warm_up_configuration
            .as_ref()
            .and_then(|configured| configured.only_start_evaluating_after_warm_up_period_ends)
            .unwrap_or(false);
        if sits_out_the_period || !self.evaluation_window_filled(&alarm, now_secs, budget) {
            return true;
        }
        // "If `false`, the alarm ends the warm-up period early. Evaluation begins as soon as the
        // alarm has enough metric data to fill its evaluation window." — the window ends HERE,
        // so a later write that only re-states the configured duration does not bring it back.
        self.end_warm_up(name, Some(now_secs));
        false
    }

    /// Record that the warm-up of the alarm `name` is over: the evaluation state it reported is
    /// cleared, and a window that ended EARLY (`ended_secs`) is remembered as having ended then.
    ///
    /// Only the warm-up's own evaluation state is cleared: any other value the alarm reports
    /// there was put on it by something else and is not this window's to drop.
    fn end_warm_up(&mut self, name: &str, ended_secs: Option<f64>) {
        let Some(stored) = self.alarms.get_mut(name) else {
            return;
        };
        if let Some(ended) = ended_secs {
            stored.warm_up_ends_secs = Some(ended);
        }
        if stored.alarm.evaluation_state.as_deref() == Some(IN_WARM_UP) {
            stored.alarm.evaluation_state = None;
        }
    }

    /// Whether the series `alarm` grades hold enough data to FILL its evaluation window: as many
    /// real datapoints over the windows of its evaluation range as its `EvaluationPeriods` asks
    /// for ("Evaluation begins as soon as the alarm has enough metric data to fill its evaluation
    /// window", `WarmUpConfiguration$OnlyStartEvaluatingAfterWarmUpPeriodEnds`).
    ///
    /// Counted over the same range, on the same grid and with the same fold the evaluation itself
    /// grades, so "enough data" means the datapoints that evaluation would really have. An alarm
    /// this engine resolves no series for has none of them, so its warm-up stands for the whole
    /// span it was configured with rather than ending on data nothing graded.
    fn evaluation_window_filled(
        &self,
        alarm: &MetricAlarm,
        now_secs: f64,
        budget: &mut EvaluationBudget,
    ) -> bool {
        let Some(evaluation_periods) = evaluated_periods(alarm) else {
            return false;
        };
        let grid = EvaluationGrid::of(alarm, now_secs);
        let watched = self.watched_data(alarm, now_secs, budget);
        let range_periods = evaluation_range_periods(
            evaluation_periods,
            graded_window_secs(alarm),
            watched.storage_span,
            grid,
            now_secs,
        );
        let Some(graded) = self.graded_series(alarm, range_periods, grid, now_secs, budget) else {
            return false;
        };
        graded
            .windows
            .iter()
            .filter(|window| window.is_some())
            .count()
            >= evaluation_periods
    }
}
