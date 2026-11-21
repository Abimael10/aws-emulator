// SPDX-License-Identifier: CC0-1.0

//! The METRIC SELECTION a `CloudWatch Alarm Configuration Change` event reports, end to end over
//! the wire through the real `MultiServiceRouter` with CloudWatch, EventBridge and Amazon SQS on
//! one engine — so what a client sees is what is asserted: the envelope on the queue a matching
//! rule targets.
//!
//! The RULE asserted here: **an alarm event's `configuration` states WHAT the alarm evaluates as
//! `metrics`, the list of metric-data queries the alarm holds — one entry per query of an alarm
//! configured with a `Metrics` array, the ONE query its metric IS for an alarm configured with a
//! single metric, and no `metrics` at all for an alarm that evaluates no metric — each entry
//! naming its metric by `namespace`, `name` and its dimensions, beside the period it is
//! aggregated over and the statistic it is aggregated with.** "One item in the Metrics array is
//! the expression that the alarm watches" (`PutMetricAlarmInput$Metrics`) is the selection a
//! query-array alarm states for itself; an alarm configured with `Namespace`, `MetricName`,
//! `Statistic` and `Period` states the same selection in the single-metric form, so the event
//! reports it as the one query that metric is and a consumer reads one shape either way. The id
//! of a query the alarm itself did not name is CloudWatch's own, so it is asserted as an
//! identifier that is there and belongs to that one alarm, never as a literal.
//!
//! The rows differ along what the rule generalises over: HOW the alarm's metric selection is
//! configured (a single metric, a single metric in a unit under an extended statistic, a
//! `Metrics` array, a query language), how many queries the selection holds, how many dimensions
//! identify the metric, and the period and statistic it is aggregated with.
//!
//! A second test takes the same rule over a SEQUENCE of writes to one alarm: each write's event
//! reports the selection that write left beside the one it replaced, and the alarm's own query
//! keeps one id across all of them. A third takes the BOUNDARY the rendering draws: whether a
//! write changed the configuration is decided on what the client wrote, so a write that only
//! states a member the rendering would have defaulted is still an update.

use serde_json::{json, Value};

use crate::alarm_configuration_change_events::{
    alarm_arn, assert_envelope, emulator_with_a_bus, eventbridge, given_target_queue, monitoring,
    queue_arn_of, until_delivered, CREATE, DETAIL_TYPE, SOURCE, UPDATE,
};
use crate::harness::{merged, Emulator};

/// The namespace and metric the alarms below watch.
const NAMESPACE: &str = "Custom/MetricSelection";
const METRIC: &str = "Latency";

/// The period the base request aggregates over, as the modelled seconds member carries it.
const PERIOD: i64 = 60;

/// One row: an alarm written one way, and the metric selection the event it publishes must
/// report.
struct Row {
    /// What this row is about.
    case: &'static str,
    /// The alarm this row writes, under its own name so one row's event is never another's.
    alarm: &'static str,
    /// The members that replace the single-metric form of the base request.
    patch: Value,
    /// The `metrics` the event's configuration must report, and `None` for an alarm that
    /// evaluates no metric at all and so reports no metric queries. An entry carrying no `id` is
    /// one the alarm did not name: the id CloudWatch generated for it is asserted separately.
    metrics: Option<Value>,
}

/// The base metric alarm request of this suite, with `patch` merged over it.
fn metric_alarm(name: &str, patch: &Value) -> Value {
    merged(
        json!({
            "AlarmName": name,
            "AlarmDescription": "the alarm of this row",
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "QueueName", "Value": name}],
            "Statistic": "Average",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "ignore",
        }),
        patch,
    )
}

