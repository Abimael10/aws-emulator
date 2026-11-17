// SPDX-License-Identifier: CC0-1.0

//! The CHANGE CALENDAR entry an SSM document of that type carries, and the state it leaves
//! a calendar in at one instant.
//!
//! A Change Calendar is an SSM document whose content is an iCalendar (RFC 5545) entry:
//! the calendar is OPEN or CLOSED by default ([`CALENDAR_TYPE_PROPERTY`]) and each `VEVENT`
//! it holds covers the spans of time in which it takes the OTHER state — events of a
//! default-open calendar are its closed periods and events of a default-closed calendar are
//! its open periods. So the state at an instant is a function of the entry's own content:
//! nothing here is stored beside the document, and reading a calendar writes nothing.
//!
//! The content is read by the maintained iCalendar reader (`icalendar`) and a recurring
//! event's `RRULE`/`RDATE`/`EXDATE` are expanded by the maintained recurrence
//! implementation (`rrule`), so this module states only what Change Calendar adds on top of
//! RFC 5545: the default-state property, that an event covers the opposite state, and that
//! several calendars read together intersect ([`state_at`]).
//!
//! ## What bounds the work, given the content is the caller's
//!
//! A document's content and the list of calendars a read names are both caller-supplied, so
//! every quantity one read can grow is bounded, and the bounds do not merely multiply:
//!
//! * the walk over the entry's component tree is iterative, with a bounded worklist, a
//!   nesting limit ([`MAX_COMPONENT_NESTING`]) and a visit limit ([`MAX_COMPONENTS`]) —
//!   both measured on the raw bytes BEFORE the iCalendar reader descends through them
//!   ([`guard_calendar_components`]) and again as the tree is walked, so caller-chosen
//!   nesting drives no recursion;
//! * one entry holds at most [`MAX_ENTRIES`] events, counted as the walk collects them;
//! * ONE read asks the recurrence engine for at most [`MAX_OCCURRENCE_STEPS_PER_READ`]
//!   occurrences ACROSS every event of every calendar it names
//!   ([`OccurrenceBudget`]) — the single bound on the product of the per-calendar,
//!   per-event and per-occurrence counts, and what also bounds the spans and the transition
//!   candidates one read holds;
//! * the search for the next transition stops at [`TRANSITION_HORIZON_DAYS`] and walks at
//!   most [`MAX_TRANSITION_CANDIDATES`] boundaries;
//! * a read whose calendars ask for more than that is REFUSED rather than answered from a
//!   truncated expansion, because a silently truncated expansion reports a state the
//!   calendar does not have.

use std::collections::VecDeque;

use super::*;

use ::chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// The `DocumentType` of a Change Calendar entry, from the modelled `DocumentType` enum
/// (`crate::ops::types::DOCUMENT_TYPE_VALUES`). A read of a calendar accepts no other
/// type.
pub const CHANGE_CALENDAR_DOCUMENT_TYPE: &str = "ChangeCalendar";

/// "An OPEN calendar indicates that actions are allowed to proceed"
/// (`GetCalendarStateResponse$State`), from the modelled `CalendarState` enum
/// (`crate::ops::types::CALENDAR_STATE_VALUES`).
pub const CALENDAR_STATE_OPEN: &str = "OPEN";

/// "A CLOSED calendar indicates that actions aren't allowed to proceed"
/// (`GetCalendarStateResponse$State`), the other modelled `CalendarState` value.
pub const CALENDAR_STATE_CLOSED: &str = "CLOSED";

/// The iCalendar property a Change Calendar entry states its DEFAULT state in: the state
/// the calendar is in outside every event it holds.
const CALENDAR_TYPE_PROPERTY: &str = "X-CALENDAR-TYPE";

/// The [`CALENDAR_TYPE_PROPERTY`] value of a calendar that is OPEN outside its events, so
/// each event is a closed period.
const CALENDAR_TYPE_DEFAULT_OPEN: &str = "DEFAULT_OPEN";

/// The [`CALENDAR_TYPE_PROPERTY`] value of a calendar that is CLOSED outside its events, so
/// each event is an open period.
const CALENDAR_TYPE_DEFAULT_CLOSED: &str = "DEFAULT_CLOSED";

/// The iCalendar component the whole entry is wrapped in.
const VCALENDAR_COMPONENT: &str = "VCALENDAR";

/// The iCalendar component one event of the entry is.
const VEVENT_COMPONENT: &str = "VEVENT";

/// The properties an event is read through: when it starts, when it ends or how long it
/// lasts, and the recurrence that repeats it.
const DTSTART_PROPERTY: &str = "DTSTART";

