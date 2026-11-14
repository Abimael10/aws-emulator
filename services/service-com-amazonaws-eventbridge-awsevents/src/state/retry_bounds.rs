// SPDX-License-Identifier: CC0-1.0

//! The two BOUNDS a target's `RetryPolicy` puts on a retried delivery, and which
//! of them ended one.
//!
//! The model states the rule itself. `RetryPolicy$MaximumRetryAttempts` is "the
//! maximum number of retry attempts to make before the request fails. Retry
//! attempts continue until either the maximum number of attempts is made or until
//! the duration of the `MaximumEventAgeInSeconds` is met", and
//! `RetryPolicy$MaximumEventAgeInSeconds` is "the maximum amount of time, in
//! seconds, to continue to make retry attempts" — so a retryable delivery failure
//! is retried under TWO bounds at once and the delivery ends on whichever is met
//! first. WHICH one it was is the only thing that tells a consumer of a
//! dead-letter queue an endpoint outage that used the attempts up from one that
//! outlived the event: it is reported beside the attempt count on the dead letter
//! ([`EXHAUSTED_RETRY_CONDITION`]).
//!
//! Both bounds have a value for every target, because AWS documents defaults for
//! a target that configures neither ([`DEFAULT_MAXIMUM_RETRY_ATTEMPTS`],
//! [`DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS`]) — so the question is never "is there
//! a bound" but "which bound is the binding one".
//!
//! ## The age bound is read against the retry PACING
//!
//! A retry sequence is spread over time: AWS paces the retries of one delivery
//! with exponential backoff, which is what makes an age bound reachable at all
//! while an attempt allowance of up to 185 is not. This module states that pacing
//! ([`pacing_before`]) and nothing else performs it: the emulator delivers inside
//! the request that published the event, so it makes the attempts of the schedule
//! without ever waiting for the schedule — the same discipline every due instant
//! in this emulator is handled with. The age a bound is read against is therefore
//! the event's REAL age when the delivery began, plus the pacing the retries
//! already made account for.
//!
//! An event that is already older than its target's bound when the delivery
//! begins — one published with a `Time` in the past, say — is past the bound on
//! its first retryable failure, and is dead-lettered on the age condition with no
//! retry at all.

/// The dead-letter message attribute naming WHICH bound ended a retried
/// delivery.
pub const EXHAUSTED_RETRY_CONDITION: &str = "EXHAUSTED_RETRY_CONDITION";

/// The dead-letter message attribute carrying how many RETRIES the delivery
/// made, beside the first attempt.
pub const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";

/// What [`EXHAUSTED_RETRY_CONDITION`] names when the target's retry ALLOWANCE is
/// what ended the delivery: the member of the retry policy that bounds it.
pub const MAXIMUM_RETRY_ATTEMPTS_CONDITION: &str = "MaximumRetryAttempts";

/// What [`EXHAUSTED_RETRY_CONDITION`] names when the EVENT'S AGE is what ended
/// it: the other member of the same policy.
pub const MAXIMUM_EVENT_AGE_CONDITION: &str = "MaximumEventAgeInSeconds";

/// The retry allowance a target that configures none is delivered under: "by
/// default, EventBridge retries sending the event ... up to 185 times", which is
/// also the maximum the member admits (`MaximumRetryAttempts`, `range 0..185`).
pub const DEFAULT_MAXIMUM_RETRY_ATTEMPTS: i32 = 185;

/// The age bound a target that configures none is delivered under: "by default,
/// EventBridge retries sending the event for up to 24 hours", which is also the
/// maximum the member admits (`MaximumEventAgeInSeconds`, `range 60..86400`).
pub const DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS: i32 = 86_400;

/// The largest age bound the model admits (`MaximumEventAgeInSeconds`,
/// `range 60..86400`), which is what [`MAX_RETRIES_PER_DELIVERY`] is derived
/// from.
const MAX_MAXIMUM_EVENT_AGE_IN_SECONDS: u64 = 86_400;

/// How long the pacing leaves between a failed attempt and the FIRST retry of
/// it, which each further retry doubles.
///
/// CHOSEN: AWS documents that the retries of one delivery are paced with
/// exponential backoff, and publishes no base for it. Nothing waits for this
/// value (see the module docs) — it decides which bound a sequence of retries
/// reaches first, so it is the shape of the pacing that matters and not the
/// wall-clock cost of it.
const FIRST_RETRY_DELAY_SECONDS: u64 = 1;

/// [`FIRST_RETRY_DELAY_SECONDS`] as the duration the pacing is measured in.
const FIRST_RETRY_DELAY: ::std::time::Duration =
    ::std::time::Duration::from_secs(FIRST_RETRY_DELAY_SECONDS);

