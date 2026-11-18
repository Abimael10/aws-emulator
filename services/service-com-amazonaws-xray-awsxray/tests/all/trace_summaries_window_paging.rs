// SPDX-License-Identifier: CC0-1.0

//! How `GetTraceSummaries` PAGES the window it is asked about, end to end over
//! the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! One rule, asserted over tables that span what it generalises over rather than
//! over one request: the pages of one query are hour-long slices of
//! `[StartTime, EndTime)` walked from the end of the window backwards, so
//!
//! * `ApproximateTime` ("the start time of this page of results") is the start of
//!   the slice the page served — the top of the hour that begins it when that
//!   boundary is inside the requested window, and the window's own start for the
//!   slice the window starts inside — reported as whole seconds and always inside
//!   the window asked about;
//! * a `NextToken` is due while the query has anything left to serve: the rest of
//!   a slice holding more summaries than one page carries, or the slice before
//!   this one while the window reaches into it — whether or not the slice just
//!   served held anything;
//! * resuming from that token serves the next slice, so following the tokens
//!   walks the whole window exactly once, newest slice first — which is how a
//!   reader learns what a WINDOW holds, because the slices are cut on the hour
//!   grid rather than on the window's own bounds, so a window of a few minutes
//!   that straddles a boundary answers an empty first page and reports its
//!   traces on the next one;
//! * a token naming a slice the requested window is not paged by, or no slice at
//!   all, is refused with the service's one wording for an uninterpretable
//!   pagination token.
//!
//! The rows differ along what the rule generalises over: the WIDTH of the window
//! (nothing, part of an hour, an hour, several hours, the widest window the read
//! serves), where its bounds fall relative to the hour GRID (on the hour, inside
//! one hour, spanning a boundary), how many traces a slice holds (none, some, more
//! than one page), and which slice of the window a trace falls in.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::ops::get_trace_summaries::TRACE_SUMMARIES_PAGE_SIZE;
use service_com_amazonaws_xray_awsxray::state::TRACE_PAGE_SECONDS;

use super::xray_wire::{reported, reported_trace_ids, root_document, trace_id, Answer, Xray};

/// One hour in the epoch seconds every instant on this wire travels as — the
/// length of one page of a query's window, read from the service's own statement
/// of it so the test cannot drift from the implementation's grid.
#[allow(clippy::cast_precision_loss)]
const HOUR: f64 = TRACE_PAGE_SECONDS as f64;

/// An instant well in the past that falls exactly on the hour grid
/// (1_700_002_800 = 472_223 * 3600), so a case can place a bound on a boundary or
/// deliberately off it. Far enough behind the clock that no window below reaches
/// the skew allowance the window check applies.
const ALIGNED: f64 = 1_700_002_800.0;

/// The refusal message of an answer, or the empty string.
fn said(answer: &Answer) -> &str {
    answer.message().unwrap_or_default()
}

/// The `ApproximateTime` of an answer.
fn approximate_time(answer: &Answer) -> Option<f64> {
    answer.get("ApproximateTime").as_f64()
}

/// The `NextToken` of an answer.
fn next_token(answer: &Answer) -> Option<String> {
    answer.get("NextToken").as_str().map(str::to_string)
}

/// Store `count` traces, the `index`th of them starting `offsets[index]` seconds
/// from [`ALIGNED`], and answer their ids in the order the ids sort.
async fn seed(xray: &Xray, offsets: &[f64]) -> Vec<String> {
    let mut stored = Vec::new();
    for (index, offset) in offsets.iter().enumerate() {
        let id = trace_id(u32::try_from(index + 1).expect("the index fits"));
        let start = ALIGNED + offset;
        let document = root_document(
            &format!("{:016x}", index + 1),
            &id,
            "svc",
            start,
            Some(start + 0.5),
        )
        .to_string();
        let put = xray.put_segments(&[document]).await;
        assert_eq!(
            put.status, 200,
            "seeding {id} at {start} must succeed, got {put:?}"
        );
        stored.push(id);
    }
    stored.sort();
    stored
}

// =============================================================================
// 1. The first page reports the start of the slice the window ends in, and a
//    token while an earlier slice of the window remains
// =============================================================================

