// SPDX-License-Identifier: CC0-1.0

//! What `GetMetricData`'s `MaxDatapoints` budgets, and what a page of it reports.
//!
//! The rule under test: **the budget is spent on the `Period` SLOTS of the requested range —
//! every slot of it, whether or not a datapoint was published in that slot — and one budget is
//! spent across the request's queries in order**, so a page covers the next `MaxDatapoints`
//! slots, a result reports its OWN query's progress through the range (`PartialData` while it has
//! range left, `Complete` on the page that reaches the end of it, absent from the pages after
//! that), and the pages reassemble exactly the series the same request answers unpaginated.
//!
//! "The maximum number of data points the request should return before paginating."
//! (`smithy_query(query="doc",shape="GetMetricDataInput",member="MaxDatapoints")`) — the
//! datapoints a query asks for are one per `Period` of the range it names, which is also how the
//! operation counts what a request HAS REQUESTED ("as many as 100,800 data points",
//! `GetMetricData`); "`PartialData` means that an incomplete set of data points were returned.
//! You can use the `NextToken` value that was returned and repeat your request to get more data
//! points." (`MetricDataResult$StatusCode`).
//!
//! A `NextToken` is a POSITION in that scan: replayed against the same request it answers the
//! same page, replayed with a larger budget it answers a longer page from the same position, and
//! replayed against any other request it is refused.
//!
//! The rows differ along what the rule generalises over: the granularity, how many slots the
//! range holds, the budget, the scan order, which slots hold a datapoint, and how many queries
//! the request carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, call, error_code, now_secs, ok};

/// The namespace every series of this suite is published under.
const NAMESPACE: &str = "Custom/SlotBudget";

/// How many pages one row may read before the suite calls paging non-terminating.
const MAX_PAGES: usize = 24;

/// One `MetricStat` query over the `Service=api` series of `metric`, folded at `period`.
fn query(id: &str, metric: &str, period: i64, stat: &str) -> Value {
    json!({
        "Id": id,
        "MetricStat": {
            "Metric": {
                "Namespace": NAMESPACE,
                "MetricName": metric,
                "Dimensions": [{"Name": "Service", "Value": "api"}],
            },
            "Period": period,
            "Stat": stat,
        },
    })
}

/// Publish one datum of `metric` into the slot `slot` of the grid of `period`-wide slots
/// beginning at `start`, stamped inside that slot.
async fn publish(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    period: f64,
    slot: f64,
    value: f64,
) {
    ok(
        router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": metric,
                "Dimensions": [{"Name": "Service", "Value": "api"}],
                "Value": value,
                "Unit": "Count",
                "StorageResolution": 1,
                "Timestamp": start + slot * period + period / 2.0,
            }],
        }),
    )
    .await;
}

