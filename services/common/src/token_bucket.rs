// SPDX-License-Identifier: CC0-1.0

//! The ONE token bucket an emulated service meters a caller-driven request rate with.
//!
//! Several AWS services publish a request rate as a PAIR — a steady-state rate and a burst —
//! and document the pair as the token bucket that admits requests: API Gateway's usage-plan
//! `ThrottleSettings` ("`rateLimit` … the API request steady-state rate limit", "`burstLimit`
//! … the API request burst limit", "provided by the token bucket algorithm") and API Gateway
//! v2's per-route `RouteSettings` (`ThrottlingRateLimit` / `ThrottlingBurstLimit`) are the
//! same reading of the same algorithm. Two services asked whether a request is admitted must
//! not answer differently, and a second hand-written copy of the arithmetic is also a second
//! chance to get the refill or the cap wrong, so the bucket lives here once.
//!
//! What stays with the service is everything ABOUT ITS OWN MODEL: which members declare the
//! pair, what an absent or non-positive member means for that resource, and what the thing
//! being metered is keyed by. This module owns the bucket — how much capacity is left, how it
//! refills, how far it may be drawn, and whether one request may be admitted — and, for a pair
//! whose burst is the number of requests served AT ONCE, the accounting of the submissions in
//! flight together that may draw ahead of the rate ([`Submissions`]).
//!
//! Nothing here sleeps or waits: the capacity is computed from how much time has passed
//! since the last admitted request, so a caller that was idle finds its whole burst again
//! and a caller that was not finds what the rate refilled.
//!
//! One service still carries its own copy of this arithmetic, pinned by its own suites and to
//! be MIGRATED onto this module in the change that owns that service rather than copied again:
//! `service-com-amazonaws-xray-awsxray/src/state.rs` (`RateLimiter`, whose `burst` /
//! `per_second` / `last_refill` are the same bucket over a per-account request budget).

/// The request rate one metered thing is admitted at, as the bucket reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The requests per second the bucket refills at.
    pub rate: f64,
    /// The most requests admissible at once — the bucket's capacity.
    pub burst: f64,
}

/// The capacity one metered thing has left, and the instant that was last computed at.
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    tokens: f64,
    at: ::std::time::Instant,
}

/// The capacity one request takes out of a bucket.
const ONE_REQUEST: f64 = 1.0;

impl Bucket {
    /// A bucket holding `limits.burst` requests as of `now`: a caller that has sent nothing
    /// may burst up to the capacity.
    #[must_use]
    pub fn full(limits: Limits, now: ::std::time::Instant) -> Self {
        Self {
            tokens: limits.burst,
            at: now,
        }
    }

    /// Admit one request and record it, or report that `limits` admit none at `now`.
    ///
    /// The refill is the time since the last request times the rate, capped at the burst:
    /// the bucket never holds more than the capacity however long the caller has been idle,
    /// which is what makes the steady-state rate the rate. The limits are passed per call
    /// rather than held, so limits a resource is UPDATED to take effect on the next request
    /// instead of on a bucket frozen at the old ones.
    pub fn admits(&mut self, limits: Limits, now: ::std::time::Instant) -> bool {
        self.admits_cost(limits, now, ONE_REQUEST)
    }

    /// Admit one request that takes `cost` of the capacity and record it, or report that
    /// `limits` do not admit it at `now`.
    ///
    /// The same bucket as [`Self::admits`] — same refill, same cap — for a rate published
    /// over something other than whole requests: the records or the BYTES one call carries,
    /// where one request spends as much capacity as it brings. `cost` is the caller's: a
    /// cost larger than the burst can never be admitted, so a caller whose metered thing
    /// may exceed the capacity in one request decides what such a request spends before it
    /// asks here.
    pub fn admits_cost(&mut self, limits: Limits, now: ::std::time::Instant, cost: f64) -> bool {
        if self.capacity_at(limits, now) < cost {
            return false;
        }
        self.charge(cost);
        true
    }

