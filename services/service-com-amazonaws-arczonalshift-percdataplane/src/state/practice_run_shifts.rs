// SPDX-License-Identifier: CC0-1.0

//! The PRACTICE RUN zonal shift: how long one runs for, and what stops ARC from
//! starting one.
//!
//! A practice run is a zonal shift of its own `ShiftType` (`PRACTICE_RUN`) that ARC
//! runs against a resource's practice run configuration, so it is kept beside every
//! other shift ([`super::ZonalShift`]) and everything derived from the shifts in
//! progress — `appliedWeights`, `appliedStatus` — follows for it unchanged. Two
//! things are its own, and both live here:
//!
//! * HOW LONG it runs, and the expiry time it reports while it does
//!   ([`PRACTICE_RUN_DURATION`], [`PRACTICE_RUN_EXPIRY_WINDOW`]);
//! * WHETHER one can start at all: the practice run configuration ARC holds for the
//!   resource names the conditions that block a run, and each blocker is a conflict
//!   in the resource's current state, answered with the
//!   `ConflictExceptionReason` the model names for it ([`practice_run_blocker`]).
//!
//! The alarms a configuration names are Amazon CloudWatch's, not ARC's: their state
//! is read out of the CloudWatch emulator's OWN alarm store through the shared
//! engine — the store `PutMetricAlarm` writes, `SetAlarmState` forces and
//! `DescribeAlarms` reads ([`alarms_in_alarm_state`]) — so nothing about an alarm is
//! copied into this service's store and a run is never blocked on a state the alarm
//! no longer holds. An alarm whose state cannot be read there at all — one no caller
//! created, or a CloudWatch that is not mounted beside this service — sounds for
//! nothing, so it blocks nothing.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use super::practice_runs::{names_the_day_of, window_contains};
use super::{ShiftType, ZonalShift};
use crate::ops::types::{ConflictException, ControlCondition, PracticeRunConfiguration};

/// How long an on-demand practice run goes on for.
///
/// From `StartPracticeRunResponse$expiryTime`: "The expiry time (expiration time) for
/// an on-demand practice run zonal shift is 30 minutes from the time when you start
/// the practice run, unless you cancel it before that time."
pub const PRACTICE_RUN_DURATION: Duration = Duration::from_secs(30 * 60);

/// How far ahead of the instant it is read the expiry time of a practice run stands.
///
/// From the same sentence: "However, be aware that the `expiryTime` field for
/// practice run zonal shifts always has a value of 1 minute." A practice run's expiry
/// is therefore never the end of the run: it stands one minute out and is renewed
/// while the run goes on ([`renewed_practice_run_expiry`]), which is what makes the
/// field read the same minute whenever a caller reads it.
pub const PRACTICE_RUN_EXPIRY_WINDOW: Duration = Duration::from_secs(60);

/// The message ARC answers when a blocking alarm of the resource's practice run
/// configuration is in the `ALARM` state.
///
/// One sentence for the whole class, like every other refusal of this service: it
/// names neither the alarm nor the resource the caller already named, and has no
/// variable part — one sounding alarm blocks a run exactly as ten do.
pub const PRACTICE_BLOCKING_ALARMS_RED_MESSAGE: &str =
    "Cannot start a practice run because a blocking alarm for the requested resource is in \
     the ALARM state.";

/// The `ConflictExceptionReason` the model names for that blocker
/// (`smithy_query(query="shape", shape="ConflictExceptionReason")`).
pub const PRACTICE_BLOCKING_ALARMS_RED_REASON: &str = "PracticeBlockingAlarmsRed";

/// The message ARC answers when an outcome alarm of the resource's practice run
/// configuration is in the `ALARM` state.
pub const PRACTICE_OUTCOME_ALARMS_RED_MESSAGE: &str =
    "Cannot start a practice run because an outcome alarm for the requested resource is in \
     the ALARM state.";

/// The `ConflictExceptionReason` the model names for that blocker.
pub const PRACTICE_OUTCOME_ALARMS_RED_REASON: &str = "PracticeOutcomeAlarmsRed";

/// The message ARC answers when the day the request is made on is one of the
/// resource's blocked dates.
pub const PRACTICE_IN_BLOCKED_DATES_MESSAGE: &str =
    "Cannot start a practice run because today is a blocked date for the requested resource.";

