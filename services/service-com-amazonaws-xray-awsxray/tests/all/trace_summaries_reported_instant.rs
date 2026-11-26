// SPDX-License-Identifier: CC0-1.0

//! What instant a `GetTraceSummaries` answer is LABELLED with, end to end over
//! the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! One rule: `ApproximateTime` — "the start time of this page of results"
//! (`smithy_query(query="shape",shape="GetTraceSummariesResult",
//! model_path="services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json")`)
//! — is reported on the ABSOLUTE hour grid: it is the start of the hour bucket
//! the stretch of time the page serves closes in, clipped forward to the start
//! of that stretch. So
//!
//! * a page whose stretch reaches back across a top of an hour is labelled by
//!   that boundary — an exact multiple of 3600 epoch seconds, whatever bounds
//!   the request named and whatever the clock reads;
//! * a page whose stretch lies wholly inside one hour is labelled by its own
//!   start, which for a window shorter than one page-length is the requested
//!   `StartTime`;
//! * the label therefore always lies inside the stretch of time the answer
//!   reports on, and never after the requested `EndTime`;
//! * every page cut out of ONE stretch — a stretch holding more summaries than
//!   one page carries — reports that stretch's one label, so the label says
//!   WHICH stretch was served rather than how far into it a page reached;
//! * and walking a window of several page-lengths reports one label per
//!   page-length, an hour earlier each time.
//!
//! The rows differ along what the rule generalises over: the WIDTH of the window
//! (an instant, a minute, ten minutes, exactly one page-length, a second more
//! than one, three of them, the widest window the read serves), where its bounds
//! fall relative to the absolute hour grid (on it, inside one hour, straddling a
//! boundary, ending exactly on one), and how many summaries a stretch holds
//! (none, more than one page carries).
//!
//! Which summaries a page CARRIES is not this suite's rule — that is
//! [`super::trace_summaries_window_paging`] — but the cases that seed traces
//! assert it alongside the label, because labelling an answer must not move a
//! trace off the page the window it falls in serves.

use service_com_amazonaws_xray_awsxray::ops::get_trace_summaries::TRACE_SUMMARIES_PAGE_SIZE;

use super::xray_wire::{
    close, reported, reported_trace_ids, root_document, summaries_body, trace_id, Answer, Xray,
    ALIGNED, HOUR,
};

/// How long one hour is in the epoch seconds every instant on this wire travels
/// as, written out here rather than read from the service, so the expectations
/// below are the rule's own arithmetic and not the implementation's.
const HOUR_SECONDS: f64 = 3_600.0;

/// The hour boundary at or before `instant`: the start of the absolute hour
/// bucket it falls in.
pub fn hour_floor(instant: f64) -> f64 {
    (instant / HOUR_SECONDS).floor() * HOUR_SECONDS
}

/// What the rule says an answer over the half-open stretch `[start, end)` is
/// labelled with: the start of the hour bucket that stretch CLOSES in — the
/// bucket of its last instant, which for an `end` sitting exactly on a boundary
/// is the bucket before that boundary — clipped forward to `start`.
pub fn reported_instant(start: f64, end: f64) -> f64 {
    let closing = hour_floor(end);
    let closing = if closing >= end {
        closing - HOUR_SECONDS
    } else {
        closing
    };
    closing.max(start)
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
    stored
}

// =============================================================================
// 1. One table of windows, and the label the rule gives each of their pages
// =============================================================================

/// One requested window, and the label the rule says each page of a query over
/// it reports — written out as instants of the absolute hour grid, which
/// [`ALIGNED`] sits exactly on.
struct LabelCase {
    label: &'static str,
    start: f64,
    end: f64,
    /// One entry per page the query is served in, in the order the pages are
    /// served.
    reported: Vec<f64>,
}

