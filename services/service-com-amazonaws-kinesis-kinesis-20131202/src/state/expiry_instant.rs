// SPDX-License-Identifier: CC0-1.0

//! How an EXPIRY refusal of this service spells the instant it names.
//!
//! Two refusals of this service name an instant inside their sentence — a shard iterator past
//! its age ([`super::iterator_expired_message`]) and a pagination cursor past its lifetime
//! ([`super::next_token_expired_message`]) — and Kinesis spells both of them in ONE layout:
//! the three-letter weekday, the three-letter month, the zero-padded day of the month, the
//! 24-hour time, the literal `UTC` and the four-digit year. The rendering lives here once, so
//! the two sentences cannot disagree about it, and the calendar fields come from the
//! maintained date library rather than being counted out here.

use ::chrono::DateTime;

/// The layout such an instant is rendered in, as the date library spells a format — e.g. `Thu
/// Nov 09 20:49:40 UTC 2026`. The zone is fixed text because the instant itself is placed in
/// UTC.
const EXPIRY_INSTANT_FORMAT: &str = "%a %b %d %H:%M:%S UTC %Y";

/// The instant `millis` milliseconds after the epoch names, as an expiry refusal spells it.
///
/// An instant no calendar can place — further from the epoch than the date library reaches —
/// names no time at all, so the clause carries nothing for it rather than the rendering of
/// some other instant.
pub fn expiry_instant_text(millis: u128) -> String {
    let Ok(millis) = i64::try_from(millis) else {
        return String::new();
    };
    DateTime::from_timestamp_millis(millis)
        .map(|placed| placed.format(EXPIRY_INSTANT_FORMAT).to_string())
        .unwrap_or_default()
}