/// One window, and what the rule says the first page of a query over it reports:
/// the start of the slice the window ends in, and whether a token is due.
struct WindowCase {
    label: &'static str,
    start: f64,
    end: f64,
    /// `ApproximateTime` the rule requires: the start of the page's own slice.
    page_start: f64,
    /// Whether the window reaches into a slice before the one it ends in.
    earlier_slice: bool,
}

fn window_cases() -> Vec<WindowCase> {
    vec![
        WindowCase {
            label: "a window of a single instant",
            start: ALIGNED,
            end: ALIGNED,
            page_start: ALIGNED,
            earlier_slice: false,
        },
        WindowCase {
            label: "part of an hour, starting on the hour",
            start: ALIGNED,
            end: ALIGNED + 42.0 * 60.0,
            page_start: ALIGNED,
            earlier_slice: false,
        },
        WindowCase {
            label: "part of an hour, starting inside it",
            start: ALIGNED + 600.0,
            end: ALIGNED + 1_200.0,
            page_start: ALIGNED + 600.0,
            earlier_slice: false,
        },
        WindowCase {
            label: "a window spanning one hour boundary",
            start: ALIGNED + HOUR - 600.0,
            end: ALIGNED + HOUR + 600.0,
            page_start: ALIGNED + HOUR,
            earlier_slice: true,
        },
        WindowCase {
            label: "exactly one hour, on the grid",
            start: ALIGNED,
            end: ALIGNED + HOUR,
            page_start: ALIGNED,
            earlier_slice: false,
        },
        WindowCase {
            label: "exactly one hour, off the grid",
            start: ALIGNED + 600.0,
            end: ALIGNED + HOUR + 600.0,
            page_start: ALIGNED + HOUR,
            earlier_slice: true,
        },
        WindowCase {
            label: "a window ending a second into the next slice",
            start: ALIGNED,
            end: ALIGNED + HOUR + 1.0,
            page_start: ALIGNED + HOUR,
            earlier_slice: true,
        },
        WindowCase {
            label: "several hours, on the grid",
            start: ALIGNED,
            end: ALIGNED + 6.0 * HOUR,
            page_start: ALIGNED + 5.0 * HOUR,
            earlier_slice: true,
        },
        WindowCase {
            label: "the widest window this read serves",
            start: ALIGNED,
            end: ALIGNED + 24.0 * HOUR,
            page_start: ALIGNED + 23.0 * HOUR,
            earlier_slice: true,
        },
    ]
}

#[tokio::test]
async fn the_first_page_reports_the_start_of_the_slice_the_window_ends_in() {
    for case in window_cases() {
        // A store holding nothing in any of these windows: what the envelope
        // reports is a property of the WINDOW, so an empty page states it as
        // plainly as a full one.
        let xray = Xray::new();
        let label = case.label;
        let answered = xray.summaries(case.start, case.end, None).await;
        assert_eq!(
            answered.status, 200,
            "case {label:?}: the window must be answered, got {answered:?}"
        );
        let at = approximate_time(&answered);
        assert!(
            at.is_some_and(|at| (at - case.page_start).abs() < 1e-6),
            "case {label:?}: expected ApproximateTime {} (the page's own slice start), got {at:?}",
            case.page_start
        );
        assert!(
            at.is_some_and(|at| at.fract() == 0.0),
            "case {label:?}: ApproximateTime is whole seconds, got {at:?}"
        );
        assert!(
            at.is_some_and(|at| at >= case.start && (at <= case.end)),
            "case {label:?}: ApproximateTime must lie inside [{}, {}], got {at:?}",
            case.start,
            case.end
        );
        assert_eq!(
            next_token(&answered).is_some(),
            case.earlier_slice,
            "case {label:?}: a token is due only while an earlier slice of the window remains, \
             got {answered:?}"
        );
    }
}

// =============================================================================
// 2. Following the tokens walks the window newest slice first, exactly once
// =============================================================================