fn label_cases() -> Vec<LabelCase> {
    vec![
        LabelCase {
            label: "a window of a single instant, on the hour",
            start: ALIGNED,
            end: ALIGNED,
            reported: vec![ALIGNED],
        },
        LabelCase {
            // Wholly inside one hour: the hour it closes in began before the
            // window did, so the window's own start is the first instant the
            // answer reports on.
            label: "a minute wholly inside one hour",
            start: ALIGNED + 600.0,
            end: ALIGNED + 660.0,
            reported: vec![ALIGNED + 600.0],
        },
        LabelCase {
            // Straddling the top of an hour: that boundary is inside the window,
            // so the label is the boundary.
            label: "a minute straddling the top of an hour",
            start: ALIGNED - 30.0,
            end: ALIGNED + 30.0,
            reported: vec![ALIGNED],
        },
        LabelCase {
            // The window's EndTime is exclusive, so a window ending exactly on a
            // boundary closes in the hour BEFORE it and never reports an instant
            // at its own EndTime.
            label: "ten minutes ending exactly on the hour",
            start: ALIGNED - 600.0,
            end: ALIGNED,
            reported: vec![ALIGNED - 600.0],
        },
        LabelCase {
            label: "exactly one page-length, on the grid",
            start: ALIGNED,
            end: ALIGNED + HOUR,
            reported: vec![ALIGNED],
        },
        LabelCase {
            label: "exactly one page-length, off the grid",
            start: ALIGNED + 600.0,
            end: ALIGNED + HOUR + 600.0,
            reported: vec![ALIGNED + HOUR],
        },
        LabelCase {
            // Two page-lengths, the second of them a second long: each is
            // labelled by the hour boundary it reaches back across.
            label: "a second longer than one page-length, on the grid",
            start: ALIGNED,
            end: ALIGNED + HOUR + 1.0,
            reported: vec![ALIGNED + HOUR, ALIGNED],
        },
        LabelCase {
            label: "three page-lengths, on the grid",
            start: ALIGNED,
            end: ALIGNED + 3.0 * HOUR,
            reported: vec![ALIGNED + 2.0 * HOUR, ALIGNED + HOUR, ALIGNED],
        },
        LabelCase {
            label: "three page-lengths, off the grid by half an hour",
            start: ALIGNED + 1_800.0,
            end: ALIGNED + 1_800.0 + 3.0 * HOUR,
            reported: vec![
                ALIGNED + 3.0 * HOUR,
                ALIGNED + 2.0 * HOUR,
                ALIGNED + HOUR,
            ],
        },
    ]
}

#[tokio::test]
async fn every_page_reports_the_hour_its_own_stretch_closes_in() {
    for case in label_cases() {
        let xray = Xray::new();
        let label = case.label;
        let (start, end) = (case.start, case.end);

        let mut token: Option<String> = None;
        let mut seen: Vec<f64> = Vec::new();
        for page in 0..case.reported.len() {
            let answered = xray.summaries(start, end, token.as_deref()).await;
            assert_eq!(
                answered.status, 200,
                "case {label:?}: page {page} must be answered, got {answered:?}"
            );
            let at = approximate_time(&answered);
            let expected = case.reported[page];
            assert!(
                at.is_some_and(|at| close(at, expected)),
                "case {label:?}: page {page} must report {expected}, got {at:?}"
            );
            // Whole seconds, and placeable in the window asked about: every
            // bound in this table is a whole second, and the grid is hours.
            assert!(
                at.is_some_and(|at| at.fract() == 0.0 && at >= start && at <= end),
                "case {label:?}: page {page} must report whole seconds inside [{start}, {end}], \
                 got {at:?}"
            );
            // Either an hour boundary, or the start of the window asked about
            // when the stretch served holds no boundary at all.
            assert!(
                at.is_some_and(|at| at % HOUR_SECONDS == 0.0 || close(at, start)),
                "case {label:?}: page {page} must report an hour boundary or the requested \
                 StartTime {start}, got {at:?}"
            );
            seen.push(at.expect("the page reported an instant"));
            token = next_token(&answered);
            assert_eq!(
                token.is_some(),
                page + 1 < case.reported.len(),
                "case {label:?}: page {page}: a token is due only while the query has a stretch \
                 left to serve, got {token:?}"
            );
        }
        assert_eq!(
            seen.len(),
            case.reported.len(),
            "case {label:?}: the query must be served in {} pages, got {seen:?}",
            case.reported.len()
        );
        // The rule, written out independently of the table, agrees with it on
        // every row: the label of the first page is the hour the window itself
        // closes in.
        let first = reported_instant(start, end);
        assert!(
            close(seen[0], first),
            "case {label:?}: the first page must report {first}, got {}",
            seen[0]
        );
    }
}

// =============================================================================
// 2. A long window is labelled one hour earlier per page, down the hour grid
// =============================================================================

#[tokio::test]
async fn the_widest_window_is_labelled_one_hour_earlier_per_page() {
    let xray = Xray::new();
    // The widest window this read serves, anchored on the grid: every page of it
    // is one page-length of the window, so the labels are consecutive hour
    // boundaries walked backwards from the one the window closes in.
    let start = ALIGNED;
    let end = ALIGNED + 24.0 * HOUR;

    let mut token: Option<String> = None;
    let mut labels: Vec<f64> = Vec::new();
    loop {
        let answered = xray.summaries(start, end, token.as_deref()).await;
        assert_eq!(
            answered.status, 200,
            "page {} must be answered, got {answered:?}",
            labels.len()
        );
        let at = approximate_time(&answered)
            .unwrap_or_else(|| panic!("every page reports an instant, got {answered:?}"));
        assert!(
            at % HOUR_SECONDS == 0.0,
            "page {}: a page whose stretch spans a whole hour must report an hour boundary, got \
             {at}",
            labels.len()
        );
        if let Some(previous) = labels.last() {
            assert!(
                close(previous - at, HOUR_SECONDS),
                "page {}: each page must be labelled exactly one hour before the page before it, \
                 got {at} after {previous}",
                labels.len()
            );
        }
        labels.push(at);
        token = next_token(&answered);
        if token.is_none() {
            break;
        }
    }

    assert_eq!(
        labels.len(),
        24,
        "a 24-hour window is served in 24 pages, got {labels:?}"
    );
    assert!(
        close(labels[0], end - HOUR) && close(labels[23], start),
        "the walk must run from the hour the window closes in down to its start, got {labels:?}"
    );
}

