// SPDX-License-Identifier: CC0-1.0

//! WHICH periods a metric alarm's evaluation grades over a series published into the period the
//! clock is IN, end to end over the wire.
//!
//! ONE rule, asserted over a table: **an evaluation grades only the periods of its grid that have
//! CLOSED, and a rolling range's periods are counted back from the end the clock has carried that
//! range to — so every datapoint a decision record names sits a whole period before the instant
//! the evaluation queried the series at, and for a sliding window the newest of them ends within
//! one storage granule of that instant rather than on the clock boundary its period repeats
//! at.**
//!
//! A period is aggregated into one datapoint out of every observation stamped inside it —
//! "CloudWatch aggregates all data points with time stamps that fall within each one-hour period"
//! (`smithy_query(query="doc",shape="GetMetricStatistics")`) — so a period the clock is in is
//! still being published into and no verdict is reported over it. Which periods an evaluation
//! covers is what the alarm's evaluation window selects: "Set exactly one of its members,
//! `SlidingWindow` or `WallClockWindow`. If you don't set `EvaluationWindow`, the alarm uses a
//! `SlidingWindow` by default." (`smithy_query(query="shape",shape="EvaluationWindow")`), where a
//! sliding window "advances each time the alarm is evaluated, forming a rolling time window" and
//! a wall clock window "aligns the evaluated range to fixed clock boundaries that match the
//! alarm's period, such as the top of the hour, midnight, or the start of the calendar week".
//! The granularity a series has values at is the one it is stored with: "Setting this to 1
//! specifies this metric as a high-resolution metric […] Setting this to 60 specifies this metric
//! as regular resolution, which CloudWatch stores at 1-minute resolution."
//! (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`).
//!
//! The rows differ along what the rule generalises over: the width of the period (10 and 20
//! seconds, which are read at their own granularity, and 60 and 300 seconds, which are read at
//! the minute), how many periods the look-back covers, and which evaluation window selects the
//! range — none named, `SlidingWindow` named, and `WallClockWindow` for each period such a window
//! may carry.
//!
//! Every row publishes a DENSE series: one datapoint per granule over the whole look-back, the
//! newest of them stamped inside the granule the clock is in. So whichever granule the evaluation
//! runs in, the newest period it grades holds a datapoint — and the period still being published
//! into holds one too, which is the case the rule is about.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, document_instant_secs, now_secs, ok, wait_for_evaluation, wait_for_period_room,
};

/// The customer namespace every row of the table watches.
const NAMESPACE: &str = "Custom/ClosedRollingPeriods";

/// The metric every row watches.
const METRIC: &str = "Load";

/// The value every row publishes, and the threshold it crosses: one breaching datapoint decides
/// every row's alarm, so the state it reaches is one a graded period produced.
const BREACHING_VALUE: f64 = 99.0;
const THRESHOLD: f64 = 10.0;

/// The granularity a REGULAR-resolution series has values at, in seconds: "Setting this to 60
/// specifies this metric as regular resolution, which CloudWatch stores at 1-minute resolution."
/// (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`).
const REGULAR_RESOLUTION_SECS: f64 = 60.0;

/// How far apart two instants may read and still be the same instant: a document holds an instant
/// to the MILLISECOND (`2026-09-22T23:06:00.000+0000`), so one it writes for a computed instant is
/// that instant rounded to the millisecond.
const INSTANT_TOLERANCE_SECS: f64 = 0.0015;

/// How many granules back a row's series reaches: wide enough to fill the look-back of every row
/// several times over, so the periods the evaluation grades all hold a datapoint.
const GRANULES_PUBLISHED: u32 = 10;

/// How much of the granule the clock is in a row needs ahead of it before it publishes: enough
/// for the publish and the read that follows it to happen inside that same granule on a loaded
/// host, so the datapoint of the OPEN period really is in the open period when the evaluation
/// runs. Nothing is asserted about the span — a crossed boundary only closes the period, which
/// the rule allows — so this is room, not a deadline.
const GRANULE_ROOM_SECS: f64 = 5.0;

