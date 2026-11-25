// SPDX-License-Identifier: CC0-1.0

//! The partitions a table PROJECTS rather than registers: what its `TBLPROPERTIES`
//! declare, and which partition values those properties admit.
//!
//! A table whose `projection.enabled` property is on has its partitions COMPUTED from the
//! projection properties — one per partition key, each declaring the values that key takes
//! (`projection.<key>.type`, with the `values`, `range` and `format` that kind of values is
//! stated by) — rather than read out of the catalog's partition records. So a prefix under
//! the table's location whose `<key>=<value>` path names a value the properties do not admit
//! is no partition of the table at all: a read does not scan it, however many objects are
//! physically there.
//!
//! What this module answers is exactly that: whether one partition's values are values the
//! declared projection produces. A property it cannot read — a type it gives no values for,
//! a range whose bounds it cannot read, a date format written in letters it translates no
//! field for — admits EVERY value for that key, so a projection this module does not
//! understand narrows nothing rather than dropping rows the statement asked for.

use ::std::collections::HashMap;

use ::chrono::{Months, NaiveDateTime, NaiveTime, TimeDelta, Utc};

use super::data::property;

/// The table property that says a table's partitions are projected: "to enable partition
/// projection, set `projection.enabled` to `true` in the table properties".
pub const PROJECTION_ENABLED: &str = "projection.enabled";

/// The prefix every per-key projection property carries, which is what makes the property
/// of one key `projection.<key>.<what>`.
const PROJECTION_PREFIX: &str = "projection";

/// The per-key property that says WHICH kind of values a key takes:
/// `projection.<key>.type`, one of [`TYPE_ENUM`], [`TYPE_INTEGER`], [`TYPE_DATE`] and
/// [`TYPE_INJECTED`].
const TYPE_PROPERTY: &str = "type";

/// The per-key property that lists an enumerated key's values: `projection.<key>.values`,
/// "a comma-separated list".
const VALUES_PROPERTY: &str = "values";

/// The per-key property that bounds an integer or date key: `projection.<key>.range`, "a
/// two-element comma-separated list providing the minimum and maximum range values".
const RANGE_PROPERTY: &str = "range";

/// The per-key property that says how a date key's values are WRITTEN:
/// `projection.<key>.format`, "a date format string" in the letter notation of the JVM's
/// datetime formatter.
const FORMAT_PROPERTY: &str = "format";

/// The values `projection.<key>.type` takes for a key whose values are listed one by one.
const TYPE_ENUM: &str = "enum";

/// The value for a key whose values are the whole numbers of a range.
const TYPE_INTEGER: &str = "integer";

/// The value for a key whose values are the dates of a range, written under a format.
const TYPE_DATE: &str = "date";

/// How the relative form of a date bound names the instant the range is relative to:
/// `NOW`, on its own or with an offset after it (`NOW-3DAYS`).
const RELATIVE_NOW: &str = "NOW";

/// The values ONE partition key takes, as its projection properties declare them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyProjection {
    /// The values listed, each exactly as the property wrote it.
    Enumerated(Vec<String>),
    /// The whole numbers of a closed range.
    Integers {
        /// The least value the key takes.
        from: i64,
        /// The greatest value the key takes.
        to: i64,
    },
    /// The dates of a closed range, written under a format.
    Dates {
        /// The format the key's values are written in, translated into the one
        /// [`instant_of`] reads them with.
        format: String,
        /// The earliest date the key takes.
        from: NaiveDateTime,
        /// The latest date the key takes.
        to: NaiveDateTime,
    },
    /// EVERY value: the key whose values the query supplies ([`TYPE_INJECTED`]), and one
    /// whose properties declare values this module cannot read.
    Any,
}

impl KeyProjection {
    /// Whether this key takes `value`.
    ///
    /// The comparison is the one the kind of values states: a listed value matches the text
    /// the property wrote, a number has to be a whole number of the range, and a date has to
    /// be a date of the range written under the declared format. Only the RANGE bounds the
    /// numbers and the dates: a declared interval would narrow them further, which is a
    /// narrowing this module does not make — the statement's own predicate still decides
    /// which rows a read answers with, so admitting a value the projection would step over
    /// changes no answer.
    #[must_use]
    pub fn admits(&self, value: &str) -> bool {
        match self {
            Self::Enumerated(values) => values.iter().any(|admitted| admitted == value),
            Self::Integers { from, to } => value
                .trim()
                .parse::<i64>()
                .is_ok_and(|number| number >= *from && number <= *to),
            Self::Dates { format, from, to } => instant_of(value, format)
                .is_some_and(|instant| instant >= *from && instant <= *to),
            Self::Any => true,
        }
    }
}

