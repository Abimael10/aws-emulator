// SPDX-License-Identifier: CC0-1.0

//! The `YYYY-MM` billing-month arithmetic this service's monthly charge
//! reports share.
//!
//! Every charge report of this service takes a modeled billing month
//! (`^\d{4}-(0[1-9]|1[0-2])$`), compares it with the month the clock is in and
//! reports the UTC period its charges accrued in, so that reading of a month —
//! the parse, the clock's month and the comparison of the two
//! ([`requested_month`]), and the past-month gate the Enterprise Support reads
//! apply to it ([`require_past_billing_month`]) — lives here rather than beside
//! each handler.
//!
//! The same civil-calendar arithmetic answers how wide "one calendar month" is
//! from a given instant ([`one_calendar_month_after`]), which is the bound
//! `ListBillingViews` documents for its `activeTimeRange`.

/// How many ASCII digits the modeled `BillingMonth` states a year in
/// (`^\d{4}-(0[1-9]|1[0-2])$`), which is also the width the modeled `Timestamp`
/// renders one at.
const YEAR_DIGITS: usize = 4;

/// How many ASCII digits the same expression states a month in.
const MONTH_DIGITS: usize = 2;

/// The months one calendar year holds — the modulus of the month arithmetic
/// below, and the highest month a modeled `YYYY-MM` value can name.
const MONTHS_PER_YEAR: i32 = 12;

