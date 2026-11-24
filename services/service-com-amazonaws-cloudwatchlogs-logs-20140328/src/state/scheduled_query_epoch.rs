// SPDX-License-Identifier: CC0-1.0

//! The MAGNITUDE a scheduled query's epoch instants are stated in.
//!
//! Every instant of the scheduled-query surface is a plain `long` the model names `Timestamp`
//! and the documentation describes only as being "in Unix epoch format": the request's history
//! window (`GetScheduledQueryHistory$startTime`, `$endTime`) and the query's own schedule window
//! (`scheduleStartTime` "The query will not execute before this time", `scheduleEndTime` "The
//! query will stop executing after this time"). No `timestampFormat` and no sentence of those
//! members states a UNIT, so neither of the two magnitudes an epoch instant is written in — the
//! seconds this service's query window is stated in (`StartQuery$startTime`, "the number of
//! seconds since January 1, 1970") and the milliseconds its log events and resource instants are
//! stated in — is excluded by what the member says.
//!
//! So a stated bound names the instant its own magnitude names, and this module is the one place
//! that reading happens: an instant of this era in milliseconds is at least
//! [`MILLISECOND_MAGNITUDE_FLOOR`], while the same instant in seconds is a thousandth of that
//! and stays below that floor until far beyond any epoch a client states — so the two magnitudes
//! name disjoint sets of values and a bound below the floor is the seconds it names. The store
//! keeps every instant of a scheduled query in milliseconds, so that is what a bound is read
//! into.
//!
//! A bound is read where it is USED rather than where it is stored: a write stores the value the
//! caller stated, so a reader of the query reports the schedule window back exactly as it was
//! written.

// The milliseconds one second holds, the one statement of that relation this crate keeps.
use super::MILLIS_PER_SECOND;

/// The smallest epoch instant in MILLISECONDS a client of this emulator states: `10^11`
/// milliseconds is 1973, so every instant in milliseconds a scheduled query is written with is
/// at or above it, while the same instant in seconds — and every instant in seconds until the
/// year 5138 — is below it. The two magnitudes therefore do not overlap, which is what lets a
/// bound be read in the one it states.
const MILLISECOND_MAGNITUDE_FLOOR: i64 = 100_000_000_000;

/// The epoch MILLISECONDS the bound `stated` names: the value itself where it is stated in
/// milliseconds, and the milliseconds of that many seconds where it is stated in seconds.
pub fn instant_ms(stated: i64) -> i64 {
    if stated < MILLISECOND_MAGNITUDE_FLOOR {
        stated.saturating_mul(MILLIS_PER_SECOND)
    } else {
        stated
    }
}

/// The epoch MILLISECONDS the END of a window bounded by `stated` covers.
///
/// A bound stated in seconds names a whole SECOND rather than one millisecond of it, so a window
/// ending at that second covers every instant inside it; a bound already stated in milliseconds
/// names that single millisecond.
pub fn window_end_ms(stated: i64) -> i64 {
    if stated < MILLISECOND_MAGNITUDE_FLOOR {
        instant_ms(stated).saturating_add(MILLIS_PER_SECOND - 1)
    } else {
        stated
    }
}

#[cfg(test)]
mod tests {
    use super::{instant_ms, window_end_ms, MILLISECOND_MAGNITUDE_FLOOR, MILLIS_PER_SECOND};

    /// RULE: a bound names the instant its own magnitude names, read into the milliseconds the
    /// store keeps. The rows span the two magnitudes, the floor between them and the ends of the
    /// range the member admits (`@range(min: 0)`).
    #[test]
    fn a_bound_is_read_in_the_magnitude_it_states() {
        for (case, stated, expected) in [
            ("the epoch itself, the smallest value the member admits", 0, 0),
            ("an instant stated in seconds", 1_700_000_000, 1_700_000_000_000),
            ("the largest value still stated in seconds", 99_999_999_999, 99_999_999_999_000),
            ("the floor, the smallest value stated in milliseconds", MILLISECOND_MAGNITUDE_FLOOR, MILLISECOND_MAGNITUDE_FLOOR),
            ("an instant stated in milliseconds", 1_700_000_000_000, 1_700_000_000_000),
        ] {
            let read = instant_ms(stated);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {stated} to name {expected} ms, got {read}"
            );
        }
    }

    /// RULE: the end of a window covers the whole of the unit it is stated in — a second stated
    /// in seconds, a millisecond stated in milliseconds.
    #[test]
    fn a_window_end_covers_the_unit_it_states() {
        for (case, stated, expected) in [
            ("a second, whose whole second is covered", 1_700_000_000, 1_700_000_000_999),
            ("the epoch second", 0, MILLIS_PER_SECOND - 1),
            (
                "a millisecond, which covers itself alone",
                1_700_000_000_000,
                1_700_000_000_000,
            ),
        ] {
            let covered = window_end_ms(stated);
            assert_eq!(
                covered, expected,
                "case {case:?}: expected the window ending at {stated} to cover up to \
                 {expected} ms, got {covered}"
            );
        }
    }
}
