// SPDX-License-Identifier: CC0-1.0

//! The ONE rendering of an instant this service writes into a STRING a caller READS.
//!
//! Most instants leave this service as a modelled `Timestamp` member, which the protocol
//! renders (`crate::state::to_date_time`). A few leave it as TEXT inside a document the
//! Amazon States Language itself defines — the context object's `Execution.StartTime` and
//! `State.EnteredTime`, which a definition reads through `$$` and which a diagnosis about
//! a path that could not be resolved quotes WHOLE
//! ([`super::CONTEXT_PATH_OPENING`]), and the dates a Map Run's `ResultWriter` manifest
//! records for each child workflow execution. Those are strings a client compares, so the
//! shape of one is part of what it reads, and it is stated here once rather than per call
//! site.
//!
//! The rendering is the millisecond-precision ISO 8601 instant Step Functions writes:
//! `<date>T<time>.<three digits>Z`. Its fraction is FIXED WIDTH, so the shape of the
//! string does not vary with the VALUE of the instant it carries.

use std::time::SystemTime;

/// `time` as the ISO 8601 instant this service writes into a string a caller reads.
///
/// The fraction is three digits, zero-padded ([`INSTANT_FORMAT`]), because the
/// millisecond is the precision of the field itself and not a value whose trailing zeros
/// are noise: an instant stamped `.820` renders `.820`, one stamped on the whole second
/// renders `.000`, and a client comparing one of these strings reads the same shape
/// whatever the instant. A renderer that drops the fraction's trailing zeros instead
/// makes the shape of the string a function of the value, so one instant in ten is
/// reported with two fraction digits and one in a thousand with none.
pub(super) fn iso(time: SystemTime) -> String {
    let millis = crate::state::epoch_millis(time);
    ::chrono::DateTime::<::chrono::Utc>::from_timestamp_millis(millis)
        .map(|instant| instant.format(INSTANT_FORMAT).to_string())
        // Every instant a walk holds is inside the calendar the maintained
        // implementation covers; one outside it is reported as the epoch milliseconds it
        // was taken at rather than as nothing at all.
        .unwrap_or_else(|| millis.to_string())
}

/// The rendering [`iso`] writes: the UTC calendar instant, a three-digit fraction (`%.3f`
/// writes the millisecond fraction zero-padded) and the UTC designator.
const INSTANT_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// How many characters the rendering holds: `1970-01-01T00:00:00.000Z`.
    const RENDERED_LENGTH: usize = 24;

    /// The instant `millis` after the Unix epoch.
    fn at(millis: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).expect("a positive instant"))
    }

    /// One row: what a failing assertion calls it, the instant in epoch milliseconds, and
    /// the rendering expected. The rows differ along what the rule generalises over: how
    /// many of the fraction's digits are significant, from three down to none.
    #[test]
    fn an_instant_renders_with_a_three_digit_fraction_whatever_its_value() {
        let rows: [(&str, i64, &str); 8] = [
            ("the epoch itself", 0, "1970-01-01T00:00:00.000Z"),
            ("a fraction of three digits", 123, "1970-01-01T00:00:00.123Z"),
            ("a fraction ending in one zero", 820, "1970-01-01T00:00:00.820Z"),
            ("a fraction ending in two zeros", 500, "1970-01-01T00:00:00.500Z"),
            ("a fraction of one significant digit", 5, "1970-01-01T00:00:00.005Z"),
            ("a fraction of two significant digits", 50, "1970-01-01T00:00:00.050Z"),
            ("a whole second", 1_000, "1970-01-01T00:00:01.000Z"),
            (
                "an instant of this decade whose fraction ends in a zero",
                1_795_399_859_820,
                "2026-11-23T02:10:59.820Z",
            ),
        ];

        for (case, millis, expected) in rows {
            let rendered = iso(at(millis));
            assert_eq!(
                rendered, expected,
                "case {case:?}: expected {millis} ms after the epoch to render {expected:?}, \
                 got {rendered:?}"
            );
        }
    }

    /// The INVARIANT the rows above are cases of: every instant renders in the one shape —
    /// the fixed length, the separators where the form puts them, and a fraction that is
    /// three digits carrying the instant's own milliseconds — and reads back as the very
    /// instant it was rendered from.
    ///
    /// The instants swept are every millisecond of the first two seconds (which is where
    /// a fraction of fewer significant digits falls) and one instant of each of the
    /// decades the service runs in, each walked across a whole second so that every
    /// fraction is covered there too. Bounded, and nothing is written anywhere.
    #[test]
    fn every_rendered_instant_carries_its_own_milliseconds_in_three_digits() {
        let decades = [0i64, 1_000_000_000_000, 1_795_399_859_000, 4_102_444_800_000];
        let instants = (0..2_000).chain(
            decades
                .into_iter()
                .flat_map(|base| (0..1_000).map(move |millis| base + millis)),
        );
        for millis in instants {
            let rendered = iso(at(millis));
            assert_eq!(
                rendered.len(),
                RENDERED_LENGTH,
                "case {millis}: expected a rendering of {RENDERED_LENGTH} characters, got \
                 {rendered:?}"
            );
            let fraction = &rendered[20..23];
            assert_eq!(
                (&rendered[10..11], &rendered[19..20], &rendered[23..]),
                ("T", ".", "Z"),
                "case {millis}: expected the separators of the ISO 8601 form, got {rendered:?}"
            );
            assert_eq!(
                fraction,
                format!("{:03}", millis.rem_euclid(1_000)),
                "case {millis}: expected the instant's own milliseconds in three digits, got \
                 {rendered:?}"
            );
            let read_back = ::chrono::DateTime::parse_from_rfc3339(&rendered)
                .unwrap_or_else(|why| panic!("case {millis}: {rendered:?} must read back: {why}"));
            assert_eq!(
                read_back.timestamp_millis(),
                millis,
                "case {millis}: expected {rendered:?} to name the instant it was rendered from"
            );
        }
    }
}
