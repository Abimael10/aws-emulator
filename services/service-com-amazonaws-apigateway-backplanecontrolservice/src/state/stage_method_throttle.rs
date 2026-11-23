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
//! The pair is read as the same token bucket a usage plan's throttle is read as — the
//! algorithm the account-level limits are published as ([`super::ThrottleLimits`]) — so the
//! two limits differ in what they are keyed by and in nothing else.

use std::collections::BTreeMap;

use crate::ops::types::MethodSetting;

use super::{
    accepts_method_throttle_burst, accepts_method_throttle_rate,
    ComAmazonawsApigatewayBackplanecontrolserviceState as State, ThrottleBucket, ThrottleLimits,
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

impl ThrottleLimits {
    /// The limits the `methodSettings` entry `setting` declares, or `None` when it declares
    /// none that limits anything.
    ///
    /// Either member may carry the sentinel that asks for no throttle at all
    /// ([`super::UNTHROTTLED_LIMIT`], which [`accepts_method_throttle_burst`] and
    /// [`accepts_method_throttle_rate`] admit beside the counts), and a limit that is not
    /// POSITIVE limits nothing — so a method whose settings leave either member unset, or set
    /// either to the sentinel, admits every request.
    #[must_use]
    pub fn of_method_setting(setting: &MethodSetting) -> Option<Self> {
        let burst = setting.throttling_burst_limit;
        let rate = setting.throttling_rate_limit;
        if !accepts_method_throttle_burst(burst) || !accepts_method_throttle_rate(rate) {
            return None;
        }
        let limits = Self {
            burst: f64::from(burst),
            rate,
        };
        (limits.burst > 0.0 && limits.rate.is_finite() && limits.rate > 0.0).then_some(limits)
    }
}

impl State {
    /// Whether the per-method throttle `key` names admits one more request at `now`,
    /// RECORDING the request it admits.
    ///
    /// This is the one place a stage's own `methodSettings` throttle is accounted, so every
    /// request the stage serves through that entry draws on one bucket; a bucket starts FULL,
    /// so a method nothing has called yet admits a burst up to its limit, and the limits a
    /// later `UpdateStage` writes take effect on the next request rather than on a bucket
    /// frozen at the old ones.
    ///
    /// Bounded by the records the control plane holds — one bucket per
    /// `(API, stage, method path)` — and the entries whose API or stage no longer exists are
    /// forgotten when a bucket is ADDED, which is the only step that grows the map: a request
    /// a bucket already exists for touches that bucket alone, so the hot path stays the one
    /// lookup, and the map never grows with the request count nor with the stages an account
    /// has created and deleted.
    pub fn stage_method_throttle_admits(
        &mut self,
        key: &StageMethodThrottleKey,
        limits: ThrottleLimits,
        now: ::std::time::Instant,
    ) -> bool {
        if !self.stage_method_throttle_buckets.contains_key(key) {
            // Taken out and put back because the retain reads the api and stage records
            // beside it.
            let mut buckets = ::std::mem::take(&mut self.stage_method_throttle_buckets);
            buckets.retain(|(rest_api_id, stage_name, _), _| {
                self.rest_apis
                    .get(rest_api_id)
                    .is_some_and(|api| api.stages.contains_key(stage_name))
            });
            buckets.insert(key.clone(), ThrottleBucket::full(limits, now));
            self.stage_method_throttle_buckets = buckets;
        }
        self.stage_method_throttle_buckets
            .get_mut(key)
            .is_some_and(|bucket| bucket.admits(limits, now))
    }
}

/// The buckets as the state field holds them; see [`StageMethodThrottleKey`].
pub type StageMethodThrottleBuckets = BTreeMap<StageMethodThrottleKey, ThrottleBucket>;
