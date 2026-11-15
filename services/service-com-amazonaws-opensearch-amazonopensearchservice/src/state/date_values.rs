// SPDX-License-Identifier: CC0-1.0

//! The INSTANT a value of a field mapped as a date stands for — the one reading of that
//! mapping this service has.
//!
//! A date field declares the formats its values may be written in, and a mapping that
//! declares none takes the engine's default pair ([`DEFAULT_FORMATS`]): an ISO-8601 date
//! whose time part is optional, or a count of milliseconds since the epoch. Both forms name
//! the same kind of thing — an instant — so the form a value happens to be written in is a
//! spelling and not a value: a document carrying either is indexed, and every comparison on
//! the field (a term, a range bound, an order) is taken on the instant each side parses to
//! rather than on the characters it was written with.
//!
//! What is stored is never rewritten: a document's `_source` is answered exactly as it was
//! sent, and the instant below is computed from it when a comparison needs one.
//!
//! The calendar arithmetic is the `chrono` crate's — leap years, month lengths and the
//! offset a value carries are its business, not this module's; what is stated here is only
//! WHICH shapes of text the format set admits, which is a property of the mapping.

use ::serde_json::Value;

/// The formats a date field whose mapping declares none accepts, as a mapping writes them.
///
/// Both are tried, in this order, which is what makes a four-digit year a YEAR rather than a
/// four-millisecond instant.
pub const DEFAULT_FORMATS: &str = "strict_date_optional_time||epoch_millis";

/// The mapped field types whose values are instants.
pub const DATE_FIELD_TYPES: [&str; 2] = ["date", "date_nanos"];

/// Whether `mapped_type` is one of those.
#[must_use]
pub fn is_date_type(mapped_type: &str) -> bool {
    DATE_FIELD_TYPES.contains(&mapped_type)
}

/// The instant `value` stands for, in milliseconds since the epoch, or `None` for a value
/// outside the format set.
///
/// A JSON NUMBER is epoch milliseconds, which is the whole of what that format reads; a
/// STRING is read as the ISO form first and as epoch milliseconds after, so every spelling
/// of the pair is admitted and nothing else is.
#[must_use]
pub fn instant_millis(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|millis| millis as i64)),
        Value::String(text) => parsed_millis(text),
        _ => None,
    }
}

/// The instant the TEXT `text` stands for, under the same format set.
#[must_use]
pub fn parsed_millis(text: &str) -> Option<i64> {
    iso_millis(text).or_else(|| text.parse::<i64>().ok())
}

/// The separator the optional time part follows the date part after.
const TIME_SEPARATOR: char = 'T';


/// The instant an ISO-8601 value whose time part is optional stands for: the date is
/// required, the time and the offset are not, and a value carrying no offset is read at UTC
/// — the time zone a date field whose mapping names none is read in.
///
/// STRICT about the number of digits each component is written with, because that is what
/// separates this format from the lenient one: a two-digit year or a one-digit month names no
/// value of it.
fn iso_millis(text: &str) -> Option<i64> {
    let (date, time) = match text.split_once(TIME_SEPARATOR) {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let date = strict_date(date)?;
    let Some(time) = time else {
        return Some(
            date.and_time(::chrono::NaiveTime::MIN)
                .and_utc()
                .timestamp_millis(),
        );
    };
    let (clock, offset_seconds) = split_offset(time)?;
    let clock = strict_time(clock)?;
    date.and_time(clock)
        .and_utc()
        .timestamp_millis()
        .checked_sub(i64::from(offset_seconds).checked_mul(1_000)?)
}

/// The date `yyyy`, `yyyy-MM` or `yyyy-MM-dd` names, with the components it leaves out
/// taken at the start of the year or month — which is what makes the time part optional at
/// every level rather than only the clock.
fn strict_date(text: &str) -> Option<::chrono::NaiveDate> {
    let mut parts = text.split('-');
    let year = fixed_digits(parts.next()?, 4)?;
    let month = parts.next().map_or(Some(1), |part| fixed_digits(part, 2))?;
    let day = parts.next().map_or(Some(1), |part| fixed_digits(part, 2))?;
    if parts.next().is_some() {
        return None;
    }
    ::chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)
}

/// The time of day `HH:mm`, `HH:mm:ss` or `HH:mm:ss.SSS…` names.
///
/// A fraction of a second is admitted only where the second itself is written, and is read to
/// nanosecond resolution: a `date` field keeps milliseconds and a `date_nanos` field keeps
/// nanoseconds, and both are written in this one form.
fn strict_time(text: &str) -> Option<::chrono::NaiveTime> {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };
    let mut parts = clock.split(':');
    let hour = fixed_digits(parts.next()?, 2)?;
    let minute = fixed_digits(parts.next()?, 2)?;
    let second = parts.next().map(|part| fixed_digits(part, 2));
    if parts.next().is_some() {
        return None;
    }
    let nanosecond = match fraction {
        None => 0,
        // A fraction with no second to be a fraction OF is not a value of this format.
        Some(_) if second.is_none() => return None,
        Some(fraction) => nanoseconds(fraction)?,
    };
    ::chrono::NaiveTime::from_hms_nano_opt(hour, minute, second.unwrap_or(Some(0))?, nanosecond)
}

