// SPDX-License-Identifier: CC0-1.0

//! The RESOLUTION every instant this service reports is reported at.
//!
//! Each instant this store keeps is read off the host clock, which ticks at whatever
//! resolution the platform offers — microseconds or nanoseconds. The timestamps this API
//! serves carry MILLISECONDS, so a record's instant and the member it is reported as are
//! not the same resolution, and the conversion between the two lives here rather than at
//! each of the places a clock reading is turned into a stored timestamp.
//!
//! One spelling for all of them, so no response of this service reports an instant at one
//! resolution while another reports the same kind of instant at a finer one.

use super::*;

/// `at` as the modelled timestamp a response reports it as: epoch seconds carrying at most
/// milliseconds.
///
/// The INSTANT is unchanged — only the resolution it is reported at is decided here — so a
/// record written once reads back with the same value on every read, and two records
/// written at the same instant report the same timestamp. An instant before the epoch,
/// which only a clock set before 1970 could produce, is reported as the epoch rather than
/// panicking.
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
        // The rows differ along what the rule generalises over: how far past the epoch the
        // instant is, and how fine the tick it carries below a second is.
        let rows = [
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
        for (case, since_epoch, expected_secs, expected_nanos) in rows {
            let reported = reported_instant(SystemTime::UNIX_EPOCH + since_epoch);
            assert_eq!(
                reported.secs(),
                expected_secs,
                "case {case:?}: expected {expected_secs} epoch seconds, got {reported:?}"
            );
            assert_eq!(
                reported.subsec_nanos(),
                expected_nanos,
                "case {case:?}: expected {expected_nanos} subsecond nanoseconds, got \
                 {reported:?}"
            );
        }
    }
}