#[tokio::test]
async fn the_pages_walk_the_window_one_slice_at_a_time_newest_first() {
    let xray = Xray::new();
    // One trace in each of the three slices of the window, plus one the window
    // excludes: the slices are cut on the hour grid, so which page a trace
    // appears on follows from its own start time.
    let stored = seed(
        &xray,
        &[
            10.0,
            HOUR + 10.0,
            2.0 * HOUR + 10.0,
            3.0 * HOUR + 10.0,
        ],
    )
    .await;
    let (start, end) = (ALIGNED, ALIGNED + 3.0 * HOUR);

    // The pages, newest slice first: each slice's own start, and the trace that
    // slice holds.
    let expected: Vec<(f64, Vec<String>)> = vec![
        (ALIGNED + 2.0 * HOUR, vec![stored[2].clone()]),
        (ALIGNED + HOUR, vec![stored[1].clone()]),
        (ALIGNED, vec![stored[0].clone()]),
    ];

    let mut token: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    for (page, (slice_start, holds)) in expected.iter().enumerate() {
        let answered = xray.summaries(start, end, token.as_deref()).await;
        assert_eq!(
            answered.status, 200,
            "page {page}: the slice must be served, got {answered:?}"
        );
        assert!(
            approximate_time(&answered).is_some_and(|at| (at - slice_start).abs() < 1e-6),
            "page {page}: expected ApproximateTime {slice_start} (that slice's start), got \
             {answered:?}"
        );
        assert_eq!(
            &reported(&answered),
            holds,
            "page {page}: the slice [{slice_start}, {}) holds {holds:?}, got {answered:?}",
            slice_start + HOUR
        );
        // The count is of the traces the WINDOW holds, so it is the same on
        // every page of the one query — never of the slice just served.
        assert_eq!(
            answered.get("TracesProcessedCount").as_i64(),
            Some(3),
            "page {page}: the window holds three traces, got {answered:?}"
        );
        seen.extend(reported(&answered));
        token = next_token(&answered);
        assert_eq!(
            token.is_some(),
            page + 1 < expected.len(),
            "page {page}: a token is due only while an earlier slice remains, got {token:?}"
        );
    }
    assert_eq!(
        seen,
        vec![stored[2].clone(), stored[1].clone(), stored[0].clone()],
        "the walk must yield every trace of the window exactly once, newest slice first, got \
         {seen:?}"
    );
}

// =============================================================================
// 3. A slice holding more than one page is served over several pages, each
//    reporting that same slice's start
// =============================================================================

#[tokio::test]
async fn a_slice_holding_more_than_one_page_is_served_over_several_pages() {
    let xray = Xray::new();
    // Two traces more than a page carries, all inside the one slice the window
    // covers, so the slice itself is what has to be paged.
    let total = TRACE_SUMMARIES_PAGE_SIZE + 2;
    let offsets: Vec<f64> = (0..total)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let index = index as f64;
            10.0 + index
        })
        .collect();
    let stored = seed(&xray, &offsets).await;
    let (start, end) = (ALIGNED, ALIGNED + 1_800.0);

    let mut token: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    let mut pages = 0;
    loop {
        let answered = xray.summaries(start, end, token.as_deref()).await;
        assert_eq!(
            answered.status, 200,
            "page {pages}: the page must be served, got {answered:?}"
        );
        let ids = reported(&answered);
        assert!(
            ids.len() <= TRACE_SUMMARIES_PAGE_SIZE,
            "page {pages}: a page carries at most {TRACE_SUMMARIES_PAGE_SIZE} summaries, got \
             {ids:?}"
        );
        // Every page of one slice reports that slice's own start: the instant
        // reported says WHICH slice was served, not how far into it the page
        // reached.
        assert!(
            approximate_time(&answered).is_some_and(|at| (at - ALIGNED).abs() < 1e-6),
            "page {pages}: expected ApproximateTime {ALIGNED} (the slice's start), got \
             {answered:?}"
        );
        seen.extend(ids);
        token = next_token(&answered);
        assert_eq!(
            token.is_some(),
            seen.len() < stored.len(),
            "page {pages}: a token is due while the slice has summaries left, got {token:?} \
             after {} of {}",
            seen.len(),
            stored.len()
        );
        pages += 1;
        assert!(
            pages <= total,
            "paging one slice must terminate, stopped after {pages} pages"
        );
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        seen, stored,
        "paging one slice must yield every trace of it exactly once, got {seen:?}"
    );
}

// =============================================================================
// 4. A token that names no slice of the requested window is not a cursor of it
// =============================================================================

