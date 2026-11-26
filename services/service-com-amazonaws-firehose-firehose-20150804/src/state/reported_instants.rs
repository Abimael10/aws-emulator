// SPDX-License-Identifier: CC0-1.0

//! The resolution the instants this service REPORTS are stated at.
//!
//! Every `Timestamp` member of this service's model targets a plain
//! `com.amazonaws.firehose#Timestamp` with no `@timestampFormat`
//! (`smithy_query(query="shape", shape="Timestamp")`), so on the `awsJson1_1` wire each one
//! travels as an epoch-SECONDS number. Firehose reports such an instant at MILLISECOND
//! resolution — three fractional digits at most — while the host clock this emulator reads
//! an instant from carries nanoseconds, which would be written out as a number with digits
//! no AWS response has.
//!
//! So an instant is reported through [`reported`], which states it from whole epoch
//! milliseconds. It is the one conversion from a stored instant to a reported one, so two
//! operations reporting the same stored instant cannot state it at two resolutions.

use super::*;

/// The modelled timestamp for the stored instant `at`, at the millisecond resolution this
/// service reports instants at.
///
/// Built from whole epoch milliseconds rather than from the host clock's own resolution, so
/// the epoch-seconds number every `Timestamp` member of this service travels as carries at
/// most three fractional digits.
pub fn reported(at: SystemTime) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from_millis(epoch_millis(at))
}

/// The whole milliseconds between the epoch and `at`, saturating rather than wrapping for
/// an instant too far from the epoch to count in milliseconds — which no clock this
/// emulator reads reaches.
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since_epoch| i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reported instant carries whole milliseconds, whatever resolution the stored
    /// instant has — so the epoch-seconds number it travels as has at most three fractional
    /// digits.
    #[test]
    fn a_reported_instant_carries_whole_milliseconds() {
        // (case, the stored instant, the epoch milliseconds it is reported at)
        let rows: &[(&str, SystemTime, i64)] = &[
            ("the epoch itself", SystemTime::UNIX_EPOCH, 0),
            (
                "a whole second",
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                1_700_000_000_000,
            ),
            (
                "a whole millisecond",
                SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_123),
                1_700_000_000_123,
            ),
            (
                "sub-millisecond digits, which are dropped",
                SystemTime::UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789),
                1_700_000_000_123,
            ),
            (
                "a microsecond past a whole second",
                SystemTime::UNIX_EPOCH + Duration::from_micros(1_700_000_000_000_001),
                1_700_000_000_000,
            ),
            (
                "an instant before the epoch, which no clock here reads",
                SystemTime::UNIX_EPOCH - Duration::from_secs(1),
                0,
            ),
        ];
        for (case, at, expected) in rows {
            let reported = reported(*at);
            let actual = reported.secs() * 1_000 + i64::from(reported.subsec_nanos()) / 1_000_000;
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} epoch millis, got {actual:?}"
            );
            assert_eq!(
                reported.subsec_nanos() % 1_000_000,
                0,
                "case {case:?}: expected whole milliseconds, got {} subsecond nanos",
                reported.subsec_nanos()
            );
        }
    }
}
