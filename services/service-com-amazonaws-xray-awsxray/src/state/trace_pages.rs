// SPDX-License-Identifier: CC0-1.0

//! How a trace query PAGES the window it is asked about.
//!
//! `GetTraceSummaries` reports "the start time of this page of results" and a
//! token for when "the requested time frame contained more than one page of
//! results … the first page contains the most recent results, closest to the end
//! of the time frame"
//! (`smithy_query(query="shape",shape="GetTraceSummariesResult")`), so the pages
//! of one query are STEPS OF ITS OWN WINDOW walked from the end of that window
//! backwards: the first page serves the last [`TRACE_PAGE_SECONDS`] of
//! `[StartTime, EndTime)`, each further page the step before it, and the step
//! that reaches `StartTime` is the last page the query has.
//!
//! The steps are measured FROM THE REQUESTED BOUNDS, never from an absolute
//! epoch grid nobody named: a window no longer than one step is therefore ONE
//! page carrying every trace that window holds, whatever the clock happens to
//! read, so a trace accepted a moment ago is in the answer to the minutes-wide
//! window that covers it instead of being deferred to a page the caller had no
//! reason to ask for. What a query answers is thereby a function of the window
//! asked about — the same window answers the same way whenever it is asked —
//! rather than of where that window falls between two boundaries.
//!
//! Every page's own step lies inside the requested window by construction (it is
//! clipped to `StartTime`), so no page ever selects a trace the window excludes.
//!
//! WHICH STEP a page serves is one thing; the instant the answer is LABELLED
//! with is another. "The start time of this page of results"
//! (`smithy_query(query="shape",shape="GetTraceSummariesResult")`) is reported
//! on the absolute, hour-aligned grid ([`TRACE_REPORT_BUCKET_SECONDS`]): the
//! start of the hour bucket the step being served CLOSES in, clipped forward to
//! the start of that step ([`span_approximate_time`],
//! [`WindowPage::approximate_time`]). So a step reaching back across an hour
//! boundary is labelled by that boundary — an hour-aligned instant whatever
//! bounds the request named — a step lying wholly inside one hour is labelled by
//! its own start, every page cut out of ONE step reports the same label, and
//! walking a multi-step window reports one label per step, an hour earlier each
//! time, as the walk moves back through the window. The label therefore always
//! lies inside the stretch of time the page reports on.
//!
//! WITHIN one step the summaries are served NEWEST FIRST — "the first page
//! contains the most recent results, closest to the end of the time frame" — so
//! the order a step's summaries are cut into pages in is the order of the time
//! each trace falls in the window at, descending from the end of the step, with
//! the traces sharing one instant ordered by their ids so that order is total
//! ([`step_order`]). A step holding more summaries than one page carries
//! therefore puts its most recent ones on the first page, never the ones whose
//! ids happen to spell lowest, and what a page holds does not change as
//! unrelated traces are written.
//!
//! One concern, one file: the stepping, the order a step is served in, the
//! instant a page reports and the form of the token that resumes it are stated
//! once here, so the handler that serves a page and the one that resumes one
//! cannot disagree about which step a token names or where inside it the next
//! page starts.
//!
//! The token itself is OPAQUE. `NextToken` is "the pagination token returned by a
//! previous request"
//! (`smithy_query(query="doc",shape="GetTraceSummariesRequest",member="NextToken")`)
//! and nothing of its format is published, so a caller hands it back exactly as
//! it arrived and reads nothing out of it: the step the next page serves and the
//! trace it resumes at travel INSIDE the token, behind the authenticator the
//! issuing store's secret computes over them
//! ([`::service_common::page_tokens`]), rendered as base64. No instant, ordinal
//! or trace id is therefore legible on the wire, and a value this store never
//! minted — a cursor a caller assembled, one minted by another store — names no
//! page at all, which is what the caller refuses
//! ([`super::invalid_next_token_message`]).

use ::std::cmp::Ordering;

use ::service_common::page_tokens::PageTokenKey;

