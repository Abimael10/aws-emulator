// SPDX-License-Identifier: CC0-1.0

//! The rules a schedule's own definition has to satisfy, and the normalisation
//! EventBridge Scheduler applies to the instants it stores.
//!
//! One concern, shared by the two operations that WRITE a schedule
//! (`CreateSchedule` and `UpdateSchedule`): both carry the same members, so both
//! are held to the same rules here rather than each spelling its own.
//!
//! * `ScheduleExpression` must state one of the three forms the service
//!   documents — "at expression - at(yyyy-mm-ddThh:mm:ss) rate expression -
//!   rate(value unit) cron expression - cron(fields)"
//!   (`CreateSchedule$ScheduleExpression`) — so an expression that states none of
//!   them, or states one of them malformed, is not a schedule at all
//!   ([`schedule_expression_violation`]).
//! * `ScheduleExpressionTimezone` names "the timezone in which the scheduling
//!   expression is evaluated" (`CreateSchedule$ScheduleExpressionTimezone`),
//!   which is a time zone of the IANA database: a value no zone answers to leaves
//!   the firing instants undefined, so it is not accepted
//!   ([`schedule_expression_timezone_violation`]).
//! * `StartDate` is "the date, in UTC, after which the schedule can begin
//!   invoking its target" and `EndDate` "the date, in UTC, before which the
//!   schedule can invoke its target" (`CreateSchedule$StartDate` /
//!   `$EndDate`): together they are an interval, and an interval that holds no
//!   instant is a schedule that can never fire ([`schedule_window_violation`]).
//! * an expression the service CAN read still has to name an occurrence inside
//!   the window it is read with: one whose every occurrence falls outside the
//!   span this service can name an instant in invokes its target never, which is
//!   a condition of its own and worded apart from a malformed expression
//!   ([`schedule_occurrence_violation`]).
//! * both instants are stored at MILLISECOND resolution, at the NEAREST
//!   millisecond to the instant the request stated
//!   ([`to_millisecond_precision`]), which is the resolution a schedule's
//!   timestamps are reported back at.
//!
//! The expression grammars are read by maintained parsers — the `cron` crate for
//! a `cron(...)` body and `chrono` for an `at(...)` instant — rather than by a
//! hand-written one, and a zone name by `chrono-tz`, which carries the IANA
//! database.
//!
//! The tables and the normalisation that reading needs ([`RATE_UNITS`],
//! [`CRON_FIELDS`], [`AT_EXPRESSION_FORMAT`], [`normalised_cron`]) are declared
//! here ONCE and used by [`super::occurrence`] as well, which computes the instants
//! an accepted expression names: the validator and the evaluator have to read one
//! grammar, or an expression this accepts is one the clock names no instant for.

use std::time::SystemTime;

use ::aws_smithy_types::DateTime;

/// The units a `rate(value unit)` expression may name, and what each is worth in
/// seconds: "rate(value unit) […] unit: The unit of time. Required: Yes. Values:
/// minute | minutes | hour | hours | day | days" (the service's schedule-types
/// reference). Both spellings of each unit are accepted, as the service accepts
/// them.
pub(crate) const RATE_UNITS: &[(&str, u64)] = &[
    ("minute", 60),
    ("minutes", 60),
    ("hour", 3_600),
    ("hours", 3_600),
    ("day", 86_400),
    ("days", 86_400),
];

/// How many whitespace-separated fields a `cron(...)` body carries: "cron(fields)
/// […] Minutes Hours Day-of-month Month Day-of-week Year" — six.
pub(crate) const CRON_FIELDS: usize = 6;

/// The indexes of the two fields a `?` ("no specific value") is a statement in —
/// day-of-month and day-of-week — which is the same statement `*` makes in the
/// grammar the `cron` crate reads.
pub(crate) const NO_SPECIFIC_VALUE_FIELDS: [usize; 2] = [2, 4];

/// The field value that states "no specific value".
const NO_SPECIFIC_VALUE: &str = "?";

/// The index of the YEAR field among the six the format documents.
const YEAR_FIELD: usize = 5;

/// The years a `cron(...)` year field may name: "Year […] 1970-2199" (the
/// wildcards-and-values table of the service's schedule-types reference).
const CRON_YEARS: (i32, i32) = (1970, 2199);

/// The last year this service can name an occurrence AT: the ceiling of the year
/// the shared grammar reads ([`cron_field_parses`] refuses the year after it),
/// which is therefore the end of the lookahead the validator and the evaluator
/// ([`super::occurrence`]) share.
///
/// An expression whose years all fall past it names occurrences this service
/// could never perform — the condition [`schedule_occurrence_violation`] refuses
/// — while the year field itself is still one the published grammar admits
/// ([`CRON_YEARS`]), so such an expression is well-formed rather than malformed.
const LAST_FEASIBLE_YEAR: i32 = 2100;

/// The field values the normalised seven-field expression carries for every field
/// but the one under test, so a field is judged on its own.
const NEUTRAL_CRON_FIELDS: [&str; CRON_FIELDS + 1] = ["0", "0", "0", "*", "*", "*", "*"];

/// The wildcards the service's own cron dialect documents on top of the ones the
/// `cron` crate's grammar reads: `L` (last), `W` (weekday) and `#` (nth weekday
/// of the month), from the wildcards table of the service's schedule-types
/// reference. A field that carries one of them states something the shared parser
/// cannot read, so it is left to the service's grammar rather than refused for a
/// grammar it does not use.
const SERVICE_CRON_WILDCARDS: [char; 3] = ['L', 'W', '#'];

