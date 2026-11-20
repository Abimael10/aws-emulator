// SPDX-License-Identifier: CC0-1.0

//! The ATTEMPTS a rule's target invocation made, as the counter a client reads
//! them off.
//!
//! "InvocationAttempts: the number of times EventBridge attempted invoking a
//! target" (EventBridge's published metrics) — the attempt series' total, of
//! which `SuccessfulInvocationAttempts` is the part the target accepted. So an
//! attempt counts here whatever it came to: one the target took, and one it
//! refused, each count once, and a delivery that was RETRIED counts one per
//! attempt it made rather than one per delivery. It is what tells a client that
//! an event was handed over at all — a delivery accepted with no retry and
//! nothing dead-lettered is observable as exactly one attempt here, where the
//! failure counters hold no series at all.
//!
//! A handover whose invocation was never attempted counts nothing: the metric is
//! about the deliveries EventBridge MADE, so a target this emulator carries no
//! invocation for is a gap rather than an attempt. A delivery it decided without
//! reaching the target's API — a role it could not assume, a document that bars
//! the call, an input no target may be handed — IS one attempt: EventBridge made
//! that delivery and reported what it came to, which is the same handover its
//! `FailedInvocations` datapoint is about.

use super::{count_handover, retry_bounds, CloudWatchState};

/// The EventBridge metric counting the target invocations a rule ATTEMPTED,
/// whichever way each attempt went.
pub const INVOCATION_ATTEMPTS_METRIC: &str = "InvocationAttempts";

/// Count the `attempts` one handover's target invocation made, on the rule
/// `rule` of the bus `bus`.
///
/// One datapoint of 1 per attempt, as the counter is published: a count of
/// attempts is what a reader sums, so a retried delivery that made three calls
/// is three occurrences in the series and not one of value three.
///
/// `attempts` is held to what one delivery can really make — its first attempt
/// plus the retries of the pacing ([`retry_bounds::MAX_RETRIES_PER_DELIVERY`]) —
/// so this is the one place that range is stated and a caller's arithmetic cannot
/// turn one handover into an unbounded number of writes. A count past it is said
/// once in the log, the way every other bound that drops counted work here is: a
/// miscount nobody can see reads to a client as a delivery that made fewer
/// attempts than it did.
pub fn count_invocation_attempts(
    cloudwatch: &mut CloudWatchState,
    bus: &str,
    rule: &str,
    attempts: i32,
) {
    let counted = attempts.clamp(0, retry_bounds::MAX_RETRIES_PER_DELIVERY + 1);
    if counted != attempts {
        ::tracing::warn!(
            %bus,
            %rule,
            reported = attempts,
            bound = retry_bounds::MAX_RETRIES_PER_DELIVERY + 1,
            "an invocation reported more attempts than one delivery makes; the attempts past \
             the bound are not counted"
        );
    }
    for _ in 0..counted {
        count_handover(cloudwatch, INVOCATION_ATTEMPTS_METRIC, bus, rule);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bus and rule the counts below belong to.
    const BUS: &str = "orders-bus";
    const RULE: &str = "attempting";

    /// Counting the attempts of ONE handover appends one occurrence per attempt
    /// to EVERY series the counter is published under, for any count one
    /// delivery can make.
    ///
    /// The fact that does not depend on what the attempts came to, which is why
    /// it is asserted here: an invocation accepted on its last attempt counts all
    /// of them, exactly as one that never delivered does, because the attempts
    /// are reported as they are MADE.
    #[test]
    fn every_attempt_is_one_occurrence_in_each_series_the_counter_is_published_under() {
        // The counts a delivery reports: none, one, a retried few, the most the
        // pacing makes, and one past that bound.
        let rows = [
            0,
            1,
            4,
            retry_bounds::MAX_RETRIES_PER_DELIVERY + 1,
            retry_bounds::MAX_RETRIES_PER_DELIVERY + 9,
        ];
        for attempts in rows {
            let mut cloudwatch = CloudWatchState::default();
            count_invocation_attempts(&mut cloudwatch, BUS, RULE, attempts);
            // Past the bound, what is counted is the bound: the attempts a
            // delivery can really make.
            let expected = attempts.clamp(0, retry_bounds::MAX_RETRIES_PER_DELIVERY + 1);
            let series: Vec<(usize, usize)> = cloudwatch
                .metrics
                .iter()
                .filter(|(key, _)| key.metric_name == INVOCATION_ATTEMPTS_METRIC)
                .map(|(key, data)| (key.dimensions.len(), data.len()))
                .collect();
            if expected == 0 {
                assert!(
                    series.is_empty(),
                    "case {attempts} attempt(s): expected no series at all, got {series:?}"
                );
                continue;
            }
            let counted = usize::try_from(expected).expect("the bound is positive");
            // The two dimension sets a handover counter is published under: the
            // rule's whole identity, and the rule name on its own.
            let mut held = series;
            held.sort_unstable();
            assert_eq!(
                held,
                vec![(1, counted), (2, counted)],
                "case {attempts} attempt(s): expected {counted} occurrence(s) in each of the \
                 two series the counter is published under, got {held:?}"
            );
            for (key, data) in &cloudwatch.metrics {
                assert_eq!(
                    key.namespace, super::super::METRIC_NAMESPACE,
                    "case {attempts} attempt(s): expected the series in EventBridge's own \
                     namespace, got {key:?}"
                );
                let summed: f64 = data.iter().map(|datum| datum.sum).sum();
                let occurrences: f64 = data.iter().map(|datum| datum.sample_count).sum();
                assert_eq!(
                    (summed, occurrences),
                    (f64::from(expected), f64::from(expected)),
                    "case {attempts} attempt(s): expected each attempt counted once, \
                     got {data:?}"
                );
            }
        }
    }
}