/// See [`DTSTART_PROPERTY`].
const DTEND_PROPERTY: &str = "DTEND";

/// See [`DTSTART_PROPERTY`].
const DURATION_PROPERTY: &str = "DURATION";

/// The recurrence properties of an event, which are handed to the recurrence reader
/// together with the event's `DTSTART`.
const RECURRENCE_PROPERTIES: &[&str] = &["RRULE", "RDATE", "EXDATE"];

/// The `TZID` parameter of a date-time property: the IANA time zone its civil time is
/// stated in.
const TZID_PARAMETER: &str = "TZID";

/// The iCalendar basic formats a date-time and a date are written in (RFC 5545's
/// `date-time` and `date` value types).
const ICALENDAR_DATE_TIME_FORMAT: &str = "%Y%m%dT%H%M%S";

/// See [`ICALENDAR_DATE_TIME_FORMAT`].
const ICALENDAR_DATE_FORMAT: &str = "%Y%m%d";

/// The format an `ISO8601String` of this operation is written in: UTC, to the second, the
/// way Systems Manager reports `AtTime` and `NextTransitionTime`.
const ISO8601_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// How far past the instant a read asks about this service looks for the calendars' next
/// transition.
///
/// A calendar's content is caller-supplied and a recurrence can name occurrences forever,
/// so the search has to stop somewhere; a year and a day covers the next occurrence of any
/// recurrence a calendar can state, up to a yearly one. Past it no transition is reported,
/// which is what `NextTransitionTime` being optional says: nothing is known to change.
const TRANSITION_HORIZON_DAYS: i64 = 366;

/// The most events one calendar entry may hold, counted as the entry's components are
/// walked. Above it the entry is not one this service reads, rather than one it spends
/// unbounded work on.
const MAX_ENTRIES: usize = 512;

/// How deep the component tree of a calendar entry may nest, and how many components one
/// entry may hold.
///
/// The nesting is the caller's to choose, so the walk is iterative with a bounded worklist
/// ([`collected`]) rather than a descent that would exhaust the stack: an entry past either
/// bound is refused.
const MAX_COMPONENT_NESTING: usize = 16;

/// See [`MAX_COMPONENT_NESTING`].
const MAX_COMPONENTS: usize = 4096;

/// The most transition candidates one read walks: the earliest this many span boundaries
/// after the instant asked about.
const MAX_TRANSITION_CANDIDATES: usize = 1024;

/// The most calendars ONE read may name. The request states the list, so without a bound
/// the work a single call asks for grows with it; above this the read is refused rather
/// than served slowly.
pub const MAX_CALENDARS_PER_READ: usize = 64;

/// The occurrences ONE read may ask the recurrence engine for, ACROSS every event of every
/// calendar it names.
///
/// This is the bound on the PRODUCT: the per-calendar, per-event and per-occurrence counts
/// would otherwise multiply, and this budget is spent by every occurrence the engine
/// produces — the ones before the instant asked about, which the engine has to walk to
/// reach it, as well as the ones inside the search window. It therefore also bounds the
/// spans and the transition candidates one read holds in memory. A read whose calendars
/// need more is refused.
pub const MAX_OCCURRENCE_STEPS_PER_READ: usize = 65_536;

/// What asking the recurrence engine for ONE event's occurrences costs the read's budget,
/// before any occurrence it returns.
///
/// An event whose rule names nothing around the instant asked about returns no occurrence,
/// so without this charge it would spend nothing while still costing the engine a walk —
/// and a calendar may hold [`MAX_ENTRIES`] of them. Charging the ask itself bounds how many
/// events ONE read expands at all (the budget divided by this), and so the engine walks and
/// the recurrence reads one request can drive.
const OCCURRENCE_QUERY_COST: usize = 16;

/// The years an instant a calendar read works with may fall in: the range a four-digit
/// ISO-8601 year spans.
///
/// Every length, span and transition is computed from these instants, so bounding them here
/// is what keeps that arithmetic inside the range the date library represents — a calendar's
/// content is caller-supplied, and a date far outside this range is not one a change
/// calendar states.
const READABLE_YEARS: ::std::ops::RangeInclusive<i32> = 1..=9999;

/// Whether `at` is an instant this service reads ([`READABLE_YEARS`]).
fn is_readable(at: DateTime<Utc>) -> bool {
    READABLE_YEARS.contains(&::chrono::Datelike::year(&at))
}

