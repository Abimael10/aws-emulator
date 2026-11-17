// SPDX-License-Identifier: CC0-1.0

//! Which records of what one pass read off a shard are TOO OLD for the mapping to hand
//! over.
//!
//! "Discard records older than the specified age. The default value is infinite (-1)"
//! (`MaximumRecordAgeInSeconds`): a mapping that states a finite age sheds the backlog it
//! can no longer usefully process, so a record whose age has passed that bound is handed to
//! no invocation at all. The bound is the MAPPING's, not the source's, so it holds for every
//! stream source this reader reads ([`super::kinesis_stream`], [`super::dynamodb_stream`]) —
//! each of them reports the instant its records arrived at, and the age is measured from
//! that instant to the one the pass read the record at.
//!
//! A discarded record is the PASS's own to account for, exactly as a record the mapping's
//! `FilterCriteria` rejected is ([`super::record_filter`]): no invocation will report about
//! it, so the shard's position moves past it as soon as it has been read — which is what
//! lets the records behind a discarded one be delivered, and what makes a mapping whose
//! whole backlog has aged out invoke its function not at all while still reading on.
//!
//! Two cases discard nothing: the infinite default, which is what a mapping carrying no
//! value and one carrying a negative value both ask for, and a record whose source reports
//! no arrival instant, whose age this reader cannot measure.

use super::*;

/// How old a record of a mapping carrying `maximum_record_age_in_seconds` may be and still
/// be handed over, or `None` for the infinite age that discards nothing.
///
/// "The default value is infinite (-1)", so a mapping carrying no value and one carrying a
/// negative one both answer `None`; every other value the member's own range admits
/// (`@range(min: -1, max: 604800)`) is that many seconds, read as the span it states. The
/// rule this feeds is "older than the stated age" whatever that age is, so a stored value
/// below the smallest age AWS publishes for the member is honoured as the span it is rather
/// than rounded up to one this reader would prefer.
pub(super) fn maximum_record_age(maximum_record_age_in_seconds: Option<i32>) -> Option<Duration> {
    let seconds = u64::try_from(maximum_record_age_in_seconds?).ok()?;
    Some(Duration::from_secs(seconds))
}

/// The records of `records` the mapping's record age DISCARDS, by sequence number, in the
/// order the shard holds them.
///
/// A record is discarded when it is OLDER than the age the mapping allows: a record exactly
/// that old is still handed over, since what the member names is the age records are
/// discarded for being older than. A mapping with the infinite age discards none, and so
/// does a record this emulator holds no arrival instant for — an age that cannot be measured
/// is no reason to withhold a record from the function.
pub(super) fn aged_out(
    maximum_record_age: Option<Duration>,
    records: &[ShardRecord],
    read_at: SystemTime,
) -> Vec<String> {
    let Some(allowed) = maximum_record_age else {
        return Vec::new();
    };
    records
        .iter()
        .filter(|record| {
            age_of(record.arrived_at.as_ref(), read_at).is_some_and(|age| age > allowed)
        })
        .map(|record| record.sequence_number.clone())
        .collect()
}

