// SPDX-License-Identifier: CC0-1.0

//! A metric alarm's `StateValue` while the series it watches KEEPS BREACHING, end to end over
//! the wire.
//!
//! ONE rule, asserted over a table: **an alarm stays in `ALARM` for as long as the datapoints
//! its evaluation grades breach the comparison it carries — the state is recomputed from the
//! stored series on every evaluation, so a client feeding a steadily breaching metric reads
//! `ALARM` on every look and the alarm leaves it only when a graded datapoint stops
//! breaching.** `MetricAlarm$StateValue` is one of `OK`, `ALARM` and `INSUFFICIENT_DATA`, and
//! the evaluation decides between them from the datapoints in the evaluation range alone, so
//! nothing but a non-breaching datapoint — no elapsed span, no number of evaluations, no
//! boundary the alarm's period crossed — can take a standing `ALARM` away.
//!
//! Why this suite exists beside `just_published_datapoint_grading.rs`, which pins the grading
//! of ONE published datapoint: that suite reads the state once, as soon as the one datapoint it
//! published has been graded, so it says nothing about an alarm that is read AGAIN later. A
//! client keying behaviour off a sustained `ALARM` — an Application Auto Scaling cooldown, a
//! pager, a remediation — needs the state to hold across every evaluation the clock makes due
//! while the breach stands, and an evaluator that decays to `OK` or `INSUFFICIENT_DATA` under a
//! live series passes there and fails here.
//!
//! How the rule is exercised: at every look, each row's whole look-back is published afresh —
//! one breaching datapoint per window of it, stamped inside windows that have already CLOSED,
//! with nothing ever published into the window the clock is in — and then the state is read and
//! asserted. Publishing the look-back as a SPAN rather than as one point per look is what makes
//! the assertion about the evaluator rather than about the host: every look reads an alarm whose
//! graded windows really are full of breaching datapoints, however long the machine took to let
//! the previous look run, so a state other than `ALARM` is the evaluator's doing. Leaving the
//! open window empty is what keeps the suite sensitive the other way: an evaluation that graded
//! a window before its end had passed would find nothing in it and could decide nothing, which
//! fails the row instead of being masked.
//!
//! The watch is longer than the shortest row's window and more than twenty evaluation spans
//! wide, so the state every look reads was recomputed many times over, across a graded window
//! that has rolled on and over datapoints that did not exist when the suite started; which
//! window an evaluation grades on each cadence is what the sibling closed-window suites pin.
//! Nothing is asserted about how long anything took: the state is asserted at every
//! look, and the alarm's own history is read at the end so a departure from `ALARM` BETWEEN two
//! looks fails the row too.
//!
//! The rows differ along what the rule generalises over: the alarm's `Period` (sub-minute and
//! the regular cadence), the granularity the series is stored at (regular and high
//! resolution), the statistic the window is folded by, the direction of the comparison, the
//! "M out of N" counts the state is decided by, and how a window with no datapoint of its own
//! is treated.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::harness::{alarm_history_items, build_router, now_secs, ok, AlarmKind};

/// The namespace every row publishes into: a customer namespace, so the datapoints go over the
/// wire the way a client's do (`PutMetricData` refuses an `AWS/` namespace).
const NAMESPACE: &str = "Custom/BreachHeld";

/// The metric the rows watch inside it; each row has its own `Case` dimension, so the rows
/// share one emulator without sharing a series.
const METRIC: &str = "Load";

/// The threshold every row's alarm carries.
const THRESHOLD: f64 = 50.0;

/// How long the rows are watched while the breaching metric keeps being published.
///
/// Longer than the shortest row's window, so the graded window of that row rolls on during the
/// watch, and more than twenty evaluation spans wide, so the state every look reads was
/// recomputed many times over and over datapoints that did not exist when the suite started.
/// Which window an evaluation grades on each cadence is pinned by the sibling closed-window
/// suites, so this one does not have to be watched across the widest row's boundary. It is a
/// WATCH, not a wait: the invariant is asserted on every look inside it, so a slower host only
/// takes fewer looks — and it is kept short because this one test's wall clock is time the whole
/// integration target pays.
const WATCH: Duration = Duration::from_secs(12);

/// How long the rows are given to reach `ALARM` from the series published for them.
///
/// The published datapoints sit in windows that have already closed, so the state is owed as
/// soon as the first evaluation of each alarm comes due — a sub-second span after the write.
/// This deadline is far above that, so only a row that never grades the series at all fails on
/// the state it last reported, and nothing is asserted about how long a row took to get there.
const REACHED_WITHIN: Duration = Duration::from_secs(15);

