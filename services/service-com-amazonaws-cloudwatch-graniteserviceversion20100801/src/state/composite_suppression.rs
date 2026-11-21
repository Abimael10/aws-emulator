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
//! Those three reasons follow one another, which is what this module decides:
//!
//! * when the composite alarm goes into `ALARM` while the suppressor is NOT in `ALARM`, the
//!   actions are withheld while the alarm waits for the suppressor to confirm it — "the actions
//!   are suppressed because the composite alarm is waiting for the suppressor alarm to go into
//!   the `ALARM` state. The maximum waiting time is as specified in
//!   `ActionsSuppressorWaitPeriod`. After this time, the composite alarm performs its actions."
//!   — reported as `WaitPeriod`;
//! * while the suppressor is in `ALARM`, the reason is `Alarm` — "when the value is ALARM, it
//!   means that the actions are suppressed because the suppressor alarm is in ALARM";
//! * once the suppressor LEAVES `ALARM`, the suppression does not end with it but holds for the
//!   extension period the alarm was configured with ("the maximum time in seconds that the
//!   composite alarm waits after suppressor alarm goes out of the ALARM state",
//!   `CompositeAlarm$ActionsSuppressorExtensionPeriod`), reported as `ExtensionPeriod`;
//! * when the period in force has passed, nothing suppresses the actions, the alarm reports no
//!   reason at all and the actions it withheld are performed.
//!
//! The DURATION is the caller's own, taken from the member it configured the alarm with rather
//! than chosen here, and the instant the period ends is stored on the alarm by the evaluation
//! that opens it — so the suppression is a pure function of that instant and the clock, and
//! whichever call materialises an evaluation, every later caller sees the same thing.

use ::serde_json::{Map, Value};

use crate::ops::types::AlarmHistoryItem;

use super::{ACTIONS_SUPPRESSED_BY_ALARM, ALARM_STATE_ALARM};

/// `WaitPeriod`, the modelled `ActionsSuppressedBy` value for the window a composite alarm
/// spends waiting for its suppressor to go into `ALARM` (`CompositeAlarm$ActionsSuppressedBy`).
pub const ACTIONS_SUPPRESSED_BY_WAIT_PERIOD: &str = "WaitPeriod";

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
    /// The instant the timed period in force ends, carried on the alarm until it passes.
    pub until_secs: Option<f64>,
}

impl Suppression {
    /// Nothing suppresses this alarm's actions.
    fn none() -> Self {
        Self {
            suppressed_by: None,
            reason: None,
            until_secs: None,
        }
    }

    /// The suppression `phase` holds this alarm's actions in until `until_secs`.
    fn holding(phase: &'static str, named: &str, until_secs: Option<f64>) -> Self {
        Self {
            suppressed_by: Some(phase),
            reason: Some(reason_of(phase, named)),
            until_secs,
        }
    }
}

/// How the suppressor of ONE composite alarm stands as an evaluation decides its suppression.
///
/// The members are read off the alarm and off the store that holds its suppressor, so the
/// decision below is a pure function of them and the clock.
#[derive(Debug, Clone, Copy)]
pub struct SuppressorStanding<'a> {
    /// The alarm named as this alarm's actions suppressor (`CompositeAlarm$ActionsSuppressor`),
    /// or `None` for an alarm that names none.
    pub suppressor: Option<&'a str>,
    /// The state that alarm is in right now, as this store holds it.
    pub suppressor_state: Option<&'a str>,
    /// The alarm's own `ActionsSuppressorWaitPeriod`, in seconds.
    pub wait_period_secs: Option<i32>,
    /// The alarm's own `ActionsSuppressorExtensionPeriod`, in seconds.
    pub extension_period_secs: Option<i32>,
    /// The reason the alarm reports as this evaluation begins
    /// (`CompositeAlarm$ActionsSuppressedBy`), so the evaluation that sees the suppressor leave
    /// `ALARM` is the one that opens the extension period.
    pub reported: Option<&'a str>,
    /// The instant the period already in force ends, as the evaluation that opened it stored it.
    pub until_secs: Option<f64>,
    /// Whether the state this evaluation leaves the composite alarm in is `ALARM`.
    pub in_alarm: bool,
    /// Whether this evaluation is the one that MOVES the composite alarm into `ALARM`, which is
    /// the instant the wait period is measured from.
    pub entering_alarm: bool,
}