/// How a calendar read states that the name it was given names no Change Calendar
/// document.
///
/// `InvalidDocument` is "the specified SSM document doesn't exist", and a read of a
/// calendar words that miss in terms of the CALENDAR document it was looking for rather
/// than in the words a plain document read uses
/// ([`super::document_not_found_message`]): fixed text around the name the request asked
/// for, which is the only part of the request interpolated.
pub fn calendar_not_found_message(name: &str) -> String {
    format!("The specified Change Calendar Document '{name}' does not exist.")
}

/// How a calendar read states that the document the name it was given DOES name is not a
/// Change Calendar.
///
/// `InvalidDocumentType` is "the SSM document type isn't valid", and the sentence states
/// both halves of what is wrong: a document of that name exists, and its type is not the
/// calendar type.
pub fn not_a_calendar_message(name: &str) -> String {
    format!(
        "A document exists with the name '{name}', but it is not of type \
         '{CHANGE_CALENDAR_DOCUMENT_TYPE}'."
    )
}

/// The state name a calendar in this state reports.
pub fn calendar_state_name(open: bool) -> &'static str {
    if open {
        CALENDAR_STATE_OPEN
    } else {
        CALENDAR_STATE_CLOSED
    }
}

/// One instant as the `ISO8601String` this service reports it as.
pub fn rendered_instant(at: DateTime<Utc>) -> String {
    at.format(ISO8601_FORMAT).to_string()
}

/// The instant a read asks about beside the `AtTime` the answer reports, or why the
/// `AtTime` the request stated names no instant.
///
/// "If you don't specify a value or AtTime, the current time is used", and `AtTime` is
/// reported as "the time, as an ISO 8601 string, that you specified in your command" — so a
/// request that stated one has its own text reported back and a request that stated none is
/// reported the clock reading the answer was computed at.
pub fn requested_instant(
    at_time: Option<&str>,
    now: SystemTime,
) -> Result<(DateTime<Utc>, String), String> {
    let clock = maintenance_windows::calendar_instant(now)
        .ok_or_else(|| "the current time is not an instant this service can read".to_string())?;
    match at_time {
        None => Ok((clock, rendered_instant(clock))),
        Some(text) => {
            let at = read_iso8601(text.trim())
                .ok_or_else(|| format!("AtTime '{text}' isn't a time stated in ISO 8601 format"))?;
            Ok((at, text.to_string()))
        }
    }
}

/// The instant an ISO-8601 `AtTime` names, or `None` for text that names none.
///
/// An `AtTime` is "in ISO 8601 format", which is the Extended format carrying an offset —
/// the spelling every AWS SDK sends. Nothing wider is read here: a form Systems Manager
/// itself refuses must not be served as if it named an instant.
fn read_iso8601(value: &str) -> Option<DateTime<Utc>> {
    let at = DateTime::parse_from_rfc3339(value).ok()?.with_timezone(&Utc);
    is_readable(at).then_some(at)
}

/// One span of time an event of a calendar covers, half-open: the span's own end instant
/// is already outside it, so an event that ends when the next one starts leaves no instant
/// uncovered and no instant covered twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CalendarSpan {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

/// One event of a calendar: when its first occurrence starts, how long each occurrence
/// lasts, and the recurrence that repeats it.
#[derive(Debug, Clone)]
struct CalendarEntry {
    start: DateTime<Utc>,
    length: ::chrono::Duration,
    /// The recurrence the event states, read ONCE when the calendar was read: a query asks
    /// the engine through a clone of this rather than reading the content's lines again.
    /// `None` for an event that occurs once.
    recurrence: Option<::rrule::RRuleSet>,
}

/// One Change Calendar entry as this service reads it: the state the calendar is in outside
/// its events, and the events themselves.
#[derive(Debug, Clone)]
pub struct ChangeCalendar {
    /// Whether the calendar is OPEN outside its events
    /// ([`CALENDAR_TYPE_DEFAULT_OPEN`]).
    default_open: bool,
    entries: Vec<CalendarEntry>,
}

/// What ONE calendar contributes to a read: the state it is in outside its events, and the
/// spans its events cover around the instant asked about.
#[derive(Debug, Clone)]
pub struct CalendarView {
    default_open: bool,
    /// Sorted and merged ([`merged`]), so two spans neither overlap nor touch.
    spans: Vec<CalendarSpan>,
}

/// The occurrences ONE read may still ask the recurrence engine for
/// ([`MAX_OCCURRENCE_STEPS_PER_READ`]).
///
/// Held by the read rather than by a calendar, which is what makes it a bound on the
/// product: every calendar the read names spends from the same budget.
#[derive(Debug)]
pub struct OccurrenceBudget {
    left: usize,
}