/// The format an `at(...)` body states its instant in:
/// "at(yyyy-mm-ddThh:mm:ss)" — a local date and time with no zone offset, since
/// the zone is `ScheduleExpressionTimezone`'s to state.
pub(crate) const AT_EXPRESSION_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// A nanosecond count of one millisecond: the resolution a schedule's instants
/// are held at.
const NANOS_PER_MILLISECOND: u32 = 1_000_000;

/// `at` instant, `rate` period or `cron` fields: the three forms a
/// `ScheduleExpression` may state, keyed by the keyword that introduces each.
///
/// The keyword decides which grammar reads the body, so an expression that
/// states none of the three keywords states no schedule at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionForm {
    At,
    Rate,
    Cron,
}

/// The form `expression` states and the body it carries, or `None` when it states
/// no form this service defines.
fn form_of(expression: &str) -> Option<(ExpressionForm, &str)> {
    for (keyword, form) in [
        ("at", ExpressionForm::At),
        ("rate", ExpressionForm::Rate),
        ("cron", ExpressionForm::Cron),
    ] {
        if let Some(body) = expression
            .strip_prefix(keyword)
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return Some((form, body));
        }
    }
    None
}

/// How this service states an expression that is not a schedule: the class of the
/// failure, with the value the request carried, which is the only place the
/// service reports the rejected expression back.
fn invalid_expression(expression: &str) -> String {
    format!("Invalid Schedule Expression {expression}.")
}

/// Refuse a `ScheduleExpression` that states none of the three documented forms,
/// or states one of them malformed; `None` for an expression the service can read.
///
/// The length bound (1..256) is modelled, so the boundary validator has already
/// refused anything outside it by the time a handler runs.
pub fn schedule_expression_violation(expression: &str) -> Option<String> {
    let readable = match form_of(expression) {
        Some((ExpressionForm::At, body)) => at_expression_states_an_instant(body),
        Some((ExpressionForm::Rate, body)) => rate_expression_states_a_period(body),
        Some((ExpressionForm::Cron, body)) => cron_expression_states_its_instants(body),
        None => false,
    };
    (!readable).then(|| invalid_expression(expression))
}

/// Whether an `at(...)` body states an instant in the documented format.
fn at_expression_states_an_instant(body: &str) -> bool {
    ::chrono::NaiveDateTime::parse_from_str(body.trim(), AT_EXPRESSION_FORMAT).is_ok()
}

/// Whether a `rate(...)` body states a period: a positive integer of a unit the
/// service names ("value: The positive number […] unit: minute | minutes | hour |
/// hours | day | days"), and nothing else.
fn rate_expression_states_a_period(body: &str) -> bool {
    let mut fields = body.split_whitespace();
    let (Some(value), Some(unit), None) = (fields.next(), fields.next(), fields.next()) else {
        return false;
    };
    let states_a_count = value.parse::<u64>().is_ok_and(|value| value > 0);
    states_a_count && RATE_UNITS.iter().any(|(name, _)| *name == unit)
}

/// Whether a `cron(...)` body states the instants it has to.
fn cron_expression_states_its_instants(body: &str) -> bool {
    cron_fields(body).is_some()
}

/// The six fields of a `cron(...)` body this service can read, or `None` for a body
/// that states no instants at all.
///
/// A body is read when it carries the six documented fields, each field states
/// values that field admits ([`cron_field_states_its_values`]), the two DAY fields
/// state between them exactly one specific day ([`one_day_field_is_specific`]), and
/// the fields the shared grammar reads combine into an expression it accepts.
///
/// The `cron` crate reads the seven-field form (seconds first), so a literal `0`
/// seconds field is prepended, and a `?` in the day-of-month or day-of-week field
/// is the `*` that grammar spells the same statement with. A field carrying one of
/// the service's own wildcards ([`SERVICE_CRON_WILDCARDS`]) is a statement that
/// grammar does not read at all, so such a body is read on the service's grammar
/// instead of being refused on the parser's — the CROSS-FIELD rules below still
/// hold for it, since they are the service's own and not the parser's.
fn cron_fields(body: &str) -> Option<Vec<&str>> {
    let fields: Vec<&str> = body.split_whitespace().collect();
    if fields.len() != CRON_FIELDS {
        return None;
    }
    if !one_day_field_is_specific(&fields) {
        return None;
    }
    for (index, field) in fields.iter().enumerate() {
        if !cron_field_states_its_values(index, field) {
            return None;
        }
    }
    // Every field reads on its own; the fields the shared grammar reads are read
    // once more TOGETHER, so a combination of them that grammar refuses is refused
    // too. A body in the service's own dialect is left out of that reading, which
    // that grammar cannot perform; the YEAR stands at `*` for it, because the years
    // a field names are read on their own ([`cron_years`]) past the ceiling that
    // grammar's own year ordinal stops at.
    if fields.iter().any(|field| names_service_wildcard(field)) {
        return Some(fields);
    }
    let mut combined = fields.clone();
    if let Some(year) = combined.get_mut(YEAR_FIELD) {
        *year = "*";
    }
    normalised_cron(&combined)
        .parse::<::cron::Schedule>()
        .ok()
        .map(|_| fields)
}

