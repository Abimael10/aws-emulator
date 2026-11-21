// SPDX-License-Identifier: CC0-1.0

//! The values this service FILLS IN for the optional schedule members a write leaves out.
//!
//! A schedule has an EFFECTIVE configuration: EventBridge Scheduler supplies a value for
//! every optional member a `CreateSchedule`/`UpdateSchedule` request omits, and that value
//! is part of the schedule from the moment it is written — so a read answers the
//! configuration the service will invoke the schedule under rather than only the subset
//! the request happened to state. `State` is the member the store has always defaulted
//! this way ([`super::DEFAULT_SCHEDULE_STATE`]); the members here are the rest of them:
//!
//! * `ActionAfterCompletion` — "the action that EventBridge Scheduler applies to the
//!   schedule after the schedule completes invoking the target"; a schedule that asks for
//!   no action after completion keeps the `NONE` of the two the enum admits
//!   (`smithy_query(query="shape", shape="com.amazonaws.scheduler#ActionAfterCompletion")`:
//!   `NONE | DELETE`).
//! * `ScheduleExpressionTimezone` — "the timezone in which the scheduling expression is
//!   evaluated"; the instants a schedule with no zone of its own fires at are read in UTC
//!   ([`super::super::state::occurrence`] applies that zone), so UTC is the zone the
//!   schedule is evaluated in and therefore the one it reports.
//! * `Target$RetryPolicy` — every invocation has a retry policy whether or not the request
//!   stated one, and each member's default is the LARGEST value that member admits
//!   (`smithy_query(query="shape", shape="com.amazonaws.scheduler#RetryPolicy")`:
//!   `MaximumEventAgeInSeconds` `range 60..86400`, `MaximumRetryAttempts` `range 0..185`),
//!   which is the policy [`super::dead_letter`] applies to an invocation. The two members
//!   default INDEPENDENTLY: a request stating one of them is answered with the figure it
//!   stated beside the default of the one it left out.
//!
//! Materialising the defaults on the stored record — rather than projecting them in one
//! reader — is what makes every reader of a schedule agree with the invocation clock about
//! what the schedule is: the record carries the effective configuration, so nothing has to
//! re-derive it.
//!
//! One concern, shared by the two operations that WRITE a schedule: both carry the same
//! members, so both fill the same defaults in and an update cannot leave a stored schedule
//! without a value a create would have supplied.

use crate::ops::types::RetryPolicy;

use super::ScheduleRecord;

/// The `ActionAfterCompletion` a schedule takes when the request omits the member.
pub const DEFAULT_ACTION_AFTER_COMPLETION: &str = "NONE";

/// The `ScheduleExpressionTimezone` a schedule takes when the request omits the member,
/// which is the zone its expression is read in.
pub const DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE: &str = "UTC";

/// The `RetryPolicy$MaximumEventAgeInSeconds` a schedule takes when the request states
/// none: the maximum of the modelled `range 60..86400`, i.e. 24 hours.
pub const DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS: i32 = 86_400;

/// The `RetryPolicy$MaximumRetryAttempts` a schedule takes when the request states none:
/// the maximum of the modelled `range 0..185`.
pub const DEFAULT_MAXIMUM_RETRY_ATTEMPTS: i32 = 185;

/// Fill in on `record` every server-side default the write request left out, so the stored
/// schedule is the schedule's effective configuration.
///
/// Each member is filled in ONLY where the request stated nothing for it: a value the
/// request carried is the schedule's, defaults included (a request stating `UTC` is stored
/// as `UTC`, and one stating `MaximumRetryAttempts` 0 keeps that 0 rather than taking the
/// maximum).
pub fn fill_server_defaults(record: &mut ScheduleRecord) {
    if record.action_after_completion.is_none() {
        record.action_after_completion = Some(DEFAULT_ACTION_AFTER_COMPLETION.to_string());
    }
    if record.schedule_expression_timezone.is_none() {
        record.schedule_expression_timezone =
            Some(DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE.to_string());
    }
    // A target with no policy at all gets the whole default one, and a target with a
    // partial policy keeps what it stated: the two bounds are independent members.
    record.target.retry_policy = Some(effective_retry_policy(record.target.retry_policy.as_ref()));
}

/// The policy a target states, with each member it omits standing at its default.
///
/// The one place those defaults are applied, so the bounds [`super::dead_letter`] holds an
/// invocation's attempts to and the policy a stored schedule reports cannot disagree about
/// what a schedule stating no policy retries like.
pub fn effective_retry_policy(policy: Option<&RetryPolicy>) -> RetryPolicy {
    let mut effective = policy.cloned().unwrap_or_default();
    effective.maximum_event_age_in_seconds = effective
        .maximum_event_age_in_seconds
        .or(Some(DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS));
    effective.maximum_retry_attempts = effective
        .maximum_retry_attempts
        .or(Some(DEFAULT_MAXIMUM_RETRY_ATTEMPTS));
    effective
}

#[cfg(test)]
mod tests {
    //! The defaulting this module owns, over a SET of write inputs rather than one. The
    //! same rule is driven through real requests, on both operations that write a
    //! schedule, in `tests/all/schedule_effective_configuration.rs`.

    use super::*;

    /// A policy is defaulted member by member: nothing stated takes both maxima, one
    /// member stated keeps it and defaults the other, both stated are kept as sent —
    /// including a `MaximumRetryAttempts` of 0, the other published bound of that member.
    #[test]
    fn each_retry_policy_member_defaults_on_its_own() {
        let rows: [(&str, Option<RetryPolicy>, (i32, i32)); 5] = [
            (
                "no policy at all",
                None,
                (
                    DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS,
                    DEFAULT_MAXIMUM_RETRY_ATTEMPTS,
                ),
            ),
            (
                "an empty policy",
                Some(RetryPolicy::default()),
                (
                    DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS,
                    DEFAULT_MAXIMUM_RETRY_ATTEMPTS,
                ),
            ),
            (
                "only the age stated, at its lower bound",
                Some(RetryPolicy {
                    maximum_event_age_in_seconds: Some(60),
                    maximum_retry_attempts: None,
                }),
                (60, DEFAULT_MAXIMUM_RETRY_ATTEMPTS),
            ),
            (
                "only the attempt count stated, at its lower bound",
                Some(RetryPolicy {
                    maximum_event_age_in_seconds: None,
                    maximum_retry_attempts: Some(0),
                }),
                (DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS, 0),
            ),
            (
                "both stated",
                Some(RetryPolicy {
                    maximum_event_age_in_seconds: Some(120),
                    maximum_retry_attempts: Some(3),
                }),
                (120, 3),
            ),
        ];

        for (case, stated, (age, attempts)) in rows {
            let effective = effective_retry_policy(stated.as_ref());
            assert_eq!(
                (
                    effective.maximum_event_age_in_seconds,
                    effective.maximum_retry_attempts
                ),
                (Some(age), Some(attempts)),
                "case {case:?}: expected ({age}, {attempts}), got {effective:?}"
            );
        }
    }
}
