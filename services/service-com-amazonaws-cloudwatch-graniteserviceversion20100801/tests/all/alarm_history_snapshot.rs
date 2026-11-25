// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ALARM SNAPSHOT an alarm-history document records, and for the
//! periods an alarm's decision record names.
//!
//! Two rules, each asserted over a table of inputs rather than over one request:
//!
//! 1. A configuration history item records the WHOLE alarm the write left behind — the
//!    configuration it stored, the instant that configuration was updated at and the state the
//!    alarm is standing in — in the alarm-history serialisation's own spelling, which writes the
//!    OK action list all in lower case (`okactions`) and states a dimension's value before its
//!    name. An item for a write that REPLACED an alarm records the delta instead of a second
//!    snapshot: the original value of every member the write changed, and nothing it left alone.
//!    `HistoryData` is "Data about the alarm, in JSON format."
//!    (`smithy_query(query="shape",shape="AlarmHistoryItem")`), so none of this is fixed by the
//!    model: what a client parses is the member names and the nesting, which is what the rows
//!    below assert while varying the alarm's own members.
//!
//! 2. A decision record ("An explanation for the alarm state, in JSON format.",
//!    `MetricAlarm$StateReasonData`) names every period its verdict rests on by the INSTANT of
//!    that period, and reports what was measured over it — the value the statistic took and how
//!    many observations it held — only where the series held a datapoint for it: a period a
//!    missing-data arm stood in for was measured over by nothing, so its entry names the period
//!    and nothing else. The same record reaches a client through every alarm reader.

