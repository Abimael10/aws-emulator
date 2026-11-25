// SPDX-License-Identifier: CC0-1.0

//! WHICH summaries one page of `GetTraceSummaries` carries, end to end over the
//! wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! One rule, asserted over tables that span what it generalises over rather than
//! over one request: a page of a trace query carries THE MOST RECENT of the
//! summaries its part of the window holds — "the first page contains the most
//! recent results, closest to the end of the time frame"
//! (`smithy_query(query="doc",shape="GetTraceSummariesResult",member="NextToken")`)
//! — so the summaries a window holds are cut into pages in the order of the
//! instant the request windows on, descending from the end of the window, and
//! nothing about a trace's ID or about the order the store is walked in decides
//! which page it lands on. A window holding no more summaries than one page
//! carries ([`TRACE_SUMMARIES_PAGE_SIZE`]) is therefore answered WHOLE, and a
//! trace just written is on the first page of the minutes-wide window that
//! covers it however many traces that window already held.
//!
//! The rows differ along what the rule generalises over: how the lexicographic
//! order of the trace ids relates to the order of their times (ascending with
//! time, descending against it, permuted, all equal), how many traces the window
//! holds relative to one page (fewer, exactly a page, more than a page, more than
//! two pages), which instant of a trace the window is applied to (the trace's
//! start time, its segment end time), and whether the query narrows itself with a
//! filter expression.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::ops::get_trace_summaries::TRACE_SUMMARIES_PAGE_SIZE;

use super::xray_wire::{
    array, reported, reported_trace_ids, root_document, trace_id, Answer, Xray, ALIGNED,
};

/// How long the window every case below asks about is: minutes wide, far shorter
/// than one step of a query's window, so every case is about the order INSIDE
/// one page's step rather than about how a window is stepped.
const WINDOW_HALF: f64 = 300.0;

/// One trace a case stores: the number its id is spelled from, and where the
/// instant the case's query windows on falls, in seconds from [`ALIGNED`].
#[derive(Debug, Clone, Copy)]
struct Stored {
    id: u32,
    at: f64,
}

/// One window over a store the case seeds, and how the rule says its pages are
/// cut.
struct Case {
    label: &'static str,
    /// The traces the window holds, in the order the case seeds them.
    stored: Vec<Stored>,
    /// Which instant of a trace the query windows on: `None` asks for the
    /// default, which is the trace's own start time.
    range_type: Option<&'static str>,
    /// The filter expression the query carries, if any.
    filter: Option<&'static str>,
}

/// The service every seeded root segment reports, which the filtered row below
/// selects on.
const SERVICE: &str = "svc";

/// The traces of `count` cases, placed one second apart ending a minute before
/// [`ALIGNED`], with their ids spelled by `id_of` from each one's position in
/// time.
fn placed(count: usize, id_of: impl Fn(usize) -> u32) -> Vec<Stored> {
    (0..count)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let offset = -60.0 + index as f64;
            Stored {
                id: id_of(index),
                at: offset,
            }
        })
        .collect()
}

fn cases() -> Vec<Case> {
    // A permutation of the ids that follows neither the time order nor its
    // reverse, so a page cut in id order can be told from one cut in time order
    // by more than a reversal.
    const PERMUTED: [u32; 7] = [40, 17, 93, 4, 61, 25, 78];

    vec![
        Case {
            label: "fewer traces than a page, ids ascending with time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE - 1, |index| {
                u32::try_from(index + 1).expect("the index fits")
            }),
            range_type: None,
            filter: None,
        },
        Case {
            label: "exactly a page, ids DESCENDING against time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE, |index| {
                u32::try_from(TRACE_SUMMARIES_PAGE_SIZE - index).expect("the page size fits")
            }),
            range_type: None,
            filter: None,
        },
        Case {
            label: "two traces more than a page, ids ascending with time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE + 2, |index| {
                u32::try_from(index + 1).expect("the index fits")
            }),
            range_type: None,
            filter: None,
        },
        Case {
            label: "two traces more than a page, ids descending against time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE + 2, |index| {
                u32::try_from(TRACE_SUMMARIES_PAGE_SIZE + 2 - index).expect("the page size fits")
            }),
            range_type: None,
            filter: None,
        },
        Case {
            label: "more than two pages, ids ascending with time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE * 2 + 1, |index| {
                u32::try_from(index + 1).expect("the index fits")
            }),
            range_type: None,
            filter: None,
        },
        Case {
            label: "ids permuted against the order of their times",
            stored: placed(PERMUTED.len(), |index| PERMUTED[index]),
            range_type: None,
            filter: None,
        },
        Case {
            label: "traces sharing one instant, ids permuted",
            stored: PERMUTED
                .iter()
                .map(|id| Stored {
                    id: *id,
                    at: -42.0,
                })
                .collect(),
            range_type: None,
            filter: None,
        },
        Case {
            label: "the window applied to each trace's segment end time",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE + 2, |index| {
                u32::try_from(TRACE_SUMMARIES_PAGE_SIZE + 2 - index).expect("the page size fits")
            }),
            range_type: Some("Service"),
            filter: None,
        },
        Case {
            label: "a filtered query over more traces than a page",
            stored: placed(TRACE_SUMMARIES_PAGE_SIZE + 2, |index| {
                u32::try_from(index + 1).expect("the index fits")
            }),
            range_type: None,
            filter: Some("service(\"svc\")"),
        },
    ]
}