/// Whether `field` states values the field at `index` admits, judged on that field
/// alone.
///
/// Every field but the YEAR is read by the shared grammar in the place it stands
/// ([`cron_field_parses`]), or carries one of the service's own wildcards. The year
/// is read here instead ([`cron_years`]): that grammar's year ordinal stops at
/// [`LAST_FEASIBLE_YEAR`], while the field itself admits every year the service
/// publishes ([`CRON_YEARS`]) — a year past the ceiling is a year the service reads
/// and names no feasible occurrence at, not a value it cannot read.
fn cron_field_states_its_values(index: usize, field: &str) -> bool {
    if index == YEAR_FIELD {
        return cron_years(field).is_some();
    }
    cron_field_parses(index, field) || names_service_wildcard(field)
}

/// Whether the two DAY fields state between them exactly one specific day.
///
/// The cron form carries both a day-of-month and a day-of-week field, and one of
/// the two must state "no specific value" (`?`): an expression that states a day in
/// BOTH names instants the form does not define, and one that states `?` in both
/// names no day at all — so exactly one of them carries the `?`.
fn one_day_field_is_specific(fields: &[&str]) -> bool {
    NO_SPECIFIC_VALUE_FIELDS
        .iter()
        .filter(|index| fields.get(**index) == Some(&NO_SPECIFIC_VALUE))
        .count()
        == 1
}

/// The years one `cron(...)` YEAR field names, as the `(first, last, step)` terms it
/// is written in, or `None` for a field that names no years the service publishes.
///
/// The field's grammar is the shared one — a comma-separated list of terms, each a
/// year, a range of years or `*`, any of them over a step — read over
/// [`CRON_YEARS`]. It is read HERE rather than by the shared parser because that
/// parser's year ordinal stops at [`LAST_FEASIBLE_YEAR`], so it cannot represent the
/// later years this field still admits.
fn cron_years(field: &str) -> Option<Vec<(i32, i32, i32)>> {
    let mut terms = Vec::new();
    for term in field.split(',') {
        let (values, step) = match term.split_once('/') {
            Some((values, step)) => (values, step.parse::<i32>().ok().filter(|by| *by > 0)?),
            None => (term, 1),
        };
        let (first, last) = if values == "*" {
            CRON_YEARS
        } else if let Some((first, last)) = values.split_once('-') {
            (published_year(first)?, published_year(last)?)
        } else {
            let first = published_year(values)?;
            // A year over a step opens a series that runs to the end of the range;
            // a year on its own is that year.
            match term.contains('/') {
                true => (first, CRON_YEARS.1),
                false => (first, first),
            }
        };
        if first > last {
            return None;
        }
        terms.push((first, last, step));
    }
    (!terms.is_empty()).then_some(terms)
}

/// `text` as one of the years the service publishes, or `None` for anything else.
fn published_year(text: &str) -> Option<i32> {
    let year: i32 = text.parse().ok()?;
    (CRON_YEARS.0..=CRON_YEARS.1).contains(&year).then_some(year)
}

/// Whether `terms` name any year in `from..=to`.
///
/// The arithmetic is saturating: a step is a figure the REQUEST wrote, so a term may
/// state one no span of years could hold, and a saturated step names no further year
/// rather than running past what the count can hold.
fn years_reach(terms: &[(i32, i32, i32)], from: i32, to: i32) -> bool {
    terms.iter().any(|(first, last, step)| {
        let opens = (*first).max(from);
        // The term's own series: the first year it names at or after the floor is
        // the one its step lands on.
        let past = opens.saturating_sub(*first).rem_euclid(*step);
        let named = match past {
            0 => opens,
            past => opens.saturating_add(step.saturating_sub(past)),
        };
        named <= (*last).min(to)
    })
}

/// Whether `field` carries one of the wildcards this service's cron dialect
/// documents and the shared parser's grammar does not read.
fn names_service_wildcard(field: &str) -> bool {
    SERVICE_CRON_WILDCARDS
        .iter()
        .any(|wildcard| field.contains(*wildcard))
}

/// How this service states an expression it CAN read that will nonetheless never
/// invoke the schedule's target: the never-fires class of the failure, naming the
/// expression and the window it was read with.
///
/// Worded apart from [`invalid_expression`] because the two are different
/// conditions: an unreadable expression is a syntax failure a caller fixes by
/// rewriting the expression, while this one is a statement about the expression and
/// the window TOGETHER — they have no instant in common.
fn never_schedules_an_invocation(
    expression: &str,
    start_date: Option<DateTime>,
    end_date: Option<DateTime>,
) -> String {
    format!(
        "The schedule expression {expression}, with StartDate {} and EndDate {}, is invalid \
         because it will never schedule an invocation.",
        echoed_instant(start_date),
        echoed_instant(end_date)
    )
}

/// What the never-fires refusal states for a window boundary the request left out.
const ABSENT_INSTANT: &str = "null";

/// One of the window's boundaries as this service echoes it in a sentence: the UTC
/// date and time it names, carried only to the resolution it states something at —
/// no zone suffix, no seconds on a whole minute and no remainder on a whole second
/// — and [`ABSENT_INSTANT`] for a boundary the request did not state.
///
/// The instant echoed is the one the service HOLDS ([`to_millisecond_precision`]),
/// so the sentence states the same boundary a later read of the schedule would.
fn echoed_instant(at: Option<DateTime>) -> String {
    let Some(at) = at.map(to_millisecond_precision) else {
        return ABSENT_INSTANT.to_string();
    };
    let Some(utc) = utc_instant(at) else {
        return ABSENT_INSTANT.to_string();
    };
    let naive = utc.naive_utc();
    let minutes = naive.format("%Y-%m-%dT%H:%M").to_string();
    let seconds = ::chrono::Timelike::second(&naive);
    let milliseconds = ::chrono::Timelike::nanosecond(&naive) / NANOS_PER_MILLISECOND;
    match (seconds, milliseconds) {
        (0, 0) => minutes,
        (seconds, 0) => format!("{minutes}:{seconds:02}"),
        (seconds, milliseconds) => format!("{minutes}:{seconds:02}.{milliseconds:03}"),
    }
}

