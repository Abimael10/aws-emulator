// SPDX-License-Identifier: CC0-1.0

//! WHETHER the window a metric alarm's decision record names had CLOSED by the time the
//! evaluation that recorded it ran, read where a client reads it: the state-update items
//! `DescribeAlarmHistory` serves.
//!
//! ONE rule, asserted over a table: **the newest window a rolling (sliding) evaluation grades is
//! the latest span of exactly `Period` seconds, anchored on the granularity the series it grades
//! has values at, whose END is at or before the instant of that evaluation — so the granule the
//! clock is in, which is still being published into, is never aggregated into the datapoint the
//! alarm acted on, and the span a record names closed before the time stamp of the history item
//! carrying it.**
//!
//! A period is one datapoint out of every observation stamped inside it — "CloudWatch aggregates
//! all data points with time stamps that fall within each one-hour period"
//! (`smithy_query(query="doc",shape="GetMetricStatistics")`) — over "The length, in seconds, used
//! each time the metric specified in `MetricName` is evaluated. Valid values are 10, 20, 30, and
//! any multiple of 60."
//! (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Period")`). Which span of the
//! series an evaluation reads is what the alarm's evaluation window selects: "Set exactly one of
//! its members, `SlidingWindow` or `WallClockWindow`. If you don't set `EvaluationWindow`, the
//! alarm uses a `SlidingWindow` by default."
//! (`smithy_query(query="shape",shape="EvaluationWindow")`), a sliding window being the one that
//! "advances each time the alarm is evaluated, forming a rolling time window" — it advances by the
//! granularity the series has values at, which for a regular-resolution series is the minute:
//! "Setting this to 60 specifies this metric as regular resolution, which CloudWatch stores at
//! 1-minute resolution."
//! (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`). What the
//! evaluation decided is reported as "An explanation for the alarm state, in JSON format."
//! (`smithy_query(query="shape",shape="MetricAlarm")`, member `StateReasonData`), recorded on the
//! history item beside "The time stamp for the alarm history item."
//! (`smithy_query(query="shape",shape="AlarmHistoryItem")`, member `Timestamp`) — which is what
//! lets a client check the window against the instant the evaluation that graded it ran at.
//!
//! The rows of the first table differ along what the rule generalises over: the width of the
//! period (one, two, five and ten minutes), how many periods the look-back covers (one and two),
//! and whether the alarm names a `SlidingWindow` or leaves the member out and takes the same
//! window by default. Every row publishes one datapoint per whole minute, each carrying a value no
//! other minute carries, and the newest of them INSIDE the minute the clock is in — so the Sum the
//! record reports names exactly the minutes that went into it, and a window reaching into the open
//! minute would be visible both in its end and in its aggregate.
//!
//! The second test is the other half of the rule, which only two successive evaluations can show:
//! the anchor is the minute the evaluation ran in, so the graded window SLIDES by the minute
//! rather than jumping a whole period at a time.

use std::collections::BTreeMap;
use std::time::Instant;

use ::chrono::{DateTime, NaiveDateTime, Timelike};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, document_instant_secs, now_secs, ok, wait_for_period_room, LOOK_INTERVAL,
    WORK_WAIT,
};

/// The customer namespace every alarm of this suite watches.
const NAMESPACE: &str = "Custom/ClosedGradedWindows";

/// The metric they watch.
const METRIC: &str = "Load";

/// The granularity a REGULAR-resolution series has values at, in seconds: "Setting this to 60
/// specifies this metric as regular resolution, which CloudWatch stores at 1-minute resolution."
/// (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`).
const MINUTE_SECS: f64 = 60.0;

/// The value the first table's alarms are compared against: every window of their series sums to
/// more, so each of them is decided by the window it graded rather than by a missing-data arm.
const THRESHOLD: f64 = 0.5;

/// How far apart two instants may read and still be the same instant: a document holds an instant
/// to the MILLISECOND (`2026-09-22T23:06:00.000+0000`), so one it writes for a computed instant is
/// that instant rounded to the millisecond.
const DOCUMENT_INSTANT_TOLERANCE_SECS: f64 = 0.0015;

/// How far apart two aggregates may read and still be the same number.
const VALUE_TOLERANCE: f64 = 1e-6;

/// How many whole minutes back the first table's series reaches: wider than the widest look-back
/// any row covers, so every window an evaluation grades holds a datapoint of every minute in it
/// however many minutes pass between the publish and the evaluation that reads it.
const MINUTES_PUBLISHED: u32 = 14;

