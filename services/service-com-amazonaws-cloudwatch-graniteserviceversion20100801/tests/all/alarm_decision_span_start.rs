// SPDX-License-Identifier: CC0-1.0

//! WHERE a metric alarm's decision record opens the span its state was decided over, end to end
//! over the wire.
//!
//! ONE rule, asserted over a table: **a decision record's `startDate` is the start of the oldest
//! period of the alarm's look-back, on the grid its `EvaluationWindow` lays those periods — for a
//! `WallClockWindow` the clock boundary the alarm's period repeats at, and for a rolling window
//! the look-back's own width behind the end that range has advanced to, read at the granularity
//! the series it grades has values at — and the periods it grades are counted from that start, so
//! every datapoint the verdict rests on sits a whole number of periods after it.** So an alarm
//! whose period is wider than the granularity its range's end is read at opens its span INSIDE
//! the aligned period the clock is in rather than on that period's boundary, which is what
//! distinguishes a range that rolls from one that is aligned.
//!
//! "The evaluation window that an alarm uses to select the range of metric data that it
//! evaluates each time it runs. This is a union type. Set exactly one of its members,
//! `SlidingWindow` or `WallClockWindow`. If you don't set `EvaluationWindow`, the alarm uses a
//! `SlidingWindow` by default." (`smithy_query(query="shape",shape="EvaluationWindow")`), where a
//! sliding window "advances each time the alarm is evaluated, forming a rolling time window" and
//! a wall clock window "aligns the evaluated range to fixed clock boundaries that match the
//! alarm's period, such as the top of the hour, midnight, or the start of the calendar week,
//! optionally in a specific time zone". The granularity a range is read at is the one a datapoint
//! is stored with: "Setting this to 1 specifies this metric as a high-resolution metric […]
//! Setting this to 60 specifies this metric as regular resolution, which CloudWatch stores at
//! 1-minute resolution."
//! (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`). The record
//! itself is "an explanation for the alarm state, in JSON format."
//! (`smithy_query(query="shape",shape="MetricAlarm")`, member `StateReasonData`).
//!
//! The rows of the first table differ along what the rule generalises over: the width of the
//! period (10 and 20 seconds, which are high-resolution cadences read at their own granularity,
//! and 60, 300 and 3,600 seconds, which are read at the minute), how many periods the look-back
//! covers (one, two and three), and whether the alarm names a `SlidingWindow` or leaves the
//! member out and takes the same window by default. The second test is the other half of the
//! rule, which only the two windows side by side can show: twin alarms of the same period over
//! the same datapoints open their spans at DIFFERENT instants — the aligned one on the clock
//! boundary of the period it graded, the rolling one inside the aligned period the clock is in.

use std::time::Duration;

use ::chrono::{DateTime, NaiveDateTime, Timelike};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, document_instant_secs, graded_window_stamp, now_secs, ok, wait_for_evaluation,
};

/// The customer namespace every alarm of this suite watches.
const NAMESPACE: &str = "Custom/DecisionSpans";

/// The metric they watch.
const METRIC: &str = "Load";

/// The value every alarm of this suite is compared against.
const THRESHOLD: f64 = 10.0;

/// The value published, which crosses [`THRESHOLD`].
const BREACHING_VALUE: f64 = 99.0;

/// The granularity a REGULAR-resolution series has values at, in seconds: "Setting this to 60
/// specifies this metric as regular resolution, which CloudWatch stores at 1-minute resolution."
/// (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`).
const REGULAR_RESOLUTION_SECS: f64 = 60.0;

/// How far apart two instants may read and still be the same instant: a document holds an instant
/// to the MILLISECOND (`2026-09-22T23:06:00.000+0000`), so one it writes for a computed instant is
/// that instant rounded to the millisecond.
const DOCUMENT_INSTANT_TOLERANCE_SECS: f64 = 0.0015;

