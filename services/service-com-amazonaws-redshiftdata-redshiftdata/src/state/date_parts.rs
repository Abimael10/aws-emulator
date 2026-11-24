// SPDX-License-Identifier: CC0-1.0

//! The DATE PART a Redshift date or time function takes as its first argument,
//! and the functions that take one.
//!
//! `DATEDIFF`, `DATEADD`, `DATE_PART` and `EXTRACT` are each written with a bare
//! date-part KEYWORD in that position — `datediff(microsecond, a, b)`,
//! `extract(dow from t)` — so the keyword is not a column reference and not a
//! value: it names the unit the function works in. The engine behind this
//! service carries neither the functions nor that vocabulary, so both live here:
//!
//! * [`PUBLISHED_DATE_PARTS`] is the vocabulary — every published spelling of
//!   every unit, with the unit it names;
//! * [`install`] registers the functions on every database connection, as real
//!   implementations computing over the instants they are given, at the
//!   MICROSECOND resolution a Redshift timestamp carries.
//!
//! The dialect reader ([`super::redshift_sql`]) is what turns the keyword into
//! the string these implementations read, so the keyword never reaches the
//! engine as an identifier.
//!
//! Source for the vocabulary and for which function admits which part: the
//! Amazon Redshift *Database Developer Guide*, "Date parts for date or timestamp
//! functions" — the table of date parts, their abbreviations and aliases, whose
//! day-of-week and day-of-year parts are the ones it admits for `DATE_PART` and
//! `EXTRACT` alone. Amazon Redshift is a PostgreSQL-derived server (see
//! [`super::redshift_sql`]), so the two refusals a unit can earn — one no
//! vocabulary names, and one that names no span for the function asking — are
//! worded as that server words them.

use ::chrono::{Datelike, Days, Months, NaiveDate, NaiveDateTime, TimeDelta, Timelike};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Error, Result};

/// The unit one date part names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Millennium,
    Century,
    Decade,
    Epoch,
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    /// The day of the week a date falls on, which names a FIELD of an instant
    /// and no span between two of them.
    DayOfWeek,
    /// The day of the year, which names a field for the same reason.
    DayOfYear,
}

impl Unit {
    /// Whether this unit names a SPAN between two instants, which is what
    /// `DATEDIFF` counts and what `DATEADD` moves an instant by.
    ///
    /// The day-of-week and day-of-year parts name a field of one instant and no
    /// span, which is why the published table admits them for `DATE_PART` and
    /// `EXTRACT` alone.
    fn spans(self) -> bool {
        !matches!(self, Self::DayOfWeek | Self::DayOfYear)
    }
}

/// One published date part: the unit it names and every spelling the published
/// table gives for it, lower-cased (a date part is a keyword, so it is matched
/// without regard to case).
#[derive(Debug, Clone, Copy)]
pub struct DatePart {
    /// The unit this part names.
    pub unit: Unit,
    /// Its published spellings: the name, its abbreviations and its aliases.
    pub spellings: &'static [&'static str],
}