impl OccurrenceBudget {
    /// The budget one read starts with.
    pub fn for_one_read() -> Self {
        Self {
            left: MAX_OCCURRENCE_STEPS_PER_READ,
        }
    }

    /// How many occurrences the engine may be asked for next: what is left of the budget,
    /// and never more than one request of the engine can report.
    fn request(&self) -> u16 {
        u16::try_from(self.left).unwrap_or(u16::MAX)
    }

    /// Spend `occurrences` of the budget, or report that the read has asked for more than
    /// one read walks.
    fn spend(&mut self, occurrences: usize) -> Result<(), String> {
        self.left = self.left.checked_sub(occurrences).ok_or_else(|| {
            format!(
                "its events name more occurrences around that time than this emulator walks \
                 in one request (at most {MAX_OCCURRENCE_STEPS_PER_READ})"
            )
        })?;
        Ok(())
    }
}

/// What a read of one or more calendars works out: the state at the instant it asked about,
/// and when that state next changes.
#[derive(Debug, Clone)]
pub struct CalendarStateAt {
    /// Whether the calendars are OPEN at that instant.
    pub open: bool,
    /// When the state changes next, or `None` when nothing changes it within the search
    /// horizon ([`TRANSITION_HORIZON_DAYS`]).
    pub next_transition: Option<DateTime<Utc>>,
}

/// How far ahead a read at `at` looks for the next transition
/// ([`TRANSITION_HORIZON_DAYS`]).
///
/// The one reading of the horizon, so the spans a calendar is viewed through and the
/// candidates the transition is looked for among cover the same window.
fn horizon(at: DateTime<Utc>) -> DateTime<Utc> {
    at.checked_add_signed(::chrono::Duration::days(TRANSITION_HORIZON_DAYS))
        .unwrap_or(at)
}

/// The state the calendars are in at `at`, and when it next changes.
///
/// "If you specify more than one calendar in a request, the command returns the status of
/// OPEN only if all calendars in the request are open. If one or more calendars in the
/// request are closed, the status returned is CLOSED" (`GetCalendarState`), so the state is
/// the INTERSECTION of the calendars' own states. The state is piecewise constant and only
/// an event boundary can change it, so the next transition is the earliest boundary after
/// `at` at which the intersection differs from the state at `at` — which is not necessarily
/// the earliest boundary at all, since one calendar closing while another is already closed
/// changes nothing.
///
/// A read with NO calendar is the empty intersection, which is open; the operation refuses
/// such a request before reaching here.
pub fn state_at(views: &[CalendarView], at: DateTime<Utc>) -> CalendarStateAt {
    let horizon = horizon(at);
    let open_at = |instant: DateTime<Utc>| {
        views
            .iter()
            .all(|view| view.default_open != covered(&view.spans, instant))
    };

    let open = open_at(at);
    // Bounded by the read's own occurrence budget: every span was paid for out of it, so
    // this list holds at most two instants per occurrence that budget allowed.
    let mut candidates: Vec<DateTime<Utc>> = views
        .iter()
        .flat_map(|view| view.spans.iter())
        .flat_map(|span| [span.start, span.end])
        .filter(|boundary| *boundary > at && *boundary <= horizon)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates.truncate(MAX_TRANSITION_CANDIDATES);

    CalendarStateAt {
        open,
        next_transition: candidates
            .into_iter()
            .find(|candidate| open_at(*candidate) != open),
    }
}

/// Whether one of `spans` covers `instant`.
///
/// `spans` is sorted and merged ([`merged`]), so the only span that can cover an instant is
/// the last one that starts at or before it.
fn covered(spans: &[CalendarSpan], instant: DateTime<Utc>) -> bool {
    let next = spans.partition_point(|span| span.start <= instant);
    next.checked_sub(1)
        .and_then(|index| spans.get(index))
        .is_some_and(|span| span.end > instant)
}