/// The `ConflictExceptionReason` the model names for that blocker.
pub const PRACTICE_IN_BLOCKED_DATES_REASON: &str = "PracticeInBlockedDates";

/// The message ARC answers when the instant the request is made at falls in one of
/// the resource's blocked windows.
pub const PRACTICE_IN_BLOCKED_WINDOWS_MESSAGE: &str =
    "Cannot start a practice run because the current time is in a blocked window for the \
     requested resource.";

/// The `ConflictExceptionReason` the model names for that blocker.
pub const PRACTICE_IN_BLOCKED_WINDOWS_REASON: &str = "PracticeInBlockedWindows";

/// The message ARC answers when the resource names allowed windows and the instant
/// the request is made at falls in none of them.
pub const PRACTICE_OUTSIDE_ALLOWED_WINDOWS_MESSAGE: &str =
    "Cannot start a practice run because the current time is outside the allowed windows for \
     the requested resource.";

/// The `ConflictExceptionReason` the model names for that blocker.
pub const PRACTICE_OUTSIDE_ALLOWED_WINDOWS_REASON: &str = "PracticeOutsideAllowedWindows";

/// The expiry time a practice run that has reached the one it reported takes next, or
/// `None` when its run is over and the shift is therefore `EXPIRED`.
///
/// A practice run's expiry stands [`PRACTICE_RUN_EXPIRY_WINDOW`] out and is renewed
/// up to [`PRACTICE_RUN_DURATION`] past the instant the run started, which is what
/// the two sentences of `StartPracticeRunResponse$expiryTime` state together: the
/// field always reads one minute ahead, and the run goes on for thirty. The renewal
/// is a change the CLOCK made due — it was owed at that instant whether or not
/// anybody called — so the sweep every operation runs materialises it
/// ([`super::ComAmazonawsArczonalshiftPercdataplaneState::expire_due_shifts`]) and no
/// read can show a caller a practice run another caller would not also see.
///
/// Answers `None` for every other kind of shift: a customer-initiated shift expires
/// at the time the caller asked for and is renewed only by `UpdateZonalShift`.
#[must_use]
pub fn renewed_practice_run_expiry(shift: &ZonalShift, now: SystemTime) -> Option<SystemTime> {
    if shift.shift_type != ShiftType::PracticeRun {
        return None;
    }
    let run_ends_at = shift.start_time.checked_add(PRACTICE_RUN_DURATION)?;
    if run_ends_at <= now {
        return None;
    }
    // `checked_add`, so a renewal never depends on the instant staying
    // representable; past the end of the run the expiry is the end of the run.
    Some(now.checked_add(PRACTICE_RUN_EXPIRY_WINDOW)?.min(run_ends_at))
}

/// The expiry time a practice run STARTED at `start_time` reports, which is the first
/// of the renewals above.
#[must_use]
pub fn practice_run_expiry_time(start_time: SystemTime) -> Option<SystemTime> {
    start_time.checked_add(PRACTICE_RUN_EXPIRY_WINDOW)
}

