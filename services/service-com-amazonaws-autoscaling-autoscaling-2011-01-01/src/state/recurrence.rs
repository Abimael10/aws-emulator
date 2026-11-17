// SPDX-License-Identifier: CC0-1.0

//! The recurring schedule a scheduled action states, and the next instant it fires at.
//!
//! "The recurring schedule for this action. This format consists of five fields separated by
//! white spaces: [Minute] [Hour] [Day_of_Month] [Month_of_Year] [Day_of_Week] […] For more
//! information about this format, see Crontab" and "Cron expressions use Universal
//! Coordinated Time (UTC) by default"
//! (`PutScheduledUpdateGroupActionType$Recurrence`), with
//! `PutScheduledUpdateGroupActionType$TimeZone` naming the zone one is read in: "Valid values
//! are the canonical names of the IANA time zones".
//!
//! So the grammar here is UNIX crontab's, which is NOT the six-field `cron(…)` grammar other
//! services of this emulator document ([`service_common::schedule_expression`]): five fields
//! rather than six, no year, the days of week numbered from Sunday as 0, and a restricted
//! day-of-month and day-of-week matched as EITHER rather than as both.
//!
//! The fields themselves are read by the maintained `cron` crate this workspace already
//! parses cron with, rather than by a grammar written here. That parser reads the six-field
//! form (seconds first) and numbers the days of week from Sunday as 1, so each Unix field is
//! stated in the spelling it reads: a literal `0` seconds field, and a day of week by NAME,
//! which is unambiguous in either numbering. The either-match is taken by asking it for both
//! readings of such an expression and keeping the earlier occurrence.
//!
//! The work is bounded by the LENGTH of the expression: `Recurrence` is
//! `XmlStringMaxLen255`, whose `@length` bound the validating walk enforces before a handler
//! reads it, and the occurrence is one parser step per reading.

use std::time::{Duration, SystemTime};

/// How many fields a `Recurrence` states, from the documentation quoted above.
pub const RECURRENCE_FIELDS: usize = 5;

/// The day-of-week names, indexed by the UNIX day-of-week number (Sunday as 0).
///
/// A day NAMED is the same day whichever numbering reads it, so naming each day a crontab
/// field selects is what carries that field into the parser's own numbering unchanged.
const DAY_OF_WEEK_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// The field a crontab expression leaves unrestricted.
const UNRESTRICTED: &str = "*";

/// The latest instant this module computes with, in seconds since the epoch: a schedule and
/// the instant it is computed from both reach it from a request, so every conversion is
/// bounded rather than trusted. 253 402 300 799 is 9999-12-31T23:59:59Z, the end of the
/// range a timestamp this service reports can name.
const MAX_TIMESTAMP_SECS: u64 = 253_402_300_799;

/// A recurrence this module read: the readings an occurrence of it is the earliest of.
///
/// One reading for an expression that restricts at most one of the day of month and the day
/// of week; two for one that restricts both, because crontab fires such an expression when
/// EITHER matches.
#[derive(Debug, Clone)]
pub struct Recurrence {
    readings: Vec<::cron::Schedule>,
}

impl Recurrence {
    /// The first instant this recurrence fires at strictly after `after`, read in `zone`.
    ///
    /// `None` when it names no further occurrence, or none this module can represent: the
    /// expression and the instant both come from a request, so every step is checked.
    #[must_use]
    pub fn next_occurrence(&self, after: SystemTime, zone: ::chrono_tz::Tz) -> Option<SystemTime> {
        use ::chrono::TimeZone;

        let after = zone.from_utc_datetime(&datetime_of(after)?.naive_utc());
        self.readings
            .iter()
            .filter_map(|reading| reading.after(&after).next())
            .filter_map(instant_of)
            .min()
    }
}

/// Read `recurrence` as the crontab expression the model documents, or `None` when it is not
/// one.
///
/// Every field is handed to the parser; what this adds is the FORM of the expression — the
/// field count the documentation states, the Unix day-of-week numbering, and the either-match
/// of a day of month beside a day of week.
#[must_use]
pub fn parse(recurrence: &str) -> Option<Recurrence> {
    let fields: Vec<&str> = recurrence.split_whitespace().collect();
    let [minute, hour, day_of_month, month, day_of_week] = fields.as_slice() else {
        return None;
    };
    let day_of_week = day_of_week_in_parser_spelling(day_of_week)?;
    // "The specification of days can be made in two fields: month day and weekday. If both
    // are specified in an entry, they are cumulative" (crontab): an expression restricting
    // both fires when either matches, which is the earlier of the two readings below.
    let readings = if *day_of_month == UNRESTRICTED || day_of_week == UNRESTRICTED {
        vec![reading(minute, hour, day_of_month, month, &day_of_week)?]
    } else {
        vec![
            reading(minute, hour, day_of_month, month, UNRESTRICTED)?,
            reading(minute, hour, UNRESTRICTED, month, &day_of_week)?,
        ]
    };
    Some(Recurrence { readings })
}

