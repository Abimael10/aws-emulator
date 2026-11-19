// SPDX-License-Identifier: CC0-1.0

//! What a `@paginated` X-Ray read SAYS when it is handed a pagination token it
//! cannot interpret, end to end over the wire (the `Xray` harness in
//! [`super::xray_wire`]).
//!
//! One rule for every paginated read of this service: a `NextToken` that is not
//! a cursor a previous response of the same query handed out — unparseable,
//! naming nothing in the keyspace being walked, or past its end — is refused
//! with the operation's modelled client-input error, and the message is the
//! service's own FIXED text followed by the offending token taken verbatim from
//! the request: nothing quotes it, nothing follows it, and nothing else of the
//! request or of the service reaches the sentence.
//!
//! The rows below differ along what the rule generalises over: WHICH paginated
//! operation is called (a trace query, a listing, a graph walk, an aggregation),
//! what KIND of cursor that operation issues (a trace id, a name, an offset, a
//! reference id, an epoch second), and the FORM of the token sent (a word, a
//! number past the end, a number too large for the cursor's type, text with
//! spaces and punctuation, the longest token the model admits, the empty
//! string). A token the service DID issue still pages, so the refusal is about
//! tokens this service cannot interpret and not about paging at all.
//!
//! That last part is asserted over a query whose window is a FIXED pair of
//! instants reused on every page, because a token of this read is a cursor into
//! the query that issued it — the window included. A walk that re-reads the
//! clock for each page asks about a window that has moved, which is a different
//! query, so what it gets back would depend on how much time passed between two
//! calls rather than on the rule.
//!
//! `BatchGetTraces` is not a row: it serves every id one call carries, so it
//! hands out no cursor and refuses ANY token for being a token at all rather
//! than for not being one it issued (see `trace_id_list_reads.rs`).

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::ops::get_trace_summaries::TRACE_SUMMARIES_PAGE_SIZE;
use service_com_amazonaws_xray_awsxray::state::invalid_next_token_message;

use super::xray_wire::{
    reported, reported_trace_ids, root_document, summaries_body, trace_id, Answer, Xray, ALIGNED,
    HOUR,
};

/// Now, in the epoch seconds every timestamp on this wire travels as.
fn now() -> f64 {
    service_com_amazonaws_xray_awsxray::state::now_epoch_seconds()
}

/// The refusal message of an answer, or the empty string.
fn said(answer: &Answer) -> &str {
    answer.message().unwrap_or_default()
}

/// Token forms no paginated read of this service can interpret, whichever kind
/// of cursor it issues: none of them parses as a number, none names a stored
/// group, rule or insight, and each sorts past every keyspace a fresh store
/// holds.
///
/// The longest row is 100 characters, the maximum `GetGroupsNextToken`
/// @length admits
/// (`smithy_query(query="shape",shape="GetGroupsNextToken")`), so one request
/// shape does not refuse the row before a handler sees it.
fn uninterpretable_tokens() -> Vec<String> {
    vec![
        "bogus-token".to_string(),
        "never-issued".to_string(),
        "zzz-past-every-keyspace".to_string(),
        "token with spaces".to_string(),
        "tok+en/with=special&chars".to_string(),
        "x".repeat(100),
    ]
}

/// One emulator holding exactly one completed trace, `trace_id(1)`.
///
/// Every row's store holds it so that a read resolving a trace id BEFORE it
/// reads the token reaches the token at all, and so that each keyspace a
/// cursor is judged against is non-empty. Every token form above sorts past
/// this id, so none of them is a cursor inside it.
async fn seeded() -> Xray {
    let xray = Xray::new();
    let document = root_document(
        "1111111111111111",
        &trace_id(1),
        "svc",
        now() - 60.0,
        Some(now() - 59.0),
    )
    .to_string();
    let put = xray.put_segments(&[document]).await;
    assert_eq!(
        put.status, 200,
        "seeding one trace must succeed, got {put:?}"
    );
    xray
}

