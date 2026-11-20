// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `DescribeQueries` reports about a started query, and in which
//! order it reports them.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` — so a response that is not computed from the
//! shared store cannot pass.
//!
//! The rules:
//!
//! * every reported `QueryInfo` carries `bytesScanned`, `queryDuration` and `userIdentity`
//!   beside the members that identify it, because each is a fact about the query's own
//!   execution and the identity that asked for it;
//! * the `queryString` it reports is the EFFECTIVE query: CloudWatch Logs Insights names the
//!   log groups to read, the time range and the row limit inside a Logs Insights QL query, so
//!   a start that names them as parameters instead is stored as the `SOURCE` and `limit`
//!   commands those parameters stand for — and a query that names its own sources, or is
//!   written in one of the OpenSearch languages, is stored as submitted;
//! * the enumeration is newest-created-first, and a `nextToken` is a position in THAT order,
//!   so a paginating client walks from the most recent query to the oldest and sees each one
//!   exactly once.
//!
//! The tables span what each rule generalises over: which member names the log groups, how
//! many it names, whether the query string selects its own, which query language it is written
//! in, whether the start names a row limit, and how large a page is.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::QUERY_STATUS_COMPLETE;

use crate::harness::{base_ms, batch, group_with_stream, now_ms, Logs};

/// The operations every case calls.
const START: &str = "StartQuery";
const DESCRIBE: &str = "DescribeQueries";
const RESULTS: &str = "GetQueryResults";

/// The log groups the cases query.
const GROUP: &str = "/projection/orders";
const OTHER_GROUP: &str = "/projection/payments";

/// How long a case waits, in all, for a query to reach the status the clock owes it: a
/// deadline generous enough for a loaded host, reached by looking at the reported query again
/// until it holds rather than by sleeping for a span the test guessed.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long such a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Both log groups, the second holding one event, and the window every query below is started
/// with — in the unit `StartQuery` documents for its range ("epoch time, the number of seconds
/// since January 1, 1970").
async fn account_with_groups(logs: &Logs) -> (i64, i64) {
    group_with_stream(logs, GROUP, "stream-1", None).await;
    logs.ok("CreateLogGroup", &json!({"logGroupName": OTHER_GROUP}))
        .await;
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": "stream-1",
            "logEvents": batch(&[(base_ms(), "ERROR order 1 failed")]),
        }),
    )
    .await;
    let now_secs = now_ms() / 1_000 + 1;
    (now_secs - 2 * 60 * 60, now_secs)
}

/// Start `request` and answer the `queryId` it minted.
async fn started(logs: &Logs, case: &str, request: &Value) -> String {
    let answer = logs.ok(START, request).await;
    answer["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartQuery answers a queryId, got {answer}"))
        .to_string()
}