/// `spans` sorted by start and merged, so two spans of the list neither overlap nor touch.
fn merged(mut spans: Vec<CalendarSpan>) -> Vec<CalendarSpan> {
    spans.sort_unstable_by_key(|span| (span.start, span.end));
    let mut merged: Vec<CalendarSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

impl ChangeCalendar {
    /// The view of this calendar a read at `at` needs, or why this service does not read it
    /// around that instant.
    ///
    /// An occurrence that STARTED before `at` is included when it is still running there, so
    /// the state at `at` is read from the event covering it rather than from the next one to
    /// start. Every occurrence the recurrence engine is asked for is paid for out of
    /// `budget` — including the ones BEFORE the window, which the engine walks to reach it —
    /// and an expansion the budget cannot pay for is refused rather than truncated: a
    /// truncated expansion would report a state the calendar does not have.
    pub fn view(
        &self,
        at: DateTime<Utc>,
        budget: &mut OccurrenceBudget,
    ) -> Result<CalendarView, String> {
        let to = horizon(at);
        let mut spans = Vec::new();
        for entry in &self.entries {
            if entry.length <= ::chrono::Duration::zero() {
                // A zero-length event covers no instant, so it can neither change the
                // state nor be a transition.
                continue;
            }
            let earliest = at
                .checked_sub_signed(entry.length)
                .unwrap_or(DateTime::<Utc>::MIN_UTC);
            match &entry.recurrence {
                None => {
                    if entry.start >= earliest && entry.start <= to {
                        budget.spend(1)?;
                        spans.extend(entry.span(entry.start));
                    }
                }
                Some(recurrence) => {
                    for occurrence in entry.occurrences(recurrence, earliest, to, budget)? {
                        spans.extend(entry.span(occurrence));
                    }
                }
            }
        }
        Ok(CalendarView {
            default_open: self.default_open,
            spans: merged(spans),
        })
    }
}

impl CalendarEntry {
    /// The span the occurrence starting at `start` covers, or `None` for an occurrence
    /// whose end falls outside the range this service reads: such a span covers no instant
    /// a read can ask about.
    fn span(&self, start: DateTime<Utc>) -> Option<CalendarSpan> {
        Some(CalendarSpan {
            start,
            end: start.checked_add_signed(self.length)?,
        })
    }

    /// The occurrences of this recurring event that start anywhere in `[from, to]`, or why
    /// this service does not expand it.
    ///
    /// The engine produces occurrences from the event's own `DTSTART` forward, so reaching
    /// `from` costs it one occurrence per occurrence BEFORE `from`: those are counted
    /// first, against the read's budget, so the work the engine is asked to do is bounded by
    /// what the budget allows rather than by how far in the past the content put `DTSTART`.
    fn occurrences(
        &self,
        recurrence: &::rrule::RRuleSet,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        budget: &mut OccurrenceBudget,
    ) -> Result<Vec<DateTime<Utc>>, String> {
        let from_tz = from.with_timezone(&::rrule::Tz::UTC);
        let to_tz = to.with_timezone(&::rrule::Tz::UTC);
        // The ask itself, before any occurrence it returns: an event whose rule names
        // nothing here still costs the engine a walk.
        budget.spend(OCCURRENCE_QUERY_COST)?;

        let preceding = recurrence.clone().before(from_tz).all(budget.request());
        budget.spend(preceding.dates.len())?;
        if preceding.limited {
            return Err(format!(
                "its events name more occurrences before that time than this emulator walks \
                 in one request (at most {MAX_OCCURRENCE_STEPS_PER_READ})"
            ));
        }

        let occurrences = recurrence
            .clone()
            .after(from_tz)
            .before(to_tz)
            .all(budget.request());
        budget.spend(occurrences.dates.len())?;
        if occurrences.limited {
            return Err(format!(
                "its events name more occurrences around that time than this emulator walks \
                 in one request (at most {MAX_OCCURRENCE_STEPS_PER_READ})"
            ));
        }
        Ok(occurrences
            .dates
            .into_iter()
            .map(|occurrence| occurrence.with_timezone(&Utc))
            .collect())
    }
}

/// The Change Calendar entry `content` holds, or why it is not one this service reads.
///
/// The rejection is the sentence `UnsupportedCalendarException` — "the calendar entry
/// contained in the specified SSM document isn't supported" — is worded with: the content
/// is the document's own, so what is wrong with it is what the caller has to fix.
pub fn parse_calendar(content: &str) -> Result<ChangeCalendar, String> {
    let unfolded = ::icalendar::parser::unfold(content);
    // Applied BEFORE the reader is handed anything: it descends per nesting level, so the
    // depth and the breadth the bytes ask for are measured while there is still no tree to
    // measure them on.
    guard_calendar_components(&unfolded)?;
    let read = ::icalendar::parser::read_calendar(&unfolded)
        .map_err(|why| format!("its content isn't an iCalendar entry: {why}"))?;
    let (properties, events) = collected(&read)?;

    let default_open = match named_property(&properties, CALENDAR_TYPE_PROPERTY)
        .map(|property| property.val.as_str().trim().to_string())
    {
        // A calendar that states no type at all is the kind Change Calendar creates by
        // default: open, with each event a closed period.
        None => true,
        Some(value) if value.eq_ignore_ascii_case(CALENDAR_TYPE_DEFAULT_OPEN) => true,
        Some(value) if value.eq_ignore_ascii_case(CALENDAR_TYPE_DEFAULT_CLOSED) => false,
        Some(value) => {
            return Err(format!(
                "its {CALENDAR_TYPE_PROPERTY} '{value}' is neither \
                 {CALENDAR_TYPE_DEFAULT_OPEN} nor {CALENDAR_TYPE_DEFAULT_CLOSED}"
            ))
        }
    };

    let mut entries = Vec::with_capacity(events.len());
    for event in events {
        entries.push(read_entry(event)?);
    }
    Ok(ChangeCalendar {
        default_open,
        entries,
    })
}

/// Pre-flight bounds on the components of untrusted content, applied BEFORE the iCalendar
/// reader builds a tree from it.
///
/// This is a GUARD, not a parser: it counts the component delimiters of the UNFOLDED
/// content line by line, so folding cannot hide one, and it measures only quantities a
/// hiding trick can push UP and never DOWN — how many components the bytes open and how
/// deeply they nest. The reader descends once per nesting level, so the depth has to be
/// bounded while there is still no tree to measure; the same reason the document content
/// guard ([`super::guard_document_content`]) bounds YAML nesting before that parser runs.
fn guard_calendar_components(unfolded: &str) -> Result<(), String> {
    let starts = |line: &str, delimiter: &str| {
        line.get(..delimiter.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(delimiter))
    };
    let mut depth = 0usize;
    let mut components = 0usize;
    for line in unfolded.lines() {
        let line = line.trim_start();
        if starts(line, "BEGIN:") {
            depth += 1;
            components += 1;
            if depth > MAX_COMPONENT_NESTING {
                return Err(format!(
                    "its components nest deeper than the {MAX_COMPONENT_NESTING} levels this \
                     service walks"
                ));
            }
            if components > MAX_COMPONENTS {
                return Err(format!(
                    "it holds more than the {MAX_COMPONENTS} components this service walks"
                ));
            }
        } else if starts(line, "END:") {
            depth = depth.saturating_sub(1);
        }
    }
    Ok(())
}

/// The calendar-level properties and the events `read` holds, or why its component tree is
/// not one this service walks.
///
/// The nesting is the caller's to choose, so the walk is ITERATIVE over a worklist rather
/// than a recursive descent: nesting past [`MAX_COMPONENT_NESTING`], more than
/// [`MAX_COMPONENTS`] components or more than [`MAX_ENTRIES`] events are each refused as the
/// walk reaches them, so no caller-chosen depth or breadth is walked before the bound
/// applies. An entry whose `VCALENDAR` the reader hands back as a component of its own is
/// read the same way as one whose properties it hoists.
#[allow(clippy::type_complexity)]
fn collected<'a>(
    read: &'a ::icalendar::parser::Calendar<'a>,
) -> Result<
    (
        Vec<&'a ::icalendar::parser::Property<'a>>,
        Vec<&'a ::icalendar::parser::Component<'a>>,
    ),
    String,
