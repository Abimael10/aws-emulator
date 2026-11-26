// SPDX-License-Identifier: CC0-1.0

//! What one INSTANT looks like in a `Date` member of this service's responses.
//!
//! Every timestamp DynamoDB reports targets `com.amazonaws.dynamodb#Date` — "the date and
//! time when the table was created, in UNIX epoch time format"
//! (`TableDescription$CreationDateTime`, and every sibling Date member reads the same way) —
//! and the wire form of that shape is epoch seconds at MILLISECOND resolution: three
//! fractional digits at most. The clock this process reads is finer than that, so an instant
//! is truncated to the millisecond it falls in before it is reported. A finer number would
//! state a precision the service does not have, and a client that round-trips, formats,
//! hashes or compares it would read digits AWS never emits.
//!
//! One place, so every Date member of every response has the same shape; it lives beside
//! [`super`] because it is about how an instant is REPORTED rather than about what the store
//! holds. `super` re-exports it, so every caller still names it `crate::state::…`.

use std::time::SystemTime;

use aws_smithy_types::DateTime;

/// How many nanoseconds one millisecond holds — the resolution a reported instant keeps.
const NANOS_PER_MILLISECOND: u32 = 1_000_000;

/// `instant` as a `Date` member of a response states it: epoch seconds, truncated to the
/// millisecond it falls in.
pub fn reported_date(instant: SystemTime) -> DateTime {
    let date = DateTime::from(instant);
    DateTime::from_secs_and_nanos(
        date.secs(),
        (date.subsec_nanos() / NANOS_PER_MILLISECOND) * NANOS_PER_MILLISECOND,
    )
}

/// `instant` as TEXT: the date-time form an AWS timestamp takes where one is written into a
/// document or interpolated into a sentence (`2020-11-04T07:28:34.028Z`), at the same
/// millisecond resolution [`reported_date`] reports one at.
///
/// One place, so an instant a manifest carries and an instant a refusal names read the same
/// way. An instant too far out to render in that form at all renders as the epoch second it
/// carries, so the text always names the value in hand.
pub fn reported_instant_text(instant: SystemTime) -> String {
    let reported = reported_date(instant);
    reported
        .fmt(aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|_| reported.secs().to_string())
}
