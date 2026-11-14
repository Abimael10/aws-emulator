// SPDX-License-Identifier: CC0-1.0

//! The RATE an API destination is invoked at: which instant the next invocation of one may
//! be made, and how many invocations are waiting for a slot at once.
//!
//! "The maximum number of requests per second to send to the HTTP invocation endpoint"
//! (`CreateApiDestination$InvocationRateLimitPerSecond`) is a ceiling on the destination
//! rather than on one event: EventBridge paces its invocations of a destination to that
//! rate and DEFERS the ones it cannot make yet instead of dropping them, so a burst larger
//! than the rate is still delivered — every event exactly once, spread over time.
//!
//! This is the bookkeeping that ceiling needs, and nothing else: each destination's next
//! free instant, booked by whoever is about to invoke it
//! ([`InvocationPacing::book`]). The invocation itself is made by the delivery path
//! (`crate::ops::put_events`), which waits for the instant it booked — in the request where
//! that is a wait a publisher would not notice, and on a task of its own where it is longer,
//! because `PutEvents` answers before a delivery is made.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, SystemTime};

/// How many invocations may be waiting for a destination's slot at once, across every
/// destination of a store.
///
/// This emulator's OWN bound, not a limit AWS publishes: each waiting invocation is a task
/// this process holds until its slot comes, so an unbounded burst at a slow destination
/// would otherwise be an unbounded number of them. It is far above what a client bursts at
/// one destination (the published ceiling is 300 invocations a second, and a destination is
/// limited to at most that), and a burst past it is invoked without waiting rather than
/// dropped — the delivery is what matters, and the log says the pacing was given up.
pub const MAX_DEFERRED_INVOCATIONS: usize = 1_024;

/// What the API destinations of one store have booked, and how many invocations are waiting.
#[derive(Debug, Default)]
pub struct InvocationPacing {
    /// The instant the next invocation of each destination may be made at, keyed by the
    /// destination's ARN. An entry whose instant has passed is dropped at the next booking,
    /// so this holds only the destinations currently being invocated faster than their rate.
    next_free: BTreeMap<String, SystemTime>,
    /// How many invocations are waiting for a slot right now
    /// ([`MAX_DEFERRED_INVOCATIONS`]).
    deferred: usize,
    /// The tasks those waiting invocations are running on, so none of them is
    /// unreachable while it waits: a handle nothing holds cannot be stopped. Each push
    /// drops the handles of the invocations that have already been made, so this holds
    /// about as many handles as there are invocations waiting.
    waiting: Vec<::tokio::task::JoinHandle<()>>,
}

impl InvocationPacing {
    /// Book the instant the next invocation of the destination `arn` may be made at, for a
    /// destination limited to `rate` invocations per second, given the clock has reached
    /// `now`.
    ///
    /// The booking is what paces the destination: the answer is `now` while the destination
    /// is inside its rate, and the end of the last booked slot once it is not — so N
    /// invocations booked in one instant are spread one period apart, in the order they were
    /// booked, and none of them is dropped. A rate of less than one request a second is not
    /// a rate a destination carries (`InvocationRateLimitPerSecond` is modelled
    /// `range min 1`), so a value below that paces as one.
    pub fn book(&mut self, arn: &str, rate: i32, now: SystemTime) -> SystemTime {
        let period = Duration::from_nanos(1_000_000_000 / u64::from(rate.max(1).unsigned_abs()));
        // A destination whose last slot has passed is inside its rate again, so its booking
        // is dropped rather than kept for the lifetime of the store.
        self.next_free.retain(|_, slot| *slot > now);
        let at = self
            .next_free
            .get(arn)
            .copied()
            .filter(|slot| *slot > now)
            .unwrap_or(now);
        self.next_free
            .insert(arn.to_string(), at.checked_add(period).unwrap_or(at));
        at
    }

    /// Take one of the places an invocation may wait for its slot in: `false` when
    /// [`MAX_DEFERRED_INVOCATIONS`] are already waiting, in which case nothing is taken and
    /// the invocation must be made rather than deferred.
    pub fn take_a_waiting_place(&mut self) -> bool {
        if self.deferred >= MAX_DEFERRED_INVOCATIONS {
            return false;
        }
        self.deferred += 1;
        true
    }

    /// Give back the place [`Self::take_a_waiting_place`] took, which the waiting invocation
    /// does once it has been made — on every path, so a failed delivery does not hold a
    /// place for the life of the process.
    pub fn release_a_waiting_place(&mut self) {
        self.deferred = self.deferred.saturating_sub(1);
    }

    /// Keep the task a waiting invocation runs on, dropping the handles of the invocations
    /// that have already been made.
    pub fn hold_waiting(&mut self, task: ::tokio::task::JoinHandle<()>) {
        self.waiting.retain(|held| !held.is_finished());
        self.waiting.push(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A destination inside its rate is invoked at once, and one past it is paced: the
    /// bookings of a burst are one period apart, in order, and every one of them is served.
    #[test]
    fn a_burst_is_paced_at_the_destinations_own_rate() {
        // (the rate the destination carries, how many invocations the burst holds)
        let rows: [(i32, u32); 4] = [(1, 4), (2, 5), (10, 3), (300, 2)];
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (rate, burst) in rows {
            let mut pacing = InvocationPacing::default();
            let period = Duration::from_nanos(1_000_000_000 / u64::from(rate.unsigned_abs()));
            for nth in 0..burst {
                let at = pacing.book("arn:aws:events:us-west-2:1:api-destination/d/1", rate, now);
                let expected = now + period * nth;
                assert_eq!(
                    at, expected,
                    "case rate {rate}/s, invocation {nth} of {burst}: expected the slot \
                     {:?} after the first, got {:?}",
                    period * nth,
                    at.duration_since(now)
                );
            }
        }
    }

    /// Two destinations are paced independently: one at its ceiling does not delay another.
    #[test]
    fn each_destination_is_paced_on_its_own() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut pacing = InvocationPacing::default();
        for arn in ["arn:aws:events:us-west-2:1:api-destination/a/1"; 3] {
            pacing.book(arn, 1, now);
        }
        let other = pacing.book("arn:aws:events:us-west-2:1:api-destination/b/1", 1, now);
        assert_eq!(
            other, now,
            "case a second destination: its own rate is untouched by another's burst, got \
             {other:?}"
        );
    }

    /// A booking whose slot has passed is forgotten, so the clock catching up puts a
    /// destination back inside its rate.
    #[test]
    fn a_passed_booking_is_forgotten() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut pacing = InvocationPacing::default();
        let arn = "arn:aws:events:us-west-2:1:api-destination/d/1";
        pacing.book(arn, 1, now);
        let later = pacing.book(arn, 1, now + Duration::from_secs(5));
        assert_eq!(
            later,
            now + Duration::from_secs(5),
            "case the clock moved past the booking: the next invocation is due at once, got \
             {later:?}"
        );
    }

    /// The places an invocation may wait in are bounded, and giving one back frees it.
    #[test]
    fn the_waiting_places_are_bounded() {
        let mut pacing = InvocationPacing::default();
        for nth in 0..MAX_DEFERRED_INVOCATIONS {
            assert!(
                pacing.take_a_waiting_place(),
                "case place {nth} of {MAX_DEFERRED_INVOCATIONS}: it is inside the bound"
            );
        }
        assert!(
            !pacing.take_a_waiting_place(),
            "case one past the bound: no further invocation waits, got a place"
        );
        pacing.release_a_waiting_place();
        assert!(
            pacing.take_a_waiting_place(),
            "case a released place: it is available again, got none"
        );
    }
}
