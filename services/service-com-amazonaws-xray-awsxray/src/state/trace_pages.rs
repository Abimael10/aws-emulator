// SPDX-License-Identifier: CC0-1.0

//! How a trace query PAGES the window it is asked about.
//!
//! `GetTraceSummaries` reports "the start time of this page of results" and a
//! token for when "the requested time frame contained more than one page of
//! results … the first page contains the most recent results, closest to the end
//! of the time frame"
//! (`smithy_query(query="shape",shape="GetTraceSummariesResult")`), so the pages
//! of one query are SLICES OF ITS WINDOW walked from the end of the window
//! backwards, not one list of everything the window holds: each page is one
//! hour-long slice ([`TRACE_PAGE_SECONDS`]) of `[StartTime, EndTime)`, the first
//! page is the slice the window ends in, and a token is due while an earlier
//! slice of the window remains — whether or not the slice just served held
//! anything, because a page holding nothing says nothing about what the rest of
//! the window holds, and a client that stops at the first empty page has read
//! only the last hour of the range it asked about.
//!
//! The slices are cut on the hour GRID rather than from the requested bounds, so
//! the start of a page is the top of the hour that begins it: two queries over
//! overlapping windows place the same trace on the same boundary, and the
//! instant a page reports is a time a reader can place in the range it asked
//! about rather than an artefact of when it asked. Every page reports an instant
//! inside the requested window, because a slice is clipped to that window before
//! it is reported.
//!
//! One concern, one file: the slicing, the instant a page reports and the form
//! of the token that resumes it are stated once here, so the handler that serves
//! a page and the one that resumes one cannot disagree about which slice a token
//! names.

/// How long one page of a trace query's window is, in seconds.
///
/// The hour the service pages a multi-hour window by: the model declares no page
/// size for `GetTraceSummaries`, so the slice is the service's own, and it is
/// stated here as the one place that decides both the boundaries a window is cut
/// on and the instant each page reports.
pub const TRACE_PAGE_SECONDS: i64 = 60 * 60;

/// The separator between the two parts of a page token: the slice the next page
/// serves, and — when that slice still has summaries left in it — the trace the
/// next page resumes at. A trace id carries no colon
/// (`smithy_query(query="shape",shape="TraceId")`), so the split is unambiguous.
const TOKEN_SEPARATOR: char = ':';

/// The most pages one window is cut into: the widest window a trace query serves
/// ([`super::MAX_TRACE_SUMMARIES_WINDOW_HOURS`]) covers that many whole hours
/// plus the part-hour it starts in. A wider window is refused before any page is
/// cut, so the cap only bounds the walk below; it never decides an answer.
const MAX_WINDOW_PAGES: usize = super::MAX_TRACE_SUMMARIES_WINDOW_HOURS as usize + 1;

/// One page of a `[start, end)` window: the slice of that window it serves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPage {
    /// The hour boundary this page is cut on, which is what a token names it by.
    pub boundary: i64,
    /// Where the page's own slice starts: the boundary, or the start of the
    /// requested window when the window starts inside this slice. This is "the
    /// start time of this page of results".
    pub start: f64,
    /// Where the page's own slice ends, exclusive: the next boundary, or the end
    /// of the requested window when the window ends inside this slice.
    pub end: f64,
    /// The boundary of the next page — the slice before this one — while the
    /// requested window reaches into one, and `None` for the earliest slice of
    /// the window, after which the query has nothing left to serve.
    pub earlier: Option<i64>,
}

/// The hour boundary at or before `seconds`: the top of the hour `seconds` falls
/// in.
///
/// Every arithmetic step of this module is saturating or checked, because both
/// bounds of a window are the caller's: a window naming an instant at the far
/// edge of the range the modelled timestamp holds answers rather than ending the
/// request, which is the same belt [`super::timestamp_of`] puts on a stored
/// instant.
#[must_use]
pub fn hour_boundary_at_or_before(seconds: f64) -> i64 {
    #[allow(clippy::cast_precision_loss)]
    let hour = TRACE_PAGE_SECONDS as f64;
    // A float-to-integer cast saturates, and the quotient of a finite instant by
    // an hour is far inside `i64`; a non-finite one cannot reach here, because
    // every instant a window carries is converted from a modelled timestamp.
    #[allow(clippy::cast_possible_truncation)]
    let whole = (seconds / hour).floor() as i64;
    whole.saturating_mul(TRACE_PAGE_SECONDS)
}

