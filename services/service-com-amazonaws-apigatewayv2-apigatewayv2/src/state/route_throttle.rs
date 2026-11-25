// SPDX-License-Identifier: CC0-1.0

//! The throttle a stage's route settings put in front of a route: what is metered, at what
//! rate, and when a request is refused for it.
//!
//! `RouteSettings` carries two members, and both are the token bucket API Gateway meters a
//! route with: "ThrottlingBurstLimit — specifies the throttling burst limit" is how many
//! requests may arrive at once, and "ThrottlingRateLimit — specifies the throttling rate
//! limit" is how many per second may arrive on average
//! (`smithy_query(query="shape", shape="RouteSettings")`). A request the bucket has no
//! capacity for is the one the gateway refuses itself, before the route's integration is
//! reached at all, so a route whose settings throttle it and its twin on the same stage
//! answer differently for the same api.
//!
//! The ARITHMETIC of the bucket is not written here, and neither is the accounting of the
//! submissions in flight together: both are the tree's one token bucket
//! ([`::service_common::token_bucket`], whose [`Submissions`] this module meters a route
//! with), so this service and every other that meters a published rate-and-burst pair cannot
//! disagree about whether a request is admitted. What this module owns is what the MODEL says
//! — which members declare the pair, what an absent one means, and that the thing metered is
//! one route of one stage.
//!
//! What that shared reading gives a route is what the burst limit states. The burst a
//! published pair names is how many requests the gateway serves AT ONCE, so the requests in
//! flight TOGETHER at a route may draw ahead of the rate, and the burst is BOTH how many of
//! them are served at once and how far they may draw: what they take is charged to the same
//! bucket, so the rate repays it and the route is served at its rate over time. Both budgets
//! come back — the burst's room as each request is answered, the bucket's capacity as the rate
//! refills it — so a route under load that never lets up keeps being served. A request that
//! arrives while the route has nothing in flight arrived ALONE — at the instant it is metered
//! the gateway has nothing else of the route to serve it beside, and cannot know what has not
//! arrived yet — so the bucket answers it, which is what meters a caller that waits for each
//! answer before asking again. So at one instant a throttled route serves its burst and sheds
//! the excess rather than all of it, and over time it serves its rate — a route whose bucket
//! was full may serve the burst it held and then a burst drawn ahead of the rate before the
//! rate has refilled either, which is the draw being repaid rather than a second burst being
//! granted.
//!
//! The buckets are DATA-PLANE state: how much of each route's capacity the requests already
//! served have taken, and which submissions of it the gateway is still answering. A
//! route the stage states NEITHER limit for is not metered at all: the stage configured no
//! throttle for it, and metering it against a limit nothing declared would refuse requests no
//! setting asked to be refused.

use std::collections::BTreeMap;
use std::time::Instant;

use ::service_common::token_bucket::{Limits as BucketLimits, Submissions};

/// The requests per second a route is metered at when its settings name a burst limit but no
/// rate: the account-level rate quota API Gateway publishes for a Region, which is the
/// throttle in force when nothing narrower is configured.
const ACCOUNT_RATE_LIMIT: f64 = 10_000.0;

/// The burst a route is metered with when its settings name a rate limit but no burst: the
/// account-level burst quota API Gateway publishes, for the same reason.
const ACCOUNT_BURST_LIMIT: f64 = 5_000.0;

/// The throttle one route is metered with, as the bucket reads it.
pub type Limits = BucketLimits;

/// The limits the stage's settings for one route state, or `None` for a route the stage
/// states neither limit for — which is not metered.
///
/// A member the settings leave out is the account-level quota that applies when nothing
/// narrower is configured, so a setting that names only one of the two still meters the
/// route by the one it names. A negative value names no capacity at all and is read as none,
/// the way a zero is.
pub fn limits_of(rate: Option<f64>, burst: Option<i32>) -> Option<Limits> {
    if rate.is_none() && burst.is_none() {
        return None;
    }
    Some(Limits {
        rate: rate.filter(|rate| rate.is_finite()).unwrap_or(ACCOUNT_RATE_LIMIT).max(0.0),
        burst: burst
            .map(|burst| f64::from(burst).max(0.0))
            .unwrap_or(ACCOUNT_BURST_LIMIT),
    })
}

