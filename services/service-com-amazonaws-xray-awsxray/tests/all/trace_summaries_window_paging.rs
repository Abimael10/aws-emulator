// SPDX-License-Identifier: CC0-1.0

//! How `GetTraceSummaries` PAGES the window it is asked about, end to end over
//! the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! One rule, asserted over tables that span what it generalises over rather than
//! over one request: the pages of one query are steps of `[StartTime, EndTime)`
//! measured back from the window's OWN `EndTime` — never cut on the absolute
//! epoch-hour grid — so
//!
//! * a window no longer than one step ([`TRACE_PAGE_SECONDS`]) is answered in
//!   ONE page carrying every trace it holds, wherever its bounds fall relative
//!   to the top of an hour, and that page carries no `NextToken`;
//! * `ApproximateTime` ("the start time of this page of results") is the start of
//!   the step the page served — one step before that page ends, and the requested
//!   `StartTime` for the page that reaches it — so it always lies inside the
//!   window asked about;
//! * a `NextToken` is due while the query has anything left to serve: the rest of
//!   a step holding more summaries than one page carries, or the step before this
//!   one while the window reaches into it;
//! * resuming from that token serves the next step, so following the tokens walks
//!   the whole window exactly once, newest step first;
//! * a token naming a page the requested window is not served in, or no page at
//!   all, is refused with the service's one wording for an uninterpretable
//!   pagination token.
//!
//! The rows differ along what the rule generalises over: the WIDTH of the window
//! (nothing, a second, part of an hour, an hour, several hours, the widest window
//! the read serves), where its bounds fall relative to the absolute hour GRID (on
//! the hour, inside one hour, straddling a boundary), how many traces a step holds
//! (none, some, more than one page), and which step of the window a trace falls
//! in.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::ops::get_trace_summaries::TRACE_SUMMARIES_PAGE_SIZE;

use super::xray_wire::{
    reported, reported_trace_ids, root_document, trace_id, Answer, Xray, ALIGNED, HOUR,
};

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

/// Store one trace per entry of `offsets`, the `index`th of them starting
/// `offsets[index]` seconds from [`ALIGNED`], and answer their ids in the order
/// the ids sort.
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
// 1. The first page reports the start of the window's last step, and a token
//    only while an earlier step of the window remains
// =============================================================================

/// One window, and what the rule says the first page of a query over it reports:
/// the start of the step the window ends in, and whether a token is due.
struct WindowCase {
    label: &'static str,
    start: f64,
    end: f64,
    /// `ApproximateTime` the rule requires: the start of the page's own step,
    /// which is the requested `StartTime` for a window no longer than one step.
    page_start: f64,
    /// Whether the window reaches into a step before the one it ends in.
    earlier_step: bool,
}

fn window_cases() -> Vec<WindowCase> {
    vec![
        WindowCase {
            label: "a window of a single instant",
            start: ALIGNED,
            end: ALIGNED,
            page_start: ALIGNED,
            earlier_step: false,
        },
        WindowCase {
            label: "a window one second wide, ending on the hour",
            start: ALIGNED - 1.0,
            end: ALIGNED,
            page_start: ALIGNED - 1.0,
            earlier_step: false,
        },
        WindowCase {
            label: "part of an hour, starting on the hour",
            start: ALIGNED,
            end: ALIGNED + 42.0 * 60.0,
            page_start: ALIGNED,
            earlier_step: false,
        },
        WindowCase {
            label: "part of an hour, starting inside it",
            start: ALIGNED + 600.0,
            end: ALIGNED + 1_200.0,
            page_start: ALIGNED + 600.0,
            earlier_step: false,
        },
        WindowCase {
            label: "minutes wide, straddling the top of an hour",
            start: ALIGNED + HOUR - 600.0,
            end: ALIGNED + HOUR + 600.0,
            page_start: ALIGNED + HOUR - 600.0,
            earlier_step: false,
        },
        WindowCase {
            label: "exactly one step, on the grid",
            start: ALIGNED,
            end: ALIGNED + HOUR,
            page_start: ALIGNED,
            earlier_step: false,
        },
        WindowCase {
            label: "exactly one step, off the grid",
            start: ALIGNED + 600.0,
            end: ALIGNED + HOUR + 600.0,
            page_start: ALIGNED + 600.0,
            earlier_step: false,
        },
        WindowCase {
            label: "one second longer than a step",
            start: ALIGNED,
            end: ALIGNED + HOUR + 1.0,
            page_start: ALIGNED + 1.0,
            earlier_step: true,
        },
        WindowCase {
            label: "several steps, on the grid",
            start: ALIGNED,
            end: ALIGNED + 6.0 * HOUR,
            page_start: ALIGNED + 5.0 * HOUR,
            earlier_step: true,
        },
        WindowCase {
            label: "several steps, off the grid",
            start: ALIGNED + 600.0,
            end: ALIGNED + 3.0 * HOUR + 600.0,
            page_start: ALIGNED + 2.0 * HOUR + 600.0,
            earlier_step: true,
        },
        WindowCase {
            label: "the widest window this read serves",
            start: ALIGNED,
            end: ALIGNED + 24.0 * HOUR,
            page_start: ALIGNED + 23.0 * HOUR,
            earlier_step: true,
        },
    ]
}

