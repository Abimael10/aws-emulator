// SPDX-License-Identifier: CC0-1.0

//! The REQUEST RATE one account's calls to an Athena API are admitted at, and what Athena
//! answers the calls past it with.
//!
//! Athena serves the calls of a throttled API from a TOKEN BUCKET, and publishes that
//! bucket as a pair: the bucket's replenishment rate, per second, and the burst multiplier
//! on it — so the capacity a caller may spend at once is the rate times the multiplier, and
//! the rate is what governs afterwards. A call the bucket does not admit is not served
//! slowly, it is REFUSED with the throttling error the operation models, and a client's
//! retry and backoff path exists for exactly that answer. The rate is therefore part of the
//! operation's contract rather than of its capacity.
//!
//! Three pieces:
//!
//! 1. **which operations are metered, and at what pair** — [`METERED_CALL_RATES`], with the
//!    source of each entry's pair. An operation absent from it is not metered here, because
//!    nothing states or records a rate to meter it at; a rate is never invented for one.
//! 2. **what each account has left** — [`RequestRates`], one
//!    [`::service_common::token_bucket::Bucket`] per metered operation and Region, which is
//!    the tree's one reading of such a pair. Nothing here sleeps: the capacity is computed
//!    from the time since the last admitted call, so a caller that paused finds its budget
//!    again.
//! 3. **the refusal** — [`THROTTLE_MESSAGE`], the sentence a throttled call states. The
//!    refused call performs nothing, so it leaves every record of the account exactly as it
//!    was: nothing is created, nothing is consumed, and a later call inside the quota reads
//!    back what it read before.

use std::collections::HashMap;
use std::time::Instant;

use ::service_common::token_bucket::{Bucket, Limits};

/// The sentence a call refused for its RATE states.
///
/// AWS's throttling wording for this class, and fixed text end to end: the quota the call
/// exceeded is published rather than stated per call, so the sentence carries no name, id or
/// value from the request and reads the same whichever operation was metered.
pub const THROTTLE_MESSAGE: &str = "Rate exceeded";

/// The pair Athena serves one API's calls under: the per-second replenishment rate of the
/// token bucket, and the burst multiplier on it.
#[derive(Debug, Clone, Copy)]
pub struct CallRate {
    /// The operation's own name ([`smithy_server_runtime::Operation::NAME`]), which is what
    /// a call rate is published per.
    pub operation: &'static str,
    /// The bucket's replenishment rate, in calls per second.
    pub rate: f64,
    /// The multiplier on that rate the burst capacity is: the most calls admissible at once.
    pub burst_multiplier: f64,
}

/// The replenishment rate and burst multiplier each metered Athena API's calls are served
/// under, per account and Region. Each entry states where its pair comes from.
///
/// `CreateNotebook`: a notebook create past the rate is refused with the modelled
/// `TooManyRequestsException`, which recorded behaviour shows — a sustained concurrent burst
/// of creates against one Spark enabled workgroup answers some callers with that throttle
/// while others are served. That recording establishes the refusal but not the figures, and
/// Athena's quota catalog publishes no row for this API, so the pair here is a STAND-IN taken
/// from the create API that catalog does publish a pair for — `CreateNamedQuery`, at five
/// calls per second with a burst multiplier of four, the class an Athena create is served in.
/// Should a pair be published for this API, it replaces these figures.
///
/// Every other operation of this service is deliberately absent. Metering one at figures
/// nothing publishes for it and no recording shows it throttled at would refuse a call AWS
/// serves, which is the worse answer; an entry is added only for an operation shown to be
/// metered, and with the pair whose source it names.
pub const METERED_CALL_RATES: &[CallRate] = &[CallRate {
    operation: "CreateNotebook",
    rate: 5.0,
    burst_multiplier: 4.0,
}];

/// The most buckets one account's record keeps at once.
///
/// A bucket is keyed by the Region the request addressed, which the caller chooses, so the
/// map is bounded like every other store of this service: at the ceiling, the buckets that
/// have refilled to their whole capacity are dropped first, which changes no decision — a
/// caller with its whole burst left is in exactly the position a caller with no bucket is.
const MAX_METERED_BUDGETS: usize = 256;

/// The limits `operation`'s calls are admitted under, or `None` when it is not metered —
/// which is every operation [`METERED_CALL_RATES`] does not name.
///
/// The burst is the rate times the published multiplier: that is the capacity a caller may
/// spend at once, after which the rate is what governs.
#[must_use]
pub fn metered_limits(operation: &str) -> Option<Limits> {
    METERED_CALL_RATES
        .iter()
        .find(|metered| metered.operation == operation)
        .map(|metered| Limits {
            rate: metered.rate,
            burst: metered.rate * metered.burst_multiplier,
        })
}

