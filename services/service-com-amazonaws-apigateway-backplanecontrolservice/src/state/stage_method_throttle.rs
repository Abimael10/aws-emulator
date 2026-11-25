// SPDX-License-Identifier: CC0-1.0

//! The rate ONE deployed method of a stage admits requests at, as the token bucket that
//! meters it holds the accounting.
//!
//! A stage's `methodSettings` entry carries a THROTTLE of its own —
//! `MethodSetting$throttlingRateLimit` ("specifies the throttling rate limit") and
//! `MethodSetting$throttlingBurstLimit` ("specifies the throttling burst limit") — which is
//! the request rate the STAGE admits for the method the entry applies to, whoever is
//! calling. It is a different limit from a usage plan's: a plan's throttle is per API key it
//! meters ([`super::ThrottleBucket`] keyed by [`super::metering_key`]), while this one is the
//! method's on that stage and applies to every request it serves, keyed by
//! [`StageMethodThrottleKey`].
//!
//! Neither the ARITHMETIC of the bucket nor the accounting of the requests in flight together
//! is written here: both are the tree's one token bucket
//! ([`::service_common::token_bucket`], whose [`Submissions`] this module meters a method
//! with), so no two surfaces metering a published rate-and-burst pair can disagree about
//! whether a request is admitted. What this module owns is what the MODEL says the pair means
//! for a method of a stage: which members declare it, what a member that bounds nothing means,
//! and what the thing metered is keyed by.
//!
//! What that shared reading gives a method is what the burst limit states: the burst is how
//! many requests the stage serves AT ONCE — "the burst quota is determined by the maximum
//! concurrent request submissions API Gateway can fulfill without returning a 429" — so the
//! submissions of one method in flight TOGETHER may draw ahead of the rate, and the burst
//! bounds both how many of them are served at once and how far they may draw, while a request
//! that arrives with nothing of the method in flight is answered by the bucket alone. So at
//! one instant a throttled method serves its burst and sheds the excess rather than all of it,
//! and over time it serves its rate.

use std::collections::BTreeMap;

use ::service_common::token_bucket::Submissions;

use crate::ops::types::MethodSetting;

use super::{
    accepts_method_throttle_burst, accepts_method_throttle_rate,
    ComAmazonawsApigatewayBackplanecontrolserviceState as State, ThrottleLimits,
};

/// What ONE throttled method of one stage is accounted under: the API, the stage, and the
/// METHOD PATH the limits are in force for — `{resource_path}/{http_method}`, the form
/// `Stage$methodSettings` names a method by.
///
/// The METHOD rather than the `methodSettings` key the limits were read from, because the
/// limits are each method's: an entry keyed by the whole-stage wildcard is the DEFAULT for
/// every method of the stage ("default method throttling"), so each of them is metered at
/// that rate on a bucket of its own, exactly as one named by its own entry is.
pub type StageMethodThrottleKey = (String, String, String);

/// The key the throttle of ONE method of one stage is accounted under; see
/// [`StageMethodThrottleKey`].
///
/// `resource_path` is the deployed resource path the call matched and `http_method` the verb
/// the serving method is configured under, which the pair spells the way
/// `Stage$methodSettings` spells a method path — "method paths defined as
/// `{resource_path}/{http_method}`" — with the root resource's own trailing separator not
/// doubled, so the api's root method is `/{http_method}`.
#[must_use]
pub fn stage_method_throttle_key(
    rest_api_id: &str,
    stage_name: &str,
    resource_path: &str,
    http_method: &str,
) -> StageMethodThrottleKey {
    (
        rest_api_id.to_string(),
        stage_name.to_string(),
        format!("{}/{http_method}", resource_path.trim_end_matches('/')),
    )
}