/// Every date part this dialect publishes, in the order the published table
/// lists them (the largest unit first).
///
/// Source: the Amazon Redshift *Database Developer Guide*, "Date parts for date
/// or timestamp functions".
pub const PUBLISHED_DATE_PARTS: &[DatePart] = &[
    DatePart {
        unit: Unit::Millennium,
        spellings: &["millennium", "millennia", "mil", "mils"],
    },
    DatePart {
        unit: Unit::Century,
        spellings: &["century", "centuries", "c", "cent", "cents"],
    },
    DatePart {
        unit: Unit::Decade,
        spellings: &["decade", "decades", "dec", "decs"],
    },
    DatePart {
        unit: Unit::Epoch,
        spellings: &["epoch"],
    },
    DatePart {
        unit: Unit::Year,
        spellings: &["year", "years", "y", "yr", "yrs"],
    },
    DatePart {
        unit: Unit::Quarter,
        spellings: &["quarter", "quarters", "qtr", "qtrs"],
    },
    DatePart {
        unit: Unit::Month,
        spellings: &["month", "months", "mon", "mons"],
    },
    DatePart {
        unit: Unit::Week,
        spellings: &["week", "weeks", "w"],
    },
    DatePart {
        unit: Unit::DayOfWeek,
        spellings: &["dayofweek", "dow", "dw", "weekday"],
    },
    DatePart {
        unit: Unit::DayOfYear,
        spellings: &["dayofyear", "doy", "dy", "yearday"],
    },
    DatePart {
        unit: Unit::Day,
        spellings: &["day", "days", "d"],
    },
    DatePart {
        unit: Unit::Hour,
        spellings: &["hour", "hours", "h", "hr", "hrs"],
    },
    DatePart {
        unit: Unit::Minute,
        spellings: &["minute", "minutes", "m", "min", "mins"],
    },
    DatePart {
        unit: Unit::Second,
        spellings: &["second", "seconds", "s", "sec", "secs"],
    },
    DatePart {
        unit: Unit::Millisecond,
        spellings: &[
            "millisecond",
            "milliseconds",
            "ms",
            "msec",
            "msecs",
            "msecond",
            "mseconds",
            "millisec",
            "millisecs",
            "millisecon",
        ],
    },
    DatePart {
        unit: Unit::Microsecond,
        spellings: &[
            "microsecond",
            "microseconds",
            "microsec",
            "microsecs",
            "usecond",
            "useconds",
            "us",
            "usec",
            "usecs",
        ],
    },
];

/// The unit `keyword` names, or `None` for a word this vocabulary does not
/// publish — which is a word that is not a date part at all.
#[must_use]
pub fn unit_named(keyword: &str) -> Option<Unit> {
    let keyword = keyword.trim();
    PUBLISHED_DATE_PARTS
        .iter()
        .find(|part| {
            part.spellings
                .iter()
                .any(|spelling| spelling.eq_ignore_ascii_case(keyword))
        })
        .map(|part| part.unit)
}

/// Whether `keyword` names a date part the function `DATEDIFF`/`DATEADD` takes:
/// one that names a SPAN. The dialect reader asks this before it reads the
/// keyword as a date part, so a word those functions admit no part of is left
/// as the caller wrote it.
#[must_use]
pub fn names_a_span(keyword: &str) -> bool {
    unit_named(keyword).is_some_and(Unit::spans)
}

/// Register every function this module implements on `connection`.
///
/// Called from [`super::Database::open`], so each one is there before the
/// connection sees caller SQL. All three are deterministic: the same instants in
/// the same unit give the same answer.
pub fn install(connection: &Connection) -> Result<()> {
    let pure = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    // "DATEDIFF … returns the difference between the date parts of two date or
    // time expressions": the count of date-part boundaries from the second
    // argument to the third.
    connection.create_scalar_function("datediff", 3, pure, |ctx| {
        let (Some(unit), Some(from), Some(to)) = (
            part_argument(ctx, 0)?,
            instant_argument(ctx, 1)?,
            instant_argument(ctx, 2)?,
        ) else {
            return Ok(None);
        };
        if !unit.spans() {
            return Err(unsupported_unit(ctx, 0));
        }
        Ok(Some(span(unit, from, to)))
    })?;
    // "DATEADD … increments a date or time by a specified interval", the
    // interval being a count of the named date part.
    connection.create_scalar_function("dateadd", 3, pure, |ctx| {
        let (Some(unit), Some(count), Some(instant)) = (
            part_argument(ctx, 0)?,
            integer_argument(ctx, 1)?,
            instant_argument(ctx, 2)?,
        ) else {
            return Ok(None);
        };
        if !unit.spans() {
            return Err(unsupported_unit(ctx, 0));
        }
        let moved = advanced(unit, count, instant)
            .ok_or_else(|| failed("timestamp out of range".to_string()))?;
        Ok(Some(rendered(moved)))
    })?;
    // "DATE_PART … extracts date part values from an expression", which is the
    // same reading `EXTRACT(<datepart> FROM <expr>)` asks for — the dialect
    // reader re-spells that form as this call.
    connection.create_scalar_function("date_part", 2, pure, |ctx| {
        let (Some(unit), Some(instant)) = (part_argument(ctx, 0)?, instant_argument(ctx, 1)?)
        else {
            return Ok(None);
        };
        Ok(Some(field(unit, instant)))
    })?;
    Ok(())
}

