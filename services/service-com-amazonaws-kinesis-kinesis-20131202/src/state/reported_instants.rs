// SPDX-License-Identifier: CC0-1.0

//! The two halves of ONE rule about instants: the resolution this service keeps them at.
//!
//! Every instant this service stores is kept to the MILLISECOND — the resolution the other
//! values it hands a client are already keyed by (a record's sequence number, a pagination
//! cursor, a shard iterator) — so [`timestamp`] reports a stored instant at that resolution
//! and [`requested_millis`] takes an instant a REQUEST carried at the millisecond nearest
//! it. The two sit beside each other because they have to agree: a client that hands back
//! an instant this service reported must name the very millisecond it was told about, and
//! the `awsJson1_1` wire carries a `Timestamp` as an epoch-seconds NUMBER, which is read
//! back through a binary float and so arrives a few nanoseconds either side of the number
//! that was written.

use super::*;

/// The modelled timestamp for a stored instant, at the millisecond resolution this
/// service's instants are kept at.
///
/// Built from whole epoch milliseconds ([`epoch_millis`]) rather than from the host clock's
/// own resolution, so the epoch-seconds number every `Timestamp` member of this service
/// travels as carries at most three fractional digits.
pub fn timestamp(at: SystemTime) -> ::aws_smithy_types::DateTime {
    ::aws_smithy_types::DateTime::from_millis(i64::try_from(epoch_millis(at)).unwrap_or(i64::MAX))
}

/// The epoch millisecond the instant `at` a request carried names, or `None` when that
/// instant is too far from the epoch to be one.
///
/// The NEAREST millisecond, not the one below it: an epoch-seconds number is read back
/// through a binary float, which lands a few nanoseconds either side of the number that was
/// written, so an instant this service reported as a whole millisecond arrives as that
/// millisecond give or take — and truncating it would make half of the instants a client
/// hands back name the millisecond before the one it was told about.
pub fn requested_millis(at: ::aws_smithy_types::DateTime) -> Option<i64> {
    at.secs()
        .checked_mul(1_000)?
        .checked_add(i64::from((at.subsec_nanos() + 500_000) / 1_000_000))
}
