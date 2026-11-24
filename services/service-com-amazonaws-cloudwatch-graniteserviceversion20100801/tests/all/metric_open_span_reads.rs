// SPDX-License-Identifier: CC0-1.0

//! Reading a series at a period FINER than the span its datapoints are stored in, while the
//! clock is still INSIDE that span, end to end over the wire.
//!
//! ONE rule, asserted over a table: **a read at a period finer than the span a datapoint is
//! stored in can only be answered with that WHOLE span's aggregate, and it IS answered with it, at
//! the start of the span the datapoint is stored in — "For metrics with regular resolution, a
//! period can be as short as one minute (60 seconds) and must be a multiple of 60. For
//! high-resolution metrics that are collected at intervals of less than one minute, the period can
//! be 1, 5, 10, 20, 30, 60, or any multiple of 60. High-resolution metrics are those metrics
//! stored by a `PutMetricData` call that includes a `StorageResolution` of 1 second."
//! (`smithy_query(query="doc",shape="GetMetricStatisticsInput",member="Period")`). The ONE period
//! that asks for detail a regular-resolution span holds none of is the one-second period, which
//! resolves the per-second detail only high-resolution storage keeps ("CloudWatch stores the
//! metric with sub-minute resolution down to one second", `MetricDatum$StorageResolution`): that
//! read is answered from the data stored AT that resolution and from nothing else, whether or not
//! the clock has left the coarser span.
//! So a one-second read of a regular-resolution datapoint holds NO datapoint of it, while every
//! division of a minute a read's windows are laid on (5, 10, 20 and 30 seconds), the minute itself
//! and any coarser period answer from that datapoint as it stands — whether the minute it is
//! stored in is one the clock has left or one still running.**
//!
//! The rows differ along what the rule generalises over: the period the read asks for (every
//! sub-minute value `MetricStat$Period` admits, the minute itself and a multiple of it), the
//! storage resolution the datapoint was published at (1, 60 and omitted — "if you omit this, the
//! default of 60 is used"), and whether the span the datapoint is stored in is one the clock has
//! left. A high-resolution datapoint of the same instant is the control on every row: its span is
//! its own second, so no period a read may ask for is finer than it and it is answered from
//! whichever period is asked.
//!
//! Nothing here waits on the clock and no row's outcome depends on how fast the host runs: the
//! open span is a minute minutes AHEAD of this run (`PutMetricData` takes a time stamp "no more
//! than two hours in the future"), and the closed one a minute that passed well before it
//! started, so a slow host cannot turn one into the other.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, ok};

/// One minute, in the epoch seconds a published time stamp carries.
const MINUTE: f64 = 60.0;

/// The namespace every series of this suite is published under.
const NAMESPACE: &str = "MyApp/OpenSpan";

/// The value every observation carries, so a datapoint that IS answered is recognisable by its
/// `Sum` as well as by its instant.
const VALUE: f64 = 42.0;

/// How far from this run's own minute the two spans lie: far enough that a host taking minutes
/// over this suite still reads the open span as open and the closed one as closed, and well
/// inside both the two-hour limit on a published time stamp and the three-hour tier a sub-minute
/// period is served from.
const SPAN_DISTANCE_MINUTES: f64 = 10.0;

/// Every period a read may ask for below a minute — the high-resolution set
/// `GetMetricStatisticsInput$Period` lists — each one finer than a regular-resolution
/// datapoint's own span.
const SUB_MINUTE_PERIODS: [i64; 5] = [1, 5, 10, 20, 30];

/// The divisions of a minute a read's own windows are laid on: "If you set Period to 5, 10, 20, or
/// 30, the start time of your request is rounded down to the nearest time that corresponds to even
/// 5-, 10-, 20-, or 30-second divisions of a minute." (`GetMetricStatisticsInput$StartTime`) —
/// [`SUB_MINUTE_PERIODS`] without the one-second period, which is the one that resolves detail a
/// regular-resolution span holds none of.
const MINUTE_DIVISION_PERIODS: [i64; 4] = [5, 10, 20, 30];

/// The one sub-minute period below those divisions.
const PER_SECOND_PERIOD: i64 = 1;

/// The periods a regular-resolution datapoint's span is NOT finer than: the minute it is stored
/// at and a multiple of it.
const MINUTE_OR_MORE_PERIODS: [i64; 2] = [60, 300];

/// The first of [`MINUTE_OR_MORE_PERIODS`]: the minute a regular-resolution datapoint is stored
/// at, which is also the window it is folded into when a read asks for exactly that period.
const MINUTE_PERIOD: i64 = 60;

/// The two spellings of regular resolution, which "if you omit this, the default of 60 is used"
/// makes one class: `StorageResolution` 60, and the member left out.
const REGULAR_RESOLUTIONS: [(&str, Option<i64>); 2] =
    [("StorageResolution 60", Some(60)), ("an omitted StorageResolution", None)];

/// The start of the minute this run is in.
fn this_minute() -> f64 {
    (epoch_now() / MINUTE).floor() * MINUTE
}