/// One row of the table.
#[derive(Clone, Copy, Debug)]
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The alarm's `Period`, in seconds.
    period: i32,
    /// The alarm's `EvaluationPeriods`.
    evaluation_periods: i32,
    /// Whether the row names a `SlidingWindow` or leaves `EvaluationWindow` out — "the alarm uses
    /// a `SlidingWindow` by default" either way.
    names_window: bool,
}

/// The table: one row per shape the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        // A high-resolution cadence, read at its own granularity rather than at the minute.
        case: "10-second periods over three of them",
        period: 10,
        evaluation_periods: 3,
        names_window: false,
    },
    Row {
        case: "20-second periods over two of them, naming the window",
        period: 20,
        evaluation_periods: 2,
        names_window: true,
    },
    Row {
        // The shortest regular cadence, where the look-back's width and the read granularity are
        // the same span.
        case: "one-minute periods over one of them",
        period: 60,
        evaluation_periods: 1,
        names_window: false,
    },
    Row {
        case: "one-minute periods over three of them, naming the window",
        period: 60,
        evaluation_periods: 3,
        names_window: true,
    },
    Row {
        // The recorded shape: five-minute periods, whose span is wider than the granularity the
        // series has values at, so the range opens inside the aligned period.
        case: "five-minute periods over one of them, naming the window",
        period: 300,
        evaluation_periods: 1,
        names_window: true,
    },
    Row {
        case: "five-minute periods over three of them",
        period: 300,
        evaluation_periods: 3,
        names_window: false,
    },
    Row {
        case: "hour periods over three of them, naming the window",
        period: 3_600,
        evaluation_periods: 3,
        names_window: true,
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
        "Statistic": "Maximum",
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        // One breaching datapoint decides the alarm whatever the periods beside it hold, so a row
        // reaches a graded state from the single datapoint it publishes.
        "TreatMissingData": "notBreaching",
        "ActionsEnabled": false,
    });
    if row.names_window {
        alarm["EvaluationWindow"] = json!({"SlidingWindow": {}});
    }
    alarm
}

