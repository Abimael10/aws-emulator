// SPDX-License-Identifier: CC0-1.0

//! The maintenance windows this service holds: "a maintenance window […] defines a
//! schedule for performing potentially disruptive actions on your nodes"
//! (`smithy_query(query="doc",shape="CreateMaintenanceWindow")`).
//!
//! `CreateMaintenanceWindow` mints one and `GetMaintenanceWindow` reports it, both
//! through the one store below
//! ([`super::ComAmazonawsSsmAmazonssmState::maintenance_windows`]), so whatever the
//! create persisted is what a later read reports.
//!
//! A window is reached through the endpoint of ONE Region — Systems Manager is a regional
//! service — so the store is keyed by `(region, window id)`, the id being the one the
//! create minted ([`mint_window_id`]). An id nothing created names no window, which is
//! what the modelled `DoesNotExistException` reports
//! ([`maintenance_window_not_found_message`]).
//!
//! What a window WILL do is read from its schedule rather than stored: "The schedule of
//! the maintenance window in the form of a cron or rate expression"
//! (`CreateMaintenanceWindow$Schedule`) names the instants the window opens at, and
//! `NextExecutionTime` is "the next time the maintenance window will actually run, taking
//! into account any specified times for the maintenance window to become active or
//! inactive" (`MaintenanceWindowIdentity$NextExecutionTime`) — so it is COMPUTED from the
//! stored schedule, the stored activation window and the clock ([`WindowSchedule`]), and
//! moves on its own as the clock does.
//!
//! The same reading answers for the whole series rather than for its first instant alone:
//! "information about upcoming executions of a maintenance window"
//! (`smithy_query(query="doc",shape="DescribeMaintenanceWindowSchedule")`) is the successive
//! instants that schedule names inside the interval the window is active over
//! ([`MaintenanceWindowRecord::scheduled_executions`]), so a window's next execution and a
//! report of its upcoming executions can never disagree.
//!
//! The grammars are read by maintained parsers rather than by a hand-written one: the
//! `cron` crate for a `cron(...)` body normalised by [`normalised_cron`], `chrono` for an
//! `at(...)` instant and for the ISO-8601 instants that bound a window, and `chrono-tz`
//! for the IANA zone a window states its schedule in. A body carrying one of the
//! wildcards this service's own cron dialect documents and that crate's grammar does not
//! read (`L`, `W`, `#`) is accepted by the write path and names no instant here, so such
//! a window is held as asked for rather than opened at an instant guessed for it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use ::chrono::{Offset, TimeZone};

use crate::ops::types::{MaintenanceWindowIdentity, PatchOrchestratorFilter};

use super::{ComAmazonawsSsmAmazonssmState, NANOS_PER_MILLISECOND};

/// What every maintenance window id starts with, from the modelled `MaintenanceWindowId`
/// pattern (`^mw-[0-9a-f]{17}$`).
pub const WINDOW_ID_PREFIX: &str = "mw-";

/// How many lowercase hex digits a window id carries after [`WINDOW_ID_PREFIX`], from the
/// same modelled pattern — which is also what makes an id exactly the modelled length
/// (`len 20..20`).
pub const WINDOW_ID_HEX_DIGITS: usize = 17;

/// The `ResourceTypeForTagging` value whose id is a maintenance window's minted id:
/// "MaintenanceWindow: mw-012345abcde" (`AddTagsToResourceRequest$ResourceId`), from the
/// modelled enum (`crate::ops::types::RESOURCE_TYPE_FOR_TAGGING_VALUES`).
pub const TAGGING_TYPE_MAINTENANCE_WINDOW: &str = "MaintenanceWindow";

/// Whether a window a create has just minted is enabled.
///
/// `CreateMaintenanceWindow` carries no `Enabled` member at all while
/// `UpdateMaintenanceWindow` does ("Whether the maintenance window is enabled"), so a
/// created window is enabled and only an update can disable it.
pub const CREATED_ENABLED: bool = true;

/// How many maintenance windows this store holds at once, across every Region.
///
/// This is the EMULATOR's own ceiling rather than a quota Systems Manager publishes: a
/// create is the only thing that admits a window, callers may run as many as they like,
/// and the store is held in memory for as long as the process runs — so without a ceiling
/// the one map would grow for as long as creates keep arriving. A create past it is
/// refused through the modelled `ResourceLimitExceededException` and writes nothing.
pub const MAX_MAINTENANCE_WINDOWS: usize = 100;

/// What a create refused by [`MAX_MAINTENANCE_WINDOWS`] reports: the emulator's own
/// ceiling, named as the emulator's, with the figure it stands at.
#[must_use]
pub fn maintenance_window_ceiling_message() -> String {
    format!(
        "This emulator holds at most {MAX_MAINTENANCE_WINDOWS} maintenance windows across \
         every Region"
    )
}

/// How this service reports a `WindowId` that names no maintenance window reachable
/// through this endpoint, through `DoesNotExistException`.
///
/// The error's own account of its class is "the specified resource, such as a maintenance
/// window or patch baseline, doesn't exist" (`smithy_query(query="doc",
/// shape="DoesNotExistException")`), so the sentence names the window the request asked
/// for and nothing else of it.
#[must_use]
pub fn maintenance_window_not_found_message(window_id: &str) -> String {
    format!("Maintenance window {window_id} does not exist")
}

/// The units a `rate(value unit)` schedule may name, and what each is worth in seconds,
/// from the rate-expression table of the service's cron-and-rate reference: "unit: The
/// unit of time […] minute | minutes | hour | hours | day | days". Both spellings of each
/// unit are accepted, as the service accepts them.
const RATE_UNITS: &[(&str, u64)] = &[
    ("minute", 60),
    ("minutes", 60),
    ("hour", 3_600),
    ("hours", 3_600),
    ("day", 86_400),
    ("days", 86_400),
];

/// How many whitespace-separated fields a `cron(...)` body carries, from the same
/// reference's cron table: Minutes, Hours, Day-of-month, Month, Day-of-week, Year — six.
const CRON_FIELDS: usize = 6;

/// The index of the day-of-month field among the six a `cron(...)` body carries.
const DAY_OF_MONTH_FIELD: usize = 2;

/// The index of the day-of-week field among those six.
const DAY_OF_WEEK_FIELD: usize = 4;

/// The indexes of the two fields a `?` ("no specific value") is a statement in —
/// day-of-month and day-of-week — which is the same statement `*` makes in the grammar the
/// `cron` crate reads.
const NO_SPECIFIC_VALUE_FIELDS: [usize; 2] = [DAY_OF_MONTH_FIELD, DAY_OF_WEEK_FIELD];

/// How many times one weekday can fall in a month, which is what the `<day-of-week>#<nth>`
/// form may ask for: a 31-day month holds five of one weekday at most.
const MAX_WEEKDAYS_IN_MONTH: u8 = 5;

/// The field values the normalised seven-field expression carries for every field but the
/// one under test, so a field is judged on its own.
const NEUTRAL_CRON_FIELDS: [&str; CRON_FIELDS + 1] = ["0", "0", "0", "*", "*", "*", "*"];

/// The format an `at(...)` body states its instant in: `at(yyyy-mm-ddThh:mm:ss)`, a local
/// date and time with no zone offset, since the zone is `ScheduleTimezone`'s to state.
const AT_EXPRESSION_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// The date and the time of day an instant a window reports is written with, down to the
/// MINUTE — the shortest ISO-8601 Extended rendering, which a whole minute is written as.
///
/// `MaintenanceWindowStringDateTime` is a plain string shape with no `timestampFormat`
/// trait, so the text is the service's to write and a client parses what it finds:
/// Systems Manager writes an ISO-8601 Extended instant whose components BELOW the minute
/// are stated only when they carry something — the seconds when they are not zero, and the
/// milliseconds beside them when they are not either
/// ([`INSTANT_SECONDS_FORMAT`], [`INSTANT_MILLISECONDS_FORMAT`]) — so the instants a
/// schedule names on whole minutes are written to the minute, and one a series anchored
/// off the minute lands on carries every component it has.
const INSTANT_MINUTE_FORMAT: &str = "%Y-%m-%dT%H:%M";

/// What the seconds of such an instant are appended as, when they are not zero.
const INSTANT_SECONDS_FORMAT: &str = ":%S";

/// What its milliseconds are appended as, when they are not zero: the resolution every
/// instant this service writes is held at.
const INSTANT_MILLISECONDS_FORMAT: &str = "%.3f";

/// The designator an instant read at UTC ends with, rather than a zero offset.
const INSTANT_UTC_DESIGNATOR: &str = "Z";

/// The offset an instant read in a zone that is not at UTC ends with instead.
const INSTANT_OFFSET_FORMAT: &str = "%:z";

/// The civil date-and-time form a window's `StartDate`/`EndDate` may state without a zone,
/// the zone then being the window's own: both are documented "in ISO-8601 Extended format"
/// (`CreateMaintenanceWindow$StartDate`).
const CIVIL_DATE_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// The zone a schedule is read in when the window names none: the two instants that bound
/// a window and the schedule itself are then read in UTC, which is the zone an ISO-8601
/// instant with no offset of its own is taken at.
const DEFAULT_ZONE: ::chrono_tz::Tz = ::chrono_tz::Tz::UTC;

/// A day, which is the unit `ScheduleOffset` states its wait in: "The number of days to
/// wait after the date and time specified by a cron expression before running the
/// maintenance window" (`CreateMaintenanceWindow$ScheduleOffset`).
const ONE_DAY: Duration = Duration::from_secs(86_400);

/// The filter keys a describe of the account's maintenance windows may narrow by:
/// "Supported filter keys are Name and Enabled"
/// (`DescribeMaintenanceWindowsRequest$Filters`). What a refusal of another key lists, and
/// the one place that set is written.
pub const WINDOW_FILTER_KEYS: &[&str] = &["Name", "Enabled"];

/// Whether `key` is one of the filter keys a window describe takes
/// ([`WINDOW_FILTER_KEYS`]).
#[must_use]
pub fn is_window_filter_key(key: &str) -> bool {
    WINDOW_FILTER_KEYS.contains(&key)
}

/// The smallest page a describe of the account's windows serves, from the modelled
/// `MaintenanceWindowMaxResults` (`range 10..100`) — the SAME member a describe of a
/// window's targets is bounded by, so both read the range from the one place.
pub const MIN_WINDOW_PAGE_SIZE: i32 = super::maintenance_window_targets::MIN_TARGET_PAGE_SIZE;

/// The largest page that read serves, from the same modelled member, which is also the
/// page a request naming no `MaxResults` is served.
pub const MAX_WINDOW_PAGE_SIZE: i32 = super::maintenance_window_targets::MAX_TARGET_PAGE_SIZE;

/// The page size a window describe serves (see `super::page_size`).
pub fn window_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(max_results, MIN_WINDOW_PAGE_SIZE, MAX_WINDOW_PAGE_SIZE)
}

/// The filter keys a report of a window's UPCOMING executions may narrow by: "you can limit
/// maintenance window executions to only those scheduled before or after a certain date and
/// time" (`DescribeMaintenanceWindowScheduleRequest$Filters`) — the two ends of that range,
/// each value stating an instant in the same ISO-8601 Extended form a window states its own
/// bounds in. What a refusal of another key lists, and the one place that set is written.
pub const SCHEDULE_FILTER_KEYS: &[&str] = &["Before", "After"];

/// Whether `key` is one of the filter keys a schedule report takes
/// ([`SCHEDULE_FILTER_KEYS`]).
#[must_use]
pub fn is_schedule_filter_key(key: &str) -> bool {
    SCHEDULE_FILTER_KEYS.contains(&key)
}