/// This instant, in the epoch seconds a published time stamp carries.
fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Publish one observation of `metric` at `timestamp`, at `resolution` seconds of storage
/// resolution (omitting the member where `resolution` is `None`).
async fn publish(
    router: &MultiServiceRouter,
    metric: &str,
    timestamp: f64,
    resolution: Option<i64>,
) {
    let mut datum = json!({
        "MetricName": metric,
        "Value": VALUE,
        "Unit": "Count",
        "Timestamp": timestamp,
    });
    if let Some(resolution) = resolution {
        datum["StorageResolution"] = json!(resolution);
    }
    ok(
        router,
        "PutMetricData",
        &json!({ "Namespace": NAMESPACE, "MetricData": [datum] }),
    )
    .await;
}

/// The `(Timestamp, Sum)` pairs `GetMetricStatistics` answers with for `metric` over
/// `[start, end)` at `period`, oldest first.
async fn datapoints(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<(f64, f64)> {
    let body = ok(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": start,
            "EndTime": end,
            "Period": period,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    let mut points: Vec<(f64, f64)> = body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Timestamp"].as_f64().unwrap_or_default(),
                point["Sum"].as_f64().unwrap_or_default(),
            )
        })
        .collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    points
}

/// The same read through `GetMetricData`, whose fold is the same one.
async fn metric_data(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<(f64, f64)> {
    let body = ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "m1",
                "MetricStat": {
                    "Metric": { "Namespace": NAMESPACE, "MetricName": metric },
                    "Period": period,
                    "Stat": "Sum",
                },
            }],
            "StartTime": start,
            "EndTime": end,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    let result = &body["MetricDataResults"][0];
    let stamps: Vec<f64> = result["Timestamps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    let values: Vec<f64> = result["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    let mut points: Vec<(f64, f64)> = stamps.into_iter().zip(values).collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    points
}