/// One wire instant as the UTC date and time it names, or `None` for one no
/// calendar reading of this clock covers.
fn utc_instant(at: DateTime) -> Option<::chrono::DateTime<::chrono::Utc>> {
    ::chrono::DateTime::from_timestamp(at.secs(), at.subsec_nanos())
}

/// The calendar year `at` falls in, read in UTC — the years a schedule's window
/// opens in, which is the floor the feasible lookahead is measured from.
fn year_of(at: ::chrono::DateTime<::chrono::Utc>) -> i32 {
    ::chrono::Datelike::year(&at)
}

/// Refuse a readable `ScheduleExpression` that names no occurrence the schedule
/// could ever be invoked at; `None` for one with an occurrence ahead of it.
///
/// A cron expression's YEAR field states which years its instants fall in, so an
/// expression none of whose years falls inside the schedule's window — behind the
/// instant it opens at, past the instant it closes at, or past the end of the
/// lookahead this service can name an instant in ([`LAST_FEASIBLE_YEAR`]) — has no
/// occurrence in common with that window: the schedule would invoke its target
/// never, which is refused when it is WRITTEN rather than stored as a schedule
/// nothing will ever be delivered for.
///
/// The window opens at `StartDate` when the request carries one and at the instant
/// of the request otherwise, which is the same opening the empty-window rule reads
/// it at ([`schedule_window_violation`]), and closes at `EndDate` when the request
/// carries one — both boundaries, the two the sentence states. The YEAR is the
/// resolution the test is taken at: a year the window admits any part of still
/// holds the instants of that year the expression names, so only a year wholly
/// outside the window is ruled out. An expression the service cannot read at all is
/// the malformed class instead ([`schedule_expression_violation`]), and a
/// `rate(...)` names an occurrence a period on from wherever its series is started,
/// so neither is this rule's.
pub fn schedule_occurrence_violation(
    expression: &str,
    start_date: Option<DateTime>,
    end_date: Option<DateTime>,
    now: SystemTime,
) -> Option<String> {
    let Some((ExpressionForm::Cron, body)) = form_of(expression) else {
        return None;
    };
    let fields = cron_fields(body)?;
    let years = cron_years(fields.get(YEAR_FIELD)?)?;
    let opens = year_of(::chrono::DateTime::<::chrono::Utc>::from(now));
    let opens = match start_date.and_then(utc_instant) {
        Some(start) => opens.max(year_of(start)),
        None => opens,
    };
    let closes = match end_date.and_then(utc_instant) {
        Some(end) => LAST_FEASIBLE_YEAR.min(year_of(end)),
        None => LAST_FEASIBLE_YEAR,
    };
    (!years_reach(&years, opens, closes))
        .then(|| never_schedules_an_invocation(expression, start_date, end_date))
}

/// The six documented fields as the seven-field expression the `cron` crate reads.
pub(crate) fn normalised_cron(fields: &[&str]) -> String {
    let mut expression = String::from(NEUTRAL_CRON_FIELDS[0]);
    for (index, field) in fields.iter().enumerate() {
        expression.push(' ');
        expression.push_str(cron_field_as_parsed(index, field));
    }
    expression
}

/// One field as the `cron` crate's grammar reads it: `?` ("no specific value") is
/// `*` in that grammar, in the two fields it is a statement in.
fn cron_field_as_parsed(index: usize, field: &str) -> &str {
    if field == "?" && NO_SPECIFIC_VALUE_FIELDS.contains(&index) {
        "*"
    } else {
        field
    }
}

/// Whether `field` is one the cron grammar reads in the place `index` names,
/// judged by parsing it there with every other field neutral — so the answer is
/// about that field and not about the rest of the expression.
fn cron_field_parses(index: usize, field: &str) -> bool {
    let mut expression = NEUTRAL_CRON_FIELDS;
    expression[index + 1] = cron_field_as_parsed(index, field);
    expression
        .join(" ")
        .parse::<::cron::Schedule>()
        .is_ok()
}

/// How this service states a time zone no IANA zone answers to: the zone class of
/// the failure, with the value the request carried, which is the only place the
/// service reports WHICH member it rejected — so it is worded apart from the
/// expression class ([`invalid_expression`]).
fn invalid_timezone(timezone: &str) -> String {
    format!("Invalid timezone {timezone}.")
}

/// Refuse a `ScheduleExpressionTimezone` that names no time zone; `None` for an
/// absent member (the expression is then evaluated in UTC) or a zone the IANA
/// database carries.
///
/// The length bound (1..50) is modelled, so the boundary validator has already
/// refused anything outside it.
pub fn schedule_expression_timezone_violation(timezone: Option<&str>) -> Option<String> {
    let timezone = timezone?;
    timezone
        .parse::<::chrono_tz::Tz>()
        .err()
        .map(|_| invalid_timezone(timezone))
}