#[tokio::test]
async fn a_token_outside_the_requested_window_is_refused_with_the_one_wording() {
    let xray = Xray::new();
    let stored = seed(&xray, &[10.0, HOUR + 10.0]).await;
    let (start, end) = (ALIGNED, ALIGNED + 2.0 * HOUR);

    // Rows differ along HOW the token misses a slice of this window: a slice
    // before the window, a slice after it, an instant off the hour grid, a
    // boundary with nothing after the separator, and a slice of the window
    // naming a trace past every stored one.
    let rows: Vec<(&str, String)> = vec![
        ("a slice before the requested window", format!("{:.0}", ALIGNED - HOUR)),
        (
            "a slice after the requested window",
            format!("{:.0}", ALIGNED + 2.0 * HOUR),
        ),
        ("an instant off the hour grid", format!("{:.0}", ALIGNED + 60.0)),
        ("a boundary with no trace after it", format!("{ALIGNED:.0}:")),
        (
            "a trace past every stored one",
            format!("{ALIGNED:.0}:{}", trace_id(99)),
        ),
    ];

    for (label, token) in rows {
        let answered = xray.summaries(start, end, Some(&token)).await;
        assert_eq!(
            (
                answered.status,
                answered.error_type.as_deref(),
                said(&answered)
            ),
            (
                400,
                Some("InvalidRequestException"),
                format!("Invalid token {token}").as_str()
            ),
            "case {label:?}: a token this query never issued must be refused with the one \
             wording, got {answered:?}"
        );
    }

    // And the token this query DID issue resumes it: the slice before the one
    // the window ends in, holding the trace that slice holds.
    let first = xray.summaries(start, end, None).await;
    let issued = next_token(&first)
        .unwrap_or_else(|| panic!("a two-slice window must issue a token, got {first:?}"));
    let resumed = xray.summaries(start, end, Some(&issued)).await;
    assert_eq!(
        (resumed.status, reported(&resumed)),
        (200, vec![stored[0].clone()]),
        "the issued token must serve the earlier slice, got {resumed:?}"
    );
}

// =============================================================================
// 5. A window at the edge of the range a timestamp holds is answered, not fatal
// =============================================================================

#[tokio::test]
async fn a_window_at_the_edge_of_the_timestamp_range_is_answered() {
    let xray = Xray::new();

    // Both bounds of a window are the caller's, and the published checks bound
    // only the span, the order and how far AHEAD of the clock the end may land —
    // so a pair of instants at either extreme of what the modelled timestamp
    // holds reaches the paging arithmetic. Each row must come back as an answer
    // or as the modelled refusal for an invalid request, never as a server
    // failure. The rows differ along which extreme the window sits at and how
    // wide it is there.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "the far negative end of the range",
            json!(-1e300_f64),
            json!(-9_223_372_036_854_773_760_i64),
        ),
        (
            "a narrow window at the far negative end",
            json!(-9_223_372_036_854_775_000_i64),
            json!(-9_223_372_036_854_773_760_i64),
        ),
        (
            "a zero-width window at the far negative end",
            json!(-1e300_f64),
            json!(-1e300_f64),
        ),
        (
            "a window reaching back from the hour grid's origin",
            json!(-3_600.0_f64),
            json!(0.0_f64),
        ),
    ];

    for (label, start, end) in rows {
        let answered = xray
            .call("/TraceSummaries", &json!({"StartTime": start, "EndTime": end}))
            .await;
        assert!(
            answered.status == 200
                || (answered.status == 400
                    && answered.error_type.as_deref() == Some("InvalidRequestException")),
            "case {label:?}: expected 200 or the modelled InvalidRequestException, got \
             {answered:?}"
        );
        if answered.status == 200 {
            assert!(
                reported(&answered).is_empty(),
                "case {label:?}: no stored trace lies in that window, got {answered:?}"
            );
        }
    }
}


// =============================================================================
// 6. A window reports every trace it holds, wherever its bounds fall on the
//    grid — which is what following the tokens is for
// =============================================================================

/// One window over the store [`grid_cases`] seeds, and what the rule says a
/// reader of it sees: the trace ids the WHOLE query reports (newest slice
/// first), and the ids its FIRST page carries — which differ exactly when the
/// window straddles a boundary and holds something before it.
struct GridCase {
    label: &'static str,
    start: f64,
    end: f64,
    whole: Vec<String>,
    first_page: Vec<String>,
}