/// How long one page's step of a trace query's window is, in seconds.
///
/// The model declares no page size for `GetTraceSummaries`, so the step is the
/// service's own, and it is stated here as the one place that decides both how a
/// window is cut into pages and the instant each page reports.
pub const TRACE_PAGE_SECONDS: i64 = 60 * 60;

/// The separator between the parts of the CURSOR a page token carries: the step
/// the next page serves, and — when that step still has summaries left in it —
/// where inside the step's own order that page resumes ([`StepCursor`]). Neither
/// a trace id (`smithy_query(query="shape",shape="TraceId")`) nor a rendered
/// instant carries a colon, so the split is unambiguous.
///
/// It is a detail of what travels INSIDE the token, never of the token's own
/// text: the cursor is authenticated and encoded before it reaches the wire
/// ([`page_token`]).
const TOKEN_SEPARATOR: char = ':';

/// The most pages one window is cut into: the widest window a trace query serves
/// ([`super::MAX_TRACE_SUMMARIES_WINDOW_HOURS`]) is that many steps long. A
/// wider window is refused before any page is cut, so the cap only bounds the
/// walk below; it never decides an answer.
const MAX_WINDOW_PAGES: usize = super::MAX_TRACE_SUMMARIES_WINDOW_HOURS as usize;

/// How long the ABSOLUTE, hour-aligned bucket an answer is LABELLED by is, in
/// seconds: the hour.
///
/// The model puts no constraint on the granularity of the instant a query
/// reports (`GetTraceSummariesResult$ApproximateTime` targets `Timestamp` with
/// no range or format), so the bucket is stated here as the service's own, and
/// every label is read off it ([`span_approximate_time`]).
pub const TRACE_REPORT_BUCKET_SECONDS: i64 = 60 * 60;

/// The start of the absolute, hour-aligned bucket `instant` falls in:
/// `floor(instant / TRACE_REPORT_BUCKET_SECONDS)` buckets from the epoch.
///
/// A bucket of the ABSOLUTE grid, so the same stretch of time is named by the
/// same instant in every answer that reports it, and a reader bucketing or
/// de-duplicating answers by that instant gets one bucket per hour of traffic
/// rather than one value per call. It is never the clock: when a response was
/// built is not a time any result was recorded at.
///
/// An instant that is no instant at all — one that compares with nothing, or one
/// reaching past every instant there is — falls in no bucket, so it is answered
/// as itself and bounded where it is converted ([`super::timestamp_of`]).
#[must_use]
pub fn hour_bucket_start(instant: f64) -> f64 {
    if !instant.is_finite() {
        return instant;
    }
    #[allow(clippy::cast_precision_loss)]
    let bucket = TRACE_REPORT_BUCKET_SECONDS as f64;
    (instant / bucket).floor() * bucket
}

/// The start of the absolute hour bucket the LAST INSTANT of a `[_, end)` span
/// falls in.
///
/// The bucket `end` itself falls in, except where `end` sits exactly on a
/// boundary: an exclusive end on a boundary is the first instant of the NEXT
/// bucket, which the span does not reach, so the span closes in the bucket
/// before it.
fn closing_hour_bucket(end: f64) -> f64 {
    let bucket = hour_bucket_start(end);
    #[allow(clippy::cast_precision_loss)]
    let seconds = TRACE_REPORT_BUCKET_SECONDS as f64;
    if bucket >= end {
        return bucket - seconds;
    }
    bucket
}

/// What an answer over the span `[start, end)` is LABELLED with — "the start
/// time of this page of results": the start of the absolute hour bucket that
/// span closes in, clipped forward to the span's own start.
///
/// So a span reaching back across an hour boundary is labelled by that boundary,
/// an hour-aligned instant whatever bounds the request named, while one lying
/// wholly inside a single hour is labelled by its own start, the first instant
/// it reports on. Either way the label lies inside the span, so a reader can
/// place it in the stretch of time the answer describes, and two answers over
/// spans closing in the same hour are labelled alike.
#[must_use]
pub fn span_approximate_time(start: f64, end: f64) -> f64 {
    closing_hour_bucket(end).max(start)
}