/// The projection the table properties `parameters` declare for the partition `keys`, in key
/// order, or `None` for a table whose partitions are NOT projected — whose partitions are
/// the ones the catalog records.
#[must_use]
pub fn declared(
    parameters: &HashMap<String, String>,
    keys: &[String],
) -> Option<Vec<KeyProjection>> {
    let enabled = property(parameters, PROJECTION_ENABLED)?;
    if !enabled.eq_ignore_ascii_case("true") {
        return None;
    }
    Some(keys.iter().map(|key| of_key(parameters, key)).collect())
}

/// The values the key `key` takes, as its own properties declare them.
fn of_key(parameters: &HashMap<String, String>, key: &str) -> KeyProjection {
    let Some(declared) = property(parameters, &key_property(key, TYPE_PROPERTY)) else {
        return KeyProjection::Any;
    };
    let kind = declared.to_ascii_lowercase();
    let read = match kind.as_str() {
        TYPE_ENUM => enumerated(parameters, key),
        TYPE_INTEGER => integers(parameters, key),
        TYPE_DATE => dates(parameters, key),
        // The `injected` type — "values are provided in the query itself" — names no value of
        // its own, and neither does a type this module does not read: both admit every value.
        _ => None,
    };
    read.unwrap_or(KeyProjection::Any)
}

/// The property `projection.<key>.<what>`, which is how one key's projection is declared.
fn key_property(key: &str, what: &str) -> String {
    format!("{PROJECTION_PREFIX}.{key}.{what}")
}

/// The values an ENUMERATED key takes: the entries of its `values` list, each trimmed of the
/// space around it.
fn enumerated(parameters: &HashMap<String, String>, key: &str) -> Option<KeyProjection> {
    let listed = property(parameters, &key_property(key, VALUES_PROPERTY))?;
    let values: Vec<String> = listed
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    (!values.is_empty()).then_some(KeyProjection::Enumerated(values))
}

/// The values an INTEGER key takes: the whole numbers of its declared range.
fn integers(parameters: &HashMap<String, String>, key: &str) -> Option<KeyProjection> {
    let (from, to) = bounds(parameters, key)?;
    let from: i64 = from.parse().ok()?;
    let to: i64 = to.parse().ok()?;
    (from <= to).then_some(KeyProjection::Integers { from, to })
}

/// The values a DATE key takes: the dates of its declared range, read under its declared
/// format.
///
/// A bound is written either as a date in that same format or RELATIVE to the current
/// instant ([`relative_bound`]), which is the form a range that moves with the clock is
/// declared in.
fn dates(parameters: &HashMap<String, String>, key: &str) -> Option<KeyProjection> {
    let declared = property(parameters, &key_property(key, FORMAT_PROPERTY))?;
    let format = chrono_format(declared)?;
    let (from, to) = bounds(parameters, key)?;
    let now = now();
    let from = date_bound(from, &format, now)?;
    let to = date_bound(to, &format, now)?;
    (from <= to).then_some(KeyProjection::Dates { format, from, to })
}

/// The two bounds a key's `range` property states, each trimmed.
fn bounds<'held>(
    parameters: &'held HashMap<String, String>,
    key: &str,
) -> Option<(&'held str, &'held str)> {
    let range = property(parameters, &key_property(key, RANGE_PROPERTY))?;
    let (from, to) = range.split_once(',')?;
    Some((from.trim(), to.trim()))
}

/// The current instant, as the relative form of a date bound is measured from.
fn now() -> NaiveDateTime {
    let instant: ::chrono::DateTime<Utc> = ::std::time::SystemTime::now().into();
    instant.naive_utc()
}

/// One bound of a date range: the date it names under `format`, or the instant its relative
/// form names from `now`.
fn date_bound(bound: &str, format: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let relative = bound
        .get(..RELATIVE_NOW.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(RELATIVE_NOW));
    if relative {
        return relative_bound(bound.get(RELATIVE_NOW.len()..).unwrap_or_default(), now);
    }
    instant_of(bound, format)
}

