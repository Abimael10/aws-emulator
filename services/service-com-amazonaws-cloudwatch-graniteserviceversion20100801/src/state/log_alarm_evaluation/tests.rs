// SPDX-License-Identifier: CC0-1.0

//! Unit cover for the M-out-of-N fold itself: which state a window of query results reaches,
//! which it does not reach at all, and what the missing-data treatment makes of a window no
//! execution has filled yet.

use super::*;

/// One row of the fold's table: the window the executions produced, the M-out-of-N configuration
/// and the comparison they are graded with, and the state they reach.
struct Row<'a> {
    /// What this row is about.
    case: &'a str,
    /// The values the executions produced, oldest first.
    values: &'a [f64],
    /// "the number of most recent scheduled query results that the alarm evaluates against the
    /// threshold (the N in M-of-N evaluation)" (`LogAlarm$QueryResultsToEvaluate`).
    evaluate: i32,
    /// "the number of query results … that must breach the threshold to trigger the alarm to
    /// transition to ALARM (the M in M-of-N evaluation)" (`LogAlarm$QueryResultsToAlarm`).
    to_alarm: i32,
    /// "The value to compare with the aggregated query result." (`LogAlarm$Threshold`)
    threshold: f64,
    /// The arithmetic each value is compared with.
    comparison: &'a str,
    /// "How this alarm handles missing data points." (`LogAlarm$TreatMissingData`), or `None`
    /// for an alarm that states none.
    treatment: Option<&'a str>,
    /// The state the window reaches, and `None` for a window that reaches neither direction —
    /// which leaves the alarm the state it has.
    state: Option<&'a str>,
}

/// The alarm a case folds: the M-out-of-N configuration, the comparison and the missing-data
/// treatment, and nothing else — the fold reads no other member.
fn alarm(row: &Row<'_>) -> LogAlarm {
    LogAlarm {
        alarm_name: Some("folded".to_string()),
        query_results_to_evaluate: Some(row.evaluate),
        query_results_to_alarm: Some(row.to_alarm),
        threshold: Some(row.threshold),
        comparison_operator: Some(row.comparison.to_string()),
        treat_missing_data: row.treatment.map(str::to_string),
        ..LogAlarm::default()
    }
}

/// The query results a case evaluates, oldest first.
fn results(values: &[f64]) -> Vec<LogQueryResult> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| LogQueryResult {
            query_result_id: format!("execution-{index}"),
            query_execution_id: format!("execution-{index}"),
            status: RESULT_STATUS_COMPLETE.to_string(),
            value: Some(*value),
            group: None,
            at_secs: 1_700_000_000.0 + index as f64,
        })
        .collect()
}

#[test]
fn the_window_decides_the_state_asymmetrically() {
    let rows = [
        Row {
            case: "one breaching value of the two evaluated, with one needed",
            values: &[2.0, 7.0],
            evaluate: 2,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            treatment: None,
            state: Some(ALARM_STATE_ALARM),
        },
        Row {
            case: "both values non-breaching, which is what leaving ALARM takes",
            values: &[2.0, 2.0],
            evaluate: 2,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            treatment: None,
            state: Some(ALARM_STATE_OK),
        },
        Row {
            case: "one non-breaching value of the two evaluated: neither direction is reached",
            values: &[2.0],
            evaluate: 2,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            treatment: None,
            state: None,
        },
        Row {
            case: "the same window, with the missing result treated as not breaching",
            values: &[2.0],
            evaluate: 2,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            treatment: Some("notBreaching"),
            state: Some(ALARM_STATE_OK),
        },
        Row {
            case: "no result at all, with the missing results treated as breaching",
            values: &[],
            evaluate: 2,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            treatment: Some("breaching"),
            state: Some(ALARM_STATE_ALARM),
        },
        Row {
            case: "one breaching value of three, with two needed and the rest still missing",
            values: &[9.0],
            evaluate: 3,
            to_alarm: 2,
            threshold: 3.0,
            comparison: "GreaterThanThreshold",
            treatment: None,
            state: None,
        },
        Row {
            case: "two breaching values of three, with two needed",
            values: &[1.0, 9.0, 9.0],
            evaluate: 3,
            to_alarm: 2,
            threshold: 3.0,
            comparison: "GreaterThanThreshold",
            treatment: None,
            state: Some(ALARM_STATE_ALARM),
        },
        Row {
            case: "two non-breaching of three with two needed to breach, which leaves ALARM",
            values: &[9.0, 1.0, 1.0],
            evaluate: 3,
            to_alarm: 2,
            threshold: 3.0,
            comparison: "GreaterThanThreshold",
            treatment: None,
            state: Some(ALARM_STATE_OK),
        },
        Row {
            case: "a value under a less-than comparison",
            values: &[1.0],
            evaluate: 1,
            to_alarm: 1,
            threshold: 3.0,
            comparison: "LessThanThreshold",
            treatment: None,
            state: Some(ALARM_STATE_ALARM),
        },
    ];

    for row in &rows {
        let case = row.case;
        let alarm = alarm(row);
        let window = results(row.values);
        let verdict = Verdict::of(&alarm, &window, None, 1_700_000_100.0);
        let reached = verdict.as_ref().map(|verdict| verdict.state.as_str());
        assert_eq!(
            reached, row.state,
            "case {case:?}: expected {:?}, got {reached:?}",
            row.state
        );
        let Some(verdict) = verdict else {
            continue;
        };
        // The explanation states the comparison the fold made and the window it made it over.
        assert!(
            verdict.reason.contains("query result"),
            "case {case:?}: expected the reason to count the query results, got {:?}",
            verdict.reason
        );
        let data: ::serde_json::Value = ::serde_json::from_str(&verdict.reason_data)
            .unwrap_or_else(|_| panic!("case {case:?}: the reason data is JSON"));
        assert_eq!(
            data["threshold"].as_f64(),
            Some(row.threshold),
            "case {case:?}: expected the threshold it compared against, got {data}"
        );
        assert_eq!(
            data["results"].as_array().map(Vec::len),
            Some(row.values.len()),
            "case {case:?}: expected the window's own results, got {data}"
        );
    }
}

#[test]
fn a_run_that_produced_nothing_leaves_the_alarm_without_data() {
    let alarm = alarm(&Row {
        case: "a run that produced nothing",
        values: &[],
        evaluate: 2,
        to_alarm: 1,
        threshold: 3.0,
        comparison: "GreaterThanOrEqualToThreshold",
        treatment: None,
        state: Some(ALARM_STATE_INSUFFICIENT_DATA),
    });
    let verdict = Verdict::of(&alarm, &[], Some("the query could not be run"), 1_700_000_100.0)
        .expect("a failure is explained");
    assert_eq!(
        verdict.state, ALARM_STATE_INSUFFICIENT_DATA,
        "expected no graded state, got {verdict:?}"
    );
    assert!(
        verdict.reason.contains("the query could not be run"),
        "expected the reason to carry what failed, got {:?}",
        verdict.reason
    );
}