/// The smallest page a report of upcoming executions serves, from the modelled
/// `MaintenanceWindowSearchMaxResults` (`range 1..`).
pub const MIN_SCHEDULE_PAGE_SIZE: i32 = 1;

/// The largest page that report serves, which is also the page a request naming no
/// `MaxResults` is served.
///
/// That modelled member states no maximum of its own, while the series a schedule names runs
/// as long as the schedule does — a `rate(...)` or `cron(...)` window is scheduled for as far
/// ahead as anyone asks — so the page is bounded HERE, at the largest page the other
/// maintenance window reads of this service serve ([`MAX_WINDOW_PAGE_SIZE`]), rather than
/// left for a request to choose without limit.
pub const MAX_SCHEDULE_PAGE_SIZE: i32 = MAX_WINDOW_PAGE_SIZE;

/// What a `MaxResults` past the page this report serves is refused with: the emulator's own
/// ceiling, named as the emulator's and with the figure the request asked for, so a caller
/// does not read it as a bound the service models — the modelled member states none
/// ([`MAX_SCHEDULE_PAGE_SIZE`]).
#[must_use]
pub fn schedule_page_ceiling_message(requested: i32) -> String {
    format!(
        "Value '{requested}' at 'maxResults' is more than the {MAX_SCHEDULE_PAGE_SIZE} upcoming \
         executions this emulator serves in one page"
    )
}

/// The page size a report of upcoming executions serves: the modelled lower bound refused the
/// way every other page of this service refuses a figure outside its range
/// (`super::page_size`), and the emulator's own ceiling named as the emulator's
/// ([`schedule_page_ceiling_message`]).
pub fn schedule_page_size(max_results: Option<i32>) -> Result<usize, String> {
    if let Some(asked) = max_results.filter(|asked| *asked > MAX_SCHEDULE_PAGE_SIZE) {
        return Err(schedule_page_ceiling_message(asked));
    }
    super::page_size(max_results, MIN_SCHEDULE_PAGE_SIZE, MAX_SCHEDULE_PAGE_SIZE)
}

/// The instants a schedule report's `Filters` admit: the instant its executions have to fall
/// after and the one they have to fall before, each `None` when nothing bounds that end — or
/// `None` for filters that admit no instant at all.
///
/// Both ends are EXCLUSIVE: "only those scheduled before or after a certain date and time"
/// (`DescribeMaintenanceWindowScheduleRequest$Filters`) admits what stands before or after the
/// instant stated and not what stands at it, so the two ends read the same way round.
///
/// A filter's values UNION while the filters INTERSECT, the rule every filtered read of this
/// service follows: an execution passes an `After` filter when it falls after ANY of that
/// filter's instants (so the earliest of them is the bound), passes a `Before` filter when it
/// falls before any of them (so the latest is), and has to pass every filter stated. A value
/// naming no instant is the instant of no execution, so it admits none — the reading
/// [`MaintenanceWindowRecord::matches_filter`] gives a value spelling neither boolean — and a
/// filter carrying only such values, or no value at all, therefore admits nothing.
///
/// A key outside [`SCHEDULE_FILTER_KEYS`] bounds nothing here either; its caller refuses such
/// a key, naming it, before anything is asked about the windows.
#[must_use]
pub fn schedule_filter_span(
    filters: &[PatchOrchestratorFilter],
) -> Option<(Option<SystemTime>, Option<SystemTime>)> {
    let mut not_before: Option<SystemTime> = None;
    let mut not_after: Option<SystemTime> = None;
    for filter in filters {
        let stated: Vec<SystemTime> = filter
            .values
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|value| schedule_filter_instant(value))
            .collect();
        match filter.key.as_deref().unwrap_or_default() {
            "After" => {
                let earliest = stated.into_iter().min()?;
                not_before = Some(not_before.map_or(earliest, |held| held.max(earliest)));
            }
            "Before" => {
                let latest = stated.into_iter().max()?;
                not_after = Some(not_after.map_or(latest, |held| held.min(latest)));
            }
            _ => return None,
        }
    }
    Some((not_before, not_after))
}

/// The instant a schedule filter's value states, or `None` for text naming none.
///
/// Read the way a window's own `StartDate` and `EndDate` are ([`window_instant`]) — ISO-8601
/// Extended, at UTC when the text carries no offset of its own, since a filter states no zone
/// — and additionally in the shortest form this service WRITES an execution time in, the one
/// whose seconds carry nothing ([`INSTANT_MINUTE_FORMAT`]), so a caller can bound a range by
/// an instant it was reported.
fn schedule_filter_instant(value: &str) -> Option<SystemTime> {
    let value = value.trim();
    window_instant(value, DEFAULT_ZONE).or_else(|| {
        seconds_supplied(value).and_then(|stated| window_instant(&stated, DEFAULT_ZONE))
    })
}

/// `value` with the zero seconds an instant written only to the minute leaves out supplied,
/// or `None` when it already states them (or states no time of day at all).
fn seconds_supplied(value: &str) -> Option<String> {
    let (date, rest) = value.split_once('T')?;
    // Where the zone this instant may carry starts: its designator or the sign of its
    // offset, neither of which stands in the time of day itself.
    let zone_at = rest
        .find(['Z', 'z', '+', '-'])
        .unwrap_or(rest.len());
    let (time, zone) = rest.split_at(zone_at);
    (time.matches(':').count() == 1).then(|| format!("{date}T{time}:00{zone}"))
}

/// The byte a schedule cursor puts between the instant it resumes at and the window that
/// instant belongs to. Neither a decimal count of milliseconds nor a window id carries it, so
/// it splits a cursor exactly once.
const SCHEDULE_CURSOR_SEPARATOR: char = '/';

/// Where a page of a window schedule report resumes: the entry the next page starts at.
///
/// A schedule report walks the executions of the windows it was asked about in the order they
/// are scheduled, the window id breaking a tie so that order is total — which is exactly what
/// this cursor names, so a page resumes at an ENTRY rather than at an offset into a series
/// that is computed afresh, from a fresh clock reading, for every request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScheduleCursor {
    /// The instant the entry is scheduled at, in whole milliseconds since the epoch — the
    /// resolution this service states an instant at ([`super::NANOS_PER_MILLISECOND`]), so a
    /// cursor names the same instant the page it was handed out by reported.
    pub at: u64,
    /// The window the entry belongs to.
    pub window_id: String,
}

impl ScheduleCursor {
    /// The cursor of the execution scheduled at `at` in the window `window_id`.
    #[must_use]
    pub fn of(at: SystemTime, window_id: &str) -> Self {
        let since_epoch = at
            .duration_since(::std::time::UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or_default();
        Self {
            at: u64::try_from(since_epoch).unwrap_or(u64::MAX),
            window_id: window_id.to_string(),
        }
    }

    /// The `NextToken` a page hands out to resume here.
    #[must_use]
    pub fn page_token(&self) -> String {
        format!("{}{SCHEDULE_CURSOR_SEPARATOR}{}", self.at, self.window_id)
    }

    /// The cursor a `NextToken` names, or `None` for a value this service did not hand out —
    /// one that does not split into a count of milliseconds and a window id of the modelled
    /// form ([`is_maintenance_window_id`]) — which is what `InvalidNextToken` reports.
    #[must_use]
    pub fn of_token(token: &str) -> Option<Self> {
        let (at, window_id) = token.split_once(SCHEDULE_CURSOR_SEPARATOR)?;
        let at: u64 = at.parse().ok()?;
        is_maintenance_window_id(window_id).then(|| Self {
            at,
            window_id: window_id.to_string(),
        })
    }

    /// The exclusive lower bound a page resuming here searches a schedule from: one
    /// millisecond before the instant this cursor names, since the cursor states that instant
    /// to the millisecond and the occurrence it names may stand anywhere inside it.
    ///
    /// No schedule names two occurrences inside one millisecond — the shortest period a
    /// `rate(...)` states is a minute ([`RATE_UNITS`]) and a `cron(...)` expression names
    /// instants a minute apart at the closest — so the first occurrence past this bound is
    /// the one the cursor names.
    #[must_use]
    pub fn search_from(&self) -> SystemTime {
        ::std::time::UNIX_EPOCH
            .checked_add(Duration::from_millis(self.at))
            .and_then(|at| at.checked_sub(Duration::from_millis(1)))
            .unwrap_or(::std::time::UNIX_EPOCH)
    }
}

/// Whether `id` is a maintenance window id — the modelled `MaintenanceWindowId` pattern
/// (`^mw-[0-9a-f]{17}$`), which is [`WINDOW_ID_PREFIX`] followed by exactly
/// [`WINDOW_ID_HEX_DIGITS`] lowercase hex digits.
///
/// This crate's one reading of that shape: what tells a window id apart from text that
/// could never name a window, whether it arrives as an identifier or as a page token.
#[must_use]
pub fn is_maintenance_window_id(id: &str) -> bool {
    id.strip_prefix(WINDOW_ID_PREFIX).is_some_and(|digits| {
        digits.len() == WINDOW_ID_HEX_DIGITS
            && digits
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// How a filter's value states a boolean, or `None` for a value stating neither.
///
/// "For example, Name=MyMaintenanceWindow and Enabled=True"
/// (`DescribeMaintenanceWindowsRequest$Filters`): a filter's values arrive as text, so the
/// value is read as the boolean it spells however it is cased. A value spelling neither
/// boolean is the value of no window's `Enabled`, so it selects none.
fn boolean_filter_value(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// How this service states a `Schedule` that is not a schedule at all.
///
/// The one sentence Systems Manager words this error class with, declared once for the whole
/// crate beside the association schedules that first stated it
/// (`super::associations::INVALID_SCHEDULE`, quoting the modelled `InvalidSchedule`: "The
/// schedule is invalid. Verify your cron or rate expression and try again"). It is fixed
/// text for the class and interpolates nothing from the request, so every refusal of a cron
/// or rate expression this service cannot run on is worded the same way.
pub const INVALID_SCHEDULE: &str = super::associations::INVALID_SCHEDULE;

/// How this service states a `ScheduleTimezone` no IANA zone answers to, with the value
/// the request carried.
#[must_use]
pub fn invalid_schedule_timezone_message(timezone: &str) -> String {
    format!("Invalid schedule timezone {timezone}.")
}

/// Refuse a `Schedule` that states none of the forms this service documents, or states one
/// of them malformed; `None` for an expression the service can read.
///
/// The length bound (1..256) is modelled, so the boundary walk has already refused
/// anything outside it by the time a handler runs. Both operations that WRITE a window's
/// schedule are held to this one rule, so a schedule one accepts is one the other does.
#[must_use]
pub fn schedule_violation(schedule: &str) -> Option<&'static str> {
    let readable = match form_of(schedule) {
        Some((ScheduleForm::At, body)) => at_expression_states_an_instant(body),
        Some((ScheduleForm::Rate, body)) => rate_period(body).is_some(),
        Some((ScheduleForm::Cron, body)) => cron_expression_states_its_instants(body),
        None => false,
    };
    (!readable).then_some(INVALID_SCHEDULE)
}

/// Refuse a `ScheduleTimezone` that names no time zone; `None` for an absent member (the
/// schedule is then read in UTC) or a zone the IANA database carries.
///
/// "The time zone that the scheduled maintenance window executions are based on, in
/// Internet Assigned Numbers Authority (IANA) format"
/// (`CreateMaintenanceWindow$ScheduleTimezone`): a value no zone answers to leaves the
/// instants the window opens at undefined, so it is not accepted.
#[must_use]
pub fn schedule_timezone_violation(timezone: Option<&str>) -> Option<String> {
    let timezone = timezone.filter(|zone| !zone.is_empty())?;
    timezone
        .parse::<::chrono_tz::Tz>()
        .err()
        .map(|_| invalid_schedule_timezone_message(timezone))
}

/// How this service words a `Cutoff` that is not smaller than the `Duration` it is taken
/// against: the figure the request carried, and the figure the service names the bound
/// with — the duration's last hour, one short of the duration itself, which is the largest
/// cutoff a window of that duration could carry.
#[must_use]
pub fn cutoff_not_smaller_than_duration_message(cutoff: i32, duration: i32) -> String {
    let bound = duration.saturating_sub(1);
    format!("Cutoff was {cutoff} but must be smaller than {bound} (Duration  - 1 )")
}

/// Refuse a `Cutoff` that is not smaller than the `Duration` it is stated beside; `None`
/// when the two hold together.
///
/// "The value you specify for Duration determines the specific end time for the maintenance
/// window based on the time it begins. No maintenance window tasks are permitted to start
/// after the resulting endtime minus the number of hours you specify for Cutoff"
/// (`smithy_query(query="doc",shape="CreateMaintenanceWindow")`): the cutoff is counted
/// back from the end of a window that lasts `Duration` hours, so a cutoff of the whole
/// duration or more leaves no span in which a task could ever start — the largest cutoff a
/// window can carry is one hour short of its duration. Each figure's own range (`Cutoff`
/// 0..23, `Duration` 1..24) is MODELLED, so the boundary walk has already refused anything
/// outside it; this is the rule the PAIR holds together under, and every operation that
/// writes the pair is held to it.
#[must_use]
pub fn cutoff_violation(cutoff: i32, duration: i32) -> Option<String> {
    (cutoff >= duration).then(|| cutoff_not_smaller_than_duration_message(cutoff, duration))
}

/// `at` instant, `rate` period or `cron` fields: the forms a maintenance window's schedule
/// may state, keyed by the keyword that introduces each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleForm {
    At,
    Rate,
    Cron,
}

/// The form `schedule` states and the body it carries, or `None` when it states no form
/// this service defines.
fn form_of(schedule: &str) -> Option<(ScheduleForm, &str)> {
    for (keyword, form) in [
        ("at", ScheduleForm::At),
        ("rate", ScheduleForm::Rate),
        ("cron", ScheduleForm::Cron),
    ] {
        if let Some(body) = schedule
            .trim()
            .strip_prefix(keyword)
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return Some((form, body));
        }
    }
    None
}

/// Whether an `at(...)` body states an instant in the documented format.
fn at_expression_states_an_instant(body: &str) -> bool {
    ::chrono::NaiveDateTime::parse_from_str(body.trim(), AT_EXPRESSION_FORMAT).is_ok()
}

/// The period a `rate(...)` body states, or `None` when it states none: a positive whole
/// number of a unit the service names ([`RATE_UNITS`]), and nothing else.
///
/// A figure so large that the first occurrence of the series falls off the calendar this
/// module places instants on states no period either ([`is_schedulable`]): such a series has
/// no date to open at, so it is refused with the rest of the unreadable schedules rather
/// than stored as a schedule nothing could ever report.
fn rate_period(body: &str) -> Option<Duration> {
    let mut fields = body.split_whitespace();
    let (Some(value), Some(unit), None) = (fields.next(), fields.next(), fields.next()) else {
        return None;
    };
    let value: u64 = value.parse().ok()?;
    if value == 0 {
        return None;
    }
    let unit_seconds = RATE_UNITS
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, seconds)| *seconds)?;
    // A figure of units no span can represent states no period at all: it is refused
    // rather than saturated into one the series could never move past.
    let seconds = value.checked_mul(unit_seconds)?;
    is_schedulable(seconds).then(|| Duration::from_secs(seconds))
}

