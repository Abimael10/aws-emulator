// SPDX-License-Identifier: CC0-1.0

//! The RESOLUTION every instant this service records is reported at.
//!
//! Every instant here is read off the host clock, which ticks at whatever resolution the
//! platform offers — microseconds or nanoseconds. The timestamps this API serves are plain
//! `Timestamp` members with no `@timestampFormat` of their own, so they travel in the
//! protocol's default form (an epoch-seconds NUMBER for `awsJson1_0`, and the corresponding
//! epoch value for `rpcv2Cbor`), and that form carries no finer resolution than a
//! MILLISECOND: an instant with no sub-second part is a bare integer, and one with a
//! sub-second part carries at most three fractional digits.
//!
//! So the reading a clock offers and the timestamp a member reports are not the same
//! resolution, and the one conversion between them lives here rather than in each record's
//! projection — a handler that records an instant takes it through [`reported_instant`] and
//! stores the result, so every read of that record reports the same value.

use super::*;

/// `at` as the modelled timestamp this service records and reports it as: epoch seconds at
/// millisecond resolution.
///
/// Truncation, never rounding: the millisecond an instant falls in is the one it is
/// reported in, so a reported timestamp never names an instant that has not happened yet
/// and the order of two instants is never reversed by reporting them.
///
/// Converting an already-reported value again changes nothing, so recording a timestamp
/// derived from one is stable too. An instant before the epoch, which only a clock set
/// before 1970 could produce, is reported as the epoch rather than panicking.
#[must_use]
pub fn reported_instant(at: SystemTime) -> DateTime {
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_millis()).ok())
        .unwrap_or_default();
    DateTime::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reported instant carries at most millisecond resolution, and the
    /// milliseconds it carries are the instant's own.
    #[test]
    fn a_reported_instant_carries_milliseconds() {
        // The table spans what the rule generalises over: the epoch itself, a whole
        // second, a whole millisecond, and the sub-millisecond ticks a host clock offers
        // but no timestamp of this wire carries.
        let cases = [
            ("the epoch", Duration::from_secs(0), 0, 0),
            (
                "a whole second",
                Duration::from_secs(1_700_000_000),
                1_700_000_000,
                0,
            ),
            (
                "a whole millisecond",
                Duration::from_millis(1_700_000_000_123),
                1_700_000_000,
                123_000_000,
            ),
            (
                "a microsecond past a millisecond",
                Duration::from_micros(1_700_000_000_123_456),
                1_700_000_000,
                123_000_000,
            ),
            (
                "a nanosecond past a millisecond",
                Duration::from_nanos(1_700_000_000_123_456_789),
                1_700_000_000,
                123_000_000,
            ),
        ];
        for (case, since_epoch, expected_secs, expected_nanos) in cases {
            let reported = reported_instant(SystemTime::UNIX_EPOCH + since_epoch);
            assert_eq!(
                reported.secs(),
                expected_secs,
                "case {case:?}: expected {expected_secs} epoch seconds, got {reported:?}"
            );
            assert_eq!(
                reported.subsec_nanos(),
                expected_nanos,
                "case {case:?}: expected {expected_nanos} subsecond nanoseconds, got {reported:?}"
            );
        }
    }

    /// RULE: reporting a reported instant again changes nothing, so a timestamp derived
    /// from one already recorded keeps its value.
    #[test]
    fn reporting_a_reported_instant_is_stable() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_987_654_321);
        let once = reported_instant(at);
        let twice =
            reported_instant(SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_987));
        assert_eq!(
            (twice.secs(), twice.subsec_nanos()),
            (once.secs(), once.subsec_nanos()),
            "expected a reported instant to be stable under a second reporting, got {twice:?} \
             instead of {once:?}"
        );
    }
}