/// The zone a recurrence is read in: the one `TimeZone` names, or UTC when it names none.
///
/// `None` when the name is not one of "the canonical names of the IANA time zones", which is
/// what the member documents — the database those names are resolved against is the
/// maintained one this workspace already resolves them with.
#[must_use]
pub fn time_zone(name: Option<&str>) -> Option<::chrono_tz::Tz> {
    match name {
        None => Some(::chrono_tz::UTC),
        Some(name) => name.parse::<::chrono_tz::Tz>().ok(),
    }
}

/// One reading of an expression, in the six-field form the parser reads: the seconds field a
/// minute-resolution expression states as a literal `0`, then the five fields as given.
fn reading(
    minute: &str,
    hour: &str,
    day_of_month: &str,
    month: &str,
    day_of_week: &str,
) -> Option<::cron::Schedule> {
    format!("0 {minute} {hour} {day_of_month} {month} {day_of_week}")
        .parse::<::cron::Schedule>()
        .ok()
}

/// The day-of-week field as the EXPLICIT list of days it selects, named.
///
/// Crontab's own numbering (Sunday as 0, and 7 as Sunday again) is not the parser's (Sunday
/// as 1), and a crontab range may wrap the week — `5-7` is Friday to Sunday and `6-0` is
/// Saturday to Sunday — so rewriting the numbers in place would hand the parser a range that
/// runs backwards in ITS numbering and be refused for an expression crontab accepts. Each
/// item is therefore expanded here, over the week crontab numbers, into the days it selects,
/// and those days are NAMED: at most seven of them, in one spelling both numberings read the
/// same way.
///
/// `None` when an item names no day — a number past the week, an unknown day name, a step of
/// zero — so such an expression is not one this module reads.
fn day_of_week_in_parser_spelling(field: &str) -> Option<String> {
    if field == UNRESTRICTED {
        return Some(UNRESTRICTED.to_string());
    }
    let mut selected = [false; DAY_OF_WEEK_NAMES.len()];
    for item in field.split(',') {
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => (base, step.parse::<usize>().ok().filter(|step| *step > 0)?),
            None => (item, 1),
        };
        // The span the item covers, as crontab numbers it: a range states both ends, a step
        // over a single day runs from that day to the end of the week, and a bare day is
        // itself. `*` is every day.
        let (first, last) = match base {
            UNRESTRICTED => (0, DAY_OF_WEEK_NAMES.len() - 1),
            _ => match base.split_once('-') {
                Some((from, to)) => (crontab_day_of_week(from)?, crontab_day_of_week(to)?),
                None if step > 1 => (
                    crontab_day_of_week(base)?,
                    // A step over a bare day runs from that day to the END of the week
                    // crontab numbers — which counts 7 as Sunday again, so `5/2` is Friday
                    // and Sunday.
                    DAY_OF_WEEK_NAMES.len(),
                ),
                None => {
                    let day = crontab_day_of_week(base)?;
                    (day, day)
                }
            },
        };
        // A range whose end is before its start wraps the week, and one that spans the whole
        // week selects every day of it.
        let days = DAY_OF_WEEK_NAMES.len();
        let span = if last >= first {
            last - first
        } else {
            days - first + last
        }
        .min(days - 1);
        let mut offset = 0;
        while offset <= span {
            if let Some(day) = selected.get_mut((first + offset) % days) {
                *day = true;
            }
            offset += step;
        }
    }
    let named: Vec<&str> = DAY_OF_WEEK_NAMES
        .iter()
        .enumerate()
        .filter(|(day, _)| selected.get(*day).copied().unwrap_or(false))
        .map(|(_, name)| *name)
        .collect();
    (!named.is_empty()).then(|| named.join(","))
}