    /// Admit one request that may draw on the capacity the rate has NOT refilled yet — up to
    /// one `limits.burst` AHEAD of it — and record it, or report that `limits` admit none even
    /// that far ahead at `now`.
    ///
    /// The same bucket as [`Self::admits`], with its floor one burst lower, and the same
    /// charge: what is taken ahead of the rate is still spent, so the rate REPAYS it and what
    /// a caller is served over time is still the steady-state rate. What this bounds is how far
    /// ahead of the rate the bucket may be drawn — a BUDGET, not a count, and one that is lower
    /// by a whole burst whatever the bucket already held, so a caller may take the capacity the
    /// bucket holds and then draw a burst more before the rate has refilled it, and is held to
    /// the rate afterwards until the draw is repaid. How MANY requests are served at once is a
    /// different bound and the caller's own: the reading a pair gets whose burst is the number
    /// served together counts the requests it is serving and stops asking at the burst.
    pub fn admits_ahead(&mut self, limits: Limits, now: ::std::time::Instant) -> bool {
        if self.capacity_at(limits, now) < ONE_REQUEST - limits.burst {
            return false;
        }
        self.charge(ONE_REQUEST);
        true
    }

    /// The capacity left at `now`, refilled first: the time since the last look times the
    /// rate, capped at the burst, which is what makes the steady-state rate the rate.
    ///
    /// The PEEK half of [`Self::admits_cost`], for a resource bounded by SEVERAL published
    /// rates at once (a Kinesis shard's records per second AND its write bytes per second):
    /// a request such a resource does not admit must spend none of them, so every budget is
    /// asked what it has left before any of them is charged. The limits are passed per call
    /// for the reason [`Self::admits`] gives.
    pub fn capacity_at(&mut self, limits: Limits, now: ::std::time::Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * limits.rate).min(limits.burst);
        self.at = now;
        self.tokens
    }

    /// Spend `cost` of the capacity: the CHARGE half, taken only once every budget of the
    /// resource has answered that it holds what the request brings
    /// ([`Self::capacity_at`]). Charges what it is given, so a caller that has not asked
    /// first can take the bucket below zero.
    pub fn charge(&mut self, cost: f64) {
        self.tokens -= cost;
    }
}

/// One metered thing's capacity BESIDE the submissions of it the service is still answering,
/// for a published pair whose burst is the number of requests served AT ONCE.
///
/// Several AWS services publish such a pair — API Gateway's per-method `MethodSetting`
/// throttle and API Gateway v2's per-route `RouteSettings` are the same reading — and what
/// that reading needs is the request metered AROUND the answer rather than at a point in it: a
/// submission is one the next arrival arrives TOGETHER with from the moment it arrives until
/// the service has answered it. The requests in flight together may draw ahead of the rate
/// ([`Bucket::admits_ahead`]) while fewer than burst-many are being served, so at one instant
/// a metered thing serves its burst and sheds the excess rather than all of it; what they take
/// is charged to the same bucket, so the rate repays it and what is served over time is still
/// the steady-state rate. A submission that arrives while nothing of it is in flight arrived
/// ALONE — at the instant it is metered the service has nothing else of it to serve it beside,
/// and cannot know what has not arrived yet — so the bucket alone answers it, which is what
/// meters a caller that waits for each answer before asking again.
///
/// Both budgets come back: the burst's room as each request is answered, the bucket's capacity
/// as the rate refills it, so a metered thing under load that never lets up keeps being served
/// rather than closing. What stays with the SERVICE is what its own model says — which members
/// declare the pair, what an absent one means, and what the thing being metered is keyed by.
#[derive(Debug)]
pub struct Submissions {
    /// The capacity the steady-state rate refills, which every admitted request takes one of.
    bucket: Bucket,
    /// How many submissions the service is answering right now: arrived and not answered yet.
    /// One the throttle SHEDS counts too while it is being answered, because it arrived
    /// alongside the others — it is one of the requests that arrived together, whatever it is
    /// answered with.
    in_flight: usize,
    /// How many requests are BEING SERVED right now: admitted and not answered yet. This is
    /// what the burst bounds — the requests in flight together — so it is released as each of
    /// them is answered rather than accumulated.
    served_in_flight: usize,
}

impl Submissions {
    /// A metered thing nothing has been metered against yet: a full bucket, nothing in flight.
    #[must_use]
    pub fn new(limits: Limits, now: ::std::time::Instant) -> Self {
        Self {
            bucket: Bucket::full(limits, now),
            in_flight: 0,
            served_in_flight: 0,
        }
    }