/// The year and month of a modeled `YYYY-MM` billing month, or `None` for a
/// value outside that form.
///
/// The whole expression is enforced here rather than only its separator: four
/// digits of year, then two digits naming a month of the calendar
/// (`0[1-9]|1[0-2]`). Every reader of a month takes its parts from here, so no
/// value a caller can state reaches the arithmetic below outside the range the
/// model admits.
pub fn parse_month(billing_month: &str) -> Option<(i32, i32)> {
    let (year, month) = billing_month.split_once('-')?;
    if year.len() != YEAR_DIGITS || month.len() != MONTH_DIGITS {
        return None;
    }
    if !year
        .bytes()
        .chain(month.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let month = month.parse().ok()?;
    if !(1..=MONTHS_PER_YEAR).contains(&month) {
        return None;
    }
    Some((year.parse().ok()?, month))
}

/// A month as the count of whole calendar months before it, so two months
/// compare and subtract.
///
/// Saturating, so no pair of values can make the count wrap or panic: a month
/// comes from [`parse_month`] or from the clock, both inside the four-digit
/// range the model states, and an index that saturates still orders the same
/// way.
pub fn month_index((year, month): (i32, i32)) -> i32 {
    year.saturating_mul(MONTHS_PER_YEAR)
        .saturating_add(month - 1)
}

/// The first instant of a month, read off the UTC civil calendar.
///
/// `None` for a year the modeled `Timestamp` cannot render at four digits,
/// which only the month after the last `YYYY-MM` month is.
pub fn first_instant_of(year: i32, month: i32) -> Option<::aws_smithy_types::DateTime> {
    ::aws_smithy_types::DateTime::from_str(
        &format!("{year:04}-{month:02}-01T00:00:00Z"),
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .ok()
}

/// The month after `(year, month)` on the UTC civil calendar.
///
/// Saturating on the year for the same reason [`month_index`] is: the last year
/// the modeled `Timestamp` renders has no month after it anyway, and
/// [`first_instant_of`] answers `None` for one it cannot render.
pub fn next_month((year, month): (i32, i32)) -> (i32, i32) {
    if month >= MONTHS_PER_YEAR {
        (year.saturating_add(1), 1)
    } else {
        (year, month + 1)
    }
}

/// The half-open UTC period of one calendar month: its first instant, and the
/// first instant of the month after it.
///
/// AWS bills on UTC calendar months, so both bounds come from the month's own
/// position on the civil calendar — never from a fixed month length. Shared by
/// the reports that take a modeled `YYYY-MM` month ([`month_period`]) and by
/// `ListBillingViewSegments`, whose default period is "the current billing
/// period, which is the calendar month in UTC".
pub fn calendar_month_period(
    year: i32,
    month: i32,
) -> Option<(::aws_smithy_types::DateTime, ::aws_smithy_types::DateTime)> {
    let (next_year, next_month) = next_month((year, month));
    Some((
        first_instant_of(year, month)?,
        first_instant_of(next_year, next_month)?,
    ))
}

/// Nanoseconds in one second — the scale an instant carries its sub-second
/// part on.
const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

/// The instant one calendar month after `at`, in seconds since the epoch — on
/// the same scale as [`super::date_time_secs`], so a request `Timestamp`
/// compares against it directly.
///
/// The calendar arithmetic is `chrono`'s `checked_add_months`: one calendar
/// month after an instant is the same day and time of the following month, with
/// a day that month does not have (the 31st of a 30-day month, the 29th of a
/// short February) clamped to its last. The width of one calendar month is
/// therefore the calendar's — 28, 29, 30 or 31 days according to the month the
/// span starts in — never a fixed span, the same reason
/// [`calendar_month_period`] takes both its bounds from the calendar.
///
/// `None` for an instant outside the range that calendar covers, and for one
/// whose following month is past its end.
pub fn one_calendar_month_after(at: &::aws_smithy_types::DateTime) -> Option<f64> {
    let start = ::chrono::DateTime::from_timestamp(at.secs(), at.subsec_nanos())?;
    let one_month_on = start.checked_add_months(::chrono::Months::new(1))?;
    // The month addition keeps the time of day, so one calendar month on falls
    // on the start's own second and its own fraction of a second.
    Some(one_month_on.timestamp() as f64 + f64::from(at.subsec_nanos()) / NANOS_PER_SECOND)
}

/// The request member every read of this service takes a billing month in.
pub const BILLING_MONTH_MEMBER: &str = "billingMonth";

/// Why a requested billing month is not one the operation that took it reports
/// charges for.
///
/// The DECISION is shared, the exception is not: each handler turns one of
/// these into the modeled error its own operation declares, so one rule reaches
/// the wire in one wording from every read that applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingMonthRefusal {
    /// The request member `member` broke `constraint`, which continues the
    /// sentence the member name starts — a request to refuse as invalid.
    InvalidField {
        member: &'static str,
        constraint: &'static str,
    },
    /// `what` could not be read, which is a failure of this emulator rather
    /// than of the request.
    Unavailable { what: &'static str },
}

/// Where a requested billing month sits relative to the month the clock is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedMonth {
    /// A month whose billing period has closed, so its charges are finalized.
    Past,
    /// The month the clock is in, whose billing period has not closed.
    Current,
    /// A month that has not begun.
    Future,
}

/// Place a requested modeled `YYYY-MM` billing month against the month the
/// clock is in at `at`.
///
/// Every operation that takes a billing month decides what it answers for one
/// from this single placement — parse the value, read the clock's month,
/// compare the two — so no handler re-derives the calendar and two reads cannot
/// drift apart on which months they answer.
pub fn requested_month(
    billing_month: &str,
    at: ::std::time::SystemTime,
) -> Result<RequestedMonth, BillingMonthRefusal> {
    let requested = parse_month(billing_month).ok_or(BillingMonthRefusal::InvalidField {
        member: BILLING_MONTH_MEMBER,
        constraint: "must name a calendar month",
    })?;
    let current = super::billing_period(at).ok_or(BillingMonthRefusal::Unavailable {
        what: "the current billing month",
    })?;
    Ok(match month_index(requested).cmp(&month_index(current)) {
        ::std::cmp::Ordering::Less => RequestedMonth::Past,
        ::std::cmp::Ordering::Equal => RequestedMonth::Current,
        ::std::cmp::Ordering::Greater => RequestedMonth::Future,
    })
}

/// The gate the modeled `EnterpriseSupportBillingMonth` states: "This must be a
/// month in the past."
///
/// The current month's billing period has not closed and a later one has not
/// begun, so neither has an Enterprise Support charge to report and both are
/// refused rather than answered with an empty month. Every read of the
/// Enterprise Support profile applies the gate through here — the way those
/// same reads decide their payer-account boundary through
/// [`super::enterprise_support_read_refusal`] — so all of them refuse the same
/// months, naming the same member and the same constraint.
pub fn require_past_billing_month(
    billing_month: &str,
    at: ::std::time::SystemTime,
) -> Result<(), BillingMonthRefusal> {
    if requested_month(billing_month, at)? == RequestedMonth::Past {
        return Ok(());
    }
    Err(BillingMonthRefusal::InvalidField {
        member: BILLING_MONTH_MEMBER,
        constraint: "must be a month in the past",
    })
}

/// The half-open UTC period a billing month's charges accrue in: the first
/// instant of the month, and the first instant of the month after it.
pub fn month_period(
    billing_month: &str,
) -> Option<(::aws_smithy_types::DateTime, ::aws_smithy_types::DateTime)> {
    let (year, month) = parse_month(billing_month)?;
    calendar_month_period(year, month)
}