/// Whether a span of `seconds` from the epoch lands on an instant this module can place on
/// the calendar at all — the range the date library represents, rather than the range a
/// `SystemTime` happens to hold.
///
/// A rate schedule's period is the one caller-supplied figure that can carry an occurrence
/// past that range, so it is taken against this before it is stored.
fn is_schedulable(seconds: u64) -> bool {
    ::std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .and_then(calendar_instant)
        .is_some()
}

/// Whether a `cron(...)` body states the instants it has to: six fields, each one the cron
/// grammar reads in the place it stands.
///
/// The `cron` crate reads the seven-field form (seconds first), so a literal `0` seconds
/// field is prepended, and a `?` in the day-of-month or day-of-week field is the `*` that
/// grammar spells the same statement with. A field stating one of this service's own
/// dialect forms ([`names_service_wildcard`]) is a statement that grammar does not read at
/// all, so it is accepted on the service's grammar instead of being refused on the
/// parser's.
fn cron_expression_states_its_instants(body: &str) -> bool {
    let fields: Vec<&str> = body.split_whitespace().collect();
    if fields.len() != CRON_FIELDS {
        return false;
    }
    let service_dialect = fields
        .iter()
        .enumerate()
        .any(|(index, field)| names_service_wildcard(index, field));
    for (index, field) in fields.iter().enumerate() {
        if !cron_field_parses(index, field) && !names_service_wildcard(index, field) {
            return false;
        }
    }
    // Every field reads on its own; the whole expression is read once more so a
    // combination of fields the grammar refuses is refused too — except for a body in this
    // service's own dialect, which that grammar cannot read as a whole either.
    service_dialect || normalised_cron(&fields).parse::<::cron::Schedule>().is_ok()
}

/// Whether `field`, standing at `index` among the six a `cron(...)` body carries, states one
/// of the forms this service's own cron dialect adds to the grammar the `cron` crate reads.
///
/// The forms, from the wildcards table of the service's cron-and-rate reference, each only
/// in the field it is documented for: `L` (the last day) in day-of-month and in day-of-week,
/// `<day>W` (the nearest weekday to a day of the month) in day-of-month, and
/// `<day-of-week>#<nth>` (the nth weekday of the month) in day-of-week. A dialect token
/// anywhere else states nothing the service documents, so it is left to that grammar — which
/// refuses it.
///
/// Matching the form is what keeps an ordinary token out of this: `WED` ends in no `W` of
/// its own and `JUL` is not the `L` wildcard, so both are still read — and refused — by that
/// grammar rather than waved through as dialect. The day a form names is read by that same
/// grammar in the field it stands in, so a day of the month or of the week that does not
/// exist is no dialect form either.
fn names_service_wildcard(index: usize, field: &str) -> bool {
    // The last day of the month or of the week, on its own or as the last weekday of the
    // month (`LW`).
    if field == "L" || field == "LW" {
        return NO_SPECIFIC_VALUE_FIELDS.contains(&index);
    }
    // The nearest weekday to a day of the month: the day, then `W`.
    if let Some(day) = field.strip_suffix('W') {
        return index == DAY_OF_MONTH_FIELD && cron_field_parses(DAY_OF_MONTH_FIELD, day);
    }
    // The nth weekday of the month: the day of the week, then `#`, then which one.
    if let Some((day_of_week, nth)) = field.split_once('#') {
        return index == DAY_OF_WEEK_FIELD
            && cron_field_parses(DAY_OF_WEEK_FIELD, day_of_week)
            && nth
                .parse::<u8>()
                .is_ok_and(|nth| (1..=MAX_WEEKDAYS_IN_MONTH).contains(&nth));
    }
    false
}

/// The six documented fields as the seven-field expression the `cron` crate reads.
fn normalised_cron(fields: &[&str]) -> String {
    let mut expression = String::from(NEUTRAL_CRON_FIELDS[0]);
    for (index, field) in fields.iter().enumerate() {
        expression.push(' ');
        expression.push_str(cron_field_as_parsed(index, field));
    }
    expression
}

/// One field as the `cron` crate's grammar reads it: `?` ("no specific value") is `*` in
/// that grammar, in the two fields it is a statement in.
fn cron_field_as_parsed(index: usize, field: &str) -> &str {
    if field == "?" && NO_SPECIFIC_VALUE_FIELDS.contains(&index) {
        "*"
    } else {
        field
    }
}

/// Whether `field` is one the cron grammar reads in the place `index` names, judged by
/// parsing it there with every other field neutral — so the answer is about that field and
/// not about the rest of the expression.
fn cron_field_parses(index: usize, field: &str) -> bool {
    let mut expression = NEUTRAL_CRON_FIELDS;
    expression[index + 1] = cron_field_as_parsed(index, field);
    expression.join(" ").parse::<::cron::Schedule>().is_ok()
}

/// The zone `timezone` names, or the default one when it names none — which the write path
/// has already refused anything unreadable for
/// ([`schedule_timezone_violation`]).
fn zone_of(timezone: Option<&str>) -> ::chrono_tz::Tz {
    timezone
        .filter(|zone| !zone.is_empty())
        .and_then(|zone| zone.parse().ok())
        .unwrap_or(DEFAULT_ZONE)
}

/// The instants one window's schedule names, read in the zone the window states them in.
#[derive(Debug, Clone)]
pub enum WindowSchedule {
    /// `at(<instant>)`: the ONE instant that expression names, already resolved against
    /// the window's zone.
    Once(SystemTime),
    /// `rate(<value> <unit>)`: one occurrence every period, the series anchored at the
    /// instant the window was created, since a rate expression states the INTERVAL and no
    /// first instant of its own.
    Rate(Duration),
    /// `cron(<six fields>)`: an occurrence at each instant the expression names, read in
    /// the window's own zone.
    Cron(Box<::cron::Schedule>, ::chrono_tz::Tz),
}

impl WindowSchedule {
    /// The occurrences `schedule` names, read in `timezone`, or `None` when it names none
    /// this module can compute — text stating no documented form, or a `cron(...)` body in
    /// this service's own dialect.
    #[must_use]
    pub fn parse(schedule: &str, timezone: Option<&str>) -> Option<Self> {
        let zone = zone_of(timezone);
        match form_of(schedule)? {
            (ScheduleForm::At, body) => at_instant(body, zone).map(Self::Once),
            (ScheduleForm::Rate, body) => rate_period(body).map(Self::Rate),
            (ScheduleForm::Cron, body) => {
                let fields: Vec<&str> = body.split_whitespace().collect();
                if fields.len() != CRON_FIELDS {
                    return None;
                }
                normalised_cron(&fields)
                    .parse()
                    .ok()
                    .map(|parsed| Self::Cron(Box::new(parsed), zone))
            }
        }
    }

    /// The first occurrence strictly AFTER `after`, or `None` when this schedule names no
    /// instant beyond it — a one-time window whose instant has passed, or a cron
    /// expression whose year field is behind the clock.
    ///
    /// `anchor` is where a rate series is measured from (the instant the window was
    /// created), so the occurrences of one window's rate schedule are the same series
    /// whenever it is asked for rather than a period on from each caller.
    #[must_use]
    pub fn next_after(&self, after: SystemTime, anchor: SystemTime) -> Option<SystemTime> {
        match self {
            Self::Once(at) => (*at > after).then_some(*at),
            Self::Rate(period) => {
                // Computed rather than stepped: the search start is caller-influenced (a
                // window's activation date may be far ahead of the anchor), so the
                // occurrence is found by arithmetic on the period and never by a loop
                // whose length a request could choose.
                let period_seconds = period.as_secs().max(1);
                let elapsed = after
                    .duration_since(anchor)
                    .map(|since| since.as_secs())
                    .unwrap_or_default();
                let periods = elapsed
                    .checked_div(period_seconds)?
                    .checked_add(1)?
                    .checked_mul(period_seconds)?;
                anchor.checked_add(Duration::from_secs(periods))
            }
            Self::Cron(schedule, zone) => {
                let from = calendar_instant(after)?.with_timezone(zone);
                schedule
                    .after(&from)
                    .next()
                    .map(|instant| SystemTime::from(instant.with_timezone(&::chrono::Utc)))
            }
        }
    }

