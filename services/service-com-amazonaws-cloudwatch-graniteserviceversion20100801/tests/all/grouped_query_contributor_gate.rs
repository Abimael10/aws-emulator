// SPDX-License-Identifier: CC0-1.0

//! WHICH alarms `DescribeAlarmContributors` answers for, and what it answers for one it does.
//!
//! The rule under test: **an alarm is read for its contributors exactly when its evaluation grades
//! a VECTOR of time series** — a contributor "Represents an individual contributor to a
//! multi-timeseries alarm" and the operation "Returns the information of the current alarm
//! contributors that are in ALARM state"
//! (`smithy_query(query="operation",operation="DescribeAlarmContributors")`), and a Metrics
//! Insights statement that groups its result returns one series per group ("This field can contain
//! either a Metrics Insights query, or a metric math expression",
//! `smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`). So an alarm whose
//! watched query is such a statement is SERVED (HTTP 200, one entry per group that is in `ALARM`,
//! in the order the statement's own clause ranks them), an alarm graded on ONE series — the metric
//! its own members name, one query of a `Metrics` array, an expression folding a grouped query
//! down to one series, a composite alarm's rule, a log alarm's scheduled query — is refused for
//! its TYPE, and a name no alarm carries is refused for its absence.
//!
//! The pagination token is a separate question from the alarm's kind: the model declares
//! `InvalidNextToken` and `ResourceNotFoundException` and no alarm-type error at all, so a token
//! this read never issued is refused as that token whichever alarm the request named.
//!
//! The rows of the first table differ along the FORM of the statement the alarm watches — the
//! aggregate it folds by, whether its metric is quoted, whether the `FROM` names a `SCHEMA` or the
//! namespace alone, how many labels it groups by, the direction it ranks in and whether it ranks
//! at all, whether it limits its groups, the case the keywords are written in, and where in the
//! `Metrics` array the watched query sits — beside the forms that grade one series; the rows of
//! the second differ along the direction the statement ranks its groups in. The recorded probe is
//! one row among them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, call, call_in_query_mode, error_code, error_message, graded_window_stamp, merged,
    metric_alarm_request, ok, wait_for_period_room, AlarmKind, LOOK_INTERVAL, WORK_WAIT,
};

/// The header a query-compatible client reads the legacy error code and the fault side in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The legacy report of the request-validation class, which is what a query-compatible client
/// branches on for an alarm that has no contributors to report at all.
const VALIDATION_REPORT: &str = "ValidationError;Sender";

/// The legacy report of the modelled `InvalidNextToken` class
/// (`smithy_query(query="operation",operation="DescribeAlarmContributors")`: client fault,
/// `awsQueryError` code `InvalidNextToken`).
const TOKEN_REPORT: &str = "InvalidNextToken;Sender";

/// The namespace the first table's alarms are configured over — the one
/// [`crate::harness::metric_alarm_request`] names, so a row that keeps the base request's metric
/// members and a row that replaces them with a statement read the same namespace.
const BASE_NAMESPACE: &str = "Custom/Alarms";

/// The metric those statements select.
const BASE_METRIC: &str = "Latency";

/// One minute, the granularity every statement below is read at: "a period can be as short as one
/// minute (60 seconds)" (`MetricDataQuery$Period`).
const MINUTE: f64 = 60.0;

/// How much of the period the second test needs left to publish its series and read the verdict
/// back inside the window it placed them relative to.
const PERIOD_ROOM: f64 = 20.0;

/// How CloudWatch states that the alarm a request named is not one graded on a vector of series.
fn not_multi_time_series(name: &str) -> String {
    format!("Alarm {name} is not a multi time series alarm")
}

/// How this read refuses a token it did not issue, quoting the value the request carried.
fn invalid_token(token: &str) -> String {
    format!("Invalid NextToken : {token}")
}

/// The members that replace the single-metric form of the base request with the `Metrics` array
/// `queries`.
///
/// "If you use the Metrics parameter, you cannot include the Namespace, MetricName, Dimensions,
/// Period, Unit, Statistic, or ExtendedStatistic parameters of PutMetricAlarm in the same
/// operation" (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Metrics")`), so every
/// one of them is dropped.
fn metrics_patch(queries: Value) -> Value {
    json!({
        "Namespace": null,
        "MetricName": null,
        "Statistic": null,
        "Period": null,
        "Metrics": queries,
    })
}