/// Assert one refusal: 400 `InvalidRequestException` whose message is the fixed
/// text plus `token`, verbatim and undecorated.
fn assert_invalid_token(case: &str, token: &str, refused: &Answer) {
    assert_eq!(
        (refused.status, refused.error_type.as_deref()),
        (400, Some("InvalidRequestException")),
        "case {case:?}: expected 400 InvalidRequestException, got {refused:?}"
    );
    let expected = format!("Invalid token {token}");
    assert_eq!(
        said(refused),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        said(refused)
    );
}

// =============================================================================
// 1. The fixed text is the service's own, and the token is all that varies
// =============================================================================

#[tokio::test]
async fn the_fixed_text_carries_the_token_verbatim_and_nothing_else() {
    let mut forms = uninterpretable_tokens();
    forms.push(String::new());
    forms.push("7".to_string());

    for token in &forms {
        let message = invalid_next_token_message(token);
        assert_eq!(
            message,
            format!("Invalid token {token}"),
            "case {token:?}: the fixed text must be the service's own, got {message:?}"
        );
        assert!(
            message.starts_with("Invalid token "),
            "case {token:?}: the fixed text leads the message, got {message:?}"
        );
        assert!(
            message.ends_with(token.as_str()),
            "case {token:?}: nothing follows the token, got {message:?}"
        );
        assert!(
            !message.contains('`') && !message.contains('\'') && !message.contains('"'),
            "case {token:?}: the token is reported undecorated, got {message:?}"
        );
        assert!(
            !message.contains("service"),
            "case {token:?}: the message explains no provenance, got {message:?}"
        );
    }
}

// =============================================================================
// 2. Every paginated read words an uninterpretable token the same way
// =============================================================================

/// One row per paginated read: its label, its URI, and the request body it is
/// called with for a given token. A fresh emulator serves each row, so the
/// keyspace every token is judged against is the one a fresh store holds.
#[allow(clippy::type_complexity)]
fn paginated_reads() -> Vec<(&'static str, &'static str, Box<dyn Fn(&str) -> Value>)> {
    vec![
        (
            "GetTraceSummaries",
            "/TraceSummaries",
            Box::new(|token: &str| {
                json!({"StartTime": now() - 3_600.0, "EndTime": now(), "NextToken": token})
            }),
        ),
        (
            "GetGroups",
            "/Groups",
            Box::new(|token: &str| json!({"NextToken": token})),
        ),
        (
            "GetIndexingRules",
            "/GetIndexingRules",
            Box::new(|token: &str| json!({"NextToken": token})),
        ),
        (
            "GetSamplingRules",
            "/GetSamplingRules",
            Box::new(|token: &str| json!({"NextToken": token})),
        ),
        (
            "GetSamplingStatisticSummaries",
            "/SamplingStatisticSummaries",
            Box::new(|token: &str| json!({"NextToken": token})),
        ),
        (
            "GetTimeSeriesServiceStatistics",
            "/TimeSeriesServiceStatistics",
            Box::new(|token: &str| {
                json!({"StartTime": now() - 3_600.0, "EndTime": now(), "NextToken": token})
            }),
        ),
        (
            "GetServiceGraph",
            "/ServiceGraph",
            Box::new(|token: &str| {
                json!({"StartTime": now() - 3_600.0, "EndTime": now(), "NextToken": token})
            }),
        ),
        (
            "GetInsightSummaries",
            "/InsightSummaries",
            Box::new(|token: &str| {
                // The read resolves a group before it reads the token, and
                // refuses a request naming none, so the row names the reserved
                // account-level group every region holds.
                json!({
                    "StartTime": now() - 3_600.0,
                    "EndTime": now(),
                    "GroupName": "Default",
                    "NextToken": token,
                })
            }),
        ),
        (
            "GetTraceGraph",
            "/TraceGraph",
            Box::new(|token: &str| json!({"TraceIds": [trace_id(1)], "NextToken": token})),
        ),
    ]
}