/// One page of a `[start, end)` window: the step of that window it serves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPage {
    /// Which step of the window this page serves, counted back from the end of
    /// the window: `0` is the step the window ends in, which is the first page.
    /// This is what a token names a page by.
    pub index: usize,
    /// Where the page's own step starts: one step before it ends, or the start
    /// of the requested window for the page that reaches it. This SELECTS the
    /// traces the page carries; it is not what the answer is labelled with,
    /// which is read off the absolute hour grid
    /// ([`WindowPage::approximate_time`]).
    pub start: f64,
    /// Where the page's own step ends, exclusive: the end of the requested
    /// window for the first page, and the start of the page before it after
    /// that.
    pub end: f64,
    /// The index of the next page — the step before this one — while the
    /// requested window reaches into one, and `None` for the step that reaches
    /// the start of the window, after which the query has nothing left to serve.
    pub earlier: Option<usize>,
}

impl WindowPage {
    /// The instant an answer serving this page REPORTS — "the start time of this
    /// page of results": the label of this page's own STEP
    /// ([`span_approximate_time`]).
    ///
    /// Read off the step rather than off the summaries it was cut into pages of,
    /// so every page cut out of one step — a step holding more summaries than
    /// one page carries — reports that step's one label, and a walk back through
    /// a multi-step window reports one label per step, an hour earlier each
    /// time.
    #[must_use]
    pub fn approximate_time(&self) -> f64 {
        span_approximate_time(self.start, self.end)
    }
}

/// The instant `steps` steps back from `end`.
///
/// Both the start and the end of every page are read from this one expression,
/// so the instant one page ends at is bit-for-bit the instant the page before it
/// starts at: a trace sitting exactly on a step boundary is served by exactly one
/// page of the query.
///
/// Every arithmetic step of this module is taken on instants converted from a
/// modelled timestamp, which this service bounds
/// ([`super::MAX_SEGMENT_EPOCH_SECONDS`]), so a window naming an instant at the
/// far edge of that range answers rather than ending the request.
fn step_back(end: f64, steps: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let seconds = TRACE_PAGE_SECONDS as f64;
    #[allow(clippy::cast_precision_loss)]
    let steps = steps as f64;
    end - seconds * steps
}

/// How many pages the window `[start, end)` is served in: one per step of the
/// window, with the last step clipped to the start of the window.
///
/// Zero for a window that names no interval (an `EndTime` at or before the
/// `StartTime`): there is no step to serve, which the caller answers as the one
/// empty page such a window has.
#[must_use]
pub fn window_page_count(start: f64, end: f64) -> usize {
    // A window that names no interval — and one naming an instant that is no
    // instant at all, which compares with nothing — has no step to serve.
    if end <= start || end.is_nan() || start.is_nan() {
        return 0;
    }
    // The smallest number of steps that reaches back to the start of the
    // window. Walked rather than divided, so the count cannot disagree with the
    // boundaries [`step_back`] produces and a sliver of the window be left on
    // no page at all.
    (1..MAX_WINDOW_PAGES)
        .find(|steps| step_back(end, *steps) <= start)
        .unwrap_or(MAX_WINDOW_PAGES)
}

/// The `index`th page of the window `[start, end)`, or `None` when the window is
/// served in fewer pages than that — which is what a token naming a page of some
/// other window looks like here.
///
/// The step is clipped to the requested window at its start, so a page never
/// selects a trace the window excludes.
#[must_use]
pub fn window_page(start: f64, end: f64, index: usize) -> Option<WindowPage> {
    let pages = window_page_count(start, end);
    if index >= pages {
        return None;
    }
    Some(WindowPage {
        index,
        start: step_back(end, index + 1).max(start),
        end: step_back(end, index),
        earlier: (index + 1 < pages).then_some(index + 1),
    })
}

/// WHERE inside one step's own order a page resumes: the position of the first
/// summary that page serves.
///
/// A step is served newest first ([`step_order`]), so a position in it is the
/// instant the trace falls in the window at together with that trace's id, which
/// orders the traces sharing one instant. A position rather than a trace alone,
/// so a page resumes at the right place in that time order even when the trace
/// the previous page stopped at has since been evicted.
#[derive(Debug, Clone, PartialEq)]
pub struct StepCursor {
    /// The instant the trace this page starts at falls in the window at — the
    /// time the query's `TimeRangeType` selects.
    pub instant: f64,
    /// That trace's id, which decides the order among the traces sharing one
    /// instant.
    pub trace: String,
}