/// How this service states an EMPTY invocation window, whichever boundary the
/// request stated it with: the window has ONE violation — the instant it opens at
/// does not come before the instant it closes at — so it has one sentence, and the
/// sentence carries nothing from the request or from state. A request stating only
/// an `EndDate` is held to the same text as one stating both boundaries, since the
/// start it is judged against is implicit rather than different.
const START_DATE_BEFORE_END_DATE: &str = "The StartDate you specify must come before the EndDate.";

/// Refuse a `StartDate`/`EndDate` pair that names an EMPTY invocation window —
/// one holding no instant at which the schedule could invoke its target.
///
/// The window opens at `StartDate` ("after which the schedule can begin invoking
/// its target") when the request carries one, and otherwise at the instant of the
/// request, since nothing bounds it earlier; it closes at `EndDate` ("before
/// which the schedule can invoke its target"). So a pair whose start is after its
/// end is refused, and an end already behind the clock is refused when no start
/// was given — one window rule, stated by [`START_DATE_BEFORE_END_DATE`] either
/// way.
pub fn schedule_window_violation(
    start_date: Option<DateTime>,
    end_date: Option<DateTime>,
    now: SystemTime,
) -> Option<String> {
    let end_date = end_date?;
    let holds_no_instant = match start_date {
        Some(start_date) => instant(start_date) > instant(end_date),
        None => instant(end_date) <= instant(DateTime::from(now)),
    };
    holds_no_instant.then(|| START_DATE_BEFORE_END_DATE.to_string())
}

/// One wire instant as the pair that orders it: `DateTime` carries no ordering of
/// its own, and a second count with its sub-second remainder compares exactly.
fn instant(at: DateTime) -> (i64, u32) {
    (at.secs(), at.subsec_nanos())
}

/// A nanosecond count of one second, which a rounded-up remainder carries into.
const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// One of a schedule's instants at the resolution the service holds it: the
/// instant at the NEAREST whole millisecond.
///
/// EventBridge Scheduler reports `StartDate`/`EndDate` at millisecond resolution,
/// and the instant it reports is the one the request stated: a client that writes a
/// schedule and reads it back sees the millisecond it submitted.
///
/// The nearest millisecond, rather than the one below, is what answers the instant
/// submitted: a timestamp travels in this protocol as DECIMAL epoch seconds and is
/// read through a binary floating-point number, which represents almost no decimal
/// remainder exactly — a submitted `.12` arrives a few hundred nanoseconds short of
/// 120 ms, so discarding the remainder would report a whole millisecond less than
/// was accepted. That error is a fraction of a microsecond — orders of magnitude
/// below the millisecond the instant is held at — so the nearest millisecond is the
/// one the request stated.
pub fn to_millisecond_precision(at: DateTime) -> DateTime {
    let whole_milliseconds = at
        .subsec_nanos()
        .saturating_add(NANOS_PER_MILLISECOND / 2)
        / NANOS_PER_MILLISECOND;
    let remainder = whole_milliseconds.saturating_mul(NANOS_PER_MILLISECOND);
    // A remainder that rounds up to a whole second is that next second, with no
    // remainder left: the instant moves, the resolution does not.
    if remainder >= NANOS_PER_SECOND {
        DateTime::from_secs_and_nanos(at.secs().saturating_add(1), 0)
    } else {
        DateTime::from_secs_and_nanos(at.secs(), remainder)
    }
}

#[cfg(test)]
mod tests {
    //! The rules this module owns, each over a SET of inputs rather than one
    //! recorded expression. The same rules are driven through real requests in
    //! `tests/all/schedule_definition_rules.rs`.

    use super::*;

    /// Every documented form, well-formed and malformed, is read as the service
    /// documents it.
    #[test]
    fn an_expression_states_one_of_the_three_documented_forms() {
        // (case, expression, states a schedule)
        let rows: [(&str, &str, bool); 26] = [
            ("a rate of minutes", "rate(5 minutes)", true),
            ("a rate of one minute", "rate(1 minute)", true),
            // A singular unit with a plural count, and the reverse, are both
            // forms the unit set carries.
            ("a plural unit with one", "rate(1 minutes)", true),
            ("a singular unit with five", "rate(5 minute)", true),
            ("a rate of hours", "rate(12 hours)", true),
            ("a rate of days", "rate(1 day)", true),
            ("a rate of zero", "rate(0 minutes)", false),
            ("a rate of nothing", "rate(bogus)", false),
            ("a rate with no unit", "rate(5)", false),
            ("a rate in a unit nothing measures", "rate(5 fortnights)", false),
            ("a negative rate", "rate(-5 minutes)", false),
            ("a fractional rate", "rate(1.5 hours)", false),
            ("a rate with a trailing field", "rate(5 minutes please)", false),
            ("a six-field cron", "cron(0 8 1 * ? *)", true),
            ("a cron naming weekdays", "cron(0 18 ? * MON-FRI *)", true),
            ("a cron over a step", "cron(0/15 * * * ? *)", true),
            // The wildcards the service's own dialect documents.
            ("a cron on the last day of the month", "cron(0 8 L * ? *)", true),
            ("a cron on the nearest weekday", "cron(0 8 15W * ? *)", true),
            ("a cron on the second Tuesday", "cron(0 8 ? * 3#2 *)", true),
            ("a cron of five fields", "cron(0 8 1 * ?)", false),
            ("a cron of prose", "cron(not a cron)", false),
            ("a cron with a field nothing parses", "cron(0 8 1 * QQ *)", false),
            ("an empty cron", "cron()", false),
            ("an at instant", "at(2027-01-01T00:00:00)", true),
            ("an at instant with a zone", "at(2027-01-01T00:00:00Z)", false),
            ("no form at all", "every 5 minutes", false),
        ];

        for (case, expression, states_a_schedule) in rows {
            let violation = schedule_expression_violation(expression);
            assert_eq!(
                violation.is_none(),
                states_a_schedule,
                "case {case:?}: expected states_a_schedule={states_a_schedule}, got \
                 {violation:?}"
            );
            if let Some(message) = violation {
                assert!(
                    message.contains(expression),
                    "case {case:?}: the refusal states the expression the request \
                     carried, got {message:?}"
                );
            }
        }
    }