/// What the actions of a composite alarm are suppressed by at `now_secs`.
#[must_use]
pub fn suppression(standing: &SuppressorStanding<'_>, now_secs: f64) -> Suppression {
    let Some(suppressor) = standing.suppressor else {
        // An alarm with no suppressor suppresses nothing, whatever it once carried.
        return Suppression::none();
    };
    let named = super::alarm_rule_reference_name(suppressor);
    if standing.suppressor_state == Some(ALARM_STATE_ALARM) {
        return Suppression::holding(
            ACTIONS_SUPPRESSED_BY_ALARM,
            named,
            // Both timed periods run from an instant the suppressor's own state fixes — the
            // wait for it to reach this state is over, and the extension runs from the instant
            // it LEAVES this state — so neither is owed while it is in it.
            None,
        );
    }
    // The suppressor is not in ALARM, so what holds the actions is one of the two timed
    // periods: the instant it ends is stored by the evaluation that OPENS it and read by every
    // later evaluation, so the window is measured from the transition that opened it rather
    // than from whichever call happens to look next.
    let (phase, until) = if standing.reported == Some(ACTIONS_SUPPRESSED_BY_ALARM) {
        // The suppressor has just left ALARM: the extension period opens here.
        (
            ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD,
            now_secs + period_secs(standing.extension_period_secs),
        )
    } else if let (Some(phase), Some(until)) = (held_phase(standing.reported), standing.until_secs)
    {
        // A period already in force runs to the instant it was opened for.
        (phase, until)
    } else if standing.entering_alarm {
        // "The maximum time in seconds that the composite alarm waits for the suppressor alarm
        // to go into the ALARM state." (`CompositeAlarm$ActionsSuppressorWaitPeriod`) — the
        // wait is measured from the instant this alarm goes into ALARM, which is the instant
        // its actions would otherwise fire.
        (
            ACTIONS_SUPPRESSED_BY_WAIT_PERIOD,
            now_secs + period_secs(standing.wait_period_secs),
        )
    } else {
        return Suppression::none();
    };
    // The wait is this alarm's own wait for its suppressor to confirm the ALARM state it went
    // into, so it is over as soon as the alarm is no longer in that state.
    if phase == ACTIONS_SUPPRESSED_BY_WAIT_PERIOD && !standing.in_alarm {
        return Suppression::none();
    }
    // "After this time, the composite alarm performs its actions."
    if now_secs >= until {
        return Suppression::none();
    }
    Suppression::holding(phase, named, Some(until))
}

/// The timed period `reported` names, or `None` when it names neither of them.
fn held_phase(reported: Option<&str>) -> Option<&'static str> {
    match reported? {
        ACTIONS_SUPPRESSED_BY_WAIT_PERIOD => Some(ACTIONS_SUPPRESSED_BY_WAIT_PERIOD),
        ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD => Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD),
        _ => None,
    }
}

/// One configured suppressor period as the seconds it is counted in. A period the alarm carries
/// none of, or a negative one, is no wait at all.
fn period_secs(configured: Option<i32>) -> f64 {
    f64::from(configured.unwrap_or_default().max(0))
}

/// What `ActionsSuppressedReason` states about the actions `phase` is withholding.
fn reason_of(phase: &'static str, named: &str) -> String {
    match phase {
        ACTIONS_SUPPRESSED_BY_WAIT_PERIOD => {
            format!("Actions suppressed by wait period of alarm {named}")
        }
        ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD => {
            format!("Actions suppressed by extension period of alarm {named}")
        }
        _ => format!("Actions suppressed by alarm {named}"),
    }
}

/// The `HistoryItemType` a suppressed transition's actions are recorded under: `Action` is the
/// type every record of what became of a transition's actions carries
/// (`smithy_query(query="shape", shape="HistoryItemType")`), and a suppression is what became of
/// these.
const SUPPRESSED_HISTORY_ITEM_TYPE: &str = "Action";