/// How many RETRIES one delivery makes here at the very most.
///
/// Derived from the model rather than chosen ([`retries_inside`]): it is the last
/// retry whose place in the pacing still falls inside the largest age bound a
/// target may configure ([`MAX_MAXIMUM_EVENT_AGE_IN_SECONDS`]), so every bound
/// the model admits is reached by one of the two conditions above rather than by
/// this number. It is a bound on the real requests one delivery makes, which the
/// attempt allowance (up to 185) does not give.
pub const MAX_RETRIES_PER_DELIVERY: i32 = retries_inside(MAX_MAXIMUM_EVENT_AGE_IN_SECONDS);

/// How many retries of the pacing fall inside `age_seconds`: the last retry
/// whose place is before it, since a retry due AT the bound is one the bound
/// ends ([`RetryBounds::exhausted_after`]).
const fn retries_inside(age_seconds: u64) -> i32 {
    let mut retry: i32 = 0;
    // One step past the last retry that fits is where this stops, and the shift
    // below stays defined whatever age is asked for.
    while retry < 32 && ((1u64 << (retry + 1)) - 1) * FIRST_RETRY_DELAY_SECONDS < age_seconds {
        retry += 1;
    }
    retry
}

/// Where retry number `retry` (the first retry is 1) sits in the pacing: how long
/// after the delivery's first attempt it is made.
///
/// The exponential schedule, in closed form: each retry waits twice as long as
/// the one before it, so the retry that comes after `retry - 1` retries sits at
/// `(2^retry - 1)` first delays.
pub fn pacing_before(retry: i32) -> ::std::time::Duration {
    // Clamped so the shift below is defined for every input, and because no
    // delivery here makes more retries than this (one past the bound is asked
    // for: that is the retry whose place decides the stop).
    let retry = retry.clamp(0, MAX_RETRIES_PER_DELIVERY + 1) as u32;
    FIRST_RETRY_DELAY.saturating_mul((1u32 << retry) - 1)
}

/// The bounds one target's `RetryPolicy` puts on a retried delivery.
#[derive(Clone, Copy, Debug)]
pub struct RetryBounds {
    /// `MaximumRetryAttempts`, or its documented default.
    attempts: i32,
    /// `MaximumEventAgeInSeconds`, or its documented default.
    age: ::std::time::Duration,
}

impl RetryBounds {
    /// The bounds a target carrying these members is delivered under, each
    /// falling back to the default AWS documents for a target that leaves it
    /// out.
    pub fn of(
        maximum_retry_attempts: Option<i32>,
        maximum_event_age_in_seconds: Option<i32>,
    ) -> Self {
        let attempts = maximum_retry_attempts
            .unwrap_or(DEFAULT_MAXIMUM_RETRY_ATTEMPTS)
            .max(0);
        let seconds = maximum_event_age_in_seconds
            .unwrap_or(DEFAULT_MAXIMUM_EVENT_AGE_IN_SECONDS)
            .max(0);
        Self {
            attempts,
            age: ::std::time::Duration::from_secs(u64::from(seconds.unsigned_abs())),
        }
    }

    /// Which bound ends the delivery after `retries_made` retries, or `None`
    /// while both still admit another one.
    ///
    /// `age_at_first_attempt` is how old the event already was when the delivery
    /// began, so the age bound is read against the event's own age and not
    /// against how long this delivery has been running.
    ///
    /// The allowance is asked first: the model's sentence names it first, and a
    /// target that allows no retry at all is stopped by it whatever its age
    /// bound says. The age bound ends the retries once the next one would be
    /// made AT it — "until the duration ... is met" — which is the same boundary
    /// `service-com-amazonaws-scheduler-awschronosservice`'s own reading of this
    /// policy takes, so the two services answer the modelled rule alike.
    pub fn exhausted_after(
        &self,
        retries_made: i32,
        age_at_first_attempt: ::std::time::Duration,
    ) -> Option<&'static str> {
        if retries_made >= self.attempts {
            return Some(MAXIMUM_RETRY_ATTEMPTS_CONDITION);
        }
        let next = retries_made.saturating_add(1);
        if age_at_first_attempt.saturating_add(pacing_before(next)) >= self.age {
            return Some(MAXIMUM_EVENT_AGE_CONDITION);
        }
        None
    }
}