/// How much of the minute a row needs ahead of it before it publishes: enough for the publish and
/// the evaluation that follows it to fall in the same minute the newest datapoint is stamped in,
/// so the rule is exercised on a minute that really is still open. Nothing is asserted about the
/// span — a crossed boundary only closes that minute, which the rule allows — so this is room,
/// not a deadline.
const MINUTE_ROOM_SECS: f64 = 5.0;

/// One row of the first table.
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
        // The narrowest regular cadence: the period and the granularity the window is anchored on
        // are the same span.
        case: "one-minute periods over one of them",
        period: 60,
        evaluation_periods: 1,
        names_window: false,
    },
    Row {
        case: "one-minute periods over two of them, naming the window",
        period: 60,
        evaluation_periods: 2,
        names_window: true,
    },
    Row {
        case: "two-minute periods over two of them",
        period: 120,
        evaluation_periods: 2,
        names_window: false,
    },
    Row {
        // The recorded shape: five-minute periods over a series published once a minute.
        case: "five-minute periods over one of them",
        period: 300,
        evaluation_periods: 1,
        names_window: false,
    },
    Row {
        case: "five-minute periods over one of them, naming the window",
        period: 300,
        evaluation_periods: 1,
        names_window: true,
    },
    Row {
        case: "ten-minute periods over one of them",
        period: 600,
        evaluation_periods: 1,
        names_window: false,
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
        // The Sum over a window names the minutes that went into it, which is what makes an
        // aggregate over a window reaching into the open minute tell itself apart.
        "Statistic": "Sum",
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        // Only the datapoints the row really published decide it.
        "TreatMissingData": "missing",
        "ActionsEnabled": false,
    });
    if row.names_window {
        alarm["EvaluationWindow"] = json!({"SlidingWindow": {}});
    }
    alarm
}

/// The series every row of the first table publishes: one datapoint per whole minute over
/// [`MINUTES_PUBLISHED`] minutes, the newest of them stamped INSIDE the minute the clock is in —
/// the minute the rule is about — and each carrying a value no other minute carries, so the Sum
/// over a span of them names exactly the minutes it was taken over.
fn published_series(now: f64) -> Vec<(f64, f64)> {
    let open = minute_start(now);
    let mut series = vec![(now, 1.0)];
    for minutes_back in 1..MINUTES_PUBLISHED {
        series.push((
            open - f64::from(minutes_back) * MINUTE_SECS,
            f64::from(1_u32 << minutes_back),
        ));
    }
    series
}

