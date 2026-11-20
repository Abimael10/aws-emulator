// SPDX-License-Identifier: CC0-1.0

//! CloudWatch Metrics Insights queries, end to end over the wire: what a `SELECT` returns from
//! the datapoints `PutMetricData` published, and what an alarm over one is decided from.
//!
//! Two rules, each asserted over a table whose rows differ along what the rule generalises over.
//!
//! 1. **A Metrics Insights query SELECTS the published metrics and returns one time series per
//!    group of its `GROUP BY`, ranked by its `ORDER BY`.** "This field can contain either a
//!    Metrics Insights query, or a metric math expression."
//!    (`smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`) — the `FROM` names
//!    the namespace (and, as `SCHEMA(...)`, the exact dimension keys a series must carry), the
//!    `SELECT`'s function and argument name the aggregate and the metric, a `WHERE` selects by the
//!    labels of a series, and each group is that function folded over every series of the group,
//!    window by window. The rows differ along the function, the shape of the `FROM`, the predicate
//!    of the `WHERE`, the keys grouped by, the direction of the `ORDER BY`, the `LIMIT`, and the
//!    case the keywords are written in.
//! 2. **An alarm over such a query is decided by its CONTRIBUTORS**: "The query must return a
//!    result of vector type. Each entry in the vector result represents an alarm contributor."
//!    (`smithy_query(query="doc",shape="AlarmPromQLCriteria",member="Query")`) — each entry is
//!    graded against the alarm's own threshold, the alarm is in `ALARM` while any entry breaches,
//!    and `DescribeAlarmContributors` "Returns the information of the current alarm contributors
//!    that are in ALARM state", in the order the query ranked them. The rows differ along the
//!    direction the query ranks its entries in, which entries breach, and how many of them do.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, described_alarm, graded_window_stamp, now_secs, ok, wait_for_evaluation,
    wait_for_period_room, AlarmKind, LOOK_INTERVAL, WORK_WAIT,
};

/// The namespace every series of this suite is published under.
const NAMESPACE: &str = "Custom/Insights";

/// The metric every query of this suite selects.
const METRIC: &str = "Latency";

/// One minute, the granularity every query of this suite is read at: "a period can be as short
/// as one minute (60 seconds)" (`MetricDataQuery$Period`).
const MINUTE: f64 = 60.0;

/// How much of the period the alarm rows need left to publish their datapoints and read the
/// verdict back inside the window they placed them relative to.
const PERIOD_ROOM: f64 = 20.0;

/// Publish `values` into the series of [`METRIC`] that `dimensions` names, all stamped inside
/// the one window `at` falls in.
async fn publish(
    router: &MultiServiceRouter,
    namespace: &str,
    metric: &str,
    dimensions: &[(&str, &str)],
    at: f64,
    values: &[f64],
) {
    let dimensions: Vec<Value> = dimensions
        .iter()
        .map(|(name, value)| json!({"Name": name, "Value": value}))
        .collect();
    let data: Vec<Value> = values
        .iter()
        .map(|value| {
            json!({
                "MetricName": metric,
                "Dimensions": dimensions,
                "Value": value,
                "Timestamp": at,
            })
        })
        .collect();
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": namespace, "MetricData": data}),
    )
    .await;
}

/// The `(label, values)` of every result a `GetMetricData` answer carries, in the order it
/// listed them.
fn answered(body: &Value) -> Vec<(String, Vec<f64>)> {
    body.get("MetricDataResults")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetMetricData returns MetricDataResults, got {body}"))
        .iter()
        .map(|result| {
            let label = result
                .get("Label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let values = result
                .get("Values")
                .and_then(Value::as_array)
                .map(|listed| listed.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            (label, values)
        })
        .collect()
}

/// One row of the query table: the case, the query, and the `(label, values)` of each result in
/// the order the answer lists them. Named so the row type stays simple enough to read.
type QueryCase = (&'static str, String, Vec<(&'static str, Vec<f64>)>);

/// One row of the contributor table: the case, the query the alarm watches, and the labels of
/// the contributors it must report, in order.
type ContributorCase = (&'static str, String, Vec<&'static str>);

/// The `Id` every query of the first table carries, which is also the label a result of ONE
/// series is reported under: "the default CloudWatch generates" for an expression
/// (`MetricDataQuery$Label`).
const QUERY_ID: &str = "insights";

/// The result of one query over the window `[start, start + 60)`.
async fn queried(router: &MultiServiceRouter, expression: &str, start: f64) -> Value {
    ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": QUERY_ID,
                "Expression": expression,
                "Period": 60,
            }],
            "StartTime": start,
            "EndTime": start + MINUTE,
        }),
    )
    .await
}