    /// A time zone is one the IANA database carries, and an absent member is no
    /// violation at all.
    #[test]
    fn a_timezone_names_an_iana_zone() {
        // (case, timezone, names a zone)
        let rows: [(&str, Option<&str>, bool); 8] = [
            ("no zone at all", None, true),
            ("UTC", Some("UTC"), true),
            ("a European zone", Some("Europe/London"), true),
            ("an American zone", Some("America/New_York"), true),
            ("an Etc zone", Some("Etc/GMT+5"), true),
            ("a zone nothing names", Some("Not/AZone"), false),
            ("an offset rather than a zone", Some("+01:00"), false),
            ("prose", Some("here"), false),
        ];

        for (case, timezone, names_a_zone) in rows {
            let violation = schedule_expression_timezone_violation(timezone);
            assert_eq!(
                violation.is_none(),
                names_a_zone,
                "case {case:?}: expected names_a_zone={names_a_zone}, got {violation:?}"
            );
        }
    }

    /// The window a schedule may fire in holds at least one instant.
    #[test]
    fn a_window_holds_an_instant() {
        let now = SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_000_000);
        let at = |secs: i64| DateTime::from_secs(secs);
        // (case, start, end, holds an instant)
        let rows: [(&str, Option<DateTime>, Option<DateTime>, bool); 8] = [
            ("neither boundary", None, None, true),
            ("a start only", Some(at(2_000_000)), None, true),
            ("an end ahead of the clock", None, Some(at(2_000_000)), true),
            ("an end behind the clock", None, Some(at(999_999)), false),
            ("an end at the clock", None, Some(at(1_000_000)), false),
            ("an ordered pair", Some(at(2_000_000)), Some(at(3_000_000)), true),
            ("a reversed pair", Some(at(3_000_000)), Some(at(2_000_000)), false),
            // A single-instant window is one the schedule can fire in.
            ("one instant", Some(at(2_000_000)), Some(at(2_000_000)), true),
        ];

