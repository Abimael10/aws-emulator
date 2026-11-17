// SPDX-License-Identifier: CC0-1.0

//! WHICH datapoints an evaluated metric alarm's `StateReason` names, end to end over the wire.
//!
//! ONE rule, asserted over a table: **the datapoints a state reason cites are the ones on the
//! side of the comparison the state it explains stands for — the sentence states the comparison
//! as that direction reads it ("… `<n>` out of the last `<m>` datapoints `[…]` were greater than
//! the threshold" for `ALARM`, "… were not greater than the threshold" for `OK`), so every value
//! inside the brackets is one the comparison read that way, and there are exactly as many of them
//! as the count the sentence opens with.**
//!
//! The reason is "an explanation for the alarm state, in text format"
//! (`smithy_query(query="shape",shape="MetricAlarm")`, member `StateReason`), and which direction
//! the state stands for is the alarm's own "M out of N" predicate: `DatapointsToAlarm` breaching
//! datapoints put it in `ALARM` ("the number of data points that must be breaching to trigger the
//! alarm",
//! `smithy_query(query="doc",shape="PutMetricAlarmInput",member="DatapointsToAlarm")`) and the
//! complement of that over `EvaluationPeriods` puts it in `OK`. A window that holds datapoints on
//! BOTH sides is therefore the case the rule is about: a datapoint that breached the threshold is
//! no part of the explanation of an `OK` state, which says of every value it names that the
//! comparison did not hold for it.
//!
//! The rows differ along what the rule generalises over: the direction the state was decided in,
//! the comparison operator, how many datapoints lie on each side, the threshold, the statistic
//! that folds a window, the width of the period, and M and N.
//!
//! The machine-readable half of the decision is NOT narrowed: `StateReasonData` reports every
//! datapoint the window held (`recentDatapoints`) beside the ones the verdict rests on, so each
//! row also reads the values it excluded from the sentence back out of the record.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, graded_window_stamp, ok, wait_for_evaluation};

/// The customer namespace every row of the table watches.
const NAMESPACE: &str = "Custom/CitedDatapoints";

/// The metric every row watches.
const METRIC: &str = "Load";

/// One datapoint a row publishes: its value and WHICH period of the alarm it belongs to,
/// counting back from the newest period an evaluation grades.
#[derive(Clone, Copy, Debug)]
struct Placed {
    /// The value published.
    value: f64,
    /// How many periods back from the newest period an evaluation grades its time stamp falls
    /// in.
    periods_back: u32,
}

/// A datapoint of `value` stamped inside the `periods_back`th period back from the newest one an
/// evaluation grades.
const fn placed(value: f64, periods_back: u32) -> Placed {
    Placed {
        value,
        periods_back,
    }
}

/// One row of the table.
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The alarm's `Period`, in seconds.
    period: i32,
    /// The alarm's `Statistic`.
    statistic: &'static str,
    /// The alarm's `ComparisonOperator`.
    comparison: &'static str,
    /// The alarm's `Threshold`.
    threshold: f64,
    /// The alarm's `EvaluationPeriods` — the N.
    evaluation_periods: i32,
    /// The alarm's `DatapointsToAlarm` — the M, where the row names one.
    datapoints_to_alarm: Option<i32>,
    /// The datapoints the row publishes, each placed in a period of its own.
    data: &'static [Placed],
    /// The state the evaluation must put the alarm in.
    state: &'static str,
    /// The values its reason must cite, as the sentence writes them.
    cited: &'static [&'static str],
    /// The values its reason must NOT cite — the ones on the other side of the comparison —
    /// which `StateReasonData` reports all the same.
    not_cited: &'static [&'static str],
}

