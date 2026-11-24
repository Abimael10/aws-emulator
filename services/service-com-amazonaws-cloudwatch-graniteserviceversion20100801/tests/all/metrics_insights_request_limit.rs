// SPDX-License-Identifier: CC0-1.0

//! How many Metrics Insights queries one `GetMetricData` request may carry, and what it is told
//! about a window reaching past the two weeks such a query can read.
//!
//! Two rules, each asserted over a table whose rows differ along what the rule generalises over.
//!
//! 1. **A request carries at most ONE Metrics Insights query, beside any number of retrieving
//!    and metric-math ones.** "If you include a Metrics Insights query, each `GetMetricData`
//!    operation can include only one query. But the same `GetMetricData` operation can also
//!    retrieve other metrics." (`GetMetricData`) — so the limit is on the `SELECT` statements of
//!    a request, and a request carrying two of them is refused with the published limit, whatever
//!    else it carries. The rows differ along how many `SELECT`s the request carries and what
//!    sits beside them.
//! 2. **A window reaching further back than a Metrics Insights query can read is REPORTED, not
//!    refused**: "Metrics Insights queries can query the most recent two weeks of metric data."
//!    (`GetMetricData`) — the row is still answered and the answer carries the message about the
//!    operation itself ("A message appears here only if it is related to the global
//!    `GetMetricData` operation",
//!    `smithy_query(query="doc",shape="GetMetricDataOutput",member="Messages")`), while the same
//!    window asked for by a retrieving query alone carries no message at all: the window belongs
//!    to that query language. The rows differ along how far back the window reaches and along
//!    whether the request carries a `SELECT` at all.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, now_secs, ok};

/// The namespace this suite publishes and selects under.
const NAMESPACE: &str = "Custom/InsightsLimit";

/// The metric its queries select.
const METRIC: &str = "Load";

/// A day, in the epoch seconds a request's range is named in.
const DAY: f64 = 86_400.0;

/// The class a refusal of this read carries, as an awsJson body names its shape.
const VALIDATION: &str = "ValidationException";

/// The code the message about a window past the one a Metrics Insights query reads carries.
const MAX_WINDOW_CODE: &str = "MaxQueryTimeRangeExceeded";

/// What that message says.
const MAX_WINDOW_MESSAGE: &str = "Max time window exceeded for query";

/// A `SELECT` over the published series, grouped by `Host`.
fn select() -> String {
    format!("SELECT SUM({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Host) GROUP BY Host")
}

/// A request over `queries`, reading `[start, end)`.
fn read(queries: Value, start: f64, end: f64) -> Value {
    json!({
        "MetricDataQueries": queries,
        "StartTime": start,
        "EndTime": end,
    })
}

/// One retrieving query over the published series.
fn retrieving(id: &str, period: i64) -> Value {
    json!({
        "Id": id,
        "MetricStat": {
            "Metric": {
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Host", "Value": "h1"}],
            },
            "Period": period,
            "Stat": "Sum",
        },
    })
}