/// Publish one breaching datapoint of `name`'s series at each of `stamps`.
///
/// Stored at high resolution so the instant a datapoint is aggregated at is its own time stamp —
/// a regular-resolution datapoint has no sub-minute detail stored ("CloudWatch stores at 1-minute
/// resolution"), which a row with a sub-minute period could not place a datapoint inside at all.
async fn publish_breaching(router: &MultiServiceRouter, name: &str, stamps: &[f64]) {
    let data: Vec<Value> = stamps
        .iter()
        .map(|at| {
            json!({
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": name}],
                "Value": BREACHING_VALUE,
                "StorageResolution": 1,
                "Timestamp": at,
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

/// Read one alarm back, by the name it was created under.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = ok(router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    answered
        .pointer("/MetricAlarms/0")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the alarm {name:?}, got {answered}"))
}

/// The `StateReasonData` of a described alarm, parsed: it is "an explanation for the alarm state,
/// in JSON format", so a consumer reads it as a document.
fn reason_data(case: &str, alarm: &Value) -> Value {
    let text = alarm
        .get("StateReasonData")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected StateReasonData, got {alarm}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: StateReasonData is not JSON: {error}"))
}

/// One instant a decision record carries, as epoch seconds.
fn instant_of(case: &str, record: &Value, member: &str) -> f64 {
    record
        .get(member)
        .and_then(Value::as_str)
        .and_then(document_instant_secs)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} instant, got {record}"))
}

/// The instants of the datapoints the verdict rests on, oldest first.
fn graded_instants(record: &Value) -> Vec<f64> {
    let mut instants: Vec<f64> = record
        .get("evaluatedDatapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(document_instant_secs)
        })
        .collect();
    instants.sort_by(f64::total_cmp);
    instants
}

/// The start of the `period`-second window `secs` falls in, for a period the clock repeats it at.
fn window_start(secs: f64, period: f64) -> f64 {
    (secs / period).floor() * period
}

/// The granularity the end of a ROLLING range is read at, and so the granularity the span its
/// record opens at is stated in: the minute a regular-resolution datapoint is aggregated into, or
/// the alarm's own period where that is finer — the granularity a high-resolution series is folded
/// at.
fn read_granularity(period: f64) -> f64 {
    period.min(REGULAR_RESOLUTION_SECS)
}

/// The start of the granule `secs` falls in, at the granularity `period` is read at — taken from
/// the CALENDAR fields of that instant for the minute, rather than from the arithmetic the
/// evaluator makes, so the expectation is not a second copy of the implementation.
fn granule_start(secs: f64, period: f64) -> f64 {
    if period < REGULAR_RESOLUTION_SECS {
        return window_start(secs, period);
    }
    #[allow(clippy::cast_possible_truncation)]
    let local: NaiveDateTime = DateTime::from_timestamp(secs.floor() as i64, 0)
        .expect("the test clock reads an instant")
        .naive_utc();
    #[allow(clippy::cast_precision_loss)]
    let truncated = local
        .with_second(0)
        .and_then(|at| at.with_nanosecond(0))
        .expect("the start of a minute is an instant")
        .and_utc()
        .timestamp() as f64;
    truncated
}

#[tokio::test]
async fn a_rolling_range_opens_its_span_a_look_back_behind_the_query_instant() {
    // Every row watches its own series, so all of them are configured on ONE emulator and the
    // evaluation delay is waited out once rather than per row.
    let router = build_router();
    let mut names: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("rolling-{index}");
        ok(&router, "PutMetricAlarm", &alarm_of(row, &name)).await;
        // One breaching datapoint, stamped in the middle of the newest period of its own width
        // that has closed, which is a period every row's evaluation range reaches.
        publish_breaching(&router, &name, &[graded_window_stamp(f64::from(row.period))]).await;
        names.push(name);
    }
    wait_for_evaluation();

    for (row, name) in ROWS.iter().zip(&names) {
        let case = row.case;
        let period = f64::from(row.period);
        let evaluation_periods = f64::from(row.evaluation_periods);
        let granularity = read_granularity(period);
        let reported = described(&router, case, name).await;
        assert_eq!(
            reported.get("StateValue").and_then(Value::as_str),
            Some("ALARM"),
            "case {case:?}: the breaching datapoint decides the alarm, got {reported}"
        );
        let record = reason_data(case, &reported);
        let query_date = instant_of(case, &record, "queryDate");
        let start_date = instant_of(case, &record, "startDate");
        let graded = graded_instants(&record);
        assert!(
            !graded.is_empty(),
            "case {case:?}: expected a graded datapoint, got {record}"
        );

        // THE RULE, first half: the span opens a LOOK-BACK behind the instant the evaluation
        // queried the series at — bar the granule its range's end is read at, which the rolling
        // range includes while it is still open.
        let reaches_back = query_date - start_date;
        assert!(
            reaches_back + DOCUMENT_INSTANT_TOLERANCE_SECS
                >= evaluation_periods * period - granularity,
            "case {case:?}: a range of {evaluation_periods} periods of {period} seconds reaches \
             back about that far from the query instant {query_date}, got a span of \
             {reaches_back} seconds opening at {start_date} in {record}"
        );
        // It opens at an instant the series it graded has values at, rather than between two of
        // them.
        assert_eq!(
            (start_date - granule_start(start_date, period)).abs(),
            0.0,
            "case {case:?}: the span opens at a multiple of the {granularity}-second granularity \
             the series has values at, got {start_date} in {record}"
        );
        // THE RULE, second half: the periods the range is folded into are counted from that
        // start, so every datapoint the verdict rests on is a whole number of periods after it —
        // and for a one-period look-back the single datapoint IS the instant the span opens at.
        for instant in &graded {
            let into_span = instant - start_date;
            let off_grid = into_span.rem_euclid(period);
            let off_grid = off_grid.min(period - off_grid);
            assert!(
                into_span >= -DOCUMENT_INSTANT_TOLERANCE_SECS
                    && off_grid < DOCUMENT_INSTANT_TOLERANCE_SECS,
                "case {case:?}: a graded datapoint sits a whole number of {period}-second \
                 periods after the span's start ({start_date}), got {instant} in {record}"
            );
        }
        if row.evaluation_periods == 1 {
            assert_eq!(
                graded,
                vec![start_date],
                "case {case:?}: a one-period look-back grades exactly the period its span opens \
                 at ({start_date}), got {graded:?} in {record}"
            );
        }
        // And where the alarm's period is wider than that granularity, the start lies INSIDE the
        // aligned period the query instant falls in rather than on its boundary: this is the
        // difference between a range that rolls and one that is aligned.
        if period > granularity {
            let boundary = window_start(query_date, period);
            assert!(
                start_date < boundary,
                "case {case:?}: a rolling range reaches back before the {period}-second boundary \
                 at {boundary} that the query instant {query_date} follows, got {start_date} in \
                 {record}"
            );
        }
    }
}

/// The period the twins of the second test are evaluated on.
///
/// Five minutes: the shortest period a `WallClockWindow` may carry that is wider than the
/// granularity a regular-resolution series has values at — "the alarm's period must be 1 minute
/// (60 seconds), 5 minutes (300 seconds), 1 hour (3,600 seconds), 1 day (86,400 seconds), or 1
/// week (604,800 seconds)" (`smithy_query(query="shape",shape="WallClockWindow")`) — so it is the
/// narrowest shape the two windows can be told apart on at all.
const CONTRAST_PERIOD: i32 = 300;

/// How far into the aligned period the twins' datapoints are published at the earliest.
///
/// A rolling range's boundaries are counted back from an end read at the MINUTE and one
/// evaluation cadence behind the clock, so once the clock is a minute into the aligned period
/// that end has passed the period's own boundary and the span it opens lies strictly INSIDE the
/// period before it — which is what makes the two windows' spans differ at all. It is also what
/// the datapoint inside the open aligned period needs: stamped in the newest minute that has
/// CLOSED, it is inside that period only once the clock is a minute into it. Right at the
/// boundary the two grids can still agree.
const CONTRAST_PHASE_FROM_SECS: f64 = 60.0;

/// How much of the aligned period must be left when they are published and read.
///
/// The twins' datapoints are placed relative to the period the clock is in, so the reading has to
/// fall in that same period; and the rolling range's end, which is read at the minute, has to stay
/// short of the period's own end, so the span it opens — a look-back behind that end — lies
/// strictly before the aligned boundary the clock has last passed rather than on it.
///
/// Wider than that end alone needs, so the case does not depend on how fast the host gets from
/// the publish to the read: whatever room is left over is slack a loaded machine may spend on the
/// two writes, the evaluation and the two reads between them. The cost is the wait ahead of the
/// case, which is bounded by the span outside the admissible phase — under three minutes, and
/// nothing is asserted about it.
const CONTRAST_ROOM_SECS: f64 = 100.0;

/// Wait until the clock is at least `from` seconds into the `period`-second window it is in and at
/// least `room` seconds short of that window's end.
///
/// The wait is an await, so a test driving the service meanwhile still runs, and it waits for the
/// CONDITION rather than for a span: whatever the phase it starts in, it returns inside the next
/// window at the latest.
async fn wait_for_window_phase(period: f64, from: f64, room: f64) {
    while !(from..=period - room).contains(&now_secs().rem_euclid(period)) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The alarm request one twin configures, differing only in the evaluation window it selects its
/// range with.
fn twin_of(name: &str, window: &Value) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Case", "Value": name}],
        "Statistic": "Maximum",
        "Period": CONTRAST_PERIOD,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "missing",
        "EvaluationWindow": window,
        "ActionsEnabled": false,
    })
}