#[tokio::test]
async fn a_one_second_read_of_an_open_minute_holds_no_regular_resolution_datapoint() {
    let router = build_router();
    // A minute the clock has NOT left, by a margin no host can run through.
    let minute = this_minute() + SPAN_DISTANCE_MINUTES * MINUTE;
    let stamped = minute + 30.0;
    // A range reaching a minute either side of that one, so the minute's own boundary is inside
    // it: what decides each row is the span the datapoint is stored in, not the range's ends.
    let (start, end) = (minute - MINUTE, minute + 2.0 * MINUTE);

    for (case, resolution) in REGULAR_RESOLUTIONS {
        let metric = format!("Open{}", resolution.unwrap_or_default());
        publish(&router, &metric, stamped, resolution).await;
        // The control published at the SAME instant, whose span is its own second: no period a
        // read may ask for is finer than it, so every row must answer from it.
        let control = format!("{metric}HighRes");
        publish(&router, &control, stamped, Some(1)).await;

        for (reader, got) in [
            (
                "GetMetricStatistics",
                datapoints(&router, &metric, start, end, PER_SECOND_PERIOD).await,
            ),
            (
                "GetMetricData",
                metric_data(&router, &metric, start, end, PER_SECOND_PERIOD).await,
            ),
        ] {
            assert!(
                got.is_empty(),
                "case {case:?} at Period {PER_SECOND_PERIOD}: a minute the clock has not left \
                 holds no complete aggregate to answer a read of the per-second detail it keeps \
                 none of with, so {reader} must answer with no datapoint, got {got:?}"
            );
        }

        // Every division of a minute a read's windows ARE laid on is answered from the datapoint
        // as it stands, at the minute it is stored in — a read at a granularity the series does
        // not hold is answered at the granularity it does hold, not with nothing.
        for period in MINUTE_DIVISION_PERIODS {
            for (reader, got) in [
                (
                    "GetMetricStatistics",
                    datapoints(&router, &metric, start, end, period).await,
                ),
                (
                    "GetMetricData",
                    metric_data(&router, &metric, start, end, period).await,
                ),
            ] {
                assert_eq!(
                    got,
                    vec![(minute, VALUE)],
                    "case {case:?} at Period {period}: the datapoint of a running minute is \
                     reported at the minute it is stored in, so {reader} must answer with it, got \
                     {got:?}"
                );
            }
        }

        for period in SUB_MINUTE_PERIODS {
            let answered = datapoints(&router, &control, start, end, period).await;
            assert_eq!(
                answered.len(),
                1,
                "case {case:?} at Period {period}: the high-resolution twin of the same instant \
                 is stored at its own second, which no period a read may ask for is finer than, \
                 so it must still be answered, got {answered:?}"
            );
        }

        // The other half of the rule: at the span's OWN granularity, and at any coarser period,
        // the window being folded is that span or covers it, so the datapoint of a minute still
        // running is answered from as it stands.
        for period in MINUTE_OR_MORE_PERIODS {
            for (reader, got) in [
                (
                    "GetMetricStatistics",
                    datapoints(&router, &metric, start, end, period).await,
                ),
                (
                    "GetMetricData",
                    metric_data(&router, &metric, start, end, period).await,
                ),
            ] {
                assert_eq!(
                    got.len(),
                    1,
                    "case {case:?} at Period {period}: the datapoint of a running minute is read \
                     at the span it is stored at, so {reader} must answer with it, got {got:?}"
                );
                let (at, sum) = got[0];
                assert!(
                    (sum - VALUE).abs() < f64::EPSILON,
                    "case {case:?} at Period {period}: expected {reader} to answer the published \
                     value {VALUE}, got {sum}"
                );
                // At the minute the datapoint is stored at, the window it is folded into IS
                // that minute, so that is the instant it is reported under. Which window a
                // COARSER period folds it into is the business of the read's own window grid
                // (`metric_range_selection`), not of this rule.
                if period == MINUTE_PERIOD {
                    assert!(
                        (at - minute).abs() < f64::EPSILON,
                        "case {case:?} at Period {period}: expected {reader} to report the \
                         datapoint under the minute it is stored at ({minute}), got {at}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_closed_minute_is_reported_at_every_division_of_a_minute_but_not_per_second() {
    let router = build_router();
    // A minute that closed well before this run, and recent enough to sit inside the tier a
    // sub-minute period is served from ("Data points with a period of less than 60 seconds are
    // available for 3 hours").
    let closed = this_minute() - SPAN_DISTANCE_MINUTES * MINUTE;
    let stamped = closed + 30.0;
    let (start, end) = (closed - MINUTE, closed + 2.0 * MINUTE);

    for (case, resolution) in REGULAR_RESOLUTIONS {
        let metric = format!("Closed{}", resolution.unwrap_or_default());
        publish(&router, &metric, stamped, resolution).await;

        // Every division of a minute a read's windows are laid on answers from the closed
        // minute's aggregate, at that minute's own boundary.
        for period in MINUTE_DIVISION_PERIODS {
            for (reader, got) in [
                (
                    "GetMetricStatistics",
                    datapoints(&router, &metric, start, end, period).await,
                ),
                (
                    "GetMetricData",
                    metric_data(&router, &metric, start, end, period).await,
                ),
            ] {
                assert_eq!(
                    got,
                    vec![(closed, VALUE)],
                    "case {case:?} at Period {period}: a closed minute's aggregate is complete, \
                     so {reader} must answer with it at that minute's own boundary, got {got:?}"
                );
            }
        }

        // The one-second period selects only the data stored at that resolution, so the minute's
        // aggregate is not reported there even once that minute has closed.
        for (reader, got) in [
            (
                "GetMetricStatistics",
                datapoints(&router, &metric, start, end, PER_SECOND_PERIOD).await,
            ),
            (
                "GetMetricData",
                metric_data(&router, &metric, start, end, PER_SECOND_PERIOD).await,
            ),
        ] {
            assert!(
                got.is_empty(),
                "case {case:?} at Period {PER_SECOND_PERIOD}: a read of the per-second detail a \
                 regular-resolution series keeps none of selects only high-resolution data, so \
                 {reader} must answer with no datapoint, got {got:?}"
            );
        }
    }
}

/// The RECORDED shape of the one-second half of the rule: a datum published at the CURRENT
/// instant, read at the per-second period straight afterwards.
///
/// A regular-resolution series holds no per-second detail at all, so such a read answers with no
/// datapoint of it whether or not the minute the publish fell in has closed by the time the read
/// is served — nothing in this row depends on the clock or on how fast the host runs. The
/// high-resolution twin of the same instant is the control that keeps it from passing vacuously.
/// The divisions of a minute a read's windows ARE laid on answer from such a datapoint straight
/// away, which `live_metric_read_back` tables.
#[tokio::test]
async fn a_one_second_read_of_the_running_minute_holds_no_regular_resolution_datapoint() {
    let router = build_router();

    for (case, resolution) in REGULAR_RESOLUTIONS {
        let metric = format!("Running{}", resolution.unwrap_or_default());
        // Published at THIS instant, as a client publishing live metrics does, and read over a
        // range reaching a minute either side of the minute it falls in.
        let published_at = epoch_now();
        let minute = (published_at / MINUTE).floor() * MINUTE;
        publish(&router, &metric, published_at, resolution).await;
        let control = format!("{metric}HighRes");
        publish(&router, &control, published_at, Some(1)).await;
        let (start, end) = (minute - MINUTE, minute + 2.0 * MINUTE);

        let got = datapoints(&router, &metric, start, end, PER_SECOND_PERIOD).await;
        assert!(
            got.is_empty(),
            "case {case:?} at Period {PER_SECOND_PERIOD}: a regular-resolution series keeps no \
             per-second detail, so a read of it must answer with no datapoint, got {got:?}"
        );
        let second = published_at.floor();
        let answered = datapoints(&router, &control, start, end, PER_SECOND_PERIOD).await;
        assert_eq!(
            answered,
            vec![(second, VALUE)],
            "case {case:?} at Period {PER_SECOND_PERIOD}: the high-resolution twin stores the \
             second it was published in, so it must be answered at that second ({second}), got \
             {answered:?}"
        );
    }
}