/// What the record of a suppressed transition states happened to the actions, beside the
/// `Succeeded` and `Failed` an executed action is recorded with.
const SUPPRESSED_ACTION_STATE: &str = "Suppressed";

/// The history item recording that a transition's configured actions were SUPPRESSED by the
/// alarm's actions suppressor, rather than executed.
///
/// The transition happens and is recorded as itself, and this is the account of the actions it
/// did not execute: which of the modelled reasons withheld them, which state they were
/// configured for, and which ARNs they were. Without it a suppressed transition is
/// indistinguishable, in the only record an operator has, from an action that was executed and
/// failed.
pub fn suppressed_actions_history_item(
    alarm_name: &str,
    suppressed_by: &str,
    new_state_value: &str,
    actions: &[String],
    now_secs: f64,
) -> AlarmHistoryItem {
    let mut document = Map::new();
    document.insert(
        "version".to_string(),
        Value::from(super::HISTORY_DATA_VERSION),
    );
    document.insert(
        "actionState".to_string(),
        Value::from(SUPPRESSED_ACTION_STATE),
    );
    document.insert("suppressedBy".to_string(), Value::from(suppressed_by));
    document.insert("stateValue".to_string(), Value::from(new_state_value));
    let suppressed = Value::from(
        actions
            .iter()
            .map(|action| Value::from(action.as_str()))
            .collect::<Vec<Value>>(),
    );
    let mut with_actions = document.clone();
    with_actions.insert("suppressedActions".to_string(), suppressed);
    AlarmHistoryItem {
        alarm_name: Some(alarm_name.to_string()),
        alarm_contributor_id: None,
        // Only a composite alarm carries an actions suppressor, so the item belongs to that
        // kind of alarm's history.
        alarm_type: Some(super::ALARM_TYPE_COMPOSITE.to_string()),
        timestamp: Some(super::to_timestamp(now_secs)),
        history_item_type: Some(SUPPRESSED_HISTORY_ITEM_TYPE.to_string()),
        history_summary: Some(super::alarm_actions::bounded(format!(
            "Alarm actions suppressed by {suppressed_by}"
        ))),
        // An action ARN is `@length 1..1024` and a state may carry five of them, so the list of
        // suppressed ARNs is the part of this document that need not fit the modelled
        // `HistoryData` length: it is dropped rather than truncated, because a document is
        // parsed rather than read, and what identifies the suppression is in both renderings.
        history_data: Some(super::bounded_history_data(&[
            Value::Object(with_actions),
            Value::Object(document),
        ])),
        alarm_contributor_attributes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A standing with no period in force and nothing reported, for the suppressor `name` in
    /// `state`.
    fn standing<'a>(name: &'a str, state: &'a str) -> SuppressorStanding<'a> {
        SuppressorStanding {
            suppressor: Some(name),
            suppressor_state: Some(state),
            wait_period_secs: Some(120),
            extension_period_secs: Some(60),
            reported: None,
            until_secs: None,
            in_alarm: true,
            entering_alarm: false,
        }
    }

    /// RULE: the reason follows the suppressor — `Alarm` while it is in ALARM, then
    /// `ExtensionPeriod` for the configured period after it leaves, then nothing.
    #[test]
    fn the_reason_follows_the_suppressor_and_then_the_extension_period() {
        let in_alarm = suppression(&standing("mute", ALARM_STATE_ALARM), 100.0);
        assert_eq!(
            in_alarm.suppressed_by,
            Some(ACTIONS_SUPPRESSED_BY_ALARM),
            "case \"a suppressor in ALARM\": expected the Alarm reason, got {in_alarm:?}"
        );

        let left = suppression(
            &SuppressorStanding {
                reported: in_alarm.suppressed_by,
                until_secs: in_alarm.until_secs,
                ..standing("mute", "OK")
            },
            100.0,
        );
        assert_eq!(
            (left.suppressed_by, left.until_secs),
            (Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD), Some(160.0)),
            "case \"the suppressor leaving ALARM\": expected the extension period to open, got \
             {left:?}"
        );

        let inside = suppression(
            &SuppressorStanding {
                reported: left.suppressed_by,
                until_secs: left.until_secs,
                ..standing("mute", "OK")
            },
            159.0,
        );
        assert_eq!(
            inside.suppressed_by,
            Some(ACTIONS_SUPPRESSED_BY_EXTENSION_PERIOD),
            "case \"inside the extension period\": expected it to hold, got {inside:?}"
        );

        let past = suppression(
            &SuppressorStanding {
                reported: inside.suppressed_by,
                until_secs: inside.until_secs,
                ..standing("mute", "OK")
            },
            160.0,
        );
        assert_eq!(
            (past.suppressed_by, past.until_secs),
            (None, None),
            "case \"past the extension period\": expected nothing to suppress the actions, got \
             {past:?}"
        );
    }

    /// RULE: an alarm going into ALARM while its suppressor is not in ALARM waits for it, for
    /// the configured wait period and no longer.
    #[test]
    fn going_into_alarm_without_the_suppressor_opens_the_wait_period() {
        let entering = suppression(
            &SuppressorStanding {
                entering_alarm: true,
                ..standing("mute", "OK")
            },
            100.0,
        );
        assert_eq!(
            (entering.suppressed_by, entering.until_secs),
            (Some(ACTIONS_SUPPRESSED_BY_WAIT_PERIOD), Some(220.0)),
            "case \"going into ALARM with the suppressor in OK\": expected the wait period to \
             open, got {entering:?}"
        );

        // (case, the instant the next evaluation runs at, what it must then report)
        let rows: [(&str, f64, Option<&str>); 3] = [
            ("inside the wait period", 101.0, Some(ACTIONS_SUPPRESSED_BY_WAIT_PERIOD)),
            ("the last instant of it", 219.999, Some(ACTIONS_SUPPRESSED_BY_WAIT_PERIOD)),
            ("past the wait period", 220.0, None),
        ];
        for (case, at, wanted) in rows {
            let held = suppression(
                &SuppressorStanding {
                    reported: entering.suppressed_by,
                    until_secs: entering.until_secs,
                    ..standing("mute", "OK")
                },
                at,
            );
            assert_eq!(
                held.suppressed_by, wanted,
                "case {case:?}: expected {wanted:?}, got {held:?}"
            );
        }

        // The wait period gives way to the Alarm reason the moment the suppressor confirms it,
        // and the extension period then runs from THERE rather than from the wait.
        let confirmed = suppression(
            &SuppressorStanding {
                reported: entering.suppressed_by,
                until_secs: entering.until_secs,
                ..standing("mute", ALARM_STATE_ALARM)
            },
            101.0,
        );
        assert_eq!(
            (confirmed.suppressed_by, confirmed.until_secs),
            (Some(ACTIONS_SUPPRESSED_BY_ALARM), None),
            "case \"the suppressor confirming the wait\": expected the Alarm reason, got \
             {confirmed:?}"
        );
    }

    /// RULE: an alarm that is not in ALARM and never had a suppressor in ALARM is suppressed by
    /// nothing, and so is one carrying no suppressor at all.
    #[test]
    fn nothing_suppresses_an_alarm_whose_suppressor_was_never_in_alarm() {
        // (case, the standing to decide)
        let rows: [(&str, SuppressorStanding<'_>); 4] = [
            (
                "no suppressor at all",
                SuppressorStanding {
                    suppressor: None,
                    suppressor_state: None,
                    ..standing("mute", "OK")
                },
            ),
            ("a suppressor in OK", standing("mute", "OK")),
            (
                "a suppressor in no state",
                SuppressorStanding {
                    suppressor_state: None,
                    ..standing("mute", "OK")
                },
            ),
            (
                "an alarm that goes into OK rather than ALARM",
                SuppressorStanding {
                    in_alarm: false,
                    reported: Some(ACTIONS_SUPPRESSED_BY_WAIT_PERIOD),
                    until_secs: Some(220.0),
                    ..standing("mute", "OK")
                },
            ),
        ];
        for (case, standing) in rows {
            let decided = suppression(&standing, 100.0);
            assert_eq!(
                decided,
                Suppression::none(),
                "case {case:?}: expected nothing to suppress the actions, got {decided:?}"
            );
        }
    }
}