/// How often a look is taken inside the watch, and while waiting for the first `ALARM`.
///
/// Named apart from `crate::harness::LOOK_INTERVAL`, which other suites import, because it is
/// deliberately sparser: a look costs this suite a publish AND a read, and a departure between
/// two looks is caught by the history read at the end rather than by the density of the looks,
/// so the looks only have to be frequent enough to see a state the evaluator holds for one of
/// its half-second spans.
const WATCH_LOOK_INTERVAL: Duration = Duration::from_millis(500);

/// The granularity a datapoint published without a `StorageResolution` is stored at: "Setting
/// this to 60 specifies this metric as regular resolution, which CloudWatch stores at 1-minute
/// resolution" (`MetricDatum$StorageResolution`), which is the default — "if you do not specify
/// it the default of 60 is used".
const REGULAR_RESOLUTION_SECS: f64 = 60.0;

/// The only history item type a state change is recorded under
/// (`DescribeAlarmHistoryInput$HistoryItemType`).
const STATE_UPDATE: &str = "StateUpdate";

/// The state every row must reach and then hold.
const ALARM: &str = "ALARM";

/// How the history summary of a state change OUT of [`ALARM`] opens: the summary names the
/// state the change moved the alarm from, so this prefix is what a departure looks like
/// whatever it departed to.
const LEFT_ALARM: &str = "Alarm updated from ALARM";

/// One row: how the alarm is configured, and the value published for it.
struct Row {
    case: &'static str,
    /// "The length, in seconds, used each time the metric specified in MetricName is
    /// evaluated" (`PutMetricAlarmInput$Period`).
    period: i64,
    /// The `StorageResolution` every datapoint of the row is published with, or `None` for a
    /// publish naming none, which is the regular resolution
    /// ([`REGULAR_RESOLUTION_SECS`]).
    storage_resolution: Option<i64>,
    statistic: &'static str,
    comparison_operator: &'static str,
    evaluation_periods: i64,
    datapoints_to_alarm: i64,
    treat_missing_data: &'static str,
    /// The value the row keeps publishing, which BREACHES its comparison.
    value: f64,
}

impl Row {
    /// How wide one of this row's windows is, in seconds.
    #[allow(clippy::cast_precision_loss)]
    fn period_secs(&self) -> f64 {
        self.period as f64
    }

    /// The granularity this row's series is STORED at, in seconds: one second for a
    /// high-resolution publish, a minute for a regular-resolution one.
    #[allow(clippy::cast_precision_loss)]
    fn stored_span_secs(&self) -> f64 {
        self.storage_resolution
            .map_or(REGULAR_RESOLUTION_SECS, |resolution| resolution as f64)
    }

    /// How far apart this row's fed datapoints are stamped.
    ///
    /// A series holds one datapoint per storage span however often it is published into, so
    /// points spaced more finely than that span would collapse into one datapoint and the
    /// look-back would hold fewer datapoints than it grades. Spacing them by the wider of the
    /// row's period and that span is what makes each point a datapoint of its own whatever
    /// resolution the row publishes at.
    fn feed_spacing_secs(&self) -> f64 {
        self.period_secs().max(self.stored_span_secs())
    }
}