/// Store the traces `case` names and answer their ids in the order the rule says
/// the pages of its query report them: by the instant the query windows on,
/// descending, with the traces sharing one instant ordered by their ids so the
/// order is total.
///
/// The instant the case windows on is written into the document the member the
/// query names reads: the trace's own `start_time` by default, and the segment's
/// `end_time` for a query that windows on when a trace's segments finished.
async fn seed(xray: &Xray, case: &Case) -> Vec<String> {
    let windows_on_end = case.range_type == Some("Service");
    for (index, trace) in case.stored.iter().enumerate() {
        let id = trace_id(trace.id);
        let at = ALIGNED + trace.at;
        let (start, end) = if windows_on_end {
            (at - 5.0, at)
        } else {
            (at, at + 0.5)
        };
        let document =
            root_document(&format!("{:016x}", index + 1), &id, SERVICE, start, Some(end))
                .to_string();
        let put = xray.put_segments(&[document]).await;
        assert_eq!(
            (put.status, array(&put.body, "UnprocessedTraceSegments").len()),
            (200, 0),
            "case {:?}: seeding {id} at {at} must be accepted whole, got {put:?}",
            case.label
        );
    }

    let mut order = case.stored.clone();
    order.sort_by(|left, right| {
        right
            .at
            .total_cmp(&left.at)
            .then_with(|| left.id.cmp(&right.id))
    });
    order.iter().map(|trace| trace_id(trace.id)).collect()
}

/// The request body of `case`'s query, carrying the token `token` when one is
/// given.
fn query(case: &Case, token: Option<&str>) -> Value {
    let mut body = json!({
        "StartTime": ALIGNED - WINDOW_HALF,
        "EndTime": ALIGNED + WINDOW_HALF,
    });
    if let Some(range_type) = case.range_type {
        body["TimeRangeType"] = json!(range_type);
    }
    if let Some(filter) = case.filter {
        body["FilterExpression"] = json!(filter);
    }
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    body
}

/// The `NextToken` of an answer.
fn next_token(answer: &Answer) -> Option<String> {
    answer.get("NextToken").as_str().map(str::to_string)
}

#[tokio::test]
async fn the_first_page_carries_the_windows_most_recent_summaries() {
    for case in cases() {
        let label = case.label;
        let xray = Xray::new();
        let newest_first = seed(&xray, &case).await;

        let first = xray.call("/TraceSummaries", &query(&case, None)).await;
        assert_eq!(
            first.status, 200,
            "case {label:?}: the window must be answered, got {first:?}"
        );
        let due: Vec<String> = newest_first
            .iter()
            .take(TRACE_SUMMARIES_PAGE_SIZE)
            .cloned()
            .collect();
        assert_eq!(
            reported(&first),
            due,
            "case {label:?}: expected the most recent summaries {due:?} on the first page, got {:?}",
            reported(&first)
        );
        assert_eq!(
            next_token(&first).is_some(),
            newest_first.len() > TRACE_SUMMARIES_PAGE_SIZE,
            "case {label:?}: a token is due only while the window has summaries left, got {first:?}"
        );

        // Asked again, unchanged: which summaries a page carries follows from the
        // window and from the traces it holds, so a client that re-asks the same
        // unpaginated query reads the same most recent results rather than a
        // different arbitrary subset.
        let again = xray.call("/TraceSummaries", &query(&case, None)).await;
        assert_eq!(
            again.body, first.body,
            "case {label:?}: the identical query must answer identically, got {again:?} after \
             {first:?}"
        );
    }
}