/// What a dead letter reports for an api-destination delivery whose endpoint
/// ANSWERED and did not take the event.
///
/// AWS's own wording, with the status the endpoint answered and the body it
/// answered with as the only variable parts: a consumer of the queue reads the
/// endpoint's status back out of the message.
pub fn api_destination_status_failure(status: u16, payload: &str) -> String {
    format!("ApiDestination returned HTTP status {status} with payload: {payload}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pacing is the exponential schedule in closed form, from the first
    /// retry up.
    #[test]
    fn the_pacing_doubles_every_retry() {
        for retry in 1..=MAX_RETRIES_PER_DELIVERY {
            let expected = ::std::time::Duration::from_secs(u64::from(
                (1u32 << u32::try_from(retry).expect("the retry number is small")) - 1,
            ));
            assert_eq!(
                pacing_before(retry),
                expected,
                "case retry {retry}: expected {expected:?}, got {:?}",
                pacing_before(retry)
            );
        }
    }

    /// [`MAX_RETRIES_PER_DELIVERY`] is the last retry inside the largest age
    /// bound the model admits, and the one after it is at or past it: that is
    /// what makes every bound the model admits reachable.
    #[test]
    fn the_retry_bound_covers_every_age_the_model_admits() {
        let widest = ::std::time::Duration::from_secs(MAX_MAXIMUM_EVENT_AGE_IN_SECONDS);
        assert!(
            pacing_before(MAX_RETRIES_PER_DELIVERY) < widest,
            "case widest bound: retry {MAX_RETRIES_PER_DELIVERY} sits at {:?}, not inside \
             {widest:?}",
            pacing_before(MAX_RETRIES_PER_DELIVERY)
        );
        assert!(
            pacing_before(MAX_RETRIES_PER_DELIVERY + 1) >= widest,
            "case widest bound: retry {} sits at {:?}, still inside {widest:?}",
            MAX_RETRIES_PER_DELIVERY + 1,
            pacing_before(MAX_RETRIES_PER_DELIVERY + 1)
        );
    }

    /// One row of the table below: the case, the `MaximumRetryAttempts` and
    /// `MaximumEventAgeInSeconds` the target carries, how many retries have been
    /// made, how old the event was when the delivery began, and the condition
    /// reported.
    type BoundRow = (
        &'static str,
        Option<i32>,
        Option<i32>,
        i32,
        u64,
        Option<&'static str>,
    );

    /// Which bound ends a delivery, over the range of policies a target may
    /// carry: the allowance when it is the smaller, the age when the pacing
    /// reaches it first, and nothing while both admit another retry.
    #[test]
    fn the_binding_bound_is_the_one_reported() {
        let rows: [BoundRow; 9] = [
            ("no retry allowed at all", Some(0), Some(60), 0, 0, Some(MAXIMUM_RETRY_ATTEMPTS_CONDITION)),
            ("one retry allowed, none made", Some(1), Some(900), 0, 0, None),
            ("one retry allowed, one made", Some(1), Some(900), 1, 0, Some(MAXIMUM_RETRY_ATTEMPTS_CONDITION)),
            // 1 + 2 + 4 + 8 + 16 = 31 seconds of pacing, and the sixth retry
            // would sit at 63 — past the smallest age bound the model admits.
            ("the smallest age bound, inside it", Some(185), Some(60), 4, 0, None),
            ("the smallest age bound, reached", Some(185), Some(60), 5, 0, Some(MAXIMUM_EVENT_AGE_CONDITION)),
            ("an event already older than its bound", Some(185), Some(60), 0, 120, Some(MAXIMUM_EVENT_AGE_CONDITION)),
            ("no policy at all, inside both", None, None, 10, 0, None),
            ("no policy at all, the default age reached", None, None, 16, 0, Some(MAXIMUM_EVENT_AGE_CONDITION)),
            ("an allowance under the default age", Some(2), None, 2, 0, Some(MAXIMUM_RETRY_ATTEMPTS_CONDITION)),
        ];
        for (case, attempts, age, made, aged, expected) in rows {
            let bounds = RetryBounds::of(attempts, age);
            let reported =
                bounds.exhausted_after(made, ::std::time::Duration::from_secs(aged));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// The failure a dead letter reports for an answered endpoint is AWS's
    /// wording, carrying the status and the payload.
    #[test]
    fn the_answered_failure_carries_the_status_and_the_payload() {
        for (status, payload) in [(500u16, ""), (503, "upstream down"), (400, "{}")] {
            let reported = api_destination_status_failure(status, payload);
            assert_eq!(
                reported,
                format!("ApiDestination returned HTTP status {status} with payload: {payload}"),
                "case {status} {payload:?}: got {reported:?}"
            );
            assert!(
                reported.contains(&format!("HTTP status {status}")),
                "case {status} {payload:?}: the status is readable back out, got {reported:?}"
            );
        }
    }
}