    /// Whether this schedule is one a `ScheduleOffset` applies to: the offset is "the
    /// number of days to wait after the date and time specified by a cron expression"
    /// (`CreateMaintenanceWindow$ScheduleOffset`), so it moves the occurrences of a cron
    /// schedule and of nothing else.
    #[must_use]
    pub fn takes_schedule_offset(&self) -> bool {
        matches!(self, Self::Cron(_, _))
    }
}

/// One clock reading as a calendar instant, or `None` when it falls outside the range the
/// date library represents.
///
/// Every reading of a `SystemTime` as a date goes through here rather than through the
/// library's own infallible conversion, which resolves the instant with an `unwrap`: a
/// `SystemTime` holds spans no calendar date exists for, and a schedule's period is
/// caller-supplied, so an instant past the calendar has to answer "no date" instead of
/// ending the process.
///
/// `pub(super)`, so the other slices of this state module that read a clock as a calendar
/// date — the monthly releases a published patch catalogue holds
/// ([`super::patch_catalog`]) — read it through this one conversion rather than a second
/// copy of it.
pub(super) fn calendar_instant(at: SystemTime) -> Option<::chrono::DateTime<::chrono::Utc>> {
    let (seconds, nanos) = match at.duration_since(::std::time::UNIX_EPOCH) {
        Ok(since) => (i64::try_from(since.as_secs()).ok()?, since.subsec_nanos()),
        // Before the epoch: the span back to it, as a negative second count.
        Err(earlier) => (
            i64::try_from(earlier.duration().as_secs())
                .ok()?
                .checked_neg()?,
            0,
        ),
    };
    ::chrono::Utc.timestamp_opt(seconds, nanos).single()
}

/// The instant an `at(...)` body names, read in `zone`.
///
/// A local time a zone's offset change leaves AMBIGUOUS (a clock that goes back over it)
/// resolves to the EARLIER of its two instants, and one that zone skips entirely names no
/// instant at all — the reading `chrono` gives a local time, so nothing here invents an
/// offset for either case.
fn at_instant(body: &str, zone: ::chrono_tz::Tz) -> Option<SystemTime> {
    let naive = ::chrono::NaiveDateTime::parse_from_str(body.trim(), AT_EXPRESSION_FORMAT).ok()?;
    let local = zone.from_local_datetime(&naive).earliest()?;
    Some(SystemTime::from(local.with_timezone(&::chrono::Utc)))
}

/// The instant an ISO-8601 Extended `MaintenanceWindowStringDateTime` names, read in
/// `zone` when the text carries no offset of its own, or `None` for text that names none.
///
/// `StartDate` and `EndDate` are modelled as plain STRINGS, so the framework parses
/// neither and the stored text is echoed back exactly as the request stated it; this is
/// only how the stored text is read when the next execution has to be placed inside the
/// window the two of them bound.
fn window_instant(value: &str, zone: ::chrono_tz::Tz) -> Option<SystemTime> {
    let value = value.trim();
    if let Ok(fixed) = ::chrono::DateTime::parse_from_rfc3339(value) {
        return Some(SystemTime::from(fixed.with_timezone(&::chrono::Utc)));
    }
    let naive = ::chrono::NaiveDateTime::parse_from_str(value, CIVIL_DATE_TIME_FORMAT).ok()?;
    let local = zone.from_local_datetime(&naive).earliest()?;
    Some(SystemTime::from(local.with_timezone(&::chrono::Utc)))
}

/// One instant as a `MaintenanceWindowStringDateTime`: ISO-8601 Extended, written in
/// `zone` — so a window whose schedule is read in a zone reports the instants it opens at
/// in that same zone, and one read at UTC reports them with the `Z` designator rather than
/// with a zero offset.
///
/// The components below the minute are stated only when they carry something
/// ([`INSTANT_MINUTE_FORMAT`]), and the finest the service holds is the millisecond, so a
/// closer reading of the clock is dropped rather than written.
///
/// `None` for an instant outside the calendar ([`calendar_instant`]): such an instant has no
/// ISO-8601 rendering, so nothing is reported for it.
fn rendered_instant(at: SystemTime, zone: ::chrono_tz::Tz) -> Option<String> {
    use ::chrono::Timelike as _;

    let local = calendar_instant(at)?.with_timezone(&zone);
    let milliseconds = local.nanosecond() / NANOS_PER_MILLISECOND;
    let local = local.with_nanosecond(milliseconds * NANOS_PER_MILLISECOND)?;

    let mut format = String::from(INSTANT_MINUTE_FORMAT);
    if local.second() != 0 || milliseconds != 0 {
        format.push_str(INSTANT_SECONDS_FORMAT);
        if milliseconds != 0 {
            format.push_str(INSTANT_MILLISECONDS_FORMAT);
        }
    }
    if local.offset().fix().local_minus_utc() == 0 {
        format.push_str(INSTANT_UTC_DESIGNATOR);
    } else {
        format.push_str(INSTANT_OFFSET_FORMAT);
    }
    Some(local.format(&format).to_string())
}

/// One maintenance window the account created, as the store holds it.
///
/// Every member a read of a window reports is here or derived from here, so a read
/// projects this record rather than inventing anything: what the create persisted is what
/// a later read reports.
#[derive(Debug, Clone)]
pub struct MaintenanceWindowRecord {
    /// The server-minted id this record is stored under ([`mint_window_id`]).
    pub window_id: String,
    /// The Region whose endpoint this window was created through.
    pub region: String,
    /// "The name of the maintenance window" — required on create, so a window always has
    /// one.
    pub name: String,
    /// "An optional description for the maintenance window", or `None` for a create that
    /// carried none.
    pub description: Option<String>,
    /// "The date and time, in ISO-8601 Extended format, for when you want the maintenance
    /// window to become active", held exactly as the request stated it.
    pub start_date: Option<String>,
    /// "The date and time, in ISO-8601 Extended format, for when you want the maintenance
    /// window to become inactive", held exactly as the request stated it.
    pub end_date: Option<String>,
    /// "The schedule of the maintenance window in the form of a cron or rate expression" —
    /// required on create.
    pub schedule: String,
    /// "The time zone that the scheduled maintenance window executions are based on, in
    /// Internet Assigned Numbers Authority (IANA) format", or `None` when the window
    /// states none and its schedule is read in UTC.
    pub schedule_timezone: Option<String>,
    /// "The number of days to wait after the date and time specified by a cron expression
    /// before running the maintenance window", or `None` for a window that waits none.
    pub schedule_offset: Option<i32>,
    /// "The duration of the maintenance window in hours" — required on create.
    pub duration: i32,
    /// "The number of hours before the end of the maintenance window that […] Systems
    /// Manager stops scheduling new tasks for execution" — required on create, defaulted
    /// to 0 by the model.
    pub cutoff: i32,
    /// "Enables a maintenance window task to run on managed nodes, even if you haven't
    /// registered those nodes as targets" — required on create, defaulted to false by the
    /// model.
    pub allow_unassociated_targets: bool,
    /// "Indicates whether the maintenance window is enabled"
    /// ([`CREATED_ENABLED`] until an update says otherwise).
    pub enabled: bool,
    /// The "user-provided idempotency token" the create carried, or `None` when it carried
    /// none: what makes a retry of that create report the window it already minted rather
    /// than minting a second one.
    pub client_token: Option<String>,
    /// The window's tags, keyed by tag key, as the create supplied them and as the tag
    /// operations rewrite them (`super::tagging`).
    pub tags: BTreeMap<String, String>,
    /// When the create ran, which is also where a `rate(...)` schedule's series is
    /// anchored.
    pub created_date: SystemTime,
    /// Which create of this store minted the window: the value the store's mint counter
    /// stood at ([`super::ComAmazonawsSsmAmazonssmState::next_maintenance_window_id`]).
    ///
    /// A listing of the account's windows reports them in the order they were created, and
    /// the clock alone does not order two creates that land inside one of its ticks, so the
    /// counter — which rises once per create and never repeats — is what makes that order
    /// total and genuinely creation-ordered.
    pub created_sequence: u64,
    /// When the window was last written. Equal to [`created_date`](Self::created_date)
    /// until something updates the window.
    pub modified_date: SystemTime,
}

impl MaintenanceWindowRecord {
    /// The zone this window's schedule and bounding instants are read in.
    fn zone(&self) -> ::chrono_tz::Tz {
        zone_of(self.schedule_timezone.as_deref())
    }

    /// The instants this window's stored schedule names, or `None` when the stored
    /// schedule names none this emulator can place (this service's own cron dialect).
    #[must_use]
    pub fn schedule_occurrences(&self) -> Option<WindowSchedule> {
        WindowSchedule::parse(&self.schedule, self.schedule_timezone.as_deref())
    }

    /// "The next time the maintenance window will actually run, taking into account any
    /// specified times for the maintenance window to become active or inactive"
    /// (`MaintenanceWindowIdentity$NextExecutionTime`), or `None` when the window will not
    /// run again.
    ///
    /// The FIRST of the instants this window is scheduled to open at after `now`
    /// ([`Self::scheduled_execution_times`]) — so a single read of a window and a report of
    /// its upcoming executions answer from the one series rather than from two readings of
    /// the schedule, and the value moves on its own as the clock does rather than being
    /// frozen at the instant of a write.
    #[must_use]
    pub fn next_execution_time(&self, now: SystemTime) -> Option<String> {
        self.scheduled_execution_times(now, None, 1)
            .into_iter()
            .next()
    }