        for (case, start_date, end_date, holds_an_instant) in rows {
            let violation = schedule_window_violation(start_date, end_date, now);
            assert_eq!(
                violation.is_none(),
                holds_an_instant,
                "case {case:?}: expected holds_an_instant={holds_an_instant}, got \
                 {violation:?}"
            );
            // One window rule, one sentence: a window stated by an end alone is
            // refused with the same text as one stated by both boundaries.
            if let Some(message) = violation {
                assert_eq!(
                    message, START_DATE_BEFORE_END_DATE,
                    "case {case:?}: expected {START_DATE_BEFORE_END_DATE:?}, got {message:?}"
                );
            }
        }
    }

    /// An instant is held at the millisecond NEAREST the one the request stated,
    /// so a remainder the wire's floating-point form lands just short of is held
    /// at the millisecond it names rather than the one below it, and one in the
    /// upper half of its millisecond lands on the millisecond above.
    #[test]
    fn an_instant_is_held_to_the_nearest_millisecond() {
        // (case, submitted nanos, the second held, the nanos held)
        let rows: [(&str, u32, i64, u32); 15] = [
            ("a whole second", 0, 7, 0),
            ("whole milliseconds", 123_000_000, 7, 123_000_000),
            // A remainder in the LOWER half of its millisecond keeps it.
            ("microseconds", 123_456_000, 7, 123_000_000),
            ("nanoseconds", 123_456_789, 7, 123_000_000),
            ("under half a millisecond", 499_999, 7, 0),
            ("half a second", 500_000_000, 7, 500_000_000),
            // A remainder in the UPPER half lands on the millisecond above.
            ("past half a millisecond", 123_600_000, 7, 124_000_000),
            ("microseconds rounding up", 777_777_000, 7, 778_000_000),
            ("nanoseconds rounding up", 613_600_001, 7, 614_000_000),
            // Just short of a whole millisecond is that millisecond: this is the
            // shape a decimal remainder read through a binary float arrives in.
            ("a nanosecond under a millisecond", 999_999, 7, 1_000_000),
            (
                "a nanosecond under a later millisecond",
                119_999_999,
                7,
                120_000_000,
            ),
            // Exactly half a millisecond rounds up, away from zero.
            ("half a millisecond", 500_000, 7, 1_000_000),
            ("half a millisecond into the second", 1_500_000, 7, 2_000_000),
            // A remainder that rounds up to a whole second carries into the next
            // second and leaves no remainder behind.
            ("a nanosecond under a second", 999_999_999, 8, 0),
            ("half a millisecond under the second", 999_500_000, 8, 0),
        ];

        for (case, submitted, held_secs, held_nanos) in rows {
            let normalised = to_millisecond_precision(DateTime::from_secs_and_nanos(7, submitted));
            assert_eq!(
                (normalised.secs(), normalised.subsec_nanos()),
                (held_secs, held_nanos),
                "case {case:?}: expected the instant held at {held_secs}s {held_nanos}ns, got \
                 {normalised:?}"
            );
            assert_eq!(
                normalised.subsec_nanos() % NANOS_PER_MILLISECOND,
                0,
                "case {case:?}: a held instant carries whole milliseconds, got {normalised:?}"
            );
        }
    }

    /// The cron form carries a day-of-month field and a day-of-week field, and
    /// exactly one of the two states "no specific value": a day stated in both names
    /// instants the form does not define, and `?` in both names no day at all. The
    /// rows differ along which day field carries the `?`, along what the other
    /// states — a number, a range, a list, a step, a named day, one of the service's
    /// own wildcards — and along how many of them carry it.
    #[test]
    fn exactly_one_of_the_two_day_fields_states_no_specific_value() {
        // (case, expression, states a schedule)
        let rows: [(&str, &str, bool); 14] = [
            ("a day of the month", "cron(0 12 1 * ? *)", true),
            ("every day of the month", "cron(0 12 * * ? *)", true),
            ("a day of the week", "cron(0 12 ? * MON *)", true),
            ("a range of weekdays", "cron(0 12 ? * MON-FRI *)", true),
            ("a list of days of the month", "cron(0 12 1,15 * ? *)", true),
            ("a step over the days of the month", "cron(0 12 1/5 * ? *)", true),
            ("the last day of the month", "cron(0 12 L * ? *)", true),
            ("the nth weekday of the month", "cron(0 12 ? * 3#2 *)", true),
            ("the nearest weekday", "cron(0 12 3W * ? *)", true),
            // Both day fields stating a day, including the dialect's own wildcards,
            // which are held to this rule exactly as the shared grammar's forms are.
            ("a day of the month beside a day of the week", "cron(0 12 1 * MON *)", false),
            ("every day of the month beside a weekday", "cron(0 12 * * MON *)", false),
            ("the last day beside the nth weekday", "cron(0 12 L * 3#2 *)", false),
            ("the nearest weekday beside a weekday", "cron(0 12 3W * 3#2 *)", false),
            // Neither field stating a day states no day at all.
            ("no specific value in either field", "cron(0 12 ? * ? *)", false),
        ];

        for (case, expression, states_a_schedule) in rows {
            let violation = schedule_expression_violation(expression);
            assert_eq!(
                violation.is_none(),
                states_a_schedule,
                "case {case:?}: expected states_a_schedule={states_a_schedule}, got \
                 {violation:?}"
            );
            if let Some(message) = violation {
                assert_eq!(
                    message,
                    invalid_expression(expression),
                    "case {case:?}: a cron the service cannot read is the malformed class"
                );
            }
        }
    }

    /// GUARD on [`LAST_FEASIBLE_YEAR`]: it is the ceiling of the year the shared
    /// grammar reads, so the grammar reads that year and not the one after it — which
    /// is what makes it the end of the lookahead the evaluator can name an instant
    /// in. The published range reaches further ([`CRON_YEARS`]), which is why a year
    /// past the ceiling is still a year the service reads.
    #[test]
    fn the_lookahead_ends_where_the_shared_grammar_stops_reading_years() {
        assert!(
            cron_field_parses(YEAR_FIELD, &LAST_FEASIBLE_YEAR.to_string()),
            "case the last feasible year: expected the shared grammar to read \
             {LAST_FEASIBLE_YEAR}"
        );
        let past = LAST_FEASIBLE_YEAR + 1;
        assert!(
            !cron_field_parses(YEAR_FIELD, &past.to_string()),
            "case the year after it: expected the shared grammar not to read {past}"
        );
        assert!(
            published_year(&past.to_string()).is_some(),
            "case the year after it: expected {past} to be a year the service publishes"
        );
    }

    /// A YEAR field is read over the years the service publishes, and the years it
    /// names decide whether the expression can still fire: the rows differ along the
    /// form the field is written in (a year, a range, a list, a step, `*`), along
    /// whether its years reach the lookahead, and along values no year answers to.
    #[test]
    fn a_year_field_is_read_over_the_years_the_service_publishes() {
        // (case, the year field, the years it reaches from 2026 on)
        let rows: [(&str, &str, Option<bool>); 14] = [
            ("every year", "*", Some(true)),
            ("a year inside the lookahead", "2031", Some(true)),
            ("the first year of the lookahead", "2026", Some(true)),
            ("the last year of the lookahead", "2100", Some(true)),
            ("a year past the lookahead", "2176", Some(false)),
            ("the last year the service publishes", "2199", Some(false)),
            ("a year already behind the window", "2020", Some(false)),
            ("a range reaching into the lookahead", "2090-2199", Some(true)),
            ("a range wholly past the lookahead", "2150-2199", Some(false)),
            ("a list with one reachable year", "2020,2176,2040", Some(true)),
            ("a list of unreachable years", "2020,2176", Some(false)),
            ("a step inside the lookahead", "2030/5", Some(true)),
            // Values the published grammar does not read at all.
            ("a year before the published range", "1969", None),
            ("a year past the published range", "2200", None),
        ];

        for (case, field, reaches) in rows {
            let years = cron_years(field);
            assert_eq!(
                years.is_some(),
                reaches.is_some(),
                "case {case:?}: expected the field read={}, got {years:?}",
                reaches.is_some()
            );
            if let (Some(years), Some(reaches)) = (years, reaches) {
                assert_eq!(
                    years_reach(&years, 2026, LAST_FEASIBLE_YEAR),
                    reaches,
                    "case {case:?}: expected reaches={reaches}, got {years:?}"
                );
            }
        }
    }

    /// An expression the service READS but none of whose years falls inside the
    /// schedule's window — behind its opening, past its close, or past the end of
    /// the lookahead — is refused as one that will never invoke the target, never as
    /// a malformed expression. The rows differ along the year the expression names,
    /// along the boundaries the window states and along the form the expression is
    /// written in.
    #[test]
    fn a_readable_expression_with_no_reachable_occurrence_is_refused_as_never_firing() {
        // The clock the rows are judged against: 1 January 2027, 00:00:00 UTC.
        let now = SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_798_761_600);
        let at = |secs: i64| DateTime::from_secs(secs);
        /// One row: the case, the expression, the window's two boundaries, and
        /// whether the write is refused as one that will never fire.
        type OccurrenceRow = (
            &'static str,
            &'static str,
            Option<DateTime>,
            Option<DateTime>,
            bool,
        );
        let rows: [OccurrenceRow; 12] = [
            ("a year inside the lookahead", "cron(0 12 * * ? 2030)", None, None, false),
            ("every year", "cron(0 12 * * ? *)", None, None, false),
            ("a year past the lookahead", "cron(0 12 * * ? 2176)", None, None, true),
            (
                "a year past the lookahead with a window",
                "cron(0 12 * * ? 2176)",
                Some(at(1_800_000_000)),
                Some(at(1_900_000_000)),
                true,
            ),
            (
                // The window's opening moves the floor, so a year behind the start
                // the request named is unreachable even though the field reads.
                "a year behind the window's opening",
                "cron(0 12 * * ? 2030)",
                Some(at(2_240_000_000)),
                None,
                true,
            ),
            (
                // The window's close moves the ceiling the same way: 2029-07-11 for
                // this instant, which the 2030 the expression names is past.
                "a year past the window's close",
                "cron(0 12 * * ? 2030)",
                Some(at(1_800_000_000)),
                Some(at(1_878_000_000)),
                true,
            ),
            (
                // The same expression with a close in its own year is reachable: the
                // test is taken at the resolution of a year.
                "a year the window's close falls in",
                "cron(0 12 * * ? 2030)",
                Some(at(1_800_000_000)),
                Some(at(1_900_000_000)),
                false,
            ),
            (
                "every year with a close ahead of the clock",
                "cron(0 12 * * ? *)",
                None,
                Some(at(1_900_000_000)),
                false,
            ),
            ("a year behind the clock", "cron(0 12 * * ? 2020)", None, None, true),
            // The other two forms name no year at all, so this rule is not theirs.
            ("a rate", "rate(5 minutes)", None, None, false),
            ("an instant", "at(2030-01-01T00:00:00)", None, None, false),
            // An expression the service cannot read is the malformed class instead.
            ("an unreadable expression", "cron(0 12 1 * MON *)", None, None, false),
        ];

        for (case, expression, start_date, end_date, refused) in rows {
            let violation = schedule_occurrence_violation(expression, start_date, end_date, now);
            assert_eq!(
                violation.is_some(),
                refused,
                "case {case:?}: expected refused={refused}, got {violation:?}"
            );
            if let Some(message) = violation {
                assert_eq!(
                    message,
                    never_schedules_an_invocation(expression, start_date, end_date),
                    "case {case:?}: expected the never-fires sentence, got {message:?}"
                );
            }
        }
    }

    /// The never-fires sentence states the expression and both window boundaries as
    /// the service echoes them: the UTC date and time, no zone suffix, carried only
    /// to the resolution the instant states something at, and `null` for a boundary
    /// the request left out.
    #[test]
    fn the_never_fires_sentence_echoes_the_window_as_the_service_states_it() {
        // (case, the instant, the echo)
        let rows: [(&str, Option<DateTime>, &str); 6] = [
            ("no boundary at all", None, ABSENT_INSTANT),
            (
                "a whole minute",
                Some(DateTime::from_secs(1_798_761_600)),
                "2027-01-01T00:00",
            ),
            (
                "a whole second inside the minute",
                Some(DateTime::from_secs(1_798_761_630)),
                "2027-01-01T00:00:30",
            ),
            (
                "a remainder of milliseconds",
                Some(DateTime::from_secs_and_nanos(1_798_761_630, 120_000_000)),
                "2027-01-01T00:00:30.120",
            ),
            (
                // A finer remainder is echoed at the resolution the service holds.
                "a remainder finer than a millisecond",
                Some(DateTime::from_secs_and_nanos(1_798_761_630, 120_499_000)),
                "2027-01-01T00:00:30.120",
            ),
            (
                "a remainder inside a whole minute",
                Some(DateTime::from_secs_and_nanos(1_798_761_600, 5_000_000)),
                "2027-01-01T00:00:00.005",
            ),
        ];

        for (case, instant, echo) in rows {
            assert_eq!(
                echoed_instant(instant),
                echo,
                "case {case:?}: expected the instant echoed as {echo:?}"
            );
        }

        let expression = "cron(0 12 * * ? 2176)";
        let start = DateTime::from_secs(1_798_761_600);
        assert_eq!(
            never_schedules_an_invocation(expression, Some(start), None),
            format!(
                "The schedule expression {expression}, with StartDate 2027-01-01T00:00 and \
                 EndDate null, is invalid because it will never schedule an invocation."
            ),
            "case the whole sentence: expected the expression and the window in it"
        );
    }
}