use crate::harness::{
    build_router, document_instant_secs, graded_window_stamp, merged, metric_alarm_request, ok,
    reported_creation_id, LOOK_INTERVAL, WORK_WAIT,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::time::Instant;

/// How far apart two readings of one instant may be and still be the same instant: the
/// millisecond the document form writes.
const INSTANT_TOLERANCE_SECS: f64 = 0.001;

/// The state a write that CREATES a metric alarm leaves it in: "When this operation creates an
/// alarm, the alarm state is immediately set to INSUFFICIENT_DATA." (`PutMetricAlarm`).
const INITIAL_STATE: &str = "INSUFFICIENT_DATA";

/// The configuration-update items of `name`, oldest first.
async fn configuration_items(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let body = ok(
        router,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "HistoryItemType": "ConfigurationUpdate",
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    body.get("AlarmHistoryItems")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One item's `HistoryData`, both as the text a client receives and as the document it parses.
fn history_document(case: &str, item: &Value) -> (String, Value) {
    let raw = item
        .get("HistoryData")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected HistoryData, got {item}"))
        .to_string();
    let document = serde_json::from_str::<Value>(&raw)
        .unwrap_or_else(|error| panic!("case {case:?}: HistoryData is not JSON: {error}"));
    (raw, document)
}

/// The metric alarm `name` as `DescribeAlarms` reports it.
async fn described(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    body.pointer("/MetricAlarms/0")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Assert that `snapshot` states `member` in the form a document writes an instant in, and that
/// the instant it names is `expected`.
fn assert_document_instant(case: &str, snapshot: &Value, member: &str, expected: f64) {
    let text = snapshot
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} in {snapshot}"));
    let secs = document_instant_secs(text).unwrap_or_else(|| {
        panic!(
            "case {case:?}: {member} must be written as a document instant \
             (yyyy-MM-dd'T'HH:mm:ss.SSS+0000), got {text:?}"
        )
    });
    assert!(
        (secs - expected).abs() <= INSTANT_TOLERANCE_SECS,
        "case {case:?}: {member} must be the instant the alarm reports ({expected}), got {secs}"
    );
}

#[tokio::test]
async fn a_configuration_item_states_the_whole_alarm_the_write_left_behind() {
    let router = build_router();

    // Rows differ along the members the alarm carries: a bare alarm, a percentile alarm with
    // dimensions and actions, an alarm with every action list, and one over a metric-math
    // expression that names no single metric at all.
    let rows: Vec<(&str, Value)> = vec![
        ("only the required members", json!({})),
        (
            "a percentile alarm with dimensions and an OK action",
            json!({
                "Statistic": null,
                "ExtendedStatistic": "p90",
                "ActionsEnabled": false,
                "TreatMissingData": "breaching",
                "OKActions": ["arn:aws:sns:us-east-1:000000000000:all-clear"],
                "Dimensions": [
                    {"Name": "Camera_name", "Value": "ALL"},
                    {"Name": "Operation", "Value": "FrameQuartzLag"},
                ],
            }),
        ),
        (
            "every action list and a description",
            json!({
                "AlarmDescription": "watches the queue",
                "OKActions": ["arn:aws:sns:us-east-1:000000000000:clear"],
                "AlarmActions": ["arn:aws:sns:us-east-1:000000000000:page"],
                "InsufficientDataActions": ["arn:aws:sns:us-east-1:000000000000:unknown"],
                "Unit": "Milliseconds",
                "DatapointsToAlarm": 1,
            }),
        ),
        (
            "a metric-math alarm",
            json!({
                "Namespace": null,
                "MetricName": null,
                "Statistic": null,
                "Period": null,
                "Metrics": [{
                    "Id": "m1",
                    "Expression": "1 + 1",
                    "ReturnData": true,
                }],
            }),
        ),
    ];

    for (index, (case, patch)) in rows.iter().enumerate() {
        let name = format!("snapshot-{index}");
        let request = merged(metric_alarm_request(&name), patch);
        ok(&router, "PutMetricAlarm", &request).await;

        let items = configuration_items(&router, &name).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: one write records one configuration item, got {items:?}"
        );
        assert_eq!(
            items[0].get("HistorySummary").and_then(Value::as_str),
            Some(format!("Alarm \"{name}\" created").as_str()),
            "case {case:?}: a created alarm's summary names it, got {:?}",
            items[0]
        );
        let (raw, document) = history_document(case, &items[0]);
        assert_eq!(
            document.get("type").and_then(Value::as_str),
            Some("Create"),
            "case {case:?}: a first write created the alarm, got {document}"
        );
        let snapshot = document
            .get("createdAlarm")
            .cloned()
            .unwrap_or(Value::Null);

        // The instant the configuration was updated at is the one the alarm itself reports: a
        // client reads the configuration's age out of the item without a second call.
        let alarm = described(&router, &name).await;
        let configured = alarm
            .get("AlarmConfigurationUpdatedTimestamp")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a configuration instant, {alarm}"));
        assert_document_instant(
            case,
            &snapshot,
            "alarmConfigurationUpdatedTimestamp",
            configured,
        );
        // The state the write left the alarm standing in, and when it last reported it — a write
        // that CREATES an alarm sets both at once.
        assert_eq!(
            snapshot.get("stateValue").and_then(Value::as_str),
            Some(INITIAL_STATE),
            "case {case:?}: the snapshot states the state the write left, got {snapshot}"
        );
        assert_document_instant(case, &snapshot, "stateUpdatedTimestamp", configured);

        // The OK action list is spelled all in lower case, and under no other name.
        assert!(
            snapshot.get("okActions").is_none(),
            "case {case:?}: the OK list is spelled okactions, got {snapshot}"
        );
        let asked_ok_actions = request.get("OKActions").cloned();
        if let Some(asked) = asked_ok_actions {
            assert_eq!(
                snapshot.get("okactions"),
                Some(&asked),
                "case {case:?}: expected okactions {asked}, got {snapshot}"
            );
        }

        // A dimension states its VALUE before its name, which is what a consumer reading the
        // document's text sees.
        for dimension in request
            .get("Dimensions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let (dimension_name, value) = (
                dimension.get("Name").and_then(Value::as_str).unwrap_or(""),
                dimension.get("Value").and_then(Value::as_str).unwrap_or(""),
            );
            let expected = format!(r#"{{"value":"{value}","name":"{dimension_name}"}}"#);
            assert!(
                raw.contains(&expected),
                "case {case:?}: expected the dimension as {expected}, got {raw}"
            );
        }
    }
}

/// One row of the update table: what the second write changes, and the members the item must
/// report as changed beside the configuration instant every write moves on.
struct Change {
    /// The case this row is.
    case: &'static str,
    /// The patch the second write applies to the first request.
    patch: Value,
    /// The document members whose original value the item must report.
    changed: &'static [&'static str],
}

#[tokio::test]
async fn an_update_item_states_the_original_values_of_the_members_it_changed() {
    let router = build_router();

    // Rows differ in WHAT the second write changes: one number, the comparison, a statistic
    // swapped for a percentile, an action list, a dimension set — and a re-stated
    // configuration, which changes nothing but the instant of the write itself.
    let rows = [
        Change {
            case: "a changed threshold",
            patch: json!({"Threshold": 42.5}),
            changed: &["threshold"],
        },
        Change {
            case: "a changed comparison",
            patch: json!({"ComparisonOperator": "LessThanThreshold"}),
            changed: &["comparisonOperator"],
        },
        Change {
            case: "a statistic replaced by a percentile",
            patch: json!({"Statistic": null, "ExtendedStatistic": "p90"}),
            changed: &["statistic", "extendedStatistic"],
        },
        Change {
            case: "a changed period and evaluation count",
            patch: json!({"Period": 300, "EvaluationPeriods": 3}),
            changed: &["period", "evaluationPeriods"],
        },
        Change {
            case: "an alarm action added",
            patch: json!({"AlarmActions": ["arn:aws:sns:us-east-1:000000000000:page"]}),
            changed: &["alarmActions"],
        },
        Change {
            case: "a dimension set added",
            patch: json!({"Dimensions": [{"Name": "Stage", "Value": "prod"}]}),
            changed: &["dimensions"],
        },
        Change {
            case: "the same configuration written again",
            patch: json!({}),
            changed: &[],
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("delta-{index}");
        let first = metric_alarm_request(&name);
        ok(&router, "PutMetricAlarm", &first).await;
        let created = history_document(case, &configuration_items(&router, &name).await[0])
            .1
            .get("createdAlarm")
            .cloned()
            .unwrap_or(Value::Null);

        ok(&router, "PutMetricAlarm", &merged(first, &row.patch)).await;
        let items = configuration_items(&router, &name).await;
        assert_eq!(
            items.len(),
            2,
            "case {case:?}: two writes record two items, got {items:?}"
        );
        assert_eq!(
            items[1].get("HistorySummary").and_then(Value::as_str),
            Some(format!("Alarm \"{name}\" updated").as_str()),
            "case {case:?}: an updated alarm's summary names it, got {:?}",
            items[1]
        );
        let (_, document) = history_document(case, &items[1]);
        assert_eq!(
            document.get("type").and_then(Value::as_str),
            Some("Update"),
            "case {case:?}: overwriting an alarm records an update, got {document}"
        );
        assert!(
            document.get("originalAlarm").is_none(),
            "case {case:?}: an update records the delta, not a second snapshot, got {document}"
        );
        let delta = document
            .get("originalUpdatedFields")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected originalUpdatedFields, got {document}")
            });

        // Exactly the members the write changed, plus the instant the configuration was updated
        // at, which every write moves on.
        let mut reported: Vec<&str> = delta.keys().map(String::as_str).collect();
        reported.sort_unstable();
        let mut expected: Vec<&str> = row.changed.to_vec();
        expected.push("alarmConfigurationUpdatedTimestamp");
        expected.sort_unstable();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the changed members {expected:?}, got {document}"
        );
        // …each carrying the value the alarm the write REPLACED had for it, and nothing about
        // the state, which an update leaves alone.
        for member in row.changed {
            let before = created.get(*member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                delta.get(*member),
                Some(&before),
                "case {case:?}: {member} must be reported with its original value {before}, got \
                 {document}"
            );
        }
        for untouched in ["stateValue", "stateUpdatedTimestamp", "alarmName"] {
            assert!(
                !delta.contains_key(untouched),
                "case {case:?}: an update changes no {untouched}, got {document}"
            );
        }
    }
}