/// The rows. Each keeps publishing a value its own comparison breaches, so the state asserted
/// is the one the series it is fed really implies.
const ROWS: &[Row] = &[
    // The shortest cadence an alarm may be evaluated on, over a series stored at one-second
    // granularity: the graded window rolls several times per watch.
    Row {
        case: "the shortest period over a high-resolution series",
        period: 10,
        storage_resolution: Some(1),
        statistic: "Maximum",
        comparison_operator: "GreaterThanThreshold",
        evaluation_periods: 3,
        datapoints_to_alarm: 1,
        // Nothing but a graded breaching datapoint can hold this alarm in ALARM: a window with
        // no datapoint of its own is left out of the evaluation, and an evaluation that can
        // decide nothing reports INSUFFICIENT_DATA.
        treat_missing_data: "missing",
        value: THRESHOLD + 30.0,
    },
    // The regular cadence over a regular-resolution series, breaching BELOW its threshold: the
    // configuration a client scaling on a quiet metric writes.
    Row {
        case: "the regular cadence over a regular-resolution series, breaching below",
        period: 60,
        storage_resolution: None,
        statistic: "Minimum",
        comparison_operator: "LessThanThreshold",
        evaluation_periods: 1,
        datapoints_to_alarm: 1,
        treat_missing_data: "missing",
        value: THRESHOLD - 40.0,
    },
    // Both of two periods must breach, and a window with no datapoint counts as not breaching —
    // so a lost datapoint would take the alarm to OK rather than leaving it where it was.
    Row {
        case: "two of two periods, with missing data not breaching",
        period: 20,
        storage_resolution: Some(1),
        statistic: "Average",
        comparison_operator: "GreaterThanOrEqualToThreshold",
        evaluation_periods: 2,
        datapoints_to_alarm: 2,
        treat_missing_data: "notBreaching",
        value: THRESHOLD,
    },
    // Two of five, on a cadence between the two above and over a regular-resolution series —
    // where the series holds one datapoint per minute however often it is published into, so
    // the datapoints graded are far sparser than the windows.
    Row {
        case: "two of five periods over a regular-resolution series",
        period: 30,
        storage_resolution: None,
        statistic: "Maximum",
        comparison_operator: "GreaterThanThreshold",
        evaluation_periods: 5,
        datapoints_to_alarm: 2,
        treat_missing_data: "notBreaching",
        value: THRESHOLD + 1.0,
    },
    // One of two, with the other direction of the comparison at its boundary.
    Row {
        case: "one of two periods breaching at its threshold",
        period: 20,
        storage_resolution: Some(1),
        statistic: "Minimum",
        comparison_operator: "LessThanOrEqualToThreshold",
        evaluation_periods: 2,
        datapoints_to_alarm: 1,
        treat_missing_data: "missing",
        value: THRESHOLD,
    },
];

/// The `Case` dimension value one row's series is published and watched under: the row's own
/// prose, reduced to one identifier, so neither the dimension nor the alarm name depends on how
/// the case happens to be punctuated.
fn dimension(row: &Row) -> String {
    let mut identifier = String::with_capacity(row.case.len());
    for character in row.case.chars() {
        if character.is_ascii_alphanumeric() {
            identifier.push(character);
        } else if !identifier.ends_with('-') {
            identifier.push('-');
        }
    }
    identifier.trim_matches('-').to_string()
}

/// The alarm name one row carries.
fn alarm_name(row: &Row) -> String {
    format!("breach-held-{}", dimension(row))
}

/// One datapoint of `row`, stamped at `timestamp` when one is given and at the instant of the
/// call otherwise — "if you do not specify a time stamp, the time stamp of the data point is
/// the time that PutMetricData was called" (`MetricDatum$Timestamp`).
fn datum(row: &Row, timestamp: Option<f64>) -> Value {
    let mut datum = json!({
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Case", "Value": dimension(row)}],
        "Value": row.value,
    });
    if let Some(resolution) = row.storage_resolution {
        datum["StorageResolution"] = json!(resolution);
    }
    if let Some(timestamp) = timestamp {
        datum["Timestamp"] = json!(timestamp);
    }
    datum
}

/// Publish the breaching series every row's look-back is graded over, as of this instant: one
/// datapoint per window of the look-back, each stamped inside a window that has already CLOSED
/// (one spacing back and further), plus one spare so a boundary crossed between this publish
/// and the read still leaves the look-back full.
///
/// This is the feed a client keeps up while it watches the alarm, and it is published as a SPAN
/// rather than as a single point at the instant of the call for a reason the suite depends on:
/// what the rule is about is the state an alarm holds while the datapoints it grades breach, so
/// every look has to read an alarm whose look-back really is full of breaching datapoints —
/// however long the host took to let the previous look run. A feed of one point per look would
/// instead make the state depend on how promptly this process was scheduled, which is not what
/// is being asserted.
///
/// The open window is left out: nothing is ever published into the window the clock is in, so an
/// evaluation that graded a window before its end had passed would find no datapoint in it and
/// could decide nothing — the row would fail rather than be held up by a window still being
/// published into.
async fn feed(router: &smithy_server_runtime::MultiServiceRouter) {
    let now = now_secs();
    let mut data: Vec<Value> = Vec::new();
    for row in ROWS {
        let spacing = row.feed_spacing_secs();
        for back in 1..=row.evaluation_periods + 1 {
            #[allow(clippy::cast_precision_loss)]
            let back = back as f64;
            data.push(datum(row, Some(now - back * spacing)));
        }
    }
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": data}),
    )
    .await;
}