    /// The instants this window is scheduled to OPEN at that fall after `after` and before
    /// `until` when one is stated, in ascending order: at most `limit` of them.
    ///
    /// Both bounds a caller asks for are EXCLUSIVE and both are bounds on the instant the
    /// window opens at — the instant the schedule names moved on by the whole days a
    /// `ScheduleOffset` waits — so a window that waits is reported by when it RUNS and not by
    /// when its expression fired.
    ///
    /// The series is read from the stored schedule, the stored activation window and the
    /// window's zone, so it moves on its own as the clock does and nothing about it is
    /// stored. A window contributes no instant at all when it is disabled ("whether the
    /// maintenance window is enabled"), when its schedule names none this emulator can place,
    /// or when a bound it STATES names no instant this reader can place — such a bound leaves
    /// the interval the window would run in unknown, so nothing is reported for it rather than
    /// instants computed as though the window carried no bound at all. The series ends at
    /// `until`, at the window's own inactivation — which an execution standing exactly at it
    /// still runs on, the window being active up to that instant — or at the last instant the
    /// schedule names.
    #[must_use]
    pub fn scheduled_executions(
        &self,
        after: SystemTime,
        until: Option<SystemTime>,
        limit: usize,
    ) -> Vec<SystemTime> {
        let mut opens_at: Vec<SystemTime> = Vec::new();
        if !self.enabled || limit == 0 {
            return opens_at;
        }
        let zone = self.zone();
        let Some(occurrences) = self.schedule_occurrences() else {
            return opens_at;
        };
        // "for when you want the maintenance window to become active […] allows you to
        // delay activation of the maintenance window until the specified future date": the
        // series starts at the later of the bound asked for and that activation, so a window
        // whose activation is still ahead first runs at the first instant after it.
        let mut opens_after = after;
        if let Some(stated) = self.stated_start_date() {
            match window_instant(stated, zone) {
                Some(active_from) => opens_after = opens_after.max(active_from),
                None => return opens_at,
            }
        }
        // "for when you want the maintenance window to become inactive": the window runs no
        // more once that instant has passed — an execution standing AT it still runs, the
        // window being active up to it.
        let mut inactive_from = None;
        if let Some(stated) = self.stated_end_date() {
            match window_instant(stated, zone) {
                Some(stated) => inactive_from = Some(stated),
                None => return opens_at,
            }
        }
        // The offset waits whole days after each instant the cron expression named, and
        // applies to a cron schedule and to nothing else
        // ([`WindowSchedule::takes_schedule_offset`]).
        let offset = self
            .schedule_offset
            .filter(|_| occurrences.takes_schedule_offset())
            .filter(|days| *days > 0)
            .and_then(|days| u32::try_from(days).ok())
            .unwrap_or_default();
        let waited = ONE_DAY * offset;
        // Every bound is a bound on the instant the window OPENS at, while the schedule names
        // the instant the wait is counted from — so each bound is carried back over that wait
        // and the walk takes the schedule's own instants against it. Counting the wait forward
        // instead would drop an execution that opens inside the bound from a named instant
        // that stands before it.
        let Some(mut cursor) = opens_after.checked_sub(waited) else {
            return opens_at;
        };
        let named_no_later_than = match (until, inactive_from) {
            (None, None) => None,
            // The bound asked for is EXCLUSIVE at both ends ("only those scheduled before or
            // after a certain date and time"), so the last instant it admits is one
            // millisecond before it — the resolution this service states an instant at —
            // while the window's own inactivation admits an execution standing exactly at it.
            (until, inactive_from) => {
                let asked_last = match until {
                    None => None,
                    Some(until) => match until.checked_sub(Duration::from_millis(1)) {
                        Some(last) => Some(last),
                        // A bound so early that nothing at all stands before it.
                        None => return opens_at,
                    },
                };
                // The last instant the SCHEDULE may name for the execution it opens to still
                // to be admitted: the narrower of the two bounds, carried back over the wait.
                let last = [asked_last, inactive_from]
                    .into_iter()
                    .flatten()
                    .min()
                    .and_then(|last| last.checked_sub(waited));
                match last {
                    Some(last) => Some(last),
                    // Every instant the schedule could name opens past the bound.
                    None => return opens_at,
                }
            }
        };
        while opens_at.len() < limit {
            let Some(named) = occurrences.next_after(cursor, self.created_date) else {
                break;
            };
            // Every pass moves the search past the instant just named, so the walk takes
            // `limit` steps at most and no occurrence is reported twice.
            cursor = named;
            if named_no_later_than.is_some_and(|last| named > last) {
                break;
            }
            let Some(opens) = named.checked_add(waited) else {
                break;
            };
            opens_at.push(opens);
        }
        opens_at
    }

    /// The same instants as the `MaintenanceWindowStringDateTime`s this window reports them
    /// as ([`Self::execution_time`]), the series stopping at the first instant that has no
    /// such rendering.
    #[must_use]
    pub fn scheduled_execution_times(
        &self,
        after: SystemTime,
        until: Option<SystemTime>,
        limit: usize,
    ) -> Vec<String> {
        self.scheduled_executions(after, until, limit)
            .into_iter()
            .map_while(|at| self.execution_time(at))
            .collect()
    }

    /// One instant this window opens at, as this window reports it: written in the zone the
    /// window states its schedule in ([`rendered_instant`]), so every instant a window
    /// reports is read in the one zone.
    #[must_use]
    pub fn execution_time(&self, at: SystemTime) -> Option<String> {
        rendered_instant(at, self.zone())
    }

    /// The activation instant this window STATES, or `None` when it states none — a member
    /// sent EMPTY names nothing, the way this crate reads every optional string off the wire
    /// (`super::non_empty`).
    fn stated_start_date(&self) -> Option<&str> {
        self.start_date
            .as_deref()
            .map(str::trim)
            .filter(|stated| !stated.is_empty())
    }

    /// The inactivation instant this window states, read the same way.
    fn stated_end_date(&self) -> Option<&str> {
        self.end_date
            .as_deref()
            .map(str::trim)
            .filter(|stated| !stated.is_empty())
    }

    /// The modelled `MaintenanceWindowIdentity` for this record: "information about the
    /// maintenance window" as a listing reports it, every member projected from what the
    /// write persisted, with `NextExecutionTime` read from the stored schedule and the
    /// clock the same way a single read of the window reads it
    /// ([`Self::next_execution_time`]).
    #[must_use]
    pub fn to_maintenance_window_identity(&self, now: SystemTime) -> MaintenanceWindowIdentity {
        MaintenanceWindowIdentity {
            window_id: Some(self.window_id.clone()),
            name: Some(self.name.clone()),
            description: self.description.clone(),
            enabled: self.enabled,
            duration: Some(self.duration),
            cutoff: self.cutoff,
            schedule: Some(self.schedule.clone()),
            schedule_timezone: self.schedule_timezone.clone(),
            schedule_offset: self.schedule_offset,
            // Modelled as plain strings, so each is reported exactly as the write stated it.
            end_date: self.end_date.clone(),
            start_date: self.start_date.clone(),
            next_execution_time: self.next_execution_time(now),
        }
    }

    /// Whether the filter `key`=`values` selects this window.
    ///
    /// Each key narrows by one property of the window, and a filter's values UNION: a
    /// window is selected when one of that filter's values is its value for the key. A key
    /// outside [`WINDOW_FILTER_KEYS`] selects nothing — its caller refuses such a key
    /// before anything is asked about the windows.
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        match key {
            "Name" => values.contains(&self.name),
            "Enabled" => values
                .iter()
                .any(|value| boolean_filter_value(value) == Some(self.enabled)),
            _ => false,
        }
    }

    /// The members a create SUPPLIED, rendered as one canonical string.
    ///
    /// This is what "the parameters don't match the original call to the API with the same
    /// idempotency token" (`IdempotentParameterMismatch`) is decided on: two creates
    /// carrying one idempotency token are the same call when their supplied members render
    /// the same way. The minted id, the Region, the token itself and the instants are not
    /// supplied members, so they are not in it.
    #[must_use]
    pub fn creation_fingerprint(&self) -> String {
        let mut rendered = String::new();
        let _ = write!(
            rendered,
            "name={}\ndescription={}\nstart={}\nend={}\nschedule={}\ntimezone={}\n\
             offset={}\nduration={}\ncutoff={}\nunassociated={}\n",
            self.name,
            self.description.as_deref().unwrap_or_default(),
            self.start_date.as_deref().unwrap_or_default(),
            self.end_date.as_deref().unwrap_or_default(),
            self.schedule,
            self.schedule_timezone.as_deref().unwrap_or_default(),
            self.schedule_offset.unwrap_or_default(),
            self.duration,
            self.cutoff,
            self.allow_unassociated_targets,
        );
        rendered.push_str("tags=");
        for (key, value) in &self.tags {
            let _ = write!(rendered, "[{key}={value}]");
        }
        rendered
    }
}

/// Mint the `WindowId` of a maintenance window being created: [`WINDOW_ID_PREFIX`]
/// followed by [`WINDOW_ID_HEX_DIGITS`] lowercase hex digits.
///
/// Through the crate's one minter of a prefixed hex identifier
/// ([`super::minted_prefixed_hex_id`]): the digits are the leading bytes of a digest of the
/// store's own mint counter and the instant of the create, so two windows of one store
/// never carry one id and an id carries nothing from the request that created it.
#[must_use]
pub fn mint_window_id(sequence: u64, at: SystemTime) -> String {
    super::minted_prefixed_hex_id(WINDOW_ID_PREFIX, WINDOW_ID_HEX_DIGITS, sequence, at)
}

impl ComAmazonawsSsmAmazonssmState {
    /// The maintenance window `window_id` names in `region`, or `None` when that Region
    /// holds none — which is what a not-found refusal reports.
    pub fn maintenance_window(
        &self,
        region: &str,
        window_id: &str,
    ) -> Option<&MaintenanceWindowRecord> {
        self.maintenance_windows
            .get(&(region.to_string(), window_id.to_string()))
    }

    /// Every maintenance window `region` holds, in the order they were created — the order
    /// a listing of the account's windows reports them in, by the mint counter each create
    /// recorded ([`MaintenanceWindowRecord::created_sequence`]), with the minted id breaking
    /// a tie so the order is total and a page can resume at an id.
    pub fn maintenance_windows_in(&self, region: &str) -> Vec<&MaintenanceWindowRecord> {
        let mut held: Vec<&MaintenanceWindowRecord> = self
            .maintenance_windows
            .iter()
            .filter(|((region_of, _), _)| region_of == region)
            .map(|(_, window)| window)
            .collect();
        held.sort_by(|left, right| {
            left.created_sequence
                .cmp(&right.created_sequence)
                .then_with(|| left.window_id.cmp(&right.window_id))
        });
        held
    }

    /// The same window, to write it.
    pub fn maintenance_window_mut(
        &mut self,
        region: &str,
        window_id: &str,
    ) -> Option<&mut MaintenanceWindowRecord> {
        self.maintenance_windows
            .get_mut(&(region.to_string(), window_id.to_string()))
    }

    /// Store `record` as a maintenance window of its own Region, keyed by its id.
    pub fn insert_maintenance_window(&mut self, record: MaintenanceWindowRecord) {
        let key = (record.region.clone(), record.window_id.clone());
        self.maintenance_windows.insert(key, record);
    }

    /// Whether this store can take another maintenance window
    /// ([`MAX_MAINTENANCE_WINDOWS`]).
    pub fn can_hold_another_maintenance_window(&self) -> bool {
        self.maintenance_windows.len() < MAX_MAINTENANCE_WINDOWS
    }

    /// Mint the id of a maintenance window being created in this store.
    pub fn next_maintenance_window_id(&mut self, at: SystemTime) -> String {
        self.maintenance_window_sequence += 1;
        mint_window_id(self.maintenance_window_sequence, at)
    }

    /// The window `region` holds under the idempotency token `client_token`, or `None`
    /// when no create carrying that token has run there.
    pub fn maintenance_window_with_client_token(
        &self,
        region: &str,
        client_token: &str,
    ) -> Option<&MaintenanceWindowRecord> {
        self.maintenance_windows
            .iter()
            .filter(|((held, _), _)| held == region)
            .map(|(_, record)| record)
            .find(|record| record.client_token.as_deref() == Some(client_token))
    }
}

#[cfg(test)]
mod tests {
    //! The rules this module owns, each over a SET of inputs rather than one recorded
    //! schedule. The same rules are driven through real requests in
    //! `tests/all/maintenance_windows.rs`.

    use super::*;