/// How old a record stamped `arrived_at` was at `read_at`, or `None` for a record carrying
/// no arrival instant.
///
/// The ONE reading of how old a record of a shard is: both what this module's decision is
/// taken on and what the `IteratorAge` a pass meters is taken from
/// ([`super::iterator_age_millis`]).
///
/// Never negative: a record this emulator stamped in the future — which the coarse stamp of
/// a source reporting whole seconds can be, read inside that same second — is no record of
/// the past, so its age is nothing rather than a span counted backwards. A span no duration
/// can hold answers `None` as a record carrying no instant does, since an age that cannot be
/// measured is no reason to withhold a record.
pub(super) fn age_of(
    arrived_at: Option<&::aws_smithy_types::DateTime>,
    read_at: SystemTime,
) -> Option<Duration> {
    let arrived_at = arrived_at?;
    let read_at = ::aws_smithy_types::DateTime::from(read_at);
    let seconds = (read_at.as_secs_f64() - arrived_at.as_secs_f64()).max(0.0);
    Duration::try_from_secs_f64(seconds).ok()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{aged_out, maximum_record_age};
    use crate::state::event_source_poller::ShardRecord;

    /// One record stamped `arrived_at` seconds before `now`, named by its sequence number.
    fn record(sequence_number: &str, arrived_at: Option<SystemTime>) -> ShardRecord {
        ShardRecord {
            sequence_number: sequence_number.to_string(),
            arrived_at: arrived_at.map(::aws_smithy_types::DateTime::from),
            envelope: ::serde_json::json!({}),
        }
    }

    /// RULE: a mapping carrying no finite record age discards nothing, and every value the
    /// member's range admits above the infinite default is that many seconds.
    ///
    /// The rows differ along what the rule generalises over: the member absent, the infinite
    /// default, both bounds the member's own range admits above it, and a value inside it.
    #[test]
    fn the_configured_age_is_a_span_only_where_it_is_not_infinite() {
        // (case, the member's value, the age it allows)
        let rows: Vec<(&str, Option<i32>, Option<Duration>)> = vec![
            ("the-member-absent", None, None),
            ("the-infinite-default", Some(-1), None),
            ("zero", Some(0), Some(Duration::ZERO)),
            ("one-second", Some(1), Some(Duration::from_secs(1))),
            ("the-smallest-age-aws-publishes", Some(60), Some(Duration::from_secs(60))),
            (
                "the-largest-the-range-admits",
                Some(604_800),
                Some(Duration::from_secs(604_800)),
            ),
        ];
        for (case, configured, expected) in rows {
            let allowed = maximum_record_age(configured);
            assert_eq!(
                allowed, expected,
                "case {case:?}: expected {expected:?} for {configured:?}, got {allowed:?}"
            );
        }
    }

    /// One row of the table below: the case, the age the mapping allows, the records the
    /// pass read, and the sequence numbers that age discards.
    type AgeRow = (
        &'static str,
        Option<Duration>,
        Vec<ShardRecord>,
        Vec<&'static str>,
    );

    /// RULE: a record older than the age the mapping allows is discarded; one that is not,
    /// and every record of a mapping with the infinite age, is handed over.
    ///
    /// The rows differ along what the rule generalises over: the age configured (infinite,
    /// and finite values either side of the records' own ages), how old the records are
    /// against it (younger, exactly it, older), how many of them are discarded (none, some,
    /// all), and a record carrying no arrival instant at all.
    #[test]
    fn a_record_older_than_the_age_the_mapping_allows_is_discarded() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let ago = |seconds: u64| Some(now - Duration::from_secs(seconds));
        // (case, the age allowed, the records, the sequence numbers discarded)
        let rows: Vec<AgeRow> = vec![
            (
                "the-infinite-age-discards-nothing",
                None,
                vec![record("1", ago(0)), record("2", ago(100_000))],
                vec![],
            ),
            (
                "records-younger-than-the-age",
                Some(Duration::from_secs(60)),
                vec![record("1", ago(0)), record("2", ago(59))],
                vec![],
            ),
            (
                "a-record-exactly-the-age-is-not-older-than-it",
                Some(Duration::from_secs(60)),
                vec![record("1", ago(60))],
                vec![],
            ),
            (
                "the-oldest-records-of-a-batch",
                Some(Duration::from_secs(60)),
                vec![record("1", ago(600)), record("2", ago(61)), record("3", ago(5))],
                vec!["1", "2"],
            ),
            (
                "a-whole-batch-past-the-age",
                Some(Duration::from_secs(60)),
                vec![record("1", ago(61)), record("2", ago(3_600))],
                vec!["1", "2"],
            ),
            (
                "a-record-with-no-arrival-instant-is-not-discarded",
                Some(Duration::from_secs(60)),
                vec![record("1", None), record("2", ago(3_600))],
                vec!["2"],
            ),
            (
                "an-age-of-zero-discards-every-record-with-an-age",
                Some(Duration::ZERO),
                vec![record("1", ago(1)), record("2", ago(0)), record("3", None)],
                vec!["1"],
            ),
        ];
        for (case, allowed, records, expected) in rows {
            let discarded = aged_out(allowed, &records, now);
            assert_eq!(
                discarded,
                expected
                    .iter()
                    .map(|name| (*name).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?}: expected the records {expected:?} discarded under {allowed:?}, \
                 got {discarded:?}"
            );
        }
    }
}