/// One day of the week as CRONTAB numbers it: a number in 0..=7 (0 and 7 both Sunday), or a
/// day name whichever case it is written in.
///
/// The seven-day week is numbered from Sunday, so a number past it names no day; `7` is kept
/// as seven rather than folded to zero, because it is the END of a range like `5-7` and
/// folding it would turn that range into Friday alone.
fn crontab_day_of_week(token: &str) -> Option<usize> {
    if token.is_empty() {
        return None;
    }
    if token.chars().all(|character| character.is_ascii_digit()) {
        return token
            .parse::<usize>()
            .ok()
            .filter(|number| *number <= DAY_OF_WEEK_NAMES.len());
    }
    let token = token.to_uppercase();
    DAY_OF_WEEK_NAMES
        .iter()
        .position(|name| *name == token.as_str())
}

/// `at` as a UTC date and time, or `None` when it is outside the range this module computes
/// in.
fn datetime_of(at: SystemTime) -> Option<::chrono::DateTime<::chrono::Utc>> {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs()
        .min(MAX_TIMESTAMP_SECS);
    ::chrono::DateTime::from_timestamp(i64::try_from(seconds).ok()?, 0)
}

/// A zoned date and time as a `SystemTime`, or `None` when it is outside that range.
fn instant_of<Tz: ::chrono::TimeZone>(at: ::chrono::DateTime<Tz>) -> Option<SystemTime> {
    let seconds = u64::try_from(at.timestamp()).ok()?;
    (seconds <= MAX_TIMESTAMP_SECS)
        .then(|| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{parse, time_zone, RECURRENCE_FIELDS};
    use std::time::{Duration, SystemTime};

    /// The instant `seconds` after the epoch, which every case below computes from.
    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// The seconds since the epoch of the next occurrence of `recurrence` after `from`, read
    /// in the zone `zone` names.
    fn next(recurrence: &str, zone: Option<&str>, from: u64) -> Option<u64> {
        let zone = time_zone(zone).expect("the case names a zone the database holds");
        parse(recurrence)?
            .next_occurrence(at(from), zone)
            .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|since_epoch| since_epoch.as_secs())
    }

    /// RULE: a crontab expression's next occurrence is the next instant it names, in the zone
    /// it is read in — whichever field carries the restriction, and with a restricted day of
    /// month beside a restricted day of week matched as EITHER, the way crontab states it.
    ///
    /// Every row counts from 1970-01-01T00:00:00Z, a Thursday, so the day each one lands on
    /// is a fact about the expression rather than about when the test ran.
    #[test]
    fn a_recurrence_names_its_next_occurrence() {
        // (case, recurrence, time zone, the expected occurrence in seconds since the epoch)
        let rows: &[(&str, &str, Option<&str>, u64)] = &[
            ("every minute", "* * * * *", None, 60),
            ("every hour, on the half hour", "30 * * * *", None, 1_800),
            ("every day at midnight", "0 0 * * *", None, 86_400),
            // 1970-01-02 is a Friday, so the first Friday after the epoch is one day on.
            (
                "a recurrence naming a day of the week, in the crontab numbering",
                "0 0 * * 5",
                None,
                86_400,
            ),
            ("every Friday, by name", "0 0 * * FRI", None, 86_400),
            // Sunday is 0 in crontab and 7 "on some systems": both name 1970-01-04.
            ("every Sunday, as 0", "0 0 * * 0", None, 3 * 86_400),
            ("every Sunday, as 7", "0 0 * * 7", None, 3 * 86_400),
            // A range crontab writes forwards but the parser's own numbering would read
            // backwards: Friday to Sunday, so the Friday one day on is the next occurrence.
            ("a range ending at Sunday as 7", "0 0 * * 5-7", None, 86_400),
            // ... and one that WRAPS the week: Saturday and Sunday, so the Saturday two
            // days on.
            ("a range wrapping the week", "0 0 * * 6-0", None, 2 * 86_400),
            // The whole week, written as a range across it.
            ("a range spanning the week", "0 0 * * 0-7", None, 86_400),
            // Every other day from Sunday: Sunday, Tuesday, Thursday, Saturday — so the
            // Saturday two days after the Thursday the epoch falls on.
            ("a step over the week", "0 0 * * */2", None, 2 * 86_400),
            ("a range of names", "0 0 * * MON-FRI", None, 86_400),
            ("a step over a range", "0 0 * * 1-5/2", None, 86_400),
            ("a list of days", "0 0 * * 2,5", None, 86_400),
            // The either-match: the 5th of the month, or any Friday — the Friday is sooner.
            (
                "a day of the month beside a day of the week",
                "0 0 5 * 5",
                None,
                86_400,
            ),
            // The same expression without the day of week waits for the 5th itself.
            ("the day of the month alone", "0 0 5 * *", None, 4 * 86_400),
            // A zone behind UTC reaches its own midnight after the epoch instant itself:
            // `Etc/GMT+9` is UTC-09:00, the sign convention of the IANA names the member's
            // own documentation cites.
            (
                "every day at midnight in a zone behind UTC",
                "0 0 * * *",
                Some("Etc/GMT+9"),
                9 * 3_600,
            ),
            // ... and one ahead of UTC (`Etc/GMT-9` is UTC+09:00) has already passed its own
            // midnight at the epoch instant, so it waits for the next one.
            (
                "every day at midnight in a zone ahead of UTC",
                "0 0 * * *",
                Some("Etc/GMT-9"),
                86_400 - 9 * 3_600,
            ),
            (
                "a list of months, as the member's own documentation writes one",
                "30 0 1 1,6,12 *",
                None,
                // 1970-01-01T00:30:00Z: the epoch instant is the first of January, and
                // half past midnight on it is still to come.
                1_800,
            ),
        ];

        for (case, recurrence, zone, expected) in rows {
            let reported = next(recurrence, *zone, 0);
            assert_eq!(
                reported,
                Some(*expected),
                "case {case:?}: expected the occurrence {expected} seconds after the epoch, \
                 got {reported:?}"
            );
        }
    }

    /// RULE: a step over a BARE day runs from that day to the end of the week crontab
    /// numbers, which counts 7 as Sunday again — so `5/2` selects Friday and Sunday.
    #[test]
    fn a_step_over_a_bare_day_runs_to_the_end_of_the_week() {
        // Counting from Saturday 1970-01-03T00:00:00Z, two days after the epoch.
        let saturday = 2 * 86_400;
        let reported = next("0 0 * * 5/2", None, saturday);
        assert_eq!(
            reported,
            Some(3 * 86_400),
            "expected the Sunday `5/2` also selects, got {reported:?}"
        );
        // The Friday alone waits for the next week's Friday.
        let reported = next("0 0 * * 5", None, saturday);
        assert_eq!(
            reported,
            Some(8 * 86_400),
            "expected the following Friday for the day alone, got {reported:?}"
        );
    }

    /// RULE: a value that is not a crontab expression of the documented shape states no
    /// schedule — whether it carries the wrong number of fields or a field no position reads.
    #[test]
    fn a_value_that_is_not_a_crontab_expression_states_no_schedule() {
        let rows: &[(&str, &str)] = &[
            ("empty", ""),
            ("one field short", "0 0 * *"),
            ("one field too many", "0 0 * * * *"),
            ("a minute that is not a minute", "sixty 0 * * *"),
            ("an hour out of range", "0 99 * * *"),
            ("a day of the month out of range", "0 0 99 * *"),
            ("a month that is not a month", "0 0 1 SMARCH *"),
            ("a day of the week out of range", "0 0 * * 9"),
            ("a day of the week that is not a day", "0 0 * * FUNDAY"),
            ("an AWS six-field cron(…) body", "0 10 * * ? *"),
        ];

        for (case, recurrence) in rows {
            assert!(
                parse(recurrence).is_none(),
                "case {case:?}: expected {recurrence:?} to state no schedule"
            );
        }
        // The documented field count is what the rows above are counted against.
        assert_eq!(
            RECURRENCE_FIELDS, 5,
            "the documented format states five fields"
        );
    }

    /// RULE: the zone a recurrence is read in is the one `TimeZone` names, UTC when it names
    /// none, and no zone at all when the name is not one the IANA database holds.
    #[test]
    fn a_time_zone_is_a_canonical_iana_name() {
        assert_eq!(
            time_zone(None),
            Some(::chrono_tz::UTC),
            "a recurrence naming no zone is read in UTC"
        );
        for name in ["UTC", "Etc/GMT+9", "Pacific/Tahiti", "Europe/Berlin"] {
            assert!(
                time_zone(Some(name)).is_some(),
                "case {name:?}: expected a canonical IANA name to resolve"
            );
        }
        for name in ["", "Mars/Olympus_Mons", "PST", "utc/+9"] {
            assert!(
                time_zone(Some(name)).is_none(),
                "case {name:?}: expected a name outside the database to resolve to no zone"
            );
        }
    }
}