/// The api, the stage and the route key one metered route's accounting is held under.
type MeteredRoute = (String, String, String);

/// The throttles of every metered route of this account.
#[derive(Debug, Default)]
pub struct RouteThrottles {
    routes: BTreeMap<MeteredRoute, Submissions>,
}

impl RouteThrottles {
    /// One submission of this route ARRIVES: whether the route's throttle admits it, taking
    /// its capacity for it when it does.
    ///
    /// One budget answers — the bucket the rate refills, which every admitted request is
    /// charged one of. What the arrival decides is how far the bucket may be drawn for it, and
    /// the burst decides how many requests the route is served AT ONCE, which is the shared
    /// reading of a pair whose burst is the number served together
    /// ([`Submissions::arrived`]): a request that arrives while the route has others still
    /// unanswered may draw on capacity the rate has not refilled yet while fewer than
    /// burst-many requests are being served, and one that arrives while the route has nothing
    /// in flight takes only what the bucket holds.
    ///
    /// Neither budget is spent for good: the burst's room comes back as each request is
    /// answered ([`Self::answered`]) and the bucket's capacity as the rate refills it, which is
    /// what keeps a route under load that never lets up being served at its rate rather than
    /// closing.
    ///
    /// Every submission this answers for must be released with [`Self::answered`] once the
    /// gateway has answered it, or the route is held to be serving it for ever.
    pub fn arrived(
        &mut self,
        api_id: &str,
        stage_name: &str,
        route_key: &str,
        limits: Limits,
        now: Instant,
    ) -> bool {
        self.routes
            .entry(route_of(api_id, stage_name, route_key))
            .or_insert_with(|| Submissions::new(limits, now))
            .arrived(limits, now)
    }

    /// The gateway has ANSWERED one submission of this route: it is no longer one of the
    /// requests a later arrival arrives together with, and one it SERVED leaves the burst's
    /// room for the next.
    ///
    /// A route this holds nothing for is one whose api was deleted while the gateway was still
    /// answering a submission of it ([`Self::clear_api`]) — the accounting it belonged to is
    /// gone with the api, so there is nothing to release.
    pub fn answered(&mut self, api_id: &str, stage_name: &str, route_key: &str, served: bool) {
        let Some(metered) = self.routes.get_mut(&route_of(api_id, stage_name, route_key)) else {
            ::tracing::debug!(
                api_id,
                stage = stage_name,
                route_key,
                "a submission was answered for a route this account no longer meters, so \
                 nothing of it is held"
            );
            return;
        };
        metered.answered(served);
    }

    /// Forget every bucket of one api, for an api that is no longer there.
    pub fn clear_api(&mut self, api_id: &str) {
        self.routes.retain(|(held, _, _), _| held != api_id);
    }
}