/// Publish `series` into `name`'s own series, at REGULAR resolution: a datapoint of such a series
/// is aggregated in the minute it was published in, which is the granularity the rule is about.
async fn publish(router: &MultiServiceRouter, name: &str, series: &[(f64, f64)]) {
    let data: Vec<Value> = series
        .iter()
        .map(|(at, value)| {
            json!({
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": name}],
                "Value": value,
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

/// What was published, by the minute it was aggregated into — the suite's own ledger of the
/// series, which is what the aggregate a record reports is checked against.
fn ledger_of(series: &[(f64, f64)]) -> BTreeMap<i64, f64> {
    let mut folded: BTreeMap<i64, f64> = BTreeMap::new();
    for (at, value) in series {
        *folded.entry(minute_key(*at)).or_insert(0.0) += *value;
    }
    folded
}

/// The Sum of the minutes `[start, end)` holds and how many minutes that is, or `None` when the
/// suite published no datapoint for one of them — in which case the aggregate cannot be checked
/// against the ledger at all.
fn published_window(ledger: &BTreeMap<i64, f64>, start: f64, end: f64) -> Option<(f64, f64)> {
    let mut sum = 0.0;
    let mut minutes = 0.0;
    let mut at = start;
    while at < end - DOCUMENT_INSTANT_TOLERANCE_SECS {
        sum += *ledger.get(&minute_key(at))?;
        minutes += 1.0;
        at += MINUTE_SECS;
    }
    Some((sum, minutes))
}

/// The minute `secs` falls in, taken from the CALENDAR fields of that instant rather than from the
/// arithmetic the evaluator makes, so the expectation is not a second copy of the implementation.
fn minute_start(secs: f64) -> f64 {
    #[allow(clippy::cast_possible_truncation)]
    let at: NaiveDateTime = DateTime::from_timestamp(secs.floor() as i64, 0)
        .expect("the test clock reads an instant")
        .naive_utc();
    #[allow(clippy::cast_precision_loss)]
    let start = at
        .with_second(0)
        .and_then(|at| at.with_nanosecond(0))
        .expect("the start of a minute is an instant")
        .and_utc()
        .timestamp() as f64;
    start
}

/// The same minute as the key the ledger holds it under.
#[allow(clippy::cast_possible_truncation)]
fn minute_key(secs: f64) -> i64 {
    minute_start(secs) as i64
}

/// One instant a decision record carries, as epoch seconds.
fn instant_of(case: &str, record: &Value, member: &str) -> f64 {
    record
        .get(member)
        .and_then(Value::as_str)
        .and_then(document_instant_secs)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} instant, got {record}"))
}

/// One datapoint a verdict rested on, as the record names it.
#[derive(Debug, Clone, Copy)]
struct Cited {
    /// The start of the period it was aggregated out of.
    at: f64,
    /// How many observations that period held.
    sample_count: Option<f64>,
    /// The value the statistic took over them.
    value: Option<f64>,
}

/// The datapoints a verdict rested on, oldest first.
fn cited_datapoints(record: &Value) -> Vec<Cited> {
    let mut cited: Vec<Cited> = record
        .get("evaluatedDatapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            let at = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(document_instant_secs)?;
            Some(Cited {
                at,
                sample_count: entry.get("sampleCount").and_then(Value::as_f64),
                value: entry.get("value").and_then(Value::as_f64),
            })
        })
        .collect();
    cited.sort_by(|left, right| left.at.total_cmp(&right.at));
    cited
}

/// The state-update history of one alarm, oldest first: each item's own time stamp beside the
/// decision record of the evaluation that wrote it, for the items that name the windows they
/// graded.
async fn state_update_records(router: &MultiServiceRouter, name: &str) -> Vec<(f64, Value)> {
    let described = ok(
        router,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "HistoryItemType": "StateUpdate",
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    described
        .get("AlarmHistoryItems")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let stamp = item.get("Timestamp").and_then(Value::as_f64)?;
            let data = item.get("HistoryData").and_then(Value::as_str)?;
            let document: Value = serde_json::from_str(data).ok()?;
            let record = document.pointer("/newState/stateReasonData")?;
            // Only the items that name the windows the evaluation graded.
            record.get("evaluatedDatapoints")?;
            Some((stamp, record.clone()))
        })
        .collect()
}

/// Wait until `name`'s history holds at least `wanted` such records.
///
/// A wait for the CONDITION: it looks again until the records are there or a generous deadline
/// passes, so nothing here depends on how fast the host gets from a publish to the evaluation it
/// makes due.
async fn until_records(
    router: &MultiServiceRouter,
    name: &str,
    wanted: usize,
) -> Vec<(f64, Value)> {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let records = state_update_records(router, name).await;
        if records.len() >= wanted || Instant::now() >= deadline {
            assert!(
                records.len() >= wanted,
                "expected at least {wanted} evaluated state updates of {name:?}, got \
                 {}",
                records.len()
            );
            return records;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Assert THE RULE over one history item: the window its record names is the latest span of
/// `period` seconds that is anchored on the minute and had ENDED when the item was stamped, and
/// the span the record opens at reaches the whole look-back back from that end.
fn assert_window_had_closed(
    case: &str,
    stamp: f64,
    record: &Value,
    period: f64,
    look_back: f64,
) -> f64 {
    let open_minute = minute_start(stamp);
    let query_date = instant_of(case, record, "queryDate");
    assert!(
        (query_date - stamp).abs() < DOCUMENT_INSTANT_TOLERANCE_SECS,
        "case {case:?}: the record of the evaluation that wrote a history item is the one it \
         queried the series at ({stamp}), got {query_date} in {record}"
    );
    assert_eq!(
        record.get("period").and_then(Value::as_f64),
        Some(period),
        "case {case:?}: the record names the period its windows are {period} seconds wide, got \
         {record}"
    );
    let start_date = instant_of(case, record, "startDate");
    // The relation a client reconciling the alarm's decision against the metric store checks:
    // the span the record names had CLOSED when the evaluation that recorded it ran.
    assert!(
        start_date + period <= stamp + DOCUMENT_INSTANT_TOLERANCE_SECS,
        "case {case:?}: the evaluation recorded at {stamp} read a window opening at {start_date} \
         and ending {} , which had not closed yet, in {record}",
        start_date + period
    );
    assert_eq!(
        start_date.rem_euclid(MINUTE_SECS),
        0.0,
        "case {case:?}: the span opens on the minute the series has values at, got {start_date} \
         in {record}"
    );
    // And it opens the whole look-back behind the end of the newest window the evaluation could
    // grade, which is the start of the minute that evaluation ran in: the minute still being
    // published into is left out of the range altogether.
    let expected_start = open_minute - look_back;
    assert!(
        (start_date - expected_start).abs() < DOCUMENT_INSTANT_TOLERANCE_SECS,
        "case {case:?}: a look-back of {look_back} seconds behind the last closed minute \
         ({open_minute}) of the evaluation recorded at {stamp} opens at {expected_start}, got \
         {start_date} in {record}"
    );
    open_minute
}

#[tokio::test]
async fn every_graded_window_had_closed_when_its_history_item_was_recorded() {
    // Every row watches its own series, so all of them are configured on ONE emulator and the
    // evaluation delay is waited out once rather than per row.
    let router = build_router();
    wait_for_period_room(MINUTE_SECS, MINUTE_ROOM_SECS).await;
    let series = published_series(now_secs());
    let ledger = ledger_of(&series);
    let mut names: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("closed-window-{index}");
        ok(&router, "PutMetricAlarm", &alarm_of(row, &name)).await;
        publish(&router, &name, &series).await;
        names.push(name);
    }

    for (row, name) in ROWS.iter().zip(&names) {
        let case = row.case;
        let period = f64::from(row.period);
        let look_back = f64::from(row.evaluation_periods) * period;
        // The published series decides every row's alarm, so each of them records an evaluated
        // state update once its evaluation is due.
        let records = until_records(&router, name, 1).await;
        for (stamp, record) in &records {
            let open_minute = assert_window_had_closed(case, *stamp, record, period, look_back);
            let cited = cited_datapoints(record);
            assert!(
                !cited.is_empty(),
                "case {case:?}: an evaluated state update names the datapoints its verdict rested \
                 on, got {record}"
            );
            for point in &cited {
                assert_eq!(
                    point.at.rem_euclid(MINUTE_SECS),
                    0.0,
                    "case {case:?}: a graded window begins on the minute the series has values \
                     at, got {} in {record}",
                    point.at
                );
                // THE RULE: the window ends no later than the start of the minute the evaluation
                // ran in, so the minute still being published into was never aggregated into it.
                assert!(
                    point.at + period <= open_minute + DOCUMENT_INSTANT_TOLERANCE_SECS,
                    "case {case:?}: the window [{}, {}) the evaluation at {stamp} graded reaches \
                     into the minute opening at {open_minute}, which had not closed yet, in \
                     {record}",
                    point.at,
                    point.at + period
                );
                // And what it reports over that window is the aggregate of exactly the minutes
                // inside it, as the suite published them.
                let Some((sum, minutes)) =
                    published_window(&ledger, point.at, point.at + period)
                else {
                    panic!(
                        "case {case:?}: the window [{}, {}) names a minute this suite published \
                         no datapoint for, so its aggregate cannot be checked; {record}",
                        point.at,
                        point.at + period
                    )
                };
                assert_eq!(
                    point.sample_count,
                    Some(minutes),
                    "case {case:?}: the window [{}, {}) holds {minutes} published minutes, got \
                     {:?} in {record}",
                    point.at,
                    point.at + period,
                    point.sample_count
                );
                assert!(
                    point
                        .value
                        .is_some_and(|value| (value - sum).abs() < VALUE_TOLERANCE),
                    "case {case:?}: the Sum over the minutes of [{}, {}) is {sum}, got {:?} in \
                     {record}",
                    point.at,
                    point.at + period,
                    point.value
                );
            }
            // The newest window graded is the LATEST one that had closed: the grading is held to
            // the periods that are complete, and it is not held back any further than that.
            let newest = cited.last().copied().expect("a cited datapoint");
            assert!(
                (newest.at + period - open_minute).abs() < DOCUMENT_INSTANT_TOLERANCE_SECS,
                "case {case:?}: the newest window graded ends at the last closed minute \
                 ({open_minute}) of the evaluation recorded at {stamp}, got [{}, {}) in {record}",
                newest.at,
                newest.at + period
            );
        }
    }
}

/// The period of the second test's alarm: five minutes, wider than the minute its window is
/// anchored on, which is the only shape a window anchored on the minute can be told apart from one
/// anchored on the alarm's own period at all.
const RE_ANCHOR_PERIOD: i32 = 300;

/// The value the second test's alarm is compared against, and the two values it publishes: the
/// quiet minutes together stay under it, and one breaching minute carries the window over it.
const RE_ANCHOR_THRESHOLD: f64 = 100.0;
const QUIET_VALUE: f64 = 1.0;
const BREACHING_VALUE: f64 = 1_000.0;

/// The second test's alarm: one five-minute window, decided by the Sum over it.
fn re_anchoring_alarm(name: &str) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Case", "Value": name}],
        "Statistic": "Sum",
        "Period": RE_ANCHOR_PERIOD,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": RE_ANCHOR_THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "missing",
        "ActionsEnabled": false,
    })
}

