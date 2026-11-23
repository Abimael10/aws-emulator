// SPDX-License-Identifier: CC0-1.0

//! The instants this crate publishes as CALENDAR TEXT — a year, a month's name and a time of
//! day — rather than as an epoch count.
//!
//! There are two of them, and they are here together because what they share is reading an
//! instant's calendar fields at all: the Common Log Format instant a function URL's request
//! context carries ([`crate::service::function_url`]) and the `basicProperties.timestamp` of a
//! record an Amazon MQ mapping hands its function ([`super::mq_poller`]). Each is ONE format of
//! the instant by `chrono`, this crate's dependency for calendar work, so neither caller walks
//! a rendering's text to recover the fields and no month-name table is written out here.

use std::time::SystemTime;

/// The instant `at` as the Common Log Format writes one: `31/Oct/2026:21:10:43 +0000`.
///
/// The offset is fixed at UTC because the instant is rendered in UTC.
#[must_use]
pub fn common_log_format(at: SystemTime) -> String {
    ::chrono::DateTime::<::chrono::Utc>::from(at)
        .format("%d/%b/%Y:%H:%M:%S +0000")
        .to_string()
}

/// The instant `seconds` epoch seconds names, written as the month, the day and the year and
/// then the time of day on a TWELVE-HOUR clock: `Jan 1, 1970, 12:33:41 AM`.
///
/// `None` for a count no instant can be computed for at all.
#[must_use]
pub fn twelve_hour_clock(seconds: u64) -> Option<String> {
    let at = ::chrono::DateTime::from_timestamp(i64::try_from(seconds).ok()?, 0)?;
    Some(at.format("%b %-d, %Y, %-I:%M:%S %p").to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{common_log_format, twelve_hour_clock};

    /// RULE: an instant is written in the Common Log Format as its zero-padded day, its
    /// month's name, its year and its time of day at UTC.
    ///
    /// The rows differ along what the rule generalises over: the epoch, a single-digit day, a
    /// two-digit day late in the year, and an instant carrying fractional seconds.
    #[test]
    fn an_instant_is_written_as_the_common_log_format_spells_one() {
        // (case, the instant, the text)
        let rows: Vec<(&str, SystemTime, &str)> = vec![
            ("the-epoch", UNIX_EPOCH, "01/Jan/1970:00:00:00 +0000"),
            (
                "a-single-digit-day",
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                "14/Nov/2023:22:13:20 +0000",
            ),
            (
                "a-day-late-in-the-year",
                UNIX_EPOCH + Duration::from_secs(1_767_225_043),
                "31/Dec/2025:23:50:43 +0000",
            ),
            (
                "an-instant-carrying-fractional-seconds",
                UNIX_EPOCH + Duration::from_millis(1_700_000_000_500),
                "14/Nov/2023:22:13:20 +0000",
            ),
        ];
        for (case, at, expected) in rows {
            let written = common_log_format(at);
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: an instant is written on a twelve-hour clock as its month's name, its day without
    /// padding, its year, and the time of day on that clock with its half of the day named.
    ///
    /// The rows differ along what the rule generalises over: midnight, an instant inside the
    /// first hour, noon, an hour past noon, and an instant in another month and year.
    #[test]
    fn an_instant_is_written_on_a_twelve_hour_clock() {
        // (case, the epoch seconds, the text)
        let rows: Vec<(&str, u64, &str)> = vec![
            ("midnight", 0, "Jan 1, 1970, 12:00:00 AM"),
            ("inside-the-first-hour", 2_021, "Jan 1, 1970, 12:33:41 AM"),
            ("noon", 43_200, "Jan 1, 1970, 12:00:00 PM"),
            ("an-hour-past-noon", 46_800, "Jan 1, 1970, 1:00:00 PM"),
            (
                "another-month-and-year",
                1_700_000_000,
                "Nov 14, 2023, 10:13:20 PM",
            ),
        ];
        for (case, seconds, expected) in rows {
            let written = twelve_hour_clock(seconds);
            assert_eq!(
                written.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }
}
