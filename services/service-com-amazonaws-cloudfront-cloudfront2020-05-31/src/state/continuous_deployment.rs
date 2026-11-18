// SPDX-License-Identifier: CC0-1.0

//! How a continuous deployment policy SPLITS production traffic by WEIGHT: which of the
//! viewer requests a `SingleWeight` traffic configuration covers are routed to the staging
//! distribution, and which viewer sessions its session stickiness holds to the side their
//! first request was routed to.
//!
//! A `SingleHeader` configuration needs none of this — "if the HTTP request contains a header
//! and value that matches what you specify here, the request is sent to the staging
//! distribution", which is read off the request itself — so that decision lives with the
//! plane that reads the request. A WEIGHT is a share of a SEQUENCE of requests, so what the
//! policy has already routed is part of the policy's own record, exactly as the share of
//! requests a real-time log configuration represents is part of its own
//! ([`super::StoredRealtimeLogConfig::represents_next_request`]).

use std::time::{Duration, SystemTime};

use crate::ops::types::SessionStickinessConfig;

use super::StoredContinuousDeploymentPolicy;

/// The most viewer SESSIONS one policy's session stickiness holds open at a time.
///
/// A bound of this EMULATOR's, not a CloudFront one: a session is opened for a viewer
/// address this policy has not seen, so without a ceiling the record would grow with the
/// number of distinct viewers. Sessions that have ended are dropped before this is consulted,
/// so it bounds the LIVE ones alone, and a viewer arriving past it is routed by the weight
/// itself rather than held to a side.
const MAX_STICKY_SESSIONS: usize = 1_000;

/// One viewer session a policy's session stickiness holds open: the side it was routed to and
/// the two instants its TTLs are measured from.
///
/// "Session stickiness provides the ability to define multiple requests from a single viewer
/// as a single session. This prevents the potentially inconsistent experience of sending some
/// of a given user's requests to your staging distribution, while others are sent to your
/// primary distribution" — so the side is decided once, when the session opens, and every
/// later request of that session follows it.
#[derive(Debug, Clone)]
pub struct StickySession {
    /// Whether this session's requests are served by the STAGING distribution.
    pub staged: bool,
    /// When this session's FIRST request was routed, which `MaximumTTL` — "the maximum amount
    /// of time to consider requests from the viewer as being part of the same session" — is
    /// measured from.
    pub opened: SystemTime,
    /// When this session's LAST request was routed, which `IdleTTL` — "the amount of time
    /// after which you want sessions to cease if no requests are received" — is measured
    /// from.
    pub last_request: SystemTime,
}

impl StickySession {
    /// Whether this session is still open at `now`, under the TTLs its policy names.
    fn is_open(&self, now: SystemTime, idle: Duration, maximum: Duration) -> bool {
        let since_last = now.duration_since(self.last_request).unwrap_or_default();
        let since_opened = now.duration_since(self.opened).unwrap_or_default();
        since_last <= idle && since_opened <= maximum
    }
}

/// The TTL `seconds` names, as a duration; a negative value names none.
fn ttl(seconds: i32) -> Duration {
    Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

impl StoredContinuousDeploymentPolicy {
    /// Whether the NEXT viewer request this policy's `SingleWeight` configuration covers is
    /// routed to the staging distribution, advancing the policy's own bookkeeping.
    ///
    /// `Weight` is "the percentage of traffic to send to a staging distribution, expressed as
    /// a decimal number between 0 and 0.15. For example, a value of 0.10 means 10% of traffic
    /// is sent to the staging distribution", so that share of the requests the policy covers
    /// goes to staging and the rest to the primary ([`Self::takes_next_share`] is what makes
    /// the long-run share the weight itself).
    ///
    /// Where the configuration names SESSION STICKINESS, the share is drawn once per SESSION
    /// rather than once per request: a viewer whose session is still open is served by the
    /// side that session opened on, which is what "prevents the potentially inconsistent
    /// experience of sending some of a given user's requests to your staging distribution,
    /// while others are sent to your primary distribution".
    pub fn routes_weighted_request(
        &mut self,
        weight: f32,
        stickiness: Option<&SessionStickinessConfig>,
        viewer: &str,
        now: SystemTime,
    ) -> bool {
        let Some(stickiness) = stickiness else {
            return self.takes_next_share(weight);
        };
        let (idle, maximum) = (ttl(stickiness.idle_ttl), ttl(stickiness.maximum_ttl));
        // A session that has ceased is no session: dropped before this request is placed, so
        // a viewer returning after its session ended opens a new one and the live sessions
        // are what [`MAX_STICKY_SESSIONS`] bounds.
        self.sticky_sessions
            .retain(|_, session| session.is_open(now, idle, maximum));
        if let Some(session) = self.sticky_sessions.get_mut(viewer) {
            session.last_request = now;
            return session.staged;
        }
        let staged = self.takes_next_share(weight);
        if self.sticky_sessions.len() < MAX_STICKY_SESSIONS {
            self.sticky_sessions.insert(
                viewer.to_string(),
                StickySession {
                    staged,
                    opened: now,
                    last_request: now,
                },
            );
        }
        staged
    }

    /// Whether the next request the policy covers takes the staging distribution's share of
    /// it, advancing the policy's own traffic cursor.
    ///
    /// Each covered request adds the weight to the cursor, and the request is routed to
    /// staging — spending a whole request's worth — as soon as the cursor holds one. What is
    /// left over CARRIES to the next request, which is what makes the long-run share the
    /// weight itself at every value of it: 0.15 sends three of every twenty requests, 0.05 one
    /// of every twenty, and 0 none. No randomness, and the cursor stays below one whole
    /// request plus the weight, which the create and update paths hold to
    /// [`super::MAX_TRAFFIC_WEIGHT`].
    ///
    /// The cursor counts in [`WHOLE_REQUEST_SHARE`]ths of a request rather than in the
    /// weight's own binary fraction, so adding the share up is exact and the share a weight
    /// names over a run of requests is the one it names however long the run is.
    fn takes_next_share(&mut self, weight: f32) -> bool {
        self.traffic_cursor = self.traffic_cursor.saturating_add(share_of_a_request(weight));
        if self.traffic_cursor < WHOLE_REQUEST_SHARE {
            return false;
        }
        self.traffic_cursor -= WHOLE_REQUEST_SHARE;
        true
    }
}

/// One whole request, in the units the traffic cursor counts in.
///
/// `Weight` is "a decimal number between 0 and 0.15", so a hundredth of a percent is the
/// finest share one can name and four decimal places hold every such value exactly.
const WHOLE_REQUEST_SHARE: i64 = 10_000;

/// [`WHOLE_REQUEST_SHARE`] as the real number the weight is multiplied by.
const WHOLE_REQUEST_SHARE_AS_REAL: f64 = 10_000.0;

/// The share of ONE request a weight names, in [`WHOLE_REQUEST_SHARE`]ths.
///
/// A value outside "between 0 and 0.15" is held to the closed interval the cursor can spend —
/// nothing at all, or the whole request — which the create and update paths have already
/// refused a configuration outside of.
fn share_of_a_request(weight: f32) -> i64 {
    (f64::from(weight) * WHOLE_REQUEST_SHARE_AS_REAL)
        .round()
        .clamp(0.0, WHOLE_REQUEST_SHARE_AS_REAL) as i64
}