/// The limits the `methodSettings` entry `setting` declares, or `None` when it declares none
/// that limits anything.
///
/// Either member may carry the sentinel that asks for no throttle at all
/// ([`super::UNTHROTTLED_LIMIT`], which [`accepts_method_throttle_burst`] and
/// [`accepts_method_throttle_rate`] admit beside the counts), and a limit that is not
/// POSITIVE limits nothing — so a method whose settings leave either member unset, or set
/// either to the sentinel, admits every request.
#[must_use]
pub fn method_setting_limits(setting: &MethodSetting) -> Option<ThrottleLimits> {
    let burst = setting.throttling_burst_limit;
    let rate = setting.throttling_rate_limit;
    if !accepts_method_throttle_burst(burst) || !accepts_method_throttle_rate(rate) {
        return None;
    }
    let limits = ThrottleLimits {
        burst: f64::from(burst),
        rate,
    };
    (limits.burst > 0.0 && limits.rate.is_finite() && limits.rate > 0.0).then_some(limits)
}

impl State {
    /// One submission of this method ARRIVES: whether the stage's throttle for it admits the
    /// request, taking its capacity for it when it does.
    ///
    /// This is the one place a stage's own `methodSettings` throttle is accounted, so every
    /// request the stage serves through that entry draws on one bucket; a bucket starts FULL,
    /// so a method nothing has called yet admits a burst up to its limit, and the limits a
    /// later `UpdateStage` writes take effect on the next request rather than on a bucket
    /// frozen at the old ones.
    ///
    /// What the arrival decides is how far the bucket may be drawn for it: a request that
    /// arrives while the method has others still unanswered is one of the requests in flight
    /// TOGETHER, so it may draw on capacity the rate has not refilled yet while fewer than
    /// burst-many requests are being served, and a request that arrives alone takes only what
    /// the bucket holds. Neither budget is spent for good — the burst's room comes back as
    /// each request is answered ([`Self::stage_method_throttle_answered`]) and the bucket's
    /// capacity as the rate refills it.
    ///
    /// Every submission this answers for must be released with
    /// [`Self::stage_method_throttle_answered`] once the stage has answered it, or the method
    /// is held to be serving it for ever.
    ///
    /// Bounded by the records the control plane holds — one entry per
    /// `(API, stage, method path)` — and the entries whose API or stage no longer exists are
    /// forgotten when one is ADDED, which is the only step that grows the map: a request an
    /// entry already exists for touches that entry alone, so the hot path stays the one
    /// lookup, and the map never grows with the request count nor with the stages an account
    /// has created and deleted.
    pub fn stage_method_throttle_arrived(
        &mut self,
        key: &StageMethodThrottleKey,
        limits: ThrottleLimits,
        now: ::std::time::Instant,
    ) -> bool {
        if !self.stage_method_throttle_buckets.contains_key(key) {
            // Taken out and put back because the retain reads the api and stage records
            // beside it. An entry the stage is still answering a submission of is kept
            // whatever became of its records: it is released by the answer, not here.
            let mut buckets = ::std::mem::take(&mut self.stage_method_throttle_buckets);
            buckets.retain(|(rest_api_id, stage_name, _), metered| {
                metered.answering()
                    || self
                        .rest_apis
                        .get(rest_api_id)
                        .is_some_and(|api| api.stages.contains_key(stage_name))
            });
            buckets.insert(key.clone(), Submissions::new(limits, now));
            self.stage_method_throttle_buckets = buckets;
        }
        self.stage_method_throttle_buckets
            .get_mut(key)
            .is_some_and(|metered| metered.arrived(limits, now))
    }

    /// The stage has ANSWERED one submission of this method: it is no longer one of the
    /// requests a later arrival arrives together with, and one it SERVED leaves the burst's
    /// room for the next.
    ///
    /// An entry this holds nothing for is one whose api or stage was deleted while the stage
    /// was still answering a submission of it — the accounting it belonged to is gone with the
    /// records, so there is nothing to release.
    pub fn stage_method_throttle_answered(&mut self, key: &StageMethodThrottleKey, served: bool) {
        let Some(metered) = self.stage_method_throttle_buckets.get_mut(key) else {
            ::tracing::debug!(
                api_id = %key.0,
                stage = %key.1,
                method = %key.2,
                "a submission was answered for a method this account no longer meters, so \
                 nothing of it is held"
            );
            return;
        };
        metered.answered(served);
    }
}