/// How many `unit` boundaries lie between `from` and `to`, counted from `from`
/// to `to` — negative when `to` is the earlier instant.
///
/// Each instant is reduced to the ORDINAL of the `unit` it falls in and the two
/// ordinals are subtracted, which is what counting date-part boundaries is:
/// `DATEDIFF(year, '2008-12-31', '2009-01-01')` is one year, because the two
/// dates fall in adjacent years.
fn span(unit: Unit, from: NaiveDateTime, to: NaiveDateTime) -> i64 {
    ordinal(unit, to).saturating_sub(ordinal(unit, from))
}

/// Which `unit` an instant falls in, numbered so that two adjacent units differ
/// by one. The epoch's own instant is 0 in every unit.
fn ordinal(unit: Unit, instant: NaiveDateTime) -> i64 {
    let micros = microseconds(instant);
    let year = i64::from(instant.year());
    let month = i64::from(instant.month());
    match unit {
        // The calendar units, counted off the proleptic Gregorian calendar the
        // civil-calendar library holds: a millennium and a century are numbered
        // from year 1 (the 21st century starts in 2001), and a decade is the
        // year's own tens digit, which is how this server numbers them.
        Unit::Millennium => divided(year - 1, 1000) + 1,
        Unit::Century => divided(year - 1, 100) + 1,
        Unit::Decade => divided(year, 10),
        Unit::Year => year,
        Unit::Quarter => year * 4 + (month - 1) / 3,
        Unit::Month => year * 12 + (month - 1),
        // A week starts on a MONDAY, which is the day the ISO week this server
        // reports starts on; the epoch's own day was a Thursday, so the count
        // is taken from the Monday before it.
        Unit::Week => divided(divided(micros, MICROS_PER_DAY) + EPOCH_WEEKDAY_OFFSET, 7),
        Unit::Day => divided(micros, MICROS_PER_DAY),
        Unit::Hour => divided(micros, MICROS_PER_HOUR),
        Unit::Minute => divided(micros, MICROS_PER_MINUTE),
        // `epoch` is the count of SECONDS since the epoch, so a span counted in
        // it is a span of seconds.
        Unit::Second | Unit::Epoch => divided(micros, MICROS_PER_SECOND),
        Unit::Millisecond => divided(micros, MICROS_PER_MILLISECOND),
        Unit::Microsecond => micros,
        // Neither names a span; the callers refuse such a unit before asking.
        Unit::DayOfWeek | Unit::DayOfYear => 0,
    }
}

/// `instant` moved by `count` of `unit`, or `None` when the result is outside
/// the range of instants the calendar holds.
///
/// A month or a year is added as a CALENDAR step, so the day of the month is
/// held where it is and clamped to the last day of a shorter target month;
/// everything else is a fixed number of microseconds.
fn advanced(unit: Unit, count: i64, instant: NaiveDateTime) -> Option<NaiveDateTime> {
    let months = |months: i64| {
        let magnitude = u32::try_from(months.unsigned_abs()).ok()?;
        if months >= 0 {
            instant.checked_add_months(Months::new(magnitude))
        } else {
            instant.checked_sub_months(Months::new(magnitude))
        }
    };
    let days = |days: i64| {
        let magnitude = days.unsigned_abs();
        if days >= 0 {
            instant.checked_add_days(Days::new(magnitude))
        } else {
            instant.checked_sub_days(Days::new(magnitude))
        }
    };
    let micros = |micros: i64| {
        instant.checked_add_signed(TimeDelta::microseconds(count.checked_mul(micros)?))
    };
    match unit {
        Unit::Millennium => months(count.checked_mul(12_000)?),
        Unit::Century => months(count.checked_mul(1_200)?),
        Unit::Decade => months(count.checked_mul(120)?),
        Unit::Year => months(count.checked_mul(12)?),
        Unit::Quarter => months(count.checked_mul(3)?),
        Unit::Month => months(count),
        Unit::Week => days(count.checked_mul(7)?),
        Unit::Day => days(count),
        Unit::Hour => micros(MICROS_PER_HOUR),
        Unit::Minute => micros(MICROS_PER_MINUTE),
        Unit::Second | Unit::Epoch => micros(MICROS_PER_SECOND),
        Unit::Millisecond => micros(MICROS_PER_MILLISECOND),
        Unit::Microsecond => micros(1),
        Unit::DayOfWeek | Unit::DayOfYear => None,
    }
}