/// The state `DescribeAlarms` reports for every row, in the rows' own order — one read for all
/// of them, so a look at the table is one request.
async fn reported_states(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let names: Vec<String> = ROWS.iter().map(alarm_name).collect();
    let body = ok(router, "DescribeAlarms", &json!({"AlarmNames": names})).await;
    let reported = body["MetricAlarms"].as_array().cloned().unwrap_or_default();
    ROWS.iter()
        .map(|row| {
            let name = alarm_name(row);
            reported
                .iter()
                .find(|alarm| alarm["AlarmName"].as_str() == Some(name.as_str()))
                .and_then(|alarm| alarm["StateValue"].as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Feed the rows and look at them until every one of them reports `ALARM`, or until the wait is
/// up — a wait for the CONDITION, so what a row that never gets there fails on is the state it
/// last reported.
async fn until_every_row_alarms(
    router: &smithy_server_runtime::MultiServiceRouter,
) -> Vec<String> {
    let deadline = Instant::now() + REACHED_WITHIN;
    let mut last = reported_states(router).await;
    while last.iter().any(|state| state != ALARM) && Instant::now() < deadline {
        tokio::time::sleep(WATCH_LOOK_INTERVAL).await;
        feed(router).await;
        last = reported_states(router).await;
    }
    last
}

/// The summaries of the state changes recorded for one row's alarm.
async fn recorded_changes(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<String> {
    alarm_history_items(router, AlarmKind::Metric, name, STATE_UPDATE)
        .await
        .iter()
        .map(|item| {
            item["HistorySummary"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_metric_alarm_holds_alarm_while_the_series_it_watches_keeps_breaching() {
    let router = build_router();

    // The breaching series the look-backs are graded over is published BEFORE the alarms are
    // configured, so each alarm's very first evaluation grades a breach: the state each row
    // then holds was never written by anything but an evaluation of the series.
    feed(&router).await;
    for row in ROWS {
        let name = alarm_name(row);
        ok(
            &router,
            "PutMetricAlarm",
            &json!({
                "AlarmName": name,
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": dimension(row)}],
                "Statistic": row.statistic,
                "Period": row.period,
                "EvaluationPeriods": row.evaluation_periods,
                "DatapointsToAlarm": row.datapoints_to_alarm,
                "Threshold": THRESHOLD,
                "ComparisonOperator": row.comparison_operator,
                "TreatMissingData": row.treat_missing_data,
            }),
        )
        .await;
    }

    let reached = until_every_row_alarms(&router).await;
    for (row, reached) in ROWS.iter().zip(&reached) {
        assert_eq!(
            reached.as_str(),
            ALARM,
            "case {:?}: expected the alarm to grade the breaching datapoints published for it \
             into {ALARM}, got {reached:?}",
            row.case
        );
    }

    // The watch: the breach is kept up and the state is read on every look, so a row whose
    // alarm decays at ANY point inside the watch fails on the state it decayed to.
    let watch_until = Instant::now() + WATCH;
    let mut looks: usize = 0;
    loop {
        feed(&router).await;
        let states = reported_states(&router).await;
        looks += 1;
        for (row, state) in ROWS.iter().zip(&states) {
            assert_eq!(
                state.as_str(),
                ALARM,
                "case {:?}: expected the alarm to hold {ALARM} while every window of its \
                 look-back held a breaching datapoint (look {looks}), got {state:?}",
                row.case
            );
        }
        if Instant::now() >= watch_until {
            break;
        }
        tokio::time::sleep(WATCH_LOOK_INTERVAL).await;
    }

    // And a departure BETWEEN two looks is caught where the alarm records its own state
    // changes: the alarm moved INTO ALARM once and never out of it, however many evaluations
    // the clock made due while the breach stood — "the timestamp of the last change to the
    // alarm's StateValue" (`MetricAlarm$StateTransitionedTimestamp`) is written by every state
    // change, and each one is recorded in the alarm's history.
    for row in ROWS {
        let name = alarm_name(row);
        let changes = recorded_changes(&router, &name).await;
        assert!(
            !changes.is_empty(),
            "case {:?}: expected the state change into {ALARM} to be recorded, got {changes:?}",
            row.case
        );
        let departures: Vec<&String> = changes
            .iter()
            .filter(|summary| summary.starts_with(LEFT_ALARM))
            .collect();
        assert!(
            departures.is_empty(),
            "case {:?}: expected no state change away from {ALARM} while the metric kept \
             breaching, got {departures:?} among {changes:?}",
            row.case
        );
    }
}