/// The windows the rule is asserted over, against a store holding one trace in
/// each of three adjacent slices: `stored` is the ids of the traces seeded at
/// [`GRID_OFFSETS`], in that order.
fn grid_cases(stored: &[String]) -> Vec<GridCase> {
    let (before, inside, later) = (stored[0].clone(), stored[1].clone(), stored[2].clone());
    vec![
        GridCase {
            label: "a window wholly inside one slice",
            start: ALIGNED,
            end: ALIGNED + 60.0,
            whole: vec![inside.clone()],
            first_page: vec![inside.clone()],
        },
        GridCase {
            label: "a window straddling a boundary, its trace before it",
            start: ALIGNED - 60.0,
            end: ALIGNED + 1.0,
            whole: vec![before.clone()],
            first_page: Vec::new(),
        },
        GridCase {
            label: "a window straddling a boundary, with a trace each side",
            start: ALIGNED - 60.0,
            end: ALIGNED + 60.0,
            whole: vec![inside.clone(), before.clone()],
            first_page: vec![inside.clone()],
        },
        GridCase {
            label: "a window ending exactly on a boundary",
            start: ALIGNED - 60.0,
            end: ALIGNED,
            whole: vec![before.clone()],
            first_page: vec![before.clone()],
        },
        GridCase {
            label: "a window spanning three slices",
            start: ALIGNED - HOUR,
            end: ALIGNED + 2.0 * HOUR,
            whole: vec![later.clone(), inside, before],
            first_page: vec![later],
        },
        GridCase {
            label: "a window holding nothing",
            start: ALIGNED + 2.0 * HOUR,
            end: ALIGNED + 2.0 * HOUR + 60.0,
            whole: Vec::new(),
            first_page: Vec::new(),
        },
    ]
}

/// Where the three traces the grid cases are read over start, relative to
/// [`ALIGNED`]: one in the slice before that boundary, one just after it, and
/// one a slice later.
const GRID_OFFSETS: [f64; 3] = [-30.0, 30.0, HOUR + 30.0];

