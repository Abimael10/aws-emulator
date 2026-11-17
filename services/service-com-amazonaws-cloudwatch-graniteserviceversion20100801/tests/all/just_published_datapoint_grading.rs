// SPDX-License-Identifier: CC0-1.0

//! How far BACK a metric alarm's evaluation range reaches, end to end over the wire.
//!
//! ONE rule, asserted over a table: **the datapoints a client has published for the series an
//! alarm watches are the ones its evaluation grades, so the range it searches reaches back at
//! least as far as the granularity those datapoints are STORED at** — a datapoint published at
//! regular resolution is aggregated into the whole minute it falls in ("Setting this to 60
//! specifies this metric as regular resolution, which CloudWatch stores at 1-minute
//! resolution", `smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`), so
//! a series at that resolution holds one datapoint per minute however often it is published
//! into, and an alarm whose `Period` is finer than a minute has at most one real datapoint per
//! minute of range. A range narrower than that granularity would let the newest datapoint of
//! the series fall out of it before any evaluation had graded it, and the alarm would never see
//! a breach the client really published.
//!
//! So ONE datapoint, published the way a client publishes one — `PutMetricData` with no
//! `Timestamp`, which stamps it at the instant of the call ("If you do not specify a time
//! stamp, the time stamp of the data point is the time that `PutMetricData` was called",
//! `smithy_query(query="doc",shape="MetricDatum",member="Timestamp")`) — decides the alarm that
//! watches it, whatever `Period` that alarm is evaluated on and whatever phase of its storage
//! span the call lands in. It decides it in the direction the datapoint really is on: a value
//! the comparison does not breach leaves the alarm in `OK`. It decides it once the period the
//! datapoint landed in has CLOSED — a period still being published into grades nothing — which
//! for a datapoint stamped at the instant of the call is at most one storage span away.
//!
//! The rows differ along what the rule generalises over: the alarm's `Period` (each
//! sub-minute cadence `PutMetricAlarm` admits — "Valid values are 10, 20, 30, and any multiple
//! of 60" — the minute itself and a multiple of it), the granularity the datapoint is stored at
//! (regular and high resolution), the statistic the window is folded by, the direction of the
//! comparison, and whether the published value breaches at all.

use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{build_router, call, ok, AlarmKind};

/// The namespace the rows publish into: a customer namespace, so the datapoint goes over the
/// wire the way a client's does (`PutMetricData` refuses an `AWS/` namespace).
const NAMESPACE: &str = "Custom/JustPublished";

/// The metric the rows watch inside it.
const METRIC: &str = "Load";

/// The threshold every row's alarm carries.
const THRESHOLD: f64 = 50.0;

/// How long a row waits for the state its one datapoint implies, and how often it looks.
///
/// A datapoint is graded by the evaluation that follows the CLOSE of the period it landed in,
/// and a datapoint stamped at the instant of the call lands in the storage span the clock is in
/// — a minute for a regular-resolution series, the longest any row here waits — so the deadline
/// is that span and the sub-second evaluation delay behind it, with room to spare. A row that
/// never grades the datapoint fails on what it last read instead of hanging; nothing is asserted
/// about how long the grading took.
const GRADED_WITHIN: Duration = Duration::from_secs(90);
/// See [`GRADED_WITHIN`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// One row: the alarm's period, the resolution the datapoint is stored at, how the window is
/// folded, which way the comparison runs, the value published and the state the alarm must
/// reach.
struct Row {
    case: &'static str,
    period: i64,
    /// The `StorageResolution` the datapoint is published with, or `None` for a publish that
    /// names none — "This field is optional, if you do not specify it the default of 60 is
    /// used" (`MetricDatum$StorageResolution`), which is the regular resolution.
    storage_resolution: Option<i64>,
    statistic: &'static str,
    comparison_operator: &'static str,
    /// The arm a window with no datapoint of its own is counted under.
    ///
    /// A row whose value BREACHES names `notBreaching`, under which nothing but a graded
    /// breaching datapoint can reach `ALARM`; a row whose value does NOT breach names
    /// `missing`, under which an evaluation that could decide nothing leaves the alarm in
    /// `INSUFFICIENT_DATA` — so `OK` is reachable only by grading the datapoint that really
    /// was published. Either way the state the row asserts is one only a graded datapoint
    /// produces.
    treat_missing_data: &'static str,
    value: f64,
    state: &'static str,
}