/// The shapes of metric selection this suite spans.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single metric aggregated by a statistic",
            alarm: "selection-single-metric",
            patch: json!({}),
            metrics: Some(json!([{
                "metricStat": {
                    "metric": {
                        "namespace": NAMESPACE,
                        "name": METRIC,
                        "dimensions": {"QueueName": "selection-single-metric"},
                    },
                    "period": PERIOD,
                    "stat": "Average",
                },
                "returnData": true,
            }])),
        },
        Row {
            case: "a single metric aggregated by an extended statistic, in a unit, with no \
                   dimensions",
            alarm: "selection-extended-statistic",
            patch: json!({
                "Statistic": null,
                "ExtendedStatistic": "p95",
                "Unit": "Milliseconds",
                "Dimensions": [],
            }),
            metrics: Some(json!([{
                "metricStat": {
                    "metric": {
                        "namespace": NAMESPACE,
                        "name": METRIC,
                        "dimensions": {},
                    },
                    "period": PERIOD,
                    "stat": "p95",
                    "unit": "Milliseconds",
                },
                "returnData": true,
            }])),
        },
        Row {
            case: "a single metric identified by two dimensions, aggregated over a longer period",
            alarm: "selection-two-dimensions",
            patch: json!({
                "Statistic": "Sum",
                "Period": 300,
                "Dimensions": [
                    {"Name": "QueueName", "Value": "selection-two-dimensions"},
                    {"Name": "Stage", "Value": "prod"},
                ],
            }),
            metrics: Some(json!([{
                "metricStat": {
                    "metric": {
                        "namespace": NAMESPACE,
                        "name": METRIC,
                        "dimensions": {
                            "QueueName": "selection-two-dimensions",
                            "Stage": "prod",
                        },
                    },
                    "period": 300,
                    "stat": "Sum",
                },
                "returnData": true,
            }])),
        },
        Row {
            case: "a query array whose two queries the caller named",
            alarm: "selection-query-array",
            // "If you use the Metrics parameter, you cannot include the Namespace, MetricName,
            // Dimensions, Period, Unit, Statistic, or ExtendedStatistic parameters of
            // PutMetricAlarm in the same operation" (`PutMetricAlarmInput$Metrics`).
            patch: json!({
                "Namespace": null,
                "MetricName": null,
                "Statistic": null,
                "Period": null,
                "Dimensions": null,
                "Metrics": [
                    {
                        "Id": "m1",
                        "MetricStat": {
                            "Metric": {
                                "Namespace": NAMESPACE,
                                "MetricName": METRIC,
                                "Dimensions": [
                                    {"Name": "QueueName", "Value": "selection-query-array"},
                                ],
                            },
                            "Period": PERIOD,
                            "Stat": "Average",
                        },
                        "ReturnData": true,
                    },
                    {"Id": "e1", "Expression": "m1 * 2", "Label": "doubled", "ReturnData": false},
                ],
            }),
            metrics: Some(json!([
                {
                    "id": "m1",
                    "metricStat": {
                        "metric": {
                            "namespace": NAMESPACE,
                            "name": METRIC,
                            "dimensions": {"QueueName": "selection-query-array"},
                        },
                        "period": PERIOD,
                        "stat": "Average",
                    },
                    "returnData": true,
                },
                {"id": "e1", "expression": "m1 * 2", "label": "doubled", "returnData": false},
            ])),
        },
        Row {
            case: "an alarm that evaluates a query language instead of a metric",
            alarm: "selection-query-language",
            // Such an alarm names no metric, no threshold and no comparison: the query itself
            // decides its contributors (`AlarmPromQLCriteria$Query`).
            patch: json!({
                "Namespace": null,
                "MetricName": null,
                "Statistic": null,
                "Period": null,
                "Dimensions": null,
                "EvaluationPeriods": null,
                "Threshold": null,
                "ComparisonOperator": null,
                "EvaluationCriteria": {"PromQLCriteria": {"Query": "avg_over_time(latency[5m])"}},
            }),
            metrics: None,
        },
    ]
}

/// Wire a rule matching EVERY alarm configuration change of this account to the queue at `url`:
/// one queue holds what each row published, so a row whose event never arrives is a missing
/// event rather than a rule that matched nothing.
async fn given_rule_for_configuration_changes(
    emulator: &Emulator,
    case: &str,
    rule_name: &str,
    queue_url: &str,
) {
    let pattern = json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}).to_string();
    eventbridge(
        emulator,
        case,
        "PutRule",
        &json!({"Name": rule_name, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let queue_arn = queue_arn_of(emulator, queue_url).await;
    let wired = eventbridge(
        emulator,
        case,
        "PutTargets",
        &json!({"Rule": rule_name, "Targets": [{"Id": "q", "Arn": queue_arn}]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );
}

/// The event among `held` reporting the configuration change of the alarm `alarm`.
fn event_of<'a>(case: &str, held: &'a [Value], alarm: &str) -> &'a Value {
    held.iter()
        .find(|event| event.pointer("/detail/alarmName") == Some(&json!(alarm)))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a configuration change event for the alarm {alarm:?}, \
                 got {held:?}"
            )
        })
}