/// The instant a RELATIVE bound names: `now` shifted by the offset written after `NOW`, and
/// `now` itself where nothing is written after it.
///
/// The offset is a sign, a count and a unit (`-3DAYS`, `+1YEAR`), which are the units
/// Athena's own documentation states for such a bound; a unit this does not read names no
/// instant.
fn relative_bound(offset: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let offset = offset.trim();
    if offset.is_empty() {
        return Some(now);
    }
    let (sign, rest) = match offset.strip_prefix('+') {
        Some(rest) => (1i64, rest),
        None => (-1i64, offset.strip_prefix('-')?),
    };
    let counted = rest.trim_start();
    let digits: String = counted.chars().take_while(char::is_ascii_digit).collect();
    let count: i64 = digits.parse().ok()?;
    let unit = counted
        .get(digits.len()..)
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    let months = |count: i64| -> Option<u32> { u32::try_from(count).ok() };
    match unit.trim_end_matches('S') {
        "YEAR" => {
            let step = Months::new(months(count.saturating_mul(12))?);
            if sign > 0 {
                now.checked_add_months(step)
            } else {
                now.checked_sub_months(step)
            }
        }
        "MONTH" => {
            let step = Months::new(months(count)?);
            if sign > 0 {
                now.checked_add_months(step)
            } else {
                now.checked_sub_months(step)
            }
        }
        "WEEK" => TimeDelta::try_weeks(sign.saturating_mul(count))
            .and_then(|step| now.checked_add_signed(step)),
        "DAY" => TimeDelta::try_days(sign.saturating_mul(count))
            .and_then(|step| now.checked_add_signed(step)),
        "HOUR" => TimeDelta::try_hours(sign.saturating_mul(count))
            .and_then(|step| now.checked_add_signed(step)),
        "MINUTE" => TimeDelta::try_minutes(sign.saturating_mul(count))
            .and_then(|step| now.checked_add_signed(step)),
        "SECOND" => TimeDelta::try_seconds(sign.saturating_mul(count))
            .and_then(|step| now.checked_add_signed(step)),
        _ => None,
    }
}

/// The instant `value` names when read under the translated `format`, or `None` when the
/// format does not read the whole of it — which is text that is no value of this key at all.
///
/// A format that names no day, or no month either, still names a date: the FIRST of the
/// period it names, which is the instant a bound and a partition value written that way both
/// stand for. A format naming no time of day names the start of the day.
fn instant_of(value: &str, format: &str) -> Option<NaiveDateTime> {
    use ::chrono::format::{parse, Parsed, StrftimeItems};

    let mut parsed = Parsed::new();
    parse(&mut parsed, value.trim(), StrftimeItems::new(format)).ok()?;
    let mut date = parsed.to_naive_date();
    if date.is_err() {
        let _ = parsed.set_day(1);
        date = parsed.to_naive_date();
    }
    if date.is_err() {
        let _ = parsed.set_month(1);
        date = parsed.to_naive_date();
    }
    let time = parsed.to_naive_time().unwrap_or(NaiveTime::MIN);
    Some(date.ok()?.and_time(time))
}