/// Which evaluation window a row's alarm selects its range with.
#[derive(Debug, Clone, Copy)]
enum Window {
    /// `EvaluationWindow` left out: "the alarm uses a `SlidingWindow` by default".
    Default,
    /// `SlidingWindow` named.
    Sliding,
    /// `WallClockWindow` in UTC.
    WallClock,
}

impl Window {
    /// Whether the boundaries of this window's periods are the clock's own — the aligned grid.
    fn aligned(self) -> bool {
        matches!(self, Self::WallClock)
    }
}

/// One row of the table.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The alarm's `Period`, in seconds.
    period: i32,
    /// The alarm's `EvaluationPeriods`.
    evaluation_periods: i32,
    /// The window it selects its range with.
    window: Window,
}

/// The table: one row per shape the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "ten-second periods, no evaluation window named",
        period: 10,
        evaluation_periods: 2,
        window: Window::Default,
    },
    Row {
        case: "twenty-second periods, naming the sliding window",
        period: 20,
        evaluation_periods: 1,
        window: Window::Sliding,
    },
    Row {
        case: "one-minute periods, no evaluation window named",
        period: 60,
        evaluation_periods: 3,
        window: Window::Default,
    },
    Row {
        // The recorded shape: a five-minute period, whose span is wider than the granularity the
        // series has values at, so its rolling range ends inside the aligned period the clock is
        // in.
        case: "five-minute periods, no evaluation window named",
        period: 300,
        evaluation_periods: 2,
        window: Window::Default,
    },
    Row {
        case: "five-minute periods, naming the sliding window",
        period: 300,
        evaluation_periods: 1,
        window: Window::Sliding,
    },
    Row {
        case: "one-minute periods on the aligned grid",
        period: 60,
        evaluation_periods: 2,
        window: Window::WallClock,
    },
    Row {
        case: "five-minute periods on the aligned grid",
        period: 300,
        evaluation_periods: 1,
        window: Window::WallClock,
    },
];

/// The granularity the series of a row with this period is stored at, and so the granularity the
/// end of its rolling range is read at: the minute, or the period itself where that is finer.
fn granularity(period: f64) -> f64 {
    period.min(REGULAR_RESOLUTION_SECS)
}

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
        // Only the datapoints the row really published are graded.
        "TreatMissingData": "missing",
        "ActionsEnabled": false,
    });
    match row.window {
        Window::Default => {}
        Window::Sliding => alarm["EvaluationWindow"] = json!({"SlidingWindow": {}}),
        Window::WallClock => {
            alarm["EvaluationWindow"] = json!({"WallClockWindow": {"Timezone": "UTC"}});
        }
    }
    alarm
}