> {
    let mut properties: Vec<&::icalendar::parser::Property> = read.properties.iter().collect();
    let mut events: Vec<&::icalendar::parser::Component> = Vec::new();
    let mut worklist: VecDeque<(&::icalendar::parser::Component, usize)> =
        read.components.iter().map(|component| (component, 1)).collect();
    let mut visited = 0usize;

    while let Some((component, depth)) = worklist.pop_front() {
        visited += 1;
        if visited > MAX_COMPONENTS {
            return Err(format!(
                "it holds more than the {MAX_COMPONENTS} components this service walks"
            ));
        }
        if depth > MAX_COMPONENT_NESTING {
            return Err(format!(
                "its components nest deeper than the {MAX_COMPONENT_NESTING} levels this \
                 service walks"
            ));
        }
        let name = component.name.as_str();
        if name.eq_ignore_ascii_case(VEVENT_COMPONENT) {
            if events.len() >= MAX_ENTRIES {
                return Err(format!(
                    "it holds more than the {MAX_ENTRIES} events this service reads"
                ));
            }
            events.push(component);
            continue;
        }
        if name.eq_ignore_ascii_case(VCALENDAR_COMPONENT) {
            properties.extend(component.properties.iter());
        }
        worklist.extend(
            component
                .components
                .iter()
                .map(|nested| (nested, depth + 1)),
        );
    }
    Ok((properties, events))
}