/// The time part split from the offset that may follow it, with the offset as the seconds
/// east of UTC it names: `Z` for none at all, or `±HH`, `±HHmm`, `±HH:mm`.
fn split_offset(text: &str) -> Option<(&str, i32)> {
    // The zero offset is written with a `Z`, in either case the format admits it in.
    if let Some(clock) = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix('z'))
    {
        return Some((clock, 0));
    }
    // The sign is looked for from the END, so the clock itself keeps its own separators.
    let Some(at) = text.rfind(['+', '-']) else {
        return Some((text, 0));
    };
    let (clock, offset) = text.split_at(at);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let offset = offset.get(1..)?;
    let (hours, minutes) = match offset.split_once(':') {
        Some((hours, minutes)) => (hours, Some(minutes)),
        None if offset.len() > 2 => (offset.get(..2)?, offset.get(2..)),
        None => (offset, None),
    };
    let hours = fixed_digits(hours, 2)?;
    let minutes = minutes.map_or(Some(0), |minutes| fixed_digits(minutes, 2))?;
    let seconds = i32::try_from(hours.checked_mul(3_600)?.checked_add(minutes.checked_mul(60)?)?)
        .ok()?;
    Some((clock, sign * seconds))
}

/// The number `text` spells in EXACTLY `digits` ASCII digits, and nothing else.
fn fixed_digits(text: &str, digits: usize) -> Option<u32> {
    (text.len() == digits && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The nanoseconds a fraction-of-a-second written as `text` names: its digits padded out to
/// nanosecond places, and the places past them dropped — which is what a resolution finer
/// than the field keeps comes to.
fn nanoseconds(text: &str) -> Option<u32> {
    /// How many digits a fraction of a second is read to.
    const NANOSECOND_DIGITS: usize = 9;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut digits: String = text.chars().take(NANOSECOND_DIGITS).collect();
    while digits.len() < NANOSECOND_DIGITS {
        digits.push('0');
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the format set admits an ISO-8601 value whose time part is optional and a count
    /// of epoch milliseconds, and the instant it reads is the same whichever of them the
    /// value was written in.
    ///
    /// The rows span the levels the ISO form may stop at, the offsets it may carry and the
    /// number forms, each against the instant it names.
    #[test]
    fn the_default_format_set_reads_both_of_its_forms() {
        // (the case, the value, the instant in epoch milliseconds)
        let rows: &[(&str, Value, i64)] = &[
            ("a date alone", ::serde_json::json!("2026-01-01"), 1_767_225_600_000),
            ("a year and a month", ::serde_json::json!("2026-01"), 1_767_225_600_000),
            ("a year alone", ::serde_json::json!("2026"), 1_767_225_600_000),
            (
                "a date and a clock",
                ::serde_json::json!("2026-01-01T00:00:00"),
                1_767_225_600_000,
            ),
            (
                "a date, a clock and the zero offset",
                ::serde_json::json!("2026-01-01T00:00:00Z"),
                1_767_225_600_000,
            ),
            (
                "a clock without seconds",
                ::serde_json::json!("2026-01-01T01:00Z"),
                1_767_229_200_000,
            ),
            (
                "a fraction of a second",
                ::serde_json::json!("2026-01-01T00:00:00.250Z"),
                1_767_225_600_250,
            ),
            (
                "an offset east of UTC",
                ::serde_json::json!("2026-01-01T01:00:00+01:00"),
                1_767_225_600_000,
            ),
            (
                "an offset west of UTC, written without a colon",
                ::serde_json::json!("2025-12-31T23:00:00-0100"),
                1_767_225_600_000,
            ),
            (
                "a leap day",
                ::serde_json::json!("2024-02-29"),
                1_709_164_800_000,
            ),
            ("epoch milliseconds as a number", ::serde_json::json!(1_767_225_600_000_i64), 1_767_225_600_000),
            (
                "epoch milliseconds as text",
                ::serde_json::json!("1767225600000"),
                1_767_225_600_000,
            ),
            ("the epoch itself", ::serde_json::json!(0), 0),
            (
                "an instant before the epoch",
                ::serde_json::json!("1969-12-31T23:59:59Z"),
                -1_000,
            ),
        ];
        for (case, value, expected) in rows {
            let read = instant_millis(value);
            assert_eq!(
                read,
                Some(*expected),
                "case {case:?}: expected {expected} ms, got {read:?}"
            );
        }
    }

    /// RULE: a value outside the format set names no instant — which is what makes a field
    /// mapped as a date refuse it.
    #[test]
    fn a_value_outside_the_format_set_names_no_instant() {
        // (the case, the value)
        let rows: &[(&str, Value)] = &[
            ("text that is no date at all", ::serde_json::json!("not-a-date")),
            ("a month nothing numbers", ::serde_json::json!("2026-13-01")),
            ("a day the month has not", ::serde_json::json!("2026-02-30")),
            ("a year written in two digits", ::serde_json::json!("26-01-01")),
            ("a month written in one digit", ::serde_json::json!("2026-1-01")),
            ("a date with a trailing component", ::serde_json::json!("2026-01-01-01")),
            ("a clock with no minutes", ::serde_json::json!("2026-01-01T00")),
            ("an hour nothing clocks", ::serde_json::json!("2026-01-01T24:00:00Z")),
            ("a fraction with no second", ::serde_json::json!("2026-01-01T00:00.5Z")),
            ("an offset in one digit", ::serde_json::json!("2026-01-01T00:00:00+1:00")),
            ("an empty value", ::serde_json::json!("")),
            ("a value of another kind", ::serde_json::json!(true)),
            ("an object", ::serde_json::json!({ "at": "2026-01-01" })),
        ];
        for (case, value) in rows {
            let read = instant_millis(value);
            assert!(
                read.is_none(),
                "case {case:?}: expected no instant, got {read:?}"
            );
        }
    }
}