#[tokio::test]
async fn an_aligned_range_opens_on_a_clock_boundary_and_a_rolling_one_inside_the_period() {
    let router = build_router();
    let period = f64::from(CONTRAST_PERIOD);
    wait_for_window_phase(period, CONTRAST_PHASE_FROM_SECS, CONTRAST_ROOM_SECS).await;

    let aligned = "aligned-twin";
    let rolling = "rolling-twin";
    for (name, window) in [
        (aligned, json!({"WallClockWindow": {"Timezone": "UTC"}})),
        (rolling, json!({"SlidingWindow": {}})),
    ] {
        ok(&router, "PutMetricAlarm", &twin_of(name, &window)).await;
    }

    // The same two breaching datapoints for each twin, both stamped in a minute that has CLOSED,
    // because the rolling range's end is read at the minute: a stamp in the minute still being
    // published into is in no range yet, whatever second of it the suite happens to run in. One
    // of them sits in the last closed minute of the aligned period BEFORE this one, and one in
    // the newest closed minute, which is inside the aligned period the clock is IN. The aligned
    // window grades only the closed period; the rolling range has already reached the open one.
    let open_start = window_start(now_secs(), period);
    let closed_start = open_start - period;
    let in_the_closed_period = open_start - REGULAR_RESOLUTION_SECS / 2.0;
    let in_the_open_period = graded_window_stamp(REGULAR_RESOLUTION_SECS);
    for name in [aligned, rolling] {
        publish_breaching(&router, name, &[in_the_closed_period, in_the_open_period]).await;
    }
    wait_for_evaluation();

    let case = "the aligned twin";
    let reported = described(&router, case, aligned).await;
    assert_eq!(
        reported.get("StateValue").and_then(Value::as_str),
        Some("ALARM"),
        "case {case:?}: the breaching datapoint of the closed aligned period decides the alarm, \
         got {reported}"
    );
    let aligned_record = reason_data(case, &reported);
    let aligned_start = instant_of(case, &aligned_record, "startDate");
    assert!(
        (aligned_start - closed_start).abs() < DOCUMENT_INSTANT_TOLERANCE_SECS,
        "case {case:?}: an aligned range opens on the clock boundary its graded period began at \
         ({closed_start}), got {aligned_start} in {aligned_record}"
    );
    assert_eq!(
        aligned_start.rem_euclid(period),
        0.0,
        "case {case:?}: an aligned range opens at a boundary a {period}-second period repeats at, \
         got {aligned_start} in {aligned_record}"
    );

    let case = "the rolling twin";
    let reported = described(&router, case, rolling).await;
    assert_eq!(
        reported.get("StateValue").and_then(Value::as_str),
        Some("ALARM"),
        "case {case:?}: a rolling range reaches the period the clock is in, got {reported}"
    );
    let rolling_record = reason_data(case, &reported);
    let rolling_start = instant_of(case, &rolling_record, "startDate");
    let query_date = instant_of(case, &rolling_record, "queryDate");
    let graded = graded_instants(&rolling_record);
    assert_eq!(
        graded,
        vec![rolling_start],
        "case {case:?}: a one-period rolling look-back grades exactly the period its span opens \
         at ({rolling_start}), got {graded:?} in {rolling_record}"
    );
    assert!(
        query_date - rolling_start + DOCUMENT_INSTANT_TOLERANCE_SECS
            >= period - read_granularity(period),
        "case {case:?}: a range of one period of {period} seconds reaches back about that far \
         from the query instant {query_date}, got a span opening at {rolling_start} in \
         {rolling_record}"
    );
    // The two windows name DIFFERENT spans over the very same datapoints: the aligned one begins
    // on the boundary of the period it graded, the rolling one inside that period's successor.
    assert!(
        rolling_start > closed_start && rolling_start < open_start,
        "case {case:?}: a rolling range opens inside the aligned period ({closed_start} .. \
         {open_start}) rather than on either boundary, got {rolling_start} in {rolling_record}"
    );
    assert_ne!(
        rolling_start.rem_euclid(period),
        0.0,
        "case {case:?}: a rolling range opens at no boundary a {period}-second period repeats \
         at, got {rolling_start} in {rolling_record}"
    );
    assert_ne!(
        rolling_start, aligned_start,
        "case {case:?}: the two evaluation windows must open their spans at different instants, \
         got {rolling_start} for the rolling twin and {aligned_start} for the aligned one \
         ({rolling_record})"
    );
}
