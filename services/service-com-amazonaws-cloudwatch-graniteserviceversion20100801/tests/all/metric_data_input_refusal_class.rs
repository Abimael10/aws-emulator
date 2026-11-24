// SPDX-License-Identifier: CC0-1.0

//! How `GetMetricData` refuses a request it cannot read, and what it accepts beside each refusal.
//!
//! The rule under test: **this read declares none of this service's parameter faults, so every
//! refusal about its INPUT is the request-validation layer's — HTTP 400 under that layer's class
//! (`ValidationError` on the wire a CloudWatch client branches on, the shape
//! `ValidationException` an awsJson body names) — carrying AWS's fixed sentence for that fault
//! with the offending member's own request path interpolated and the terminator every sentence
//! that layer renders ends with**.
//!
//! `smithy_query(query="operation",operation="GetMetricData")` lists `InvalidNextToken` as the
//! operation's only error, so no parameter fault of this service is a code a client of this read
//! can branch on; the member paths are the ones the query protocol spells
//! (`MetricDataQueries.member.<position>.<member>`), taken from the request.
//!
//! The rows differ along what the rule generalises over: WHICH fault the request carries, which
//! member it is about, and where in the request's array that member sits. Beside each refusal is
//! the neighbouring value the same member ACCEPTS, so the table is the rule and not a blanket
//! rejection.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, now_secs, ok};

/// The namespace every row reads under; nothing is published into it, so no row is answered or
/// refused for the data it holds.
const NAMESPACE: &str = "Custom/InputRefusals";

/// The class every refusal of this read carries, as an awsJson body names its shape.
const VALIDATION: &str = "ValidationException";

/// The grammar a query's `Id` must match, as the refusal publishes it: "The valid characters are
/// letters, numbers, and underscore. The first character must be a lowercase letter."
/// (`smithy_query(query="doc",shape="MetricDataQuery",member="Id")`)
const ID_PATTERN: &str = "^[a-z][a-zA-Z0-9_]*$";

/// One `MetricStat` query, with `patch` merged over it.
fn query(id: &str, patch: &Value) -> Value {
    crate::harness::merged(
        json!({
            "Id": id,
            "MetricStat": {
                "Metric": {"Namespace": NAMESPACE, "MetricName": "Latency"},
                "Period": 60,
                "Stat": "Sum",
            },
        }),
        patch,
    )
}

/// A one-minute window a row reads, aligned so the rounding `GetMetricDataInput$StartTime`
/// documents leaves it where the row put it.
fn window() -> f64 {
    (now_secs() / 60.0).floor() * 60.0 - 5.0 * 60.0
}

/// A request over `queries` for that window, with `patch` merged over it.
fn request(queries: Value, patch: &Value) -> Value {
    let start = window();
    crate::harness::merged(
        json!({
            "MetricDataQueries": queries,
            "StartTime": start,
            "EndTime": start + 60.0,
        }),
        patch,
    )
}

/// A `MetricStat` whose `Period` is `period`, as a row patches one in.
fn with_period(period: i64) -> Value {
    json!({"MetricStat": {
        "Metric": {"Namespace": NAMESPACE, "MetricName": "Latency"},
        "Period": period,
        "Stat": "Sum",
    }})
}

/// A `MetricStat` whose `Stat` is `stat`.
fn with_stat(stat: &str) -> Value {
    json!({"MetricStat": {
        "Metric": {"Namespace": NAMESPACE, "MetricName": "Latency"},
        "Period": 60,
        "Stat": stat,
    }})
}

