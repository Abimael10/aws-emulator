// SPDX-License-Identifier: CC0-1.0

//! What `DescribeAlarmContributors` answers, and for WHICH alarms it answers at all.
//!
//! The rule under test: only an alarm whose configuration evaluates a query over several time
//! series has contributors — "Each entry in the vector result represents an alarm contributor"
//! (`AlarmPromQLCriteria$Query`), and a contributor "Represents an individual contributor to a
//! multi-timeseries alarm" (`AlarmContributor`) — so a read of such an alarm reports the
//! contributors that are in `ALARM`, a read of an alarm of any other form is refused for its
//! TYPE, and a read of a name no alarm carries is refused for its absence. The two refusals are
//! different classes and are answered as such, and a `NextToken` this read never issued is
//! refused quoting the value the request carried.
//!
//! Every table row is a distinct input along what the rule generalises over (the form the alarm's
//! configuration identifies what it grades in, which kind of alarm it is, which state it is in,
//! and how a requested name misses), so one recorded call is one row among many.

use crate::harness::{
    build_router, call, described_alarm, error_code, error_message, merged, metric_alarm_request,
    ok, AlarmKind,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// How CloudWatch states that the alarm a request named has no contributors to report, as this
/// suite expects it for `name`.
fn not_multi_time_series(name: &str) -> String {
    format!("Alarm {name} is not a multi time series alarm")
}

/// The contributors `DescribeAlarmContributors` reports for `name`, with the whole answer beside
/// them so an assertion can name what it saw.
async fn contributors(router: &MultiServiceRouter, name: &str) -> (Vec<Value>, Value) {
    let answered = ok(
        router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": name}),
    )
    .await;
    let reported = answered
        .get("AlarmContributors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    (reported, answered)
}

/// A string member of a body.
fn text(value: &Value, member: &str) -> Option<String> {
    value
        .get(member)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Force `name` into `state`, the one seam that puts an alarm of ANY kind into a state a reader
/// can then be asked about without waiting for a metric to be published and graded.
async fn force_state(router: &MultiServiceRouter, name: &str, state: &str) {
    ok(
        router,
        "SetAlarmState",
        &json!({"AlarmName": name, "StateValue": state, "StateReason": format!("{state} for {name}")}),
    )
    .await;
}

/// The members that replace the single-metric form with the `Metrics` array `queries`.
///
/// "If you use the Metrics parameter, you cannot include the Namespace, MetricName, Dimensions,
/// Period, Unit, Statistic, or ExtendedStatistic parameters of PutMetricAlarm in the same
/// operation" (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Metrics")`), so every
/// one of them is dropped from the base request.
fn metrics_patch(queries: Value) -> Value {
    json!({
        "Namespace": null,
        "MetricName": null,
        "Statistic": null,
        "Period": null,
        "Metrics": queries,
    })
}

/// One `MetricStat` query of a `Metrics` array, retrieving `Custom/Alarms Latency` under
/// `dimensions`.
fn metric_query(id: &str, dimensions: &[(&str, &str)], returns_data: bool) -> Value {
    let mut metric = json!({"Namespace": "Custom/Alarms", "MetricName": "Latency"});
    if !dimensions.is_empty() {
        metric["Dimensions"] = Value::Array(
            dimensions
                .iter()
                .map(|(name, value)| json!({"Name": name, "Value": value}))
                .collect(),
        );
    }
    json!({
        "Id": id,
        "MetricStat": {"Metric": metric, "Period": 60, "Stat": "Average"},
        "ReturnData": returns_data,
    })
}

/// The members that make an alarm one whose state comes from a QUERY over several series:
/// "The PromQL query that the alarm evaluates. The query must return a result of vector type.
/// Each entry in the vector result represents an alarm contributor."
/// (`AlarmPromQLCriteria$Query`) — one of the two configurations of this model whose result is a
/// vector of series.
fn promql_patch(query: &str) -> Value {
    json!({
        "Namespace": null,
        "MetricName": null,
        "Statistic": null,
        "Period": null,
        "EvaluationPeriods": null,
        "Threshold": null,
        "ComparisonOperator": null,
        "EvaluationCriteria": {"PromQLCriteria": {"Query": query}},
        "EvaluationInterval": 60,
    })
}

/// One query of a `Metrics` array stating `expression` — a Metrics Insights query or a metric
/// math expression ("This field can contain either a Metrics Insights query, or a metric math
/// expression", `smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`).
fn expression_query(id: &str, expression: &str, returns_data: bool) -> Value {
    json!({
        "Id": id,
        "Expression": expression,
        "Period": 60,
        "ReturnData": returns_data,
    })
}

#[tokio::test]
async fn only_an_alarm_over_several_series_has_contributors_to_report() {
    let router = build_router();

    // (case, the alarm's kind, the members merged over the static-threshold base request,
    //  whether the alarm has contributors to report).
    //
    // Rows differ along the FORM the configuration identifies what the alarm grades in — the
    // alarm's own metric members, a `Metrics` array of a metric, a `Metrics` array computing an
    // expression, a composite alarm's rule over other alarms, a log alarm's scheduled query, a
    // query over a vector of series, and a Metrics Insights query with and without the clause
    // that groups its result — because that form is what decides whether the alarm has
    // contributors at all.
    let cases: Vec<(&str, AlarmKind, Value, bool)> = vec![
        (
            "an alarm on the one metric its own members name",
            AlarmKind::Metric,
            json!({}),
            false,
        ),
        (
            "an alarm on one dimensioned metric",
            AlarmKind::Metric,
            json!({"Dimensions": [{"Name": "Service", "Value": "api"}]}),
            false,
        ),
        (
            "an alarm on the one query of a `Metrics` array it watches",
            AlarmKind::Metric,
            metrics_patch(json!([metric_query("watched", &[("Service", "api")], true)])),
            false,
        ),
        (
            "an alarm on an expression computed from one metric",
            AlarmKind::Metric,
            metrics_patch(json!([
                metric_query("fed", &[("Service", "api")], false),
                {"Id": "watched", "Expression": "fed * 2", "ReturnData": true},
            ])),
            false,
        ),
        (
            "a composite alarm, whose state comes from other alarms",
            AlarmKind::Composite,
            json!({}),
            false,
        ),
        (
            "a log alarm, whose state comes from a scheduled query",
            AlarmKind::Log,
            json!({}),
            false,
        ),
        (
            "an alarm over a query whose result is a vector of series",
            AlarmKind::Metric,
            promql_patch("avg_over_time(latency[5m])"),
            true,
        ),
        (
            "an alarm on a Metrics Insights query that groups its result",
            AlarmKind::Metric,
            metrics_patch(json!([expression_query(
                "grouped",
                "SELECT AVG(Latency) FROM SCHEMA(\"Custom/Alarms\", Service) GROUP BY Service",
                true,
            )])),
            true,
        ),
        (
            "the same query written in lower case",
            AlarmKind::Metric,
            metrics_patch(json!([expression_query(
                "grouped",
                "select avg(Latency) from schema(\"Custom/Alarms\", Service) group by Service",
                true,
            )])),
            true,
        ),
        (
            "an alarm on a Metrics Insights query that groups nothing",
            AlarmKind::Metric,
            metrics_patch(json!([expression_query(
                "ungrouped",
                "SELECT AVG(Latency) FROM SCHEMA(\"Custom/Alarms\", Service)",
                true,
            )])),
            false,
        ),
        (
            "an alarm whose expression folds a grouped query to one series",
            AlarmKind::Metric,
            metrics_patch(json!([
                expression_query(
                    "grouped",
                    "SELECT AVG(Latency) FROM SCHEMA(\"Custom/Alarms\", Service) GROUP BY Service",
                    false,
                ),
                expression_query("folded", "MAX(grouped)", true),
            ])),
            false,
        ),
        (
            "an alarm on a query whose dimension value carries the clause's words",
            AlarmKind::Metric,
            metrics_patch(json!([expression_query(
                "quoted",
                "SELECT AVG(Latency) FROM SCHEMA(\"Custom/Alarms\", Service) \
                 WHERE Service = 'group by'",
                true,
            )])),
            false,
        ),
        (
            "an alarm on a grouped query that also carries those words as a value",
            AlarmKind::Metric,
            metrics_patch(json!([expression_query(
                "grouped",
                "SELECT AVG(Latency) FROM SCHEMA(\"Custom/Alarms\", Service) \
                 WHERE Service != 'group by' GROUP BY Service",
                true,
            )])),
            true,
        ),
    ];

    for (index, (case, kind, patch, reports)) in cases.into_iter().enumerate() {
        let name = format!("contributor-kind-{index}");
        match kind {
            AlarmKind::Metric => {
                ok(
                    &router,
                    "PutMetricAlarm",
                    &merged(metric_alarm_request(&name), &patch),
                )
                .await;
            }
            _ => kind.create(&router, &name, None).await,
        }
        // The alarm the row created IS there: whatever this read answers, it answers about an
        // alarm this store holds.
        assert!(
            described_alarm(&router, kind, &name).await.is_some(),
            "case {case:?}: the row's own alarm must exist before the read is judged"
        );

        let (status, body) = call(
            &router,
            "DescribeAlarmContributors",
            &json!({"AlarmName": name}),
        )
        .await;
        if reports {
            assert_eq!(
                status, 200,
                "case {case:?}: an alarm over several series is read, got {status} with {body}"
            );
            // Nothing has put a contributor of it into ALARM yet, so there is none to report.
            assert_eq!(
                body.get("AlarmContributors"),
                Some(&json!([])),
                "case {case:?}: no contributor is in ALARM yet, got {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected HTTP 400 for an alarm that is not of that kind, \
                 got {status} with {body}"
            );
            let got = error_message(&body);
            assert_eq!(
                got,
                not_multi_time_series(&name),
                "case {case:?}: expected the service's own sentence, got {got:?} in {body}"
            );
        }

        // The refusal is about the alarm's TYPE, not about the state it happens to be in: a row
        // whose alarm can be forced into ALARM is answered the same way there.
        if !matches!(kind, AlarmKind::Log) {
            force_state(&router, &name, "ALARM").await;
            let (status, body) = call(
                &router,
                "DescribeAlarmContributors",
                &json!({"AlarmName": name}),
            )
            .await;
            let expected = if reports { 200 } else { 400 };
            assert_eq!(
                status, expected,
                "case {case:?}: the answer must not depend on the alarm's state, \
                 got {status} with {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_contributor_read_refuses_a_name_no_alarm_carries_and_a_token_it_did_not_issue() {
    let router = build_router();
    let present = "present-alarm";
    ok(
        &router,
        "PutMetricAlarm",
        &merged(
            metric_alarm_request(present),
            &promql_patch("sum by (host) (rate(errors[1m]))"),
        ),
    )
    .await;
    ok(&router, "PutMetricAlarm", &metric_alarm_request("deleted-alarm")).await;
    ok(
        &router,
        "DeleteAlarms",
        &json!({"AlarmNames": ["deleted-alarm"]}),
    )
    .await;
    force_state(&router, present, "ALARM").await;

    // Rows differ along HOW the requested name misses the alarms the store holds.
    for (case, name) in [
        ("a name nothing was created under", "absent-alarm"),
        ("an alarm created and then deleted", "deleted-alarm"),
        ("a stored name in another case", "PRESENT-ALARM"),
        ("a stored name with a suffix", "present-alarm-2"),
        ("a stored name with a prefix", "my-present-alarm"),
    ] {
        let (status, body) = call(
            &router,
            "DescribeAlarmContributors",
            &json!({"AlarmName": name}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} with {body}"
        );
        let got = error_message(&body);
        assert_eq!(
            got,
            format!("No alarm for name {name} was found"),
            "case {case:?}: expected the service's own sentence, got {got:?} in {body}"
        );
    }

    // No call of this operation issues a token, so every token a request can carry is one this
    // service did not issue. Rows differ along the form of the token, and along whether the alarm
    // the request names exists at all — the token is judged on either path.
    for (case, name, token) in [
        ("text that is no token", present, "not-a-token"),
        ("a token-looking hex string", present, "6162636465"),
        ("an empty token", present, ""),
        ("a token with a name no alarm carries", "absent-alarm", "not-a-token"),
    ] {
        let (status, body) = call(
            &router,
            "DescribeAlarmContributors",
            &json!({"AlarmName": name, "NextToken": token}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidNextToken"),
            "case {case:?}: expected HTTP 400 InvalidNextToken, got {status} with {body}"
        );
        let got = error_message(&body);
        let expected = format!("Invalid NextToken : {token}");
        assert_eq!(
            got, expected,
            "case {case:?}: expected the sentence quoting the token, got {got:?} in {body}"
        );
    }

    // The refusals changed nothing: the alarm the store holds still reports its contributor.
    let (reported, answered) = contributors(&router, present).await;
    assert_eq!(
        reported.len(),
        1,
        "case \"after the refusals\": the alarm in ALARM must still report its contributor, \
         got {answered}"
    );
}

#[tokio::test]
async fn a_contributor_is_reported_while_it_is_in_alarm_and_keeps_its_identity() {
    let router = build_router();

    // Rows differ along the query the alarm evaluates, since that is what its contributors come
    // from, and every row is read in each of the states the alarm can be put into.
    let queries = [
        ("a query grouping by one label", "sum by (host) (rate(errors[1m]))"),
        ("a query over a window", "avg_over_time(latency[5m])"),
        ("a query comparing two series", "errors / requests"),
    ];

    let mut ids: Vec<(&str, String)> = vec![];
    for (index, (case, query)) in queries.into_iter().enumerate() {
        let name = format!("multi-series-alarm-{index}");
        ok(
            &router,
            "PutMetricAlarm",
            &merged(metric_alarm_request(&name), &promql_patch(query)),
        )
        .await;

        // Before anything puts a contributor into ALARM there is none to report.
        let (reported, answered) = contributors(&router, &name).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: an alarm that is not in ALARM owes no contributor, got {answered}"
        );

        force_state(&router, &name, "ALARM").await;
        let (reported, answered) = contributors(&router, &name).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the contributor the ALARM state accounts for, got {answered}"
        );
        assert!(
            answered.get("NextToken").is_none(),
            "case {case:?}: every contributor of the alarm is in this page, so no token is owed, \
             got {answered}"
        );
        let contributor = reported.first().cloned().unwrap_or_default();
        let alarm = described_alarm(&router, AlarmKind::Metric, &name)
            .await
            .unwrap_or_default();
        // Why the contributor is in ALARM, and when it got there, is what the alarm itself
        // reports: its state IS what the query's entries put it in.
        assert_eq!(
            (
                text(&contributor, "StateReason"),
                contributor.get("StateTransitionedTimestamp"),
            ),
            (
                text(&alarm, "StateReason"),
                alarm.get("StateTransitionedTimestamp"),
            ),
            "case {case:?}: expected the alarm's own reason and transition instant, \
             got {contributor} beside {alarm}"
        );
        // The identifier fits the member's modelled 1..16 length and is stable across reads.
        let id = text(&contributor, "ContributorId").unwrap_or_default();
        assert!(
            (1..=16).contains(&id.chars().count()),
            "case {case:?}: expected an id of 1 to 16 characters, got {id:?}"
        );
        let (again, _) = contributors(&router, &name).await;
        assert_eq!(
            again.first().and_then(|entry| text(entry, "ContributorId")),
            Some(id.clone()),
            "case {case:?}: a contributor keeps its id across reads, got {again:?}"
        );
        ids.push((case, id));

        // Back in OK, the alarm has no contributor in ALARM state again: the read reports the
        // state the alarm is in now rather than one it was in.
        force_state(&router, &name, "OK").await;
        let (reported, answered) = contributors(&router, &name).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: an alarm that is not in ALARM owes no contributor, got {answered}"
        );
    }

    // Two alarms' contributors are two contributors: the identifier distinguishes what it names
    // rather than being one fixed value.
    let distinct: std::collections::BTreeSet<&String> = ids.iter().map(|(_, id)| id).collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "the contributors of different alarms must carry different ids, got {ids:?}"
    );
}