/// The `Id` of every result of one answer, in the order it listed them.
fn answered_ids(body: &Value) -> Vec<String> {
    body.get("MetricDataResults")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetMetricData returns MetricDataResults, got {body}"))
        .iter()
        .map(|result| {
            result
                .get("Id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The `(Code, Value)` of every message the ANSWER carries about the operation itself.
fn operation_messages(body: &Value) -> Vec<(String, String)> {
    body.get("Messages")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetMetricData returns Messages, got {body}"))
        .iter()
        .map(|message| {
            (
                message
                    .get("Code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                message
                    .get("Value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Publish one datapoint of the `Host=h1` series inside the minute at `at`.
async fn publish(router: &::smithy_server_runtime::MultiServiceRouter, at: f64) {
    ok(
        router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Host", "Value": "h1"}],
                "Value": 12.0,
                "Timestamp": at,
            }],
        }),
    )
    .await;
}

#[tokio::test]
async fn a_request_carries_one_select_beside_any_number_of_other_queries() {
    let router = build_router();
    let window = (now_secs() / 60.0).floor() * 60.0 - 5.0 * 60.0;
    publish(&router, window + 30.0).await;

    // (case, the queries, the ids the answer reports): one `SELECT` is answered beside whatever
    // else the request carries, and its own result comes back under its query's `Id`.
    let answered: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "one SELECT alone",
            json!([{"Id": "q", "Expression": select(), "Period": 60}]),
            vec!["q"],
        ),
        (
            "one SELECT beside a retrieving query",
            json!([
                {"Id": "q", "Expression": select(), "Period": 60},
                retrieving("m1", 60),
            ]),
            vec!["q", "m1"],
        ),
        (
            "one SELECT beside a retrieving query and a metric-math one",
            json!([
                {"Id": "q", "Expression": select(), "Period": 60},
                retrieving("m1", 60),
                {"Id": "e1", "Expression": "ABS(m1)", "Period": 60},
            ]),
            vec!["q", "m1", "e1"],
        ),
    ];
    for (case, queries, ids) in answered {
        let body = ok(
            &router,
            "GetMetricData",
            &read(queries, window, window + 60.0),
        )
        .await;
        let reported = answered_ids(&body);
        assert_eq!(
            reported, ids,
            "case {case:?}: expected a result per query under {ids:?}, got {body}"
        );
    }

    // (case, the queries): two `SELECT`s in one request are refused with the published limit,
    // whatever else the request carries and wherever the second one sits.
    let refused: Vec<(&str, Value)> = vec![
        (
            "two SELECTs",
            json!([
                {"Id": "q1", "Expression": select(), "Period": 60},
                {"Id": "q2", "Expression": select(), "Period": 60},
            ]),
        ),
        (
            "two SELECTs with a retrieving query between them",
            json!([
                {"Id": "q1", "Expression": select(), "Period": 60},
                retrieving("m1", 60),
                {"Id": "q2", "Expression": select(), "Period": 60},
            ]),
        ),
        (
            "three SELECTs",
            json!([
                {"Id": "q1", "Expression": select(), "Period": 60},
                {"Id": "q2", "Expression": select(), "Period": 60},
                {"Id": "q3", "Expression": select(), "Period": 60},
            ]),
        ),
    ];
    for (case, queries) in refused {
        let (status, body) = call(
            &router,
            "GetMetricData",
            &read(queries, window, window + 60.0),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, VALIDATION),
            "case {case:?}: expected HTTP 400 {VALIDATION}, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            "Maximum number of queries (1) exceeded",
            "case {case:?}: expected the published limit, got {body}"
        );
    }
}

#[tokio::test]
async fn a_window_past_the_one_a_select_reads_is_reported_and_still_answered() {
    let router = build_router();
    let now = (now_secs() / 60.0).floor() * 60.0;
    publish(&router, now - 5.0 * 60.0).await;

    // (case, how far back the window begins, whether the answer reports the window): the window a
    // Metrics Insights query reads is the most recent two weeks, so a range beginning well
    // inside it is reported on at all, and one beginning well outside it is. Both ends are left
    // a day of room, because where the window begins moves with the clock.
    let rows: Vec<(&str, f64, bool)> = vec![
        ("a range of one hour", 3_600.0, false),
        ("a range of thirteen days", 13.0 * DAY, false),
        ("a range of fifteen days", 15.0 * DAY, true),
        ("a range of twenty days", 20.0 * DAY, true),
        ("a range of a hundred days", 100.0 * DAY, true),
    ];
    for (case, back, reported) in rows {
        let start = now - back;
        let selected = ok(
            &router,
            "GetMetricData",
            &read(
                json!([{"Id": "q", "Expression": select(), "Period": 3_600}]),
                start,
                now,
            ),
        )
        .await;
        let expected = if reported {
            vec![(MAX_WINDOW_CODE.to_string(), MAX_WINDOW_MESSAGE.to_string())]
        } else {
            Vec::new()
        };
        assert_eq!(
            operation_messages(&selected),
            expected,
            "case {case:?}: expected the operation messages {expected:?}, got {selected}"
        );
        // The row is answered either way: the message is about the window, not a refusal.
        assert_eq!(
            answered_ids(&selected),
            vec!["q".to_string()],
            "case {case:?}: the query is still answered, got {selected}"
        );

        // The same window asked for by a retrieving query alone carries no message: the window
        // belongs to the query language, not to the range.
        let retrieved = ok(
            &router,
            "GetMetricData",
            &read(json!([retrieving("m1", 3_600)]), start, now),
        )
        .await;
        assert_eq!(
            operation_messages(&retrieved),
            Vec::new(),
            "case {case:?}: a retrieving query reports nothing about the operation, got \
             {retrieved}"
        );
    }
}