// =============================================================================
// 3. Every page cut out of ONE stretch carries that stretch's one label
// =============================================================================

#[tokio::test]
async fn the_pages_of_one_stretch_share_its_label() {
    let xray = Xray::new();
    // More summaries than one page carries, all inside a window far shorter than
    // one page-length that straddles the top of an hour: the window is one
    // stretch served over several pages, so every page reports that stretch's
    // own label — the boundary it reaches back across — and the pages together
    // still carry every trace the window holds.
    let total = TRACE_SUMMARIES_PAGE_SIZE + 2;
    let offsets: Vec<f64> = (0..total)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let index = index as f64;
            -60.0 + index
        })
        .collect();
    let stored = seed(&xray, &offsets).await;
    let start = ALIGNED - 300.0;
    let end = ALIGNED + 300.0;

    let pages = xray
        .trace_summary_pages(&summaries_body(start, end, None))
        .await;
    assert!(
        pages.len() > 1,
        "{total} summaries must be served over more than one page, got {pages:?}"
    );
    for (index, page) in pages.iter().enumerate() {
        let at = approximate_time(page);
        assert!(
            at.is_some_and(|at| close(at, ALIGNED)),
            "page {index} must report the boundary {ALIGNED} its stretch reaches back across, got \
             {at:?}"
        );
    }
    let mut walked = reported_trace_ids(&pages);
    walked.sort();
    let mut held = stored.clone();
    held.sort();
    assert_eq!(
        walked, held,
        "labelling the answer must not move a trace off the window that holds it, got {pages:?}"
    );
}

// =============================================================================
// 4. The invariants hold wherever the window falls on the grid
// =============================================================================

/// Where the ten-minute window of the sweep below is anchored, in seconds from
/// the absolute hour boundary [`ALIGNED`]: on the boundary, a second each side
/// of it, at the edges of the window's own half-width, around the half hour,
/// just short of the next boundary, and inside the hour before.
const ANCHORS: [f64; 9] = [
    0.0, 1.0, -1.0, 300.0, 301.0, 1_800.0, 3_299.0, 3_599.0, -1_800.0,
];

/// Half the width of the window the sweep asks about: minutes wide, far shorter
/// than one page-length, so each case is one stretch.
const SWEEP_HALF: f64 = 300.0;

#[tokio::test]
async fn the_label_is_an_hour_boundary_or_the_requested_start_wherever_the_window_falls() {
    for anchor in ANCHORS {
        let xray = Xray::new();
        let stored = seed(&xray, &[anchor - 120.0, anchor]).await;
        let start = ALIGNED + anchor - SWEEP_HALF;
        let end = ALIGNED + anchor + SWEEP_HALF;

        let answered = xray.summaries(start, end, None).await;
        assert_eq!(
            answered.status, 200,
            "case {anchor}: the window [{start}, {end}) must be answered, got {answered:?}"
        );
        let at = approximate_time(&answered);
        // Either the hour boundary the window reaches back across, or — when the
        // window lies wholly inside one hour — the requested StartTime.
        assert!(
            at.is_some_and(|at| at % HOUR_SECONDS == 0.0 || close(at, start)),
            "case {anchor}: the label must be an hour boundary or the requested StartTime \
             {start}, got {at:?}"
        );
        assert!(
            at.is_some_and(|at| at >= start && at <= end),
            "case {anchor}: the label must lie inside [{start}, {end}], got {at:?}"
        );
        let expected = reported_instant(start, end);
        assert!(
            at.is_some_and(|at| close(at, expected)),
            "case {anchor}: expected the label {expected}, got {at:?}"
        );
        // Read from the window rather than from the clock, so the identical
        // query asked again is labelled identically.
        let again = xray.summaries(start, end, None).await;
        assert_eq!(
            again.body, answered.body,
            "case {anchor}: the identical query must answer identically, got {again:?} after \
             {answered:?}"
        );
        // And both traces the window holds are still served on its one page.
        assert_eq!(
            reported(&answered),
            vec![stored[1].clone(), stored[0].clone()],
            "case {anchor}: the window's own traces must still be served, newest first, got \
             {answered:?}"
        );
        assert!(
            next_token(&answered).is_none(),
            "case {anchor}: the complete answer to a window shorter than one page-length carries \
             no token, got {answered:?}"
        );
    }
}