#[tokio::test]
async fn following_the_tokens_walks_the_window_newest_first() {
    for case in cases() {
        let label = case.label;
        let xray = Xray::new();
        let newest_first = seed(&xray, &case).await;

        let pages = xray.trace_summary_pages(&query(&case, None)).await;
        assert_eq!(
            reported_trace_ids(&pages),
            newest_first,
            "case {label:?}: the pages must report every summary the window holds exactly once, \
             newest first, got {:?}",
            reported_trace_ids(&pages)
        );
        for (position, page) in pages.iter().enumerate() {
            let ids = reported(page);
            assert!(
                ids.len() <= TRACE_SUMMARIES_PAGE_SIZE,
                "case {label:?}: page {position} carries at most {TRACE_SUMMARIES_PAGE_SIZE} \
                 summaries, got {ids:?}"
            );
        }
        let held = i64::try_from(newest_first.len()).expect("the case is small");
        for (position, page) in pages.iter().enumerate() {
            assert_eq!(
                page.get("TracesProcessedCount").as_i64(),
                Some(held),
                "case {label:?}: page {position} must report the {held} traces the window holds, \
                 got {page:?}"
            );
        }
    }
}

/// How many traces the window below already holds before the trace under test is
/// written into it: more than one page, so a page cut anywhere but at the end of
/// the window would miss the newest trace.
const ALREADY_HELD: usize = TRACE_SUMMARIES_PAGE_SIZE + 3;

#[tokio::test]
async fn a_just_written_trace_is_on_the_first_page_of_a_busy_window() {
    // The read-after-write shape over a window that is not empty: the ids of the
    // traces already in it are spelled HIGH and the new one's LOW, so a page
    // taken in id order cannot hold the new trace while a page taken in time
    // order has to. The rows differ along which instant the query windows on.
    for range_type in [None, Some("Service"), Some("Event")] {
        let label = range_type.unwrap_or("the default");
        let xray = Xray::new();

        // `Event` windows on when this service ACCEPTED a segment, which is the
        // clock's instant rather than one a document carries, so that row is
        // placed around the clock and the others around the fixed instant.
        let windows_on_accept = range_type == Some("Event");
        let anchor = if windows_on_accept {
            service_com_amazonaws_xray_awsxray::state::now_epoch_seconds()
        } else {
            ALIGNED
        };

        for index in 0..ALREADY_HELD {
            let id = trace_id(u32::try_from(0xffff_ff00 + index).expect("the index fits"));
            #[allow(clippy::cast_precision_loss)]
            let at = anchor - 120.0 + index as f64;
            let document = root_document(
                &format!("{:016x}", index + 1),
                &id,
                SERVICE,
                at,
                Some(at + 0.5),
            )
            .to_string();
            let put = xray.put_segments(&[document]).await;
            assert_eq!(
                put.status, 200,
                "case {label:?}: seeding {id} must succeed, got {put:?}"
            );
        }

        // The trace under test: the newest in the window, and the lowest id in
        // it.
        let newest = trace_id(1);
        let at = anchor - 1.0;
        let put = xray
            .put_segments(&[root_document(
                "00000000000000ff",
                &newest,
                SERVICE,
                at,
                Some(at + 0.5),
            )
            .to_string()])
            .await;
        assert_eq!(
            (put.status, array(&put.body, "UnprocessedTraceSegments").len()),
            (200, 0),
            "case {label:?}: the trace under test must be accepted whole, got {put:?}"
        );

        let mut body = json!({
            "StartTime": anchor - WINDOW_HALF,
            "EndTime": anchor + WINDOW_HALF,
        });
        if let Some(range_type) = range_type {
            body["TimeRangeType"] = json!(range_type);
        }
        let answered = xray.call("/TraceSummaries", &body).await;
        assert_eq!(
            answered.status, 200,
            "case {label:?}: the window must be answered, got {answered:?}"
        );
        let ids = reported(&answered);
        assert_eq!(
            ids.first().map(String::as_str),
            Some(newest.as_str()),
            "case {label:?}: the newest trace in the window must head its first page, got {ids:?}"
        );
        assert_eq!(
            ids.len(),
            TRACE_SUMMARIES_PAGE_SIZE.min(ALREADY_HELD + 1),
            "case {label:?}: the first page carries a page of the window's most recent summaries, \
             got {ids:?}"
        );
    }
}