/// What each metered operation has left to spend, per Region — the account's own request
/// budgets.
#[derive(Debug, Default)]
pub struct RequestRates {
    /// One budget per (Region, operation) a metered call has arrived for. At most
    /// [`MAX_METERED_BUDGETS`].
    budgets: HashMap<(String, String), Bucket>,
}

impl RequestRates {
    /// Admit one call to `operation` in `region` at `now` and spend its capacity, or report
    /// that the published pair does not admit it.
    ///
    /// `limits` are that operation's own ([`metered_limits`]), passed in so the budget is
    /// metered at the pair in force on the call rather than at one frozen when the budget
    /// was first taken.
    pub fn admits(
        &mut self,
        operation: &str,
        region: &str,
        limits: Limits,
        now: Instant,
    ) -> bool {
        let key = (region.to_string(), operation.to_string());
        if let Some(bucket) = self.budgets.get_mut(&key) {
            return bucket.admits(limits, now);
        }
        if self.budgets.len() >= MAX_METERED_BUDGETS {
            // Only the budgets still being spent are worth keeping; a full one decides
            // nothing a fresh one would not.
            self.budgets
                .retain(|_, bucket| bucket.capacity_at(limits, now) < limits.burst);
        }
        let mut bucket = Bucket::full(limits, now);
        let admitted = bucket.admits(limits, now);
        if self.budgets.len() < MAX_METERED_BUDGETS {
            self.budgets.insert(key, bucket);
        }
        admitted
    }
}

/// Whether the call to `operation` signed for `region` is past this account's rate for it,
/// and so refused with the operation's throttling error.
///
/// `false` for every operation no rate is metered for, and for an engine carrying no store
/// of this service: a budget that cannot be read refuses nothing, because refusing a call
/// AWS serves is the worse answer.
///
/// Call it with this service's own store NOT locked: the store is taken here.
#[must_use]
pub fn is_throttled<Eng: Send + Sync + 'static>(
    engine: &Eng,
    operation: &str,
    region: &str,
) -> bool {
    let Some(limits) = metered_limits(operation) else {
        return false;
    };
    let Some(state) = super::service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    !store
        .request_rates
        .admits(operation, region, limits, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// RULE: a metered operation admits its whole burst — the published rate times the
    /// published multiplier — at once, then what the rate refilled, per Region and per
    /// operation.
    #[test]
    fn a_metered_rate_admits_its_burst_and_then_refills() {
        let start = Instant::now();
        let mut rates = RequestRates::default();
        let operation = "CreateNotebook";
        let limits = metered_limits(operation).expect("the operation is metered");

        // How many calls arriving at one instant are admitted: the burst, and the next one
        // is refused.
        let mut admitted: u32 = 0;
        for _ in 0..1_000 {
            if !rates.admits(operation, "us-east-1", limits, start) {
                break;
            }
            admitted += 1;
        }
        assert_eq!(
            f64::from(admitted),
            limits.burst,
            "case \"calls arriving at one instant\": expected the whole burst {}, got \
             {admitted}",
            limits.burst
        );
        // Another Region is another budget: the rate is served per Region.
        assert!(
            rates.admits(operation, "eu-west-1", limits, start),
            "case \"the first call in another Region\": expected it admitted"
        );
        // And the rate refills: one call's capacity returns after one call's worth of time.
        let refilled = start + Duration::from_secs_f64(1.0 / limits.rate);
        assert!(
            rates.admits(operation, "us-east-1", limits, refilled),
            "case \"a call once the rate refilled one\": expected it admitted"
        );
        // However long the caller then pauses, the capacity is the burst and no more.
        let idle = refilled + Duration::from_secs(3_600);
        let mut after_idle: u32 = 0;
        while rates.admits(operation, "us-east-1", limits, idle) {
            after_idle += 1;
            assert!(
                f64::from(after_idle) <= limits.burst,
                "case \"calls after an idle hour\": the bucket admitted more than its burst"
            );
        }
        assert_eq!(
            f64::from(after_idle),
            limits.burst,
            "case \"calls after an idle hour\": expected the burst again, got {after_idle}"
        );
    }

    /// RULE: only the operations a rate is published or recorded for are metered at all, and
    /// each metered pair admits something.
    #[test]
    fn an_operation_with_no_metered_rate_is_not_metered() {
        for operation in [
            "ExportNotebook",
            "GetNotebookMetadata",
            "ListNotebookMetadata",
            "StartQueryExecution",
        ] {
            let limits = metered_limits(operation);
            assert!(
                limits.is_none(),
                "case {operation:?}: expected no metered rate, got {limits:?}"
            );
        }
        for metered in METERED_CALL_RATES {
            let limits = metered_limits(metered.operation);
            assert!(
                limits.is_some_and(|limits| limits.rate > 0.0 && limits.burst >= limits.rate),
                "case {:?}: a metered pair must admit at least one second of its rate, got \
                 {limits:?}",
                metered.operation
            );
        }
    }
}