    /// One row of [`the_next_execution_is_the_schedules_next_instant`]: what a failing
    /// assertion calls it, the schedule, the zone it is read in, the days it waits, and
    /// the instant reported.
    type NextExecutionRow = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<i32>,
        &'static str,
    );

    /// One row of [`a_window_that_will_not_run_reports_no_next_execution`]: what a failing
    /// assertion calls it, and how that row's window differs from the base one.
    type WindowVariantRow = (&'static str, fn(&mut MaintenanceWindowRecord));

    /// One row of [`the_creation_fingerprint_covers_every_supplied_member`]: a
    /// [`WindowVariantRow`] beside whether the two creates are the same call.
    type FingerprintRow = (&'static str, fn(&mut MaintenanceWindowRecord), bool);

    /// An instant as a clock reading, which the rows below measure from.
    fn epoch(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Every documented form of a schedule, well-formed and malformed, is read as the
    /// service documents it — and a refusal states the expression the request carried.
    #[test]
    fn a_schedule_states_one_of_the_documented_forms() {
        // (case, schedule, states a schedule)
        let rows: [(&str, &str, bool); 32] = [
            ("a rate of minutes", "rate(30 minutes)", true),
            ("a rate of one minute", "rate(1 minute)", true),
            ("a plural unit with one", "rate(1 minutes)", true),
            ("a singular unit with five", "rate(5 minute)", true),
            ("a rate of hours", "rate(12 hours)", true),
            ("a rate of days", "rate(7 days)", true),
            ("a rate of a thousand days", "rate(1000 days)", true),
            ("a rate of zero", "rate(0 minutes)", false),
            ("a rate with no unit", "rate(5)", false),
            ("a rate in a unit nothing measures", "rate(5 fortnights)", false),
            ("a fractional rate", "rate(1.5 hours)", false),
            // A period whose first occurrence falls off the calendar: no date exists for
            // such a series to open at, so it states no schedule.
            ("a rate past the calendar", "rate(99999999999 days)", false),
            ("a rate of the largest figure there is", "rate(18446744073709551615 minutes)", false),
            ("a six-field cron", "cron(0 16 ? * TUE *)", true),
            ("a cron naming weekdays", "cron(0 18 ? * MON-FRI *)", true),
            ("a cron over a step", "cron(0/15 * * * ? *)", true),
            // The wildcards this service's own dialect documents.
            ("a cron on the last day of the month", "cron(0 8 L * ? *)", true),
            ("a cron on the nearest weekday", "cron(0 8 15W * ? *)", true),
            ("a cron on the third Tuesday", "cron(30 23 ? * TUE#3 *)", true),
            // Ordinary tokens that merely CARRY a dialect character are still read by the
            // shared grammar: one it names is served and one it does not is refused.
            ("a cron naming one weekday by name", "cron(0 8 ? * WED *)", true),
            ("a cron naming a weekday that is not one", "cron(0 8 ? * WEDX *)", false),
            ("a cron naming a day of the month that is not one", "cron(0 8 32W * ? *)", false),
            ("a cron on a weekday of no such ordinal", "cron(0 8 ? * TUE#x *)", false),
            ("a cron of five fields", "cron(0 8 1 * ?)", false),
            ("a cron of prose", "cron(not a cron)", false),
            // A dialect form states something only in the field it is documented for, so one
            // standing anywhere else is read \u2014 and refused \u2014 by the shared grammar.
            ("the last day as a minute", "cron(L 8 ? * MON *)", false),
            ("the nearest weekday as an hour", "cron(0 15W ? * MON *)", false),
            ("the nth weekday as a month", "cron(0 8 ? TUE#3 ? *)", false),
            ("the last day as a day of the week", "cron(0 8 ? * L *)", true),
            ("an at instant", "at(2030-01-01T00:00:00)", true),
            ("an at instant carrying a zone", "at(2030-01-01T00:00:00Z)", false),
            ("no form at all", "every 30 minutes", false),
        ];

        for (case, schedule, states_a_schedule) in rows {
            let violation = schedule_violation(schedule);
            assert_eq!(
                violation.is_none(),
                states_a_schedule,
                "case {case:?}: expected states_a_schedule={states_a_schedule}, got \
                 {violation:?}"
            );
            if let Some(message) = violation {
                assert_eq!(
                    message, INVALID_SCHEDULE,
                    "case {case:?}: the refusal is worded as this error class is, got \
                     {message:?}"
                );
            }
        }
    }

    /// A schedule time zone is one the IANA database carries, and an absent member is no
    /// violation at all.
    #[test]
    fn a_schedule_timezone_names_an_iana_zone() {
        // (case, timezone, names a zone)
        let rows: [(&str, Option<&str>, bool); 7] = [
            ("no zone at all", None, true),
            ("UTC", Some("UTC"), true),
            ("an American zone", Some("America/Los_Angeles"), true),
            ("an Asian zone", Some("Asia/Seoul"), true),
            ("a zone nothing names", Some("Not/AZone"), false),
            ("an offset rather than a zone", Some("+01:00"), false),
            ("prose", Some("here"), false),
        ];

        for (case, timezone, names_a_zone) in rows {
            let violation = schedule_timezone_violation(timezone);
            assert_eq!(
                violation.is_none(),
                names_a_zone,
                "case {case:?}: expected names_a_zone={names_a_zone}, got {violation:?}"
            );
        }
    }

    /// One row of [`the_series_is_the_schedules_successive_instants`]: what a failing
    /// assertion calls it, the schedule, the zone it is read in, the days it waits, how many
    /// instants are asked for, and the instants reported.
    type ScheduleSeriesRow = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<i32>,
        usize,
        &'static [&'static str],
    );

    /// One row of [`the_schedule_filters_bound_the_range_reported`]: what a failing assertion
    /// calls it, the filters stated, and the span they admit.
    type ScheduleSpanRow = (
        &'static str,
        Vec<PatchOrchestratorFilter>,
        Option<(Option<SystemTime>, Option<SystemTime>)>,
    );

    /// One window, so the rows below differ only along the member each is about.
    fn window(schedule: &str) -> MaintenanceWindowRecord {
        MaintenanceWindowRecord {
            window_id: mint_window_id(1, epoch(1_893_456_000)),
            region: "us-east-1".to_string(),
            name: "a-window".to_string(),
            description: None,
            start_date: None,
            end_date: None,
            schedule: schedule.to_string(),
            schedule_timezone: None,
            schedule_offset: None,
            duration: 2,
            cutoff: 1,
            allow_unassociated_targets: false,
            enabled: CREATED_ENABLED,
            client_token: None,
            tags: BTreeMap::new(),
            // 2030-01-01T00:00:00Z, a Tuesday.
            created_date: epoch(1_893_456_000),
            created_sequence: 1,
            modified_date: epoch(1_893_456_000),
        }
    }

    /// The next execution is the first instant the stored schedule names after the clock,
    /// read in the window's own zone; the rows differ along the form of the schedule, the
    /// zone it is read in and the offset it waits.
    #[test]
    fn the_next_execution_is_the_schedules_next_instant() {
        // 2030-01-01T00:00:00Z, a Tuesday.
        let now = epoch(1_893_456_000);
        // (case, schedule, timezone, offset, the instant reported)
        let rows: [NextExecutionRow; 7] = [
            ("a cron at an hour of the day", "cron(0 8 * * ? *)", None, None, "2030-01-01T08:00Z"),
            (
                "the same cron in a western zone",
                "cron(0 8 * * ? *)",
                Some("America/Los_Angeles"),
                None,
                "2030-01-01T08:00-08:00",
            ),
            (
                "a cron on one day of the week",
                "cron(0 0 ? * WED *)",
                None,
                None,
                "2030-01-02T00:00Z",
            ),
            (
                "the same cron waiting two days",
                "cron(0 0 ? * WED *)",
                None,
                Some(2),
                "2030-01-04T00:00Z",
            ),
            ("a rate of hours", "rate(6 hours)", None, None, "2030-01-01T06:00Z"),
            // An offset states a wait after a CRON instant, so a rate schedule keeps its
            // own series.
            ("a rate carrying an offset", "rate(6 hours)", None, Some(3), "2030-01-01T06:00Z"),
            (
                "a one-time instant still ahead",
                "at(2030-06-01T12:00:00)",
                None,
                None,
                "2030-06-01T12:00Z",
            ),
        ];

        for (case, schedule, timezone, offset, expected) in rows {
            let mut held = window(schedule);
            held.schedule_timezone = timezone.map(str::to_string);
            held.schedule_offset = offset;
            let next = held.next_execution_time(now);
            assert_eq!(
                next.as_deref(),
                Some(expected),
                "case {case:?}: expected the next execution at {expected:?}, got {next:?}"
            );
        }
    }

    /// A window that will not run again reports no next execution: the rows differ along
    /// the reason it will not.
    #[test]
    fn a_window_that_will_not_run_reports_no_next_execution() {
        let now = epoch(1_893_456_000);
        // (case, how the window differs)
        let rows: [WindowVariantRow; 7] = [
            ("a disabled window", |held| held.enabled = false),
            ("a one-time instant already passed", |held| {
                held.schedule = "at(2020-01-01T00:00:00)".to_string();
            }),
            ("an activation window already closed", |held| {
                held.end_date = Some("2029-12-31T00:00:00Z".to_string());
            }),
            // A bound the window STATES that names no instant leaves the interval it would
            // run in unknown, so nothing is reported rather than an instant computed as
            // though the bound were absent.
            ("an activation stated in no readable form", |held| {
                held.start_date = Some("not-a-date".to_string());
            }),
            ("an inactivation stated in no readable form", |held| {
                held.end_date = Some("whenever".to_string());
            }),
            ("an activation stated as a date with no time", |held| {
                held.start_date = Some("2030-03-01".to_string());
            }),
            // This service's own cron dialect names instants the shared grammar cannot
            // place, so no instant is reported for one.
            ("a schedule in the service's own dialect", |held| {
                held.schedule = "cron(0 8 L * ? *)".to_string();
            }),
        ];

        for (case, differ) in rows {
            let mut held = window("cron(0 8 * * ? *)");
            differ(&mut held);
            let next = held.next_execution_time(now);
            assert_eq!(
                next, None,
                "case {case:?}: expected no next execution, got {next:?}"
            );
        }
    }

    /// A window whose activation is still ahead first runs after it becomes active, and
    /// one whose activation has passed runs on its own schedule.
    #[test]
    fn an_activation_date_moves_the_first_execution() {
        let now = epoch(1_893_456_000);
        // (case, start date, the instant reported)
        let rows: [(&str, Option<&str>, &str); 5] = [
            ("no activation at all", None, "2030-01-01T08:00Z"),
            ("an activation already passed", Some("2029-01-01T00:00:00Z"), "2030-01-01T08:00Z"),
            ("an activation stated empty", Some(""), "2030-01-01T08:00Z"),
            ("an activation still ahead", Some("2030-03-01T00:00:00Z"), "2030-03-01T08:00Z"),
            // A civil instant with no offset of its own is read in the window's zone.
            ("an activation stated without a zone", Some("2030-03-01T00:00:00"), "2030-03-01T08:00Z"),
        ];

        for (case, start_date, expected) in rows {
            let mut held = window("cron(0 8 * * ? *)");
            held.start_date = start_date.map(str::to_string);
            let next = held.next_execution_time(now);
            assert_eq!(
                next.as_deref(),
                Some(expected),
                "case {case:?}: expected the next execution at {expected:?}, got {next:?}"
            );
        }
    }

    /// A rate schedule's series is anchored at the instant the window was created, so the
    /// next occurrence is the first period boundary after the search start — computed, not
    /// stepped, however far ahead a caller's activation date puts that start.
    #[test]
    fn a_rate_series_is_anchored_at_the_create() {
        let now = epoch(1_893_456_000);
        let mut held = window("rate(1 minute)");
        // A century past the anchor: a series stepped one minute at a time would walk
        // 52 million periods to reach it.
        held.start_date = Some("2130-01-01T00:00:00Z".to_string());
        let next = held.next_execution_time(now);
        assert_eq!(
            next.as_deref(),
            Some("2130-01-01T00:01Z"),
            "expected the first period boundary after the activation, got {next:?}"
        );
    }

    /// An instant reported as a window's next execution is written in ISO-8601 Extended
    /// form, stating the components BELOW the minute only when they carry something: the
    /// seconds when they are not zero, the milliseconds beside them when they are not
    /// either, and a closer reading of the clock not at all. The rows differ along the
    /// components the instant carries, the form of the schedule that named it and the zone
    /// it is read in.
    #[test]
    fn a_next_execution_states_the_components_it_carries() {
        // 2030-01-01T00:00:13.040Z: an anchor that is no whole minute, so a series measured
        // from it lands on instants with seconds and milliseconds of their own.
        let odd_anchor = epoch(1_893_456_013) + Duration::from_millis(40);
        // The same anchor read closer than the service holds an instant.
        let finer_than_a_millisecond = odd_anchor + Duration::from_nanos(123_456);
        // (case, schedule, the instant the create recorded, the zone, the instant reported)
        let rows: [(&str, &str, SystemTime, Option<&str>, &str); 6] = [
            (
                "a cron naming a whole minute",
                "cron(0 8 * * ? *)",
                epoch(1_893_456_000),
                None,
                "2030-01-01T08:00Z",
            ),
            (
                "a rate anchored off the minute",
                "rate(1 hour)",
                odd_anchor,
                None,
                "2030-01-01T01:00:13.040Z",
            ),
            (
                "the same rate read in a western zone",
                "rate(1 hour)",
                odd_anchor,
                Some("America/Los_Angeles"),
                "2029-12-31T17:00:13.040-08:00",
            ),
            // Whole seconds carry no milliseconds to state.
            (
                "a rate anchored on a whole second",
                "rate(1 hour)",
                epoch(1_893_456_013),
                None,
                "2030-01-01T01:00:13Z",
            ),
            // A reading finer than the millisecond the service holds states nothing
            // further.
            (
                "a rate anchored finer than a millisecond",
                "rate(1 hour)",
                finer_than_a_millisecond,
                None,
                "2030-01-01T01:00:13.040Z",
            ),
            (
                "a one-time instant naming a whole minute",
                "at(2030-06-01T12:00:00)",
                epoch(1_893_456_000),
                None,
                "2030-06-01T12:00Z",
            ),
        ];

        for (case, schedule, created_date, timezone, expected) in rows {
            let mut held = window(schedule);
            held.created_date = created_date;
            held.modified_date = created_date;
            held.schedule_timezone = timezone.map(str::to_string);
            // 2030-01-01T00:00:20Z: past every anchor above, so each row reports an
            // instant still ahead of the clock.
            let next = held.next_execution_time(epoch(1_893_456_020));
            assert_eq!(
                next.as_deref(),
                Some(expected),
                "case {case:?}: expected the next execution at {expected:?}, got {next:?}"
            );
        }
    }

    /// A window's cutoff has to be SMALLER than its duration, each figure's own modelled
    /// range notwithstanding: the rows differ along the figures and cover each published
    /// bound of both ranges (`Cutoff` 0..23, `Duration` 1..24) and the pairs on either side
    /// of the rule.
    #[test]
    fn a_cutoff_stays_under_the_duration() {
        // (case, cutoff, duration, is refused)
        let rows: [(&str, i32, i32, bool); 9] = [
            ("no cutoff under the smallest duration", 0, 1, false),
            ("a cutoff one hour short of the duration", 1, 2, false),
            ("the largest cutoff under the largest duration", 23, 24, false),
            ("no cutoff under the largest duration", 0, 24, false),
            ("a cutoff equal to the duration", 2, 2, true),
            ("a cutoff one hour past the duration", 2, 1, true),
            ("the largest cutoff with the smallest duration", 23, 1, true),
            ("the largest cutoff equal to the duration", 23, 23, true),
            ("no cutoff is never refused", 0, 2, false),
        ];

        for (case, cutoff, duration, refused) in rows {
            let violation = cutoff_violation(cutoff, duration);
            assert_eq!(
                violation.is_some(),
                refused,
                "case {case:?}: expected refused={refused} for cutoff {cutoff} and duration \
                 {duration}, got {violation:?}"
            );
            if let Some(message) = violation {
                let bound = duration - 1;
                assert_eq!(
                    message,
                    format!("Cutoff was {cutoff} but must be smaller than {bound} (Duration  - 1 )"),
                    "case {case:?}: the refusal is worded as this error class is, got \
                     {message:?}"
                );
            }
        }
    }

    /// Two creates carrying one idempotency token are the same call exactly when the
    /// members they supplied are the same, whichever member differs.
    #[test]
    fn the_creation_fingerprint_covers_every_supplied_member() {
        // (case, how the second create differs, renders the same way)
        let rows: [FingerprintRow; 9] = [
            ("nothing at all", |_| {}, true),
            // Neither the minted id nor the instants are supplied members.
            ("the minted id", |held| held.window_id = mint_window_id(9, epoch(1)), true),
            ("the instants", |held| held.created_date = epoch(42), true),
            ("the name", |held| held.name = "another-window".to_string(), false),
            ("the description", |held| held.description = Some("new".to_string()), false),
            ("the schedule", |held| held.schedule = "rate(1 day)".to_string(), false),
            ("the duration", |held| held.duration = 3, false),
            ("the cutoff", |held| held.cutoff = 0, false),
            ("a tag", |held| {
                held.tags.insert("Env".to_string(), "prod".to_string());
            }, false),
        ];

        for (case, differ, same_call) in rows {
            let first = window("cron(0 8 * * ? *)");
            let mut second = window("cron(0 8 * * ? *)");
            differ(&mut second);
            let matches = first.creation_fingerprint() == second.creation_fingerprint();
            assert_eq!(
                matches, same_call,
                "case {case:?}: expected same_call={same_call}, got {matches} for \
                 {:?} and {:?}",
                first.creation_fingerprint(),
                second.creation_fingerprint()
            );
        }
    }

    /// A listing of one Region's windows is ordered by the create that minted each, even
    /// when the clock cannot tell two creates apart, and holds that Region's windows alone.
    ///
    /// The rows differ along the Region a window was created through and along whether the
    /// instants the creates recorded order them.
    #[test]
    fn the_region_listing_is_ordered_by_the_create_that_minted_each() {
        let mut store = ComAmazonawsSsmAmazonssmState::default();
        // (case, the Region, the instant the create recorded)
        let rows: [(&str, &str, u64); 4] = [
            ("the first window here", "us-east-1", 1_893_456_000),
            // One tick of a clock that cannot tell these two apart.
            ("a window created in the same tick", "us-east-1", 1_893_456_000),
            ("a window created later", "us-east-1", 1_893_456_030),
            ("a window of another Region", "eu-central-1", 1_893_456_010),
        ];
        let mut minted: Vec<(&str, String, &str)> = Vec::new();
        for (case, region, at) in rows {
            let at = epoch(at);
            let mut record = window("cron(0 8 * * ? *)");
            record.region = region.to_string();
            record.created_date = at;
            record.modified_date = at;
            record.window_id = store.next_maintenance_window_id(at);
            record.created_sequence = store.maintenance_window_sequence;
            minted.push((case, record.window_id.clone(), region));
            store.insert_maintenance_window(record);
        }

        for region in ["us-east-1", "eu-central-1"] {
            let expected: Vec<String> = minted
                .iter()
                .filter(|(_, _, held)| *held == region)
                .map(|(_, window_id, _)| window_id.clone())
                .collect();
            let reported: Vec<String> = store
                .maintenance_windows_in(region)
                .into_iter()
                .map(|held| held.window_id.clone())
                .collect();
            assert_eq!(
                reported, expected,
                "case {region:?}: expected the Region's windows in the order they were \
                 created, got {reported:?}"
            );
        }
    }

    /// A filter selects a window exactly when one of its values is the window's value for
    /// the key it names. The rows differ along the key, the values under it and the window
    /// the filter is asked about — including a DISABLED window, the state only an update
    /// puts a window in.
    #[test]
    fn a_filter_selects_the_windows_whose_value_it_names() {
        // (case, the key, its values, whether an enabled window and a disabled one are
        // selected)
        let rows: [(&str, &str, &[&str], bool, bool); 9] = [
            ("the window's own name", "Name", &["a-window"], true, true),
            ("another window's name", "Name", &["other-window"], false, false),
            (
                "either name, whose values union",
                "Name",
                &["other-window", "a-window"],
                true,
                true,
            ),
            ("no value at all", "Name", &[], false, false),
            // "For example […] Enabled=True": the value is read as the boolean it spells
            // however it is cased.
            ("enabled, as the documented example cases it", "Enabled", &["True"], true, false),
            ("enabled, lowercased", "Enabled", &["true"], true, false),
            ("not enabled, uppercased", "Enabled", &["FALSE"], false, true),
            ("either state, whose values union", "Enabled", &["true", "false"], true, true),
            ("a value spelling neither boolean", "Enabled", &["maybe"], false, false),
        ];

        for (case, key, values, selects_enabled, selects_disabled) in rows {
            let values: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
            let enabled = window("cron(0 8 * * ? *)");
            let mut disabled = window("cron(0 8 * * ? *)");
            disabled.enabled = false;
            for (state, held, expected) in [
                ("enabled", &enabled, selects_enabled),
                ("disabled", &disabled, selects_disabled),
            ] {
                let selected = held.matches_filter(key, &values);
                assert_eq!(
                    selected, expected,
                    "case {case:?} against the {state} window: expected selected={expected}, \
                     got {selected}"
                );
            }
            // A key outside the documented set names no property of a window, so it selects
            // none; its caller refuses such a key before asking.
            assert!(
                !enabled.matches_filter("NotAKey", &values),
                "case {case:?}: expected an undocumented key to select nothing"
            );
        }
    }

    /// A page token is a window id exactly when it carries the modelled
    /// `MaintenanceWindowId` form (`^mw-[0-9a-f]{17}$`): the rows differ along the prefix,
    /// the number of digits and the alphabet they are drawn from.
    #[test]
    fn a_window_id_carries_the_modelled_form() {
        let minted = mint_window_id(7, epoch(1_893_456_000));
        // (case, the text, whether it is a window id)
        let rows: [(&str, &str, bool); 8] = [
            ("an id this service minted", minted.as_str(), true),
            ("the shortest and longest hex digit", "mw-00000000000000000", true),
            ("every digit at its maximum", "mw-fffffffffffffffff", true),
            ("one digit short", "mw-0000000000000000", false),
            ("one digit long", "mw-000000000000000000", false),
            ("no prefix at all", "00000000000000000", false),
            ("hex in upper case, which the pattern excludes", "mw-FFFFFFFFFFFFFFFFF", false),
            ("a digit outside the hex alphabet", "mw-0000000000000000g", false),
        ];

        for (case, text, expected) in rows {
            let read = is_maintenance_window_id(text);
            assert_eq!(
                read, expected,
                "case {case:?}: expected is_maintenance_window_id({text:?})={expected}, got {read}"
            );
        }
    }

    /// Every filter key this read takes is one the service documents, and the page it serves
    /// stays inside the modelled `MaintenanceWindowMaxResults` range (10..100) — including
    /// at each bound and one past it.
    #[test]
    fn the_filter_keys_and_the_page_bounds_are_the_modelled_ones() {
        // The documented set: "Supported filter keys are Name and Enabled".
        assert_eq!(
            WINDOW_FILTER_KEYS,
            &["Name", "Enabled"],
            "expected the documented filter keys, got {WINDOW_FILTER_KEYS:?}"
        );
        for key in WINDOW_FILTER_KEYS {
            assert!(
                is_window_filter_key(key),
                "case {key:?}: expected a documented key to be taken"
            );
        }
        assert!(
            !is_window_filter_key("name"),
            "expected the filter keys to be read case-sensitively"
        );

        // (case, the MaxResults asked for, the page served)
        let rows: [(&str, Option<i32>, Option<usize>); 6] = [
            ("no bound at all", None, Some(MAX_WINDOW_PAGE_SIZE as usize)),
            ("the smallest page", Some(MIN_WINDOW_PAGE_SIZE), Some(MIN_WINDOW_PAGE_SIZE as usize)),
            ("the largest page", Some(MAX_WINDOW_PAGE_SIZE), Some(MAX_WINDOW_PAGE_SIZE as usize)),
            ("one page in from the top", Some(MAX_WINDOW_PAGE_SIZE - 1), Some((MAX_WINDOW_PAGE_SIZE - 1) as usize)),
            ("one below the smallest", Some(MIN_WINDOW_PAGE_SIZE - 1), None),
            ("one above the largest", Some(MAX_WINDOW_PAGE_SIZE + 1), None),
        ];
        for (case, asked, expected) in rows {
            let served = window_page_size(asked).ok();
            assert_eq!(
                served, expected,
                "case {case:?}: expected window_page_size({asked:?})={expected:?}, got {served:?}"
            );
        }
    }

    /// The series a window is scheduled over is the successive instants its schedule names,
    /// the first of them being the next execution a single read of the window reports: the
    /// rows differ along the form of the schedule, the zone it is read in, the days it waits
    /// and how many instants are asked for.
    #[test]
    fn the_series_is_the_schedules_successive_instants() {
        // 2030-01-01T00:00:00Z, a Tuesday.
        let now = epoch(1_893_456_000);
        // (case, schedule, timezone, offset, how many are asked for, the instants reported)
        let rows: [ScheduleSeriesRow; 6] = [
            (
                "three days of a daily cron",
                "cron(0 8 * * ? *)",
                None,
                None,
                3,
                &["2030-01-01T08:00Z", "2030-01-02T08:00Z", "2030-01-03T08:00Z"],
            ),
            (
                "the same cron read in a western zone",
                "cron(0 8 * * ? *)",
                Some("America/Los_Angeles"),
                None,
                2,
                &["2030-01-01T08:00-08:00", "2030-01-02T08:00-08:00"],
            ),
            (
                "a weekly cron waiting two whole days",
                "cron(0 0 ? * WED *)",
                None,
                Some(2),
                2,
                &["2030-01-04T00:00Z", "2030-01-11T00:00Z"],
            ),
            (
                "a rate series anchored at the create",
                "rate(6 hours)",
                None,
                None,
                3,
                &["2030-01-01T06:00Z", "2030-01-01T12:00Z", "2030-01-01T18:00Z"],
            ),
            (
                "a one-time instant, which the series holds once",
                "at(2030-06-01T12:00:00)",
                None,
                None,
                4,
                &["2030-06-01T12:00Z"],
            ),
            (
                "no instant asked for at all",
                "cron(0 8 * * ? *)",
                None,
                None,
                0,
                &[],
            ),
        ];

        for (case, schedule, timezone, offset, limit, expected) in rows {
            let mut held = window(schedule);
            held.schedule_timezone = timezone.map(str::to_string);
            held.schedule_offset = offset;
            let series = held.scheduled_execution_times(now, None, limit);
            assert_eq!(
                series,
                expected
                    .iter()
                    .map(|at| (*at).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?}: expected {expected:?}, got {series:?}"
            );
            // The next execution a single read reports is the first of the same series —
            // asked for the series itself rather than for a page of none of it.
            if limit > 0 {
                assert_eq!(
                    held.next_execution_time(now).as_deref(),
                    expected.first().copied(),
                    "case {case:?}: expected the next execution to be the series' first instant"
                );
            }
        }

        // The series stops at the bound asked for, and at the window's own inactivation
        // whichever of the two comes first.
        let daily = window("cron(0 8 * * ? *)");
        let until = epoch(1_893_456_000 + 3 * 86_400);
        assert_eq!(
            daily.scheduled_execution_times(now, Some(until), 10),
            vec![
                "2030-01-01T08:00Z".to_string(),
                "2030-01-02T08:00Z".to_string(),
                "2030-01-03T08:00Z".to_string(),
            ],
            "case \"a bound inside the series\": expected the series to stop at it"
        );
        let mut inactive = daily.clone();
        inactive.end_date = Some("2030-01-02T00:00:00Z".to_string());
        assert_eq!(
            inactive.scheduled_execution_times(now, Some(until), 10),
            vec!["2030-01-01T08:00Z".to_string()],
            "case \"a window that becomes inactive first\": expected the series to stop there"
        );

        // A bound standing exactly ON an execution: the bound asked for is exclusive, so that
        // execution is past the range, while the window's own inactivation admits one standing
        // at it.
        let at_an_execution = epoch(1_893_456_000 + 86_400 + 8 * 3_600);
        assert_eq!(
            daily.scheduled_execution_times(now, Some(at_an_execution), 10),
            vec!["2030-01-01T08:00Z".to_string()],
            "case \"a bound standing on an execution\": expected that execution to be past the \
             range"
        );
        let mut inactive_then = daily.clone();
        inactive_then.end_date = Some("2030-01-02T08:00:00Z".to_string());
        assert_eq!(
            inactive_then.scheduled_execution_times(now, None, 10),
            vec![
                "2030-01-01T08:00Z".to_string(),
                "2030-01-02T08:00Z".to_string(),
            ],
            "case \"a window inactive from an execution's own instant\": expected that \
             execution to run"
        );

        // Every bound is a bound on the instant the window OPENS at, which a `ScheduleOffset`
        // moves whole days past the instant the expression named: an execution opening inside
        // the bound is reported even though the instant it was named at stands outside it.
        let mut waits = window("cron(0 0 ? * WED *)");
        waits.schedule_offset = Some(2);
        // The Wednesday is 2030-01-02T00:00Z, so the window opens on the 4th; the bound stands
        // between the two.
        let between = epoch(1_893_456_000 + 2 * 86_400);
        assert_eq!(
            waits.scheduled_execution_times(between, None, 1),
            vec!["2030-01-04T00:00Z".to_string()],
            "case \"a window that waits whole days\": expected the opening instant to be taken \
             against the bound"
        );
        assert_eq!(
            waits.scheduled_execution_times(now, Some(epoch(1_893_456_000 + 4 * 86_400)), 5),
            vec!["2030-01-04T00:00Z".to_string()],
            "case \"a window that waits whole days, bounded above\": expected the opening \
             instant to be taken against that bound too"
        );
    }

    /// A page cursor round-trips through the token it is handed out as, and no value this
    /// service did not hand out reads as one: the rows differ along what makes the text
    /// something else.
    #[test]
    fn a_schedule_cursor_round_trips_only_through_a_token_it_minted() {
        let window_id = mint_window_id(3, epoch(1_893_456_000));
        for at in [
            epoch(0),
            epoch(1_893_456_000),
            ::std::time::UNIX_EPOCH + Duration::from_millis(1_893_456_000_123),
        ] {
            let cursor = ScheduleCursor::of(at, &window_id);
            let token = cursor.page_token();
            assert_eq!(
                ScheduleCursor::of_token(&token),
                Some(cursor.clone()),
                "case {token:?}: expected the cursor to round-trip"
            );
            // The search a resumed page runs starts before the instant the cursor names, so
            // the occurrence it names is the first one that search finds.
            assert!(
                cursor.search_from() < at.max(::std::time::UNIX_EPOCH + Duration::from_millis(1)),
                "case {token:?}: expected the search to start before the instant named"
            );
        }

        // (case, the text)
        let rows: [(&str, &str); 6] = [
            ("empty", ""),
            ("prose", "not-a-token"),
            ("an instant with no window", "1893456000000"),
            ("a position that is no instant", "later/mw-00000000000000000"),
            ("a position past the count a cursor holds", "99999999999999999999/mw-00000000000000000"),
            ("a position naming no window", "1893456000000/not-a-window"),
        ];
        for (case, token) in rows {
            assert_eq!(
                ScheduleCursor::of_token(token),
                None,
                "case {case:?}: expected no cursor for {token:?}"
            );
        }
    }

    /// The filters a schedule report takes bound the range of instants it serves: each
    /// filter's values union, the filters intersect, and a filter admitting no instant
    /// admits nothing at all. The page it serves stays inside the bounds this read states.
    #[test]
    fn the_schedule_filters_bound_the_range_reported() {
        assert_eq!(
            SCHEDULE_FILTER_KEYS,
            &["Before", "After"],
            "expected the two ends of the documented range, got {SCHEDULE_FILTER_KEYS:?}"
        );
        for key in SCHEDULE_FILTER_KEYS {
            assert!(
                is_schedule_filter_key(key),
                "case {key:?}: expected a documented key to be taken"
            );
        }
        assert!(
            !is_schedule_filter_key("before"),
            "expected the filter keys to be read case-sensitively"
        );

        let filter = |key: &str, values: &[&str]| PatchOrchestratorFilter {
            key: Some(key.to_string()),
            values: Some(values.iter().map(|value| (*value).to_string()).collect()),
        };
        let at = |seconds: u64| Some(epoch(seconds));
        // 2030-01-01T00:00:00Z and the two days after it.
        let first = 1_893_456_000;
        // (case, the filters, the span they admit)
        let rows: Vec<ScheduleSpanRow> = vec![
            ("no filter at all", vec![], Some((None, None))),
            (
                "one instant to be after",
                vec![filter("After", &["2030-01-01T00:00:00Z"])],
                Some((at(first), None)),
            ),
            (
                "an instant written only to the minute",
                vec![filter("Before", &["2030-01-02T00:00Z"])],
                Some((None, at(first + 86_400))),
            ),
            (
                "values that union on the earliest instant to be after",
                vec![filter(
                    "After",
                    &["2030-01-03T00:00:00Z", "2030-01-01T00:00:00Z"],
                )],
                Some((at(first), None)),
            ),
            (
                "values that union on the latest instant to be before",
                vec![filter(
                    "Before",
                    &["2030-01-03T00:00:00Z", "2030-01-01T00:00:00Z"],
                )],
                Some((None, at(first + 2 * 86_400))),
            ),
            (
                "both ends, which intersect",
                vec![
                    filter("After", &["2030-01-01T00:00:00Z"]),
                    filter("Before", &["2030-01-03T00:00:00Z"]),
                ],
                Some((at(first), at(first + 2 * 86_400))),
            ),
            (
                "two filters of one key, the narrower bound standing",
                vec![
                    filter("Before", &["2030-01-03T00:00:00Z"]),
                    filter("Before", &["2030-01-02T00:00:00Z"]),
                ],
                Some((None, at(first + 86_400))),
            ),
            (
                "a value naming no instant",
                vec![filter("Before", &["whenever"])],
                None,
            ),
            ("a filter carrying no value", vec![filter("After", &[])], None),
        ];
        for (case, filters, expected) in rows {
            let span = schedule_filter_span(&filters);
            assert_eq!(
                span, expected,
                "case {case:?}: expected {expected:?}, got {span:?}"
            );
        }

        // (case, the MaxResults asked for, the page served)
        let pages: [(&str, Option<i32>, Option<usize>); 5] = [
            ("no bound at all", None, Some(MAX_SCHEDULE_PAGE_SIZE as usize)),
            ("the smallest page", Some(MIN_SCHEDULE_PAGE_SIZE), Some(MIN_SCHEDULE_PAGE_SIZE as usize)),
            ("the largest page", Some(MAX_SCHEDULE_PAGE_SIZE), Some(MAX_SCHEDULE_PAGE_SIZE as usize)),
            ("one below the smallest", Some(MIN_SCHEDULE_PAGE_SIZE - 1), None),
            ("one above the largest", Some(MAX_SCHEDULE_PAGE_SIZE + 1), None),
        ];
        for (case, asked, expected) in pages {
            let served = schedule_page_size(asked).ok();
            assert_eq!(
                served, expected,
                "case {case:?}: expected schedule_page_size({asked:?})={expected:?}, got {served:?}"
            );
        }
    }
}