#[tokio::test]
async fn every_paginated_read_refuses_an_uninterpretable_token_with_one_wording() {
    let tokens = uninterpretable_tokens();

    for (operation, path, body_for) in paginated_reads() {
        let xray = seeded().await;
        for token in &tokens {
            let case = format!("{operation} with {token:?}");
            let refused = xray.call(path, &body_for(token)).await;
            assert_invalid_token(&case, token, &refused);
        }
    }
}

#[tokio::test]
async fn a_tag_listing_refuses_an_uninterpretable_token_the_same_way() {
    let xray = Xray::new();
    // The listing resolves its `ResourceARN` before it reads the token, so the
    // row needs a resource that exists. The name sorts BEFORE the reserved
    // `Default` group, so the group store's keyspace is unchanged for the rows
    // above.
    let created = xray
        .call(
            "/CreateGroup",
            &json!({"GroupName": "Alpha", "FilterExpression": "ok"}),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "creating the group to list tags of must succeed, got {created:?}"
    );
    let arn = created
        .get("Group")
        .get("GroupARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGroup must answer a GroupARN, got {created:?}"))
        .to_string();

    for token in uninterpretable_tokens() {
        let case = format!("ListTagsForResource with {token:?}");
        let refused = xray
            .call(
                "/ListTagsForResource",
                &json!({"ResourceARN": &arn, "NextToken": &token}),
            )
            .await;
        assert_invalid_token(&case, &token, &refused);
    }
}

// =============================================================================
// 3. The same wording for a token whose FORM is a cursor, but not one of this
//    query's
// =============================================================================

#[tokio::test]
async fn a_cursor_shaped_token_outside_the_query_is_worded_the_same_way() {
    let xray = seeded().await;

    // Rows: a number too large for the cursor's own type, a number that is not
    // whole, a trace id past every stored one, and the empty string — which is a
    // token no response ever hands out, and whose message is therefore the fixed
    // text and nothing more.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "GetServiceGraph resumed past the widest ReferenceId",
            "/ServiceGraph",
            json!({
                "StartTime": now() - 3_600.0,
                "EndTime": now(),
                "NextToken": "2147483648",
            }),
            "2147483648",
        ),
        (
            "GetTimeSeriesServiceStatistics resumed at a fractional second",
            "/TimeSeriesServiceStatistics",
            json!({"StartTime": now() - 3_600.0, "EndTime": now(), "NextToken": "12.5"}),
            "12.5",
        ),
        (
            "GetTraceSummaries resumed past every stored trace",
            "/TraceSummaries",
            json!({
                "StartTime": now() - 3_600.0,
                "EndTime": now(),
                "NextToken": trace_id(99),
            }),
            "1-00000063-000000000000000000000063",
        ),
        (
            "GetIndexingRules resumed from the empty string",
            "/GetIndexingRules",
            json!({"NextToken": ""}),
            "",
        ),
    ];

    for (case, path, body, token) in rows {
        let refused = xray.call(path, &body).await;
        assert_invalid_token(case, token, &refused);
    }
}

// =============================================================================
// 4. A token this service DID issue still pages
// =============================================================================

/// The `EndTime` every case below queries, and the instant the traces it seeds
/// are placed relative to: the harness's fixed instant plus ten minutes, so the
/// window a case asks about falls off the absolute hour grid as well as off the
/// clock.
///
/// Read from no clock, so one query's window is the same window on every one of
/// its pages — which is what a paginator sends, since a token is a cursor into
/// the query that issued it — and what the walk below yields is a function of
/// the window and the stored traces alone, never of how much time passed
/// between two calls.
const QUERY_END: f64 = ALIGNED + 600.0;

/// One query whose pages are walked with the tokens the service itself issued.
struct PagingCase {
    label: &'static str,
    /// How many traces each STEP of the window holds, counted back from the end
    /// of the window: `per_step[0]` is the step the window ends in, which is
    /// the first page. Its length is how many steps wide the window is.
    per_step: Vec<usize>,
}

