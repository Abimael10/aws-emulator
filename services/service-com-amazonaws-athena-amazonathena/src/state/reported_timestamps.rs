// SPDX-License-Identifier: CC0-1.0

//! The resolution the instants in this service's responses are reported at.
//!
//! Every instant this service records is read off the host clock, which ticks at whatever
//! resolution the platform offers — microseconds or nanoseconds. Athena's own timestamps
//! carry MILLISECONDS: every `Date` member of this model is an epoch-seconds number on the
//! awsJson1_1 wire, and the service serves no finer resolution than a millisecond in one. So
//! the instant a record holds and the member it is reported as are not the same resolution,
//! and the conversion between them lives here rather than in each record's projection.

use std::time::SystemTime;

/// `at` as the modelled timestamp a response reports it as: epoch seconds at millisecond
/// resolution.
///
/// The stored instant is unchanged — only the resolution it is REPORTED at is decided here —
/// so a record written once reads back with the same value on every read, and two records
/// written at the same instant report the same timestamp. Every projection of an instant this
/// service read off the clock goes through this, so no response reports one resolution while
/// another reports a different one.
///
/// An instant before the epoch, which only a clock set before 1970 could produce, is reported
/// as the epoch rather than panicking.
#[must_use]
pub fn reported_time(at: SystemTime) -> ::aws_smithy_types::DateTime {
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_millis()).ok())
        .unwrap_or_default();
    ::aws_smithy_types::DateTime::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// RULE: a reported instant carries at most millisecond resolution, and the milliseconds
    /// it carries are the instant's own.
    #[test]
    fn a_reported_instant_carries_milliseconds() {
        // The table spans what the rule generalises over: the epoch itself, a whole second, a
        // whole millisecond, and the sub-millisecond ticks a host clock offers but no Athena
        // timestamp carries.
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
            let reported = reported_time(SystemTime::UNIX_EPOCH + since_epoch);
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

    /// RULE: the resolution is decided per READ, so two reads of one instant report one value
    /// and an instant is never carried forward at a finer resolution than it is served at.
    #[test]
    fn one_instant_is_reported_as_one_value() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_987_654_321);
        let first = reported_time(at);
        let second = reported_time(at);
        assert_eq!(
            (first.secs(), first.subsec_nanos()),
            (second.secs(), second.subsec_nanos()),
            "expected two reads of one instant to report one value, got {first:?} then \
             {second:?}"
        );
        // Reporting the reported value again changes nothing: it is already at the
        // resolution the wire carries.
        let again = reported_time(SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_987));
        assert_eq!(
            (again.secs(), again.subsec_nanos()),
            (first.secs(), first.subsec_nanos()),
            "expected the reported instant to be stable under a second reporting, got \
             {again:?} instead of {first:?}"
        );
    }
}