/// The table: one row per shape the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        // The recorded case: a 3-of-3 window holding two breaching datapoints and one in range
        // is one breaching datapoint short of M, so the alarm is in OK — explained by the
        // datapoint that was NOT above the threshold.
        case: "a 3-of-3 window two breaching datapoints deep, in OK",
        period: 10,
        statistic: "Maximum",
        comparison: "GreaterThanThreshold",
        threshold: 50.0,
        evaluation_periods: 3,
        datapoints_to_alarm: Some(3),
        data: &[placed(90.0, 0), placed(90.0, 1), placed(12.0, 2)],
        state: "OK",
        cited: &["12.0"],
        not_cited: &["90.0"],
    },
    Row {
        // The same window under a 1-of-3 predicate: the breaching datapoint decides it, and the
        // two in range are no part of the explanation.
        case: "a 1-of-3 window with one breaching datapoint, in ALARM",
        period: 10,
        statistic: "Maximum",
        comparison: "GreaterThanThreshold",
        threshold: 50.0,
        evaluation_periods: 3,
        datapoints_to_alarm: Some(1),
        data: &[placed(90.0, 0), placed(12.0, 1), placed(12.0, 2)],
        state: "ALARM",
        cited: &["90.0"],
        not_cited: &["12.0"],
    },
    Row {
        // The comparison running the other way: breaching is BELOW the threshold, and the
        // datapoints above it are the ones left out.
        case: "a 2-of-4 window under a less-than comparison, in ALARM",
        period: 10,
        statistic: "Minimum",
        comparison: "LessThanThreshold",
        threshold: 20.0,
        evaluation_periods: 4,
        datapoints_to_alarm: Some(2),
        data: &[
            placed(5.0, 0),
            placed(5.5, 1),
            placed(40.0, 2),
            placed(41.0, 3),
        ],
        state: "ALARM",
        cited: &["5.0", "5.5"],
        not_cited: &["40.0", "41.0"],
    },
    Row {
        // M defaulting to N: a 5-of-5 alarm needs ONE datapoint in range to be in OK, and that
        // one datapoint is the whole of its explanation.
        case: "a 5-of-5 window with one datapoint in range, in OK",
        period: 10,
        statistic: "Average",
        comparison: "GreaterThanOrEqualToThreshold",
        threshold: 100.0,
        evaluation_periods: 5,
        datapoints_to_alarm: None,
        data: &[
            placed(100.0, 0),
            placed(101.0, 1),
            placed(99.0, 2),
            placed(102.0, 3),
            placed(103.0, 4),
        ],
        state: "OK",
        cited: &["99.0"],
        not_cited: &["100.0", "101.0", "102.0", "103.0"],
    },
    Row {
        // The regular cadence, a negative threshold and a fractional value: the sides of the
        // comparison are what decides the citation, not the shape of the number.
        case: "a 1-of-2 window over one-minute periods, in OK",
        period: 60,
        statistic: "Maximum",
        comparison: "LessThanThreshold",
        threshold: -2.5,
        evaluation_periods: 2,
        datapoints_to_alarm: Some(2),
        data: &[placed(-7.25, 0), placed(0.5, 1)],
        state: "OK",
        cited: &["0.5"],
        not_cited: &["-7.25"],
    },
];

/// The alarm request one row configures, watching a series of its own so every row of the table
/// shares one emulator.
fn alarm_of(row: &Row, name: &str) -> Value {
    let mut alarm = json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Case", "Value": name}],
        "Statistic": row.statistic,
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "Threshold": row.threshold,
        "ComparisonOperator": row.comparison,
        // Only the datapoints the row really publishes are graded: nothing stands in for a
        // window that holds none, so the counts the sentence reports are over those datapoints.
        "TreatMissingData": "missing",
        "ActionsEnabled": false,
    });
    if let Some(count) = row.datapoints_to_alarm {
        alarm["DatapointsToAlarm"] = json!(count);
    }
    alarm
}

/// Publish a row's datapoints, each stamped INSIDE the period it names.
///
/// Stored at high resolution so the instant a datapoint is aggregated at is its own time stamp:
/// a regular-resolution datapoint has no sub-minute detail stored, which a row with a sub-minute
/// period could not place a datapoint inside at all. The stamps are placed in the MIDDLE of the
/// period they name, counting back from the newest period an evaluation grades, so a period
/// boundary crossed between the publish and the reading moves every one of a row's datapoints
/// back together and the evaluation still grades the same ones.
async fn publish_placed(router: &MultiServiceRouter, name: &str, row: &Row) {
    let period = f64::from(row.period);
    let newest_graded = graded_window_stamp(period);
    let data: Vec<Value> = row
        .data
        .iter()
        .map(|datum| {
            json!({
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": name}],
                "Value": datum.value,
                "StorageResolution": 1,
                "Timestamp": newest_graded - f64::from(datum.periods_back) * period,
            })
        })
        .collect();
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": data}),
    )
    .await;
}