/// One event of a calendar as this service reads it, or why it is not one it reads.
fn read_entry(event: &::icalendar::parser::Component) -> Result<CalendarEntry, String> {
    let properties: Vec<&::icalendar::parser::Property> = event.properties.iter().collect();
    let dtstart = named_property(&properties, DTSTART_PROPERTY).ok_or_else(|| {
        format!("one of its {VEVENT_COMPONENT} events states no {DTSTART_PROPERTY}")
    })?;
    let (start, start_is_date) = read_date_time(dtstart)?;
    let length = read_length(&properties, start, start_is_date)?;
    if length < ::chrono::Duration::zero() {
        return Err(format!(
            "one of its {VEVENT_COMPONENT} events ends before it starts"
        ));
    }

    let recurring = |property: &::icalendar::parser::Property| {
        RECURRENCE_PROPERTIES
            .iter()
            .any(|name| property.name.as_str().eq_ignore_ascii_case(name))
    };
    let recurrence = if properties.iter().any(|property| recurring(property)) {
        // The lines handed to the recurrence reader are re-assembled from the properties
        // the content stated, so a value carrying a line break would contribute content
        // lines of its own and the recurrence expanded would not be the one the document
        // states. Such a value is refused rather than re-assembled.
        let mut lines = Vec::with_capacity(1 + properties.len());
        for property in [dtstart]
            .into_iter()
            .chain(properties.iter().copied().filter(|p| recurring(p)))
        {
            lines.push(rendered_line(property).ok_or_else(|| {
                format!(
                    "one of its {VEVENT_COMPONENT} events states a {} carrying a line break",
                    property.name.as_str()
                )
            })?);
        }
        let text = lines.join("\n");
        // Read ONCE here, so content whose recurrence this service cannot expand is
        // refused rather than silently contributing no occurrence, and a read of the
        // calendar asks the engine through a clone of what this read rather than through
        // the lines again.
        Some(text.parse::<::rrule::RRuleSet>().map_err(|why| {
            format!(
                "one of its {VEVENT_COMPONENT} events states a recurrence this service can't \
                 expand: {why}"
            )
        })?)
    } else {
        None
    };

    Ok(CalendarEntry {
        start,
        length,
        recurrence,
    })
}

/// How long each occurrence of an event lasts: the span to its `DTEND`, the `DURATION` it
/// states, or — for an event that states neither — the one day RFC 5545 gives an event
/// whose start is a DATE and nothing at all for one whose start is a date-time.
fn read_length(
    properties: &[&::icalendar::parser::Property],
    start: DateTime<Utc>,
    start_is_date: bool,
) -> Result<::chrono::Duration, String> {
    if let Some(dtend) = named_property(properties, DTEND_PROPERTY) {
        let (end, _) = read_date_time(dtend)?;
        // Both instants are inside the range this service reads, so the span between them
        // is one the date library represents.
        return Ok(end.signed_duration_since(start));
    }
    if let Some(duration) = named_property(properties, DURATION_PROPERTY) {
        return read_duration(duration.val.as_str().trim());
    }
    Ok(if start_is_date {
        ::chrono::Duration::days(1)
    } else {
        ::chrono::Duration::zero()
    })
}

/// The span an iCalendar `DURATION` states ("P1D", "PT2H30M", "P2W"), or why it is not one
/// this service reads.
///
/// RFC 5545's `dur-value` is made of weeks, days, hours, minutes and seconds — it carries
/// neither years nor months, so a value that states one is not a duration of a calendar
/// entry at all.
fn read_duration(value: &str) -> Result<::chrono::Duration, String> {
    let rejected = || {
        format!(
            "one of its {VEVENT_COMPONENT} events states a {DURATION_PROPERTY} '{value}' that \
             isn't an iCalendar duration"
        )
    };
    let parsed = ::iso8601::duration(value).map_err(|_| rejected())?;
    let span = match parsed {
        ::iso8601::Duration::Weeks(weeks) => {
            ::std::time::Duration::from_secs(u64::from(weeks) * 7 * 24 * 60 * 60)
        }
        ::iso8601::Duration::YMDHMS {
            year: 0, month: 0, ..
        } => ::std::time::Duration::from(parsed),
        ::iso8601::Duration::YMDHMS { .. } => return Err(rejected()),
    };
    ::chrono::Duration::from_std(span).map_err(|_| rejected())
}

