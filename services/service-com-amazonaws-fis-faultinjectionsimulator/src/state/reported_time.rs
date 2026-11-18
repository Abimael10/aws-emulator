// SPDX-License-Identifier: CC0-1.0

//! The GRANULARITY every timestamp this service reports is projected at.
//!
//! FIS's timestamp members (`Experiment$creationTime`/`startTime`/`endTime`,
//! `ExperimentAction$startTime`/`endTime`, `ExperimentTemplate$creationTime`/
//! `lastUpdateTime` and the summaries' copies of them) carry no `@timestampFormat`, so
//! `restJson1` writes each of them as an epoch-seconds NUMBER, and the service reports
//! those instants in whole seconds: the value a response carries never has a sub-second
//! part. A record stamped straight off the host clock has one, so the instant is cut to
//! the second HERE — at the one boundary where a stored instant becomes a reported
//! timestamp — rather than at each projection's own call site.
//!
//! Only the REPORTED value is cut. What the store measures its owed transitions against
//! keeps every bit of the instant it recorded ([`SystemTime`] throughout
//! [`crate::state`]), so a span this service plans is unaffected by how the instant is
//! reported; a reader comparing two reported timestamps therefore sees that span to the
//! second it is reported at.

use std::time::{Duration, SystemTime};

/// `at` as the timestamp FIS reports: the instant cut down to the whole second it falls
/// in.
///
/// Cut towards the past, never up, so a reported timestamp never names an instant that has
/// not happened yet, and monotonic: of two instants the later one is never reported
/// earlier, so the ORDER and the non-strict ordering of the record's own instants survive
/// the projection.
#[must_use]
pub fn reported_time(at: SystemTime) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from(whole_second(at))
}

/// The whole second `at` falls in, as an instant.
///
/// Private to this module: what the rest of the crate wants is the PROJECTION
/// ([`reported_time`]), and the records keep the instant the clock gave them.
fn whole_second(at: SystemTime) -> SystemTime {
    match at.duration_since(SystemTime::UNIX_EPOCH) {
        // Past the epoch: drop the sub-second part of the elapsed span.
        Ok(since) => SystemTime::UNIX_EPOCH + Duration::from_secs(since.as_secs()),
        // Before it, which no instant a request reaches this service at is: the cut is
        // still towards the past, so a sub-second part counts as a whole second BACK.
        Err(before) => {
            let since = before.duration();
            let seconds = match since.subsec_nanos() {
                0 => since.as_secs(),
                _ => since.as_secs().saturating_add(1),
            };
            SystemTime::UNIX_EPOCH
                .checked_sub(Duration::from_secs(seconds))
                .unwrap_or(SystemTime::UNIX_EPOCH)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instants a case projects, and the whole second each falls in: the epoch itself,
    /// an instant already on a second boundary, ones carrying a sub-second part at each end
    /// of the second, and one before the epoch.
    #[test]
    fn every_reported_timestamp_is_a_whole_number_of_seconds() {
        let at = |seconds: u64, nanos: u32| {
            SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos)
        };
        let rows: &[(&str, SystemTime, i64)] = &[
            ("the epoch", SystemTime::UNIX_EPOCH, 0),
            ("a whole second", at(1_700_000_000, 0), 1_700_000_000),
            ("one nanosecond past a second", at(1_700_000_000, 1), 1_700_000_000),
            (
                "the host clock's seven-digit tail",
                at(1_794_227_006, 256_788_500),
                1_794_227_006,
            ),
            ("half a second", at(1_700_000_000, 500_000_000), 1_700_000_000),
            (
                "one nanosecond short of the next second",
                at(1_700_000_000, 999_999_999),
                1_700_000_000,
            ),
            (
                "an instant before the epoch",
                SystemTime::UNIX_EPOCH - Duration::new(2, 500_000_000),
                -3,
            ),
        ];
        for (case, instant, expected_seconds) in rows {
            let reported = reported_time(*instant);
            assert_eq!(
                reported.subsec_nanos(),
                0,
                "case {case:?}: a reported timestamp carries no sub-second part, got {reported:?}"
            );
            assert_eq!(
                reported.secs(),
                *expected_seconds,
                "case {case:?}: expected {expected_seconds} epoch seconds, got {reported:?}"
            );
        }
    }

    /// The projection never moves an instant forward, and never reports the later of two
    /// instants earlier than the other.
    #[test]
    fn the_projection_is_monotonic_and_never_names_a_future_instant() {
        let base = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        let spans: &[(&str, Duration)] = &[
            ("no span at all", Duration::ZERO),
            ("one nanosecond", Duration::from_nanos(1)),
            ("half a second", Duration::from_millis(500)),
            ("a run's span", Duration::from_secs(15)),
            ("an hour", Duration::from_secs(3_600)),
        ];
        for (case, span) in spans {
            let later = base + *span;
            assert!(
                whole_second(later) <= later,
                "case {case:?}: expected the reported instant not to follow {later:?}, got {:?}",
                whole_second(later)
            );
            assert!(
                reported_time(later).secs() >= reported_time(base).secs(),
                "case {case:?}: expected the later instant not to be reported before \
                 {:?}, got {:?}",
                reported_time(base),
                reported_time(later)
            );
        }
    }
}