/// The queries `document` reports, ready to compare against `expected`, and the ids CloudWatch
/// GENERATED among them.
///
/// An expected entry that names no `id` is a query the alarm did not name, so the id the event
/// reports for it is CloudWatch's own: it is asserted to be there and then taken out of the
/// comparison, because its value is minted rather than written by the caller.
fn comparable_queries(
    case: &str,
    document: Option<&Value>,
    expected: &[Value],
) -> (Vec<Value>, Vec<String>) {
    let reported = document
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut comparable = Vec::new();
    let mut generated = Vec::new();
    for (index, entry) in reported.iter().enumerate() {
        let mut entry = entry.clone();
        let caller_named_it = expected
            .get(index)
            .is_some_and(|expected| expected.get("id").is_some());
        if !caller_named_it {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                !id.is_empty(),
                "case {case:?}: expected the query CloudWatch reports for the alarm's own metric \
                 to carry an id, got {entry}"
            );
            generated.push(id);
            if let Some(members) = entry.as_object_mut() {
                members.remove("id");
            }
        }
        comparable.push(entry);
    }
    (comparable, generated)
}

/// The period the first query of the selection at `pointer` is aggregated over.
fn query_period(event: &Value, pointer: &str) -> Option<i64> {
    event
        .pointer(&format!("{pointer}/0/metricStat/period"))
        .and_then(Value::as_i64)
}

/// The id of the first query of the selection at `pointer`.
fn query_id(event: &Value, pointer: &str) -> Option<String> {
    event
        .pointer(&format!("{pointer}/0/id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// RULE: the configuration an alarm event reports states the alarm's metric selection as the
/// list of queries the alarm evaluates, whichever way that selection was configured.
#[tokio::test]
async fn every_alarm_shape_reports_the_metric_selection_it_evaluates() {
    let case = "the metric selection a configuration change reports";
    let emulator = emulator_with_a_bus();
    let queue = given_target_queue(&emulator, "metric-selection-q").await;
    // The rule is wired BEFORE any alarm exists, so the event each create publishes is matched
    // by a rule that was already live.
    given_rule_for_configuration_changes(&emulator, case, "metric-selection-rule", &queue).await;

    let rows = rows();
    for row in &rows {
        monitoring(
            &emulator,
            "PutMetricAlarm",
            &metric_alarm(row.alarm, &row.patch),
        )
        .await;
    }

    let mut held: Vec<Value> = Vec::new();
    until_delivered(&emulator, &queue, rows.len(), &mut held).await;
    assert_eq!(
        held.len(),
        rows.len(),
        "case {case:?}: expected one configuration change event per written alarm, got {held:?}"
    );

    let mut generated_ids: Vec<String> = Vec::new();
    for row in &rows {
        let event = event_of(row.case, &held, row.alarm).clone();
        let arn = alarm_arn(&emulator, row.case, "MetricAlarm", row.alarm).await;
        assert_envelope(row.case, &event, row.alarm, &arn, CREATE);
        let reported = event.pointer("/detail/configuration/metrics");
        let Some(expected) = row.metrics.as_ref() else {
            assert_eq!(
                reported, None,
                "case {:?}: expected an alarm that evaluates no metric to report no metric \
                 queries, got {event}",
                row.case
            );
            continue;
        };
        let expected = expected.as_array().cloned().unwrap_or_default();
        let (comparable, generated) = comparable_queries(row.case, reported, &expected);
        assert_eq!(
            comparable, expected,
            "case {:?}: expected the configuration to report the queries {expected:?}, got \
             {comparable:?} in {event}",
            row.case
        );
        generated_ids.extend(generated);
    }

    // A query CloudWatch generated for one alarm's own metric belongs to that alarm: two alarms
    // report two ids, so a consumer correlating one alarm's events is never handed another's.
    let mut distinct = generated_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        generated_ids.len(),
        "case {case:?}: expected every alarm's own query to carry its own id, got \
         {generated_ids:?}"
    );
}

/// RULE, over a SEQUENCE of writes to one alarm: every write that changes the metric selection
/// publishes its own event, reporting the selection THAT write left beside the one it replaced —
/// and the query the alarm's own metric is keeps ONE id across all of them, so a consumer
/// correlating the events of one alarm reads the same query each time.
#[tokio::test]
async fn every_write_reports_the_metric_selection_it_stored_and_the_one_it_replaced() {
    let case = "a re-aggregated alarm's metric selection";
    // The rows are the periods written, in order; they differ along the value the write stores.
    let periods: [i64; 3] = [60, 300, 900];

    let emulator = emulator_with_a_bus();
    let queue = given_target_queue(&emulator, "metric-selection-sequence-q").await;
    given_rule_for_configuration_changes(
        &emulator,
        case,
        "metric-selection-sequence-rule",
        &queue,
    )
    .await;

    let alarm = "selection-re-aggregated";
    for period in periods {
        monitoring(
            &emulator,
            "PutMetricAlarm",
            &metric_alarm(alarm, &json!({"Period": period})),
        )
        .await;
    }

    let mut held: Vec<Value> = Vec::new();
    until_delivered(&emulator, &queue, periods.len(), &mut held).await;
    assert_eq!(
        held.len(),
        periods.len(),
        "case {case:?}: expected one configuration change event per write, got {held:?}"
    );

    for (index, period) in periods.iter().enumerate() {
        let event = held
            .iter()
            .find(|event| query_period(event, "/detail/configuration/metrics") == Some(*period))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}, the write of period {period}: expected an event whose \
                     configuration reports that period, got {held:?}"
                )
            });
        let operation = if index == 0 { CREATE } else { UPDATE };
        assert_eq!(
            event.pointer("/detail/operation"),
            Some(&json!(operation)),
            "case {case:?}, the write of period {period}: expected the {operation:?} operation, \
             got {event}"
        );
        let replaced = query_period(event, "/detail/previousConfiguration/metrics");
        let owed = index.checked_sub(1).map(|previous| periods[previous]);
        assert_eq!(
            replaced, owed,
            "case {case:?}, the write of period {period}: expected the replaced selection to \
             report the period {owed:?}, got {replaced:?} in {event}"
        );
    }

    let ids: Vec<Option<String>> = held
        .iter()
        .map(|event| query_id(event, "/detail/configuration/metrics"))
        .collect();
    let first = ids.first().cloned().unwrap_or_default();
    assert!(
        first.is_some() && ids.iter().all(|id| *id == first),
        "case {case:?}: expected one alarm's own query to keep one id across its writes, got \
         {ids:?}"
    );
}