/// The `(Id, StatusCode, values)` of every result of one answer, in the order it listed them.
fn answered(body: &Value) -> Vec<(String, String, Vec<f64>)> {
    body.get("MetricDataResults")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetMetricData returns MetricDataResults, got {body}"))
        .iter()
        .map(|result| {
            (
                result
                    .get("Id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                result
                    .get("StatusCode")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                result
                    .get("Values")
                    .and_then(Value::as_array)
                    .map(|listed| listed.iter().filter_map(Value::as_f64).collect())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Read `request` to exhaustion, following its `NextToken`, and return one entry per page.
async fn paged(
    router: &MultiServiceRouter,
    request: &Value,
) -> Vec<Vec<(String, String, Vec<f64>)>> {
    let mut pages = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut page = request.clone();
        if let Some(carried) = token.as_ref() {
            page["NextToken"] = json!(carried);
        }
        let body = ok(router, "GetMetricData", &page).await;
        pages.push(answered(&body));
        token = body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if token.is_none() {
            return pages;
        }
    }
    panic!("paging must terminate inside {MAX_PAGES} pages, got {pages:?}");
}

/// The instant the rows' ranges end at: a whole minute, so the rounding
/// `GetMetricDataInput$StartTime` documents leaves a range exactly where a row puts it.
fn window_end() -> f64 {
    (now_secs() / 60.0).floor() * 60.0
}

/// One row of the single-query table: the case, the granularity, how many slots the range holds,
/// the budget, the scan order, and the slots a datapoint is published in.
type BudgetCase = (&'static str, f64, u64, u64, bool, &'static [u64]);

#[tokio::test]
async fn the_budget_is_spent_on_the_period_slots_of_the_requested_range() {
    let router = build_router();
    let end = window_end();

    // (case, period, slots in the range, MaxDatapoints, descending, the slots holding a datum)
    let rows: Vec<BudgetCase> = vec![
        (
            // The recorded shape: eleven minute slots holding six datapoints, read two slots at
            // a time, newest first — six pages, two of which carry no value at all.
            "eleven minute slots, two per page, newest first",
            60.0,
            11,
            2,
            true,
            &[5, 6, 7, 8, 9, 10],
        ),
        (
            "the same range and budget read oldest first",
            60.0,
            11,
            2,
            false,
            &[5, 6, 7, 8, 9, 10],
        ),
        (
            "one slot per page",
            60.0,
            4,
            1,
            true,
            &[0, 3],
        ),
        (
            // No datapoint anywhere in the range: the slots are what the budget buys, so the
            // range still takes as many pages as it has slots.
            "a range holding no datapoint at all",
            60.0,
            3,
            1,
            true,
            &[],
        ),
        (
            "a budget wider than the whole range",
            60.0,
            3,
            10,
            true,
            &[1],
        ),
        (
            "five-minute slots",
            300.0,
            4,
            1,
            true,
            &[1, 2],
        ),
        (
            "half-minute slots, three per page",
            30.0,
            6,
            3,
            false,
            &[0, 5],
        ),
    ];

    for (index, (case, period, slots, budget, descending, filled)) in rows.into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let span = slots as f64 * period;
        let start = end - span;
        let metric = format!("Slots{index}");
        for slot in filled {
            #[allow(clippy::cast_precision_loss)]
            let at = *slot as f64;
            publish(&router, &metric, start, period, at, at + 1.0).await;
        }
        let order = if descending {
            "TimestampDescending"
        } else {
            "TimestampAscending"
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let request = json!({
            "MetricDataQueries": [query("m1", &metric, period as i64, "Maximum")],
            "StartTime": start,
            "EndTime": end,
            "ScanBy": order,
        });
        // What the request answers with no budget at all, which the pages have to reassemble.
        let whole = ok(&router, "GetMetricData", &request).await;
        let unpaginated = answered(&whole);
        assert_eq!(
            unpaginated.len(),
            1,
            "case {case:?}: one query answers one result, got {whole}"
        );

        let mut budgeted = request.clone();
        budgeted["MaxDatapoints"] = json!(budget);
        let pages = paged(&router, &budgeted).await;

        // One page per `MaxDatapoints` slots of the range, the last one carrying what is left.
        let expected_pages = usize::try_from(slots.div_ceil(budget)).unwrap_or(usize::MAX);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: {slots} slots at a budget of {budget} take {expected_pages} pages, \
             got {pages:?}"
        );
        let reassembled: Vec<f64> = pages
            .iter()
            .flat_map(|page| page.iter().flat_map(|(_, _, values)| values.clone()))
            .collect();
        assert_eq!(
            reassembled, unpaginated[0].2,
            "case {case:?}: the pages reassemble the unpaginated series, got {pages:?}"
        );
        let statuses: Vec<&str> = pages
            .iter()
            .map(|page| {
                page.first()
                    .map(|(_, status, _)| status.as_str())
                    .unwrap_or_else(|| panic!("case {case:?}: every page answers the query, got {pages:?}"))
            })
            .collect();
        let (last, earlier) = statuses
            .split_last()
            .unwrap_or_else(|| panic!("case {case:?}: expected at least one page"));
        assert_eq!(
            (*last, earlier.iter().all(|status| *status == "PartialData")),
            ("Complete", true),
            "case {case:?}: partial until the page that reaches the end of the range, got \
             {statuses:?}"
        );
    }
}

#[tokio::test]
async fn one_budget_is_spent_across_the_queries_of_a_request_in_order() {
    let router = build_router();
    let end = window_end();
    // A range of twenty minute slots, which is four five-minute slots for the second query: the
    // two queries ask for different numbers of slots out of one range, so the budget reaches
    // them on different pages.
    let span = 20.0 * 60.0;
    let start = end - span;
    for slot in [0.0, 19.0] {
        publish(&router, "Across", start, 60.0, slot, slot + 1.0).await;
    }

    let queries = json!([
        query("a", "Across", 60, "Maximum"),
        query("b", "Across", 300, "Maximum"),
    ]);
    let request = json!({
        "MetricDataQueries": queries,
        "StartTime": start,
        "EndTime": end,
        "ScanBy": "TimestampAscending",
    });
    let whole = answered(&ok(&router, "GetMetricData", &request).await);

    let mut budgeted = request.clone();
    // Ten of the twenty slots the first query asks for: the first two pages are spent on it, so
    // the second query is only reached once the first has none left.
    budgeted["MaxDatapoints"] = json!(10);
    let pages = paged(&router, &budgeted).await;

    for (id, expected) in whole.iter().map(|(id, _, values)| (id, values)) {
        let seen: Vec<(usize, &String, &Vec<f64>)> = pages
            .iter()
            .enumerate()
            .filter_map(|(index, results)| {
                results
                    .iter()
                    .find(|(answered_id, _, _)| answered_id == id)
                    .map(|(_, status, values)| (index, status, values))
            })
            .collect();
        let reassembled: Vec<f64> = seen
            .iter()
            .flat_map(|(_, _, values)| values.iter().copied())
            .collect();
        assert_eq!(
            &reassembled, expected,
            "case query {id:?}: the pages reassemble its unpaginated series, got {seen:?}"
        );
        let statuses: Vec<&str> = seen.iter().map(|(_, status, _)| status.as_str()).collect();
        let (last, earlier) = statuses
            .split_last()
            .unwrap_or_else(|| panic!("case query {id:?}: expected it on at least one page"));
        assert_eq!(
            (*last, earlier.iter().all(|status| *status == "PartialData")),
            ("Complete", true),
            "case query {id:?}: partial until the page that reaches the end of its range, got \
             {statuses:?}"
        );
    }
    // The query the budget finished first is NOT on the pages after it: the others keep paging.
    let last_page: Vec<&String> = pages
        .last()
        .unwrap_or_else(|| panic!("expected at least one page"))
        .iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(
        last_page,
        vec![&"b".to_string()],
        "the query that reached the end of its range on an earlier page is absent from the \
         later ones, got {pages:?}"
    );
}

#[tokio::test]
async fn a_budget_above_the_datapoints_one_page_answers_with_is_capped_at_them() {
    let router = build_router();
    let end = window_end();
    // "a total of as many as 100,800 data points" (`GetMetricData`) — the datapoints of a
    // request's range one page answers with, which is also the default `MaxDatapoints` takes.
    const PAGE: u64 = 100_800;
    // Two minute slots more than one page carries, so a budget above the ceiling still leaves
    // the range to a second page: the range is wider than a page, not the budget's to widen.
    let slots = PAGE + 2;
    #[allow(clippy::cast_precision_loss)]
    let start = end - slots as f64 * 60.0;

    // The rows differ along how far above the ceiling the budget reaches: just past it, the
    // widest value the member's own type carries, and the ceiling itself as the control.
    for (case, budget) in [
        ("the ceiling itself", PAGE),
        ("one datapoint past it", PAGE + 1),
        ("the widest budget the member can carry", u64::from(i32::MAX.unsigned_abs())),
    ] {
        let request = json!({
            "MetricDataQueries": [query("m1", "Capped", 60, "Maximum")],
            "StartTime": start,
            "EndTime": end,
            "ScanBy": "TimestampAscending",
            "MaxDatapoints": budget,
        });
        let first = ok(&router, "GetMetricData", &request).await;
        let statuses: Vec<String> = answered(&first)
            .into_iter()
            .map(|(_, status, _)| status)
            .collect();
        assert_eq!(
            statuses,
            vec!["PartialData".to_string()],
            "case {case:?}: a page covers at most the datapoints one page answers with, so the \
             two slots past it are left for another page, got {first}"
        );
        let token = first
            .get("NextToken")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the slots past one page are left for another, got {first}")
            })
            .to_string();
        let mut last = request.clone();
        last["NextToken"] = json!(token);
        let body = ok(&router, "GetMetricData", &last).await;
        let statuses: Vec<String> = answered(&body)
            .into_iter()
            .map(|(_, status, _)| status)
            .collect();
        assert_eq!(
            (statuses, body.get("NextToken")),
            (vec!["Complete".to_string()], None),
            "case {case:?}: the second page answers the rest of the range, got {body}"
        );
    }
}

#[tokio::test]
async fn a_token_is_a_position_in_the_request_it_was_minted_for() {
    let router = build_router();
    let end = window_end();
    let start = end - 6.0 * 60.0;
    for slot in 0..6 {
        #[allow(clippy::cast_precision_loss)]
        let at = f64::from(slot);
        publish(&router, "Token", start, 60.0, at, at + 1.0).await;
    }

    let request = json!({
        "MetricDataQueries": [query("m1", "Token", 60, "Maximum")],
        "StartTime": start,
        "EndTime": end,
        "ScanBy": "TimestampAscending",
        "MaxDatapoints": 2,
    });
    let first = ok(&router, "GetMetricData", &request).await;
    assert_eq!(
        answered(&first)
            .first()
            .map(|(_, _, values)| values.clone())
            .unwrap_or_default(),
        vec![1.0, 2.0],
        "the first page answers the budget's first two slots, got {first}"
    );
    let token = first
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a page with range left mints a token, got {first}"))
        .to_string();

    // The same position read with different budgets: the token says WHERE to resume, and the
    // budget says how much of the range the page answers from there. The rows differ along the
    // budget the continuation is read with — including the one that minted the token.
    for (case, budget, expected) in [
        ("the budget that minted it", 2, vec![3.0, 4.0]),
        ("a larger budget", 3, vec![3.0, 4.0, 5.0]),
        ("a budget past the end of the range", 9, vec![3.0, 4.0, 5.0, 6.0]),
        ("the smallest budget there is", 1, vec![3.0]),
    ] {
        let mut page = request.clone();
        page["MaxDatapoints"] = json!(budget);
        page["NextToken"] = json!(token.clone());
        let body = ok(&router, "GetMetricData", &page).await;
        let values = answered(&body)
            .first()
            .map(|(_, _, values)| values.clone())
            .unwrap_or_default();
        assert_eq!(
            values, expected,
            "case {case:?}: expected {expected:?} from the token's position, got {body}"
        );
    }

    // Replayed against a request that asks for something else, the token is refused rather than
    // silently resuming somewhere it does not belong. The rows differ along WHAT the
    // continuation changed.
    for (case, patch) in [
        ("another range", json!({"StartTime": start - 60.0})),
        (
            "another statistic",
            json!({"MetricDataQueries": [query("m1", "Token", 60, "Minimum")]}),
        ),
        (
            "another query beside it",
            json!({"MetricDataQueries": [
                query("m1", "Token", 60, "Maximum"),
                query("m2", "Token", 60, "Minimum"),
            ]}),
        ),
        (
            "another account named on the query",
            json!({"MetricDataQueries": [{
                "Id": "m1",
                "AccountId": "111122223333",
                "MetricStat": {
                    "Metric": {
                        "Namespace": NAMESPACE,
                        "MetricName": "Token",
                        "Dimensions": [{"Name": "Service", "Value": "api"}],
                    },
                    "Period": 60,
                    "Stat": "Maximum",
                },
            }]}),
        ),
    ] {
        let mut replayed = crate::harness::merged(request.clone(), &patch);
        replayed["NextToken"] = json!(token.clone());
        let (status, body) = call(&router, "GetMetricData", &replayed).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidNextToken"),
            "case {case:?}: a token of another request is refused, got {status} with {body}"
        );
    }
}