/// The namespace the decision-record rows publish into and watch.
const DECISION_NAMESPACE: &str = "Custom/DecisionRecord";

/// The period every decision-record row evaluates, in seconds.
const DECISION_PERIOD: i64 = 60;

/// Wait until `name` reports `wanted`, looking again until it does or a generous deadline has
/// passed — never for a fixed time, so the wait does not depend on how fast the host runs.
async fn until_state(router: &MultiServiceRouter, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let alarm = described(router, name).await;
        let reported = alarm.get("StateValue").and_then(Value::as_str);
        if reported == Some(wanted) || Instant::now() >= deadline {
            assert_eq!(
                reported,
                Some(wanted),
                "expected {name:?} to reach {wanted}, got {alarm}"
            );
            return alarm;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The decision record of `alarm`, parsed: it is "an explanation for the alarm state, in JSON
/// format", so a consumer reads it as a document.
fn decision_record(case: &str, alarm: &Value) -> Value {
    let raw = alarm
        .get("StateReasonData")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected StateReasonData, got {alarm}"));
    serde_json::from_str(raw)
        .unwrap_or_else(|error| panic!("case {case:?}: StateReasonData is not JSON: {error}"))
}

/// The alarms `DescribeAlarmsForMetric` reports for the row `metric`.
async fn for_metric(router: &MultiServiceRouter, metric: &str) -> Value {
    let body = ok(
        router,
        "DescribeAlarmsForMetric",
        &json!({
            "Namespace": DECISION_NAMESPACE,
            "MetricName": metric,
            "Statistic": "Maximum",
            "Period": DECISION_PERIOD,
        }),
    )
    .await;
    body.pointer("/MetricAlarms/0")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The alarm one decision-record row configures: it watches a series of its own, so the rows
/// share one emulator without sharing data.
fn decision_alarm(name: &str, metric: &str, treat_missing: &str) -> Value {
    merged(
        metric_alarm_request(name),
        &json!({
            "Namespace": DECISION_NAMESPACE,
            "MetricName": metric,
            "Statistic": "Maximum",
            "Period": DECISION_PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 10.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": treat_missing,
            "ActionsEnabled": false,
        }),
    )
}

#[tokio::test]
async fn a_decision_record_names_the_periods_its_verdict_rests_on() {
    let router = build_router();

    // Two rows along what the look-back HELD: a period with a measured datapoint, and a
    // look-back with nothing in it at all, whose state the missing-data arm decides.
    let measured_metric = "Measured";
    let empty_metric = "Unmeasured";
    ok(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": DECISION_NAMESPACE,
            "MetricData": [{
                "MetricName": measured_metric,
                "Value": 99.0,
                "Timestamp": graded_window_stamp(DECISION_PERIOD as f64),
            }],
        }),
    )
    .await;
    ok(
        &router,
        "PutMetricAlarm",
        &decision_alarm("record-measured", measured_metric, "missing"),
    )
    .await;
    ok(
        &router,
        "PutMetricAlarm",
        &decision_alarm("record-empty", empty_metric, "breaching"),
    )
    .await;

    // The measured row: the record names the period it graded, with what was measured over it.
    let case = "a period with a measured datapoint";
    let alarm = until_state(&router, "record-measured", "ALARM").await;
    let record = decision_record(case, &alarm);
    assert_eq!(
        record.get("period").and_then(Value::as_i64),
        Some(DECISION_PERIOD),
        "case {case:?}: a period is a whole number of seconds, got {record}"
    );
    assert!(
        record.get("startDate").and_then(Value::as_str).is_some(),
        "case {case:?}: a record that graded the series opens the span it read, got {record}"
    );
    let graded = record
        .get("evaluatedDatapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !graded.is_empty(),
        "case {case:?}: the record names the datapoint the verdict rests on, got {record}"
    );
    for entry in &graded {
        assert!(
            entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(document_instant_secs)
                .is_some(),
            "case {case:?}: an evaluated datapoint names its period, got {entry}"
        );
        assert_eq!(
            entry.get("value").and_then(Value::as_f64),
            Some(99.0),
            "case {case:?}: an evaluated datapoint reports what was measured, got {entry}"
        );
        assert!(
            entry.get("datapoint").is_none(),
            "case {case:?}: a measured datapoint reports no missing-data arm, got {entry}"
        );
    }
    assert_eq!(
        record.get("recentDatapoints").and_then(Value::as_array),
        Some(&vec![json!(99.0)]),
        "case {case:?}: the window the state was decided over is reported, got {record}"
    );

    // The empty row: the arm substituted a datapoint for the one period of the look-back, so
    // the record names that period and reports no measurement of it — and no entry stands in
    // for a period with the name of the arm that decided it.
    let case = "a look-back with no datapoint at all";
    let alarm = until_state(&router, "record-empty", "ALARM").await;
    let record = decision_record(case, &alarm);
    assert_eq!(
        record.get("recentDatapoints").and_then(Value::as_array),
        Some(&Vec::new()),
        "case {case:?}: an empty look-back reports an empty window, got {record}"
    );
    let substituted = record
        .get("evaluatedDatapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        substituted.len(),
        1,
        "case {case:?}: the verdict rests on the one period the arm stood in for, got {record}"
    );
    for entry in &substituted {
        assert!(
            entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(document_instant_secs)
                .is_some(),
            "case {case:?}: a substituted period is named by its own instant, got {entry}"
        );
        assert!(
            entry.get("value").is_none() && entry.get("sampleCount").is_none(),
            "case {case:?}: nothing was measured over that period, got {entry}"
        );
        assert!(
            entry.get("datapoint").is_none(),
            "case {case:?}: an entry reports a period, not the arm that decided it, got {entry}"
        );
    }
    assert!(
        record.get("queryDate").and_then(Value::as_str).is_some(),
        "case {case:?}: the record names the instant the query ran, got {record}"
    );

    // The same record, and the creation identity beside it, reach a client through the
    // per-metric reader too: one serialiser answers both reads.
    for (case, name, metric) in [
        ("a period with a measured datapoint", "record-measured", measured_metric),
        ("a look-back with no datapoint at all", "record-empty", empty_metric),
    ] {
        let general = described(&router, name).await;
        let per_metric = for_metric(&router, metric).await;
        assert_eq!(
            per_metric.get("StateReasonData"),
            general.get("StateReasonData"),
            "case {case:?}: both readers report the same decision record, got {per_metric}"
        );
        assert!(
            reported_creation_id(&per_metric).is_some_and(|id| id.contains('/')),
            "case {case:?}: the per-metric reader reports the creation identity, got {per_metric}"
        );
    }
}