/// The accounting as the state field holds it; see [`StageMethodThrottleKey`].
pub type StageMethodThrottleBuckets = BTreeMap<StageMethodThrottleKey, Submissions>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The key every row below meters against: one method of one stage.
    fn metered_method() -> StageMethodThrottleKey {
        stage_method_throttle_key("a1b2c3d4e5", "test", "/", "GET")
    }

    /// One row of the arrival table: what a failing assertion calls it, the pair the stage's
    /// settings state, how many submissions arrive TOGETHER, how much of the bucket is spent
    /// before they do, and how many of them the throttle admits.
    struct Burst {
        case: &'static str,
        rate: f64,
        burst: f64,
        /// How many requests were served and ANSWERED before the burst arrives, each one
        /// alone, which is what spends the bucket the burst then meets.
        spent_alone: usize,
        /// How many submissions of the burst are in flight together.
        at_once: usize,
        /// How many of them the throttle admits.
        admitted: usize,
    }

    /// RULE: the submissions of one method in flight together are served the burst and no
    /// more, so a burst larger than the allowance is PARTIALLY shed rather than shed whole,
    /// whatever the bucket held when it arrived.
    #[test]
    fn submissions_that_arrive_together_are_served_the_burst_and_no_more() {
        let start = Instant::now();
        // Rows differ along the pair the settings state, along how much of the bucket the
        // requests before the burst spent, and along how many submissions arrive together —
        // fewer than the burst, exactly the burst, and far more.
        let rows = [
            Burst {
                case: "a burst of one met by forty at once, its bucket spent",
                rate: 1.0,
                burst: 1.0,
                spent_alone: 1,
                at_once: 40,
                admitted: 1,
            },
            Burst {
                case: "a burst of one met by forty at once, its bucket full",
                rate: 1.0,
                burst: 1.0,
                spent_alone: 0,
                at_once: 40,
                admitted: 1,
            },
            Burst {
                case: "a burst of five met by forty at once, its bucket spent",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 5,
                at_once: 40,
                admitted: 5,
            },
            Burst {
                case: "a burst of five met by forty at once, its bucket full",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 0,
                at_once: 40,
                admitted: 5,
            },
            Burst {
                // The submissions that meet a burst wider than they are: the first of them
                // arrives with nothing of the method in flight, so the bucket — still full
                // here — answers it, and the rest draw on what is left.
                case: "a burst wider than the submissions that meet it, its bucket full",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 0,
                at_once: 3,
                admitted: 3,
            },
            Burst {
                // The same submissions against a SPENT bucket: the first arrived alone, so
                // the spent bucket sheds it, and the rest draw ahead of the rate.
                case: "a burst wider than the submissions that meet it, its bucket spent",
                rate: 10.0,
                burst: 5.0,
                spent_alone: 5,
                at_once: 3,
                admitted: 2,
            },
            Burst {
                case: "a rate stated in fractions of a request",
                rate: 0.5,
                burst: 2.0,
                spent_alone: 2,
                at_once: 20,
                admitted: 2,
            },
        ];
        for row in rows {
            let case = row.case;
            let key = metered_method();
            let limits = ThrottleLimits {
                rate: row.rate,
                burst: row.burst,
            };
            let mut state = State::default();
            // The requests before the burst: each arrives alone and is answered before the
            // next one, which is the caller that waits for every answer.
            for index in 0..row.spent_alone {
                let served = state.stage_method_throttle_arrived(&key, limits, start);
                assert!(
                    served,
                    "case {case:?}: request {index} of the bucket's own capacity must be served"
                );
                state.stage_method_throttle_answered(&key, served);
            }
            // Then the burst: every submission arrives before any of them is answered.
            let decided: Vec<bool> = (0..row.at_once)
                .map(|_| state.stage_method_throttle_arrived(&key, limits, start))
                .collect();
            let admitted = decided.iter().filter(|served| **served).count();
            assert_eq!(
                admitted, row.admitted,
                "case {case:?}: expected {} of {} submissions served at once, got {admitted}",
                row.admitted, row.at_once
            );
            // Release every submission, the way the stage releases one as it answers it.
            for served in &decided {
                state.stage_method_throttle_answered(&key, *served);
            }
        }
    }

    /// RULE: what a burst drew ahead of the rate is CHARGED, so the method is served at its
    /// rate over time — the next caller that waits for each answer is shed until the rate has
    /// repaid the draw, and served once it has.
    #[test]
    fn a_method_drawn_ahead_of_its_rate_is_repaid_before_it_is_served_again() {
        let start = Instant::now();
        let key = metered_method();
        let limits = ThrottleLimits {
            rate: 1.0,
            burst: 1.0,
        };
        let mut state = State::default();
        // The bucket's own capacity, spent by a request that arrived alone.
        let first = state.stage_method_throttle_arrived(&key, limits, start);
        state.stage_method_throttle_answered(&key, first);
        // Two submissions together: the second draws the burst ahead of the rate.
        let together: Vec<bool> = (0..2)
            .map(|_| state.stage_method_throttle_arrived(&key, limits, start))
            .collect();
        for served in &together {
            state.stage_method_throttle_answered(&key, *served);
        }
        assert_eq!(
            (first, together.clone()),
            (true, vec![false, true]),
            "case a burst of one drawn ahead: expected the lone request served, the first of \
             the pair shed and the second drawing the burst, got {first} then {together:?}"
        );
        // Rows differ along how long the rate has had to repay the draw: nothing yet, part of
        // it, and the whole draw plus one request's capacity.
        let rows: [(&str, Duration, bool); 3] = [
            ("at once, with the draw unrepaid", Duration::ZERO, false),
            ("part of the draw repaid", Duration::from_millis(900), false),
            ("the draw and one request repaid", Duration::from_secs(3), true),
        ];
        for (case, elapsed, expected) in rows {
            let served = state.stage_method_throttle_arrived(&key, limits, start + elapsed);
            state.stage_method_throttle_answered(&key, served);
            assert_eq!(
                served, expected,
                "case {case:?}: expected served {expected}, got {served}"
            );
        }
    }

    /// RULE: a method setting's throttle limits something only where both members state a
    /// positive limit; the sentinel that asks for no throttle, and anything not positive,
    /// meter nothing.
    #[test]
    fn a_method_setting_declares_limits_only_where_both_members_bound_something() {
        /// One row: what a failing assertion calls it, the two members the setting states,
        /// and the `(burst, rate)` the method is then metered at.
        type Row = (&'static str, i32, f64, Option<(f64, f64)>);

        // Rows differ along each member: both positive, each at the unthrottled sentinel,
        // each at zero, and a fractional rate.
        let rows: [Row; 7] = [
            ("both limits positive", 5, 10.0, Some((5.0, 10.0))),
            ("a burst of one", 1, 1.0, Some((1.0, 1.0))),
            ("a fractional rate", 2, 0.5, Some((2.0, 0.5))),
            ("the unthrottled burst sentinel", -1, 10.0, None),
            ("the unthrottled rate sentinel", 5, -1.0, None),
            ("a burst of none", 0, 10.0, None),
            ("a rate of none", 5, 0.0, None),
        ];
        for (case, burst, rate, expected) in rows {
            let setting = MethodSetting {
                throttling_burst_limit: burst,
                throttling_rate_limit: rate,
                ..MethodSetting::default()
            };
            let actual = method_setting_limits(&setting);
            let expected = expected.map(|(burst, rate)| ThrottleLimits { burst, rate });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