/// The key one metered route is held under.
fn route_of(api_id: &str, stage_name: &str, route_key: &str) -> MeteredRoute {
    (
        api_id.to_string(),
        stage_name.to_string(),
        route_key.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// One row: what a failing assertion calls it, the two members the settings state, and
    /// the `(rate, burst)` the route is then metered at.
    type Row = (&'static str, Option<f64>, Option<i32>, Option<(f64, f64)>);

    #[test]
    fn a_route_is_metered_only_where_its_settings_state_a_limit() {
        // Rows differ along which of the two members the settings state: neither (not
        // metered), each alone (the other is the account quota), and both.
        let cases: [Row; 5] = [
            ("settings naming neither limit", None, None, None),
            (
                "a rate limit alone",
                Some(10.0),
                None,
                Some((10.0, ACCOUNT_BURST_LIMIT)),
            ),
            (
                "a burst limit alone",
                None,
                Some(7),
                Some((ACCOUNT_RATE_LIMIT, 7.0)),
            ),
            ("both limits", Some(2.0), Some(1), Some((2.0, 1.0))),
            ("limits that admit nothing", Some(0.0), Some(0), Some((0.0, 0.0))),
        ];
        for (case, rate, burst, expected) in cases {
            let actual = limits_of(rate, burst);
            let expected = expected.map(|(rate, burst)| Limits { rate, burst });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// One sequential submission of a route: it arrives, it is answered, and the next one
    /// finds nothing of it in flight — the caller that waits for each answer before asking
    /// again.
    fn sequential(
        throttles: &mut RouteThrottles,
        route_key: &str,
        limits: Limits,
        now: Instant,
    ) -> bool {
        let admitted = throttles.arrived("api", "$default", route_key, limits, now);
        throttles.answered("api", "$default", route_key, admitted);
        admitted
    }

    #[test]
    fn each_route_is_metered_against_its_own_settings() {
        let start = Instant::now();
        let mut throttles = RouteThrottles::default();
        let limits = Limits {
            rate: 2.0,
            burst: 2.0,
        };
        // Rows differ along WHEN the request to the metered route arrives: inside the burst,
        // past it, and after enough time for the rate to refill one request's capacity. Each
        // is answered before the next arrives, so what admits them is the bucket alone.
        let cases: [(&str, Duration, bool); 5] = [
            ("the first request of the burst", Duration::ZERO, true),
            ("the second request of the burst", Duration::ZERO, true),
            ("one past the burst", Duration::ZERO, false),
            ("still past it a moment later", Duration::from_millis(100), false),
            ("once the rate refilled a request", Duration::from_millis(600), true),
        ];
        for (case, elapsed, expected) in cases {
            let actual = sequential(&mut throttles, "GET /orders", limits, start + elapsed);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected admitted {expected}, got {actual}"
            );
        }
        // Another route's bucket is its own: one route's throttle never refuses a twin, and
        // neither does the same route key on another stage or another api.
        for (case, api, stage, route_key) in [
            ("the twin route of a throttled one", "api", "$default", "GET /twin"),
            ("the same route key on another stage", "api", "prod", "GET /orders"),
            ("the same route key of another api", "other", "$default", "GET /orders"),
        ] {
            let admitted = throttles.arrived(api, stage, route_key, limits, start);
            throttles.answered(api, stage, route_key, admitted);
            assert!(
                admitted,
                "case {case:?}: a route is metered against its own settings only"
            );
        }
        // And a bucket an api no longer has is forgotten with it.
        throttles.clear_api("api");
        assert!(
            sequential(&mut throttles, "GET /orders", limits, start),
            "case {:?}: an api that is gone leaves no bucket behind",
            "a metered route of a deleted api"
        );
    }

    /// One row of the together-at-once table: the pair the settings state, how much of the
    /// bucket the requests already served have spent, how many submissions arrive together,
    /// and what the throttle then does.
    struct Together {
        case: &'static str,
        rate: f64,
        burst: f64,
        /// How many of the bucket's capacity are spent before the submissions arrive — none
        /// for a route whose bucket is still full.
        spent: usize,
        /// How many submissions arrive before any of them is answered.
        submissions: usize,
        /// How many of them are served.
        served: usize,
        /// Whether the FIRST of them is one of those: it arrives while the route is answering
        /// nothing, so what answers it is the bucket's own capacity.
        first_served: bool,
        /// How many of a SECOND set of submissions, arriving at once while the first set's
        /// draw is unrepaid, are served — what the burst's draw still allows.
        served_again: usize,
        /// How long the rate takes to repay both draws and refill one request more.
        repaid: Duration,
    }

    /// The burst as what the requests that arrive TOGETHER are served and may draw ahead of
    /// the rate: of the submissions a metered route has in flight at once, burst-many are
    /// served and the rest are shed — whether the bucket is full or spent at the instant they
    /// arrive, which is what keeps a burst from being shed whole without serving twice the
    /// burst — and the rate repays what they drew before the route is served again.
    #[test]
    fn submissions_that_arrive_together_are_served_the_burst_and_no_more() {
        let start = Instant::now();
        // Rows differ along the pair the settings state, how much of the bucket is left when
        // the submissions arrive (nothing, and the whole burst), how many of them arrive
        // (fewer than the burst, and far more), and a rate that never repays.
        let cases = [
            Together {
                case: "a burst of one met by forty at once, its bucket spent",
                rate: 1.0,
                burst: 1.0,
                spent: 1,
                submissions: 40,
                served: 1,
                first_served: false,
                served_again: 0,
                repaid: Duration::from_secs(2),
            },
            Together {
                case: "a burst of one met by two at once, its bucket spent",
                rate: 1.0,
                burst: 1.0,
                spent: 1,
                submissions: 2,
                served: 1,
                first_served: false,
                served_again: 0,
                repaid: Duration::from_secs(2),
            },
            Together {
                case: "a burst of five met by forty at once, its bucket spent",
                rate: 1.0,
                burst: 5.0,
                spent: 5,
                submissions: 40,
                served: 5,
                first_served: false,
                served_again: 0,
                repaid: Duration::from_secs(6),
            },
            Together {
                case: "a burst of four met by forty at once, its bucket FULL",
                rate: 1.0,
                burst: 4.0,
                spent: 0,
                submissions: 40,
                served: 4,
                first_served: true,
                // The four it served are charged, so the next set is served only what the
                // draw the burst allows still leaves — and then nothing until the rate repays.
                served_again: 4,
                repaid: Duration::from_secs(5),
            },
            Together {
                case: "a burst wider than the submissions that meet it",
                rate: 1.0,
                burst: 5.0,
                spent: 5,
                submissions: 3,
                served: 2,
                first_served: false,
                served_again: 2,
                repaid: Duration::from_secs(5),
            },
            Together {
                case: "a rate stated in fractions of a request",
                rate: 0.5,
                burst: 2.0,
                spent: 2,
                submissions: 10,
                served: 2,
                first_served: false,
                served_again: 0,
                repaid: Duration::from_secs(6),
            },
            Together {
                case: "a burst no rate ever repays",
                rate: 0.0,
                burst: 1.0,
                spent: 1,
                submissions: 40,
                served: 1,
                first_served: false,
                served_again: 0,
                repaid: Duration::from_secs(60),
            },
        ];
        for row in cases {
            let case = row.case;
            let limits = Limits {
                rate: row.rate,
                burst: row.burst,
            };
            let mut throttles = RouteThrottles::default();
            // Spend what the row says the requests already served took out of the bucket.
            for taken in 1..=row.spent {
                assert!(
                    sequential(&mut throttles, "GET /", limits, start),
                    "case {case:?}: expected request {taken} of the bucket's capacity served"
                );
            }
            // They all arrive before any of them is answered, which is what makes them
            // requests that arrived together.
            let admitted: Vec<bool> = (0..row.submissions)
                .map(|_| throttles.arrived("api", "$default", "GET /", limits, start))
                .collect();
            let admitted_count = admitted.iter().filter(|admitted| **admitted).count();
            assert_eq!(
                admitted_count, row.served,
                "case {case:?}: expected {} of {} submissions served, got {admitted_count} \
                 ({admitted:?})",
                row.served, row.submissions
            );
            // The first of them arrived while the route was answering nothing, so the bucket's
            // own capacity is what answered it; the rest drew on the burst while it left room.
            let first = usize::from(!row.first_served);
            let expected_each: Vec<bool> = (0..row.submissions)
                .map(|position| position >= first && position < first + row.served)
                .collect();
            assert_eq!(
                admitted, expected_each,
                "case {case:?}: expected the submissions served as {expected_each:?}, got \
                 {admitted:?}"
            );
            for admitted in &admitted {
                throttles.answered("api", "$default", "GET /", *admitted);
            }
            // What they were served is charged, so a second set arriving at once is served
            // only what the burst's draw still allows: the burst is a draw the rate repays,
            // not one the route gets again per set.
            let again: Vec<bool> = (0..row.submissions)
                .map(|_| throttles.arrived("api", "$default", "GET /", limits, start))
                .collect();
            let again_count = again.iter().filter(|admitted| **admitted).count();
            assert_eq!(
                again_count, row.served_again,
                "case {case:?}: expected {} served of the submissions arriving while the draw \
                 is unrepaid, got {again_count} ({again:?})",
                row.served_again
            );
            for admitted in &again {
                throttles.answered("api", "$default", "GET /", *admitted);
            }
            // Once the rate has repaid the draw the route serves again — and a rate that
            // refills nothing never repays it, so such a route stays shut.
            let after = sequential(&mut throttles, "GET /", limits, start + row.repaid);
            assert_eq!(
                after,
                row.rate > 0.0,
                "case {case:?}: expected the route served again only where its rate refills \
                 the bucket at all, got {after}"
            );
        }
    }

    /// One row of the continuous-load table: the pair the settings state, how many requests
    /// arrive and how far apart, with the route never left idle.
    struct UnderLoad {
        case: &'static str,
        rate: f64,
        burst: f64,
        /// How many requests arrive, one every [`Self::every`], each answered only after the
        /// next has arrived — so the route always has a request in flight.
        arrivals: usize,
        every: Duration,
    }

    /// A route whose in-flight set is NEVER empty is still served at its rate: the burst's room
    /// comes back as each request is answered and the bucket's capacity as the rate refills it,
    /// so load that never lets up is throttled rather than shut out.
    #[test]
    fn a_route_under_load_that_never_lets_up_keeps_being_served_at_its_rate() {
        let start = Instant::now();
        // Rows differ along the rate and burst the settings state and along how hard the load
        // presses: ten times the rate, and a hundred times it.
        let cases = [
            UnderLoad {
                case: "ten requests a second, burst two, met by ten times that",
                rate: 10.0,
                burst: 2.0,
                arrivals: 100,
                every: Duration::from_millis(10),
            },
            UnderLoad {
                case: "one request a second, burst one, met by a hundred times that",
                rate: 1.0,
                burst: 1.0,
                arrivals: 100,
                every: Duration::from_millis(10),
            },
            UnderLoad {
                case: "half a request a second, burst three, met by twenty times that",
                rate: 0.5,
                burst: 3.0,
                arrivals: 100,
                every: Duration::from_millis(100),
            },
        ];
        for row in cases {
            let case = row.case;
            let limits = Limits {
                rate: row.rate,
                burst: row.burst,
            };
            let mut throttles = RouteThrottles::default();
            // Each request arrives before the one before it is answered, so the route is never
            // idle for the whole span: there is always a request in flight.
            let mut previous: Option<bool> = None;
            let mut served = 0usize;
            for arrival in 0..row.arrivals {
                let now = start + row.every * u32::try_from(arrival).unwrap_or(u32::MAX);
                let admitted = throttles.arrived("api", "$default", "GET /", limits, now);
                if admitted {
                    served += 1;
                }
                if let Some(answered) = previous.replace(admitted) {
                    throttles.answered("api", "$default", "GET /", answered);
                }
            }
            if let Some(answered) = previous {
                throttles.answered("api", "$default", "GET /", answered);
            }
            // Over the span the load lasted, the route served what its rate refilled — the
            // whole span's worth, give or take the burst it may hold at once and the burst it
            // may draw ahead — and not the nothing a closed gate would serve.
            let span = row.every.as_secs_f64() * (row.arrivals - 1) as f64;
            let rate_worth = row.rate * span;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let lower = rate_worth.floor() as usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let upper = (rate_worth + 2.0 * row.burst).ceil() as usize;
            assert!(
                served >= lower && served <= upper,
                "case {case:?}: expected the route served between {lower} and {upper} of its \
                 {} requests (the rate's worth of the span, give or take the burst), got \
                 {served}",
                row.arrivals
            );
        }
    }
}