/// One query of a `Metrics` array stating `expression`.
fn expression_query(id: &str, expression: &str, returns_data: bool) -> Value {
    json!({
        "Id": id,
        "Expression": expression,
        "Period": 60,
        "ReturnData": returns_data,
    })
}

/// One `MetricStat` query of a `Metrics` array, retrieving the base metric.
fn metric_stat_query(id: &str, returns_data: bool) -> Value {
    json!({
        "Id": id,
        "MetricStat": {
            "Metric": {"Namespace": BASE_NAMESPACE, "MetricName": BASE_METRIC},
            "Period": 60,
            "Stat": "Average",
        },
        "ReturnData": returns_data,
    })
}

/// The members that make the alarm watch the one statement `expression`.
fn watches(expression: &str) -> Value {
    metrics_patch(json!([expression_query("watched", expression, true)]))
}

/// The contributors `DescribeAlarmContributors` reports for `name`, as `(attributes, id)` pairs in
/// the order the answer listed them, with the whole answer beside them so an assertion can name
/// what it saw.
async fn contributors(router: &MultiServiceRouter, name: &str) -> (Vec<(Value, String)>, Value) {
    let (status, body) = call(
        router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the contributors of {name:?}: expected HTTP 200, got {status} with {body}"
    );
    let reported = body
        .get("AlarmContributors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|contributor| {
            (
                contributor
                    .get("ContributorAttributes")
                    .cloned()
                    .unwrap_or(Value::Null),
                contributor
                    .get("ContributorId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    (reported, body)
}

/// Look at the contributors of `name` until it reports `wanted` of them, or until a generous
/// deadline passes — the entries come from an evaluation the clock owes, so this waits for the
/// CONDITION rather than for a span.
async fn until_contributors(
    router: &MultiServiceRouter,
    name: &str,
    wanted: usize,
) -> (Vec<(Value, String)>, Value) {
    let deadline = std::time::Instant::now() + WORK_WAIT;
    let mut seen = contributors(router, name).await;
    while seen.0.len() != wanted && std::time::Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        seen = contributors(router, name).await;
    }
    seen
}

/// Publish one datapoint of `metric` under the label `(label, value_of)` into `namespace`, stamped
/// inside the window `at` falls in.
async fn publish(
    router: &MultiServiceRouter,
    namespace: &str,
    metric: &str,
    label: (&str, &str),
    at: f64,
    value: f64,
) {
    ok(
        router,
        "PutMetricData",
        &json!({
            "Namespace": namespace,
            "MetricData": [{
                "MetricName": metric,
                "Dimensions": [{"Name": label.0, "Value": label.1}],
                "Value": value,
                "Timestamp": at,
            }],
        }),
    )
    .await;
}

/// Assert that the read of `name` carrying `token` is refused as that TOKEN — the judgement the
/// model declares, independent of which alarm the request named.
async fn assert_token_refused(router: &MultiServiceRouter, case: &str, name: &str, token: &str) {
    let (status, headers, body) = call_in_query_mode(
        router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": name, "NextToken": token}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidNextToken"),
        "case {case:?}: a token this read never issued is refused as that token, got {status} \
         with {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        invalid_token(token),
        "case {case:?}: expected the sentence quoting the token, got {message:?} in {body}"
    );
    let reported = headers
        .get(QUERY_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        reported, TOKEN_REPORT,
        "case {case:?}: expected {QUERY_ERROR_HEADER} to read {TOKEN_REPORT:?}, got {reported:?}"
    );
}

#[tokio::test]
async fn an_alarm_over_a_grouped_statement_is_read_and_one_over_a_single_series_is_refused() {
    let router = build_router();

    // (case, the alarm's kind, the members merged over the static-threshold base request, whether
    //  the alarm has contributors to report).
    let cases: Vec<(&str, AlarmKind, Value, bool)> = vec![
        (
            "a grouped statement ranked from the largest down, limited",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT AVG({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Contributor) \
                 GROUP BY Contributor ORDER BY AVG() DESC LIMIT 10"
            )),
            true,
        ),
        (
            "the same ranking on the largest observation, with no limit",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT MAX({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Contributor) \
                 GROUP BY Contributor ORDER BY MAX() DESC"
            )),
            true,
        ),
        (
            "a grouped statement whose metric is quoted",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT AVG(\"{BASE_METRIC}\") FROM SCHEMA(\"{BASE_NAMESPACE}\", Host) \
                 GROUP BY Host ORDER BY AVG() DESC LIMIT 10"
            )),
            true,
        ),
        (
            "a grouped statement reading the whole namespace",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT SUM({BASE_METRIC}) FROM \"{BASE_NAMESPACE}\" GROUP BY Service"
            )),
            true,
        ),
        (
            "a statement grouping by two labels, ranked upwards",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT MIN({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Service, Shard) \
                 GROUP BY Service, Shard ORDER BY MIN() ASC LIMIT 3"
            )),
            true,
        ),
        (
            "a counting statement, grouped and limited, written in lower case",
            AlarmKind::Metric,
            watches(&format!(
                "select count({BASE_METRIC}) from schema(\"{BASE_NAMESPACE}\", Service) \
                 group by Service order by count() desc limit 1"
            )),
            true,
        ),
        (
            // Which query the alarm grades is the one that RETURNS data, so the grouped statement
            // is still what it watches when another query of the array sits in front of it.
            "a grouped statement behind a query the alarm does not grade",
            AlarmKind::Metric,
            metrics_patch(json!([
                metric_stat_query("fed", false),
                expression_query(
                    "watched",
                    &format!(
                        "SELECT AVG({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Contributor) \
                         GROUP BY Contributor ORDER BY AVG() DESC LIMIT 5"
                    ),
                    true,
                ),
            ])),
            true,
        ),
        (
            "an alarm on the one metric its own members name",
            AlarmKind::Metric,
            json!({}),
            false,
        ),
        (
            "an alarm on the one query of a `Metrics` array it grades",
            AlarmKind::Metric,
            metrics_patch(json!([metric_stat_query("watched", true)])),
            false,
        ),
        (
            "an alarm on a statement that groups nothing",
            AlarmKind::Metric,
            watches(&format!(
                "SELECT AVG({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Contributor) \
                 ORDER BY AVG() DESC LIMIT 10"
            )),
            false,
        ),
        (
            "an alarm whose expression folds a grouped statement to one series",
            AlarmKind::Metric,
            metrics_patch(json!([
                expression_query(
                    "grouped",
                    &format!(
                        "SELECT AVG({BASE_METRIC}) FROM SCHEMA(\"{BASE_NAMESPACE}\", Contributor) \
                         GROUP BY Contributor"
                    ),
                    false,
                ),
                expression_query("watched", "MAX(grouped)", true),
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
    ];

    for (index, (case, kind, patch, reports)) in cases.into_iter().enumerate() {
        let name = format!("grouped-gate-{index}");
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

        let (status, headers, body) = call_in_query_mode(
            &router,
            "DescribeAlarmContributors",
            &json!({"AlarmName": name}),
        )
        .await;
        if reports {
            assert_eq!(
                status, 200,
                "case {case:?}: an alarm graded on a vector of series is read, got {status} \
                 with {body}"
            );
            // Nothing has been published for these alarms, so no group of the statement's result
            // is in ALARM and the page is empty — an empty page, never a refusal.
            assert_eq!(
                body.get("AlarmContributors"),
                Some(&json!([])),
                "case {case:?}: no group of the statement is in ALARM yet, got {body}"
            );
            assert!(
                headers.get(QUERY_ERROR_HEADER).is_none(),
                "case {case:?}: a served answer names no error class, got {:?}",
                headers.get(QUERY_ERROR_HEADER)
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: an alarm graded on one series is refused for its type, got \
                 {status} with {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                not_multi_time_series(&name),
                "case {case:?}: expected the service's own sentence, got {message:?} in {body}"
            );
            let reported = headers
                .get(QUERY_ERROR_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            assert_eq!(
                reported, VALIDATION_REPORT,
                "case {case:?}: expected {QUERY_ERROR_HEADER} to read {VALIDATION_REPORT:?}, \
                 got {reported:?}"
            );
        }

        // Whichever of the two the alarm is, a token this read never issued is refused as that
        // token: the pagination contract is judged independently of the alarm's kind.
        assert_token_refused(&router, case, &name, "not-a-real-token").await;
    }

    // A name no alarm carries is the third class, and it is answered as such — with the token
    // still judged first.
    let absent = "grouped-gate-absent";
    let (status, body) = call(
        &router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": absent}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"a name no alarm carries\": expected HTTP 404, got {status} with {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        format!("No alarm for name {absent} was found"),
        "case \"a name no alarm carries\": expected the service's own sentence, got {message:?}"
    );
    assert_token_refused(&router, "a token against a name no alarm carries", absent, "").await;
}

/// The namespace the second test publishes its contributing series into — its own, so what the
/// statements below select is exactly what this test published.
const CONTRIBUTOR_NAMESPACE: &str = "Custom/GroupedContributors";

/// The metric those series carry.
const CONTRIBUTOR_METRIC: &str = "Load";

/// The label the statements group by, which is what identifies a contributor.
const CONTRIBUTOR_LABEL: &str = "Contributor";

/// The threshold every group of the result is graded against.
const THRESHOLD: f64 = 100.0;

/// An alarm watching `expression`, graded against [`THRESHOLD`] over one period.
fn grouped_alarm(name: &str, expression: &str) -> Value {
    json!({
        "AlarmName": name,
        "Metrics": [expression_query("watched", expression, true)],
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "notBreaching",
    })
}

#[tokio::test]
async fn the_contributors_of_a_grouped_statement_are_its_breaching_groups_in_its_own_order() {
    let router = build_router();
    wait_for_period_room(MINUTE, PERIOD_ROOM).await;
    let at = graded_window_stamp(MINUTE);

    // Three series of one metric under the label the statements group by: two past the threshold
    // and one under it, so what the read reports is the groups in ALARM rather than every group.
    for (contributor, value) in [("east", 250.0), ("west", 300.0), ("south", 50.0)] {
        publish(
            &router,
            CONTRIBUTOR_NAMESPACE,
            CONTRIBUTOR_METRIC,
            (CONTRIBUTOR_LABEL, contributor),
            at,
            value,
        )
        .await;
    }

    // (case, the statement the alarm watches, the values of the label its contributors carry, in
    //  the order the answer must list them). The rows differ along the direction the statement
    //  ranks its groups in.
    let rows: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "a statement ranked from the largest average down",
            format!(
                "SELECT AVG({CONTRIBUTOR_METRIC}) FROM \
                 SCHEMA(\"{CONTRIBUTOR_NAMESPACE}\", {CONTRIBUTOR_LABEL}) \
                 GROUP BY {CONTRIBUTOR_LABEL} ORDER BY AVG() DESC LIMIT 10"
            ),
            vec!["west", "east"],
        ),
        (
            "the same statement ranked upwards",
            format!(
                "SELECT AVG({CONTRIBUTOR_METRIC}) FROM \
                 SCHEMA(\"{CONTRIBUTOR_NAMESPACE}\", {CONTRIBUTOR_LABEL}) \
                 GROUP BY {CONTRIBUTOR_LABEL} ORDER BY AVG() ASC LIMIT 10"
            ),
            vec!["east", "west"],
        ),
    ];

    for (index, (case, expression, expected)) in rows.into_iter().enumerate() {
        let name = format!("grouped-contributors-{index}");
        ok(&router, "PutMetricAlarm", &grouped_alarm(&name, &expression)).await;

        let (reported, body) = until_contributors(&router, &name, expected.len()).await;
        let labels: Vec<String> = reported
            .iter()
            .map(|(attributes, _)| {
                attributes
                    .get(CONTRIBUTOR_LABEL)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            labels, expected,
            "case {case:?}: expected the contributors {expected:?} in that order, got \
             {reported:?} from {body}"
        );
        for (attributes, id) in &reported {
            // "A map of attributes that describe the contributor, such as metric dimensions and
            // other identifying characteristics." (`AlarmContributor$ContributorAttributes`) — the
            // one label that identifies the group, and nothing beside it.
            assert_eq!(
                attributes.as_object().map(serde_json::Map::len),
                Some(1),
                "case {case:?}: a contributor is described by the label identifying it, got \
                 {attributes}"
            );
            // The identifier fits `ContributorId`'s modelled 1..16 length.
            assert!(
                (1..=16).contains(&id.chars().count()),
                "case {case:?}: expected an id of 1 to 16 characters, got {id:?}"
            );
        }

        // A read changes nothing a later caller sees: the same entries come back, in the same
        // order and under the same ids.
        let (again, body) = contributors(&router, &name).await;
        assert_eq!(
            again, reported,
            "case {case:?}: a repeat read must answer the same contributors, got {again:?} \
             from {body}"
        );

        // The group under the threshold is in none of the answers: the read reports the groups
        // whose own state is ALARM.
        assert!(
            !labels.iter().any(|label| label == "south"),
            "case {case:?}: a group under the threshold is no contributor, got {labels:?}"
        );
    }
}