#[tokio::test]
async fn every_input_refusal_carries_the_validation_class_and_the_sentence_of_its_fault() {
    let router = build_router();
    let start = window();

    // (case, the request, the sentence the refusal carries). Every row is one fault of the
    // table; the member paths are the request's own, which is why two rows put the same fault on
    // different positions of the array.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a range that begins where it ends",
            request(json!([query("m1", &json!({}))]), &json!({"EndTime": start})),
            "The parameter StartTime must not equal parameter EndTime.\n".to_string(),
        ),
        (
            "a range that runs backwards",
            request(
                json!([query("m1", &json!({}))]),
                &json!({"EndTime": start - 60.0}),
            ),
            "The parameter EndTime must be greater than StartTime.\n".to_string(),
        ),
        (
            "a budget of no datapoints at all",
            request(json!([query("m1", &json!({}))]), &json!({"MaxDatapoints": 0})),
            "The parameter MaxDatapoints must not equal 0.\n".to_string(),
        ),
        (
            "a negative budget",
            request(json!([query("m1", &json!({}))]), &json!({"MaxDatapoints": -1})),
            "The parameter MaxDatapoints must be greater than 0.\n".to_string(),
        ),
        (
            "a budget far below zero",
            request(
                json!([query("m1", &json!({}))]),
                &json!({"MaxDatapoints": -100_000}),
            ),
            "The parameter MaxDatapoints must be greater than 0.\n".to_string(),
        ),
        (
            "a sub-minute period outside the high-resolution set",
            request(json!([query("m1", &with_period(7))]), &json!({})),
            "The parameter MetricDataQueries.member.1.MetricStat.Period must be a value in the \
             set [ 1, 5, 10, 20, 30 ].\n"
                .to_string(),
        ),
        (
            "a period above a minute that is no whole number of them",
            request(json!([query("m1", &with_period(90))]), &json!({})),
            "The parameter MetricDataQueries.member.1.MetricStat.Period must be a multiple of \
             60.\n"
                .to_string(),
        ),
        (
            // The same fault on the query's OWN period member, which an expression is spaced at.
            "a period of that kind on the query's own member",
            request(
                json!([
                    query("m1", &json!({})),
                    {"Id": "e1", "Expression": "ABS(m1)", "Period": 45},
                ]),
                &json!({}),
            ),
            "The parameter MetricDataQueries.member.2.Period must be a value in the set [ 1, 5, \
             10, 20, 30 ].\n"
                .to_string(),
        ),
        (
            "a statistic written in the wrong case",
            request(json!([query("m1", &with_stat("sum"))]), &json!({})),
            "The value for parameter MetricDataQueries.member.1.MetricStat.Stat is not a valid \
             statistic.\n"
                .to_string(),
        ),
        (
            "a percentile above the distribution",
            request(json!([query("m1", &with_stat("p101"))]), &json!({})),
            "The value for parameter MetricDataQueries.member.1.MetricStat.Stat is not a valid \
             statistic.\n"
                .to_string(),
        ),
        (
            // The offending query THIRD in the array: the position in the path is the request's.
            "a statistic that is no statistic, on the third query",
            request(
                json!([
                    query("m1", &json!({})),
                    query("m2", &json!({})),
                    query("m3", &with_stat("Median")),
                ]),
                &json!({}),
            ),
            "The value for parameter MetricDataQueries.member.3.MetricStat.Stat is not a valid \
             statistic.\n"
                .to_string(),
        ),
        (
            "an Id beginning with an upper-case letter",
            request(json!([query("M1", &json!({}))]), &json!({})),
            format!(
                "The value for parameter MetricDataQueries.member.1.Id is not matching the \
                 expected pattern {ID_PATTERN}.\n"
            ),
        ),
        (
            "an Id carrying a character the grammar does not admit",
            request(json!([query("m-1", &json!({}))]), &json!({})),
            format!(
                "The value for parameter MetricDataQueries.member.1.Id is not matching the \
                 expected pattern {ID_PATTERN}.\n"
            ),
        ),
        (
            "a query stating both an expression and a metric",
            request(
                json!([query("m1", &json!({"Expression": "ABS(m1)"}))]),
                &json!({}),
            ),
            "The parameters MetricDataQueries.member.1.Expression and \
             MetricDataQueries.member.1.MetricStat are mutually exclusive and you have specified \
             both.\n"
                .to_string(),
        ),
        (
            "a query stating neither",
            request(json!([{"Id": "m1", "Period": 60}]), &json!({})),
            "The parameter MetricDataQueries.member.1.MetricStat is required.\n".to_string(),
        ),
        (
            "an expression calling a function nothing computes",
            request(
                json!([
                    query("m1", &json!({})),
                    {"Id": "e1", "Expression": "NOPE(m1)", "Period": 60},
                ]),
                &json!({}),
            ),
            "Error in expression 'e1': Function 'NOPE' not found".to_string(),
        ),
        (
            "one query more than a call may include",
            request(
                Value::Array(
                    (0..501)
                        .map(|index| query(&format!("m{index}"), &json!({})))
                        .collect(),
                ),
                &json!({}),
            ),
            "The collection MetricDataQueries must not have a size greater than 500.\n"
                .to_string(),
        ),
    ];

    for (case, refused, sentence) in rows {
        let (status, body) = call(&router, "GetMetricData", &refused).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, VALIDATION),
            "case {case:?}: expected HTTP 400 {VALIDATION}, got {status} with {body}"
        );
        let stated = error_message(&body);
        assert_eq!(
            stated, sentence,
            "case {case:?}: expected {sentence:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn the_neighbouring_value_of_every_refused_member_is_read() {
    let router = build_router();
    let start = window();

    // (case, the request) — one accepted value beside each refusal of the table above: the
    // documented granularities on either side of the refused ones, the smallest budget there is,
    // the highest percentile the distribution has, an id of the grammar's own characters, a lone
    // expression, a function the evaluator computes, and exactly as many queries as a call may
    // include.
    let rows: Vec<(&str, Value)> = vec![
        (
            "the shortest documented sub-minute granularity",
            request(json!([query("m1", &with_period(5))]), &json!({})),
        ),
        (
            "the longest sub-minute granularity",
            request(json!([query("m1", &with_period(30))]), &json!({})),
        ),
        (
            "a granularity of two whole minutes",
            request(json!([query("m1", &with_period(120))]), &json!({})),
        ),
        (
            "a budget of one datapoint",
            request(json!([query("m1", &json!({}))]), &json!({"MaxDatapoints": 1})),
        ),
        (
            "the whole distribution as a percentile",
            request(json!([query("m1", &with_stat("p100"))]), &json!({})),
        ),
        (
            "an id of letters, digits and an underscore",
            request(json!([query("mA1_x", &json!({}))]), &json!({})),
        ),
        (
            "a query carrying an expression alone",
            request(
                json!([
                    query("m1", &json!({})),
                    {"Id": "e1", "Expression": "ABS(m1)", "Period": 60},
                ]),
                &json!({}),
            ),
        ),
        (
            "as many queries as a call may include",
            request(
                Value::Array(
                    (0..500)
                        .map(|index| query(&format!("m{index}"), &json!({})))
                        .collect(),
                ),
                &json!({}),
            ),
        ),
        (
            "a range of one minute read oldest first",
            request(
                json!([query("m1", &json!({}))]),
                &json!({"ScanBy": "TimestampAscending", "EndTime": start + 60.0}),
            ),
        ),
    ];

    for (case, accepted) in rows {
        let body = ok(&router, "GetMetricData", &accepted).await;
        assert!(
            body.get("MetricDataResults")
                .and_then(Value::as_array)
                .is_some_and(|results| !results.is_empty()),
            "case {case:?}: an accepted request answers a result per query, got {body}"
        );
    }
}