/// The hour boundaries the window `[start, end)` is paged by, MOST RECENT FIRST
/// — the order the pages are served in.
///
/// Empty for a window that names no interval (an `EndTime` at or before the
/// `StartTime`): there is no slice to serve, which the caller answers as the one
/// empty page such a window has.
#[must_use]
pub fn window_page_boundaries(start: f64, end: f64) -> Vec<i64> {
    if end <= start {
        return Vec::new();
    }
    let first = hour_boundary_at_or_before(start);
    let mut last = hour_boundary_at_or_before(end);
    #[allow(clippy::cast_precision_loss)]
    if last as f64 >= end {
        // `end` is exclusive, so a window ending exactly on the hour ends where
        // that hour's slice begins: the slice itself is not part of the window.
        last = last.saturating_sub(TRACE_PAGE_SECONDS);
    }
    let mut boundaries = Vec::new();
    let mut at = last;
    while at >= first && boundaries.len() < MAX_WINDOW_PAGES {
        boundaries.push(at);
        // The step down the grid is CHECKED, because both bounds are
        // request-derived and the conversion above saturates: a window naming an
        // instant at the far negative end of the range would otherwise step past
        // what the type holds. A step that cannot be taken ends the walk, which
        // is the same answer as reaching the window's first slice.
        let Some(earlier) = at.checked_sub(TRACE_PAGE_SECONDS) else {
            break;
        };
        at = earlier;
    }
    boundaries
}

/// The page of the window `[start, end)` cut on `boundary`.
///
/// The slice is clipped to the requested window at both ends, so a page never
/// reports an instant outside the range the caller asked about and never selects
/// a trace the window excludes.
#[must_use]
pub fn window_page(start: f64, end: f64, boundary: i64) -> WindowPage {
    #[allow(clippy::cast_precision_loss)]
    let opens = boundary as f64;
    #[allow(clippy::cast_precision_loss)]
    let closes = boundary.saturating_add(TRACE_PAGE_SECONDS) as f64;
    WindowPage {
        boundary,
        start: start.max(opens),
        end: end.min(closes),
        earlier: (boundary > hour_boundary_at_or_before(start))
            .then(|| boundary.checked_sub(TRACE_PAGE_SECONDS))
            .flatten(),
    }
}

/// The token that resumes a query at `boundary`, at `trace` when that slice
/// still holds summaries past the page just served.
#[must_use]
pub fn page_token(boundary: i64, trace: Option<&str>) -> String {
    match trace {
        Some(trace) => format!("{boundary}{TOKEN_SEPARATOR}{trace}"),
        None => boundary.to_string(),
    }
}

/// The slice and the trace a token issued by [`page_token`] names, or `None` for
/// a value no page of any window ever handed out — which the caller refuses as
/// the uninterpretable token it is
/// ([`super::invalid_next_token_message`]).
///
/// A boundary off the hour grid is rejected here: no page is cut anywhere else,
/// so such a value is not a cursor this read issued, however numeric it looks.
/// So is one outside the instants this service can convert to the modelled
/// timestamp at all ([`super::MAX_SEGMENT_EPOCH_SECONDS`]), which no page of any
/// window could have reported.
#[must_use]
pub fn parse_page_token(token: &str) -> Option<(i64, Option<&str>)> {
    let (boundary, trace) = match token.split_once(TOKEN_SEPARATOR) {
        Some((_, "")) => return None,
        Some((boundary, trace)) => (boundary, Some(trace)),
        None => (token, None),
    };
    let boundary: i64 = boundary.parse().ok()?;
    #[allow(clippy::cast_possible_truncation)]
    let reach = super::MAX_SEGMENT_EPOCH_SECONDS as i64;
    let on_the_grid = boundary % TRACE_PAGE_SECONDS == 0;
    (on_the_grid && (-reach..=reach).contains(&boundary)).then_some((boundary, trace))
}