    /// One submission ARRIVES: whether `limits` admit it at `now`, taking its capacity for it
    /// when they do.
    ///
    /// Every submission this answers for must be released with [`Self::answered`] once the
    /// service has answered it, or it is held to be serving it for ever.
    pub fn arrived(&mut self, limits: Limits, now: ::std::time::Instant) -> bool {
        let arrived_together = self.answering();
        self.in_flight += 1;
        if !self.below_the_burst(limits) {
            return false;
        }
        let admitted = if arrived_together {
            self.bucket.admits_ahead(limits, now)
        } else {
            self.bucket.admits(limits, now)
        };
        if admitted {
            self.served_in_flight += 1;
        }
        admitted
    }

    /// One submission has been ANSWERED: it is no longer one of the requests a later arrival
    /// arrives together with, and one that was SERVED leaves the burst's room for the next.
    pub fn answered(&mut self, served: bool) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if served {
            self.served_in_flight = self.served_in_flight.saturating_sub(1);
        }
    }

    /// Whether the service is still answering a submission of this metered thing — which is
    /// what a later arrival arrives TOGETHER with, and what keeps the accounting alive while
    /// the records it belongs to are reclaimed.
    #[must_use]
    pub fn answering(&self) -> bool {
        self.in_flight > 0
    }

    /// Whether the burst leaves room for one more request to be served at once: what is being
    /// served TOGETHER is the burst, however many requests arrive and however much capacity
    /// the bucket holds when they do.
    fn below_the_burst(&self, limits: Limits) -> bool {
        let serving = u32::try_from(self.served_in_flight).unwrap_or(u32::MAX);
        f64::from(serving) < limits.burst
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_bucket_admits_its_burst_and_then_what_the_rate_refilled() {
        let start = Instant::now();
        let limits = Limits {
            rate: 2.0,
            burst: 2.0,
        };
        let mut bucket = Bucket::full(limits, start);
        // Rows differ along WHEN the request arrives: inside the burst, past it, and after
        // enough time for the rate to refill one request's capacity.
        let cases: [(&str, Duration, bool); 5] = [
            ("the first request of the burst", Duration::ZERO, true),
            ("the second request of the burst", Duration::ZERO, true),
            ("one past the burst", Duration::ZERO, false),
            ("still past it a moment later", Duration::from_millis(100), false),
            ("once the rate refilled a request", Duration::from_millis(600), true),
        ];
        for (case, elapsed, expected) in cases {
            let actual = bucket.admits(limits, start + elapsed);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected admitted {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn a_bucket_never_holds_more_than_its_burst_and_one_with_none_admits_nothing() {
        let start = Instant::now();
        // However long a caller has been idle, the capacity is the burst: three requests
        // after an hour of silence, where the burst is two, is one too many.
        let limits = Limits {
            rate: 1_000.0,
            burst: 2.0,
        };
        let mut bucket = Bucket::full(limits, start);
        let idle = start + Duration::from_secs(3_600);
        let admitted = [
            bucket.admits(limits, idle),
            bucket.admits(limits, idle),
            bucket.admits(limits, idle),
        ];
        assert_eq!(
            admitted,
            [true, true, false],
            "case {:?}: expected the burst and no more, got {admitted:?}",
            "three requests after an hour of silence"
        );
        // A bucket with no capacity admits nothing, however long it is left.
        let empty = Limits {
            rate: 0.0,
            burst: 0.0,
        };
        let mut none = Bucket::full(empty, start);
        for (case, elapsed) in [
            ("the first request", Duration::ZERO),
            ("a request a minute later", Duration::from_secs(60)),
        ] {
            let actual = none.admits(empty, start + elapsed);
            assert!(
                !actual,
                "case {case:?}: a bucket with no capacity must admit nothing, got {actual}"
            );
        }
    }

    #[test]
    fn a_request_spends_the_cost_it_brings_and_one_past_the_burst_is_never_admitted() {
        let start = Instant::now();
        let limits = Limits {
            rate: 1_000.0,
            burst: 1_000.0,
        };
        let mut bucket = Bucket::full(limits, start);
        // Rows differ along the COST one request brings, against the capacity left when it
        // arrives: a cost inside the capacity, the cost that spends exactly the rest of it,
        // one past what is left, what the rate refilled a moment later, and a cost no full
        // bucket could ever hold.
        let cases: [(&str, Duration, f64, bool); 6] = [
            ("a cost well inside the burst", Duration::ZERO, 400.0, true),
            ("the cost that spends the rest", Duration::ZERO, 600.0, true),
            ("one unit past an empty bucket", Duration::ZERO, 1.0, false),
            ("what the rate refilled", Duration::from_millis(100), 100.0, true),
            ("more than the refill", Duration::from_millis(100), 101.0, false),
            ("a cost past the whole burst", Duration::from_secs(60), 1_001.0, false),
        ];
        for (case, elapsed, cost, expected) in cases {
            let actual = bucket.admits_cost(limits, start + elapsed, cost);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected admitted {expected} for cost {cost}, got {actual}"
            );
        }
    }

    /// The peek-then-charge split: a resource bounded by SEVERAL published rates asks every
    /// budget what it holds before any of them is charged, so a request one of them refuses
    /// spends none.
    #[test]
    fn a_resource_bounded_by_two_rates_charges_neither_budget_when_one_refuses() {
        let start = Instant::now();
        let records = Limits {
            rate: 1_000.0,
            burst: 1_000.0,
        };
        let bytes = Limits {
            rate: 1_024.0,
            burst: 1_024.0,
        };
        let mut record_budget = Bucket::full(records, start);
        let mut byte_budget = Bucket::full(bytes, start);

        // One request past what the BYTE budget holds: it is refused, and the record budget
        // it never used keeps its whole capacity.
        let too_many_bytes = 2_048.0;
        let admitted = record_budget.capacity_at(records, start) >= 1.0
            && byte_budget.capacity_at(bytes, start) >= too_many_bytes;
        assert!(
            !admitted,
            "case a request past one of two budgets: expected it refused"
        );
        assert_eq!(
            record_budget.capacity_at(records, start),
            1_000.0,
            "case a request past one of two budgets: the budget that did not refuse it keeps \
             its capacity"
        );

        // The same request inside both budgets charges both.
        assert!(
            record_budget.capacity_at(records, start) >= 1.0
                && byte_budget.capacity_at(bytes, start) >= 512.0,
            "case a request inside both budgets: expected it admitted"
        );
        record_budget.charge(1.0);
        byte_budget.charge(512.0);
        assert_eq!(
            (
                record_budget.capacity_at(records, start),
                byte_budget.capacity_at(bytes, start)
            ),
            (999.0, 512.0),
            "case a request inside both budgets: expected both charged what it brought"
        );
    }

    /// One row of the draw-ahead table: the pair the limits state, how many requests the
    /// burst is worth, and how long the rate takes to repay that draw and one request more.
    struct Ahead {
        case: &'static str,
        rate: f64,
        burst: f64,
        worth: usize,
        repaid: Duration,
    }

    /// Drawing AHEAD of the rate: a bucket the rate has not refilled yet admits up to one
    /// burst more, charges every one of them, and admits nothing again until the rate has
    /// repaid what was taken — so the burst bounds how far ahead the bucket may be drawn and
    /// the rate still bounds what is taken over time.
    #[test]
    fn a_bucket_drawn_ahead_of_its_rate_is_repaid_before_it_admits_again() {
        let start = Instant::now();
        // Rows differ along the pair: a burst of one, a wider burst, a fractional rate, a
        // burst no rate ever repays, and a pair stating no capacity at all.
        let cases = [
            Ahead {
                case: "a burst of one",
                rate: 1.0,
                burst: 1.0,
                worth: 1,
                repaid: Duration::from_secs(2),
            },
            Ahead {
                case: "a burst of four",
                rate: 10.0,
                burst: 4.0,
                worth: 4,
                repaid: Duration::from_millis(500),
            },
            Ahead {
                case: "a rate stated in fractions of a request",
                rate: 0.5,
                burst: 2.0,
                worth: 2,
                repaid: Duration::from_secs(6),
            },
            Ahead {
                case: "a burst no rate ever repays",
                rate: 0.0,
                burst: 1.0,
                worth: 1,
                repaid: Duration::from_secs(60),
            },
            Ahead {
                case: "limits that admit nothing",
                rate: 0.0,
                burst: 0.0,
                worth: 0,
                repaid: Duration::from_secs(60),
            },
        ];
        for row in cases {
            let case = row.case;
            let limits = Limits {
                rate: row.rate,
                burst: row.burst,
            };
            let mut bucket = Bucket::full(limits, start);
            // Spend what the bucket held, the way the requests already admitted spend it.
            let mut spent = 0usize;
            while bucket.admits(limits, start) {
                spent += 1;
                assert!(
                    spent <= row.worth,
                    "case {case:?}: a full bucket admitted more than its burst"
                );
            }
            // Then draw ahead: the burst is how far the bucket may be drawn before the rate
            // has refilled it.
            let mut ahead = 0usize;
            while bucket.admits_ahead(limits, start) {
                ahead += 1;
                assert!(
                    ahead <= row.worth,
                    "case {case:?}: drawing ahead was not bounded by the burst"
                );
            }
            assert_eq!(
                (spent, ahead),
                (row.worth, row.worth),
                "case {case:?}: expected the burst spent and the burst drawn ahead, got \
                 {spent} spent and {ahead} ahead"
            );
            // What was drawn ahead is charged, so nothing is admitted at that instant.
            assert!(
                !bucket.admits_ahead(limits, start),
                "case {case:?}: expected nothing admitted past the draw the burst bounds"
            );
            // Once the rate has repaid the draw, the bucket is the rate's again — and a rate
            // that refills nothing never repays it.
            let after = bucket.admits(limits, start + row.repaid);
            assert_eq!(
                after,
                row.rate > 0.0,
                "case {case:?}: expected the repaid bucket to admit a request again only \
                 where the rate refills it at all, got {after}"
            );
        }
    }

    /// One row of the submissions table: the pair, how much of the bucket a caller that waits
    /// for each answer spent first, how many submissions then arrive before any is answered,
    /// and how many of those are served.
    struct Together {
        case: &'static str,
        rate: f64,
        burst: f64,
        spent_alone: usize,
        at_once: usize,
        served: usize,
    }

    /// The submissions in flight TOGETHER are served the burst and no more, so a burst larger
    /// than the allowance is partially shed rather than shed whole — whether the bucket was
    /// full or spent when they arrived — and a caller that waits for each answer is metered by
    /// the bucket alone.
    #[test]
    fn submissions_that_arrive_together_are_served_the_burst_and_no_more() {
        let start = Instant::now();
        // Rows differ along the pair, along how much of the bucket was spent before the
        // submissions arrive (nothing, and all of it), and along how many arrive: fewer than
        // the burst, and far more.
        let rows = [
            Together {
                case: "a burst of one met by forty at once, its bucket spent",
                rate: 1.0,
                burst: 1.0,
                spent_alone: 1,
                at_once: 40,
                served: 1,
            },
            Together {
                case: "a burst of one met by forty at once, its bucket full",
                rate: 1.0,
                burst: 1.0,
                spent_alone: 0,
                at_once: 40,
                served: 1,
            },
            Together {
                case: "a burst of five met by forty at once, its bucket spent",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 5,
                at_once: 40,
                served: 5,
            },
            Together {
                // The first of them arrives with nothing in flight, so a SPENT bucket sheds
                // it and only the rest may draw ahead.
                case: "fewer submissions than the burst, its bucket spent",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 5,
                at_once: 3,
                served: 2,
            },
            Together {
                case: "fewer submissions than the burst, its bucket full",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 0,
                at_once: 3,
                served: 3,
            },
            Together {
                case: "a burst no rate ever repays",
                rate: 0.0,
                burst: 1.0,
                spent_alone: 1,
                at_once: 20,
                served: 1,
            },
        ];
        for row in rows {
            let case = row.case;
            let limits = Limits {
                rate: row.rate,
                burst: row.burst,
            };
            let mut metered = Submissions::new(limits, start);
            // The caller that waits for every answer: each request arrives with nothing in
            // flight, so the bucket's own capacity is what answers it.
            for index in 0..row.spent_alone {
                let served = metered.arrived(limits, start);
                assert!(
                    served,
                    "case {case:?}: expected request {index} of the capacity served"
                );
                assert!(
                    metered.answering(),
                    "case {case:?}: a submission is in flight until it is answered"
                );
                metered.answered(served);
            }
            assert!(
                !metered.answering(),
                "case {case:?}: every answered submission is released"
            );
            // Then the submissions that arrive together: none is answered until all have
            // arrived.
            let decided: Vec<bool> = (0..row.at_once)
                .map(|_| metered.arrived(limits, start))
                .collect();
            let served = decided.iter().filter(|served| **served).count();
            assert_eq!(
                served, row.served,
                "case {case:?}: expected {} of {} submissions served at once, got {served} \
                 ({decided:?})",
                row.served, row.at_once
            );
            for served in &decided {
                metered.answered(*served);
            }
            assert!(
                !metered.answering(),
                "case {case:?}: the whole burst is released once it is answered"
            );
        }
    }
}