/// The datapoint list of a reason: the text between its first pair of square brackets.
///
/// The cited list is the first bracketed part of a reason; a missing-data arm, which no row of
/// this table configures, is reported in brackets of its own further on.
fn cited_list(reason: &str) -> Option<&str> {
    let open = reason.find('[')? + 1;
    let close = reason[open..].find(']')? + open;
    Some(&reason[open..close])
}

/// The count the sentence opens with: the `<n>` of "Threshold Crossed: `<n>` out of the last
/// `<m>` datapoints".
fn reported_count(reason: &str) -> Option<usize> {
    reason
        .strip_prefix("Threshold Crossed: ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The value of one cited entry: `12.0 (25/10/26 02:02:00)` names the value `12.0`.
fn cited_value(entry: &str) -> &str {
    entry.split_once(" (").map_or(entry, |(value, _)| value)
}

#[tokio::test]
async fn an_evaluated_state_reason_cites_only_the_datapoints_its_direction_rests_on() {
    // Every row watches its own series, so all of them are configured on ONE emulator and the
    // evaluation delay is waited out once rather than per row.
    let router = build_router();
    let mut names: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("cited-{index}");
        ok(&router, "PutMetricAlarm", &alarm_of(row, &name)).await;
        publish_placed(&router, &name, row).await;
        names.push(name);
    }
    wait_for_evaluation();

    for (row, name) in ROWS.iter().zip(&names) {
        let case = row.case;
        let described = ok(&router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        let reported = &described["MetricAlarms"][0];
        assert_eq!(
            reported.get("StateValue").and_then(Value::as_str),
            Some(row.state),
            "case {case:?}: expected StateValue {}, got {reported}",
            row.state
        );
        let reason = reported
            .get("StateReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let listed = cited_list(&reason)
            .unwrap_or_else(|| panic!("case {case:?}: no bracketed datapoint list in {reason:?}"));
        let values: Vec<&str> = listed.split(", ").map(cited_value).collect();

        // THE RULE: every value the sentence names is one the comparison read the way the
        // sentence states it, and none of the values on the other side is named at all.
        for value in &values {
            assert!(
                row.cited.contains(value),
                "case {case:?}: the reason of a {} state may name only {:?}, got {value:?} in \
                 {reason:?}",
                row.state,
                row.cited
            );
        }
        for excluded in row.not_cited {
            assert!(
                !values.contains(excluded),
                "case {case:?}: the reason of a {} state must not name {excluded:?}, got \
                 {reason:?}",
                row.state
            );
        }
        for wanted in row.cited {
            assert!(
                values.contains(wanted),
                "case {case:?}: expected the reason to name {wanted:?}, got {reason:?}"
            );
        }
        // And the count the sentence opens with is a count of exactly those datapoints.
        let counted = reported_count(&reason)
            .unwrap_or_else(|| panic!("case {case:?}: no leading count in {reason:?}"));
        assert_eq!(
            counted,
            values.len(),
            "case {case:?}: the sentence counts {counted} datapoints and names {}, got \
             {reason:?}",
            values.len()
        );

        // The datapoints left out of the sentence are still in the machine-readable half of the
        // decision: the record reports every value the window held.
        let record: Value = serde_json::from_str(
            reported
                .get("StateReasonData")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: StateReasonData must be JSON: {error}"));
        let recent: Vec<f64> = record["recentDatapoints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        for excluded in row.not_cited {
            let value: f64 = excluded
                .parse()
                .unwrap_or_else(|error| panic!("case {case:?}: {excluded:?} is a number: {error}"));
            assert!(
                recent.iter().any(|held| (held - value).abs() < f64::EPSILON),
                "case {case:?}: the record must still report {value} among the datapoints the \
                 window held, got {record}"
            );
        }
    }
}