/// The instant an iCalendar date-time property names, and whether it names a DATE rather
/// than a date-time — or why it names no instant.
///
/// A value ending in `Z` is UTC; one with a `TZID` parameter is a civil time in that IANA
/// zone, resolved to the EARLIER instant where the zone leaves it ambiguous; one with
/// neither is a floating time, which this endpoint reads at UTC.
fn read_date_time(
    property: &::icalendar::parser::Property,
) -> Result<(DateTime<Utc>, bool), String> {
    let value = property.val.as_str().trim();
    let rejected = || {
        format!(
            "one of its {VEVENT_COMPONENT} events states a {} '{value}' that isn't an \
             iCalendar date or date-time",
            property.name.as_str()
        )
    };
    let zone = match named_parameter(property, TZID_PARAMETER) {
        None => None,
        Some(tzid) => Some(tzid.parse::<::chrono_tz::Tz>().map_err(|_| {
            format!(
                "one of its {VEVENT_COMPONENT} events states a {TZID_PARAMETER} '{tzid}' that \
                 isn't an IANA time zone"
            )
        })?),
    };

    if let Some(body) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(body, ICALENDAR_DATE_TIME_FORMAT)
            .map_err(|_| rejected())?;
        return Ok((readable(Utc.from_utc_datetime(&naive), &rejected)?, false));
    }
    if value.contains('T') {
        let naive = NaiveDateTime::parse_from_str(value, ICALENDAR_DATE_TIME_FORMAT)
            .map_err(|_| rejected())?;
        let at = in_zone(naive, zone).ok_or_else(rejected)?;
        return Ok((readable(at, &rejected)?, false));
    }
    let date = NaiveDate::parse_from_str(value, ICALENDAR_DATE_FORMAT).map_err(|_| rejected())?;
    let naive = date.and_time(::chrono::NaiveTime::MIN);
    let at = in_zone(naive, zone).ok_or_else(rejected)?;
    Ok((readable(at, &rejected)?, true))
}

/// `at` itself when it is an instant this service reads, else the rejection `rejected`
/// words.
///
/// Bounding the instants a calendar states is what keeps every length and span computed
/// from them inside the range the date library represents ([`READABLE_YEARS`]).
fn readable(at: DateTime<Utc>, rejected: &impl Fn() -> String) -> Result<DateTime<Utc>, String> {
    if is_readable(at) {
        Ok(at)
    } else {
        Err(rejected())
    }
}

/// The instant a civil date-time names in `zone`, or at UTC when it carries no zone.
///
/// A civil time a zone's offset change leaves AMBIGUOUS resolves to the EARLIER of its two
/// instants, and one the zone skips entirely names no instant at all — the reading `chrono`
/// gives a local time, so nothing here invents an offset for either case.
fn in_zone(naive: NaiveDateTime, zone: Option<::chrono_tz::Tz>) -> Option<DateTime<Utc>> {
    match zone {
        None => Some(Utc.from_utc_datetime(&naive)),
        Some(zone) => Some(
            zone.from_local_datetime(&naive)
                .earliest()?
                .with_timezone(&Utc),
        ),
    }
}

/// The property of `properties` named `name`, compared the way iCalendar names are:
/// case-insensitively.
fn named_property<'a>(
    properties: &[&'a ::icalendar::parser::Property<'a>],
    name: &str,
) -> Option<&'a ::icalendar::parser::Property<'a>> {
    properties
        .iter()
        .copied()
        .find(|property| property.name.as_str().eq_ignore_ascii_case(name))
}

/// The value of the `name` parameter of `property`, or `None` when it carries none.
fn named_parameter<'a>(
    property: &'a ::icalendar::parser::Property<'a>,
    name: &str,
) -> Option<&'a str> {
    property
        .params
        .iter()
        .find(|parameter| parameter.key.as_str().eq_ignore_ascii_case(name))
        .and_then(|parameter| parameter.val.as_ref())
        .map(|value| value.as_str().trim())
}

/// One property as the content line that states it, which is what the recurrence reader
/// takes — or `None` when a part of it carries a line break, so that re-assembling it would
/// state content lines the document does not.
fn rendered_line(property: &::icalendar::parser::Property) -> Option<String> {
    fn single_line(text: &str) -> Option<&str> {
        (!text.contains(['\r', '\n'])).then_some(text)
    }

    let mut line = single_line(property.name.as_str())?.to_uppercase();
    for parameter in &property.params {
        line.push(';');
        line.push_str(single_line(parameter.key.as_str())?);
        if let Some(value) = parameter.val.as_ref() {
            line.push('=');
            line.push_str(single_line(value.as_str())?);
        }
    }
    line.push(':');
    line.push_str(single_line(property.val.as_str().trim())?);
    Some(line)
}