/// The queries walked below. The rows differ along what the rule generalises
/// over: how many traces one step holds relative to the page the service serves
/// a step in ([`TRACE_SUMMARIES_PAGE_SIZE`]) — fewer, exactly a page, one more,
/// more than two pages — how many STEPS the window is wide, and therefore which
/// kind of token the walk resumes from: a cursor inside a step, a step before
/// the one just served, or both in one walk.
fn paging_cases() -> Vec<PagingCase> {
    vec![
        PagingCase {
            label: "one step holding fewer traces than a page",
            per_step: vec![TRACE_SUMMARIES_PAGE_SIZE - 1],
        },
        PagingCase {
            label: "one step holding exactly a page",
            per_step: vec![TRACE_SUMMARIES_PAGE_SIZE],
        },
        PagingCase {
            label: "one step holding one trace more than a page",
            per_step: vec![TRACE_SUMMARIES_PAGE_SIZE + 1],
        },
        PagingCase {
            label: "one step holding more than two pages",
            per_step: vec![TRACE_SUMMARIES_PAGE_SIZE * 2 + 1],
        },
        PagingCase {
            label: "three steps, one of them holding more than a page",
            per_step: vec![2, TRACE_SUMMARIES_PAGE_SIZE + 2, 1],
        },
        PagingCase {
            label: "three steps, the middle one holding nothing",
            per_step: vec![1, 0, TRACE_SUMMARIES_PAGE_SIZE],
        },
    ]
}

/// How many pages the walk takes: one per step of the window — the query pages
/// on through a step that held nothing — and as many as it takes for a step
/// holding more summaries than one page carries.
fn expected_pages(per_step: &[usize]) -> usize {
    per_step
        .iter()
        .map(|held| held.div_ceil(TRACE_SUMMARIES_PAGE_SIZE).max(1))
        .sum()
}

/// Store the traces `case` describes, and answer their ids in the order the
/// pages of its query must report them: step by step from the end of the window
/// backwards, and inside one step in trace-id order.
///
/// Ids are handed out in that same order, so the expectation is the seeding
/// order; each trace sits half a minute before the end of its own step, well
/// clear of both boundaries of it.
async fn seed_steps(xray: &Xray, case: &PagingCase) -> Vec<String> {
    let mut stored: Vec<String> = Vec::new();
    for (step, held) in case.per_step.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let start = QUERY_END - (step as f64) * HOUR - 30.0;
        for _ in 0..*held {
            let index = stored.len() + 1;
            let id = trace_id(u32::try_from(index).expect("the index fits"));
            let document =
                root_document(&format!("{index:016x}"), &id, "svc", start, Some(start + 0.5))
                    .to_string();
            let put = xray.put_segments(&[document]).await;
            assert_eq!(
                put.status,
                200,
                "case {:?}: seeding {id:?} at {start} must succeed, got {put:?}",
                case.label
            );
            stored.push(id);
        }
    }
    stored
}

#[tokio::test]
async fn a_token_this_service_issued_still_resumes_the_query() {
    for case in paging_cases() {
        let label = case.label;
        let xray = Xray::new();
        let stored = seed_steps(&xray, &case).await;
        #[allow(clippy::cast_precision_loss)]
        let start = QUERY_END - (case.per_step.len() as f64) * HOUR;

        // Every page of the walk asks about THAT window, with only the token
        // the previous page handed out changing, and the walk stops when the
        // service stops issuing one.
        let pages = xray
            .trace_summary_pages(&summaries_body(start, QUERY_END, None))
            .await;

        let seen = reported_trace_ids(&pages);
        assert_eq!(
            seen, stored,
            "case {label:?}: paging with the tokens this service issued must yield every trace \
             once, got {seen:?}"
        );
        let due = expected_pages(&case.per_step);
        assert_eq!(
            pages.len(),
            due,
            "case {label:?}: expected {due} pages, got {}",
            pages.len()
        );
        for (position, page) in pages.iter().enumerate() {
            let ids = reported(page);
            assert!(
                ids.len() <= TRACE_SUMMARIES_PAGE_SIZE,
                "case {label:?}: page {position} carries at most {TRACE_SUMMARIES_PAGE_SIZE} \
                 summaries, got {ids:?}"
            );
        }
    }
}
