// SPDX-License-Identifier: CC0-1.0

//! The closed time interval a read scopes itself by, and the refusal a request
//! that names one running BACKWARDS carries.
//!
//! Several reads of this service take a `StartTime`/`EndTime` pair naming one
//! interval to report over. The pair is judged on the REQUEST alone, before any
//! lookup: an interval whose end precedes its beginning holds no instant at all,
//! so there is nothing for the read to report and nothing the caller could have
//! meant by it.

/// Why a read whose interval ENDS before it BEGINS is refused.
///
/// Amazon Redshift's own wording for this error class, and entirely fixed text:
/// it interpolates neither instant, neither member name nor any other value the
/// request carried.
pub const INVERTED_TIME_INTERVAL: &str = "End time must not precede start time";

/// Whether the interval a request names runs BACKWARDS: both of its ends named,
/// and the end before the beginning.
///
/// Only a request that names BOTH ends states an interval at all. An omitted end
/// is one the operation's own default supplies — the instant the request is
/// served, or a span measured back from it — so it is never a claim the caller
/// made about an ordering, and a request that names one end alone is served.
///
/// Compared at full timestamp precision, so an end a fraction of a second before
/// its start is as inverted as one a day before it.
pub fn inverted_time_interval(
    start_time: Option<::aws_smithy_types::DateTime>,
    end_time: Option<::aws_smithy_types::DateTime>,
) -> bool {
    match (start_time, end_time) {
        (Some(start), Some(end)) => end.as_nanos() < start.as_nanos(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::inverted_time_interval;
    use ::aws_smithy_types::DateTime;

    /// RULE: the pair runs backwards exactly when both ends are named and the end
    /// precedes the beginning.
    ///
    /// The rows differ along what the rule generalises over: which ends the
    /// request named, how far apart they are, and whether they differ above or
    /// below the second.
    #[test]
    fn only_a_named_pair_whose_end_precedes_its_start_is_inverted() {
        let at = DateTime::from_secs(1_700_000_000);
        let an_hour_later = DateTime::from_secs(1_700_003_600);
        let a_nanosecond_later = DateTime::from_secs_and_nanos(1_700_000_000, 1);

        let rows: [(&str, Option<DateTime>, Option<DateTime>, bool); 8] = [
            ("neither end named", None, None, false),
            ("the beginning alone", Some(an_hour_later), None, false),
            ("the end alone", None, Some(at), false),
            ("an interval an hour wide", Some(at), Some(an_hour_later), false),
            ("both ends the same instant", Some(at), Some(at), false),
            (
                "an end an hour before its start",
                Some(an_hour_later),
                Some(at),
                true,
            ),
            (
                "an end a nanosecond before its start",
                Some(a_nanosecond_later),
                Some(at),
                true,
            ),
            (
                "an end at the epoch",
                Some(at),
                Some(DateTime::from_secs(0)),
                true,
            ),
        ];
        for (case, start, end, expected) in rows {
            let actual = inverted_time_interval(start, end);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected inverted={expected}, got {actual}"
            );
        }
    }
}