/// The value of one `unit` FIELD of `instant`, which is what `DATE_PART` and
/// `EXTRACT` report.
///
/// The composite fields follow the PostgreSQL-derived server Amazon Redshift is
/// built on: `second` is the whole seconds of the minute, `millisecond` and
/// `microsecond` are the seconds of the minute carried into that unit (so
/// `microsecond` of `…:30.123456` is 30 123 456), and `epoch` is the count of
/// seconds since 1970-01-01.
fn field(unit: Unit, instant: NaiveDateTime) -> i64 {
    let second = i64::from(instant.second());
    let subsecond = i64::from(instant.nanosecond() / 1_000) % MICROS_PER_SECOND;
    match unit {
        Unit::Millennium | Unit::Century | Unit::Decade | Unit::Year | Unit::Epoch => {
            ordinal(unit, instant)
        }
        Unit::Quarter => (i64::from(instant.month()) - 1) / 3 + 1,
        Unit::Month => i64::from(instant.month()),
        // "week … The number of the week of the calendar year that the day is
        // in", which is the ISO week number.
        Unit::Week => i64::from(instant.iso_week().week()),
        Unit::Day => i64::from(instant.day()),
        Unit::Hour => i64::from(instant.hour()),
        Unit::Minute => i64::from(instant.minute()),
        Unit::Second => second,
        Unit::Millisecond => second * MICROS_PER_MILLISECOND + subsecond / 1_000,
        Unit::Microsecond => second * MICROS_PER_SECOND + subsecond,
        // "dow … day of week, 0-6 starting with Sunday".
        Unit::DayOfWeek => i64::from(instant.weekday().num_days_from_sunday()),
        Unit::DayOfYear => i64::from(instant.ordinal()),
    }
}

/// Euclidean division: the quotient rounded DOWN, so an instant before the
/// epoch falls in the unit it really falls in rather than one toward zero.
fn divided(value: i64, divisor: i64) -> i64 {
    value.div_euclid(divisor)
}

/// How many microseconds separate `instant` from the epoch.
fn microseconds(instant: NaiveDateTime) -> i64 {
    instant.and_utc().timestamp_micros()
}

/// How many days the epoch's own day (a Thursday) lies after the Monday that
/// starts its week, which is what numbers the weeks a span is counted in.
const EPOCH_WEEKDAY_OFFSET: i64 = 3;

/// The microseconds in each fixed unit.
const MICROS_PER_MILLISECOND: i64 = 1_000;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;
const MICROS_PER_DAY: i64 = 24 * MICROS_PER_HOUR;

/// One argument as the date part it names, or an error for a word no published
/// date part spells. `None` for `NULL`, which makes every function here answer
/// `NULL` — the whole family is strict, as the functions it emulates are.
fn part_argument(ctx: &Context<'_>, index: usize) -> Result<Option<Unit>> {
    let Some(keyword) = text_argument(ctx, index) else {
        return Ok(None);
    };
    match unit_named(&keyword) {
        Some(unit) => Ok(Some(unit)),
        None => Err(failed(format!(
            "timestamp units \"{keyword}\" not recognized"
        ))),
    }
}