/// RULE, over the BOUNDARY the normalised selection draws: whether a write changed the
/// configuration is decided on what the client WROTE, not on the selection the event renders —
/// so a write that only states a member the rendering would have defaulted ("If you omit this,
/// the default of true is used", `MetricDataQuery$ReturnData`) is still an update, and publishes
/// its own event.
#[tokio::test]
async fn a_write_that_only_states_a_defaulted_member_is_still_an_update() {
    let case = "a write that states a defaulted member of the selection";
    let emulator = emulator_with_a_bus();
    let queue = given_target_queue(&emulator, "metric-selection-default-q").await;
    given_rule_for_configuration_changes(&emulator, case, "metric-selection-default-rule", &queue)
        .await;

    let alarm = "selection-defaulted-member";
    // The rows are the two writes, in order: the same one query, first with the member omitted
    // and then with the value the omission means stated explicitly.
    let query = |return_data: Option<bool>| {
        let mut query = json!({
            "Id": "m1",
            "MetricStat": {
                "Metric": {
                    "Namespace": NAMESPACE,
                    "MetricName": METRIC,
                    "Dimensions": [{"Name": "QueueName", "Value": alarm}],
                },
                "Period": PERIOD,
                "Stat": "Average",
            },
        });
        if let Some(return_data) = return_data {
            query["ReturnData"] = json!(return_data);
        }
        json!({
            "Namespace": null,
            "MetricName": null,
            "Statistic": null,
            "Period": null,
            "Dimensions": null,
            "Metrics": [query],
        })
    };
    for return_data in [None, Some(true)] {
        monitoring(
            &emulator,
            "PutMetricAlarm",
            &metric_alarm(alarm, &query(return_data)),
        )
        .await;
    }

    let mut held: Vec<Value> = Vec::new();
    until_delivered(&emulator, &queue, 2, &mut held).await;
    let operations: Vec<Value> = held
        .iter()
        .map(|event| {
            event
                .pointer("/detail/operation")
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    assert_eq!(
        operations,
        vec![json!(CREATE), json!(UPDATE)],
        "case {case:?}: expected the create and then the update the second write made, got \
         {held:?}"
    );
    // Both events report the selection the same way: the member the first write omitted is
    // reported with the value the omission means.
    for event in &held {
        let return_data = event.pointer("/detail/configuration/metrics/0/returnData");
        assert_eq!(
            return_data,
            Some(&json!(true)),
            "case {case:?}: expected the query the alarm watches to report that it returns \
             data, got {return_data:?} in {event}"
        );
    }
}