/// Wait until the clock has left the minute opening at `minute`.
///
/// A wait for the CONDITION — the next minute, whenever it comes — rather than for a span, and
/// bounded by a deadline wide enough for several of them.
async fn until_a_later_minute(minute: f64) {
    let deadline = Instant::now() + WORK_WAIT;
    while minute_start(now_secs()) <= minute && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert!(
        minute_start(now_secs()) > minute,
        "the clock must leave the minute opening at {minute}"
    );
}

#[tokio::test]
async fn the_graded_window_re_anchors_with_the_minute() {
    let case = "a five-minute window over a series published once a minute";
    let router = build_router();
    let period = f64::from(RE_ANCHOR_PERIOD);
    let name = "re-anchoring-window";
    ok(&router, "PutMetricAlarm", &re_anchoring_alarm(name)).await;

    // A quiet minute published over every closed minute of the look-back: the first evaluation
    // grades a full window and reports the state that window decided.
    let open = minute_start(now_secs());
    let quiet: Vec<(f64, f64)> = (1..MINUTES_PUBLISHED)
        .map(|minutes_back| (open - f64::from(minutes_back) * MINUTE_SECS, QUIET_VALUE))
        .collect();
    publish(&router, name, &quiet).await;
    let first = until_records(&router, name, 1).await;
    let (first_stamp, first_record) = first
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a first evaluated state update"));

    // The next evaluation has to run in a LATER minute for its window to be a different span of
    // the same series, so the suite waits for the minute to turn over and then publishes a
    // datapoint that moves the alarm — which is what makes that evaluation record the window it
    // graded.
    until_a_later_minute(minute_start(first_stamp)).await;
    let last_closed_minute = minute_start(now_secs()) - MINUTE_SECS;
    publish(
        &router,
        name,
        &[(last_closed_minute + MINUTE_SECS / 2.0, BREACHING_VALUE)],
    )
    .await;
    let records = until_records(&router, name, 2).await;

    // Every record of them names a window that had closed when its own item was stamped.
    for (stamp, record) in &records {
        assert_window_had_closed(case, *stamp, record, period, period);
    }
    let (last_stamp, last_record) = records
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a second evaluated state update"));
    let first_start = instant_of(case, &first_record, "startDate");
    let last_start = instant_of(case, &last_record, "startDate");
    // THE OTHER HALF OF THE RULE: the window is anchored on the MINUTE the evaluation ran in, so
    // two evaluations a minute apart grade spans a minute apart — the window slides by the minute
    // instead of jumping a whole period at a time, and its start need not sit on a boundary the
    // alarm's own period repeats at.
    let step = last_start - first_start;
    let minutes_apart = minute_start(last_stamp) - minute_start(first_stamp);
    assert!(
        minutes_apart >= MINUTE_SECS,
        "case {case:?}: the two evaluations must run in different minutes, got {first_stamp} and \
         {last_stamp}"
    );
    assert!(
        step > 0.0
            && (step - minutes_apart).abs() < DOCUMENT_INSTANT_TOLERANCE_SECS
            && step.rem_euclid(MINUTE_SECS) < DOCUMENT_INSTANT_TOLERANCE_SECS,
        "case {case:?}: a window anchored on the minute moves with it, so two evaluations \
         {minutes_apart} seconds of minutes apart grade windows that far apart; got {first_start} \
         then {last_start} ({step} apart)"
    );
}