#[tokio::test]
async fn the_first_page_reports_the_start_of_the_windows_last_step() {
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
            "case {label:?}: expected ApproximateTime {} (the page's own step start), got {at:?}",
            case.page_start
        );
        // Every bound in this table is a whole second, so the instant derived
        // from it is one too.
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
            case.earlier_step,
            "case {label:?}: a token is due only while an earlier step of the window remains, \
             got {answered:?}"
        );
    }
}

// =============================================================================
// 2. Following the tokens walks the window one step at a time, newest first
// =============================================================================

#[tokio::test]
async fn the_pages_walk_the_window_one_step_at_a_time_newest_first() {
    let xray = Xray::new();
    // A window whose bounds sit OFF the absolute hour grid, with one trace in
    // each of its three steps and one the window excludes: which page a trace
    // appears on follows from the window's own end, so the steps here are
    // half-hour offsets from the grid.
    let stored = seed(
        &xray,
        &[
            1_810.0,
            HOUR + 1_810.0,
            2.0 * HOUR + 1_810.0,
            3.0 * HOUR + 1_810.0,
        ],
    )
    .await;
    let (start, end) = (ALIGNED + 1_800.0, ALIGNED + 1_800.0 + 3.0 * HOUR);

    // The pages, newest step first: each step's own start, and the trace that
    // step holds.
    let expected: Vec<(f64, Vec<String>)> = vec![
        (end - HOUR, vec![stored[2].clone()]),
        (end - 2.0 * HOUR, vec![stored[1].clone()]),
        (end - 3.0 * HOUR, vec![stored[0].clone()]),
    ];

    let mut token: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();
    for (page, (step_start, holds)) in expected.iter().enumerate() {
        let answered = xray.summaries(start, end, token.as_deref()).await;
        assert_eq!(
            answered.status, 200,
            "page {page}: the step must be served, got {answered:?}"
        );
        assert!(
            approximate_time(&answered).is_some_and(|at| (at - step_start).abs() < 1e-6),
            "page {page}: expected ApproximateTime {step_start} (that step's start), got \
             {answered:?}"
        );
        assert_eq!(
            &reported(&answered),
            holds,
            "page {page}: the step [{step_start}, {}) holds {holds:?}, got {answered:?}",
            step_start + HOUR
        );
        // The count is of the traces the WINDOW holds, so it is the same on
        // every page of the one query — never of the step just served.
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
            "page {page}: a token is due only while an earlier step remains, got {token:?}"
        );
    }
    assert_eq!(
        seen,
        vec![stored[2].clone(), stored[1].clone(), stored[0].clone()],
        "the walk must yield every trace of the window exactly once, newest step first, got \
         {seen:?}"
    );
}

// =============================================================================
// 3. A step holding more than one page is served over several pages, each
//    reporting that same step's start
// =============================================================================

#[tokio::test]
async fn a_step_holding_more_than_one_page_is_served_over_several_pages() {
    let xray = Xray::new();
    // Two traces more than a page carries, all inside the one step the window
    // covers, so the step itself is what has to be paged.
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
        // Every page of one step reports that step's own start: the instant
        // reported says WHICH step was served, not how far into it the page
        // reached.
        assert!(
            approximate_time(&answered).is_some_and(|at| (at - ALIGNED).abs() < 1e-6),
            "page {pages}: expected ApproximateTime {ALIGNED} (the step's start), got \
             {answered:?}"
        );
        seen.extend(ids);
        token = next_token(&answered);
        assert_eq!(
            token.is_some(),
            seen.len() < stored.len(),
            "page {pages}: a token is due while the step has summaries left, got {token:?} \
             after {} of {}",
            seen.len(),
            stored.len()
        );
        pages += 1;
        assert!(
            pages <= total,
            "paging one step must terminate, stopped after {pages} pages"
        );
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        seen, stored,
        "paging one step must yield every trace of it exactly once, got {seen:?}"
    );
}