#[tokio::test]
async fn a_query_returns_one_series_per_group_of_the_metrics_it_selects() {
    let router = build_router();
    // The window every row reads: a whole minute, ten minutes back, so the rounding
    // `GetMetricDataInput$StartTime` documents leaves the range exactly where this puts it.
    let window = (now_secs() / MINUTE).floor() * MINUTE - 10.0 * MINUTE;
    let at = window + MINUTE / 2.0;

    // Five series, so the rows differ along what each clause selects out of them: two series of
    // one `Service` under different `Shard`s, one series dimensioned by `Service` alone, one
    // series of the same metric in ANOTHER namespace, and one series of another metric.
    publish(&router, NAMESPACE, METRIC, &[("Service", "api"), ("Shard", "1")], at, &[120.0, 100.0]).await;
    publish(&router, NAMESPACE, METRIC, &[("Service", "api"), ("Shard", "2")], at, &[80.0]).await;
    publish(&router, NAMESPACE, METRIC, &[("Service", "web"), ("Shard", "1")], at, &[40.0, 20.0]).await;
    publish(&router, NAMESPACE, METRIC, &[("Service", "cache")], at, &[10.0]).await;
    publish(&router, "Custom/Other", METRIC, &[("Service", "api")], at, &[999.0]).await;
    publish(&router, NAMESPACE, "Errors", &[("Service", "api")], at, &[500.0]).await;

    // (case, the query, the `(label, values)` of each result, in the order the answer lists them)
    let rows: Vec<QueryCase> = vec![
        (
            "the sum of each group",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![
                ("Latency api", vec![300.0]),
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![60.0]),
            ],
        ),
        (
            "the mean of each group",
            format!("SELECT AVG({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![
                ("Latency api", vec![100.0]),
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![30.0]),
            ],
        ),
        (
            "the largest observation of each group",
            format!("SELECT MAX({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![
                ("Latency api", vec![120.0]),
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![40.0]),
            ],
        ),
        (
            "the smallest observation of each group",
            format!("SELECT MIN({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![
                ("Latency api", vec![80.0]),
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![20.0]),
            ],
        ),
        (
            "how many observations each group holds",
            format!("SELECT COUNT({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![
                ("Latency api", vec![3.0]),
                ("Latency cache", vec![1.0]),
                ("Latency web", vec![2.0]),
            ],
        ),
        (
            // One series is the EXPRESSION's own result, so it is labelled the way every other
            // expression's single series is: by the query's own `Id`, "the default CloudWatch
            // generates" (`MetricDataQuery$Label`).
            "a query that groups nothing, over everything it selected",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\""),
            vec![(QUERY_ID, vec![370.0])],
        ),
        (
            // `SCHEMA` names the dimension keys a series carries EXACTLY, so the series
            // dimensioned by `Service` alone is not one this query selects.
            "a schema of two dimension keys",
            format!("SELECT SUM({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service, Shard) GROUP BY Service"),
            vec![("Latency api", vec![300.0]), ("Latency web", vec![60.0])],
        ),
        (
            "a schema of one dimension key",
            format!("SELECT SUM({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service"),
            vec![("Latency cache", vec![10.0])],
        ),
        (
            "a predicate on a label's value",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" WHERE Service = 'api' GROUP BY Service"),
            vec![("Latency api", vec![300.0])],
        ),
        (
            "the same predicate negated",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" WHERE Service != 'api' GROUP BY Service"),
            vec![("Latency cache", vec![10.0]), ("Latency web", vec![60.0])],
        ),
        (
            "a predicate matching a prefix",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" WHERE Service LIKE 'w%' GROUP BY Service"),
            vec![("Latency web", vec![60.0])],
        ),
        (
            "a predicate over a set of values",
            format!(
                "SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" WHERE Service IN ('api', 'web') \
                 GROUP BY Service"
            ),
            vec![("Latency api", vec![300.0]), ("Latency web", vec![60.0])],
        ),
        (
            "two predicates that both have to hold",
            format!(
                "SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" WHERE Service = 'api' AND Shard = '1' \
                 GROUP BY Service"
            ),
            vec![("Latency api", vec![220.0])],
        ),
        (
            // A series that carries no value for a key the query groups by is in no group of
            // the result: the group is the value of that label.
            "a grouping by a label only some series carry",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Shard"),
            vec![("Latency 1", vec![280.0]), ("Latency 2", vec![80.0])],
        ),
        (
            "a grouping by two labels",
            format!("SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service, Shard"),
            vec![
                ("Latency api 1", vec![220.0]),
                ("Latency api 2", vec![80.0]),
                ("Latency web 1", vec![60.0]),
            ],
        ),
        (
            // The ranking is the QUERY's: the groups come back in the order its own clause asks
            // for, which is what makes the result a ranking rather than a set.
            "a ranking from the largest observation down",
            format!(
                "SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service ORDER BY MAX() DESC"
            ),
            vec![
                ("Latency api", vec![300.0]),
                ("Latency web", vec![60.0]),
                ("Latency cache", vec![10.0]),
            ],
        ),
        (
            "the same ranking upwards",
            format!(
                "SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service ORDER BY MAX() ASC"
            ),
            vec![
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![60.0]),
                ("Latency api", vec![300.0]),
            ],
        ),
        (
            "a ranking cut to the groups a LIMIT keeps",
            format!(
                "SELECT SUM({METRIC}) FROM \"{NAMESPACE}\" GROUP BY Service ORDER BY MAX() DESC \
                 LIMIT 2"
            ),
            vec![("Latency api", vec![300.0]), ("Latency web", vec![60.0])],
        ),
        (
            "the same query written in lower case",
            format!("select sum({METRIC}) from \"{NAMESPACE}\" group by Service"),
            vec![
                ("Latency api", vec![300.0]),
                ("Latency cache", vec![10.0]),
                ("Latency web", vec![60.0]),
            ],
        ),
        (
            // Every query the caller asked for data for is answered, so a query that selected
            // nothing reports its `Id` with no datapoint rather than being left out.
            "a query selecting a metric nothing published",
            format!("SELECT SUM(Saturation) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![(QUERY_ID, vec![])],
        ),
        (
            "a query reading another namespace",
            format!("SELECT SUM({METRIC}) FROM \"Custom/Other\" GROUP BY Service"),
            vec![("Latency api", vec![999.0])],
        ),
        (
            "a query selecting the other metric of the namespace",
            format!("SELECT SUM(Errors) FROM \"{NAMESPACE}\" GROUP BY Service"),
            vec![("Errors api", vec![500.0])],
        ),
    ];

    for (case, expression, expected) in rows {
        let body = queried(&router, &expression, window).await;
        let reported = answered(&body);
        let expected: Vec<(String, Vec<f64>)> = expected
            .into_iter()
            .map(|(label, values)| (label.to_string(), values))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?} from {body}"
        );
    }
}