impl StepCursor {
    /// The position of the trace `trace` falling in the window at `instant`.
    #[must_use]
    pub fn new(instant: f64, trace: &str) -> Self {
        Self {
            instant,
            trace: trace.to_string(),
        }
    }
}

/// The ORDER the summaries of one step are served in: by the instant each trace
/// falls in the window at, DESCENDING — the most recent first, closest to the
/// end of the step — with the traces sharing one instant ordered by their ids,
/// so the order is total and a page of a step is the same page whenever that
/// step is asked for.
///
/// Compared with [`f64::total_cmp`], so two instants always have an order and no
/// trace falls out of the ordering it is cut into pages by.
#[must_use]
pub fn step_order(left: &StepCursor, right: &StepCursor) -> Ordering {
    right
        .instant
        .total_cmp(&left.instant)
        .then_with(|| left.trace.cmp(&right.trace))
}

/// Whether the summary at `position` is one a page resuming at `cursor` serves:
/// the positions from that cursor onwards in the step's own order.
#[must_use]
pub fn at_or_after(position: &StepCursor, cursor: &StepCursor) -> bool {
    step_order(position, cursor) != Ordering::Less
}

/// The token that resumes a query at the page `index`, at `resume` when that
/// page's step still holds summaries past the page just served.
///
/// The step and that position are the CURSOR; the token is that cursor
/// authenticated under the issuing store's secret `key` and rendered base64,
/// which is the one opaque spelling this read hands a cursor out in: a caller
/// sends it back unread, and nothing about which step or position it names can be
/// read off it.
#[must_use]
pub fn page_token(key: &PageTokenKey, index: usize, resume: Option<&StepCursor>) -> String {
    let cursor = match resume {
        Some(at) => {
            // Rendered so the instant read back is the instant written: the
            // position a page resumes at has to compare exactly as it did when
            // the page before it stopped there.
            let instant = at.instant;
            format!(
                "{index}{TOKEN_SEPARATOR}{instant:?}{TOKEN_SEPARATOR}{trace}",
                trace = at.trace
            )
        }
        None => index.to_string(),
    };
    ::service_common::page_tokens::page_token(key, &cursor)
}

/// The page and the position inside it that a token issued by [`page_token`]
/// names, or `None` for a value no page of any window ever handed out — which the caller refuses as
/// the uninterpretable token it is
/// ([`super::invalid_next_token_message`]).
///
/// A value `key` did not mint carries no cursor at all: text that is not base64,
/// a cursor behind an authenticator a caller assembled, and a token minted by
/// another store all resolve to nothing, so a page is served only for a cursor
/// this store itself handed out.
///
/// Whether the page it names is one the REQUESTED window has is the caller's
/// check ([`window_page`]): a token is a cursor into the query that issued it, so
/// one naming a page beyond the window now being asked about did not come from
/// it, however well formed it looks.
#[must_use]
pub fn parse_page_token(key: &PageTokenKey, token: &str) -> Option<(usize, Option<StepCursor>)> {
    let cursor = ::service_common::page_tokens::page_cursor(key, token)?;
    let mut parts = cursor.splitn(3, TOKEN_SEPARATOR);
    let index: usize = parts.next()?.parse().ok()?;
    let resume = match (parts.next(), parts.next()) {
        (None, _) => None,
        (Some(instant), Some(trace)) if !trace.is_empty() => {
            // An instant at all: a value that is no instant — one that compares
            // with nothing, or one reaching past every instant there is — names
            // no position in any step's order, the same judgement the window
            // itself is read with above.
            let instant: f64 = instant.parse().ok()?;
            if !instant.is_finite() {
                return None;
            }
            Some(StepCursor::new(instant, trace))
        }
        // A cursor carrying a position with no trace, or a trace with no
        // instant, names no position in any step's order.
        _ => return None,
    };
    (index < MAX_WINDOW_PAGES).then_some((index, resume))
}