/// The refusal a function answers a date part it does not take: one the
/// vocabulary publishes, that names no span for this function.
fn unsupported_unit(ctx: &Context<'_>, index: usize) -> Error {
    let keyword = text_argument(ctx, index).unwrap_or_default();
    failed(format!("timestamp units \"{keyword}\" not supported"))
}

/// One argument as the instant it carries.
///
/// A timestamp is stored and written as text, in the `YYYY-MM-DD HH:MI:SS[.F]`
/// form this warehouse documents as its default `TIMEFORMAT`, with the `T`
/// separator and the trailing `Z` the ISO form carries and with a trailing UTC
/// offset where the value is a `TIMESTAMPTZ`; a `DATE` is the same form without
/// the time, and is midnight of that day. `None` for `NULL`; an error for text
/// that is no instant at all, worded as the server words it.
fn instant_argument(ctx: &Context<'_>, index: usize) -> Result<Option<NaiveDateTime>> {
    let Some(text) = text_argument(ctx, index) else {
        return Ok(None);
    };
    match instant(&text) {
        Some(instant) => Ok(Some(instant)),
        None => Err(failed(format!(
            "invalid input syntax for type timestamp: \"{text}\""
        ))),
    }
}

/// The instant a stored timestamp, timestamptz or date carries, or `None` for
/// text that is none of them.
fn instant(text: &str) -> Option<NaiveDateTime> {
    let trimmed = text.trim();
    // A trailing zone: the offset a `TIMESTAMPTZ` is written with, and the `Z`
    // the ISO form writes UTC as. The instants a cluster holds are UTC, so the
    // offset is read off and the civil time kept.
    let without_zone = trimmed
        .strip_suffix('Z')
        .or_else(|| zone_stripped(trimmed))
        .unwrap_or(trimmed);
    const TIMESTAMP_FORMATS: [&str; 4] = [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ];
    for format in TIMESTAMP_FORMATS {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(without_zone, format) {
            return Some(parsed);
        }
    }
    NaiveDate::parse_from_str(without_zone, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
}

/// The text before a trailing UTC offset (`+00`, `-05:30`), or `None` when the
/// text carries none.
fn zone_stripped(text: &str) -> Option<&str> {
    let (head, offset) = text.rsplit_once(['+', '-'])?;
    // A date's own hyphens are not an offset: an offset follows a TIME, so the
    // text before it has to carry one.
    if !head.contains(':') {
        return None;
    }
    let digits_and_colons = offset
        .chars()
        .all(|character| character.is_ascii_digit() || character == ':');
    (digits_and_colons && !offset.is_empty()).then(|| head.trim_end())
}

/// One instant as a stored timestamp is written: the `YYYY-MM-DD HH:MI:SS` form
/// this warehouse's default `TIMEFORMAT` names, carrying the fraction of a
/// second only when the instant has one — so a value this answers reads and
/// compares like a value the engine itself stored.
fn rendered(instant: NaiveDateTime) -> String {
    if instant.nanosecond() == 0 {
        return instant.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    instant.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// One argument as text, with this engine's implicit conversion of a number.
/// `None` for `NULL`.
fn text_argument(ctx: &Context<'_>, index: usize) -> Option<String> {
    match ctx.get_raw(index) {
        ValueRef::Null => None,
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        ValueRef::Integer(value) => Some(value.to_string()),
        ValueRef::Real(value) => Some(value.to_string()),
    }
}

/// One argument as a whole number of date parts. `None` for `NULL`; an error
/// for text that is no integer, worded as the server words it.
fn integer_argument(ctx: &Context<'_>, index: usize) -> Result<Option<i64>> {
    match ctx.get_raw(index) {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(value) => Ok(Some(value)),
        #[allow(clippy::cast_possible_truncation)]
        ValueRef::Real(value) => Ok(Some(value as i64)),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            text.trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|_error| failed(format!("invalid input syntax for integer: \"{text}\"")))
        }
    }
}

/// The engine error one of these functions reports: it could not produce a
/// value for the arguments it was given.
fn failed(message: String) -> Error {
    Error::UserFunctionError(message.into())
}
