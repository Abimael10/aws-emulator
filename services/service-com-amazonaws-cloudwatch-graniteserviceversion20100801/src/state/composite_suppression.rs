// SPDX-License-Identifier: CC0-1.0

//! Whether a composite alarm's ACTIONS are suppressed, and by which of the modelled reasons.
//!
//! A composite alarm may name another alarm as its ACTIONS SUPPRESSOR: "Actions will be
//! suppressed if the suppressor alarm is in the ALARM state."
//! (`CompositeAlarm$ActionsSuppressor`). Suppression does not change the composite alarm's own
//! state — the rule decides that — it withholds the actions that state would otherwise fire,
//! and the alarm REPORTS which of the modelled reasons is withholding them
//! (`CompositeAlarm$ActionsSuppressedBy`, whose values are `WaitPeriod`, `ExtensionPeriod` and
//! `Alarm`).
//!
//! Two of those reasons follow one after the other, which is what this module decides:
//!
//! * while the suppressor is in `ALARM`, the reason is `Alarm` — "when the value is ALARM, it
//!   means that the actions are suppressed because the suppressor alarm is in ALARM";
//! * once the suppressor LEAVES `ALARM`, the suppression does not end with it but holds for the
//!   extension period the alarm was configured with ("the maximum time in seconds that the
//!   composite alarm waits after suppressor alarm goes out of the ALARM state",
//!   `CompositeAlarm$ActionsSuppressorExtensionPeriod`), reported as `ExtensionPeriod`;
//! * when that period has passed, nothing suppresses the actions and the alarm reports no
//!   reason at all.
//!
//! The DURATION is the caller's own, taken from the member it configured the alarm with rather
//! than chosen here, and the instant the period ends is stored on the alarm by the evaluation
//! that opens it — so the suppression is a pure function of that instant and the clock, and
//! whichever call materialises an evaluation, every later caller sees the same thing.

use super::{ACTIONS_SUPPRESSED_BY_ALARM, ALARM_STATE_ALARM};

/// `ExtensionPeriod`, the modelled `ActionsSuppressedBy` value for the window that follows the
/// suppressor leaving `ALARM` (`CompositeAlarm$ActionsSuppressedBy`).
pub const ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD: &str = "ExtensionPeriod";

/// What one evaluation decides about a composite alarm's action suppression.
#[derive(Debug, Clone, PartialEq)]
pub struct Suppression {
    /// The modelled reason the actions are suppressed, or `None` when nothing suppresses them.
    pub suppressed_by: Option<&'static str>,
    /// Why they are suppressed, as `ActionsSuppressedReason` reports it.
    pub reason: Option<String>,
    /// The instant the extension period in force ends, carried on the alarm until it passes.
    pub extension_until_secs: Option<f64>,
}

impl Suppression {
    /// Nothing suppresses this alarm's actions.
    fn none() -> Self {
        Self {
            suppressed_by: None,
            reason: None,
            extension_until_secs: None,
        }
    }
}

/// What the actions of a composite alarm are suppressed by at `now_secs`.
///
/// `suppressor` is the alarm named as the actions suppressor and `suppressor_state` the state
/// that alarm is in right now, as this store holds it; `extension_period_secs` is the alarm's
/// own `ActionsSuppressorExtensionPeriod`; `reported` is the reason the alarm reports as this
/// evaluation begins and `extension_until_secs` the extension instant it carries, so the
/// evaluation that sees the suppressor leave `ALARM` is the one that opens the period.
#[must_use]
pub fn suppression(
    suppressor: Option<&str>,
    suppressor_state: Option<&str>,
    extension_period_secs: Option<i32>,
    reported: Option<&str>,
    extension_until_secs: Option<f64>,
    now_secs: f64,
) -> Suppression {
    let Some(suppressor) = suppressor else {
        // An alarm with no suppressor suppresses nothing, whatever it once carried.
        return Suppression::none();
    };
    let named = super::alarm_rule_reference_name(suppressor);
    if suppressor_state == Some(ALARM_STATE_ALARM) {
        return Suppression {
            suppressed_by: Some(ACTIONS_SUPPRESSED_BY_ALARM),
            reason: Some(format!("Actions suppressed by alarm {named}")),
            // The extension period runs from the instant the suppressor LEAVES this state, so
            // nothing is owed while it is still in it.
            extension_until_secs: None,
        };
    }
    // The suppressor is not in ALARM. The extension period opens the first time an evaluation
    // sees that while the actions were suppressed BY that alarm, and the instant it ends is
    // what every later evaluation reads — so the window is measured from the transition rather
    // than from whichever call happens to look next.
    let until = match extension_until_secs {
        Some(until) => until,
        None if reported == Some(ACTIONS_SUPPRESSED_BY_ALARM) => {
            now_secs + f64::from(extension_period_secs.unwrap_or_default().max(0))
        }
        None => return Suppression::none(),
    };
    if now_secs >= until {
        return Suppression::none();
    }
    Suppression {
        suppressed_by: Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD),
        reason: Some(format!(
            "Actions suppressed by extension period of alarm {named}"
        )),
        extension_until_secs: Some(until),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the reason follows the suppressor — `Alarm` while it is in ALARM, then
    /// `ExtensionPeriod` for the configured period after it leaves, then nothing.
    #[test]
    fn the_reason_follows_the_suppressor_and_then_the_extension_period() {
        let period = Some(60);
        let in_alarm = suppression(Some("mute"), Some(ALARM_STATE_ALARM), period, None, None, 100.0);
        assert_eq!(
            in_alarm.suppressed_by,
            Some(ACTIONS_SUPPRESSED_BY_ALARM),
            "case \"a suppressor in ALARM\": expected the Alarm reason, got {in_alarm:?}"
        );

        let left = suppression(
            Some("mute"),
            Some("OK"),
            period,
            in_alarm.suppressed_by,
            in_alarm.extension_until_secs,
            100.0,
        );
        assert_eq!(
            (left.suppressed_by, left.extension_until_secs),
            (Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD), Some(160.0)),
            "case \"the suppressor leaving ALARM\": expected the extension period to open, got \
             {left:?}"
        );

        let inside = suppression(
            Some("mute"),
            Some("OK"),
            period,
            left.suppressed_by,
            left.extension_until_secs,
            159.0,
        );
        assert_eq!(
            inside.suppressed_by,
            Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD),
            "case \"inside the extension period\": expected it to hold, got {inside:?}"
        );

        let past = suppression(
            Some("mute"),
            Some("OK"),
            period,
            inside.suppressed_by,
            inside.extension_until_secs,
            160.0,
        );
        assert_eq!(
            (past.suppressed_by, past.extension_until_secs),
            (None, None),
            "case \"past the extension period\": expected nothing to suppress the actions, got \
             {past:?}"
        );
    }

    /// RULE: an alarm that never had a suppressor in ALARM is suppressed by nothing, and so is
    /// one carrying no suppressor at all.
    #[test]
    fn nothing_suppresses_an_alarm_whose_suppressor_was_never_in_alarm() {
        // (case, the suppressor, the state it is in)
        let rows: [(&str, Option<&str>, Option<&str>); 3] = [
            ("no suppressor at all", None, None),
            ("a suppressor in OK", Some("mute"), Some("OK")),
            ("a suppressor in no state", Some("mute"), None),
        ];
        for (case, suppressor, state) in rows {
            let decided = suppression(suppressor, state, Some(60), None, None, 100.0);
            assert_eq!(
                decided,
                Suppression::none(),
                "case {case:?}: expected nothing to suppress the actions, got {decided:?}"
            );
        }
    }
}