/// The refusal a caller gets for starting a practice run against `configuration` at
/// `now`, or `None` when nothing the configuration names blocks one.
///
/// ONE reading of the configuration's blockers, in the order ARC takes them: the
/// alarms it watches, which say what the resource is doing NOW, then the calendar the
/// configuration keeps — a blocked date, a blocked window, and the allowed windows a
/// run has to fall inside of when the configuration names any. `red_alarms` holds the
/// `alarmIdentifier`s CloudWatch reports in the `ALARM` state
/// ([`alarms_in_alarm_state`]), read before this store is locked.
#[must_use]
pub fn practice_run_blocker(
    configuration: &PracticeRunConfiguration,
    now: SystemTime,
    red_alarms: &BTreeSet<String>,
) -> Option<ConflictException> {
    // "Blocking alarms for practice runs are optional alarms that you can specify
    // that block practice runs when one or more of the alarms is in an ALARM state"
    // (`PracticeRunConfiguration$blockingAlarms`).
    if sounds(configuration.blocking_alarms.as_deref().unwrap_or(&[]), red_alarms) {
        return Some(blocked(
            PRACTICE_BLOCKING_ALARMS_RED_MESSAGE,
            PRACTICE_BLOCKING_ALARMS_RED_REASON,
        ));
    }
    // "Outcome alarms for practice runs are alarms that you specify that end a
    // practice run when one or more of the alarms is in an ALARM state"
    // (`PracticeRunConfiguration$outcomeAlarms`) — a run that would end the instant
    // it began is one ARC does not start.
    if sounds(&configuration.outcome_alarms, red_alarms) {
        return Some(blocked(
            PRACTICE_OUTCOME_ALARMS_RED_MESSAGE,
            PRACTICE_OUTCOME_ALARMS_RED_REASON,
        ));
    }
    // "An array of one or more dates that you can specify when Amazon Web Services
    // does not start practice runs for a resource. Specify blocked dates, in UTC, in
    // the format YYYY-MM-DD" (`PracticeRunConfiguration$blockedDates`).
    if configuration
        .blocked_dates
        .iter()
        .any(|date| names_the_day_of(date, now))
    {
        return Some(blocked(
            PRACTICE_IN_BLOCKED_DATES_MESSAGE,
            PRACTICE_IN_BLOCKED_DATES_REASON,
        ));
    }
    // "An array of one or more windows of days and times that you can block ARC from
    // starting practice runs for a resource"
    // (`PracticeRunConfiguration$blockedWindows`).
    if configuration
        .blocked_windows
        .iter()
        .flatten()
        .any(|window| window_contains(window, now))
    {
        return Some(blocked(
            PRACTICE_IN_BLOCKED_WINDOWS_MESSAGE,
            PRACTICE_IN_BLOCKED_WINDOWS_REASON,
        ));
    }
    // "An array of one or more windows of days and times that you can allow ARC to
    // start practice runs for a resource"
    // (`PracticeRunConfiguration$allowedWindows`): a configuration that names any
    // allows a run only inside them, and one that names none allows a run at any
    // time.
    let allowed_windows = configuration.allowed_windows.as_deref().unwrap_or(&[]);
    if !allowed_windows.is_empty()
        && !allowed_windows
            .iter()
            .any(|window| window_contains(window, now))
    {
        return Some(blocked(
            PRACTICE_OUTSIDE_ALLOWED_WINDOWS_MESSAGE,
            PRACTICE_OUTSIDE_ALLOWED_WINDOWS_REASON,
        ));
    }
    None
}

/// Which of the alarms `configuration` watches Amazon CloudWatch reports in the
/// `ALARM` state, named as the configuration names them.
///
/// ONE read of that store per call, taken with THIS service's own store UNLOCKED:
/// the two stores are never held at once, so two services cannot invert the order
/// between them. An alarm the store holds nothing for is not in the `ALARM` state.
#[must_use]
pub fn alarms_in_alarm_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
    configuration: &PracticeRunConfiguration,
) -> BTreeSet<String> {
    let watched: BTreeSet<&str> = configuration
        .blocking_alarms
        .iter()
        .flatten()
        .chain(configuration.outcome_alarms.iter())
        .map(|condition| condition.alarm_identifier.as_str())
        .collect();
    let Some(state) = cloudwatch::state::service_state(engine) else {
        // No CloudWatch beside this service: no alarm sounds, so none blocks.
        return BTreeSet::new();
    };
    let store = state.lock();
    watched
        .into_iter()
        .filter(|identifier| {
            store.alarm_state_value(identifier).as_deref()
                == Some(cloudwatch::state::ALARM_STATE_ALARM)
        })
        .map(str::to_string)
        .collect()
}

/// Whether any of `conditions` names an alarm that is in the `ALARM` state.
fn sounds(conditions: &[ControlCondition], red_alarms: &BTreeSet<String>) -> bool {
    conditions
        .iter()
        .any(|condition| red_alarms.contains(&condition.alarm_identifier))
}

/// The conflict one blocker answers: ARC's sentence for the class, and the
/// `ConflictExceptionReason` the model names for it.
///
/// A blocker is a property of the resource's own configuration and of the instant the
/// call was made at, neither of which is a shift, so there is no `zonalShiftId` to
/// associate it with.
fn blocked(message: &str, reason: &str) -> ConflictException {
    ConflictException {
        message: message.to_string(),
        reason: reason.to_string(),
        zonal_shift_id: None,
    }
}