/// The alarm this suite's second rule drives: one query of `Metrics`, graded against a static
/// threshold over one period.
fn query_alarm(name: &str, expression: &str) -> Value {
    json!({
        "AlarmName": name,
        "Metrics": [{
            "Id": "contributors",
            "Expression": expression,
            "Period": 60,
            "ReturnData": true,
        }],
        "EvaluationPeriods": 1,
        "Threshold": 50.0,
        "ComparisonOperator": "GreaterThanThreshold",
    })
}

/// The contributors `DescribeAlarmContributors` reports for `name`, as `(id, attributes,
/// reason)` triples in the order the answer listed them.
async fn contributors(
    router: &MultiServiceRouter,
    name: &str,
) -> Vec<(String, Value, String)> {
    let body = ok(
        router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": name}),
    )
    .await;
    body.get("AlarmContributors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("DescribeAlarmContributors returns AlarmContributors, got {body}"))
        .iter()
        .map(|contributor| {
            (
                contributor
                    .get("ContributorId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                contributor
                    .get("ContributorAttributes")
                    .cloned()
                    .unwrap_or(Value::Null),
                contributor
                    .get("StateReason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Look at `name` until it reports `wanted`, or until the deadline passes — the state is
/// computed by an evaluation the clock owes, so the test waits for the CONDITION rather than for
/// a span.
async fn until_state(router: &MultiServiceRouter, name: &str, wanted: &str) -> String {
    let deadline = std::time::Instant::now() + WORK_WAIT;
    let mut seen = String::new();
    while std::time::Instant::now() < deadline {
        seen = described_alarm(router, AlarmKind::Metric, name)
            .await
            .as_ref()
            .and_then(|alarm| alarm.get("StateValue"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if seen == wanted {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

#[tokio::test]
async fn an_alarm_over_a_grouped_query_is_decided_by_its_contributors() {
    let router = build_router();
    wait_for_period_room(MINUTE, PERIOD_ROOM).await;
    let at = graded_window_stamp(MINUTE);

    // Three series of one metric, dimensioned by `Service` alone, so a `SCHEMA` of that one key
    // selects exactly them: two past the alarm's threshold of 50 and one under it.
    for (service, value) in [("api", 120.0), ("web", 90.0), ("cache", 10.0)] {
        publish(&router, NAMESPACE, METRIC, &[("Service", service)], at, &[value]).await;
    }

    // (case, the query the alarm watches, the labels of the contributors it must report, in
    //  order). The rows differ along the direction the query ranks its entries in and along how
    //  many of them breach.
    let rows: Vec<ContributorCase> = vec![
        (
            "a ranking from the largest observation down",
            format!(
                "SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service \
                 ORDER BY MAX() DESC"
            ),
            vec!["api", "web"],
        ),
        (
            "the same ranking upwards",
            format!(
                "SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service \
                 ORDER BY MAX() ASC"
            ),
            vec!["web", "api"],
        ),
        (
            "a query that ranks nothing, which reports its entries by their own labels",
            format!("SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service"),
            vec!["api", "web"],
        ),
        (
            // A predicate narrows the vector, so the alarm has one contributor to report.
            "a query selecting one of the breaching entries",
            format!(
                "SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) \
                 WHERE Service = 'web' GROUP BY Service"
            ),
            vec!["web"],
        ),
    ];

    let mut ids: Vec<(String, String)> = Vec::new();
    for (index, (case, expression, expected)) in rows.into_iter().enumerate() {
        let name = format!("insights-contributors-{index}");
        ok(&router, "PutMetricAlarm", &query_alarm(&name, &expression)).await;
        let state = until_state(&router, &name, "ALARM").await;
        assert_eq!(
            state, "ALARM",
            "case {case:?}: an entry of the query's result past the threshold puts the alarm in \
             ALARM, got {state:?}"
        );

        let reported = contributors(&router, &name).await;
        let labels: Vec<String> = reported
            .iter()
            .map(|(_, attributes, _)| {
                attributes
                    .get("Service")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            labels, expected,
            "case {case:?}: expected the contributors {expected:?} in that order, got {reported:?}"
        );
        for (id, attributes, reason) in &reported {
            // The identifier fits `ContributorId`'s modelled 1..16 length and is this
            // contributor's own.
            assert!(
                (1..=16).contains(&id.chars().count()),
                "case {case:?}: expected an id of 1 to 16 characters, got {id:?}"
            );
            assert_eq!(
                attributes.as_object().map(serde_json::Map::len),
                Some(1),
                "case {case:?}: a contributor is described by the labels that identify it, got \
                 {attributes}"
            );
            // The explanation is the grading of THIS entry: it names the value this entry's own
            // series took, not another entry's.
            let service = attributes
                .get("Service")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let value = match service {
                "api" => "120.0",
                _ => "90.0",
            };
            assert!(
                reason.contains(value),
                "case {case:?}: expected the reason of {service:?} to name its own value \
                 {value}, got {reason:?}"
            );
            ids.push((format!("{index}:{service}"), id.clone()));
        }

        // A read changes nothing a later caller sees: the same contributors come back.
        let again = contributors(&router, &name).await;
        assert_eq!(
            again, reported,
            "case {case:?}: a read of the contributors must answer the same twice, got {again:?}"
        );
    }

    // Two entries are two contributors, and one entry of two alarms is two contributors: the
    // identifier distinguishes what it names rather than being one fixed value.
    let distinct: std::collections::BTreeSet<&String> = ids.iter().map(|(_, id)| id).collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "every contributor of every alarm carries its own id, got {ids:?}"
    );
}

#[tokio::test]
async fn an_alarm_over_a_grouped_query_reports_no_contributor_while_none_breaches() {
    let router = build_router();
    wait_for_period_room(MINUTE, PERIOD_ROOM).await;
    let at = graded_window_stamp(MINUTE);
    // Every entry of the vector is under the alarm's threshold of 50.
    for (service, value) in [("api", 20.0), ("web", 30.0)] {
        publish(&router, NAMESPACE, METRIC, &[("Service", service)], at, &[value]).await;
    }
    let name = "insights-contributors-none-breaching";
    ok(
        &router,
        "PutMetricAlarm",
        &query_alarm(
            name,
            &format!("SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service"),
        ),
    )
    .await;
    let state = until_state(&router, name, "OK").await;
    assert_eq!(
        state, "OK",
        "a vector whose every entry is under the threshold puts the alarm in OK, got {state:?}"
    );
    let reported = contributors(&router, name).await;
    assert!(
        reported.is_empty(),
        "an alarm that is not in ALARM has no contributor in ALARM state, got {reported:?}"
    );

    // And a datapoint that takes ONE entry past the threshold brings that entry, and only that
    // entry, into the report — the alarm's state is the state of its contributors.
    wait_for_period_room(MINUTE, PERIOD_ROOM).await;
    publish(
        &router,
        NAMESPACE,
        METRIC,
        &[("Service", "web")],
        graded_window_stamp(MINUTE),
        &[400.0],
    )
    .await;
    wait_for_evaluation();
    let state = until_state(&router, name, "ALARM").await;
    assert_eq!(
        state, "ALARM",
        "an entry taken past the threshold puts the alarm in ALARM, got {state:?}"
    );
    let deadline = std::time::Instant::now() + WORK_WAIT;
    let mut reported = contributors(&router, name).await;
    while reported.len() != 1 && std::time::Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = contributors(&router, name).await;
    }
    let labels: Vec<String> = reported
        .iter()
        .map(|(_, attributes, _)| {
            attributes
                .get("Service")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        labels,
        vec!["web".to_string()],
        "only the entry that breaches is a contributor, got {reported:?}"
    );
}

/// A query a request may carry that this service does not evaluate is refused as the expression
/// of the query carrying it, naming what is wrong with it — not answered with an empty result.
#[tokio::test]
async fn a_query_this_service_cannot_read_is_refused_as_the_expression_it_is() {
    let router = build_router();
    let window = (now_secs() / MINUTE).floor() * MINUTE - 10.0 * MINUTE;
    // Rows differ along WHAT the statement states that no Metrics Insights query does.
    for (case, expression) in [
        ("a statement the grammar cannot read", "SELECT FROM WHERE"),
        (
            "a function no query selects",
            "SELECT MEDIAN(Latency) FROM \"Custom/Insights\"",
        ),
        (
            "a projection that is not an aggregate",
            "SELECT Latency FROM \"Custom/Insights\"",
        ),
        (
            "a predicate over something that is no label",
            "SELECT SUM(Latency) FROM \"Custom/Insights\" WHERE 1 = 1",
        ),
    ] {
        let (status, body) = crate::harness::call(
            &router,
            "GetMetricData",
            &json!({
                "MetricDataQueries": [{"Id": "insights", "Expression": expression, "Period": 60}],
                "StartTime": window,
                "EndTime": window + MINUTE,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the query to be refused, got {status} with {body}"
        );
        let message = crate::harness::error_message(&body);
        assert!(
            message.contains("insights"),
            "case {case:?}: expected the refusal to name the query carrying it, got {message:?}"
        );
    }
}
