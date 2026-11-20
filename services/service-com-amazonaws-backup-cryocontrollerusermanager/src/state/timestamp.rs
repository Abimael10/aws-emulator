// SPDX-License-Identifier: CC0-1.0

//! Unit coverage for [`super::modelled_timestamp`], the one conversion of a stored instant
//! into the modelled timestamp a response reports.
//!
//! What the wire reports is covered end to end by this crate's integration suites. What is
//! covered here is the arithmetic that conversion does, and the two branches no request can
//! reach — an instant before the epoch, and one too far from it to express as epoch
//! milliseconds — which must answer an instant rather than panic, because every read path in
//! this crate goes through the conversion.

use std::time::{Duration, UNIX_EPOCH};

use super::modelled_timestamp;

/// An instant keeps its milliseconds and loses everything finer, whatever the sub-second
/// part of the stored instant is: the rows cover a whole second, each boundary of one
/// millisecond, and the sub-millisecond readings the host clock actually produces.
#[test]
fn an_instant_is_truncated_to_the_documented_millisecond_precision() {
    // (case, the stored instant's nanoseconds past a whole second, the milliseconds it
    // must report)
    let cases = [
        ("a whole second", 0u32, 0i32),
        ("one nanosecond past", 1, 0),
        ("just under one millisecond", 999_999, 0),
        ("exactly one millisecond", 1_000_000, 1),
        ("the six-decimal reading a host clock gives", 110_162_000, 110),
        ("just under a whole second", 999_999_999, 999),
    ];
    let whole_second = UNIX_EPOCH + Duration::from_secs(1_516_925_490);
    for (case, nanos, millis) in cases {
        let at = whole_second + Duration::from_nanos(u64::from(nanos));
        let reported = modelled_timestamp(at);
        assert_eq!(
            (reported.secs(), reported.subsec_nanos()),
            (1_516_925_490, (millis as u32) * 1_000_000),
            "case {case:?}: expected {millis} ms past the second, got {reported:?}"
        );
    }
}

/// The branches no request can reach answer an instant rather than panicking: an instant
/// before the epoch is reported the other side of it, and one no epoch-millisecond count can
/// express is reported at the furthest instant that can be.
#[test]
fn an_instant_outside_the_ordinary_range_answers_an_instant() {
    let before_epoch = UNIX_EPOCH - Duration::from_millis(1_500);
    let reported = modelled_timestamp(before_epoch);
    assert_eq!(
        reported.to_millis().ok(),
        Some(-1_500),
        "an instant before the epoch is as many milliseconds the other side of it, \
         got {reported:?}"
    );

    let unexpressible = UNIX_EPOCH + Duration::from_secs(u64::MAX / 1_000);
    let reported = modelled_timestamp(unexpressible);
    assert_eq!(
        reported.to_millis().ok(),
        Some(i64::MAX),
        "an instant no epoch-millisecond count can express is reported at the furthest \
         one that can be, got {reported:?}"
    );
}
