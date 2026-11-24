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
//! clipped to `StartTime`), so the instant a page reports is a time the reader
//! can place in the range it asked about, and the page a single-step window is
//! answered in reports the requested `StartTime`.
//!
//! One concern, one file: the stepping, the instant a page reports and the form
//! of the token that resumes it are stated once here, so the handler that serves
//! a page and the one that resumes one cannot disagree about which step a token
//! names.
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

use ::service_common::page_tokens::PageTokenKey;

/// How long one page's step of a trace query's window is, in seconds.
///
/// The model declares no page size for `GetTraceSummaries`, so the step is the
/// service's own, and it is stated here as the one place that decides both how a
/// window is cut into pages and the instant each page reports.
pub const TRACE_PAGE_SECONDS: i64 = 60 * 60;

/// The separator between the two parts of the CURSOR a page token carries: the
/// step the next page serves, and — when that step still has summaries left in
/// it — the trace the next page resumes at. A trace id carries no colon
/// (`smithy_query(query="shape",shape="TraceId")`), so the split is unambiguous.
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

/// One page of a `[start, end)` window: the step of that window it serves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPage {
    /// Which step of the window this page serves, counted back from the end of
    /// the window: `0` is the step the window ends in, which is the first page.
    /// This is what a token names a page by.
    pub index: usize,
    /// Where the page's own step starts: one step before it ends, or the start
    /// of the requested window for the page that reaches it. This is "the start
    /// time of this page of results".
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
/// reports an instant outside the range the caller asked about and never selects
/// a trace the window excludes.
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

/// The token that resumes a query at the page `index`, at `trace` when that
/// page's step still holds summaries past the page just served.
///
/// The step and the trace are the CURSOR; the token is that cursor authenticated
/// under the issuing store's secret `key` and rendered base64, which is the one
/// opaque spelling this read hands a cursor out in: a caller sends it back
/// unread, and nothing about which step or trace it names can be read off it.
#[must_use]
pub fn page_token(key: &PageTokenKey, index: usize, trace: Option<&str>) -> String {
    let cursor = match trace {
        Some(trace) => format!("{index}{TOKEN_SEPARATOR}{trace}"),
        None => index.to_string(),
    };
    ::service_common::page_tokens::page_token(key, &cursor)
}

/// The page and the trace a token issued by [`page_token`] names, or `None` for
/// a value no page of any window ever handed out — which the caller refuses as
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
pub fn parse_page_token(key: &PageTokenKey, token: &str) -> Option<(usize, Option<String>)> {
    let cursor = ::service_common::page_tokens::page_cursor(key, token)?;
    let (index, trace) = match cursor.split_once(TOKEN_SEPARATOR) {
        Some((_, "")) => return None,
        Some((index, trace)) => (index.to_string(), Some(trace.to_string())),
        None => (cursor, None),
    };
    let index: usize = index.parse().ok()?;
    (index < MAX_WINDOW_PAGES).then_some((index, trace))
}