// =============================================================================
// 4. A token that names no page of the requested window is not a cursor of it
// =============================================================================

#[tokio::test]
async fn a_token_outside_the_requested_window_is_refused_with_the_one_wording() {
    let xray = Xray::new();
    let stored = seed(&xray, &[10.0, HOUR + 10.0]).await;
    // Two steps long, so the query has pages 0 and 1 and nothing else.
    let (start, end) = (ALIGNED, ALIGNED + 2.0 * HOUR);

    // Rows differ along HOW the token misses a page of this window: the page
    // after the last one the window has, a page far past it, a page past the
    // most any window is served in, a value that is no page index at all
    // (negative, fractional, textual, an instant of the kind an internal
    // boundary once was), a page index with nothing after the separator, and a
    // page of this window naming a trace past every stored one.
    let rows: Vec<(&str, String)> = vec![
        ("the page after the window's last", "2".to_string()),
        ("a page far past the window's last", "7".to_string()),
        (
            "a page past the most any window is served in",
            "24".to_string(),
        ),
        ("a negative page index", "-1".to_string()),
        ("a fractional page index", "0.5".to_string()),
        ("no page index at all", "not-a-token".to_string()),
        ("an epoch instant rather than a page", format!("{ALIGNED:.0}")),
        ("a page index with no trace after it", "0:".to_string()),
        ("a trace past every stored one", format!("0:{}", trace_id(99))),
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

    // And the token this query DID issue resumes it: the step before the one
    // the window ends in, holding the trace that step holds.
    let first = xray.summaries(start, end, None).await;
    let issued = next_token(&first)
        .unwrap_or_else(|| panic!("a two-step window must issue a token, got {first:?}"));
    let resumed = xray.summaries(start, end, Some(&issued)).await;
    assert_eq!(
        (resumed.status, reported(&resumed)),
        (200, vec![stored[0].clone()]),
        "the issued token must serve the earlier step, got {resumed:?}"
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
            "a window reaching back from the epoch",
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
// 6. A window reports every trace it holds, wherever its bounds fall relative
//    to the absolute hour grid — and a window no longer than one step reports
//    them all on its FIRST page
// =============================================================================

/// One window over the store [`GRID_OFFSETS`] seeds, and what the rule says a
/// reader of it sees: the trace ids the WHOLE query reports (newest step first),
/// and the ids its FIRST page carries — which differ only when the window is
/// longer than one step.
struct GridCase {
    label: &'static str,
    start: f64,
    end: f64,
    whole: Vec<String>,
    first_page: Vec<String>,
}

/// The windows the rule is asserted over, against a store holding one trace just
/// before an absolute hour boundary, one just after it, and one an hour later:
/// `stored` is the ids of the traces seeded at [`GRID_OFFSETS`], in that order.
fn grid_cases(stored: &[String]) -> Vec<GridCase> {
    let (before, inside, later) = (stored[0].clone(), stored[1].clone(), stored[2].clone());
    vec![
        GridCase {
            label: "a window wholly inside one absolute hour",
            start: ALIGNED,
            end: ALIGNED + 60.0,
            whole: vec![inside.clone()],
            first_page: vec![inside.clone()],
        },
        GridCase {
            label: "a minutes-wide window straddling a boundary, its trace before it",
            start: ALIGNED - 60.0,
            end: ALIGNED + 1.0,
            whole: vec![before.clone()],
            first_page: vec![before.clone()],
        },
        GridCase {
            label: "a minutes-wide window straddling a boundary, a trace each side",
            start: ALIGNED - 60.0,
            end: ALIGNED + 60.0,
            whole: vec![before.clone(), inside.clone()],
            first_page: vec![before.clone(), inside.clone()],
        },
        GridCase {
            label: "a window ending exactly on a boundary",
            start: ALIGNED - 60.0,
            end: ALIGNED,
            whole: vec![before.clone()],
            first_page: vec![before.clone()],
        },
        GridCase {
            label: "two steps, cut off the grid",
            start: ALIGNED + 30.0,
            end: ALIGNED + HOUR + 31.0,
            whole: vec![later.clone(), inside.clone()],
            first_page: vec![later.clone()],
        },
        GridCase {
            label: "a window spanning three steps",
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
/// [`ALIGNED`]: one in the absolute hour before that boundary, one just after it,
/// and one an hour later.
const GRID_OFFSETS: [f64; 3] = [-30.0, 30.0, HOUR + 30.0];

#[tokio::test]
async fn a_window_reports_every_trace_it_holds_wherever_its_bounds_fall_on_the_grid() {
    let xray = Xray::new();
    let stored = seed(&xray, &GRID_OFFSETS).await;

    for case in grid_cases(&stored) {
        let label = case.label;
        let body = json!({"StartTime": case.start, "EndTime": case.end});

        // Following the tokens reports every trace the window holds, exactly
        // once, newest step first.
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

        // And what the FIRST page of that same walk carries is the step the
        // window ends in — which for a window no longer than one step is the
        // whole window, so a client that never paginates still reads it all.
        let first = pages
            .first()
            .expect("every window is answered by at least one page");
        assert_eq!(
            reported(first),
            case.first_page,
            "case {label:?}: the first page carries the step the window ends in, got {first:?}"
        );

        // The count is of the WINDOW, so every page of the one query reports
        // the same one however many steps the window is served in.
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
async fn a_just_accepted_trace_is_on_the_first_page_of_a_minutes_wide_window() {
    let xray = Xray::new();
    // The read-after-write shape: submit a segment, then ask for the few
    // minutes around it. The one range type whose instant is the clock's is
    // `Event` — it windows on when this service last accepted a segment for the
    // trace — so this window is positioned by the clock rather than by the
    // caller, and where it happens to sit relative to the top of an hour must
    // not decide what it answers.
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

    // Five minutes back from now: shorter than one step, holding every instant
    // the trace could have been accepted at — and, run at some point in any
    // hour, a window that straddles the top of one about one time in twelve.
    let start = accepted_from - 300.0;
    let end = accepted_to + 1.0;
    let body = json!({
        "StartTime": start,
        "EndTime": end,
        "TimeRangeType": "Event",
    });

    let answered = xray.call("/TraceSummaries", &body).await;
    assert_eq!(
        answered.status, 200,
        "the window around the accepted trace must be answered, got {answered:?}"
    );
    assert_eq!(
        reported(&answered),
        vec![id.clone()],
        "a window shorter than one step must report the trace it holds on its first page, got \
         {answered:?}"
    );
    assert_eq!(
        answered.get("TracesProcessedCount").as_i64(),
        Some(1),
        "the window holds the one accepted trace, got {answered:?}"
    );
    assert!(
        next_token(&answered).is_none(),
        "the complete answer to a window shorter than one step carries no token, got \
         {answered:?}"
    );
    assert!(
        approximate_time(&answered).is_some_and(|at| (at - start).abs() < 1e-6),
        "the only page of that window begins at the requested StartTime {start}, got {answered:?}"
    );
}

// =============================================================================
// 7. The window the steps are cut from is the one the request asks about,
//    whichever instant of a trace the window is applied to
// =============================================================================

#[tokio::test]
async fn the_steps_are_cut_on_the_instant_the_request_windows_on() {
    let xray = Xray::new();
    // One trace that STARTS in the window's earlier step and ENDS in its later
    // one, so the step it falls in follows from the `TimeRangeType` the request
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

    // Rows: the range type, and which step of the window the trace then falls
    // in — the one holding its start time, or the one holding its segment end
    // time. (`Event` windows on the trace's UPDATE time, which is when this
    // service accepted the segment rather than an instant the document carries,
    // so it has no place in a table of windows in the past.)
    let rows: [(&str, f64); 2] = [("TraceId", ALIGNED), ("Service", ALIGNED + HOUR)];

    for (range_type, step_start) in rows {
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
        let newest_step = ALIGNED + HOUR;
        let on_first_page = (step_start - newest_step).abs() < 1e-6;
        assert_eq!(
            reported(&first),
            if on_first_page {
                vec![id.clone()]
            } else {
                Vec::new()
            },
            "case {range_type:?}: the trace belongs to the step at {step_start}, got {first:?}"
        );

        // And the page of the step it does belong to reports it, whichever page
        // that is.
        let mut token = next_token(&first);
        let mut found_on: Option<f64> = on_first_page.then_some(newest_step);
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
            found_on.is_some_and(|at| (at - step_start).abs() < 1e-6),
            "case {range_type:?}: the trace must be served on the page of the step at \
             {step_start}, got {found_on:?}"
        );
    }
}