/// The rows. Every one of them publishes ONE datapoint, at the instant of the call.
const ROWS: &[Row] = &[
    // The recorded configuration: the shortest cadence an alarm may be evaluated on, over a
    // series published at the default resolution, breaching BELOW its threshold.
    Row {
        case: "the shortest period over a regular-resolution series",
        period: 10,
        storage_resolution: None,
        statistic: "Maximum",
        comparison_operator: "LessThanThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD - 40.0,
        state: "ALARM",
    },
    Row {
        case: "a twenty-second period over a regular-resolution series",
        period: 20,
        storage_resolution: None,
        statistic: "Average",
        comparison_operator: "GreaterThanThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD + 1.0,
        state: "ALARM",
    },
    Row {
        case: "a thirty-second period over a regular-resolution series",
        period: 30,
        storage_resolution: None,
        statistic: "Minimum",
        comparison_operator: "GreaterThanOrEqualToThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD,
        state: "ALARM",
    },
    // The regular cadence, whose own look-back already reaches further back than a minute: the
    // rule holds there too, and the row is what would show a change to the range breaking it.
    Row {
        case: "a one-minute period over a regular-resolution series",
        period: 60,
        storage_resolution: None,
        statistic: "Sum",
        comparison_operator: "LessThanOrEqualToThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD,
        state: "ALARM",
    },
    Row {
        case: "a five-minute period over a regular-resolution series",
        period: 300,
        storage_resolution: None,
        statistic: "Maximum",
        comparison_operator: "GreaterThanThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD + 100.0,
        state: "ALARM",
    },
    // The same shortest cadence over a series stored at ONE-SECOND granularity, where the span
    // the range has to cover is a second rather than a minute.
    Row {
        case: "the shortest period over a high-resolution series",
        period: 10,
        storage_resolution: Some(1),
        statistic: "Maximum",
        comparison_operator: "LessThanThreshold",
        treat_missing_data: "notBreaching",
        value: THRESHOLD - 1.0,
        state: "ALARM",
    },
    // And the direction the datapoint really is on: a value the comparison does not breach is
    // graded just as promptly, into OK.
    Row {
        case: "a value that does not breach, over a regular-resolution series",
        period: 10,
        storage_resolution: None,
        statistic: "Maximum",
        comparison_operator: "LessThanThreshold",
        // Nothing graded leaves this alarm in INSUFFICIENT_DATA, so OK is reachable only by
        // grading the datapoint that was published.
        treat_missing_data: "missing",
        value: THRESHOLD + 1.0,
        state: "OK",
    },
    Row {
        case: "a value that does not breach, over a high-resolution series",
        period: 30,
        storage_resolution: Some(1),
        statistic: "Average",
        comparison_operator: "GreaterThanThreshold",
        treat_missing_data: "missing",
        value: THRESHOLD - 1.0,
        state: "OK",
    },
];

/// The state `DescribeAlarms` reports for each row's alarm, once it is the state that row
/// expects or once the deadline is up — so a row that never grades its datapoint fails on what
/// it last read.
///
/// Every row is looked at on EVERY pass rather than one row being waited out before the next is
/// looked at: each row's datapoint is graded by the evaluation that follows the close of the
/// period it landed in, and the evaluation range an alarm searches is only a few periods wide,
/// so a row waited out after another's whole wait could have its one datapoint fall out of the
/// range before it was ever read.
async fn awaited_states(
    router: &smithy_server_runtime::MultiServiceRouter,
    names: &[String],
) -> Vec<String> {
    let deadline = ::std::time::Instant::now() + GRADED_WITHIN;
    let mut last: Vec<String> = vec![String::new(); names.len()];
    loop {
        let mut all_reached = true;
        for (index, (row, name)) in ROWS.iter().zip(names).enumerate() {
            if last[index] == row.state {
                continue;
            }
            let described = crate::harness::described_alarm(router, AlarmKind::Metric, name).await;
            last[index] = described
                .as_ref()
                .and_then(|alarm| alarm.get("StateValue"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if last[index] != row.state {
                all_reached = false;
            }
        }
        if all_reached || ::std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

#[tokio::test]
async fn one_just_published_datapoint_decides_the_alarm_that_watches_it() {
    // Every row watches its own series — its `Case` dimension is its own — so all of them are
    // configured on ONE emulator and the close of the storage span they publish into is waited
    // out once rather than per row.
    let router = build_router();
    let mut names: Vec<String> = Vec::with_capacity(ROWS.len());
    for row in ROWS {
        let dimension = row.case.replace(' ', "-");
        let name = format!("just-published-{dimension}");

        let (status, body) = call(
            &router,
            "PutMetricAlarm",
            &json!({
                "AlarmName": name,
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": dimension}],
                "Statistic": row.statistic,
                "Period": row.period,
                "EvaluationPeriods": 1,
                "DatapointsToAlarm": 1,
                "Threshold": THRESHOLD,
                "ComparisonOperator": row.comparison_operator,
                // The arm the row names, so the state it asserts is one only a graded
                // datapoint can produce (see `Row::treat_missing_data`).
                "TreatMissingData": row.treat_missing_data,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected PutMetricAlarm to answer 200, got {status} {body:?}",
            row.case
        );

        let mut datum = json!({
            "MetricName": METRIC,
            "Dimensions": [{"Name": "Case", "Value": dimension}],
            "Value": row.value,
        });
        if let Some(resolution) = row.storage_resolution {
            datum["StorageResolution"] = json!(resolution);
        }
        ok(
            &router,
            "PutMetricData",
            &json!({"Namespace": NAMESPACE, "MetricData": [datum]}),
        )
        .await;
        names.push(name);
    }

    let reached = awaited_states(&router, &names).await;
    for ((row, name), reached) in ROWS.iter().zip(&names).zip(&reached) {
        assert_eq!(
            reached.as_str(), row.state,
            "case {:?}: expected the alarm {name} to reach {} from the one datapoint published \
             for it, got {reached:?}",
            row.case, row.state
        );
    }
}