#[tokio::test]
async fn a_window_reports_every_trace_it_holds_wherever_its_bounds_fall_on_the_grid() {
    let xray = Xray::new();
    let stored = seed(&xray, &GRID_OFFSETS).await;

    for case in grid_cases(&stored) {
        let label = case.label;
        let body = json!({"StartTime": case.start, "EndTime": case.end});

        // Following the tokens reports every trace the window holds, exactly
        // once, newest slice first — whatever the window's position on the hour
        // grid.
        let pages = xray.trace_summary_pages(&body).await;
        assert_eq!(
            reported_trace_ids(&pages),
            case.whole,
            "case {label:?}: the window [{}, {}) holds {:?}, got {:?}",
            case.start,
            case.end,
            case.whole,
            reported_trace_ids(&pages)
        );

        // And what the FIRST page of that same walk carries is the slice the
        // window ends in — so a window that straddles a boundary and holds its
        // traces before it answers an empty first page, with a token for the
        // slice that holds them.
        let first = pages
            .first()
            .expect("every window is answered by at least one page");
        assert_eq!(
            reported(first),
            case.first_page,
            "case {label:?}: the first page carries the slice the window ends in, got {first:?}"
        );

        // The count is of the WINDOW, so every page of the one query reports
        // the same one however the window is sliced.
        let held = i64::try_from(case.whole.len()).expect("the case is small");
        for (page, answered) in pages.iter().enumerate() {
            assert_eq!(
                answered.get("TracesProcessedCount").as_i64(),
                Some(held),
                "case {label:?}: page {page} must report the {held} trace(s) the window holds, \
                 got {answered:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_window_positioned_by_the_clock_reports_its_trace_across_its_pages() {
    let xray = Xray::new();
    // The one range type whose instant is the clock's: `Event` windows on when
    // this service last accepted a segment for the trace, so a window drawn
    // around NOW is the shape a dashboard asks for — and where that window sits
    // on the hour grid is not the client's to choose. This one is built to
    // straddle the boundary the current hour began on, so the walk is the only
    // way to read the whole of it.
    let id = trace_id(31);
    // The instant the segment is ACCEPTED is the clock's, so it is bracketed
    // around the call: the trace's update time lies between these two readings
    // however long the ingestion takes.
    let accepted_from = service_com_amazonaws_xray_awsxray::state::now_epoch_seconds();
    let put = xray
        .put_segments(&[root_document("00000000000003a1", &id, "svc", ALIGNED, Some(ALIGNED + 0.5))
            .to_string()])
        .await;
    assert_eq!(put.status, 200, "seeding the trace must succeed, got {put:?}");
    let accepted_to = service_com_amazonaws_xray_awsxray::state::now_epoch_seconds();

    // The window reaches back over the boundary the hour the trace was accepted
    // in began on, so it always straddles one — and it holds every instant the
    // trace could have been accepted at, so what it selects is not the clock's
    // to decide.
    #[allow(clippy::cast_precision_loss)]
    let boundary =
        service_com_amazonaws_xray_awsxray::state::hour_boundary_at_or_before(accepted_to) as f64;
    let body = json!({
        "StartTime": boundary.min(accepted_from) - 60.0,
        "EndTime": accepted_to + 1.0,
        "TimeRangeType": "Event",
    });

    let pages = xray.trace_summary_pages(&body).await;
    assert!(
        pages.len() >= 2,
        "a window reaching back over the hour boundary is cut into at least two slices, got {} \
         page(s) for {body}",
        pages.len()
    );
    assert_eq!(
        reported_trace_ids(&pages),
        vec![id.clone()],
        "the window around the instant the trace was accepted must report it exactly once, got \
         {:?}",
        reported_trace_ids(&pages)
    );
    for (page, answered) in pages.iter().enumerate() {
        assert_eq!(
            answered.get("TracesProcessedCount").as_i64(),
            Some(1),
            "page {page}: the window holds the one accepted trace, got {answered:?}"
        );
    }
}

// =============================================================================
// 7. The window the slices are cut from is the one the request asks about,
//    whichever instant of a trace the window is applied to
// =============================================================================

#[tokio::test]
async fn the_slices_are_cut_on_the_instant_the_request_windows_on() {
    let xray = Xray::new();
    // One trace that STARTS in the window's first slice and ENDS in its second,
    // so the slice it falls in follows from the `TimeRangeType` the request
    // names rather than from the trace alone.
    let id = trace_id(7);
    let start_time = ALIGNED + HOUR - 10.0;
    let document = root_document(
        "00000000000000a1",
        &id,
        "svc",
        start_time,
        Some(ALIGNED + HOUR + 10.0),
    )
    .to_string();
    let put = xray.put_segments(&[document]).await;
    assert_eq!(put.status, 200, "seeding the trace must succeed, got {put:?}");

    // Rows: the range type, and which slice of the window the trace then falls
    // in — the one holding its start time, or the one holding its segment end
    // time. (`Event` windows on the trace's UPDATE time, which is when this
    // service accepted the segment rather than an instant the document carries,
    // so it has no place in a table of windows in the past.)
    let rows: [(&str, f64); 2] = [("TraceId", ALIGNED), ("Service", ALIGNED + HOUR)];

    for (range_type, slice_start) in rows {
        let body = json!({
            "StartTime": ALIGNED,
            "EndTime": ALIGNED + 2.0 * HOUR,
            "TimeRangeType": range_type,
        });
        let first = xray.call("/TraceSummaries", &body).await;
        assert_eq!(
            first.status, 200,
            "case {range_type:?}: the window must be answered, got {first:?}"
        );
        let newest_slice = ALIGNED + HOUR;
        let on_first_page = (slice_start - newest_slice).abs() < 1e-6;
        assert_eq!(
            reported(&first),
            if on_first_page {
                vec![id.clone()]
            } else {
                Vec::new()
            },
            "case {range_type:?}: the trace belongs to the slice at {slice_start}, got {first:?}"
        );

        // And the page of the slice it does belong to reports it, whichever page
        // that is.
        let mut token = next_token(&first);
        let mut found_on: Option<f64> = on_first_page.then_some(newest_slice);
        while let Some(resume) = token {
            let page = xray
                .call(
                    "/TraceSummaries",
                    &{
                        let mut body = body.clone();
                        body["NextToken"] = json!(resume);
                        body
                    },
                )
                .await;
            assert_eq!(
                page.status, 200,
                "case {range_type:?}: the resumed page must be served, got {page:?}"
            );
            if reported(&page) == vec![id.clone()] {
                found_on = approximate_time(&page);
            }
            token = next_token(&page);
        }
        assert!(
            found_on.is_some_and(|at| (at - slice_start).abs() < 1e-6),
            "case {range_type:?}: the trace must be served on the page of the slice at \
             {slice_start}, got {found_on:?}"
        );
    }
}