/// The one query `DescribeQueries` reports under `id`, over the whole account.
async fn reported(logs: &Logs, case: &str, id: &str) -> Value {
    let page = logs.ok(DESCRIBE, &json!({})).await;
    page["queries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {DESCRIBE} reports a queries list, got {page}"))
        .iter()
        .find(|query| query["queryId"].as_str() == Some(id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the started query must be reported, got {page}"))
}

/// The query `DescribeQueries` reports under `id` once it has reached a terminal status,
/// waiting for the CONDITION rather than for a span the test guessed.
async fn completed(logs: &Logs, case: &str, id: &str) -> Value {
    let deadline = std::time::Instant::now() + STATUS_DEADLINE;
    loop {
        let query = reported(logs, case, id).await;
        if query["status"].as_str() == Some(QUERY_STATUS_COMPLETE) {
            return query;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the query to reach {QUERY_STATUS_COMPLETE}, got {query}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `SOURCE`-prefixed query a start that named its log groups and window as PARAMETERS
/// stores: the command those parameters stand for, then the submitted pipeline.
fn source_prefixed(groups: &[&str], window: (i64, i64), submitted: &str) -> String {
    let sources: Vec<String> = groups.iter().map(|name| format!("\"{name}\"")).collect();
    format!(
        "SOURCE {} START={} END={} | {submitted}",
        sources.join(", "),
        window.0,
        window.1
    )
}

/// One row of the effective-query table: what the start names besides the window, the query
/// string it submits, and the query string the service must then report.
struct EffectiveCase {
    /// What this row varies, for the assertion messages.
    case: &'static str,
    /// The members the start names beside `startTime`, `endTime` and `queryString`.
    extra: Value,
    /// The submitted `queryString`.
    submitted: String,
    /// The log groups the effective query's `SOURCE` command names, or empty where the stored
    /// query is the submitted text itself.
    sources: Vec<String>,
}

/// Every member of a reported `QueryInfo` that states a fact about the query's EXECUTION or
/// the identity behind it is present, and the `queryString` it reports is the effective query
/// the start's parameters stand for.
#[tokio::test]
async fn every_reported_query_carries_its_execution_and_identity_members() {
    let logs = Logs::new();
    let window = account_with_groups(&logs).await;
    let (_, group_arn) = logs.group_arns(GROUP).await;
    let (_, other_arn) = logs.group_arns(OTHER_GROUP).await;
    let pipeline = "fields @timestamp, @message | sort @timestamp desc";

    let cases: Vec<EffectiveCase> = vec![
        EffectiveCase {
            case: "one log group named by logGroupName",
            extra: json!({"logGroupName": GROUP}),
            submitted: pipeline.to_string(),
            sources: vec![GROUP.to_string()],
        },
        EffectiveCase {
            case: "one log group and the row limit the start names",
            extra: json!({"logGroupName": GROUP, "limit": 20}),
            submitted: format!("{pipeline} | limit 20"),
            sources: vec![GROUP.to_string()],
        },
        EffectiveCase {
            case: "one log group named by logGroupNames",
            extra: json!({"logGroupNames": [GROUP]}),
            submitted: pipeline.to_string(),
            sources: vec![GROUP.to_string()],
        },
        EffectiveCase {
            case: "two log groups named by logGroupIdentifiers, as ARNs",
            extra: json!({"logGroupIdentifiers": [group_arn, other_arn]}),
            submitted: pipeline.to_string(),
            sources: vec![GROUP.to_string(), OTHER_GROUP.to_string()],
        },
        EffectiveCase {
            case: "a query string whose own SOURCE command selects the log groups",
            extra: json!({}),
            submitted: format!("SOURCE \"{GROUP}\" | {pipeline}"),
            sources: Vec::new(),
        },
        EffectiveCase {
            case: "a PPL pipeline, whose language has source and limit syntax of its own",
            extra: json!({"queryLanguage": "PPL", "limit": 5}),
            submitted: format!("search source = {GROUP} | fields @message"),
            sources: Vec::new(),
        },
    ];

    for row in &cases {
        let mut request = json!({
            "startTime": window.0,
            "endTime": window.1,
            "queryString": row.submitted,
        });
        for (member, value) in row
            .extra
            .as_object()
            .expect("a row's extra members are an object")
        {
            request[member] = value.clone();
        }
        let id = started(&logs, row.case, &request).await;
        let query = reported(&logs, row.case, &id).await;

        // The row limit the start named is part of the effective query for the one language
        // that carries it as a command.
        let limit = request
            .get("limit")
            .and_then(Value::as_i64)
            .filter(|_| request.get("queryLanguage").is_none());
        let expected = match row.sources.as_slice() {
            [] => row.submitted.clone(),
            sources => source_prefixed(
                &sources.iter().map(String::as_str).collect::<Vec<_>>(),
                window,
                &row.submitted,
            ),
        };
        let expected = match limit {
            Some(limit) => format!("{expected} | limit {limit}"),
            None => expected,
        };
        assert_eq!(
            query["queryString"].as_str(),
            Some(expected.as_str()),
            "case {:?}: expected the effective query {expected:?}, got {query}",
            row.case
        );

        // "The total number of bytes scanned by the query": a double, present whether or not
        // the run has scanned anything yet.
        let scanned = query["bytesScanned"].as_f64();
        assert!(
            scanned.is_some_and(|bytes| bytes >= 0.0),
            "case {:?}: expected a bytesScanned of at least zero, got {query}",
            row.case
        );
        // "The duration in milliseconds that the query took to execute": a whole number of
        // milliseconds, which for a query still running is the zero span it has taken so far.
        let duration = query["queryDuration"].as_i64();
        assert!(
            duration.is_some_and(|millis| millis >= 0),
            "case {:?}: expected a queryDuration of at least zero, got {query}",
            row.case
        );
        // "The ARN of the user who ran the query": the principal the start was signed for,
        // which for these requests is the account's own published identity.
        assert_eq!(
            query["userIdentity"].as_str(),
            Some(service_common::caller_principal::ambient_user_arn().as_str()),
            "case {:?}: expected the calling principal's ARN, got {query}",
            row.case
        );
    }
}

/// Once a query has run, the figures it reports are that RUN's: the duration is the span the
/// execution took and the scanned bytes are the ones its own statistics report — zero for a
/// query that scanned nothing.
#[tokio::test]
async fn a_completed_query_reports_the_figures_of_its_own_run() {
    let logs = Logs::new();
    let window = account_with_groups(&logs).await;

    // (case, the log group queried, whether that group holds an event the query scans)
    for (case, group, holds_events) in [
        ("a query of a group holding one event", GROUP, true),
        ("a query of a group holding none", OTHER_GROUP, false),
    ] {
        let id = started(
            &logs,
            case,
            &json!({
                "logGroupName": group,
                "startTime": window.0,
                "endTime": window.1,
                "queryString": "fields @timestamp, @message",
            }),
        )
        .await;
        // The query's own results, read once the clock has completed it: this is what FIXES
        // the figures its run produced, so the report below is compared against a run that
        // has landed rather than against one still in flight.
        completed(&logs, case, &id).await;
        let results = logs.ok(RESULTS, &json!({"queryId": &id})).await;
        let statistics = results["statistics"]["bytesScanned"].as_f64();
        let query = reported(&logs, case, &id).await;

        // A completed query took a span of the clock to execute: the one between the instant
        // it was created and the instant it finished.
        let duration = query["queryDuration"].as_i64().unwrap_or_default();
        assert!(
            duration > 0,
            "case {case:?}: a completed query's execution took a span of the clock, got {query}"
        );

        // The bytes it reports are the ones its OWN results report, so the two readers of one
        // run cannot disagree — and a run that scanned nothing reports zero.
        assert_eq!(
            query["bytesScanned"].as_f64(),
            statistics,
            "case {case:?}: expected the scanned bytes of the run, got {query} beside {results}"
        );
        assert_eq!(
            statistics.unwrap_or_default() > 0.0,
            holds_events,
            "case {case:?}: expected scanned bytes exactly where the group holds events, got \
             {results}"
        );
    }
}

/// The enumeration is newest-created-first, and a page of `maxResults` resumed by `nextToken`
/// walks it from the most recent query to the oldest, reporting each one exactly once.
#[tokio::test]
async fn pages_walk_the_queries_newest_created_first() {
    let logs = Logs::new();
    let window = account_with_groups(&logs).await;

    // Four queries, started one after another, so the creation order is known.
    let mut minted: Vec<String> = Vec::new();
    for index in 1..=4 {
        minted.push(
            started(
                &logs,
                "four starts",
                &json!({
                    "logGroupName": GROUP,
                    "startTime": window.0,
                    "endTime": window.1,
                    "queryString": format!("fields @message | limit {index}"),
                }),
            )
            .await,
        );
    }
    let newest_first: Vec<String> = minted.iter().rev().cloned().collect();

    // A read that names no page size reports the whole enumeration in that order.
    let page = logs.ok(DESCRIBE, &json!({})).await;
    let ids: Vec<String> = query_ids(&page, "no maxResults");
    assert_eq!(
        ids, newest_first,
        "case \"no maxResults\": expected the queries newest first, got {ids:?}"
    );

    // One row per page size: under, at and over the number of queries held.
    for page_size in [1usize, 2, 3, 4, 5] {
        let case = format!("maxResults {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut request = json!({"maxResults": page_size});
            if let Some(token) = &token {
                request["nextToken"] = json!(token);
            }
            let page = logs.ok(DESCRIBE, &request).await;
            let ids = query_ids(&page, &case);
            assert!(
                ids.len() <= page_size,
                "case {case:?}: a page holds at most maxResults queries, got {ids:?}"
            );
            walked.extend(ids);
            pages += 1;
            assert!(
                pages <= minted.len() + 1,
                "case {case:?}: the walk terminates, got {pages} pages"
            );
            // "The token for the next set of items to return": one comes back exactly while
            // queries remain beyond the page just reported.
            let next = page.get("nextToken").and_then(Value::as_str);
            assert_eq!(
                next.is_some(),
                walked.len() < minted.len(),
                "case {case:?}: expected a token exactly while queries remain, got {page}"
            );
            match next {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, newest_first,
            "case {case:?}: expected every query once, newest first, got {walked:?}"
        );
    }
}

/// The `queryId`s one `DescribeQueries` page reports, in the order it reported them.
fn query_ids(page: &Value, case: &str) -> Vec<String> {
    page["queries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {DESCRIBE} reports a queries list, got {page}"))
        .iter()
        .map(|query| {
            query["queryId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a reported query carries an id, got {query}"))
                .to_string()
        })
        .collect()
}