/// Publish one breaching datapoint per granule of `name`'s series, over the whole look-back and
/// into the granule the clock is in.
///
/// Stored at high resolution so the instant a datapoint is aggregated at is its own time stamp: a
/// regular-resolution datapoint has no sub-minute detail stored, which a row with a sub-minute
/// period could not place a datapoint inside at all.
async fn publish_dense_series(router: &MultiServiceRouter, name: &str, period: f64) {
    let granule = granularity(period);
    let open_granule_start = (now_secs() / granule).floor() * granule;
    let mut stamps: Vec<f64> = vec![now_secs()];
    for granules_back in 1..=GRANULES_PUBLISHED {
        stamps.push(open_granule_start - f64::from(granules_back) * granule + granule / 2.0);
    }
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

/// The instants of the datapoints the verdict rests on, oldest first — each the START of the
/// period it was aggregated out of.
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

#[tokio::test]
async fn an_evaluation_grades_only_the_periods_of_its_window_that_have_closed() {
    // Every row watches its own series, so all of them are configured on ONE emulator and the
    // evaluation delay is waited out once rather than per row. The widest granularity any row
    // reads at is the minute, so the phase that is waited for is the minute's.
    let router = build_router();
    wait_for_period_room(REGULAR_RESOLUTION_SECS, GRANULE_ROOM_SECS).await;
    let mut names: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("closed-rolling-{index}");
        ok(&router, "PutMetricAlarm", &alarm_of(row, &name)).await;
        publish_dense_series(&router, &name, f64::from(row.period)).await;
        names.push(name);
    }
    wait_for_evaluation();

    for (row, name) in ROWS.iter().zip(&names) {
        let case = row.case;
        let period = f64::from(row.period);
        let granule = granularity(period);
        let described = ok(&router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        let reported = &described["MetricAlarms"][0];
        assert_eq!(
            reported.get("StateValue").and_then(Value::as_str),
            Some("ALARM"),
            "case {case:?}: the breaching series decides the alarm, got {reported}"
        );
        let record = reason_data(case, reported);
        let query_date = instant_of(case, &record, "queryDate");
        let start_date = instant_of(case, &record, "startDate");
        let graded = graded_instants(&record);
        assert!(
            !graded.is_empty(),
            "case {case:?}: expected a graded datapoint, got {record}"
        );

        // THE RULE, first half: every period the record names had CLOSED when the evaluation
        // queried the series — the period the clock is in is still being published into.
        for instant in &graded {
            assert!(
                instant + period <= query_date + INSTANT_TOLERANCE_SECS,
                "case {case:?}: the {period}-second period opening at {instant} had not closed at \
                 the query instant {query_date}, in {record}"
            );
        }
        // And the span the record opens at covers every period it names.
        assert!(
            start_date <= graded[0] + INSTANT_TOLERANCE_SECS && start_date <= query_date,
            "case {case:?}: the span's start ({start_date}) must open no later than the oldest \
             period it names ({}) and no later than the query instant ({query_date}), in \
             {record}",
            graded[0]
        );

        let newest_graded = *graded.last().unwrap_or(&f64::NAN);
        let aligned_boundary = window_start(query_date, period);
        if row.window.aligned() {
            // THE RULE, second half for an ALIGNED range: its periods are the clock's own, so
            // the newest it grades is the one that ENDS on the boundary the query instant last
            // passed.
            for instant in &graded {
                assert_eq!(
                    instant.rem_euclid(period),
                    0.0,
                    "case {case:?}: an aligned range grades periods beginning on a boundary a \
                     {period}-second period repeats at, got {instant} in {record}"
                );
            }
            assert!(
                (newest_graded + period - aligned_boundary).abs() < INSTANT_TOLERANCE_SECS,
                "case {case:?}: the newest aligned period graded must end on the boundary at \
                 {aligned_boundary} that the query instant {query_date} follows, got \
                 {newest_graded} in {record}"
            );
            continue;
        }
        // THE RULE, second half for a ROLLING range: its periods are counted back from the end
        // the clock has carried the range to, which is the newest instant the series has a value
        // at — so the newest period it grades ends within ONE granule of the query instant,
        // wherever the aligned boundary of the alarm's own period happens to lie.
        let lag = query_date - (newest_graded + period);
        assert!(
            (-INSTANT_TOLERANCE_SECS..granule + INSTANT_TOLERANCE_SECS).contains(&lag),
            "case {case:?}: a rolling range ends within one {granule}-second granule of the query \
             instant {query_date}, so the newest period it grades ends in that granule; got a \
             period ending {} ({lag} seconds behind) in {record}",
            newest_graded + period
        );
        // Which is why a rolling range whose period is wider than that granularity reaches back
        // ACROSS the clock boundary its period repeats at, instead of opening on it.
        if period > granule {
            assert!(
                start_date < aligned_boundary,
                "case {case:?}: a rolling range reaches back before the {period}-second boundary \
                 at {aligned_boundary} that the query instant {query_date} follows, got \
                 {start_date} in {record}"
            );
        }
    }
}