/// `format`, written in the letter notation a `projection.<key>.format` property states a
/// date format in, translated into the format [`instant_of`] reads a value with.
///
/// Read through the ONE translation of that notation this workspace has
/// ([`::service_common::java_date_pattern`]), so a letter means here what it means wherever
/// else a caller-supplied datetime pattern is read; a format naming a letter it translates no
/// field for names nothing, because reading it as another field would admit different values
/// than the property declares. Only a format naming a DATE is one a date projection declares:
/// one that names only a time of day bounds no range of days.
fn chrono_format(format: &str) -> Option<String> {
    let translated = ::service_common::java_date_pattern::chrono_format(format)?;
    translated.date.then_some(translated.format)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the admission table: what the row is, the key's projection properties, the
    /// value offered to it, and whether the projection admits that value.
    type Admission<'c> = (&'c str, Vec<(&'c str, &'c str)>, &'c str, bool);
    /// The projection properties of one key, as a table's `TBLPROPERTIES` carry them.
    fn properties(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    /// A table that does NOT declare the projection switch projects no partitions at all,
    /// whatever else its properties say — the partitions are then the catalog's own.
    #[test]
    fn a_table_without_the_projection_switch_projects_nothing() {
        // (case, the properties)
        let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
            ("no property at all", vec![]),
            (
                "the switch turned off, with a range declared",
                vec![
                    ("projection.enabled", "false"),
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-02"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
            ),
        ];

        for (case, entries) in cases {
            let declared = declared(&properties(&entries), &["dt".to_string()]);
            assert!(
                declared.is_none(),
                "case {case:?}: expected no projection, got {declared:?}"
            );
        }
    }

    /// Which values a declared projection ADMITS, one row per kind of values a key takes.
    ///
    /// The rows differ along the kind (`projection.<key>.type`), the form of the bound or the
    /// list, and whether the value is inside what was declared — the two rows past each
    /// bound included.
    #[test]
    fn a_declared_projection_admits_the_values_it_states() {
        // The year the clock is in, which the row with a range relative to it is of.
        let this_year = now().format("%Y").to_string();
        // (case, the key's properties, the value, whether it is a partition of the table)
        let cases: Vec<Admission<'_>> = vec![
            (
                "a date inside the range",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "2024-01-05",
                true,
            ),
            (
                "the range's own first bound",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "2024-01-01",
                true,
            ),
            (
                "the range's own last bound",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "2024-01-31",
                true,
            ),
            (
                "the day before the range",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-02,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "2024-01-01",
                false,
            ),
            (
                "the day after the range",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "2024-02-01",
                false,
            ),
            (
                "a month of a range written to the month",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01,2024-03"),
                    ("projection.dt.format", "yyyy-MM"),
                ],
                "2024-02",
                true,
            ),
            (
                "a month past a range written to the month",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01,2024-03"),
                    ("projection.dt.format", "yyyy-MM"),
                ],
                "2024-04",
                false,
            ),
            (
                "an hour of a range written to the hour",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01-00,2024-01-01-12"),
                    ("projection.dt.format", "yyyy-MM-dd-HH"),
                ],
                "2024-01-01-06",
                true,
            ),
            (
                "a date that is no date of the declared format",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-01-01,2024-01-31"),
                    ("projection.dt.format", "yyyy-MM-dd"),
                ],
                "not-a-date",
                false,
            ),
            (
                "a range relative to the clock, which today is inside",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "NOW-1YEAR,NOW+1YEAR"),
                    ("projection.dt.format", "yyyy"),
                ],
                &this_year,
                true,
            ),
            (
                "a number inside the range",
                vec![
                    ("projection.dt.type", "integer"),
                    ("projection.dt.range", "1,12"),
                ],
                "7",
                true,
            ),
            (
                "a zero-padded number inside the range",
                vec![
                    ("projection.dt.type", "integer"),
                    ("projection.dt.range", "1,12"),
                    ("projection.dt.digits", "2"),
                ],
                "07",
                true,
            ),
            (
                "a number past the range",
                vec![
                    ("projection.dt.type", "integer"),
                    ("projection.dt.range", "1,12"),
                ],
                "13",
                false,
            ),
            (
                "a listed value",
                vec![
                    ("projection.dt.type", "enum"),
                    ("projection.dt.values", "eu, us, ap"),
                ],
                "us",
                true,
            ),
            (
                "a value the list does not carry",
                vec![
                    ("projection.dt.type", "enum"),
                    ("projection.dt.values", "eu, us, ap"),
                ],
                "sa",
                false,
            ),
            (
                "a key whose values the query supplies",
                vec![("projection.dt.type", "injected")],
                "anything-at-all",
                true,
            ),
            (
                "a date format written in letters no field is read for",
                vec![
                    ("projection.dt.type", "date"),
                    ("projection.dt.range", "2024-Q1,2024-Q3"),
                    ("projection.dt.format", "yyyy-QQQ"),
                ],
                "2024-Q4",
                true,
            ),
        ];

        for (case, entries, value, expected) in cases {
            let mut entries = entries;
            entries.push(("projection.enabled", "true"));
            let declared = declared(&properties(&entries), &["dt".to_string()])
                .unwrap_or_else(|| panic!("case {case:?}: expected a declared projection"));
            let actual = declared
                .first()
                .is_some_and(|projection| projection.admits(value));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the value {value:?} to be admitted={expected}, got \
                 {actual} from {declared:?}"
            );
        }
    }
}
